//! Command-line arguments and TOML configuration.
//!
//! Every setting can be given on the command line or in the TOML file passed with
//! `--config` (same names with underscores, e.g. `openocd_addr = "..."`);
//! command-line values take precedence over file values. [`Args::resolve`] merges
//! both, applies defaults, and validates the result into a [`Config`].

use clap::{CommandFactory, Parser, ValueEnum};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Default for `--grammar-depth`. Nautilus needs the tree depth to exceed the
/// bundled grammar's minimum derivation size (~350 rule applications, mostly the
/// 32 register inits and 32 float constants). The depth is a *budget* shared by
/// all sibling nonterminals of a rule, so it must be much larger than the minimum:
/// the statement-list repetition `(statements)+` is one of ~66 siblings of the
/// start rule and only gets a ~1/66 share, which at 512 left room for a single
/// body statement. 16384 gives the body room for dozens of statements.
pub const DEFAULT_GRAMMAR_DEPTH: usize = 16384;

/// Default for `--grammar-seeds`: inputs generated from the grammar as the initial corpus.
pub const DEFAULT_GRAMMAR_SEEDS: usize = 64;

#[derive(Clone, Copy, Debug, Default, Deserialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum BackendKind {
    #[default]
    Hardware,
    Simulation,
}

/// How fuzzing progress is displayed.
#[derive(Clone, Copy, Debug, Default, Deserialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MonitorKind {
    /// Full-screen terminal UI (default)
    #[default]
    Tui,
    /// Plain progress lines on stdout
    Print,
    /// Periodic progress lines in the log file (for headless runs)
    Log,
    /// No progress output
    None,
}

/// Command-line arguments. Every setting can also be given in the TOML file passed
/// with `--config` (same names with underscores, e.g. `openocd_addr = "..."`).
/// Command-line values take precedence over the file.
#[derive(Parser, Deserialize, Debug, Default)]
#[command(version, long_version = None, about, long_about = None)]
#[serde(deny_unknown_fields)]
pub struct Args {
    /// TOML configuration file
    #[arg(short, long)]
    #[serde(skip)]
    config: Option<PathBuf>,

    /// Print the available USB VISA resources and exit
    #[arg(long)]
    #[serde(skip)]
    pub list_scopes: bool,

    /// Directory with the environment where a plain `make` assembles the binary;
    /// the program loaded onto the FPGA is `<assembly-environment>/snippet.elf`
    #[arg(short, long)]
    assembly_environment: Option<PathBuf>,

    /// Nautilus JSON grammar used to generate RISC-V assembly inputs (default: grammars/riscv64g.json)
    #[arg(long)]
    grammar: Option<PathBuf>,

    /// Nautilus grammar tree depth: must exceed the grammar's minimum derivation size and caps the generated program size (default: 16384)
    #[arg(long)]
    grammar_depth: Option<usize>,

    /// Number of inputs generated from the grammar as the initial corpus (default: 64)
    #[arg(long)]
    grammar_seeds: Option<usize>,

    /// RISC-V toolchain directory, passed to `make` as the `RISCV` environment variable
    #[arg(long)]
    riscv_toolpath: Option<PathBuf>,

    /// Directory where inputs with the new best measurement are stored,
    /// as binary plus the matching assembly as `<name>.S` and `best.S`
    #[arg(short, long)]
    solutions_path: Option<PathBuf>,

    /// Directory where every generated snippet is saved as NNNNNN.S, for debugging (default: off)
    #[arg(long)]
    snippet_dir: Option<PathBuf>,

    /// Log file (default: fuzzer.log)
    #[arg(long)]
    log_file: Option<PathBuf>,

    /// Log filter for the log file, e.g. "debug" or "info,libafl=debug" (default: RUST_LOG if set, else "info")
    #[arg(long)]
    log_level: Option<String>,

    /// Progress monitor: tui (default), print (stdout lines), log (periodic lines in the log file), none
    #[arg(long, value_enum)]
    monitor: Option<MonitorKind>,

    /// Number of independent simulation workers (default: 1)
    #[arg(long)]
    workers: Option<usize>,

    /// Dedicated assembly environment for each worker; repeat once per worker
    #[arg(long)]
    worker_assembly_environment: Option<Vec<PathBuf>>,

    /// Dedicated simulator working directory for each worker; repeat once per worker
    #[arg(long)]
    worker_simulator_working_dir: Option<Vec<PathBuf>>,

    /// Create a fresh parallel workspace with a copy of the assembly environment per worker
    #[arg(long)]
    worker_workspace: Option<PathBuf>,

    /// Evaluation backend (default: hardware)
    #[arg(long, value_enum)]
    backend: Option<BackendKind>,

    /// Path to the Chipyard Verilator executable
    #[arg(long)]
    simulator_path: Option<PathBuf>,

    /// Simulator working directory (default: current directory)
    #[arg(long)]
    simulator_working_dir: Option<PathBuf>,

    /// Simulator argument; repeat for each argument (use --simulator-arg=VALUE)
    #[arg(long, allow_hyphen_values = true)]
    simulator_arg: Option<Vec<String>>,

    /// VCD scope analyzed in-process by the integrated VCD engine
    #[arg(long)]
    target_scope: Option<String>,

    /// Optional exclusion file for the in-process VCD analysis (one full signal name per line)
    #[arg(long)]
    exclude_signal_file: Option<PathBuf>,

    /// Directory for per-run VCD trace directories (default: the simulator working directory; /tmp is not used)
    #[arg(long)]
    vcd_dir: Option<PathBuf>,

    /// Simulator timeout in milliseconds (default: 600000)
    #[arg(long)]
    simulator_timeout_ms: Option<u64>,

    /// Target clock frequency in Hz: the VCD's per-cycle toggle activity is scaled by this to get the score (default: 50000000)
    #[arg(long)]
    target_clock_hz: Option<u64>,

    /// OpenOCD Tcl RPC address (default: 127.0.0.1:6666)
    #[arg(long)]
    openocd_addr: Option<String>,

    /// VISA resource string of the oscilloscope (e.g., USB0::0x05FF::...::INSTR); see --list-scopes
    #[arg(long)]
    scope_resource: Option<String>,

    /// Scope I/O timeout in milliseconds (default: 5000)
    #[arg(long)]
    scope_timeout_ms: Option<u64>,

    /// Time in milliseconds to wait for the scope to finish an acquisition (default: 30000)
    #[arg(long)]
    scope_acquire_timeout_ms: Option<u64>,

    /// LibAFL timeout for one complete run (build, simulate, analyze, or load and measure) in milliseconds (default: 900000)
    #[arg(long)]
    exec_timeout_ms: Option<u64>,

    /// Time in milliseconds to let the target run before the running measurement (default: 100)
    #[arg(long)]
    settle_ms: Option<u64>,
}

/// Fully resolved settings (CLI > config file > defaults).
#[derive(Debug)]
pub struct Config {
    pub assembly_environment: PathBuf,
    /// Nautilus JSON grammar the inputs are generated from.
    pub grammar: PathBuf,
    /// Grammar tree depth bounding recursion during generation/mutation.
    pub grammar_depth: usize,
    /// Number of grammar-generated inputs in the initial corpus.
    pub grammar_seeds: usize,
    pub riscv_toolpath: PathBuf,
    pub solutions_path: PathBuf,
    pub snippet_dir: Option<PathBuf>,
    pub log_file: PathBuf,
    pub log_level: Option<String>,
    pub monitor: MonitorKind,
    pub backend: BackendConfig,
    pub exec_timeout_ms: u64,
    pub worker_dirs: Option<Vec<WorkerDirs>>,
    pub worker_workspace: Option<PathBuf>,
    /// Global-best tracking: exchange root and this worker's 0-based index.
    /// Set by `select_worker` in parallel mode; `None` in single-worker mode.
    pub global_best: Option<(PathBuf, usize)>,
}

/// Directories dedicated to one parallel worker.
#[derive(Debug, Clone)]
pub struct WorkerDirs {
    /// The worker's private copy of the assembly environment (built into `snippet.elf`).
    pub assembly: PathBuf,
    /// The worker's private simulator working directory (VCDs land here by default).
    pub simulator: PathBuf,
}

/// Fully resolved backend settings.
#[derive(Debug)]
pub enum BackendConfig {
    Hardware {
        openocd_addr: String,
        scope_resource: String,
        scope_timeout_ms: u64,
        scope_acquire_timeout_ms: u64,
        settle_ms: u64,
    },
    Simulation {
        simulator_path: PathBuf,
        simulator_working_dir: PathBuf,
        simulator_args: Vec<String>,
        target_scope: String,
        exclude_signal_file: Option<PathBuf>,
        vcd_dir: Option<PathBuf>,
        simulator_timeout_ms: u64,
        target_clock_hz: u64,
    },
}

impl Args {
    /// Merges in the config file (if any) and applies defaults; exits with a usage error on problems.
    pub fn resolve(self) -> Config {
        let fail = |msg: String| -> ! {
            Args::command()
                .error(clap::error::ErrorKind::ValueValidation, msg)
                .exit()
        };

        let file = match &self.config {
            Some(path) => load_file(path).unwrap_or_else(|e| fail(e)),
            None => Args::default(),
        };
        self.merge(file).unwrap_or_else(|e| fail(e))
    }

    fn merge(self, file: Args) -> Result<Config, String> {
        let require = |name: &str, v: Option<PathBuf>| -> Result<PathBuf, String> {
            v.ok_or_else(|| {
                format!("--{name} is required (on the command line or in the config file)")
            })
        };
        let require_text = |name: &str, v: Option<String>| -> Result<String, String> {
            v.filter(|s| !s.trim().is_empty())
                .ok_or_else(|| format!("--{name} is required and must not be empty"))
        };

        let assembly_environment = require(
            "assembly-environment",
            self.assembly_environment.or(file.assembly_environment),
        )?;
        let grammar = self
            .grammar
            .or(file.grammar)
            .unwrap_or_else(|| PathBuf::from("grammars/riscv64g.json"));
        if !grammar.is_file() {
            return Err(format!(
                "Grammar file {} is not a file (pass --grammar or set grammar in the config)",
                grammar.display()
            ));
        }
        let grammar_depth = self
            .grammar_depth
            .or(file.grammar_depth)
            .unwrap_or(DEFAULT_GRAMMAR_DEPTH);
        if grammar_depth == 0 {
            return Err("--grammar-depth must be at least 1".into());
        }
        let grammar_seeds = self
            .grammar_seeds
            .or(file.grammar_seeds)
            .unwrap_or(DEFAULT_GRAMMAR_SEEDS);
        if grammar_seeds == 0 {
            return Err("--grammar-seeds must be at least 1".into());
        }
        let exec_timeout_ms = self
            .exec_timeout_ms
            .or(file.exec_timeout_ms)
            .unwrap_or(900_000);
        let backend = match self.backend.or(file.backend).unwrap_or_default() {
            BackendKind::Hardware => {
                if !cfg!(feature = "hardware") {
                    return Err(
                        "Hardware backend is unavailable: rebuild with the hardware feature".into(),
                    );
                }
                BackendConfig::Hardware {
                    openocd_addr: self
                        .openocd_addr
                        .or(file.openocd_addr)
                        .unwrap_or_else(|| "127.0.0.1:6666".into()),
                    scope_resource: require_text(
                        "scope-resource",
                        self.scope_resource.or(file.scope_resource),
                    )?,
                    scope_timeout_ms: self
                        .scope_timeout_ms
                        .or(file.scope_timeout_ms)
                        .unwrap_or(5000),
                    scope_acquire_timeout_ms: self
                        .scope_acquire_timeout_ms
                        .or(file.scope_acquire_timeout_ms)
                        .unwrap_or(30000),
                    settle_ms: self.settle_ms.or(file.settle_ms).unwrap_or(100),
                }
            }
            BackendKind::Simulation => {
                if !cfg!(unix) {
                    return Err("Simulation backend is available only on Unix".into());
                }
                let simulator_timeout_ms = self
                    .simulator_timeout_ms
                    .or(file.simulator_timeout_ms)
                    .unwrap_or(600_000);
                if simulator_timeout_ms == 0 {
                    return Err("Simulator timeout must be nonzero".into());
                }
                let target_clock_hz = self
                    .target_clock_hz
                    .or(file.target_clock_hz)
                    .unwrap_or(50_000_000);
                if target_clock_hz == 0 {
                    return Err("Target clock frequency must be nonzero".into());
                }
                if exec_timeout_ms <= simulator_timeout_ms {
                    return Err("--exec-timeout-ms must exceed the simulator timeout, plus build time".into());
                }
                let simulator_args = self
                    .simulator_arg
                    .or(file.simulator_arg)
                    .unwrap_or_default();
                if !simulator_args.iter().any(|a| a.contains("{elf}"))
                    || !simulator_args.iter().any(|a| a.contains("{vcd}"))
                {
                    return Err(
                        "Simulator arguments must include {elf} and {vcd} placeholders".into(),
                    );
                }
                BackendConfig::Simulation {
                    simulator_path: require(
                        "simulator-path",
                        self.simulator_path.or(file.simulator_path),
                    )?,
                    simulator_working_dir: self
                        .simulator_working_dir
                        .or(file.simulator_working_dir)
                        .unwrap_or_else(|| ".".into()),
                    simulator_args,
                    target_scope: require_text(
                        "target-scope",
                        self.target_scope.or(file.target_scope),
                    )?,
                    exclude_signal_file: self.exclude_signal_file.or(file.exclude_signal_file),
                    vcd_dir: self.vcd_dir.or(file.vcd_dir),
                    simulator_timeout_ms,
                    target_clock_hz,
                }
            }
        };

        let workers = self.workers.or(file.workers).unwrap_or(1);
        let builds = self
            .worker_assembly_environment
            .or(file.worker_assembly_environment)
            .unwrap_or_default();
        let simulators = self
            .worker_simulator_working_dir
            .or(file.worker_simulator_working_dir)
            .unwrap_or_default();
        let worker_workspace = self.worker_workspace.or(file.worker_workspace);
        let worker_dirs = if workers == 1 {
            if !builds.is_empty() || !simulators.is_empty() || worker_workspace.is_some() {
                return Err("Worker directories require --workers greater than 1".into());
            }
            None
        } else {
            if workers == 0 || !matches!(backend, BackendConfig::Simulation { .. }) {
                return Err(
                    "Parallel workers require --workers >= 2 and the simulation backend".into(),
                );
            }
            if worker_workspace.is_some() && (!builds.is_empty() || !simulators.is_empty()) {
                return Err(
                    "--worker-workspace cannot be combined with worker directory lists".into(),
                );
            }
            if let Some(workspace) = &worker_workspace {
                let parent = workspace
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."));
                let parent = canonical_directory(parent)?;
                let name = workspace
                    .file_name()
                    .ok_or("Worker workspace needs a directory name")?;
                let root = parent.join(name);
                let template = canonical_directory(&assembly_environment)?;
                if root.starts_with(&template) || template.starts_with(&root) {
                    return Err("Worker workspace and assembly template must not overlap".into());
                }
                Some(
                    (0..workers)
                        .map(|index| {
                            let worker = root.join(format!("worker-{index:04}"));
                            WorkerDirs {
                                assembly: worker.join("build"),
                                simulator: worker.join("sim"),
                            }
                        })
                        .collect(),
                )
            } else {
                if builds.len() != workers || simulators.len() != workers {
                    return Err("Provide one worker assembly environment and simulator working directory per worker".into());
                }
                let mut dirs = Vec::with_capacity(workers);
                for (assembly, simulator) in builds.into_iter().zip(simulators) {
                    let assembly = canonical_directory(&assembly)?;
                    let simulator = canonical_directory(&simulator)?;
                    for previous in &dirs {
                        let previous: &WorkerDirs = previous;
                        for first in [&assembly, &simulator] {
                            for second in [&previous.assembly, &previous.simulator] {
                                if first.starts_with(second) || second.starts_with(first) {
                                    return Err(format!(
                                        "Worker directories overlap: {} and {}",
                                        first.display(),
                                        second.display()
                                    ));
                                }
                            }
                        }
                    }
                    dirs.push(WorkerDirs {
                        assembly,
                        simulator,
                    });
                }
                Some(dirs)
            }
        };
        Ok(Config {
            assembly_environment,
            grammar,
            grammar_depth,
            grammar_seeds,
            riscv_toolpath: require(
                "riscv-toolpath",
                self.riscv_toolpath.or(file.riscv_toolpath),
            )?,
            solutions_path: require(
                "solutions-path",
                self.solutions_path.or(file.solutions_path),
            )?,
            snippet_dir: self.snippet_dir.or(file.snippet_dir),
            log_file: self
                .log_file
                .or(file.log_file)
                .unwrap_or_else(|| "fuzzer.log".into()),
            log_level: self.log_level.or(file.log_level),
            monitor: self.monitor.or(file.monitor).unwrap_or_default(),
            backend,
            exec_timeout_ms,
            worker_dirs,
            worker_workspace,
            global_best: None,
        })
    }
}

fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
    let path = std::fs::canonicalize(path)
        .map_err(|e| format!("Cannot resolve worker directory {}: {e}", path.display()))?;
    if !path.is_dir() {
        return Err(format!(
            "Worker path is not a directory: {}",
            path.display()
        ));
    }
    Ok(path)
}

impl Config {
    /// Points this config at worker `index`'s own directories. Called by the
    /// `Launcher` closure in parallel mode with the 0-based worker index; the
    /// workspace itself was already prepared by [`crate::parallel::prepare_workspace`].
    pub fn select_worker(&mut self, index: usize) -> Result<(), String> {
        let dirs = self
            .worker_dirs
            .take()
            .ok_or("Worker index requires parallel configuration")?;
        let worker = dirs.get(index).ok_or("Worker index out of range")?;
        // The workspace was already prepared by prepare_workspace in the parent
        // (launcher mode); each forked child just selects its own directories.
        self.assembly_environment = worker.assembly.clone();
        match &mut self.backend {
            BackendConfig::Simulation {
                simulator_working_dir,
                ..
            } => *simulator_working_dir = worker.simulator.clone(),
            _ => return Err("Parallel workers require simulation".into()),
        }
        let name = format!("worker-{index:04}");
        self.global_best = Some((self.solutions_path.join("parallel-exchange"), index));
        self.solutions_path = self.solutions_path.join(&name);
        self.snippet_dir = self.snippet_dir.take().map(|dir| dir.join(&name));
        // Logging is initialized once in the broker before forking, so all
        // workers share the central log file; log_file is intentionally untouched.
        Ok(())
    }
}

/// Reads and parses a TOML config file.
fn load_file(path: &Path) -> Result<Args, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("Reading {}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("Parsing {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal set of settings; the grammar defaults to grammars/riscv64g.json,
    /// which exists in the repo, so tests can resolve without extra setup.
    fn common() -> Args {
        Args {
            assembly_environment: Some("build".into()),
            riscv_toolpath: Some("riscv".into()),
            solutions_path: Some("solutions".into()),
            ..Args::default()
        }
    }

    fn simulation() -> Args {
        Args {
            backend: Some(BackendKind::Simulation),
            simulator_path: Some("simulator".into()),
            simulator_arg: Some(vec!["{elf}".into(), "--trace={vcd}".into()]),
            target_scope: Some("TOP.dut".into()),
            ..common()
        }
    }

    #[cfg(feature = "hardware")]
    #[test]
    fn defaults_to_existing_hardware_settings() {
        let config = Args {
            scope_resource: Some("scope".into()),
            ..common()
        }
        .merge(Args::default())
        .unwrap();
        match config.backend {
            BackendConfig::Hardware {
                openocd_addr,
                scope_resource,
                scope_timeout_ms,
                scope_acquire_timeout_ms,
                settle_ms,
            } => {
                assert_eq!(openocd_addr, "127.0.0.1:6666");
                assert_eq!(scope_resource, "scope");
                assert_eq!(
                    (scope_timeout_ms, scope_acquire_timeout_ms, settle_ms),
                    (5000, 30000, 100)
                );
            }
            _ => panic!("expected hardware"),
        }
        assert_eq!(config.exec_timeout_ms, 900_000);
    }

    #[test]
    fn simulation_needs_no_scope_and_cli_overrides_file() {
        let file: Args = toml::from_str(
            r#"
            backend = "simulation"
            simulator_path = "sim"
            simulator_arg = ["{elf}", "{vcd}"]
            target_scope = "TOP.dut"
        "#,
        )
        .unwrap();
        let cli = Args::try_parse_from([
            "fuzzer",
            "--simulator-arg={elf}",
            "--simulator-arg=--trace={vcd}",
            "--target-scope",
            "TOP.other",
        ])
        .unwrap();
        let config = Args {
            assembly_environment: Some("build".into()),
            riscv_toolpath: Some("riscv".into()),
            solutions_path: Some("solutions".into()),
            ..cli
        }
        .merge(file)
        .unwrap();
        match config.backend {
            BackendConfig::Simulation {
                simulator_args,
                target_scope,
                target_clock_hz,
                ..
            } => {
                assert_eq!(simulator_args, ["{elf}", "--trace={vcd}"]);
                assert_eq!(target_scope, "TOP.other");
                // Not given anywhere: the 50 MHz default applies.
                assert_eq!(target_clock_hz, 50_000_000);
            }
            _ => panic!("expected simulation"),
        }
    }

    #[test]
    fn target_clock_hz_comes_from_file_and_cli_overrides() {
        let file = || {
            toml::from_str::<Args>(
                r#"
            target_clock_hz = 100_000_000
        "#,
            )
            .unwrap()
        };
        // File value is used when the CLI does not set it.
        let config = simulation().merge(file()).unwrap();
        match config.backend {
            BackendConfig::Simulation { target_clock_hz, .. } => {
                assert_eq!(target_clock_hz, 100_000_000);
            }
            _ => panic!("expected simulation"),
        }
        // CLI overrides the file.
        let cli = Args::try_parse_from([
            "fuzzer",
            "--backend",
            "simulation",
            "--simulator-path",
            "sim",
            "--simulator-arg={elf}",
            "--simulator-arg={vcd}",
            "--target-scope",
            "TOP.dut",
            "--target-clock-hz",
            "25000000",
        ])
        .unwrap();
        let config = Args {
            assembly_environment: Some("build".into()),
            riscv_toolpath: Some("riscv".into()),
            solutions_path: Some("solutions".into()),
            ..cli
        }
        .merge(file())
        .unwrap();
        match config.backend {
            BackendConfig::Simulation { target_clock_hz, .. } => {
                assert_eq!(target_clock_hz, 25_000_000);
            }
            _ => panic!("expected simulation"),
        }
    }

    #[test]
    fn example_simulation_config_resolves() {
        let file: Args = toml::from_str(include_str!("../fuzzer.simulation.example.toml")).unwrap();
        let config = Args::default().merge(file).unwrap();
        assert!(matches!(config.backend, BackendConfig::Simulation { .. }));
    }

    #[cfg(feature = "hardware")]
    #[test]
    fn cli_backend_overrides_file() {
        let file = simulation();
        let cli = Args::try_parse_from([
            "fuzzer",
            "--backend",
            "hardware",
            "--scope-resource",
            "scope",
        ])
        .unwrap();
        let config = cli.merge(file).unwrap();
        assert!(matches!(config.backend, BackendConfig::Hardware { .. }));
    }

    #[test]
    fn parallel_worker_paths_are_isolated_and_selected() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let mut args = simulation();
        args.workers = Some(2);
        args.worker_assembly_environment = Some(vec![a.clone(), b.clone()]);
        args.worker_simulator_working_dir = Some(vec![a.clone(), b.clone()]);
        let mut config = args.merge(Args::default()).unwrap();
        config.select_worker(1).unwrap();
        assert_eq!(config.assembly_environment, b.canonicalize().unwrap());
        assert!(config.solutions_path.ends_with("worker-0001"));
        // Central logging: the log file stays shared, no per-worker suffix.
        assert!(!config.log_file.to_string_lossy().contains("worker-"));
        assert!(config.worker_dirs.is_none());

        let mut args = simulation();
        args.workers = Some(2);
        args.worker_assembly_environment = Some(vec![a.clone(), a.clone()]);
        args.worker_simulator_working_dir = Some(vec![a, b]);
        assert!(args.merge(Args::default()).unwrap_err().contains("overlap"));
    }

    #[test]
    fn rejects_invalid_simulation_settings() {
        let mut args = simulation();
        args.simulator_arg = Some(vec!["{elf}".into()]);
        assert!(args.merge(Args::default()).unwrap_err().contains("{vcd}"));
        let mut args = simulation();
        args.simulator_timeout_ms = Some(0);
        assert!(args.merge(Args::default()).unwrap_err().contains("nonzero"));
        let mut args = simulation();
        args.exec_timeout_ms = Some(90_000);
        assert!(args
            .merge(Args::default())
            .unwrap_err()
            .contains("simulator timeout"));
        let mut args = simulation();
        args.target_scope = Some(" ".into());
        assert!(
            args.merge(Args::default())
                .unwrap_err()
                .contains("target-scope")
        );
        let mut args = simulation();
        args.target_clock_hz = Some(0);
        assert!(
            args.merge(Args::default())
                .unwrap_err()
                .contains("clock")
        );
    }

    #[cfg(not(feature = "hardware"))]
    #[test]
    fn hardware_without_feature_is_rejected() {
        assert!(
            common()
                .merge(Args::default())
                .unwrap_err()
                .contains("hardware feature")
        );
    }

    #[test]
    fn log_level_and_monitor_come_from_file_and_cli_overrides() {
        let file: Args = toml::from_str(
            r#"
            log_level = "debug"
            monitor = "log"
        "#,
        )
        .unwrap();
        let config = simulation().merge(file).unwrap();
        assert_eq!(config.log_level.as_deref(), Some("debug"));
        assert_eq!(config.monitor, MonitorKind::Log);

        let cli = Args {
            log_level: Some("warn".into()),
            monitor: Some(MonitorKind::None),
            ..simulation()
        };
        let file: Args = toml::from_str(
            r#"
            log_level = "debug"
            monitor = "log"
        "#,
        )
        .unwrap();
        let config = cli.merge(file).unwrap();
        assert_eq!(config.log_level.as_deref(), Some("warn"));
        assert_eq!(config.monitor, MonitorKind::None);

        let config = simulation().merge(Args::default()).unwrap();
        assert_eq!(config.log_level, None);
        assert_eq!(config.monitor, MonitorKind::Tui);
    }

    #[test]
    fn grammar_settings_have_defaults_and_are_validated() {
        // Defaults: repo-relative grammar file, depth 10, 64 seeds.
        let config = simulation().merge(Args::default()).unwrap();
        assert!(config.grammar.ends_with("grammars/riscv64g.json"));
        assert_eq!(
            (config.grammar_depth, config.grammar_seeds),
            (DEFAULT_GRAMMAR_DEPTH, DEFAULT_GRAMMAR_SEEDS)
        );

        let mut args = simulation();
        args.grammar = Some("does/not/exist.json".into());
        assert!(args
            .merge(Args::default())
            .unwrap_err()
            .contains("--grammar"));
        let mut args = simulation();
        args.grammar_depth = Some(0);
        assert!(args
            .merge(Args::default())
            .unwrap_err()
            .contains("grammar-depth"));
        let mut args = simulation();
        args.grammar_seeds = Some(0);
        assert!(args
            .merge(Args::default())
            .unwrap_err()
            .contains("grammar-seeds"));

        // CLI overrides the config file.
        let file: Args = toml::from_str(
            r#"
            grammar_depth = 3
            grammar_seeds = 7
        "#,
        )
        .unwrap();
        let config = simulation().merge(file).unwrap();
        assert_eq!((config.grammar_depth, config.grammar_seeds), (3, 7));
    }

    #[test]
    fn removed_worker_index_args_are_rejected() {
        // --worker-index and --internal-launcher-child were removed with the custom
        // launcher; the Launcher assigns worker IDs internally.
        assert!(Args::try_parse_from(["fuzzer", "--worker-index", "3"]).is_err());
        assert!(Args::try_parse_from(["fuzzer", "--internal-launcher-child"]).is_err());
        assert!(Args::try_parse_from(["fuzzer", "--internal-worker-index", "0"]).is_err());
    }
}
