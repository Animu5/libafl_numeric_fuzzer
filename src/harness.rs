//! The per-input harness: unparses the grammar input into assembly text, builds
//! the ELF with the snippet builder, runs the selected evaluation backend, and
//! stores the outcome in [`Shared`] for the feedback to pick up.

use crate::{
    evaluation::{EvaluationBackend, EvaluationError, Metric},
    feedback::Shared,
    snippet::SnippetBuilder,
};
use libafl::{executors::ExitKind, generators::nautilus::NautilusContext, inputs::NautilusInput};
use std::rc::Rc;

/// Builds each input and sends it to the selected evaluation backend.
pub struct Harness {
    pub builder: SnippetBuilder,
    pub backend: Box<dyn EvaluationBackend>,
    pub shared: Rc<Shared>,
    /// Grammar context used to unparse a [`NautilusInput`] into assembly text.
    /// Shared with the worker's generator and mutators via `Rc`.
    pub grammar: Rc<NautilusContext>,
}

impl Harness {
    /// Builds and evaluates one input, storing the outcome in `shared` (evaluation,
    /// assembly, input text) for the feedback; failures are recorded there or
    /// logged, so the exit kind is always `Ok`.
    pub fn run(&mut self, input: &NautilusInput) -> ExitKind {
        *self.shared.evaluation.borrow_mut() = None;
        *self.shared.assembly.borrow_mut() = None;
        self.shared.input.borrow_mut().clear();

        // Unparse the grammar tree into assembly text.
        let mut bytes = Vec::new();
        input.unparse(&self.grammar, &mut bytes);
        let assembly = String::from_utf8_lossy(&bytes).into_owned();
        let head = &bytes[..bytes.len().min(16)];
        let elf = match self.builder.build_from_source(&assembly) {
            Ok(elf) => elf,
            Err(e) => {
                tracing::warn!(
                    "Skipping input of {} bytes (head {:02x?}): snippet build failed [{}]: {e}",
                    bytes.len(),
                    head,
                    self.builder.paths_summary()
                );
                return ExitKind::Ok;
            }
        };
        match self.backend.evaluate(&elf) {
            Ok(result) => {
                match result.metric() {
                    Metric::PowerDelta => {
                        tracing::info!("Current measurement: {} W.", result.score())
                    }
                    Metric::ToggleActivity => tracing::info!(
                        "Current toggle activity: {} changes/s; coverage: {}; map coverage: {} ({} slots).",
                        result.score(),
                        crate::evaluation::format_percentage(result.toggle_coverage().unwrap_or(0.0)),
                        crate::evaluation::format_percentage(result.map_coverage().unwrap_or(0.0)),
                        result.trace_map().map_or(0, <[u8]>::len)
                    ),
                }
                *self.shared.assembly.borrow_mut() =
                    self.builder.last_assembly().map(str::to_string);
                // The persisted "input bytes" are the assembly text, so the global
                // best input file stays human-readable in grammar mode.
                self.shared.input.borrow_mut().extend_from_slice(&bytes);
                *self.shared.evaluation.borrow_mut() = Some(result);
            }
            Err(EvaluationError::Candidate(message)) => {
                tracing::warn!(
                    "Skipping input of {} bytes (head {:02x?}): {message}",
                    bytes.len(),
                    head
                );
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

    /// A one-rule grammar whose programs all unparse to the same `nop` snippet.
    fn test_grammar() -> NautilusContext {
        NautilusContext::new(5, &[vec!["PROG".to_string(), "nop\n".to_string()]])
    }

    /// Generates one input from the test grammar with a seeded rand.
    fn gen_input(ctx: &NautilusContext, seed: u64) -> NautilusInput {
        use libafl::{
            generators::{Generator, nautilus::NautilusGenerator},
            state::{HasRand, NopState},
        };
        use libafl_bolts::rands::StdRand;
        let mut state = NopState::<NautilusInput>::new();
        *state.rand_mut() = StdRand::with_seed(seed);
        NautilusGenerator::new(ctx).generate(&mut state).unwrap()
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
        let grammar = Rc::new(test_grammar());
        let mut harness = Harness {
            builder: SnippetBuilder::new(dir.clone(), "/opt/riscv".into()),
            backend: Box::new(FakeBackend {
                results: VecDeque::from([
                    Ok(Evaluation::power_delta(2.0).unwrap()),
                    Err(EvaluationError::Candidate("failed".into())),
                    Err(EvaluationError::Fatal("unrecoverable".into())),
                ]),
                paths: paths.clone(),
            }),
            shared: shared.clone(),
            grammar: grammar.clone(),
        };
        harness.run(&gen_input(&grammar, 1));
        assert_eq!(
            shared.evaluation.borrow().as_ref().unwrap().score(),
            2.0
        );
        assert!(shared.assembly.borrow().as_ref().unwrap().contains("nop"));
        assert_eq!(shared.input.borrow().as_slice(), b"nop\n");
        harness.run(&gen_input(&grammar, 2));
        assert!(shared.evaluation.borrow().is_none());
        assert!(shared.assembly.borrow().is_none());
        // A failing make (Makefile replaced by one that always fails).
        std::fs::write(dir.join("Makefile"), "all:\n\tfalse\n").unwrap();
        harness.run(&gen_input(&grammar, 3));
        assert!(shared.evaluation.borrow().is_none());
        assert!(shared.assembly.borrow().is_none());
        assert_eq!(paths.borrow().len(), 2);
        std::fs::write(
            dir.join("Makefile"),
            "snippet.elf: src/snippet.S\n\tcp $< $@\n",
        )
        .unwrap();
        harness.run(&gen_input(&grammar, 4));
        assert!(shared.evaluation.borrow().is_none());
        assert_eq!(shared.fatal.borrow().as_deref(), Some("unrecoverable"));
        assert_eq!(paths.borrow().len(), 3);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
