use crate::evaluation::{Evaluation, Metric};
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
    path::PathBuf,
    rc::Rc,
};

/// State shared between the harness and the feedback.
#[derive(Debug, Default)]
pub struct Shared {
    /// Validated evaluation of the last run; `None` if it was not measured.
    pub evaluation: Cell<Option<Evaluation>>,
    /// Assembly text decoded from the last input; `None` if it could not be decoded.
    pub assembly: RefCell<Option<String>>,
    /// Unrecoverable harness error; makes the next `is_interesting` abort the fuzz loop.
    pub fatal: RefCell<Option<String>>,
}

/// Scores each run by the evaluation that the harness stores in `shared`.
/// A fatal harness error stored there aborts the fuzz loop.
///
/// Only a new best score is interesting, so every testcase this feedback lets through is the
/// current best. Its assembly is saved next to the binary input as `<filename>.S` and as
/// `best.S`, which is therefore always the best snippet seen so far, even after an interrupt.
#[derive(Clone, Debug)]
pub struct NumericFeedback {
    /// Highest score seen so far.
    best: f64,
    /// Scores of different metrics must never be compared.
    metric: Option<Metric>,
    /// Result of the last `is_interesting` call.
    last: bool,
    /// Number of testcases saved, used in the filename.
    count: u64,
    /// Evaluation of the last run, recorded in the testcase filename.
    last_evaluation: Option<Evaluation>,
    /// Assembly decoded from the input of the last run.
    last_assembly: Option<String>,
    /// Directory that receives `<filename>.S` and `best.S`.
    solutions_dir: PathBuf,
    shared: Rc<Shared>,
    name: Cow<'static, str>,
}

impl NumericFeedback {
    /// Creates a feedback that starts with no best value and stores assemblies in `solutions_dir`.
    pub fn new(shared: Rc<Shared>, solutions_dir: PathBuf) -> Self {
        Self {
            best: f64::MIN,
            metric: None,
            last: false,
            count: 0,
            last_evaluation: None,
            last_assembly: None,
            solutions_dir,
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
    /// Interesting if the score is a new maximum.
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

        self.last_evaluation = self.shared.evaluation.take();
        self.last_assembly = self.shared.assembly.borrow_mut().take();
        self.last = match self.last_evaluation {
            Some(evaluation) => {
                if self
                    .metric
                    .is_some_and(|metric| metric != evaluation.metric())
                {
                    return Err(Error::illegal_state(
                        "Cannot compare scores from different evaluation metrics",
                    ));
                }
                self.metric = Some(evaluation.metric());
                if evaluation.score() > self.best {
                    self.best = evaluation.score();
                    match evaluation.metric() {
                        Metric::PowerDelta => {
                            tracing::info!("New best measurement delta: {}", self.best)
                        }
                        Metric::ToggleCoverage => {
                            tracing::info!("New best toggle coverage: {}", self.best)
                        }
                    }
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        Ok(self.last)
    }

    fn last_result(&self) -> Result<bool, Error> {
        Ok(self.last)
    }

    /// Names and saves the testcase and its assembly, and refreshes `best.S`.
    fn append_metadata(
        &mut self,
        _state: &mut S,
        _manager: &mut EM,
        _observers: &OT,
        testcase: &mut Testcase<I>,
    ) -> Result<(), Error> {
        if let Some(evaluation) = self.last_evaluation {
            self.count += 1;
            let label = match evaluation.metric() {
                Metric::PowerDelta => "delta",
                Metric::ToggleCoverage => "toggle_coverage",
            };
            let filename = format!("{:04}_{label}_{:.6}", self.count, evaluation.score());
            *testcase.filename_mut() = Some(filename.clone());
        }
        if let Some(assembly) = self.last_assembly.take() {
            let name = testcase
                .filename()
                .clone()
                .ok_or_else(|| Error::illegal_state("Testcase has no filename"))?;
            std::fs::write(self.solutions_dir.join(format!("{name}.S")), &assembly).map_err(
                |e| Error::illegal_state(format!("Could not write snippet assembly: {e}")),
            )?;
            std::fs::write(self.solutions_dir.join("best.S"), &assembly)
                .map_err(|e| Error::illegal_state(format!("Could not write best snippet: {e}")))?;
            tracing::debug!("Saved snippet assembly to {name}.S and best.S");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libafl::inputs::BytesInput;

    /// A fresh feedback writing into a new temporary directory.
    fn feedback() -> (NumericFeedback, Rc<Shared>, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "numeric_feedback_test_{}_{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let shared = Rc::new(Shared::default());
        (
            NumericFeedback::new(shared.clone(), dir.clone()),
            shared,
            dir,
        )
    }

    /// Simulates one run in which the harness stored a power evaluation.
    fn run(f: &mut NumericFeedback, delta: Option<f64>) -> Result<bool, Error> {
        f.shared
            .evaluation
            .set(delta.map(|v| Evaluation::power_delta(v).unwrap()));
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
        let (mut f, shared, dir) = feedback();
        assert!(run(&mut f, Some(1.0)).unwrap());
        assert!(!run(&mut f, Some(0.5)).unwrap());
        assert!(!run(&mut f, None).unwrap());
        assert!(run(&mut f, Some(2.0)).unwrap());
        std::fs::remove_dir_all(dir).unwrap();
        let _ = shared;
    }

    #[test]
    fn fatal_error_aborts() {
        let (mut f, shared, dir) = feedback();
        *shared.fatal.borrow_mut() = Some("boom".into());
        assert!(run(&mut f, None).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saving_a_testcase_stores_its_assembly() {
        let (mut f, shared, dir) = feedback();
        *shared.assembly.borrow_mut() = Some("#include \"init.inc\"\n\nnop\n".into());
        assert!(run(&mut f, Some(1.5)).unwrap());

        let mut testcase = Testcase::<BytesInput>::new(BytesInput::new(vec![]));
        <NumericFeedback as Feedback<(), BytesInput, (), ()>>::append_metadata(
            &mut f,
            &mut (),
            &mut (),
            &(),
            &mut testcase,
        )
        .unwrap();

        let expected = "#include \"init.inc\"\n\nnop\n";
        let saved = std::fs::read_to_string(dir.join("0001_delta_1.500000.S")).unwrap();
        assert_eq!(saved, expected);
        assert_eq!(
            std::fs::read_to_string(dir.join("best.S")).unwrap(),
            expected
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saving_without_assembly_still_names_the_testcase() {
        let (mut f, _shared, dir) = feedback();
        assert!(run(&mut f, Some(1.0)).unwrap());

        let mut testcase = Testcase::<BytesInput>::new(BytesInput::new(vec![]));
        <NumericFeedback as Feedback<(), BytesInput, (), ()>>::append_metadata(
            &mut f,
            &mut (),
            &mut (),
            &(),
            &mut testcase,
        )
        .unwrap();

        assert_eq!(testcase.filename(), &Some("0001_delta_1.000000".into()));
        assert!(!dir.join("best.S").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn coverage_scores_have_distinct_labels_and_keep_activity() {
        let (mut f, shared, dir) = feedback();
        let result = Evaluation::toggle_coverage(0.75, 42.0).unwrap();
        shared.evaluation.set(Some(result));
        *shared.assembly.borrow_mut() = Some("nop".into());
        assert!(
            <NumericFeedback as Feedback<(), BytesInput, (), ()>>::is_interesting(
                &mut f,
                &mut (),
                &mut (),
                &BytesInput::new(vec![]),
                &(),
                &ExitKind::Ok
            )
            .unwrap()
        );
        assert_eq!(
            f.last_evaluation.unwrap().toggle_activity_per_second(),
            Some(42.0)
        );
        let mut testcase = Testcase::new(BytesInput::new(vec![]));
        <NumericFeedback as Feedback<(), BytesInput, (), ()>>::append_metadata(
            &mut f,
            &mut (),
            &mut (),
            &(),
            &mut testcase,
        )
        .unwrap();
        assert_eq!(
            testcase.filename().as_deref(),
            Some("0001_toggle_coverage_0.750000")
        );
        assert_eq!(std::fs::read_to_string(dir.join("best.S")).unwrap(), "nop");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn different_metrics_cannot_be_compared() {
        let (mut f, shared, dir) = feedback();
        assert!(run(&mut f, Some(1.0)).unwrap());
        shared
            .evaluation
            .set(Some(Evaluation::toggle_coverage(0.5, 1.0).unwrap()));
        assert!(
            <NumericFeedback as Feedback<(), (), (), ()>>::is_interesting(
                &mut f,
                &mut (),
                &mut (),
                &(),
                &(),
                &ExitKind::Ok
            )
            .is_err()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
