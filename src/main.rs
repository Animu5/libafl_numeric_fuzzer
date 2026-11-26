//! LibAFL-based numeric fuzzer for RISC-V snippets: every input is decoded into
//! assembly, built into an ELF, and evaluated by a backend — hardware (OpenOCD +
//! oscilloscope power delta) or simulation (Verilator VCD analyzed in-process) —
//! and the [`feedback`] decides which inputs are interesting.
//!
//! Parallel fuzzing on a single node uses LibAFL's `Launcher` with LLMP
//! shared-memory IPC (see [`launch_parallel`]): the broker process owns the
//! monitor, forked workers are pinned to cores, and all of them share one central
//! log file.
//!
//! The simulation backend, parallel workers, and input exchange all use Unix-only
//! APIs (fork, flock, process groups). The hardware backend needs a Unix target too.
#[cfg(not(unix))]
compile_error!("libafl_fuzzer only supports Unix platforms");

mod cli;
mod evaluation;
mod feedback;
#[cfg(feature = "hardware")]
mod hardware;
mod harness;
mod isagate;
#[cfg(feature = "hardware")]
mod measurement;
#[cfg(feature = "hardware")]
mod openocd;
mod parallel;
#[cfg(feature = "hardware")]
mod scope;
mod simulation;
mod snippet;
mod supervisor;
mod vcdtoggle;

use clap::Parser;
use cli::{Args, BackendConfig, Config, MonitorKind};
use evaluation::EvaluationBackend;
use feedback::{NumericFeedback, Shared};
#[cfg(feature = "hardware")]
use hardware::HardwareBackend;
use harness::Harness;
use libafl::mutators::{HavocScheduledMutator, havoc_mutations};
use libafl::{
    Error,
    corpus::OnDiskCorpus,
    events::{
        EventConfig, EventFirer, EventReceiver, EventRestarter, Launcher, ProgressReporter,
        SendExiting, SimpleEventManager,
    },
    executors::InProcessExecutor,
    fuzzer::{Evaluator, Fuzzer, StdFuzzer},
    inputs::BytesInput,
    monitors::{
        Monitor, NopMonitor, SimplePrintingMonitor, TuiMonitor, stats::manager::ClientStatsManager,
    },
    schedulers::QueueScheduler,
    stages::mutational::StdMutationalStage,
    state::StdState,
};
use libafl_bolts::{
    ClientId,
    core_affinity::{Cores, get_core_ids},
    current_nanos,
    rands::StdRand,
    shmem::{ShMemProvider, StdShMemProvider},
    tuples::tuple_list,
};
#[cfg(feature = "hardware")]
use measurement::{Measurement, ScopeMeasurement};

#[cfg(feature = "hardware")]
use openocd::OpenOcd;
#[cfg(feature = "hardware")]
use scope::Scope;
use snippet::SnippetBuilder;
use std::{
    fmt, io,
    path::Path,
    rc::Rc,
    sync::{Mutex, RwLock},
    time::{Duration, Instant},
};
use tracing::Subscriber;
use tracing_subscriber::{
    EnvFilter,
    fmt::{
        FmtContext, FormatEvent,
        format::{FormatFields, Writer},
        time::{FormatTime, SystemTime},
    },
    registry::LookupSpan,
};

/// The concrete LibAFL state used by every worker.
type WorkerState =
    StdState<OnDiskCorpus<BytesInput>, BytesInput, StdRand, OnDiskCorpus<BytesInput>>;

/// Returns an error mapper that prefixes the message with `what`.
fn context<E: fmt::Display>(what: &'static str) -> impl FnOnce(E) -> Error {
    move |e| Error::illegal_state(format!("{what}: {e}"))
}

/// Process-role tag attached to every log line (see [`TaggedFormat`]). Set once per
/// process at startup: `broker` or `single` in [`main`], `worker-NNNN` in the
/// `Launcher`'s client closure, `supervisor` in the simulation supervisor.
static LOG_TAG: RwLock<String> = RwLock::new(String::new());

/// Sets the process-role tag that [`TaggedFormat`] puts into every log line.
pub(crate) fn set_log_tag(tag: impl Into<String>) {
    if let Ok(mut guard) = LOG_TAG.write() {
        *guard = tag.into();
    }
}

/// Returns the current process-role tag, or an empty string if none was set.
fn log_tag() -> String {
    LOG_TAG.read().map(|guard| guard.clone()).unwrap_or_default()
}

/// Event formatter that inserts the process-role tag between the level and the
/// target, e.g. `2026-11-25T12:34:56.789012Z  INFO [worker-0003] libafl_fuzzer: message`.
/// In parallel mode broker and workers share one log file (the workers inherit the
/// file descriptor across the fork), so the tag — not the pid — is what tells the
/// processes apart.
pub(crate) struct TaggedFormat;

impl<S, N> FormatEvent<S, N> for TaggedFormat
where
    S: Subscriber + for<'span> LookupSpan<'span>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> fmt::Result {
        SystemTime.format_time(&mut writer)?;
        write!(
            writer,
            " {:<5} [{}] {}: ",
            event.metadata().level(),
            log_tag(),
            event.metadata().target(),
        )?;
        // The formatted fields: the `message` plus any structured fields.
        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

/// Logs to `path`, truncating any existing file. Every line is tagged with the
/// process role (see [`TaggedFormat`]). The writer is unbuffered so nothing is lost
/// if a crash handler kills the process. The filter comes from `level` if given, else from
/// `RUST_LOG`, else `info`. LibAFL logs through the `log` crate; the `tracing-log` feature
/// bridges those records into tracing so they land in the same file.
fn init_logging(path: &Path, level: Option<&str>) -> io::Result<()> {
    let file = std::fs::File::create(path)?;
    let filter = match level {
        Some(level) => EnvFilter::try_new(level).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Invalid log level or filter {level:?}: {e}"),
            )
        })?,
        None => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
    };
    tracing_subscriber::fmt()
        .with_writer(Mutex::new(file))
        .with_ansi(false)
        .with_env_filter(filter)
        .event_format(TaggedFormat)
        .init();
    Ok(())
}

/// A headless monitor that writes periodic fuzzer statistics into the log file
/// via `tracing`, for runs without a terminal.
#[derive(Debug)]
struct LoggingMonitor {
    interval: Duration,
    last: Option<Instant>,
}

impl LoggingMonitor {
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }
}

impl Monitor for LoggingMonitor {
    fn display(
        &mut self,
        stats: &mut ClientStatsManager,
        name: &str,
        client_id: ClientId,
    ) -> Result<(), Error> {
        let now = Instant::now();
        if self
            .last
            .is_some_and(|last| now.duration_since(last) < self.interval)
        {
            return Ok(());
        }
        self.last = Some(now);
        let Ok(client) = stats.get(client_id) else {
            return Ok(());
        };
        let elapsed = client.start_time();
        let execs = client.executions();
        let execs_per_sec = if elapsed.is_zero() {
            0.0
        } else {
            execs as f64 / elapsed.as_secs_f64()
        };
        tracing::info!(
            "[{name}] execs={execs} ({execs_per_sec:.0}/s), corpus={}, objectives={}, \
             last new corpus {:?} ago, last objective {:?} ago",
            client.corpus_size(),
            client.objective_size(),
            client.last_corpus_time(),
            client.last_objective_time(),
        );
        Ok(())
    }
}

/// Boxes any monitor behind one type so the CLI can select it at runtime.
struct DynMonitor(Box<dyn Monitor>);

impl fmt::Debug for DynMonitor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DynMonitor")
    }
}

impl Monitor for DynMonitor {
    fn display(
        &mut self,
        stats: &mut ClientStatsManager,
        name: &str,
        client_id: ClientId,
    ) -> Result<(), Error> {
        // Never propagate a display error: ll_mp `.expect()`s the broker hooks'
        // result in its timeout loop, so an error here kills the whole broker
        // (and then every worker). The broker heartbeat calls `display` with the
        // broker's own `ClientId(0)`, which is not in the per-client stats map;
        // monitors like `SimplePrintingMonitor` return `KeyNotFound` for that.
        if let Err(error) = self.0.display(stats, name, client_id) {
            tracing::debug!("Monitor display for {name} (client {client_id:?}): {error}");
        }
        Ok(())
    }
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
fn create_monitor(kind: MonitorKind) -> Box<dyn Monitor> {
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
        std::process::exit(supervisor::supervisor_main());
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
    init_logging(&config.log_file, config.log_level.as_deref())
        .map_err(context("Could not initialize logging"))?;
    // Tag this process in the shared log file: the parallel broker, or the single
    // worker when not running under the Launcher.
    set_log_tag(if config.worker_dirs.is_some() {
        "broker"
    } else {
        "single"
    });

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
    if exchange
        .try_exists()
        .map_err(context("Checking exchange directory"))?
    {
        return Err(Error::illegal_state(format!(
            "Exchange already exists: {}",
            exchange.display()
        )));
    }
    if let Some(workspace) = &config.worker_workspace {
        if workspace
            .try_exists()
            .map_err(context("Checking worker workspace"))?
        {
            return Err(Error::illegal_state(format!(
                "Worker workspace already exists: {}",
                workspace.display()
            )));
        }
        parallel::prepare_workspace(workspace, &config.assembly_environment, workers)
            .map_err(context("Preparing worker workspace"))?;
    }
    parallel::prepare_exchange(&config.solutions_path, workers.len())
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
                // Workers are forked from the broker and inherit the log file
                // descriptor; tag their lines with the 0-based worker index.
                set_log_tag(format!("worker-{worker_index:04}"));
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
            target_scope,
            exclude_signal_file,
            vcd_dir,
            simulator_timeout_ms,
        } => {
            let settings = simulation::SimulationConfig {
                simulator: simulator_path,
                simulator_args: simulator_args.into_iter().map(Into::into).collect(),
                working_dir: simulator_working_dir,
                target_scope,
                exclude_signal_file,
                vcd_dir,
                simulator_timeout: Duration::from_millis(simulator_timeout_ms),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The broker heartbeat calls `display` with the broker's own `ClientId(0)`,
    /// which is not in the per-client stats map. `DynMonitor` must swallow the
    /// resulting error instead of letting ll_mp's broker panic on it.
    #[test]
    fn monitor_display_errors_never_propagate() {
        for mut monitor in [
            DynMonitor(Box::new(SimplePrintingMonitor {})),
            DynMonitor(Box::new(LoggingMonitor::new(Duration::from_secs(60)))),
            DynMonitor(Box::new(NopMonitor {})),
        ] {
            let mut stats = ClientStatsManager::default();
            // Empty stats map, broker id: exactly the heartbeat situation.
            assert!(
                monitor
                    .display(&mut stats, "Broker Heartbeat", ClientId(0))
                    .is_ok()
            );
        }
    }

    /// Every log line carries the process-role tag set via [`set_log_tag`], so broker,
    /// workers, and the supervisor can be told apart in the shared log file.
    #[test]
    fn log_lines_carry_the_process_role_tag() {
        use std::sync::Arc;
        use tracing_subscriber::fmt::MakeWriter;

        /// `MakeWriter` that appends every written line to a shared in-memory buffer.
        #[derive(Clone, Default)]
        struct SharedBuf(Arc<Mutex<Vec<u8>>>);
        impl io::Write for SharedBuf {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for SharedBuf {
            type Writer = Self;
            fn make_writer(&'a self) -> Self {
                self.clone()
            }
        }

        let buf = SharedBuf::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buf.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .event_format(TaggedFormat)
            .finish();
        set_log_tag("worker-0003");
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(answer = 42, "hello from the worker");
        });
        set_log_tag("");

        let output = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(
            output.contains("[worker-0003]"),
            "log line is missing the role tag: {output:?}"
        );
        assert!(
            output.contains("hello from the worker") && output.contains("answer=42"),
            "log line is missing the message or structured fields: {output:?}"
        );
    }
}
