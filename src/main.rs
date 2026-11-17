mod cli;
mod evaluation;
#[cfg(unix)]
mod exchange;
mod feedback;
#[cfg(feature = "hardware")]
mod hardware;
mod harness;
mod logging;
#[cfg(feature = "hardware")]
mod measurement;
mod monitor;
#[cfg(feature = "hardware")]
mod openocd;
#[cfg(unix)]
mod parallel;
#[cfg(feature = "hardware")]
mod scope;
#[cfg(unix)]
mod simulation;
mod snippet;

use clap::Parser;
use cli::{Args, BackendConfig, MonitorKind};
use evaluation::EvaluationBackend;
use feedback::{NumericFeedback, Shared};
#[cfg(feature = "hardware")]
use hardware::HardwareBackend;
use harness::Harness;
use libafl::mutators::{havoc_mutations, HavocScheduledMutator};
use libafl::{
    corpus::OnDiskCorpus,
    events::SimpleEventManager,
    executors::InProcessExecutor,
    fuzzer::{Evaluator, Fuzzer, StdFuzzer},
    inputs::BytesInput,
    monitors::{NopMonitor, SimplePrintingMonitor, TuiMonitor},
    schedulers::QueueScheduler,
    stages::mutational::StdMutationalStage,
    state::StdState,
    Error,
};
use monitor::{DynMonitor, LoggingMonitor};
use libafl_bolts::{current_nanos, rands::StdRand, tuples::tuple_list};
#[cfg(feature = "hardware")]
use measurement::{Measurement, ScopeMeasurement};
#[cfg(feature = "hardware")]
use openocd::OpenOcd;
#[cfg(feature = "hardware")]
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
    #[cfg(unix)]
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new("--internal-simulation-supervisor"))
    {
        std::process::exit(simulation::supervisor_main());
    }
    let args = Args::parse();
    #[cfg(unix)]
    if args.internal_launcher_child {
        // Only launcher-spawned workers watch the liveness pipe; standalone workers
        // (e.g. SLURM array tasks) may have no stdin at all.
        parallel::watch_parent();
    }

    if args.list_scopes {
        #[cfg(feature = "hardware")]
        {
            let list = Scope::list_resources("USB?*INSTR").map_err(context("VISA error"))?;
            list.iter().for_each(|r| println!("{r}"));
            return Ok(());
        }
        #[cfg(not(feature = "hardware"))]
        return Err(Error::illegal_argument(
            "--list-scopes requires the hardware feature",
        ));
    }

    let config = args.resolve();
    if let Some(workers) = &config.worker_dirs {
        #[cfg(unix)]
        return parallel::launch(
            workers,
            config.worker_workspace.as_deref(),
            &config.assembly_environment,
            &config.solutions_path,
        )
        .map_err(context("Parallel workers failed"));
        #[cfg(not(unix))]
        return Err(Error::illegal_argument("Parallel workers require Unix"));
    }
    logging::init(&config.log_file, config.log_level.as_deref())
        .map_err(context("Could not initialize logging"))?;

    if config.solutions_path.exists() && !config.solutions_path.is_dir() {
        return Err(Error::illegal_argument(
            "Solutions path exists but is not a directory",
        ));
    }
    std::fs::create_dir_all(&config.solutions_path)
        .map_err(context("Creating solutions directory"))?;

    let backend: Box<dyn EvaluationBackend> = match config.backend {
        BackendConfig::Hardware {
            openocd_addr,
            scope_resource,
            scope_timeout_ms,
            scope_acquire_timeout_ms,
            settle_ms,
        } => {
            #[cfg(feature = "hardware")]
            {
                let openocd =
                    OpenOcd::new(&openocd_addr).map_err(context("Could not connect to OpenOCD"))?;
                let scope =
                    Scope::connect(&scope_resource, Duration::from_millis(scope_timeout_ms))
                        .map_err(context("Could not connect to scope"))?;
                let measurement: Box<dyn Measurement> = Box::new(
                    ScopeMeasurement::new(scope, Duration::from_millis(scope_acquire_timeout_ms))
                        .map_err(context("Could not configure scope"))?,
                );
                Box::new(HardwareBackend::new(
                    openocd,
                    measurement,
                    Duration::from_millis(settle_ms),
                ))
            }
            #[cfg(not(feature = "hardware"))]
            {
                let _ = (
                    openocd_addr,
                    scope_resource,
                    scope_timeout_ms,
                    scope_acquire_timeout_ms,
                    settle_ms,
                );
                return Err(Error::illegal_argument(
                    "Hardware backend requires the hardware feature",
                ));
            }
        }
        BackendConfig::Simulation {
            simulator_path,
            simulator_working_dir,
            simulator_args,
            analyzer_path,
            target_scope,
            exclude_signal_file,
            vcd_dir,
            simulator_timeout_ms,
            analyzer_timeout_ms,
        } => {
            #[cfg(unix)]
            {
                let settings = simulation::SimulationConfig {
                    simulator: simulator_path,
                    simulator_args: simulator_args.into_iter().map(Into::into).collect(),
                    working_dir: simulator_working_dir,
                    analyzer: analyzer_path,
                    target_scope,
                    exclude_signal_file,
                    vcd_dir,
                    simulator_timeout: Duration::from_millis(simulator_timeout_ms),
                    analyzer_timeout: Duration::from_millis(analyzer_timeout_ms),
                };
                Box::new(
                    simulation::SimulationBackend::new(settings)
                        .map_err(context("Could not configure simulation"))?,
                )
            }
            #[cfg(not(unix))]
            {
                let _ = (
                    simulator_path,
                    simulator_working_dir,
                    simulator_args,
                    analyzer_path,
                    target_scope,
                    exclude_signal_file,
                    vcd_dir,
                    simulator_timeout_ms,
                    analyzer_timeout_ms,
                );
                return Err(Error::illegal_argument("Simulation backend requires Unix"));
            }
        }
    };

    let seeds = load_seeds(config.seed.as_deref())?;

    let shared = Rc::new(Shared::default());
    let mut builder = SnippetBuilder::new(
        config.assembly_environment,
        config.atndecode_path,
        config.riscv_toolpath,
    );
    if let Some(dir) = config.snippet_dir {
        builder = builder
            .with_archive(dir)
            .map_err(context("Could not create snippet directory"))?;
    }

    let mut harness = Harness {
        builder,
        backend,
        shared: shared.clone(),
    };

    let corpus = OnDiskCorpus::<BytesInput>::new(&config.solutions_path)?;
    let solutions = OnDiskCorpus::new(config.solutions_path.join("crashes"))?;

    let mut feedback = NumericFeedback::new(shared.clone(), config.solutions_path.clone());
    #[cfg(unix)]
    if let Some((root, _, workers)) = &config.exchange {
        // Standalone workers (e.g. SLURM array tasks) create the exchange if missing;
        // launcher mode already created it via exchange::prepare.
        exchange::ensure(root, *workers).map_err(context("Could not prepare input exchange"))?;
    }
    #[cfg(unix)]
    let mut exchange = config
        .exchange
        .map(|(path, index, workers)| exchange::Exchange::new(path, index, workers));
    #[cfg(unix)]
    if let Some(exchange) = &exchange {
        feedback.set_exchange(exchange.clone());
    }
    let mut objective = ();
    let mut state = StdState::new(
        StdRand::with_seed(current_nanos()),
        corpus,
        solutions,
        &mut feedback,
        &mut objective,
    )?;

    let mut fuzzer = StdFuzzer::new(QueueScheduler::new(), feedback, objective);

    let monitor: Box<dyn libafl::monitors::Monitor> = match config.monitor {
        MonitorKind::Tui => Box::new(
            TuiMonitor::builder()
                .title("Numeric Fuzzer")
                .version(env!("CARGO_PKG_VERSION"))
                .build(),
        ),
        MonitorKind::Print => Box::new(SimplePrintingMonitor {}),
        MonitorKind::Log => Box::new(LoggingMonitor::new(Duration::from_secs(60))),
        MonitorKind::None => Box::new(NopMonitor {}),
    };
    let mut manager = SimpleEventManager::new(DynMonitor(monitor));

    let mut target = |input: &BytesInput| harness.run(input);
    let mut executor = InProcessExecutor::builder()
        .harness(&mut target)
        .timeout(Duration::from_millis(config.exec_timeout_ms))
        .fuzzer(&mut fuzzer)
        .state(&mut state)
        .event_mgr(&mut manager)
        .build()?;

    for seed in seeds {
        #[cfg(unix)]
        if let Some(exchange) = &mut exchange {
            exchange.remember_input(seed.as_ref());
        }
        fuzzer.add_input(&mut state, &mut executor, &mut manager, seed)?;
    }

    let mut stages = tuple_list!(StdMutationalStage::new(HavocScheduledMutator::new(
        havoc_mutations()
    )));
    #[cfg(unix)]
    if let Some(exchange) = &mut exchange {
        loop {
            fuzzer.fuzz_one(&mut stages, &mut executor, &mut state, &mut manager)?;
            for input in exchange
                .pending_inputs()
                .map_err(context("Input exchange failed"))?
            {
                shared.importing.set(true);
                let raw: &[u8] = input.as_ref();
                tracing::debug!(
                    "Evaluating imported exchange input of {} bytes (head {:02x?})",
                    raw.len(),
                    &raw[..raw.len().min(16)]
                );
                let result = fuzzer.evaluate_input(&mut state, &mut executor, &mut manager, &input);
                shared.importing.set(false);
                result?;
            }
        }
    }
    fuzzer.fuzz_loop(&mut stages, &mut executor, &mut state, &mut manager)?;

    Ok(())
}
