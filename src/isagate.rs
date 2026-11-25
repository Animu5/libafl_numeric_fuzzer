//! The ISA gate: an external validator/fixer for grammar-generated snippets.
//!
//! The gate is a separate executable (see `atnwalk/cmd/isagate` in the
//! asltgen-afl-proxy project) with a simple contract:
//!
//! * the candidate assembly program is written to its **stdin**;
//! * it executes the program on its ISA model and **fixes** what would crash
//!   (misaligned or out-of-map memory accesses get a clamped offset or an
//!   inserted `li xN, <valid address>`), then prints the corrected program to
//!   **stdout** and exits 0;
//! * if the program cannot be fixed, it exits nonzero (the stderr tail says why);
//! * it is killed on timeout.
//!
//! The harness runs every grammar input through the gate before building it;
//! rejected inputs are skipped (counted in [`crate::feedback::Shared`]) and
//! never reach `make` or the backend.

use crate::supervisor::{ProcessOutput, run_process_with_stdin};
use std::{fmt, path::PathBuf, process::Command, sync::atomic::AtomicBool, time::Duration};

/// Why the ISA gate rejected (or could not process) a snippet.
#[derive(Debug)]
pub enum GateError {
    /// The gate exited nonzero: the snippet cannot be fixed.
    Rejected(String),
    /// The gate did not finish within its timeout.
    Timeout,
    /// Spawn/wait failure.
    Io(String),
}

impl fmt::Display for GateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GateError::Rejected(msg) => write!(f, "ISA gate rejected the snippet: {msg}"),
            GateError::Timeout => write!(f, "ISA gate timed out"),
            GateError::Io(msg) => write!(f, "ISA gate I/O error: {msg}"),
        }
    }
}

/// Runs the external ISA gate executable with a per-run timeout.
pub struct IsaGate {
    path: PathBuf,
    timeout: Duration,
}

impl IsaGate {
    /// Creates a gate running `path` with `timeout` per invocation.
    pub fn new(path: PathBuf, timeout: Duration) -> Self {
        Self { path, timeout }
    }

    /// Checks that the gate executable exists (fails fast at startup).
    pub fn check_executable(&self) -> Result<(), GateError> {
        if self.path.is_file() {
            Ok(())
        } else {
            Err(GateError::Io(format!(
                "{} is not a file",
                self.path.display()
            )))
        }
    }

    /// Sends `source` to the gate and returns the fixed program.
    pub fn check(&self, source: &str) -> Result<String, GateError> {
        // No cancellation: the fuzzer itself is alive while the harness runs.
        let cancelled = AtomicBool::new(false);
        let mut command = Command::new(&self.path);
        let ProcessOutput {
            status,
            timed_out,
            stdout,
            stderr,
        } = run_process_with_stdin(&mut command, self.timeout, &cancelled, source.as_bytes().to_vec())
            .map_err(|e| GateError::Io(format!("running {}: {e}", self.path.display())))?;
        if timed_out {
            return Err(GateError::Timeout);
        }
        if !status.success() {
            return Err(GateError::Rejected(
                String::from_utf8_lossy(&stderr).trim().to_string(),
            ));
        }
        let fixed = String::from_utf8_lossy(&stdout).into_owned();
        if fixed.is_empty() {
            return Err(GateError::Rejected("gate produced no output".to_string()));
        }
        Ok(fixed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(path: &str, timeout_ms: u64) -> IsaGate {
        IsaGate::new(path.into(), Duration::from_millis(timeout_ms))
    }

    #[test]
    fn passthrough_gate_returns_fixed_source() {
        let g = gate("/bin/cat", 5000);
        assert_eq!(g.check("nop\n").unwrap(), "nop\n");
    }

    #[test]
    fn rejecting_gate_is_reported_with_stderr() {
        // /bin/false exits 1 with no output.
        let g = gate("/bin/false", 5000);
        assert!(matches!(g.check("nop\n"), Err(GateError::Rejected(_))));
    }

    #[test]
    fn slow_gate_times_out_and_is_killed() {
        // yes(1) runs forever without arguments, so the gate must time out.
        let g = gate("/usr/bin/yes", 100);
        let start = std::time::Instant::now();
        assert!(matches!(g.check("nop\n"), Err(GateError::Timeout)));
        assert!(start.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn missing_executable_is_an_io_error() {
        let g = gate("/nonexistent/isagate", 5000);
        assert!(matches!(g.check("nop\n"), Err(GateError::Io(_))));
        assert!(g.check_executable().is_err());
        assert!(gate("/bin/cat", 5000).check_executable().is_ok());
    }

    #[test]
    fn empty_gate_output_is_rejected() {
        let g = gate("/bin/true", 5000);
        assert!(matches!(g.check("nop\n"), Err(GateError::Rejected(_))));
    }

    #[test]
    fn gate_receives_stdin_and_its_stdout_is_returned() {
        // dd(1) copies stdin to stdout: proves the payload round-trips.
        let g = gate("/bin/dd", 5000);
        assert_eq!(g.check("nop\n").unwrap(), "nop\n");
    }
}
