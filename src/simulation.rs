use crate::evaluation::{Evaluation, EvaluationBackend, EvaluationError, parse_toggle_metrics};
#[allow(unused_imports)]
use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::AtomicBool,
    time::Duration,
};

pub(crate) const OUTPUT_LIMIT: usize = 64 * 1024;
pub(crate) const MAX_CANDIDATE_FAILURES: u32 = 10;
pub(crate) const MAX_MEASUREMENT_FAILURES: u32 = 5;
pub(crate) const VCD_DIR_PREFIX: &str = "numeric-fuzzer-vcd-";

/// Hostname used to attribute per-run VCD directories to a host in shared filesystems.
pub(crate) fn hostname() -> String {
    let mut buffer = [0u8; 256];
    let result =
        unsafe { libc::gethostname(buffer.as_mut_ptr().cast::<libc::c_char>(), buffer.len()) };
    if result == 0 {
        let len = buffer
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(buffer.len());
        String::from_utf8_lossy(&buffer[..len]).into_owned()
    } else {
        "unknown".to_string()
    }
}

pub(crate) fn pid_is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    // EPERM means the process exists but is owned by someone else; only ESRCH means dead.
    io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Removes leftover per-run VCD directories on `parent` that belong to dead processes
/// of this host (e.g. after the supervisor was SIGKILLed by SLURM or a node crash).
/// Directories of live processes and of other hosts are never touched. Returns the
/// number of directories removed.
pub(crate) fn remove_stale_vcd_dirs(
    parent: &Path,
    host: &str,
    is_alive: impl Fn(i32) -> bool,
) -> usize {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix(VCD_DIR_PREFIX) else {
            continue;
        };
        // Full name: <prefix><host>-<pid>-<random>; the random suffix contains no dash.
        let mut parts = rest.rsplitn(3, '-');
        let (Some(_random), Some(pid), Some(dir_host)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if dir_host != host {
            continue;
        }
        let Ok(pid) = pid.parse::<i32>() else {
            continue;
        };
        if is_alive(pid) {
            continue;
        }
        match std::fs::remove_dir_all(entry.path()) {
            Ok(()) => {
                removed += 1;
                tracing::info!("Removed stale VCD directory {}", entry.path().display());
            }
            Err(e) => {
                tracing::warn!(
                    "Could not remove stale VCD directory {}: {e}",
                    entry.path().display()
                );
            }
        }
    }
    removed
}

/// Arguments are passed directly, with `{elf}` and `{vcd}` expanded within each argument.
/// Relative paths are resolved against the fuzzer's starting directory, not `working_dir`.
pub struct SimulationConfig {
    pub simulator: PathBuf,
    pub simulator_args: Vec<OsString>,
    pub working_dir: PathBuf,
    pub analyzer: PathBuf,
    pub target_scope: String,
    pub exclude_signal_file: Option<PathBuf>,
    /// Parent directory for the per-run VCD directory. Defaults to `working_dir` so large
    /// traces are not written to the (often size-limited) system temp directory.
    pub vcd_dir: Option<PathBuf>,
    pub simulator_timeout: Duration,
    pub analyzer_timeout: Duration,
}

pub struct SimulationBackend {
    config: SimulationConfig,
    candidate_failures: u32,
    measurement_failures: u32,
}

impl SimulationBackend {
    pub fn new(mut config: SimulationConfig) -> Result<Self, EvaluationError> {
        let cwd = std::env::current_dir().map_err(|e| {
            EvaluationError::Fatal(format!("Cannot resolve current directory: {e}"))
        })?;
        config.simulator = absolute(&cwd, &config.simulator);
        config.analyzer = absolute(&cwd, &config.analyzer);
        config.working_dir = absolute(&cwd, &config.working_dir);
        config.exclude_signal_file = config.exclude_signal_file.map(|p| absolute(&cwd, &p));
        config.vcd_dir = config.vcd_dir.map(|p| absolute(&cwd, &p));
        if !config.working_dir.is_dir() {
            return Err(EvaluationError::Fatal(format!(
                "Simulation working directory is not a directory: {}",
                config.working_dir.display()
            )));
        }
        for (name, path) in [
            ("simulator", &config.simulator),
            ("analyzer", &config.analyzer),
        ] {
            if !path.is_file() {
                return Err(EvaluationError::Fatal(format!(
                    "{name} executable is not a file: {}",
                    path.display()
                )));
            }
        }
        if let Some(path) = &config.exclude_signal_file
            && !path.is_file()
        {
            return Err(EvaluationError::Fatal(format!(
                "Analyzer exclusion file is not a file: {}",
                path.display()
            )));
        }
        if let Some(path) = &config.vcd_dir
            && !path.is_dir()
        {
            return Err(EvaluationError::Fatal(format!(
                "VCD directory is not a directory: {}",
                path.display()
            )));
        }
        if config.target_scope.trim().is_empty() {
            return Err(EvaluationError::Fatal(
                "Analyzer target scope is empty".into(),
            ));
        }
        if config.simulator_timeout.is_zero() || config.analyzer_timeout.is_zero() {
            return Err(EvaluationError::Fatal(
                "Process timeouts must be nonzero".into(),
            ));
        }
        if !config.simulator_args.iter().any(|a| contains(a, b"{elf}"))
            || !config.simulator_args.iter().any(|a| contains(a, b"{vcd}"))
        {
            return Err(EvaluationError::Fatal(
                "Simulator arguments must include both {elf} and {vcd} placeholders".into(),
            ));
        }
        Ok(Self {
            config,
            candidate_failures: 0,
            measurement_failures: 0,
        })
    }

    pub(crate) fn run_direct(
        &self,
        elf: &Path,
        cancelled: &AtomicBool,
    ) -> Result<Evaluation, EvaluationError> {
        let elf = std::fs::canonicalize(elf).map_err(|e| {
            EvaluationError::Fatal(format!(
                "Cannot resolve candidate ELF {}: {e}",
                elf.display()
            ))
        })?;
        let parent = self
            .config
            .vcd_dir
            .as_deref()
            .unwrap_or(&self.config.working_dir);
        // Sweep leftovers of dead same-host processes (killed supervisors leak theirs).
        let host = hostname();
        remove_stale_vcd_dirs(parent, &host, pid_is_alive);
        let prefix = format!("{VCD_DIR_PREFIX}{host}-{}-", std::process::id());
        let work = tempfile::Builder::new()
            .prefix(&prefix)
            .tempdir_in(parent)
            .map_err(|e| {
                EvaluationError::Fatal(format!(
                    "Cannot create VCD directory in {}: {e}",
                    parent.display()
                ))
            })?;
        let run_dir = std::fs::canonicalize(work.path())
            .map_err(|e| EvaluationError::Fatal(format!("Cannot resolve VCD directory: {e}")))?;
        let result = self.run_in(&elf, &run_dir.join("trace.vcd"), cancelled);
        work.close().map_err(|e| {
            EvaluationError::Fatal(format!("Cannot clean up simulation artifacts: {e}"))
        })?;
        result
    }

    fn run_in(
        &self,
        elf: &Path,
        vcd: &Path,
        cancelled: &AtomicBool,
    ) -> Result<Evaluation, EvaluationError> {
        let args: Vec<OsString> = self
            .config
            .simulator_args
            .iter()
            .map(|arg| expand(arg, elf, vcd))
            .collect();
        tracing::debug!(
            "Running simulator {:?} with args {:?} in {:?} (elf={}, vcd={})",
            self.config.simulator,
            args,
            self.config.working_dir,
            elf.display(),
            vcd.display()
        );
        let mut simulator = Command::new(&self.config.simulator);
        simulator
            .current_dir(&self.config.working_dir)
            // Keep subprocess temp files off the size-limited /tmp.
            .env("TMPDIR", &self.config.working_dir)
            .args(&args);
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                simulator.pre_exec(|| {
                    let mut limits = std::mem::MaybeUninit::<libc::rlimit>::uninit();
                    if libc::getrlimit(libc::RLIMIT_CORE, limits.as_mut_ptr()) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    let mut limits = limits.assume_init();
                    limits.rlim_cur = 0;
                    if libc::setrlimit(libc::RLIMIT_CORE, &limits) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let output = crate::supervisor::run_process(
            &mut simulator,
            self.config.simulator_timeout,
            cancelled,
        )
        .map_err(|e| EvaluationError::Fatal(format!("Simulator process: {e}")))?;
        tracing::debug!(
            "Simulator exited {} (timed_out={}); VCD {} exists: {}",
            output.status,
            output.timed_out,
            vcd.display(),
            vcd.is_file()
        );
        if output.timed_out {
            return Err(EvaluationError::Candidate(format!(
                "Simulator timed out: {}",
                output.diagnostic()
            )));
        }
        if !output.status.success() && !output.chipyard_cycle_limit() {
            return Err(EvaluationError::Candidate(format!(
                "Simulator exited {}: {}",
                output.status,
                output.diagnostic()
            )));
        }
        if !vcd.is_file() {
            return Err(EvaluationError::Measurement(format!(
                "Simulator did not produce VCD {}: {}",
                vcd.display(),
                output.diagnostic()
            )));
        }

        let mut analyzer = Command::new(&self.config.analyzer);
        analyzer
            .current_dir(&self.config.working_dir)
            .env("TMPDIR", &self.config.working_dir)
            .arg(vcd)
            .arg("--target-scope")
            .arg(&self.config.target_scope);
        if let Some(path) = &self.config.exclude_signal_file {
            analyzer.arg("--exclude-signal-file").arg(path);
        }
        tracing::debug!(
            "Running analyzer {:?} on VCD {} with target scope {:?} (exclude file: {:?}) in {:?}",
            self.config.analyzer,
            vcd.display(),
            self.config.target_scope,
            self.config.exclude_signal_file,
            self.config.working_dir
        );
        let output =
            crate::supervisor::run_process(&mut analyzer, self.config.analyzer_timeout, cancelled)
                .map_err(|e| EvaluationError::Fatal(format!("Analyzer process: {e}")))?;
        tracing::debug!(
            "Analyzer exited {} (timed_out={}), stdout {} bytes, stderr {} bytes",
            output.status,
            output.timed_out,
            output.stdout.len(),
            output.stderr.len()
        );
        if output.timed_out {
            return Err(EvaluationError::Measurement(format!(
                "Analyzer timed out: {}",
                output.diagnostic()
            )));
        }
        if !output.status.success() {
            return Err(EvaluationError::Measurement(format!(
                "Analyzer exited {}: {}",
                output.status,
                output.diagnostic()
            )));
        }
        if output.stdout_truncated {
            return Err(EvaluationError::Measurement(
                "Analyzer stdout exceeded the output limit".into(),
            ));
        }
        if !output.stderr.is_empty() {
            eprintln!(
                "Analyzer stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let stdout = std::str::from_utf8(&output.stdout).map_err(|e| {
            EvaluationError::Measurement(format!("Analyzer stdout is not UTF-8: {e}"))
        })?;
        parse_toggle_metrics(stdout)
    }

    #[cfg(not(test))]
    fn run(&self, elf: &Path) -> Result<Evaluation, EvaluationError> {
        use std::os::unix::process::CommandExt;
        let executable = std::env::current_exe().map_err(|e| {
            EvaluationError::Fatal(format!("Cannot locate supervisor executable: {e}"))
        })?;
        let mut command = Command::new(executable);
        command
            .arg("--internal-simulation-supervisor")
            .arg(elf)
            .arg(&self.config.simulator)
            .arg(&self.config.working_dir)
            .arg(&self.config.analyzer)
            .arg(&self.config.target_scope)
            .arg(
                self.config
                    .exclude_signal_file
                    .as_deref()
                    .unwrap_or_else(|| Path::new("")),
            )
            .arg(
                self.config
                    .vcd_dir
                    .as_deref()
                    .unwrap_or_else(|| Path::new("")),
            )
            .arg(self.config.simulator_timeout.as_secs().to_string())
            .arg(self.config.simulator_timeout.subsec_nanos().to_string())
            .arg(self.config.analyzer_timeout.as_secs().to_string())
            .arg(self.config.analyzer_timeout.subsec_nanos().to_string())
            .args(&self.config.simulator_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        tracing::debug!(
            "Starting simulation supervisor for ELF {} (simulator {:?}, working dir {:?}, analyzer {:?}, args {:?})",
            elf.display(),
            self.config.simulator,
            self.config.working_dir,
            self.config.analyzer,
            self.config.simulator_args
        );
        let mut child = command.spawn().map_err(|e| {
            EvaluationError::Fatal(format!("Cannot start simulation supervisor: {e}"))
        })?;
        let _parent_liveness = child
            .stdin
            .take()
            .ok_or_else(|| EvaluationError::Fatal("Supervisor stdin unavailable".into()))?;
        let output = child.wait_with_output().map_err(|e| {
            EvaluationError::Fatal(format!("Cannot wait for simulation supervisor: {e}"))
        })?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::debug!(
            "Simulation supervisor for {} exited {}: {stderr}",
            elf.display(),
            output.status
        );
        match output.status.code() {
            Some(0) => {
                let stdout = std::str::from_utf8(&output.stdout).map_err(|e| {
                    EvaluationError::Fatal(format!("Invalid supervisor output: {e}"))
                })?;
                parse_toggle_metrics(stdout)
            }
            Some(10) => Err(EvaluationError::Candidate(stderr.into_owned())),
            Some(11) => Err(EvaluationError::Measurement(stderr.into_owned())),
            _ => Err(EvaluationError::Fatal(format!(
                "Simulation supervisor exited {}: {stderr}",
                output.status
            ))),
        }
    }

    #[cfg(test)]
    fn run(&self, elf: &Path) -> Result<Evaluation, EvaluationError> {
        self.run_direct(elf, &AtomicBool::new(false))
    }
}

impl EvaluationBackend for SimulationBackend {
    fn evaluate(&mut self, elf: &Path) -> Result<Evaluation, EvaluationError> {
        match self.run(elf) {
            Ok(result) => {
                self.candidate_failures = 0;
                self.measurement_failures = 0;
                Ok(result)
            }
            Err(EvaluationError::Candidate(message)) => {
                self.candidate_failures += 1;
                if self.candidate_failures >= MAX_CANDIDATE_FAILURES {
                    Err(EvaluationError::Fatal(format!(
                        "{} consecutive simulation failures, last: {message}",
                        self.candidate_failures
                    )))
                } else {
                    Err(EvaluationError::Candidate(message))
                }
            }
            Err(EvaluationError::Measurement(message)) => {
                self.measurement_failures += 1;
                if self.measurement_failures >= MAX_MEASUREMENT_FAILURES {
                    Err(EvaluationError::Fatal(format!(
                        "{} consecutive measurement failures, last: {message}",
                        self.measurement_failures
                    )))
                } else {
                    Err(EvaluationError::Measurement(message))
                }
            }
            Err(e) => Err(e),
        }
    }
}

pub(crate) fn absolute(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    }
}

pub(crate) fn contains(arg: &OsStr, needle: &[u8]) -> bool {
    use std::os::unix::ffi::OsStrExt;
    arg.as_bytes()
        .windows(needle.len())
        .any(|part| part == needle)
}

pub(crate) fn expand(arg: &OsStr, elf: &Path, vcd: &Path) -> OsString {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let mut result = Vec::new();
    let mut remaining = arg.as_bytes();
    while !remaining.is_empty() {
        if remaining.starts_with(b"{elf}") {
            result.extend_from_slice(elf.as_os_str().as_bytes());
            remaining = &remaining[5..];
        } else if remaining.starts_with(b"{vcd}") {
            result.extend_from_slice(vcd.as_os_str().as_bytes());
            remaining = &remaining[5..];
        } else {
            result.push(remaining[0]);
            remaining = &remaining[1..];
        }
    }
    OsString::from_vec(result)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, time::Instant};

    static FIXTURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct Fixture {
        _guard: std::sync::MutexGuard<'static, ()>,
        dir: tempfile::TempDir,
        elf: PathBuf,
        simulator: PathBuf,
        analyzer: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let guard = FIXTURE_LOCK.lock().unwrap();
            let dir = tempfile::tempdir().unwrap();
            let elf = dir.path().join("candidate with space.elf");
            let simulator = dir.path().join("simulator");
            let analyzer = dir.path().join("analyzer");
            fs::write(&elf, b"ELF").unwrap();
            Self {
                _guard: guard,
                dir,
                elf,
                simulator,
                analyzer,
            }
        }

        fn script(path: &Path, content: &str) {
            let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap()).unwrap();
            use std::io::Write;
            write!(file, "#!/bin/sh\n{content}\n").unwrap();
            file.as_file()
                .set_permissions(fs::Permissions::from_mode(0o700))
                .unwrap();
            let (file, temporary_path) = file.keep().unwrap();
            drop(file);
            fs::rename(temporary_path, path).unwrap();
        }

        fn config(&self) -> SimulationConfig {
            SimulationConfig {
                simulator: self.simulator.clone(),
                simulator_args: vec!["{elf}".into(), "{vcd}".into()],
                working_dir: self.dir.path().to_owned(),
                analyzer: self.analyzer.clone(),
                target_scope: "TOP.dut".into(),
                exclude_signal_file: None,
                vcd_dir: None,
                simulator_timeout: Duration::from_secs(2),
                analyzer_timeout: Duration::from_secs(2),
            }
        }

        fn success_scripts(&self) {
            Self::script(
                &self.simulator,
                r#"
test -f "$1" || exit 3
printf '%s' "$2" > vcd-path
printf 'waveform' > "$2"
printf '%s' "${TMPDIR:-}" > tmpdir
"#,
            );
            Self::script(
                &self.analyzer,
                r#"
test "$(cat "$1")" = waveform || exit 4
test "$2" = "--target-scope" && test "$3" = "TOP.dut" || exit 5
printf 'toggle_activity=1200\ntoggle_coverage=0.75\n'
"#,
            );
        }
    }

    #[test]
    fn runs_simulator_then_analyzer_with_fresh_cleaned_trace() {
        let fixture = Fixture::new();
        fixture.success_scripts();
        let mut backend = SimulationBackend::new(fixture.config()).unwrap();
        let result = backend.evaluate(&fixture.elf).unwrap();
        assert_eq!(result.score(), 0.75);
        assert_eq!(result.toggle_activity_per_second(), Some(1200.0));
        let first = fs::read_to_string(fixture.dir.path().join("vcd-path")).unwrap();
        assert!(!Path::new(&first).exists());
        backend.evaluate(&fixture.elf).unwrap();
        let second = fs::read_to_string(fixture.dir.path().join("vcd-path")).unwrap();
        assert_ne!(first, second);
        assert!(!Path::new(&second).exists());
        // The simulator runs with TMPDIR inside the working directory, not /tmp.
        let tmpdir = fs::read_to_string(fixture.dir.path().join("tmpdir")).unwrap();
        assert_eq!(tmpdir, fixture.dir.path().to_str().unwrap());
    }

    #[test]
    fn vcd_directories_stay_out_of_the_system_temp_dir() {
        let fixture = Fixture::new();
        fixture.success_scripts();
        let working_dir = fs::canonicalize(fixture.dir.path()).unwrap();
        let mut backend = SimulationBackend::new(fixture.config()).unwrap();
        backend.evaluate(&fixture.elf).unwrap();
        // Default: the trace directory is created inside the simulator working directory.
        let vcd = fs::read_to_string(fixture.dir.path().join("vcd-path")).unwrap();
        assert!(Path::new(&vcd).starts_with(&working_dir), "{vcd}");

        // With vcd_dir set, the trace directory is created there instead.
        let traces = fixture.dir.path().join("traces");
        fs::create_dir(&traces).unwrap();
        let vcd_root = fs::canonicalize(&traces).unwrap();
        let mut config = fixture.config();
        config.vcd_dir = Some(vcd_root.clone());
        let mut backend = SimulationBackend::new(config).unwrap();
        backend.evaluate(&fixture.elf).unwrap();
        let vcd = fs::read_to_string(fixture.dir.path().join("vcd-path")).unwrap();
        assert!(Path::new(&vcd).starts_with(&vcd_root), "{vcd}");
        assert!(!Path::new(&vcd).exists());

        // A vcd_dir that is not a directory is a configuration error.
        let mut config = fixture.config();
        config.vcd_dir = Some(fixture.dir.path().join("gone"));
        assert!(matches!(
            SimulationBackend::new(config),
            Err(EvaluationError::Fatal(_))
        ));
    }

    #[test]
    fn stale_vcd_directories_of_dead_processes_are_swept() {
        let fixture = Fixture::new();
        fixture.success_scripts();
        let parent = fixture.dir.path();
        let host = hostname();
        let own_pid = std::process::id() as i32;
        let stale = parent.join(format!("{VCD_DIR_PREFIX}{host}-999999-old"));
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("trace.vcd"), b"wave").unwrap();
        let mine = parent.join(format!("{VCD_DIR_PREFIX}{host}-{own_pid}-mine"));
        fs::create_dir(&mine).unwrap();
        let other_host = parent.join(format!("{VCD_DIR_PREFIX}other-host-999999-x"));
        fs::create_dir(&other_host).unwrap();

        let removed = remove_stale_vcd_dirs(parent, &host, |pid| pid == own_pid);
        assert_eq!(removed, 1);
        assert!(!stale.exists());
        assert!(mine.exists(), "live process directory must be kept");
        assert!(other_host.exists(), "other host's directory must be kept");
        fs::remove_dir_all(&mine).unwrap();
        fs::remove_dir_all(&other_host).unwrap();

        // A full evaluation sweeps too and names its directory with host and pid.
        let mut backend = SimulationBackend::new(fixture.config()).unwrap();
        backend.evaluate(&fixture.elf).unwrap();
        let vcd = fs::read_to_string(parent.join("vcd-path")).unwrap();
        let dir_name = Path::new(&vcd)
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap();
        assert!(
            dir_name.starts_with(&format!("{VCD_DIR_PREFIX}{host}-{own_pid}-")),
            "{dir_name}"
        );
        assert!(!Path::new(&vcd).exists());
    }

    #[test]
    fn chipyard_cycle_limit_is_analyzed_but_other_failures_are_rejected() {
        let fixture = Fixture::new();
        fixture.success_scripts();
        let mut backend = SimulationBackend::new(fixture.config()).unwrap();

        for termination in ["exit 255", "kill -ABRT $$"] {
            Fixture::script(
                &fixture.simulator,
                &format!(
                    "printf waveform > \"$2\"\n\
                     head -c 70000 /dev/zero | tr '\\0' x >&2\n\
                     printf '\\n*** FAILED ***   (timeout) after     1001 simulation cycles\\n' >&2\n\
                     {termination}"
                ),
            );
            assert_eq!(backend.evaluate(&fixture.elf).unwrap().score(), 0.75);
        }

        for (marker, termination) in [
            (
                "*** FAILED *** (timeout) after 1001 simulation cycles",
                "exit 1",
            ),
            (
                "*** FAILED *** (timeout) after nope simulation cycles",
                "exit 255",
            ),
            ("*** FAILED *** other error", "kill -ABRT $$"),
        ] {
            Fixture::script(
                &fixture.simulator,
                &format!("printf waveform > \"$2\"; echo '{marker}' >&2; {termination}"),
            );
            assert!(matches!(
                backend.evaluate(&fixture.elf),
                Err(EvaluationError::Candidate(_))
            ));
        }
    }

    #[test]
    fn passes_placeholders_within_arguments_and_exclusions() {
        let fixture = Fixture::new();
        let excluded = fixture.dir.path().join("excluded signals.txt");
        fs::write(&excluded, "signal\n").unwrap();
        Fixture::script(
            &fixture.simulator,
            r#"
case "$1" in --elf=*) elf=${1#--elf=};; *) exit 2;; esac
case "$2" in --vcd=*) vcd=${2#--vcd=};; *) exit 2;; esac
test -f "$elf" || exit 3
printf 'waveform' > "$vcd"
"#,
        );
        Fixture::script(
            &fixture.analyzer,
            r#"
test "$4" = "--exclude-signal-file" && test -f "$5" || exit 4
printf 'toggle_activity=0\ntoggle_coverage=1\n'
"#,
        );
        let mut config = fixture.config();
        config.simulator_args = vec!["--elf={elf}".into(), "--vcd={vcd}".into()];
        config.exclude_signal_file = Some(excluded);
        assert_eq!(
            SimulationBackend::new(config)
                .unwrap()
                .evaluate(&fixture.elf)
                .unwrap()
                .score(),
            1.0
        );
    }

    #[test]
    fn reports_bad_configuration() {
        let fixture = Fixture::new();
        fixture.success_scripts();
        let mut config = fixture.config();
        config.simulator_args = vec!["{elf}".into()];
        assert!(matches!(
            SimulationBackend::new(config),
            Err(EvaluationError::Fatal(_))
        ));
        let mut config = fixture.config();
        config.target_scope = "".into();
        assert!(matches!(
            SimulationBackend::new(config),
            Err(EvaluationError::Fatal(_))
        ));
        let mut config = fixture.config();
        config.simulator_timeout = Duration::ZERO;
        assert!(matches!(
            SimulationBackend::new(config),
            Err(EvaluationError::Fatal(_))
        ));
    }

    #[test]
    fn rejects_missing_trace_and_invalid_analyzer_output() {
        let fixture = Fixture::new();
        fixture.success_scripts();
        Fixture::script(&fixture.simulator, "exit 0");
        let mut backend = SimulationBackend::new(fixture.config()).unwrap();
        for _ in 1..MAX_MEASUREMENT_FAILURES {
            let result = backend.evaluate(&fixture.elf);
            assert!(
                matches!(result, Err(EvaluationError::Measurement(_))),
                "{result:?}"
            );
        }
        assert!(matches!(
            backend.evaluate(&fixture.elf),
            Err(EvaluationError::Fatal(_))
        ));
        fixture.success_scripts();
        Fixture::script(
            &fixture.analyzer,
            "printf 'toggle_coverage=NaN\\ntoggle_activity=1\\n'",
        );
        let mut backend = SimulationBackend::new(fixture.config()).unwrap();
        assert!(matches!(
            backend.evaluate(&fixture.elf),
            Err(EvaluationError::Measurement(_))
        ));
        let vcd = fs::read_to_string(fixture.dir.path().join("vcd-path")).unwrap();
        assert!(!Path::new(&vcd).exists());
    }

    #[test]
    fn nonzero_and_timeout_failures_are_reported_and_bounded() {
        let fixture = Fixture::new();
        fixture.success_scripts();
        Fixture::script(&fixture.simulator, "echo failure >&2; exit 7");
        let mut backend = SimulationBackend::new(fixture.config()).unwrap();
        for _ in 1..MAX_CANDIDATE_FAILURES {
            let error = backend.evaluate(&fixture.elf).unwrap_err();
            assert!(matches!(error, EvaluationError::Candidate(_)), "{error:?}");
            assert!(error.to_string().contains("failure"));
        }
        assert!(matches!(
            backend.evaluate(&fixture.elf),
            Err(EvaluationError::Fatal(_))
        ));

        Fixture::script(&fixture.simulator, "sleep 30 & wait");
        let mut config = fixture.config();
        config.simulator_timeout = Duration::from_millis(100);
        let mut backend = SimulationBackend::new(config).unwrap();
        let start = Instant::now();
        let error = backend.evaluate(&fixture.elf).unwrap_err();
        assert!(matches!(error, EvaluationError::Candidate(_)));
        assert!(error.to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn analyzer_failure_and_timeout_are_measurement_errors() {
        let fixture = Fixture::new();
        fixture.success_scripts();
        Fixture::script(&fixture.analyzer, "echo analyzer-error >&2; exit 1");
        let mut backend = SimulationBackend::new(fixture.config()).unwrap();
        let error = backend.evaluate(&fixture.elf).unwrap_err();
        assert!(matches!(error, EvaluationError::Measurement(_)));
        assert!(error.to_string().contains("analyzer-error"));

        Fixture::script(&fixture.analyzer, "sleep 30 & wait");
        let mut config = fixture.config();
        config.analyzer_timeout = Duration::from_millis(100);
        let start = Instant::now();
        let error = SimulationBackend::new(config)
            .unwrap()
            .evaluate(&fixture.elf)
            .unwrap_err();
        assert!(matches!(error, EvaluationError::Measurement(_)));
        assert!(error.to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn successful_run_resets_failure_counters() {
        let fixture = Fixture::new();
        fixture.success_scripts();
        let mut backend = SimulationBackend::new(fixture.config()).unwrap();
        Fixture::script(&fixture.simulator, "exit 1");
        for _ in 1..MAX_CANDIDATE_FAILURES {
            assert!(backend.evaluate(&fixture.elf).is_err());
        }
        fixture.success_scripts();
        assert!(backend.evaluate(&fixture.elf).is_ok());
        Fixture::script(&fixture.simulator, "exit 1");
        assert!(matches!(
            backend.evaluate(&fixture.elf),
            Err(EvaluationError::Candidate(_))
        ));
    }

    #[test]
    fn real_analyzer_smoke_test_when_configured() {
        let Some(analyzer) = std::env::var_os("VCDTOGGLERATE_TEST_BINARY") else {
            return;
        };
        let fixture = Fixture::new();
        fs::write(
            fixture.dir.path().join("input.vcd"),
            "\
$timescale 1ns $end
$scope module TOP $end
$scope module dut $end
$var wire 1 ! value $end
$upscope $end
$upscope $end
$enddefinitions $end
#0
0!
#10
1!
",
        )
        .unwrap();
        Fixture::script(&fixture.simulator, "cp input.vcd \"$2\"");
        let mut config = fixture.config();
        config.analyzer = analyzer.into();
        let result = SimulationBackend::new(config)
            .unwrap()
            .evaluate(&fixture.elf)
            .unwrap();
        assert_eq!(result.score(), 1.0);
        assert!(result.toggle_activity_per_second().unwrap() > 0.0);
    }
}
