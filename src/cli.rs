use clap::{CommandFactory, Parser, ValueEnum};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, Deserialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum BackendKind {
    #[default]
    Hardware,
    Simulation,
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

    /// Path to the tool that decodes the bitstream into RISC-V assembly
    #[arg(short = 'd', long)]
    atndecode_path: Option<PathBuf>,

    /// RISC-V toolchain directory, passed to `make` as the `RISCV` environment variable
    #[arg(long)]
    riscv_toolpath: Option<PathBuf>,

    /// Directory where inputs with the new best measurement are stored,
    /// as binary plus the matching assembly as `<name>.S` and `best.S`
    #[arg(short, long)]
    solutions_path: Option<PathBuf>,

    /// Directory where every decoded snippet is saved as NNNNNN.S, for debugging (default: off)
    #[arg(long)]
    snippet_dir: Option<PathBuf>,

    /// Initial input file, or a directory whose files are all used as seeds (default: 16 zero bytes)
    #[arg(long)]
    seed: Option<PathBuf>,

    /// Log file (default: fuzzer.log)
    #[arg(long)]
    log_file: Option<PathBuf>,

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

    /// Path to the vcdtogglerate executable
    #[arg(long)]
    analyzer_path: Option<PathBuf>,

    /// VCD scope passed to vcdtogglerate
    #[arg(long)]
    target_scope: Option<String>,

    /// Optional vcdtogglerate exclusion file
    #[arg(long)]
    exclude_signal_file: Option<PathBuf>,

    /// Simulator timeout in milliseconds (default: 60000)
    #[arg(long)]
    simulator_timeout_ms: Option<u64>,

    /// Analyzer timeout in milliseconds (default: 30000)
    #[arg(long)]
    analyzer_timeout_ms: Option<u64>,

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

    /// LibAFL timeout for one complete run (build, load, both measurements) in milliseconds (default: 120000)
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
    pub atndecode_path: PathBuf,
    pub riscv_toolpath: PathBuf,
    pub solutions_path: PathBuf,
    pub snippet_dir: Option<PathBuf>,
    pub seed: Option<PathBuf>,
    pub log_file: PathBuf,
    pub backend: BackendConfig,
    pub exec_timeout_ms: u64,
}

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
        analyzer_path: PathBuf,
        target_scope: String,
        exclude_signal_file: Option<PathBuf>,
        simulator_timeout_ms: u64,
        analyzer_timeout_ms: u64,
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
        let atndecode_path = require(
            "atndecode-path",
            self.atndecode_path.or(file.atndecode_path),
        )?;

        let exec_timeout_ms = self
            .exec_timeout_ms
            .or(file.exec_timeout_ms)
            .unwrap_or(120_000);
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
                    .unwrap_or(60_000);
                let analyzer_timeout_ms = self
                    .analyzer_timeout_ms
                    .or(file.analyzer_timeout_ms)
                    .unwrap_or(30_000);
                if simulator_timeout_ms == 0 || analyzer_timeout_ms == 0 {
                    return Err("Simulator and analyzer timeouts must be nonzero".into());
                }
                if simulator_timeout_ms
                    .checked_add(analyzer_timeout_ms)
                    .is_none_or(|sum| exec_timeout_ms <= sum)
                {
                    return Err("--exec-timeout-ms must exceed the sum of simulator and analyzer timeouts, plus build time".into());
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
                    analyzer_path: require(
                        "analyzer-path",
                        self.analyzer_path.or(file.analyzer_path),
                    )?,
                    target_scope: require_text(
                        "target-scope",
                        self.target_scope.or(file.target_scope),
                    )?,
                    exclude_signal_file: self.exclude_signal_file.or(file.exclude_signal_file),
                    simulator_timeout_ms,
                    analyzer_timeout_ms,
                }
            }
        };

        Ok(Config {
            assembly_environment,
            atndecode_path,
            riscv_toolpath: require(
                "riscv-toolpath",
                self.riscv_toolpath.or(file.riscv_toolpath),
            )?,
            solutions_path: require(
                "solutions-path",
                self.solutions_path.or(file.solutions_path),
            )?,
            snippet_dir: self.snippet_dir.or(file.snippet_dir),
            seed: self.seed.or(file.seed),
            log_file: self
                .log_file
                .or(file.log_file)
                .unwrap_or_else(|| "fuzzer.log".into()),
            backend,
            exec_timeout_ms,
        })
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

    fn common() -> Args {
        Args {
            assembly_environment: Some("build".into()),
            atndecode_path: Some("decode".into()),
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
            analyzer_path: Some("analyzer".into()),
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
        assert_eq!(config.exec_timeout_ms, 120_000);
    }

    #[test]
    fn simulation_needs_no_scope_and_cli_overrides_file() {
        let file: Args = toml::from_str(
            r#"
            backend = "simulation"
            simulator_path = "sim"
            simulator_arg = ["{elf}", "{vcd}"]
            analyzer_path = "analyzer"
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
            atndecode_path: Some("decode".into()),
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
                ..
            } => {
                assert_eq!(simulator_args, ["{elf}", "--trace={vcd}"]);
                assert_eq!(target_scope, "TOP.other");
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
    fn rejects_invalid_simulation_settings() {
        let mut args = simulation();
        args.simulator_arg = Some(vec!["{elf}".into()]);
        assert!(args.merge(Args::default()).unwrap_err().contains("{vcd}"));
        let mut args = simulation();
        args.analyzer_timeout_ms = Some(0);
        assert!(args.merge(Args::default()).unwrap_err().contains("nonzero"));
        let mut args = simulation();
        args.exec_timeout_ms = Some(90_000);
        assert!(args.merge(Args::default()).unwrap_err().contains("sum"));
        let mut args = simulation();
        args.target_scope = Some(" ".into());
        assert!(args
            .merge(Args::default())
            .unwrap_err()
            .contains("target-scope"));
    }

    #[cfg(not(feature = "hardware"))]
    #[test]
    fn hardware_without_feature_is_rejected() {
        assert!(common()
            .merge(Args::default())
            .unwrap_err()
            .contains("hardware feature"));
    }
}
