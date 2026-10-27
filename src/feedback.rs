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
    pub delta: Cell<Option<f64>>,
    pub fatal: RefCell<Option<String>>,
}

/// Scores each run by the delta (running - idle) that the harness stores in `shared`.
/// A fatal harness error stored there aborts the fuzz loop.
#[derive(Clone, Debug)]
pub struct NumericFeedback {
    best: f64,
    last: bool,
    count: u64,
    last_value: Option<f64>,
    shared: Rc<Shared>,
    name: Cow<'static, str>,
}

impl NumericFeedback {
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
