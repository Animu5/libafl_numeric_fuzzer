//! Simulation supervisor: the private child process that runs the simulator with
//! timeouts and process-group cleanup. The VCD it produces is analyzed in-process
//! by the parent worker (see `simulation::SimulationBackend::analyze_vcd`), so the
//! trace map stays in the worker that needs it for feedback.
//! Spawned by `SimulationBackend::run` via `--internal-simulation-supervisor`.

use crate::evaluation::EvaluationError;
use crate::simulation::OUTPUT_LIMIT;
use std::{
    ffi::OsString,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub(crate) struct ProcessOutput {
    pub(crate) status: ExitStatus,
    pub(crate) timed_out: bool,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

impl ProcessOutput {
    pub(crate) fn diagnostic(&self) -> String {
        format!(
            "stderr: {:?}; stdout: {:?}",
            String::from_utf8_lossy(&self.stderr),
            String::from_utf8_lossy(&self.stdout)
        )
    }

    pub(crate) fn chipyard_cycle_limit(&self) -> bool {
        use std::os::unix::process::ExitStatusExt;
        if self.status.code() != Some(255) && self.status.signal() != Some(libc::SIGABRT) {
            return false;
        }
        String::from_utf8_lossy(&self.stderr).lines().any(|line| {
            let Some(rest) = line.strip_prefix("*** FAILED ***") else {
                return false;
            };
            let Some(cycles) = rest.trim().strip_prefix("(timeout) after") else {
                return false;
            };
            let Some(cycles) = cycles.trim().strip_suffix(" simulation cycles") else {
                return false;
            };
            !cycles.is_empty() && cycles.bytes().all(|byte| byte.is_ascii_digit())
        })
    }
}

fn read_bounded(mut reader: impl Read) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let keep = count.min(OUTPUT_LIMIT.saturating_sub(output.len()));
        output.extend_from_slice(&buffer[..keep]);
    }
    Ok(output)
}

fn read_bounded_tail(mut reader: impl Read) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(output);
        }
        let excess = (output.len() + count).saturating_sub(OUTPUT_LIMIT);
        if excess != 0 {
            output.drain(..excess);
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

pub(crate) fn run_process(
    command: &mut Command,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> io::Result<ProcessOutput> {
    use std::os::unix::process::CommandExt;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if cancelled.load(Ordering::Acquire) {
        return Err(io::Error::other("Fuzzer process exited"));
    }
    let mut child = command.spawn()?;
    let group = child.id() as libc::pid_t;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let out_thread = thread::spawn(move || read_bounded(stdout));
    let err_thread = thread::spawn(move || read_bounded_tail(stderr));
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if cancelled.load(Ordering::Acquire) => {
                let killed = kill_group(group);
                let waited = child.wait();
                break killed
                    .and(waited)
                    .and_then(|_| Err(io::Error::other("Fuzzer process exited")));
            }
            Ok(None) if start.elapsed() < timeout => thread::sleep(
                Duration::from_millis(10).min(timeout.saturating_sub(start.elapsed())),
            ),
            Ok(None) => {
                timed_out = true;
                break kill_group(group).and_then(|()| child.wait());
            }
            Err(e) => {
                let _ = kill_group(group);
                let _ = child.wait();
                break Err(e);
            }
        }
    };
    // Close inherited pipes even if a successful leader left child processes behind.
    let cleanup = kill_group(group);
    let stdout = out_thread
        .join()
        .map_err(|_| io::Error::other("stdout reader panicked"))?;
    let stderr = err_thread
        .join()
        .map_err(|_| io::Error::other("stderr reader panicked"))?;
    let status = status?;
    cleanup?;
    let stdout = stdout?;
    let stderr = stderr?;
    Ok(ProcessOutput {
        status,
        timed_out,
        stdout,
        stderr,
    })
}

fn kill_group(group: libc::pid_t) -> io::Result<()> {
    // The child owns a separate process group; negative PID addresses that group only.
    let result = unsafe { libc::kill(-group, libc::SIGKILL) };
    if result == 0 {
        return Ok(());
    }

    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

/// Runs the simulator as a supervised child: working directory and `TMPDIR` set,
/// core dumps disabled, stdout/stderr captured with a size limit, killed (with its
/// whole process group) on timeout or parent death.
pub(crate) fn run_simulator_process(
    simulator: &Path,
    working_dir: &Path,
    args: &[OsString],
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<ProcessOutput, EvaluationError> {
    let mut command = Command::new(simulator);
    command
        .current_dir(working_dir)
        // Keep subprocess temp files off the size-limited /tmp.
        .env("TMPDIR", working_dir)
        .args(args);
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
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
    run_process(&mut command, timeout, cancelled)
        .map_err(|e| EvaluationError::Fatal(format!("Simulator process: {e}")))
}

/// Maps a finished simulator run to an outcome: timeouts and nonzero exits are
/// candidate failures (except Chipyard's cycle-limit abort), a missing VCD is a
/// measurement failure.
pub(crate) fn classify_simulator_output(
    output: &ProcessOutput,
    vcd: &Path,
) -> Result<(), EvaluationError> {
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
    Ok(())
}

/// Entry point for the private child process. Its stdin is a parent-liveness pipe.
/// Arguments: `<elf> <simulator> <working_dir> <vcd> <sim_secs> <sim_nanos> <simulator args...>`
/// (the parent already expanded `{elf}`/`{vcd}`). Exit codes: 0 success, 10 candidate
/// failure, 11 measurement failure, 12 fatal.
pub fn supervisor_main() -> i32 {
    // The parent captures this process's stderr, so log there (default debug) to make
    // failures carry the full simulator command line.
    let _ = tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug")),
        )
        .try_init();
    let cancelled = Arc::new(AtomicBool::new(false));
    let reader_cancelled = Arc::clone(&cancelled);
    thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut buffer = [0; 128];
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) | Err(_) => {
                    reader_cancelled.store(true, Ordering::Release);
                    break;
                }
                Ok(_) => {}
            }
        }
    });

    match supervisor_run(&cancelled) {
        Ok(()) => 0,
        Err(EvaluationError::Candidate(message)) => {
            eprintln!("{message}");
            10
        }
        Err(EvaluationError::Measurement(message)) => {
            eprintln!("{message}");
            11
        }
        Err(EvaluationError::Fatal(message)) => {
            eprintln!("{message}");
            12
        }
    }
}

fn supervisor_run(cancelled: &AtomicBool) -> Result<(), EvaluationError> {
    let mut args = std::env::args_os().skip(2);
    let mut next = |name: &str| {
        args.next()
            .ok_or_else(|| EvaluationError::Fatal(format!("Missing supervisor argument: {name}")))
    };
    let elf = PathBuf::from(next("ELF")?);
    let simulator = PathBuf::from(next("simulator")?);
    let working_dir = PathBuf::from(next("working directory")?);
    let vcd = PathBuf::from(next("VCD path")?);
    let mut number = |name: &str| -> Result<u64, EvaluationError> {
        next(name)?
            .to_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| EvaluationError::Fatal(format!("Invalid supervisor argument: {name}")))
    };
    let sim_secs = number("simulator timeout seconds")?;
    let sim_nanos = number("simulator timeout nanoseconds")?;
    if sim_nanos >= 1_000_000_000 {
        return Err(EvaluationError::Fatal(
            "Invalid supervisor timeout nanoseconds".into(),
        ));
    }
    let simulator_args: Vec<OsString> = args.collect();
    tracing::debug!(
        "Supervisor running simulator {:?} in {:?} for ELF {} (vcd {}, args {:?})",
        simulator,
        working_dir,
        elf.display(),
        vcd.display(),
        simulator_args
    );
    let output = run_simulator_process(
        &simulator,
        &working_dir,
        &simulator_args,
        Duration::new(sim_secs, sim_nanos as u32),
        cancelled,
    )?;
    tracing::debug!(
        "Simulator exited {} (timed_out={})",
        output.status,
        output.timed_out
    );
    classify_simulator_output(&output, &vcd)
}
