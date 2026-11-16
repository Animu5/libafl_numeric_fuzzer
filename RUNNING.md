# Running the Project

## Requirements

To build and run the current version, you need:

* Rust and Cargo;
* a local LibAFL source checkout;
* OpenOCD with RISC-V target support;
* an OpenOCD-compatible debug adapter;
* a supported RISC-V target;
* a RISC-V toolchain, `make` and the `atndecode` decoder;
* an assembly environment directory with a Makefile that builds `snippet.elf` from `src/snippet.S`;
* a Teledyne oscilloscope reachable through NI-VISA (USB).

The full setup has not yet been tested on the final hardware.

## Prepare the LibAFL Dependency

`Cargo.toml` uses local path dependencies:

```toml
libafl = { path = "../LibAFL/crates/libafl", features = ["tui_monitor", "std"] }
libafl_bolts = { path = "../LibAFL/crates/libafl_bolts" }
```
The LibAFL repository must be placed in the same parent folder as the `libafl_fuzzer` project. This is required because `Cargo.toml` uses local paths to access the LibAFL crates.

If LibAFL is stored somewhere else, update the paths in `Cargo.toml` before building.

## Build the Project

Open a terminal in the project folder and check that Rust and Cargo are available:

```powershell
rustc --version
cargo --version
```

Check whether the project compiles:

```powershell
cargo check
```

Build the project with:

```powershell
cargo build
```

For an optimized release build, use:

```powershell
cargo build --release
```

If the build succeeds, the compiled files will be created in the `target` directory.

## Prepare the Build Environment

Set these in the config file or on the command line:

* `assembly_environment`: directory containing the Makefile. For each input the fuzzer writes `src/snippet.S` there (the `src` directory must exist), runs `make -B` and loads `snippet.elf`;
* `atndecode_path`: decoder that reads the bitstream on stdin and writes assembly to stdout;
* `riscv_toolpath`: RISC-V toolchain directory, passed to `make` as the environment variable `RISCV`.

The ELF is loaded by OpenOCD, so OpenOCD must be able to read `assembly_environment`.

## Start OpenOCD

OpenOCD must already be running before the Rust program is started.

The exact command depends on the debug adapter and RISC-V target. In general, OpenOCD is started with an interface configuration and a target configuration:

```powershell
openocd -f interface/<interface-config>.cfg -f target/<target-config>.cfg
```

The current Rust code connects to the OpenOCD Tcl interface at:

`127.0.0.1:6666`

The selected OpenOCD configuration must enable the Tcl interface on port `6666`.

The correct interface and target configurations for the final hardware still need to be confirmed.

## Run the Program

After OpenOCD is running, open another terminal in the project folder and run, for example:

```powershell
cargo run --release -- --config fuzzer.toml
```

See `fuzzer.example.toml` for all options. Command-line flags override the file.

## Fuzzing Behaviour

The corpus starts with `--seed` (a file or a directory of files; default: 16 zero bytes). Inputs are mutated with havoc mutators, decoded and built into a program, loaded and measured. Inputs that cannot be decoded or built are logged and skipped. If loading or resuming a program fails (e.g. it crashed the target), the target is reset with `reset halt` and the input is skipped. The run aborts if that reset fails (e.g. OpenOCD died), after 10 consecutive target failures, or after 5 consecutive measurement failures.

Inputs with a new best F1 delta are logged as `New best measurement delta: <value>` and saved to the solutions directory, with the delta in the file name. Each saved input is also stored as assembly, next to the binary input as `<name>.S` (ready to assemble: it includes `init.inc`). The assembly of the current best input is kept in `best.S`, so on an interrupt (Ctrl-C) the best snippet so far is always on disk.

## Logging

Logs go to `fuzzer.log` (override with `--log-file`); the file is overwritten on every start. Filter per module with `RUST_LOG`, e.g. `RUST_LOG=warn,libafl_fuzzer::openocd=debug`.

## Saving snippets

`--snippet-dir <DIR>` (TOML `snippet_dir`) additionally saves every decoded snippet as `NNNNNN.S`, numbered in execution order, so you can inspect what was built and loaded. The number appears in the debug log (`Saved snippet to …`), which lets you match a crash to its snippet. Snippets are not deleted, so this can use a lot of disk space on long runs.

## Run timeout

LibAFL kills the process if one run takes longer than its executor timeout (default 5 s), which a build, a program load and two scope acquisitions exceed. `--exec-timeout-ms` (default 120000) sets it; it must be larger than the sum of build time, load time, both acquisitions and `--settle-ms`. A timeout shows up in the log as `Timeout in fuzz run.`

## Oscilloscope (VISA)

Builds need the NI-VISA library; `.cargo/config.toml` sets `LIB_VISA_PATH` (adjust for your system). Use `--list-scopes` to find the resource string, then `--scope-resource <STRING>` (required).

Arm/acquire commands block until the scope has captured a full acquisition, so they use `--scope-acquire-timeout-ms` (default 30000). It must exceed the acquisition time. All other queries use `--scope-timeout-ms` (default 5000). A missed deadline is reported as a timeout and the scope buffers are cleared.

The idle level is captured (stored in M1, not read back) after the target is reset and halted. The program is then resumed and, after `--settle-ms`, the running level is captured and F1 is read once. F1 is the difference between the running and idle level, and is the score.

## Configuration file

All options can be set in a TOML file passed with `--config` (see `fuzzer.example.toml`). Command-line flags override file values.

## Source layout

`cli.rs` (arguments/config), `logging.rs`, `feedback.rs` (numeric feedback), `harness.rs` (per-input build and evaluation dispatch), `evaluation.rs` (backend contract), `hardware.rs` (load and idle/run measurement), `snippet.rs` (decode and make), `measurement.rs`, `openocd.rs`, `scope.rs` (VISA), `main.rs` (wiring).

## Simulation backend (not yet selectable)

`simulation.rs` implements the Unix simulation backend; CLI/TOML selection is
the next integration step. It accepts paths to the built simulator and
`vcdtogglerate`, a simulator working directory, ordered simulator arguments, a
target scope, an optional exclusion file, and separate process timeouts.
Relative configured paths resolve from the fuzzer's starting directory, not the
simulator working directory. Arguments are passed directly without a shell:
`{elf}` and `{vcd}` are replaced within each argument by absolute paths. Both
placeholders are required. The user must supply the Chipyard-specific flags,
including the fixed-stop flag, and ensure the simulator exits successfully and
flushes the specified VCD. No simulator flags are assumed.

The backend runs `vcdtogglerate <VCD> --target-scope <scope>`, optionally with
`--exclude-signal-file <path>`. It maximizes the reported `toggle_coverage`
fraction and records `toggle_activity` in signal-level changes per simulated
second. The analyzer uses the last change of a counted signal as its time
denominator; it may report zero for an empty selection or missing timing.
Keep the scope, exclusions and simulation window consistent between runs.

Each evaluation uses a separate supervisor that owns the temporary VCD directory
and monitors a parent-liveness pipe. On a subprocess timeout, or if the fuzzer
exits during simulation or analysis, it terminates the active process group,
reaps the direct child and removes the temporary directory. The supervisor is
isolated from the fuzzer's terminal process group, so an interrupt to the fuzzer
also closes the pipe. Configure LibAFL's outer timeout above build time plus both
subprocess deadlines. The existing decoder and `make` still have no individual
timeouts and are not supervised by this mechanism.

A machine crash, termination of the supervisor itself, or subprocesses that
deliberately leave their process group can still leave processes or artifacts
behind.
