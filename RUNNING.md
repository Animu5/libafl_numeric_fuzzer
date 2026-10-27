# Running the Project

## Requirements

To build and run the current version, you need:

* Rust and Cargo;
* a local LibAFL source checkout;
* OpenOCD with RISC-V target support;
* an OpenOCD-compatible debug adapter;
* a supported RISC-V target;
* a target ELF file;
* a Teledyne oscilloscope reachable through NI-VISA (USB).

The measurement is taken from the oscilloscope through VISA; `ScopeMeasurement::measure` in `src/measurement.rs` still has to be implemented.

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

## Prepare the Target Program

The current code expects the target ELF file at:

`<assembly-environment>/snippet.elf`

If the `testcases` folder does not exist, create it first:

```powershell
New-Item testcases -ItemType Directory
```

Then place the required ELF file inside that folder and name it:

`snippet.elf`

The ELF file must match the RISC-V target used with OpenOCD.

The code sends the path `<assembly-environment>/snippet.elf` to OpenOCD. Therefore, OpenOCD should also be started from the project folder. Otherwise, OpenOCD may not find the ELF file.

## Start OpenOCD

OpenOCD must already be running before the Rust program is started.

Start OpenOCD from the main project folder so that it can resolve the relative ELF path.

The exact command depends on the debug adapter and RISC-V target. In general, OpenOCD is started with an interface configuration and a target configuration:

```powershell
openocd -f interface/<interface-config>.cfg -f target/<target-config>.cfg
```

The current Rust code connects to the OpenOCD Tcl interface at:

`127.0.0.1:6666`

The selected OpenOCD configuration must enable the Tcl interface on port `6666`.

The correct interface and target configurations for the final hardware still need to be confirmed.

## Run the Program

After OpenOCD is running and the required files are prepared, open another terminal in the project folder and run:

```powershell
cargo run
```

For an optimized build, use:

```powershell
cargo run --release
```

A successful initial connection and setup should show messages similar to:

```text
Connected to OpenOCD at 127.0.0.1:6666
Loading program: <assembly-environment>/snippet.elf
Program loaded successfully.
Idle power: 0.0
Running power: 0.0
```

## Current Fuzzing Behaviour

The corpus starts with `--seed` (default: 16 zero bytes), inputs are mutated with havoc mutators and written to target RAM (`--input-addr`). Inputs with a new best measurement are saved to the solutions directory with the delta in the file name. OpenOCD errors and repeated measurement failures abort the run.

When an input produces a new highest measurement, the program logs the following when an input produces a new highest measurement:

```text
New best measurement: <value>
```

During an executed iteration, the current input is written to:

`current_input.bin`

The program creates a `solutions/` directory automatically and configures it as the LibAFL solutions corpus. However, the current numeric feedback is configured as normal corpus feedback, so new best-measurement inputs are added to the in-memory corpus and are not currently saved in `solutions/`.


## Logging

Logs go to `fuzzer.log` (override with `--log-file`). Filter per module with `RUST_LOG`, e.g. `RUST_LOG=warn,libafl_fuzzer::openocd=debug`.

## Oscilloscope (VISA)

Builds need the NI-VISA library; `.cargo/config.toml` sets `LIB_VISA_PATH` (adjust for your system). Use `--list-scopes` to find the resource string, then `--scope-resource <STRING>` (required). Implement `ScopeMeasurement::measure` in `src/measurement.rs`.

The fuzzer scores each input by `running - idle`: idle is measured after `reset init` (target halted), running after `resume` plus `--settle-ms`.

## Configuration file

All options can be set in a TOML file passed with `--config` (see `fuzzer.example.toml`). Command-line flags override file values.

## Source layout

`cli.rs` (arguments/config), `logging.rs`, `feedback.rs` (numeric feedback), `harness.rs` (per-input idle/run measurement), `measurement.rs`, `openocd.rs`, `scope.rs` (VISA), `main.rs` (wiring).
