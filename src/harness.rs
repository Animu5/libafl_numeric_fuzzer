use crate::feedback::Shared;
use crate::measurement::{Measurement, MeasurementPhase};
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

/// Runs one fuzz input on the target and records `running - idle` in `shared`.
pub struct Harness {
    pub openocd: OpenOcd,
    /// Decodes and builds each input into an ELF.
    pub builder: SnippetBuilder,
    /// Source of the idle and running readings.
    pub measurement: Box<dyn Measurement>,
    /// Channel to the feedback (delta and fatal errors).
    pub shared: Rc<Shared>,
    /// Wait after resume before the running measurement.
    pub settle: Duration,
    /// Consecutive failed measurements.
    pub failures: u32,
}

impl Harness {
    /// Records a fatal error for the feedback to raise and ends the run.
    fn fatal(&self, msg: String) -> ExitKind {
        *self.shared.fatal.borrow_mut() = Some(msg);
        ExitKind::Ok
    }

    /// Builds, loads and measures one input; the result goes to `shared`.
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

        // Any OpenOCD error is fatal: the feedback sees it and aborts the fuzz loop.
        let setup = self.openocd.load_program(&elf);
        if let Err(e) = setup {
            return self.fatal(format!("OpenOCD setup failed: {e}"));
        }

        // Idle measurement: target is halted after reset.
        let idle = self.measurement.measure(MeasurementPhase::Idle);

        if let Err(e) = self.openocd.resume() {
            return self.fatal(format!("OpenOCD resume failed: {e}"));
        }

        // Give the target some time to execute before measuring.
        thread::sleep(self.settle);

        match (idle, self.measurement.measure(MeasurementPhase::Run)) {
            (Ok(idle), Ok(running)) => {
                self.failures = 0;
                tracing::debug!("idle={idle} running={running} delta={}", running - idle);
                self.shared.delta.set(Some(running - idle));
            }
            (idle, running) => {
                for e in [idle.err(), running.err()].into_iter().flatten() {
                    tracing::error!("Measurement failed: {}", e);
                }
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
