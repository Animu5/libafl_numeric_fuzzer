use crate::evaluation::{Evaluation, EvaluationBackend, EvaluationError};
use crate::vcdtoggle::{self, excludes::Excluded};
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
    pub target_scope: String,
    pub exclude_signal_file: Option<PathBuf>,
    /// Parent directory for the per-run VCD directory. Defaults to `working_dir` so large
    /// traces are not written to the (often size-limited) system temp directory.
    pub vcd_dir: Option<PathBuf>,
    pub simulator_timeout: Duration,
}

pub struct SimulationBackend {
    config: SimulationConfig,
    excluded: Excluded,
    candidate_failures: u32,
    measurement_failures: u32,
}

impl SimulationBackend {
    pub fn new(mut config: SimulationConfig) -> Result<Self, EvaluationError> {
        let cwd = std::env::current_dir().map_err(|e| {
            EvaluationError::Fatal(format!("Cannot resolve current directory: {e}"))
        })?;
        config.simulator = absolute(&cwd, &config.simulator);
        config.working_dir = absolute(&cwd, &config.working_dir);
        config.exclude_signal_file = config.exclude_signal_file.map(|p| absolute(&cwd, &p));
        config.vcd_dir = config.vcd_dir.map(|p| absolute(&cwd, &p));
        if !config.working_dir.is_dir() {
            return Err(EvaluationError::Fatal(format!(
                "Simulation working directory is not a directory: {}",
                config.working_dir.display()
            )));
        }
        if !config.simulator.is_file() {
            return Err(EvaluationError::Fatal(format!(
                "simulator executable is not a file: {}",
                config.simulator.display()
            )));
        }
        if let Some(path) = &config.exclude_signal_file
            && !path.is_file()
        {
            return Err(EvaluationError::Fatal(format!(
                "Exclusion file is not a file: {}",
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
            return Err(EvaluationError::Fatal("Target scope is empty".into()));
        }
        if config.simulator_timeout.is_zero() {
            return Err(EvaluationError::Fatal("Simulator timeout must be nonzero".into()));
        }
        if !config.simulator_args.iter().any(|a| contains(a, b"{elf}"))
            || !config.simulator_args.iter().any(|a| contains(a, b"{vcd}"))
        {
            return Err(EvaluationError::Fatal(
                "Simulator arguments must include both {elf} and {vcd} placeholders".into(),
            ));
        }
        let excluded = vcdtoggle::excludes::load(config.exclude_signal_file.as_deref())
            .map_err(|e| {
                EvaluationError::Fatal(format!(
                    "Cannot load exclusion file {:?}: {e}",
                    config.exclude_signal_file
                ))
            })?;
        Ok(Self {
            config,
            excluded,
            candidate_failures: 0,
            measurement_failures: 0,
        })
    }

    #[cfg(test)]
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
        let (work, vcd) = self.prepare_trace_dir()?;
        let result = self
            .run_simulator(&elf, &vcd, cancelled)
            .and_then(|()| self.analyze_vcd(&vcd));
        work.close().map_err(|e| {
            EvaluationError::Fatal(format!("Cannot clean up simulation artifacts: {e}"))
        })?;
        result
    }

    /// Creates the per-run trace directory (sweeping leftovers of dead same-host
    /// processes first) and returns it together with the VCD path inside it.
    fn prepare_trace_dir(&self) -> Result<(tempfile::TempDir, PathBuf), EvaluationError> {
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
        Ok((work, run_dir.join("trace.vcd")))
    }

    /// Runs the simulator on `elf`, writing the trace to `vcd`, and classifies the outcome.
    #[cfg(test)]
    fn run_simulator(
        &self,
        elf: &Path,
        vcd: &Path,
        cancelled: &AtomicBool,
    ) -> Result<(), EvaluationError> {
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
        let output = crate::supervisor::run_simulator_process(
            &self.config.simulator,
            &self.config.working_dir,
            &args,
            self.config.simulator_timeout,
            cancelled,
        )?;
        tracing::debug!(
            "Simulator exited {} (timed_out={}); VCD {} exists: {}",
            output.status,
            output.timed_out,
            vcd.display(),
            vcd.is_file()
        );
        crate::supervisor::classify_simulator_output(&output, vcd)
    }

    /// Analyzes a finished trace in-process with the integrated VCD engine and builds
    /// the evaluation: the trace map plus toggle activity (score) and coverage.
    fn analyze_vcd(&self, vcd: &Path) -> Result<Evaluation, EvaluationError> {
        tracing::debug!(
            "Analyzing VCD {} in-process (scope {:?}, {} exclusions)",
            vcd.display(),
            self.config.target_scope,
            self.excluded.len()
        );
        let metrics = vcdtoggle::analyze(vcd, &self.config.target_scope, &self.excluded).map_err(
            |e| {
                EvaluationError::Measurement(format!("VCD analysis of {} failed: {e}", vcd.display()))
            },
        )?;
        let evaluation =
            Evaluation::toggle_metrics(metrics.activity(), metrics.coverage(), metrics.trace_map())?;
        tracing::debug!(
            "VCD analysis: toggles={} activity={}/s coverage={} map coverage={} map_slots={}",
            metrics.total_toggles,
            evaluation.score(),
            crate::evaluation::format_percentage(evaluation.toggle_coverage().unwrap_or(0.0)),
            crate::evaluation::format_percentage(evaluation.map_coverage().unwrap_or(0.0)),
            evaluation.trace_map().map_or(0, <[u8]>::len)
        );
        Ok(evaluation)
    }

    /// Production path: the simulator runs in the private supervisor child (timeouts,
    /// process-group cleanup, parent-liveness watch); the VCD is then analyzed
    /// in-process here so the trace map stays in this worker.
    #[cfg(not(test))]
    fn run(&self, elf: &Path) -> Result<Evaluation, EvaluationError> {
        use std::os::unix::process::CommandExt;
        let elf = std::fs::canonicalize(elf).map_err(|e| {
            EvaluationError::Fatal(format!(
                "Cannot resolve candidate ELF {}: {e}",
                elf.display()
            ))
        })?;
        let (work, vcd) = self.prepare_trace_dir()?;
        let args: Vec<OsString> = self
            .config
            .simulator_args
            .iter()
            .map(|arg| expand(arg, &elf, &vcd))
            .collect();
        let executable = std::env::current_exe().map_err(|e| {
            EvaluationError::Fatal(format!("Cannot locate supervisor executable: {e}"))
        })?;
        let mut command = Command::new(executable);
        command
            .arg("--internal-simulation-supervisor")
            .arg(&elf)
            .arg(&self.config.simulator)
            .arg(&self.config.working_dir)
            .arg(&vcd)
            .arg(self.config.simulator_timeout.as_secs().to_string())
            .arg(self.config.simulator_timeout.subsec_nanos().to_string())
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        tracing::debug!(
            "Starting simulation supervisor for ELF {} (simulator {:?}, working dir {:?}, args {:?})",
            elf.display(),
            self.config.simulator,
            self.config.working_dir,
            args
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
        let result = match output.status.code() {
            Some(0) => self.analyze_vcd(&vcd),
            Some(10) => Err(EvaluationError::Candidate(stderr.into_owned())),
            Some(11) => Err(EvaluationError::Measurement(stderr.into_owned())),
            _ => Err(EvaluationError::Fatal(format!(
                "Simulation supervisor exited {}: {stderr}",
                output.status
            ))),
        };
        work.close().map_err(|e| {
            EvaluationError::Fatal(format!("Cannot clean up simulation artifacts: {e}"))
        })?;
        result
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
    use crate::evaluation::Metric;
    use std::{fs, os::unix::fs::PermissionsExt, time::Instant};

    static FIXTURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Minimal but real VCD: signal `value` toggles twice (0→1→0), the 4-bit vector
    /// `nibble` changes once from b0000 to b0101 (2 of its 4 bits). Last change at #20
    /// with a 1ns timescale, so activity = 3 toggles / 20ns = 1.5e8 toggles/s.
    /// Bit-level coverage counts all 5 bits (`value` is a 1-bit vector), of which 3
    /// changed: 0.6.
    const FIXTURE_VCD: &str = "\
$timescale 1ns $end
$scope module TOP $end
$scope module dut $end
$var wire 1 ! value $end
$var wire 4 \" nibble $end
$upscope $end
$upscope $end
$enddefinitions $end
#0
0!
b0000 \"
#10
1!
b0101 \"
#20
0!
";

    const FIXTURE_ACTIVITY: f64 = 150_000_000.0;
    const FIXTURE_COVERAGE: f64 = 0.6;

    struct Fixture {
        _guard: std::sync::MutexGuard<'static, ()>,
        dir: tempfile::TempDir,
        elf: PathBuf,
        simulator: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let guard = FIXTURE_LOCK.lock().unwrap();
            let dir = tempfile::tempdir().unwrap();
            let elf = dir.path().join("candidate with space.elf");
            let simulator = dir.path().join("simulator");
            fs::write(&elf, b"ELF").unwrap();
            fs::write(dir.path().join("fixture.vcd"), FIXTURE_VCD).unwrap();
            Self {
                _guard: guard,
                dir,
                elf,
                simulator,
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
                target_scope: "TOP.dut".into(),
                exclude_signal_file: None,
                vcd_dir: None,
                simulator_timeout: Duration::from_secs(2),
            }
        }

        fn success_scripts(&self) {
            Self::script(
                &self.simulator,
                r#"
test -f "$1" || exit 3
printf '%s' "$2" > vcd-path
cp fixture.vcd "$2"
printf '%s' "${TMPDIR:-}" > tmpdir
"#,
            );
        }

        /// Asserts the fixture's metrics: activity 1.5e8, coverage 0.5, and a two-entry
        /// trace map with saturating per-signal toggle counts {1, 2}.
        fn assert_fixture_metrics(result: &Evaluation) {
            assert_eq!(result.metric(), Metric::ToggleActivity);
            assert_eq!(result.score(), FIXTURE_ACTIVITY);
            assert_eq!(result.toggle_coverage(), Some(FIXTURE_COVERAGE));
            let mut map = result.trace_map().unwrap().to_vec();
            map.sort_unstable();
            assert_eq!(map, vec![1, 2]);
        }
    }

    #[test]
    fn runs_simulator_and_analyzes_trace_with_fresh_cleaned_trace() {
        let fixture = Fixture::new();
        fixture.success_scripts();
        let mut backend = SimulationBackend::new(fixture.config()).unwrap();
        let result = backend.evaluate(&fixture.elf).unwrap();
        Fixture::assert_fixture_metrics(&result);
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
                    "cp fixture.vcd \"$2\"\n\
                     head -c 70000 /dev/zero | tr '\\0' x >&2\n\
                     printf '\\n*** FAILED ***   (timeout) after     1001 simulation cycles\\n' >&2\n\
                     {termination}"
                ),
            );
            let result = backend.evaluate(&fixture.elf).unwrap();
            Fixture::assert_fixture_metrics(&result);
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
                &format!("cp fixture.vcd \"$2\"; echo '{marker}' >&2; {termination}"),
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
        fs::write(&excluded, "TOP.dut.nibble\n").unwrap();
        Fixture::script(
            &fixture.simulator,
            r#"
case "$1" in --elf=*) elf=${1#--elf=};; *) exit 2;; esac
case "$2" in --vcd=*) vcd=${2#--vcd=};; *) exit 2;; esac
test -f "$elf" || exit 3
cp fixture.vcd "$vcd"
"#,
        );
        let mut config = fixture.config();
        config.simulator_args = vec!["--elf={elf}".into(), "--vcd={vcd}".into()];
        config.exclude_signal_file = Some(excluded);
        let result = SimulationBackend::new(config)
            .unwrap()
            .evaluate(&fixture.elf)
            .unwrap();
        // With `nibble` excluded only `value` counts: 2 toggles, and its single bit
        // changed, so coverage is 1.0.
        assert_eq!(result.metric(), Metric::ToggleActivity);
        assert_eq!(result.score(), 100_000_000.0);
        assert_eq!(result.toggle_coverage(), Some(1.0));
        let map = result.trace_map().unwrap();
        assert_eq!(
            map.iter().copied().filter(|&b| b > 0).collect::<Vec<_>>(),
            vec![2]
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
    fn rejects_missing_trace_and_unanalyzable_vcd() {
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
        // A trace that is not a waveform at all fails the in-process analysis.
        Fixture::script(&fixture.simulator, "printf 'waveform' > \"$2\"; printf '%s' \"$2\" > vcd-path");
        let mut backend = SimulationBackend::new(fixture.config()).unwrap();
        let result = backend.evaluate(&fixture.elf);
        assert!(
            matches!(result, Err(EvaluationError::Measurement(_))),
            "{result:?}"
        );
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
    fn unknown_target_scope_is_a_measurement_error() {
        let fixture = Fixture::new();
        fixture.success_scripts();
        let mut config = fixture.config();
        config.target_scope = "TOP.missing".into();
        let mut backend = SimulationBackend::new(config).unwrap();
        let result = backend.evaluate(&fixture.elf);
        assert!(
            matches!(result, Err(EvaluationError::Measurement(_))),
            "{result:?}"
        );
        assert!(
            result.unwrap_err().to_string().contains("TOP.missing"),
            "error names the missing scope"
        );
    }
}
