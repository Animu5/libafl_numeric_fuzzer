//! Simulation supervisor: the private child process that runs the simulator and
//! analyzer with timeouts, process-group cleanup, and VCD lifecycle management.
//! Spawned by `SimulationBackend::run` via `--internal-simulation-supervisor`.

use crate::evaluation::{Evaluation, EvaluationError};
use crate::simulation::{OUTPUT_LIMIT, SimulationBackend, SimulationConfig};
use std::{
    io::{self, Read},
    path::PathBuf,
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
    pub(crate) stdout_truncated: bool,
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

fn read_bounded(mut reader: impl Read) -> io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut truncated = false;
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let keep = count.min(OUTPUT_LIMIT.saturating_sub(output.len()));
        output.extend_from_slice(&buffer[..keep]);
        truncated |= keep < count;
    }
    Ok((output, truncated))
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
    let (stdout, stdout_truncated) = stdout?;
    let stderr = stderr?;
    Ok(ProcessOutput {
        status,
        timed_out,
        stdout,
        stdout_truncated,
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

/// Entry point for the private child process. Its stdin is a parent-liveness pipe.
pub fn supervisor_main() -> i32 {
    // The parent captures this process's stderr, so log there (default debug) to make
    // failures carry the full simulator and analyzer command lines.
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

    match supervisor_evaluate(&cancelled) {
        Ok(result) => {
            println!(
                "toggle_activity={}",
                result
                    .toggle_activity_per_second()
                    .expect("coverage has activity")
            );
            println!("toggle_coverage={}", result.score());
            0
        }
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

fn supervisor_evaluate(cancelled: &AtomicBool) -> Result<Evaluation, EvaluationError> {
    let mut args = std::env::args_os().skip(2);
    let mut next = |name| {
        args.next()
            .ok_or_else(|| EvaluationError::Fatal(format!("Missing supervisor argument: {name}")))
    };
    let elf = PathBuf::from(next("ELF")?);
    let simulator = PathBuf::from(next("simulator")?);
    let working_dir = PathBuf::from(next("working directory")?);
    let analyzer = PathBuf::from(next("analyzer")?);
    let target_scope = next("target scope")?
        .into_string()
        .map_err(|_| EvaluationError::Fatal("Target scope is not UTF-8".into()))?;
    let exclusion = PathBuf::from(next("exclusions")?);
    let vcd_dir = PathBuf::from(next("vcd directory")?);
    let mut number = |name| -> Result<u64, EvaluationError> {
        next(name)?
            .to_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| EvaluationError::Fatal(format!("Invalid supervisor argument: {name}")))
    };
    let sim_secs = number("simulator timeout seconds")?;
    let sim_nanos = number("simulator timeout nanoseconds")?;
    let analyzer_secs = number("analyzer timeout seconds")?;
    let analyzer_nanos = number("analyzer timeout nanoseconds")?;
    if sim_nanos >= 1_000_000_000 || analyzer_nanos >= 1_000_000_000 {
        return Err(EvaluationError::Fatal(
            "Invalid supervisor timeout nanoseconds".into(),
        ));
    }
    let config = SimulationConfig {
        simulator,
        simulator_args: args.collect(),
        working_dir,
        analyzer,
        target_scope,
        exclude_signal_file: if exclusion.as_os_str().is_empty() {
            None
        } else {
            Some(exclusion)
        },
        vcd_dir: if vcd_dir.as_os_str().is_empty() {
            None
        } else {
            Some(vcd_dir)
        },
        simulator_timeout: Duration::new(sim_secs, sim_nanos as u32),
        analyzer_timeout: Duration::new(analyzer_secs, analyzer_nanos as u32),
    };
    SimulationBackend::new(config)?.run_direct(&elf, cancelled)
}
