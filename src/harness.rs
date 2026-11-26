//! The per-input harness: unparses the grammar input into assembly text, runs it
//! through the ISA gate (which fixes crashing snippets or rejects them), builds
//! the ELF with the snippet builder, runs the selected evaluation backend, and
//! stores the outcome in [`Shared`] for the feedback to pick up.

use crate::{
    evaluation::{EvaluationBackend, EvaluationError, Metric},
    feedback::Shared,
    isagate,
    snippet::SnippetBuilder,
};
use libafl::{executors::ExitKind, generators::nautilus::NautilusContext, inputs::NautilusInput};
use std::rc::Rc;

/// Prepended to every generated snippet. The assembly environment's `init.inc`
/// provides the machine init: the linker script enters at `_init`, which sets
/// `mtvec`, PMP, SATP, delegates traps, enables the FPU and loads `gp`, then
/// falls through into the snippet body. Without it, float instructions trap
/// (FS is off) and arbitrary memory accesses trap (no PMP/SATP setup). The
/// original asltgen-afl-proxy `SnippetCompiler` wrote the same include line.
const SNIPPET_INIT_INCLUDE: &str = "#include \"init.inc\"\n\n";

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
    /// fixed assembly, input text) for the feedback; failures are recorded there or
    /// logged, so the exit kind is always `Ok`.
    pub fn run(&mut self, input: &NautilusInput) -> ExitKind {
        *self.shared.evaluation.borrow_mut() = None;
        *self.shared.assembly.borrow_mut() = None;
        self.shared.input.borrow_mut().clear();

        // Unparse the grammar tree into assembly text.
        let mut bytes = Vec::new();
        input.unparse(&self.grammar, &mut bytes);
        let assembly = String::from_utf8_lossy(&bytes).into_owned();

        // Run the ISA gate: it fixes the snippet (memory addresses, alignment)
        // or rejects it when it cannot be fixed.
        let fixed = match isagate::gate(&assembly) {
            Ok(fixed) => fixed,
            Err(e) => {
                self.shared.isa_gate_rejections.set(self.shared.isa_gate_rejections.get() + 1);
                tracing::debug!(
                    "Skipping input of {} bytes: {e} (rejections so far: {})",
                    assembly.len(),
                    self.shared.isa_gate_rejections.get()
                );
                return ExitKind::Ok;
            }
        };
        // Prepend the machine-init include after the gate (the gate only ever
        // sees the bare program), so the written src/snippet.S, the snippet
        // archive, the saved solution assemblies and the persisted input bytes
        // all contain the complete, buildable source.
        let source = format!("{SNIPPET_INIT_INCLUDE}{fixed}");
        let bytes = source.as_bytes();
        let head = &bytes[..bytes.len().min(16)];
        let elf = match self.builder.build_from_source(&source) {
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
                // The persisted "input bytes" are the complete assembly text
                // (include line plus gate-fixed program), so the global best
                // input file stays human-readable in grammar mode.
                self.shared.input.borrow_mut().extend_from_slice(bytes);
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

    /// A one-rule grammar whose programs all unparse to the same snippet.
    /// `mv x1,x2` passes the ISA gate unchanged; `bogus` is rejected by it.
    fn test_grammar(source: &str) -> NautilusContext {
        NautilusContext::new(5, &[vec!["PROG".to_string(), source.to_string()]])
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
        let grammar = Rc::new(test_grammar("mv x1,x2\n"));
        let mut harness = Harness {
            builder: SnippetBuilder::new(dir.clone(), "/opt/riscv".into()),
            backend: Box::new(FakeBackend {
                results: VecDeque::from([
                    Ok(Evaluation::power_delta(2.0).unwrap()),
                    Err(EvaluationError::Candidate("failed".into())),
                    Err(EvaluationError::Fatal("unrecoverable".into())),
                    Ok(Evaluation::power_delta(3.0).unwrap()),
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
        assert!(shared.assembly.borrow().as_ref().unwrap().contains("mv"));
        // The persisted input bytes are the complete source: init.inc include
        // plus the gate-fixed assembly (with labels).
        assert_eq!(
            shared.input.borrow().as_slice(),
            b"#include \"init.inc\"\n\n.Lbr0:\nmv x1,x2\n"
        );
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

        // An input the ISA gate rejects is skipped before any build happens.
        let bad_grammar = Rc::new(test_grammar("bogus\n"));
        harness.grammar = bad_grammar.clone();
        harness.run(&gen_input(&bad_grammar, 5));
        assert!(shared.evaluation.borrow().is_none());
        assert_eq!(shared.isa_gate_rejections.get(), 1);
        assert_eq!(paths.borrow().len(), 3);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
