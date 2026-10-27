use clap::{CommandFactory, Parser};
use serde::Deserialize;
use std::path::{Path, PathBuf};

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

    /// Directory where inputs with a new best measurement are stored
    #[arg(short, long)]
    solutions_path: Option<PathBuf>,

    /// Initial input file, or a directory whose files are all used as seeds (default: 16 zero bytes)
    #[arg(long)]
    seed: Option<PathBuf>,

    /// Log file (default: fuzzer.log)
    #[arg(long)]
    log_file: Option<PathBuf>,

    /// OpenOCD Tcl RPC address (default: 127.0.0.1:6666)
    #[arg(long)]
    openocd_addr: Option<String>,

    /// VISA resource string of the oscilloscope (e.g. USB0::0x05FF::...::INSTR); see --list-scopes
    #[arg(long)]
    scope_resource: Option<String>,

    /// Scope I/O timeout in milliseconds (default: 5000)
    #[arg(long)]
    scope_timeout_ms: Option<u64>,

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
    pub seed: Option<PathBuf>,
    pub log_file: PathBuf,
    pub openocd_addr: String,
    pub scope_resource: String,
    pub scope_timeout_ms: u64,
    pub settle_ms: u64,
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
        let require = |name: &str, v: Option<PathBuf>| -> PathBuf {
            v.unwrap_or_else(|| {
                fail(format!(
                    "--{name} is required (on the command line or in the config file)"
                ))
            })
        };

        let assembly_environment = require(
            "assembly-environment",
            self.assembly_environment.or(file.assembly_environment),
        );
        let atndecode_path = require(
            "atndecode-path",
            self.atndecode_path.or(file.atndecode_path),
        );

        Config {
            assembly_environment,
            atndecode_path,
            riscv_toolpath: require(
                "riscv-toolpath",
                self.riscv_toolpath.or(file.riscv_toolpath),
            ),
            solutions_path: require(
                "solutions-path",
                self.solutions_path.or(file.solutions_path),
            ),
            seed: self.seed.or(file.seed),
            log_file: self
                .log_file
                .or(file.log_file)
                .unwrap_or_else(|| "fuzzer.log".into()),
            openocd_addr: self
                .openocd_addr
                .or(file.openocd_addr)
                .unwrap_or_else(|| "127.0.0.1:6666".into()),
            scope_resource: self
                .scope_resource
                .or(file.scope_resource)
                .unwrap_or_else(|| {
                    fail("--scope-resource is required (use --list-scopes to find it)".into())
                }),
            scope_timeout_ms: self
                .scope_timeout_ms
                .or(file.scope_timeout_ms)
                .unwrap_or(5000),
            settle_ms: self.settle_ms.or(file.settle_ms).unwrap_or(100),
        }
    }
}

/// Reads and parses a TOML config file.
fn load_file(path: &Path) -> Result<Args, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("Reading {}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("Parsing {}: {e}", path.display()))
}
