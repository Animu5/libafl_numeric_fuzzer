// The simulation backend, parallel workers, and input exchange all use Unix-only
// APIs (fork, flock, process groups). The hardware backend needs a Unix target too.
#[cfg(not(unix))]
compile_error!("libafl_fuzzer only supports Unix platforms");

mod cli;
mod evaluation;
mod global_best;
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
mod parallel;
#[cfg(feature = "hardware")]
mod scope;
mod simulation;
mod snippet;

use clap::Parser;
use cli::{Args, BackendConfig, Config, MonitorKind};
use evaluation::EvaluationBackend;
use feedback::{NumericFeedback, Shared};
#[cfg(feature = "hardware")]
use hardware::HardwareBackend;
use harness::Harness;
use libafl::mutators::{havoc_mutations, HavocScheduledMutator};
use libafl::{
    corpus::OnDiskCorpus,
    events::{
        EventConfig, EventFirer, EventReceiver, EventRestarter, Launcher, ProgressReporter,
        SendExiting, SimpleEventManager,
    },
    executors::InProcessExecutor,
    fuzzer::{Evaluator, Fuzzer, StdFuzzer},
    inputs::BytesInput,
    monitors::{NopMonitor, SimplePrintingMonitor, TuiMonitor},
    schedulers::QueueScheduler,
    stages::mutational::StdMutationalStage,
    state::StdState,
    Error,
};
use libafl_bolts::{
    core_affinity::{Cores, get_core_ids},
    current_nanos,
    rands::StdRand,
    shmem::{ShMemProvider, StdShMemProvider},
    tuples::tuple_list,
};
#[cfg(feature = "hardware")]
use measurement::{Measurement, ScopeMeasurement};
use monitor::{DynMonitor, LoggingMonitor};
#[cfg(feature = "hardware")]
use openocd::OpenOcd;
#[cfg(feature = "hardware")]
use scope::Scope;
use snippet::SnippetBuilder;
use std::{path::Path, rc::Rc, time::Duration};

/// The concrete LibAFL state used by every worker.
type WorkerState =
    StdState<OnDiskCorpus<BytesInput>, BytesInput, StdRand, OnDiskCorpus<BytesInput>>;

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

/// Builds the progress monitor selected by `kind`.
fn create_monitor(kind: MonitorKind) -> Box<dyn libafl::monitors::Monitor> {
    match kind {
        MonitorKind::Tui => Box::new(
            TuiMonitor::builder()
                .title("Numeric Fuzzer")
                .version(env!("CARGO_PKG_VERSION"))
                .build(),
        ),
        MonitorKind::Print => Box::new(SimplePrintingMonitor {}),
        MonitorKind::Log => Box::new(LoggingMonitor::new(Duration::from_secs(60))),
        MonitorKind::None => Box::new(NopMonitor {}),
    }
}

/// Wires up OpenOCD, the scope, the LibAFL components and runs the fuzz loop.
fn main() -> Result<(), Error> {
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new("--internal-simulation-supervisor"))
    {
        std::process::exit(simulation::supervisor_main());
    }
    let args = Args::parse();

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

    // Initialize logging before any forking so the broker and all workers share
    // the same log file (central logging via fork-inherited file descriptor).
    logging::init(&config.log_file, config.log_level.as_deref())
        .map_err(context("Could not initialize logging"))?;

    if config.worker_dirs.is_some() {
        return launch_parallel(config);
    }

    // Single-worker mode: the monitor and event manager are created here.
    let mut manager = SimpleEventManager::new(DynMonitor(create_monitor(config.monitor)));
    run_worker(config, &mut manager)
}

/// Prepares the worker workspace and launches parallel workers via LibAFL's
/// `Launcher` with LLMP shared-memory IPC. The broker (this process) owns the
/// monitor and aggregates stats; each forked client runs `run_worker` with its
/// own `LlmpRestartingEventManager`.
fn launch_parallel(config: Config) -> Result<(), Error> {
    /// Broker port: from `LLMP_BROKER_PORT` env var (useful for tests running
    /// multiple launchers concurrently), else 1337.
    fn broker_port() -> u16 {
        std::env::var("LLMP_BROKER_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1337)
    }

    let workers = config
        .worker_dirs
        .as_deref()
        .expect("worker_dirs checked by caller");

    let exchange = config.solutions_path.join("parallel-exchange");
    if exchange.try_exists().map_err(context("Checking exchange directory"))? {
        return Err(Error::illegal_state(format!(
            "Exchange already exists: {}",
            exchange.display()
        )));
    }
    if let Some(workspace) = &config.worker_workspace {
        if workspace.try_exists().map_err(context("Checking worker workspace"))? {
            return Err(Error::illegal_state(format!(
                "Worker workspace already exists: {}",
                workspace.display()
            )));
        }
        parallel::prepare_workspace(workspace, &config.assembly_environment, workers)
            .map_err(context("Preparing worker workspace"))?;
    }
    global_best::prepare(&config.solutions_path, workers.len())
        .map_err(context("Preparing input exchange"))?;

    // CPU cores: use the process affinity mask (respects SLURM --cpus-per-task),
    // trimmed to the number of workers.
    let core_ids = get_core_ids().map_err(|e| Error::illegal_state(format!("Cores: {e}")))?;
    let mut cores = Cores {
        cmdline: "auto".to_string(),
        ids: core_ids,
    };
    cores
        .trim(workers.len())
        .map_err(|e| Error::illegal_state(format!("Trimming cores: {e}")))?;

    let monitor = create_monitor(config.monitor);

    let result = Launcher::builder()
        .shmem_provider(StdShMemProvider::new().map_err(context("Shared memory"))?)
        .monitor(DynMonitor(monitor))
        .configuration(EventConfig::from_name("numeric-fuzzer"))
        .run_client(
            move |_restored: Option<WorkerState>,
                  mut mgr,
                  client_desc: libafl::events::launcher::ClientDescription| {
                let mut config = config;
                // Launcher client IDs are 1-based; worker directories are 0-based.
                let worker_index = client_desc.id() - 1;
                config
                    .select_worker(worker_index)
                    .map_err(Error::illegal_state)?;
                run_worker(config, &mut mgr)
            },
        )
        .cores(&cores)
        .broker_port(broker_port())
        .build()
        .launch();

    match result {
        // The broker returns ShuttingDown when all clients disconnect cleanly.
        Ok(()) | Err(Error::ShuttingDown) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Sets up the backend, harness, feedback, corpus, state, fuzzer, executor and
/// runs the fuzz loop. Called in single-worker mode with a `SimpleEventManager`
/// and, in parallel mode, inside the `Launcher`'s `run_client` closure with an
/// `LlmpRestartingEventManager`.
fn run_worker<EM>(config: Config, manager: &mut EM) -> Result<(), Error>
where
    EM: EventFirer<BytesInput, WorkerState>
        + EventReceiver<BytesInput, WorkerState>
        + EventRestarter<WorkerState>
        + ProgressReporter<WorkerState>
        + SendExiting,
{
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
    // In parallel mode the feedback persists the global best to the exchange root.
    // LLMP handles the actual input exchange between workers inside fuzz_one.
    if let Some((root, worker)) = &config.global_best {
        feedback.set_global_best(root.clone(), *worker);
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

    let mut target = |input: &BytesInput| harness.run(input);
    let mut executor = InProcessExecutor::builder()
        .harness(&mut target)
        .timeout(Duration::from_millis(config.exec_timeout_ms))
        .fuzzer(&mut fuzzer)
        .state(&mut state)
        .event_mgr(manager)
        .build()?;

    for seed in seeds {
        fuzzer.add_input(&mut state, &mut executor, manager, seed)?;
    }

    let mut stages = tuple_list!(StdMutationalStage::new(HavocScheduledMutator::new(
        havoc_mutations()
    )));
    // The LLMP event manager exchanges inputs between workers inside fuzz_one.
    fuzzer.fuzz_loop(&mut stages, &mut executor, &mut state, manager)?;

    Ok(())
}
