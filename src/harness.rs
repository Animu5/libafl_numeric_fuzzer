use crate::feedback::Shared;
use crate::measurement::Measurement;
use crate::openocd::OpenOcd;
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
    pub measurement: Box<dyn Measurement>,
    pub shared: Rc<Shared>,
    pub input_addr: u32,
    pub settle: Duration,
    pub failures: u32,
}

impl Harness {
    fn fatal(&self, msg: String) -> ExitKind {
        *self.shared.fatal.borrow_mut() = Some(msg);
        ExitKind::Ok
    }

    pub fn run(&mut self, input: &BytesInput) -> ExitKind {
        let bytes = input.target_bytes();
        let data = bytes.to_slice();

        // Save current fuzz input locally for debugging.
        if let Err(e) = std::fs::write("current_input.bin", data) {
            tracing::warn!("Failed to write current_input.bin: {}", e);
        }

        // Any OpenOCD error is fatal: the feedback sees it and aborts the fuzz loop.
        let setup = self
            .openocd
            .reset_init()
            .and_then(|_| self.openocd.write_memory(self.input_addr, data));
        if let Err(e) = setup {
            return self.fatal(format!("OpenOCD setup failed: {e}"));
        }

        // Idle measurement: target is halted after reset.
        let idle = self.measurement.measure();

        if let Err(e) = self.openocd.resume() {
            return self.fatal(format!("OpenOCD resume failed: {e}"));
        }

        // Give the target some time to execute before measuring.
        thread::sleep(self.settle);

        match (idle, self.measurement.measure()) {
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
