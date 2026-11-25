# Running the Project

## Requirements

Both backends need:

* Rust and Cargo;
* a RISC-V toolchain and `make`;
* the ISA gate binary (`--isa-gate`, required), built from the asltgen-afl-proxy project (`atnwalk/cmd/isagate`);
* an assembly environment directory with a Makefile that builds `snippet.elf` from `src/snippet.S`.

The default hardware backend additionally needs OpenOCD, a debug adapter, a
supported target, and a Teledyne oscilloscope reachable through NI-VISA.
The Unix simulation backend needs a built Chipyard simulator; the VCD analysis
runs in-process through the integrated `vcdtoggle` module, no external analyzer
binary is needed.

Dependencies are specified in `Cargo.toml`.

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
* `grammar`: Nautilus JSON grammar the inputs are generated from (default `grammars/riscv64g.json`, converted from `grammars/RISCV64G.g4` by `tools/g4_to_nautilus.py`);
* `grammar_depth`: Nautilus tree depth — must exceed the grammar's minimum derivation size (~350 for the bundled grammar) and caps the generated program size (default 512);
* `grammar_seeds`: number of grammar-generated inputs in the initial corpus (default 64);
* `isa_gate`: **required** ISA gate executable. Contract: candidate program on stdin, fixed program on stdout, exit 0; exit nonzero = snippet rejected (skipped, counted in the debug log). Built from the asltgen-afl-proxy project (`atnwalk/cmd/isagate`);
* `isa_gate_timeout_ms`: timeout for one gate invocation (default 5000);
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

## Run the Program

After OpenOCD is running, open another terminal in the project folder and run, for example:

```powershell
cargo run --release -- --config fuzzer.toml
```

See `fuzzer.example.toml` for all options. Command-line flags override the file.

## Fuzzing Behaviour

The corpus starts with `--grammar-seeds` (default 64) inputs generated from the grammar (`--grammar`, default `grammars/riscv64g.json`). Inputs are parse trees mutated with Nautilus's random and recursion mutators (subtree replacement and re-expansion of nonterminals), unparsed into a complete RISC-V assembly program (the grammar emits `_start:`, the register/float-constant initialization and the data section itself, so no `init.inc` is included). Every program then goes through the ISA gate (`--isa-gate`, required): the gate executes it on its ISA model and fixes crashing constructs (memory operands get a clamped offset or an inserted `li xN, <valid address>` when the base register cannot hold one; misaligned `ld`/`lw`/`lh` offsets are realigned), then the fixed program is written to `src/snippet.S` and built, then evaluated: loaded and measured on hardware, simulated and analyzed in simulation mode. Programs the gate cannot fix are rejected and skipped (counted as rejections in the debug log); they are never built. `make` runs with `TMPDIR` set to the absolute path of the assembly environment, so the toolchain never writes intermediate files to `/tmp` (a relative `TMPDIR` is ignored by the toolchain, which then falls back to `/tmp`). Inputs that cannot be built are logged and skipped. If loading or resuming a program fails (e.g. it crashed the target), the target is reset with `reset halt` and the input is skipped. The run aborts if that reset fails (e.g. OpenOCD died), after 10 consecutive target failures, or after 5 consecutive measurement failures (in simulation mode: 10 consecutive simulator failures, 5 consecutive analysis failures).

To regenerate `grammars/riscv64g.json` after editing the vendored grammar, run `python3 tools/g4_to_nautilus.py grammars/RISCV64G.g4 grammars/riscv64g.json` and commit the result.

Interesting inputs are saved to the solutions directory. On hardware an input is interesting when it sets a new best F1 delta; it is logged as `New best measurement delta: <value>` and the delta is in the file name. In simulation an input is interesting when its trace map raises a toggle count never seen before; if it also sets the best toggle activity, the activity is in the file name. Each saved input is also stored as assembly, next to the input as `<name>.S` (a complete program: it does not include `init.inc`). The assembly of the current best input is kept in `best.S`, so on an interrupt (Ctrl-C) the best snippet so far is always on disk.

## Logging

Logs go to `fuzzer.log` (override with `--log-file`); the file is overwritten on every start. Set the filter with `--log-level` (TOML `log_level`), e.g. `--log-level debug` or `--log-level info,libafl=debug`; if unset, `RUST_LOG` is honored, otherwise the level is `info`. LibAFL logs through the `log` crate, and those records are bridged into the same file, so `libafl`/`libafl_bolts` targets can be filtered the same way. With `--log-level debug` the log shows every snippet build, every simulation invocation and in-process VCD analysis (with expanded arguments and exit statuses), and each skipped input with its size and a hex preview.

Every log line carries the process role in brackets after the level, e.g. `2026-11-25T12:34:56.789012Z  INFO [worker-0003] libafl_fuzzer: …`: `[broker]` in parallel mode, `[worker-NNNN]` (0-based worker index) for each forked worker, `[single]` in single-worker mode, and `[supervisor]` for the simulator child process. Since the broker and all workers write to one shared file, the tag is what tells the processes apart — `grep '\[worker-0003\]' fuzzer.log` shows one worker's lines.

## Progress display

`--monitor` (TOML `monitor`) selects how fuzzing progress is shown: `tui` (default; full-screen terminal UI with per-client stats in parallel mode), `print` (plain progress lines on stdout), `log` (a stats line in the log file every 60 s — use this for headless runs), or `none`. In parallel mode the broker aggregates stats from all workers, so `tui` shows a per-client dashboard and `log` writes aggregated lines to the central `--log-file`.

## Saving snippets

`--snippet-dir <DIR>` (TOML `snippet_dir`) additionally saves every generated snippet as `NNNNNN.S`, numbered in execution order, so you can inspect what was built and loaded. The number appears in the debug log (`Saved snippet to …`), which lets you match a crash to its snippet. Snippets are not deleted, so this can use a lot of disk space on long runs.

## Run timeout

LibAFL's executor timeout bounds one complete run (build plus evaluation). `--exec-timeout-ms` (default 900000) sets it; it must be larger than the sum of build time, load time, both acquisitions and `--settle-ms` (simulation: build time plus the simulator deadline; the in-process VCD analysis is a bounded single pass and adds to it). A timeout shows up in the log as `Timeout in fuzz run.`

## Oscilloscope (VISA)

Builds need the NI-VISA library; `.cargo/config.toml` sets `LIB_VISA_PATH` (adjust for your system). Use `--list-scopes` to find the resource string, then `--scope-resource <STRING>` (required).

Arm/acquire commands block until the scope has captured a full acquisition, so they use `--scope-acquire-timeout-ms` (default 30000). It must exceed the acquisition time. All other queries use `--scope-timeout-ms` (default 5000). A missed deadline is reported as a timeout and the scope buffers are cleared.

The idle level is captured (stored in M1, not read back) after the target is reset and halted. The program is then resumed and, after `--settle-ms`, the running level is captured and F1 is read once. F1 is the difference between the running and idle level, and is the score.

## Configuration file

All options can be set in a TOML file passed with `--config` (see `fuzzer.example.toml`). Command-line flags override file values.

## Source layout

`cli.rs` (arguments/config), `feedback.rs` (trace-map coverage feedback with a numeric best score), `harness.rs` (per-input unparse, build and evaluation dispatch), `evaluation.rs` (backend contract), `hardware.rs` (load and idle/run measurement), `simulation.rs` (simulator supervision + in-process VCD analysis), `vcdtoggle/` (integrated VCD toggle engine: metrics + trace map), `supervisor.rs` (simulator child process), `snippet.rs` (write assembly source and make), `measurement.rs`, `openocd.rs`, `scope.rs` (VISA), `parallel.rs` (workspace setup + global best), `isagate.rs` (ISA gate subprocess: stdin/stdout/exit-code contract, timeout, process-group kill), `main.rs` (wiring, Nautilus stages and logging), `grammars/` + `tools/g4_to_nautilus.py` (grammar and converter).

## Simulation backend

Select `--backend simulation` or `backend = "simulation"` in TOML; the default
is hardware. See `fuzzer.simulation.example.toml`. The backend accepts the path to the built simulator,
a simulator working directory, ordered simulator arguments, a target scope, an
optional exclusion file, and the simulator timeout
(`--simulator-timeout-ms`, default 600000 — 10,000,000 cycles takes minutes in
Verilator, so the default leaves room for that).
Relative configured paths resolve from the fuzzer's starting directory, not the
simulator working directory. Arguments are passed directly without a shell:
`{elf}` and `{vcd}` are replaced within each argument by absolute paths. Both
placeholders are required. The user must supply the Chipyard-specific flags,
including the fixed-stop flag. The simulator must produce a usable VCD and either
exit successfully or report the recognized Chipyard cycle-limit termination
described below. No simulator flags are assumed.

The TOML `simulator_arg` is an ordered array. On the command line, repeat
`--simulator-arg=VALUE` for each argument; if any are supplied they replace
the entire TOML array. All other CLI values also override TOML. The simulator
timeout must be positive, and `exec_timeout_ms` must exceed it; allow additional
time for building the ELF and analyzing the VCD.

After the simulator finishes, the fuzzer analyzes the VCD in-process with the
integrated `vcdtoggle` engine (`src/vcdtoggle/`, vendored from the
`vcdtogglemeasure` project): a single pass over the trace for the kept signals
below `--target-scope` (optionally minus `--exclude-signal-file <path>`). Each
run produces a trace map — one saturating toggle count per traced signal — plus
the scalar metrics: `toggle_activity` in signal-level changes per simulated
second (using the last change of a counted signal as the time denominator),
`toggle_coverage`, the fraction of bits of counted bit-vector variables that
changed at least once, and `map_coverage`, the fraction of traced signals that
toggled at least once. Coverages are reported as percentages with two digits
after the decimal point (e.g. `60.00%`). The trace map is the primary feedback:
an input is interesting when it raises any map entry above everything seen
before. `toggle_activity` is the secondary score used for best tracking; both
may be zero for an empty selection or missing timing. Keep the scope,
exclusions and simulation window consistent between runs. Use separate
solutions directories for hardware, simulation and changed simulation
configurations, since their scores are not comparable.

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
An abort may leave a partially flushed VCD: a successful analysis result does
not prove the trace includes every cycle. Validate the trace window for the
specific simulator and workload before comparing scores.

Each evaluation creates a fresh `numeric-fuzzer-vcd-*` trace directory under `vcd_dir`
(`--vcd-dir`, TOML `vcd_dir`) if configured, otherwise inside the simulator working
directory — never the system temp directory, which is often too small for
multi-gigabyte VCDs. The worker creates it before spawning the supervisor and
removes it after the in-process analysis. The simulator
subprocess likewise runs with `TMPDIR` set to the simulator working directory.
Each evaluation uses a separate
supervisor that runs the simulator and monitors a parent-liveness pipe. On a
simulator timeout, or if the fuzzer exits during simulation, it terminates the
active process group and reaps the direct child. The supervisor is
isolated from the fuzzer's terminal process group, so an interrupt to the fuzzer
also closes the pipe. Configure LibAFL's outer timeout above build time plus the
simulator deadline and the analysis time. `make` still has no individual
timeout and is not supervised by this mechanism.

Per-run VCD directories are named `numeric-fuzzer-vcd-<host>-<pid>-*`; at the start
of every evaluation, leftovers of dead same-host processes (e.g. a supervisor
SIGKILLed by SLURM at walltime or a node crash) are removed automatically, while
directories of live processes and of other hosts are never touched.

A machine crash, termination of the supervisor itself, or subprocesses that
deliberately leave their process group can still leave processes or artifacts
behind (until the next evaluation on that host sweeps the stale VCD directory).

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

Parallel workers run via LibAFL's `Launcher` with LLMP shared-memory IPC. The
broker process (the one you start) owns the monitor and aggregates per-client
stats; each forked worker is pinned to a core from the process affinity mask and
runs with its own `LlmpRestartingEventManager`. Workers exchange interesting
inputs instantly over shared memory (no polling delay); each imported input is
re-evaluated by the receiving worker and added to its corpus if its trace map
has coverage the worker has not seen before. If a worker crashes, the restarting
manager respawns it automatically with its serialized state.

Each worker keeps its own LibAFL corpus and local `best.S` under
`<solutions_path>/worker-0000`, `worker-0001`, and so on. The shared
`<solutions_path>/parallel-exchange/global-best.txt` records the full-precision
toggle activity (the score), the toggle coverage and the map coverage (the
fraction of traced signals that toggled at least once) as percentages with two
decimals, the worker, and paths to the corresponding immutable input and
assembly. Global improvements are also printed on stderr. The exchange directory
must not already exist: use a fresh solutions path for each run, especially if
simulation settings change. Snippet archives use the same per-worker
subdirectories. Logging is initialized before forking, so the broker and all
workers share one central log file (`--log-file`); every line is tagged with
its process role (`[broker]`, `[worker-NNNN]`, see *Logging*). Use
`--monitor log` for headless runs to get aggregated stats lines in that file,
or `--monitor tui` for the per-client dashboard.

## SLURM

Use the single-task pattern; see `slurm-fuzzing.example.sbatch`:

`#SBATCH --ntasks=1 --cpus-per-task=16`, then run
`libafl_fuzzer --config ... --workers 16 --worker-workspace $SCRATCH/workspace ...`.

The Launcher reads the process affinity mask set by SLURM from
`--cpus-per-task`, trims it to `--workers`, and forks one worker per core.
The workspace and `solutions_path` must be on local scratch (not `/tmp`).
Use a fresh workspace and solutions path per campaign. The central log is
`--log-file`; the shared global best is
`<solutions_path>/parallel-exchange/global-best.txt`. SLURM captures the
broker's stdout in its output file, so `--monitor print` or `log` both work.
