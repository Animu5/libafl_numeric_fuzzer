use crate::{
    evaluation::{EvaluationBackend, EvaluationError, Metric},
    feedback::Shared,
    snippet::SnippetBuilder,
};
use libafl::{
    executors::ExitKind,
    inputs::{BytesInput, HasTargetBytes},
};
use libafl_bolts::ToSlice;
use std::rc::Rc;

/// Builds each input and sends it to the selected evaluation backend.
pub struct Harness {
    pub builder: SnippetBuilder,
    pub backend: Box<dyn EvaluationBackend>,
    pub shared: Rc<Shared>,
}

impl Harness {
    pub fn run(&mut self, input: &BytesInput) -> ExitKind {
        self.shared.evaluation.set(None);
        *self.shared.assembly.borrow_mut() = None;

        let bytes = input.target_bytes();
        let elf = match self.builder.build(bytes.to_slice()) {
            Ok(elf) => elf,
            Err(e) => {
                tracing::warn!("Skipping input: {e}");
                return ExitKind::Ok;
            }
        };
        match self.backend.evaluate(&elf) {
            Ok(result) => {
                match result.metric() {
                    Metric::PowerDelta => {
                        tracing::info!("Current measurement: {} W.", result.score())
                    }
                    Metric::ToggleCoverage => tracing::info!(
                        "Current toggle coverage: {}; activity: {} changes/s.",
                        result.score(),
                        result
                            .toggle_activity_per_second()
                            .expect("coverage has activity")
                    ),
                }
                *self.shared.assembly.borrow_mut() =
                    self.builder.last_assembly().map(str::to_string);
                self.shared.evaluation.set(Some(result));
            }
            Err(EvaluationError::Candidate(message)) => {
                tracing::warn!("Skipping input: {message}");
            }
            Err(EvaluationError::Measurement(message)) => {
                tracing::error!("Measurement failed: {message}");
            }
            Err(EvaluationError::Fatal(message)) => {
                *self.shared.fatal.borrow_mut() = Some(message);
            }
        }
        ExitKind::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::Evaluation;
    use std::{
        cell::RefCell,
        collections::VecDeque,
        path::{Path, PathBuf},
    };

    struct FakeBackend {
        results: VecDeque<Result<Evaluation, EvaluationError>>,
        paths: Rc<RefCell<Vec<PathBuf>>>,
    }

    impl EvaluationBackend for FakeBackend {
        fn evaluate(&mut self, elf: &Path) -> Result<Evaluation, EvaluationError> {
            assert!(elf.is_file());
            self.paths.borrow_mut().push(elf.to_path_buf());
            self.results.pop_front().unwrap()
        }
    }

    #[test]
    fn builds_before_evaluating_and_clears_state_on_failures() {
        let dir = std::env::temp_dir().join(format!(
            "harness_test_{}_{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Makefile"),
            "snippet.elf: src/snippet.S\n\tcp $< $@\n",
        )
        .unwrap();
        let shared = Rc::new(Shared::default());
        let paths = Rc::new(RefCell::new(Vec::new()));
        let mut harness = Harness {
            builder: SnippetBuilder::new(dir.clone(), "/bin/cat".into(), "/opt/riscv".into()),
            backend: Box::new(FakeBackend {
                results: VecDeque::from([
                    Ok(Evaluation::power_delta(2.0).unwrap()),
                    Err(EvaluationError::Candidate("failed".into())),
                    Err(EvaluationError::Fatal("unrecoverable".into())),
                ]),
                paths: paths.clone(),
            }),
            shared: shared.clone(),
        };
        harness.run(&BytesInput::new(b"first".to_vec()));
        assert_eq!(shared.evaluation.get().unwrap().score(), 2.0);
        assert!(shared.assembly.borrow().as_ref().unwrap().contains("first"));
        harness.run(&BytesInput::new(b"second".to_vec()));
        assert!(shared.evaluation.get().is_none());
        assert!(shared.assembly.borrow().is_none());
        harness.builder =
            SnippetBuilder::new(dir.clone(), "/bin/false".into(), "/opt/riscv".into());
        harness.run(&BytesInput::new(b"bad".to_vec()));
        assert!(shared.evaluation.get().is_none());
        assert!(shared.assembly.borrow().is_none());
        assert_eq!(paths.borrow().len(), 2);
        harness.builder = SnippetBuilder::new(dir.clone(), "/bin/cat".into(), "/opt/riscv".into());
        harness.run(&BytesInput::new(b"third".to_vec()));
        assert!(shared.evaluation.get().is_none());
        assert_eq!(shared.fatal.borrow().as_deref(), Some("unrecoverable"));
        assert_eq!(paths.borrow().len(), 3);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
