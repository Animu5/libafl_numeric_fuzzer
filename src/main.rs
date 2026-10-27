mod cli;
mod feedback;
mod harness;
mod logging;
mod measurement;
mod openocd;
mod scope;
mod snippet;

use clap::Parser;
use cli::Args;
use feedback::{NumericFeedback, Shared};
use harness::Harness;
use libafl::mutators::{havoc_mutations, HavocScheduledMutator};
use libafl::{
    corpus::OnDiskCorpus,
    events::SimpleEventManager,
    executors::InProcessExecutor,
    fuzzer::{Evaluator, Fuzzer, StdFuzzer},
    inputs::BytesInput,
    monitors::TuiMonitor,
    schedulers::QueueScheduler,
    stages::mutational::StdMutationalStage,
    state::StdState,
    Error,
};
use libafl_bolts::{current_nanos, rands::StdRand, tuples::tuple_list};
use measurement::{Measurement, ScopeMeasurement};
use openocd::OpenOcd;
use scope::Scope;
use snippet::SnippetBuilder;
use std::{path::Path, rc::Rc, time::Duration};

/// Returns an error mapper that prefixes the message with `what`.
fn context<E: std::fmt::Display>(what: &'static str) -> impl FnOnce(E) -> Error {
    move |e| Error::illegal_state(format!("{what}: {e}"))
}

/// Loads the seed file or every regular non-hidden file in the seed directory (sorted by name).
/// Without a path, a single 16-byte zero input is used.
fn load_seeds(path: Option<&Path>) -> Result<Vec<BytesInput>, Error> {
    let Some(path) = path else {
        return Ok(vec![BytesInput::new(vec![0u8; 16])]);
    };

    let mut files = if path.is_dir() {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(path).map_err(context("Could not read seed directory"))? {
            let entry = entry.map_err(context("Could not read seed directory"))?;
            let hidden = entry.file_name().to_string_lossy().starts_with('.');
            if entry.path().is_file() && !hidden {
                files.push(entry.path());
            }
        }
        files.sort();
        files
    } else {
        vec![path.to_path_buf()]
    };

    if files.is_empty() {
        return Err(Error::illegal_argument(format!(
            "No seed files found in {}",
            path.display()
        )));
    }
    files
        .drain(..)
        .map(|f| {
            std::fs::read(&f)
                .map(BytesInput::new)
                .map_err(context("Could not read seed file"))
        })
        .collect()
}

/// Wires up OpenOCD, the scope, the LibAFL components and runs the fuzz loop.
fn main() -> Result<(), Error> {
    let args = Args::parse();

    if args.list_scopes {
        let list = Scope::list_resources("USB?*INSTR").map_err(context("VISA error"))?;
        list.iter().for_each(|r| println!("{r}"));
        return Ok(());
    }

    let config = args.resolve();
    logging::init(&config.log_file).map_err(context("Could not open log file"))?;

    if config.solutions_path.exists() && !config.solutions_path.is_dir() {
        return Err(Error::illegal_argument(
            "Solutions path exists but is not a directory",
        ));
    }
    std::fs::create_dir_all(&config.solutions_path)
        .map_err(context("Creating solutions directory"))?;

    // Connect to OpenOCD once; the harness loads a freshly built ELF for every input.
    let openocd =
        OpenOcd::new(&config.openocd_addr).map_err(context("Could not connect to OpenOCD"))?;

    let scope = Scope::connect(
        &config.scope_resource,
        Duration::from_millis(config.scope_timeout_ms),
    )
    .map_err(context("Could not connect to scope"))?;
    let measurement: Box<dyn Measurement> = Box::new(
        ScopeMeasurement::new(
            scope,
            Duration::from_millis(config.scope_acquire_timeout_ms),
        )
        .map_err(context("Could not configure scope"))?,
    );

    let seeds = load_seeds(config.seed.as_deref())?;

    let shared = Rc::new(Shared::default());
    let mut harness = Harness {
        openocd,
        builder: SnippetBuilder::new(
            config.assembly_environment,
            config.atndecode_path,
            config.riscv_toolpath,
        ),
        measurement,
        shared: shared.clone(),
        settle: Duration::from_millis(config.settle_ms),
        failures: 0,
    };

    let corpus = OnDiskCorpus::<BytesInput>::new(&config.solutions_path)?;
    let solutions = OnDiskCorpus::new(config.solutions_path.join("crashes"))?;

    let mut feedback = NumericFeedback::new(shared);
    let mut objective = ();
    let mut state = StdState::new(
        StdRand::with_seed(current_nanos()),
        corpus,
        solutions,
        &mut feedback,
        &mut objective,
    )?;

    let mut fuzzer = StdFuzzer::new(QueueScheduler::new(), feedback, objective);

    let monitor = TuiMonitor::builder()
        .title("Numeric Fuzzer")
        .version(env!("CARGO_PKG_VERSION"))
        .build();
    let mut manager = SimpleEventManager::new(monitor);

    let mut target = |input: &BytesInput| harness.run(input);
    let mut executor = InProcessExecutor::builder()
        .harness(&mut target)
        .fuzzer(&mut fuzzer)
        .state(&mut state)
        .event_mgr(&mut manager)
        .build()?;

    for seed in seeds {
        fuzzer.add_input(&mut state, &mut executor, &mut manager, seed)?;
    }

    let mut stages = tuple_list!(StdMutationalStage::new(HavocScheduledMutator::new(
        havoc_mutations()
    )));
    fuzzer.fuzz_loop(&mut stages, &mut executor, &mut state, &mut manager)?;

    Ok(())
}
