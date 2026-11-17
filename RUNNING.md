# Running the Project

## Requirements

Both backends need:

* Rust and Cargo;
* a RISC-V toolchain, `make` and the `atndecode` decoder;
* an assembly environment directory with a Makefile that builds `snippet.elf` from `src/snippet.S`.

The default hardware backend additionally needs OpenOCD, a debug adapter, a
supported target, and a Teledyne oscilloscope reachable through NI-VISA.
The Unix simulation backend needs a built Chipyard simulator and the
`vcdtogglerate` executable.

The full setup has not yet been tested on the final hardware.

LibAFL dependencies are specified in `Cargo.toml`.

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
For a simulation-only build without the NI-VISA dependency, use
`cargo build --release --no-default-features`. Such a build cannot select
hardware or use `--list-scopes`.

## Prepare the Build Environment

Set these in the config file or on the command line:

* `assembly_environment`: directory containing the Makefile. For each input the fuzzer writes `src/snippet.S` there (the `src` directory must exist), runs `make -B` and loads `snippet.elf`;
* `atndecode_path`: decoder that reads the bitstream on stdin and writes assembly to stdout;
* `riscv_toolpath`: RISC-V toolchain directory, passed to `make` as the environment variable `RISCV`.

For hardware, OpenOCD must be able to read the ELF in `assembly_environment`.
For simulation, the ELF path is passed to the configured simulator.

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

## Simulation backend

Select `--backend simulation` or `backend = "simulation"` in TOML; the default
is hardware. See `fuzzer.simulation.example.toml`. The backend accepts paths to the built simulator and
`vcdtogglerate`, a simulator working directory, ordered simulator arguments, a
target scope, an optional exclusion file, and separate process timeouts.
Relative configured paths resolve from the fuzzer's starting directory, not the
simulator working directory. Arguments are passed directly without a shell:
`{elf}` and `{vcd}` are replaced within each argument by absolute paths. Both
placeholders are required. The user must supply the Chipyard-specific flags,
including the fixed-stop flag. The simulator must produce a usable VCD and either
exit successfully or report the recognized Chipyard cycle-limit termination
described below. No simulator flags are assumed.

The TOML `simulator_arg` is an ordered array. On the command line, repeat
`--simulator-arg=VALUE` for each argument; if any are supplied they replace
the entire TOML array. All other CLI values also override TOML. Both simulator
and analyzer timeouts must be positive, and `exec_timeout_ms` must exceed their
sum; allow additional time for building the ELF.

The backend runs `vcdtogglerate <VCD> --target-scope <scope>`, optionally with
`--exclude-signal-file <path>`. It maximizes the reported `toggle_coverage`
fraction and records `toggle_activity` in signal-level changes per simulated
second. The analyzer uses the last change of a counted signal as its time
denominator; it may report zero for an empty selection or missing timing.
Keep the scope, exclusions and simulation window consistent between runs.
Use separate solutions directories for hardware, simulation and changed
simulation configurations, since their scores are not comparable.

For Chipyard, provide `+max-cycles=<limit>`, `+vcdfile={vcd}` and, when
using fast loading, `+loadmem={elf}` as simulator arguments, with `{elf}`
also supplied as the positional binary. Wrap simulator plusargs in
`+permissive` and `+permissive-off` as required by the host. Include
`+dramsim` and `+dramsim_ini_dir=<path>` if needed. The backend accepts
Chipyard's explicit cycle-limit timeout diagnostic with exit status 255 or
SIGABRT by default, then analyzes the VCD; other nonzero exits and wall-clock
timeouts remain failures. Successful simulator exits are also accepted, so a
program finishing early can yield a shorter trace. Simulator core dumps are
disabled. The limit must be configured explicitly and is checked after reset.
An abort may leave a partially flushed VCD: a successful analyzer result does
not prove the trace includes every cycle. Validate the trace window for the
specific simulator and workload before comparing scores.

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

## Parallel simulation workers

Set `workers = 2` (or `--workers 2`) and `worker_workspace = "/path/to/new-workspace"`
(or `--worker-workspace /path/to/new-workspace`) to create directories automatically.
The workspace parent must exist and the workspace itself must not exist; it is
never overwritten or automatically removed. The launcher copies the complete
`assembly_environment` into each `worker-NNNN/build` and creates an empty
`worker-NNNN/sim` working directory. The assembly template must not contain
symlinks or special files. Absolute paths, Makefile references and simulator
arguments must not cause workers to write to a shared location. Simulator
resources needed in its working directory must be supplied using absolute paths;
the simulator working-directory template is not copied in this mode. If
preparation fails, partial directories are retained for inspection.

Alternatively, omit `worker_workspace` and provide an ordered
`worker_assembly_environment` and `worker_simulator_working_dir` array with one
existing directory per worker. On the CLI, repeat
`--worker-assembly-environment PATH` and `--worker-simulator-working-dir PATH`.
The original `assembly_environment` remains required, but the per-worker
directories are used when parallel workers are enabled.

With explicit lists, create independent, complete copies of the build environment
and simulator working directory yourself. Directories for
different workers must not overlap. Avoid shared writable paths or symlinks inside
those directories: canonical directory checks cannot detect a Makefile or simulator
that writes to an external shared location. Parallel operation is available only
for simulation, not a single shared hardware target.

Each worker keeps its own LibAFL corpus and local `best.S` under
`<solutions_path>/worker-0000`, `worker-0001`, and so on. Workers publish their
local improvements and periodically import distinct inputs published by peers,
re-evaluating them through LibAFL and adding them to the receiving corpus.
The shared `<solutions_path>/parallel-exchange/global-best.txt` records the
full-precision score, activity, worker, and paths to the corresponding immutable
input and assembly relative to `parallel-exchange`. Global improvements are also
printed on the launcher terminal. The exchange directory must not already exist:
use a fresh solutions path for each run, especially if simulation settings change.
Input sharing is filesystem-based and is not instantaneous; each imported input
is re-simulated by each receiving worker. Snippet archives use the same subdirectories,
and logs are named `<log_file>.worker-0000`, etc. Worker TUI output is suppressed.
If a worker exits, the launcher stops the others and reports an error. When the
launcher exits, workers and their active build processes are terminated; active
simulation supervisors then clean up their own VCDs.
