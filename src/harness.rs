use crate::feedback::Shared;
use crate::measurement::Measurement;
use crate::openocd::OpenOcd;
use crate::snippet::SnippetBuilder;
use libafl::{
    executors::ExitKind,
    inputs::{BytesInput, HasTargetBytes},
};
use libafl_bolts::ToSlice;
use std::{rc::Rc, thread, time::Duration};

/// Number of consecutive failed measurements tolerated before aborting.
const MAX_MEASUREMENT_FAILURES: u32 = 5;

/// Number of consecutive target failures (e.g. crashing snippets) tolerated before aborting.
const MAX_TARGET_FAILURES: u32 = 10;

/// Runs one fuzz input on the target and records the scope's running-minus-idle delta in `shared`.
pub struct Harness {
    pub openocd: OpenOcd,
    /// Decodes and builds each input into an ELF.
    pub builder: SnippetBuilder,
    /// Scope measurement of the running-minus-idle delta.
    pub measurement: Box<dyn Measurement>,
    /// Channel to the feedback (delta and fatal errors).
    pub shared: Rc<Shared>,
    /// Wait after resume before the running measurement.
    pub settle: Duration,
    /// Consecutive failed measurements.
    pub failures: u32,
    /// Consecutive runs in which the target misbehaved.
    pub target_failures: u32,
}

impl Harness {
    /// Records a fatal error for the feedback to raise and ends the run.
    fn fatal(&self, msg: String) -> ExitKind {
        *self.shared.fatal.borrow_mut() = Some(msg);
        ExitKind::Ok
    }

    /// Handles an OpenOCD error caused by the current input, e.g. a snippet that crashed or
    /// wedged the target. Tries to reset the target; if that works the input is skipped,
    /// otherwise (OpenOCD gone, target unrecoverable, too many failures in a row) the run aborts.
    fn target_failed(&mut self, what: &str, err: std::io::Error) -> ExitKind {
        self.target_failures += 1;
        tracing::warn!("Skipping input, {what} failed: {err}");
        if self.target_failures >= MAX_TARGET_FAILURES {
            return self.fatal(format!(
                "{} consecutive target failures, last: {what}: {err}",
                self.target_failures
            ));
        }
        match self.openocd.reset_halt() {
            Ok(()) => ExitKind::Ok,
            Err(e) => self.fatal(format!(
                "Target did not recover after {what} failed ({err}): {e}"
            )),
        }
    }

    /// Builds, loads, and measures one input; the result goes to `shared`.
    pub fn run(&mut self, input: &BytesInput) -> ExitKind {
        let bytes = input.target_bytes();
        let data = bytes.to_slice();

        // A candidate that cannot be decoded or built is skipped, not fatal.
        let elf = match self.builder.build(data) {
            Ok(elf) => elf,
            Err(e) => {
                tracing::warn!("Skipping input: {e}");
                return ExitKind::Ok;
            }
        };

        if let Err(e) = self.openocd.load_program(&elf) {
            return self.target_failed("loading the program", e);
        }

        // Idle capture: target is halted after reset.
        let idle = self.measurement.capture_idle();

        if let Err(e) = self.openocd.resume() {
            return self.target_failed("resume", e);
        }

        // Give the target some time to execute before measuring.
        thread::sleep(self.settle);

        match idle.and_then(|()| self.measurement.measure_delta()) {
            Ok(delta) => {
                self.failures = 0;
                self.target_failures = 0;
                tracing::debug!("delta={delta}");
                self.shared.delta.set(Some(delta));
            }
            Err(e) => {
                tracing::error!("Measurement failed: {}", e);
                self.failures += 1;
                if self.failures >= MAX_MEASUREMENT_FAILURES {
                    return self.fatal(format!(
                        "{} consecutive measurement failures",
                        self.failures
                    ));
                }
            }
        }

        ExitKind::Ok
    }
}
