use crate::evaluation::{Evaluation, Metric};
use libafl::{
    Error,
    corpus::Testcase,
    executors::ExitKind,
    feedbacks::{Feedback, StateInitializer},
};
use libafl_bolts::Named;
use std::{borrow::Cow, cell::RefCell, path::PathBuf, rc::Rc};

/// State shared between the harness and the feedback.
#[derive(Debug, Default)]
pub struct Shared {
    /// Validated evaluation of the last run; `None` if it was not measured.
    pub evaluation: RefCell<Option<Evaluation>>,
    /// Assembly text decoded from the last input; `None` if it could not be decoded.
    pub assembly: RefCell<Option<String>>,
    /// Unrecoverable harness error; makes the next `is_interesting` abort the fuzz loop.
    pub fatal: RefCell<Option<String>>,
    /// Raw input bytes of the last run, used to persist the global best.
    pub input: RefCell<Vec<u8>>,
}

/// Scores each run by the evaluation that the harness stores in `shared`.
/// A fatal harness error stored there aborts the fuzz loop.
///
/// Primary feedback is the trace map of the evaluation (one saturating toggle
/// count per traced signal): a run is interesting if it raises any map entry
/// above the history. The scalar score (toggle activity or power delta) is the
/// secondary metric used for best tracking. Input exchange between parallel
/// workers is handled by the LLMP event manager; the feedback only persists the
/// global best to `parallel-exchange/global-best.txt` in parallel mode.
/// Assembly for new best inputs is saved next to the input and as `best.S`.
#[derive(Clone, Debug)]
pub struct NumericFeedback {
    /// Highest score seen so far (secondary metric).
    best: f64,
    /// Trace map history: per-slot maximum seen so far.
    history: Vec<u8>,
    /// Scores of different metrics must never be compared.
    metric: Option<Metric>,
    /// Result of the last `is_interesting` call.
    last: bool,
    last_best: bool,
    /// Number of testcases saved, used in the filename.
    count: u64,
    /// Evaluation of the last run, recorded in the testcase filename.
    last_evaluation: Option<Evaluation>,
    /// Assembly decoded from the input of the last run.
    last_assembly: Option<String>,
    /// Directory that receives `<filename>.S` and `best.S`.
    solutions_dir: PathBuf,
    shared: Rc<Shared>,
    /// Exchange root for global-best tracking; `None` in single-worker mode.
    global_best: Option<(PathBuf, usize)>,
    name: Cow<'static, str>,
}

impl NumericFeedback {
    /// Creates a feedback that starts with no best value and an empty trace map
    /// history, storing assemblies in `solutions_dir`.
    pub fn new(shared: Rc<Shared>, solutions_dir: PathBuf) -> Self {
        Self {
            best: f64::MIN,
            history: Vec::new(),
            metric: None,
            last: false,
            last_best: false,
            count: 0,
            last_evaluation: None,
            last_assembly: None,
            solutions_dir,
            shared,
            global_best: None,
            name: Cow::Borrowed("NumericFeedback"),
        }
    }

    /// Enables global-best tracking: `root` is the `parallel-exchange` directory,
    /// `worker` is this worker's 0-based index.
    pub fn set_global_best(&mut self, root: PathBuf, worker: usize) {
        self.global_best = Some((root, worker));
    }
}

impl Named for NumericFeedback {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<S> StateInitializer<S> for NumericFeedback {}

impl<EM, I, OT, S> Feedback<EM, I, OT, S> for NumericFeedback {
    /// Interesting if the trace map raises any entry above the history (primary
    /// feedback). Without a trace map (hardware backend) a new maximum score is
    /// interesting instead. The score is tracked separately as the secondary
    /// metric for best persistence.
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

        self.last_evaluation = self.shared.evaluation.borrow_mut().take();
        self.last_assembly = self.shared.assembly.borrow_mut().take();
        self.last_best = false;
        let interesting = match &self.last_evaluation {
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

                // Primary feedback: new coverage in the trace map.
                let novel = match evaluation.trace_map() {
                    Some(map) => {
                        if map.len() > self.history.len() {
                            self.history.resize(map.len(), 0);
                        }
                        let mut novel = false;
                        for (slot, &value) in map.iter().enumerate() {
                            if value > self.history[slot] {
                                self.history[slot] = value;
                                novel = true;
                            }
                        }
                        novel
                    }
                    None => false,
                };

                // Secondary metric: track the best score for persistence.
                if evaluation.score() > self.best {
                    if let Some((root, worker)) = &self.global_best {
                        let assembly = self.last_assembly.as_deref().ok_or_else(|| {
                            Error::illegal_state("No assembly for measured parallel input")
                        })?;
                        crate::parallel::update_global_best(
                            root,
                            *worker,
                            &self.shared.input.borrow(),
                            assembly,
                            evaluation,
                        )
                        .map_err(|e| {
                            Error::illegal_state(format!("Cannot update global best: {e}"))
                        })?;
                    }
                    self.best = evaluation.score();
                    self.last_best = true;
                    if let Some(assembly) = &self.last_assembly {
                        std::fs::write(self.solutions_dir.join("best.S"), assembly).map_err(
                            |e| Error::illegal_state(format!("Could not write best snippet: {e}")),
                        )?;
                    }
                    match evaluation.metric() {
                        Metric::PowerDelta => {
                            tracing::info!("New best measurement delta: {}", self.best)
                        }
                        Metric::ToggleActivity => {
                            tracing::info!(
                                "New best toggle activity: {} changes/s (coverage {}, map {})",
                                self.best,
                                crate::evaluation::format_percentage(
                                    evaluation.toggle_coverage().unwrap_or(0.0)
                                ),
                                crate::evaluation::format_percentage(
                                    evaluation.map_coverage().unwrap_or(0.0)
                                )
                            )
                        }
                    }
                }

                if evaluation.trace_map().is_some() {
                    novel
                } else {
                    self.last_best
                }
            }
            None => false,
        };
        self.last = interesting;
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
        if self.last_best {
            if let Some(evaluation) = &self.last_evaluation {
                self.count += 1;
                let label = match evaluation.metric() {
                    Metric::PowerDelta => "delta",
                    Metric::ToggleActivity => "toggle_activity",
                };
                let filename = format!("{:04}_{label}_{:.6}", self.count, evaluation.score());
                *testcase.filename_mut() = Some(filename.clone());
            }
        } else {
            self.count += 1;
            *testcase.filename_mut() = Some(format!("import-{:04}", self.count));
        }
        if let Some(assembly) = self.last_assembly.take() {
            let name = testcase
                .filename()
                .clone()
                .ok_or_else(|| Error::illegal_state("Testcase has no filename"))?;
            std::fs::write(self.solutions_dir.join(format!("{name}.S")), &assembly).map_err(
                |e| Error::illegal_state(format!("Could not write snippet assembly: {e}")),
            )?;
            tracing::debug!("Saved snippet assembly to {name}.S");
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
        *f.shared.evaluation.borrow_mut() =
            delta.map(|v| Evaluation::power_delta(v).unwrap());
        <NumericFeedback as Feedback<(), (), (), ()>>::is_interesting(
            f,
            &mut (),
            &mut (),
            &(),
            &(),
            &ExitKind::Ok,
        )
    }

    /// Simulates one simulation-backend run with the given activity and trace map.
    fn run_toggle(
        f: &mut NumericFeedback,
        activity: f64,
        map: &[u8],
    ) -> Result<bool, Error> {
        *f.shared.evaluation.borrow_mut() =
            Some(Evaluation::toggle_metrics(activity, 0.5, map.to_vec()).unwrap());
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
    fn non_best_inputs_are_not_interesting_and_do_not_replace_best() {
        let (mut f, shared, dir) = feedback();
        *shared.assembly.borrow_mut() = Some("best".into());
        assert!(run(&mut f, Some(2.0)).unwrap());
        let mut best = Testcase::<BytesInput>::new(BytesInput::new(vec![]));
        <NumericFeedback as Feedback<(), BytesInput, (), ()>>::append_metadata(
            &mut f,
            &mut (),
            &mut (),
            &(),
            &mut best,
        )
        .unwrap();

        // A valid but lower score is not interesting and does not become best.S.
        assert!(!run(&mut f, None).unwrap());
        *shared.assembly.borrow_mut() = Some("lower".into());
        assert!(!run(&mut f, Some(1.0)).unwrap());
        assert_eq!(std::fs::read_to_string(dir.join("best.S")).unwrap(), "best");
        std::fs::remove_dir_all(dir).unwrap();
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
    fn trace_map_is_the_primary_feedback_and_activity_the_secondary() {
        let (mut f, shared, dir) = feedback();
        // First run: new coverage (map entry 0 goes 0 -> 3), becomes best by activity.
        *shared.assembly.borrow_mut() = Some("nop".into());
        assert!(run_toggle(&mut f, 42.0, &[3, 0]).unwrap());
        assert!(f.last_best);
        assert_eq!(f.last_evaluation.as_ref().unwrap().score(), 42.0);
        assert_eq!(
            f.last_evaluation.as_ref().unwrap().toggle_coverage(),
            Some(0.5)
        );
        // Same map, higher activity: not interesting (no new coverage) but still the best.
        assert!(!run_toggle(&mut f, 100.0, &[3, 0]).unwrap());
        assert!(f.last_best);
        // New coverage in slot 1, lower activity: interesting, but not the best.
        assert!(run_toggle(&mut f, 10.0, &[3, 1]).unwrap());
        assert!(!f.last_best);
        // Higher count in an already-seen slot is new coverage too.
        assert!(run_toggle(&mut f, 10.0, &[4, 1]).unwrap());
        // A shorter map only compares its own prefix.
        assert!(!run_toggle(&mut f, 10.0, &[4]).unwrap());

        let mut testcase = Testcase::new(BytesInput::new(vec![]));
        <NumericFeedback as Feedback<(), BytesInput, (), ()>>::append_metadata(
            &mut f,
            &mut (),
            &mut (),
            &(),
            &mut testcase,
        )
        .unwrap();
        // Last run was interesting but not the best: saved as an import.
        assert_eq!(testcase.filename().as_deref(), Some("import-0001"));
        // best.S holds the highest-activity run's assembly.
        assert_eq!(std::fs::read_to_string(dir.join("best.S")).unwrap(), "nop");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn toggle_activity_scores_have_distinct_labels() {
        let (mut f, shared, dir) = feedback();
        *shared.assembly.borrow_mut() = Some("nop".into());
        assert!(run_toggle(&mut f, 42.0, &[1]).unwrap());
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
            Some("0001_toggle_activity_42.000000")
        );
        assert_eq!(std::fs::read_to_string(dir.join("best.S")).unwrap(), "nop");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn different_metrics_cannot_be_compared() {
        let (mut f, shared, dir) = feedback();
        assert!(run(&mut f, Some(1.0)).unwrap());
        *shared.evaluation.borrow_mut() =
            Some(Evaluation::toggle_metrics(1.0, 0.5, vec![1]).unwrap());
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
