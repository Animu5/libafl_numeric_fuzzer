//! Hardware backend: loads each built ELF onto the target through OpenOCD and
//! scores it by the running-minus-idle power delta measured on the oscilloscope.
//! Loading/resuming failures reset the target (`reset halt`) and are skipped;
//! too many consecutive failures abort the campaign.

use crate::{
    evaluation::{Evaluation, EvaluationBackend, EvaluationError},
    measurement::Measurement,
    openocd::OpenOcd,
};
use std::{io, path::Path, thread, time::Duration};

const MAX_MEASUREMENT_FAILURES: u32 = 5;
const MAX_TARGET_FAILURES: u32 = 10;

/// The target-control operations the hardware backend needs (OpenOCD by default,
/// faked in tests).
pub(crate) trait Target {
    /// Loads `elf` onto the halted target.
    fn load_program(&mut self, elf: &Path) -> io::Result<()>;
    /// Resumes the target (the programs are infinite loops).
    fn resume(&mut self) -> io::Result<()>;
    /// Resets and halts the target, e.g. to recover from a crashed program.
    fn reset_halt(&mut self) -> io::Result<()>;
}

impl Target for OpenOcd {
    fn load_program(&mut self, elf: &Path) -> io::Result<()> {
        OpenOcd::load_program(self, elf)
    }

    fn resume(&mut self) -> io::Result<()> {
        OpenOcd::resume(self)
    }

    fn reset_halt(&mut self) -> io::Result<()> {
        OpenOcd::reset_halt(self)
    }
}

/// Evaluates candidates on the real target; generic over the target so tests can
/// substitute a fake.
pub struct HardwareBackend<T = OpenOcd> {
    target: T,
    measurement: Box<dyn Measurement>,
    settle: Duration,
    failures: u32,
    target_failures: u32,
}

impl<T> HardwareBackend<T> {
    /// Creates the backend from a connected target and a configured measurement;
    /// `settle` is the time the target runs before the running level is captured.
    pub fn new(target: T, measurement: Box<dyn Measurement>, settle: Duration) -> Self {
        Self {
            target,
            measurement,
            settle,
            failures: 0,
            target_failures: 0,
        }
    }
}

impl<T: Target> HardwareBackend<T> {
    fn target_failed(&mut self, what: &str, err: io::Error) -> EvaluationError {
        self.target_failures += 1;
        if self.target_failures >= MAX_TARGET_FAILURES {
            return EvaluationError::Fatal(format!(
                "{} consecutive target failures, last: {what}: {err}",
                self.target_failures
            ));
        }
        match self.target.reset_halt() {
            Ok(()) => EvaluationError::Candidate(format!("{what}: {err}")),
            Err(e) => EvaluationError::Fatal(format!(
                "Target did not recover after {what} failed ({err}): {e}"
            )),
        }
    }
}

impl<T: Target> EvaluationBackend for HardwareBackend<T> {
    fn evaluate(&mut self, elf: &Path) -> Result<Evaluation, EvaluationError> {
        if let Err(e) = self.target.load_program(elf) {
            return Err(self.target_failed("loading the program", e));
        }

        let idle = self.measurement.capture_idle();
        if let Err(e) = self.target.resume() {
            return Err(self.target_failed("resume", e));
        }
        thread::sleep(self.settle);

        let result = idle
            .and_then(|()| self.measurement.measure_delta())
            .map_err(|e| EvaluationError::Measurement(e.to_string()))
            .and_then(Evaluation::power_delta);
        match result {
            Ok(evaluation) => {
                self.failures = 0;
                self.target_failures = 0;
                Ok(evaluation)
            }
            Err(e) => {
                self.failures += 1;
                if self.failures >= MAX_MEASUREMENT_FAILURES {
                    Err(EvaluationError::Fatal(format!(
                        "{} consecutive measurement failures, last: {e}",
                        self.failures,
                    )))
                } else {
                    Err(e)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::MeasurementError;
    use std::{cell::RefCell, rc::Rc};

    #[derive(Default)]
    struct State {
        calls: Vec<&'static str>,
        fail_load: bool,
        fail_resume: bool,
        fail_reset: bool,
        fail_idle: bool,
        fail_measure: bool,
        delta: f64,
    }

    struct FakeTarget(Rc<RefCell<State>>);

    impl Target for FakeTarget {
        fn load_program(&mut self, _: &Path) -> io::Result<()> {
            let mut state = self.0.borrow_mut();
            state.calls.push("load");
            if state.fail_load {
                Err(io::Error::other("load failed"))
            } else {
                Ok(())
            }
        }

        fn resume(&mut self) -> io::Result<()> {
            let mut state = self.0.borrow_mut();
            state.calls.push("resume");
            if state.fail_resume {
                Err(io::Error::other("resume failed"))
            } else {
                Ok(())
            }
        }

        fn reset_halt(&mut self) -> io::Result<()> {
            let mut state = self.0.borrow_mut();
            state.calls.push("reset");
            if state.fail_reset {
                Err(io::Error::other("reset failed"))
            } else {
                Ok(())
            }
        }
    }

    struct FakeMeasurement(Rc<RefCell<State>>);

    impl Measurement for FakeMeasurement {
        fn capture_idle(&mut self) -> Result<(), MeasurementError> {
            let mut state = self.0.borrow_mut();
            state.calls.push("idle");
            if state.fail_idle {
                Err(MeasurementError::InvalidReading("idle".into()))
            } else {
                Ok(())
            }
        }

        fn measure_delta(&mut self) -> Result<f64, MeasurementError> {
            let mut state = self.0.borrow_mut();
            state.calls.push("measure");
            if state.fail_measure {
                Err(MeasurementError::InvalidReading("run".into()))
            } else {
                Ok(state.delta)
            }
        }
    }

    fn backend(state: &Rc<RefCell<State>>) -> HardwareBackend<FakeTarget> {
        HardwareBackend::new(
            FakeTarget(state.clone()),
            Box::new(FakeMeasurement(state.clone())),
            Duration::ZERO,
        )
    }

    #[test]
    fn successful_evaluation_preserves_capture_order() {
        let state = Rc::new(RefCell::new(State {
            delta: -1.5,
            ..State::default()
        }));
        let result = backend(&state).evaluate(Path::new("snippet.elf")).unwrap();
        assert_eq!(result.score(), -1.5);
        assert_eq!(state.borrow().calls, ["load", "idle", "resume", "measure"]);
    }

    #[test]
    fn target_failure_recovers_and_aborts_at_limit() {
        let state = Rc::new(RefCell::new(State {
            fail_load: true,
            ..State::default()
        }));
        let mut backend = backend(&state);
        for _ in 1..MAX_TARGET_FAILURES {
            assert!(matches!(
                backend.evaluate(Path::new("snippet.elf")),
                Err(EvaluationError::Candidate(_))
            ));
        }
        assert!(matches!(
            backend.evaluate(Path::new("snippet.elf")),
            Err(EvaluationError::Fatal(_))
        ));
        let calls = &state.borrow().calls;
        assert_eq!(calls.iter().filter(|&&c| c == "load").count(), 10);
        assert_eq!(calls.iter().filter(|&&c| c == "reset").count(), 9);
    }

    #[test]
    fn resume_failure_recovers_even_if_idle_failed() {
        let state = Rc::new(RefCell::new(State {
            fail_idle: true,
            fail_resume: true,
            ..State::default()
        }));
        let mut backend = backend(&state);
        assert!(matches!(
            backend.evaluate(Path::new("snippet.elf")),
            Err(EvaluationError::Candidate(_))
        ));
        assert_eq!(state.borrow().calls, ["load", "idle", "resume", "reset"]);
        state.borrow_mut().fail_reset = true;
        assert!(matches!(
            backend.evaluate(Path::new("snippet.elf")),
            Err(EvaluationError::Fatal(_))
        ));
    }

    #[test]
    fn idle_failure_still_resumes_and_measurement_failures_abort_at_limit() {
        let state = Rc::new(RefCell::new(State {
            fail_idle: true,
            ..State::default()
        }));
        let mut backend = backend(&state);
        for _ in 1..MAX_MEASUREMENT_FAILURES {
            assert!(matches!(
                backend.evaluate(Path::new("snippet.elf")),
                Err(EvaluationError::Measurement(_))
            ));
        }
        assert!(matches!(
            backend.evaluate(Path::new("snippet.elf")),
            Err(EvaluationError::Fatal(_))
        ));
        assert_eq!(state.borrow().calls, ["load", "idle", "resume"].repeat(5));
    }

    #[test]
    fn successful_measurement_resets_both_failure_counters() {
        let state = Rc::new(RefCell::new(State {
            fail_load: true,
            ..State::default()
        }));
        let mut backend = backend(&state);
        for _ in 1..MAX_TARGET_FAILURES {
            assert!(backend.evaluate(Path::new("snippet.elf")).is_err());
        }
        state.borrow_mut().fail_load = false;
        state.borrow_mut().fail_measure = true;
        for _ in 1..MAX_MEASUREMENT_FAILURES {
            assert!(backend.evaluate(Path::new("snippet.elf")).is_err());
        }
        state.borrow_mut().fail_measure = false;
        assert!(backend.evaluate(Path::new("snippet.elf")).is_ok());
        state.borrow_mut().fail_load = true;
        assert!(matches!(
            backend.evaluate(Path::new("snippet.elf")),
            Err(EvaluationError::Candidate(_))
        ));
        state.borrow_mut().fail_load = false;
        state.borrow_mut().fail_measure = true;
        assert!(matches!(
            backend.evaluate(Path::new("snippet.elf")),
            Err(EvaluationError::Measurement(_))
        ));
    }
}
