use libafl::{
    corpus::Testcase,
    executors::ExitKind,
    feedbacks::{Feedback, StateInitializer},
    Error,
};
use libafl_bolts::Named;
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    rc::Rc,
};

/// State shared between the harness and the feedback.
#[derive(Debug, Default)]
pub struct Shared {
    /// Running-minus-idle delta (F1) of the last run; `None` if it was not measured.
    pub delta: Cell<Option<f64>>,
    /// Unrecoverable harness error; makes the next `is_interesting` abort the fuzz loop.
    pub fatal: RefCell<Option<String>>,
}

/// Scores each run by the delta (F1, running minus idle) that the harness stores in `shared`.
/// A fatal harness error stored there aborts the fuzz loop.
#[derive(Clone, Debug)]
pub struct NumericFeedback {
    /// Highest delta seen so far.
    best: f64,
    /// Result of the last `is_interesting` call.
    last: bool,
    /// Number of testcases saved, used in the filename.
    count: u64,
    /// Delta of the last run, recorded in the testcase filename.
    last_value: Option<f64>,
    shared: Rc<Shared>,
    name: Cow<'static, str>,
}

impl NumericFeedback {
    /// Creates a feedback that starts with no best value.
    pub fn new(shared: Rc<Shared>) -> Self {
        Self {
            best: f64::MIN,
            last: false,
            count: 0,
            last_value: None,
            shared,
            name: Cow::Borrowed("NumericFeedback"),
        }
    }
}

impl Named for NumericFeedback {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<S> StateInitializer<S> for NumericFeedback {}

impl<EM, I, OT, S> Feedback<EM, I, OT, S> for NumericFeedback {
    /// Interesting if the delta is a new maximum; NaN and missing deltas are not.
    fn is_interesting(
        &mut self,
        _state: &mut S,
        _manager: &mut EM,
        _input: &I,
        _observers: &OT,
        _exit_kind: &ExitKind,
    ) -> Result<bool, Error> {
        if let Some(msg) = self.shared.fatal.borrow_mut().take() {
            return Err(Error::illegal_state(msg));
        }

        self.last_value = self.shared.delta.take();
        self.last = match self.last_value {
            Some(v) if !v.is_nan() && v > self.best => {
                self.best = v;
                tracing::info!("New best measurement delta: {}", v);
                true
            }
            _ => false,
        };
        Ok(self.last)
    }

    fn last_result(&self) -> Result<bool, Error> {
        Ok(self.last)
    }

    /// Names the saved testcase `NNNN_delta_<value>`.
    fn append_metadata(
        &mut self,
        _state: &mut S,
        _manager: &mut EM,
        _observers: &OT,
        testcase: &mut Testcase<I>,
    ) -> Result<(), Error> {
        if let Some(v) = self.last_value {
            self.count += 1;
            *testcase.filename_mut() = Some(format!("{:04}_delta_{v:.6}", self.count));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulates one run in which the harness stored `delta`.
    fn run(f: &mut NumericFeedback, delta: Option<f64>) -> Result<bool, Error> {
        f.shared.delta.set(delta);
        <NumericFeedback as Feedback<(), (), (), ()>>::is_interesting(
            f,
            &mut (),
            &mut (),
            &(),
            &(),
            &ExitKind::Ok,
        )
    }

    #[test]
    fn only_new_best_is_interesting() {
        let mut f = NumericFeedback::new(Rc::new(Shared::default()));
        assert!(run(&mut f, Some(1.0)).unwrap());
        assert!(!run(&mut f, Some(0.5)).unwrap());
        assert!(!run(&mut f, None).unwrap());
        assert!(!run(&mut f, Some(f64::NAN)).unwrap());
        assert!(run(&mut f, Some(2.0)).unwrap());
    }

    #[test]
    fn fatal_error_aborts() {
        let shared = Rc::new(Shared::default());
        *shared.fatal.borrow_mut() = Some("boom".into());
        let mut f = NumericFeedback::new(shared);
        assert!(run(&mut f, None).is_err());
    }
}
