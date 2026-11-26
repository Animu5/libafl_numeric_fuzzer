# LibAFL Numeric Fuzzer with OpenOCD Integration

## Project Overview

This project is a prototype LibAFL-based fuzzer. The hardware backend uses a numeric measurement (power delta) as feedback; the simulation backend uses the VCD trace map as coverage-style feedback plus a numeric score.

The intended use case is to generate or mutate inputs for a RISC-V target and maximize a measured value such as power consumption.

The default hardware backend communicates with the target through OpenOCD. A
simulation backend runs a Chipyard-built Verilator simulator and analyzes the
resulting VCD in-process with the integrated `vcdtogglerate` engine
(`src/vcdtoggle/`): each run produces a trace map (one saturating toggle count
per traced signal) that drives coverage feedback, with toggle activity as the
secondary score.

## Current Implementation

For every fuzz input, both backends first build an ELF:

1. pipes the bitstream into `atndecode`, which produces RISC-V assembly;
2. validates and fixes the decoded program with the built-in ISA gate (`src/isagate/`, a Rust port of `atnwalk/cmd/isagate` from the asltgen-afl-proxy project): the gate replays the program line by line on its ISA model, fixes what would crash (misaligned/out-of-map memory accesses get a clamped offset or an inserted `li` with a valid address; immediates are wrapped to their field widths), and labels every statement `.Lbr{j}:` so branches can target any statement — the branch operand becomes `.Lbr{imm mod n}`, i.e. the decoded immediate picks a forward or backward target, resolved by gas against the real (compressed-instruction) layout so targets always land on an instruction boundary; unfixable programs are rejected and skipped;
3. writes the fixed program as `<assembly-environment>/src/snippet.S`, preceded by the required `#include "init.inc"` (machine init; the linker enters at `_init` and falls through into the snippet);
4. runs `make` in the assembly environment (with `RISCV` set to `--riscv-toolpath`) to build `snippet.elf`;
For the default hardware backend it then:

1. resets the target through OpenOCD and loads the ELF;
2. captures the idle level on the oscilloscope (VISA) into memory M1 while the target is halted;
3. resumes the target (the programs are infinite loops), waits `--settle-ms`, captures the running level and reads F1 (running minus idle, computed by the scope);
4. scores the input by that delta and treats it as interesting if it is a new best.

The simulation backend instead runs the ELF in the supplied simulator and
analyzes its VCD in-process: an input is interesting when its trace map covers
a signal (or toggle count) never seen before; the toggle activity per simulated
cycle (rising clock edges counted in the VCD) times `--target-clock-hz`
(default 50 MHz) is the secondary score used for best tracking, i.e. the
expected changes per wall-clock second on the real chip. See
`fuzzer.simulation.example.toml` and `RUNNING.md` for configuration.
Parallel simulation workers run on a single node via LibAFL's `Launcher` with LLMP
shared-memory IPC: workers are forked and pinned to cores, exchange interesting
inputs instantly over shared memory, and are restarted automatically on crash.
A build template is copied into isolated per-worker workspaces; each worker keeps
its own corpus alongside a shared global best; see `RUNNING.md`.

Inputs that cannot be decoded or built are logged and skipped. If loading or resuming a program fails (e.g. it crashed the target), the target is reset with `reset halt` and the input is skipped. The run aborts if that reset fails (e.g. OpenOCD died), after 10 consecutive target failures, or after 5 consecutive measurement failures.

The corpus starts with a seed (`--seed` file or directory of files, default 16 zero bytes) that is mutated with LibAFL's havoc mutators. Interesting inputs are saved to the solutions directory: on hardware an input is interesting when it sets a new best power delta (the delta is in the file name); in simulation an input is interesting when its trace map covers something new (the toggle activity is in the file name when it is also the best). Every saved input is accompanied by its decoded assembly as `<name>.S`, and the assembly of the current best input is kept up to date as `best.S`, so the best snippet survives an interrupt (Ctrl-C).

## Project Files

* `src/main.rs`: wiring of config, OpenOCD, scope and LibAFL;
* `src/cli.rs`: arguments and TOML config;
* `src/harness.rs`: per-input build and evaluation dispatch;
* `src/evaluation.rs`, `src/hardware.rs`, `src/simulation.rs`: backend contract,
  hardware execution and the simulation backend;
* `src/vcdtoggle/`: integrated VCD toggle analysis (vendored from the
  `vcdtogglemeasure` project) producing metrics and the trace map;
* `src/snippet.rs`: `atndecode`, ISA gate and `make` steps;
* `src/isagate/`: built-in ISA gate (`machine.rs` is the ISA model, `gate.rs`
  classifies and replays each program line and places the branch labels);
* `src/feedback.rs`: trace-map coverage feedback with a numeric best score;
* `src/measurement.rs`, `src/scope.rs`: scope commands and VISA connection;
* `src/openocd.rs`: OpenOCD Tcl client;
* `src/parallel.rs`: worker workspace setup and global-best tracking;
* `src/supervisor.rs`: the supervised simulator child process;
* `fuzzer.example.toml`, `fuzzer.simulation.example.toml`, `slurm-fuzzing.example.sbatch`, `RUNNING.md` (setup and execution), `CHANGELOG.md` (notable changes).

## Current Limitations

The simulation score is toggle activity (changes per simulated cycle times
the target clock frequency), not watts. The integrated analyzer may report zero
activity when no counted signal changed, the trace carries no timestamps, or
the clock never rose, so zero alone does not establish that the intended
signals were measured; the trace map tells apart "no signal toggled" from
"the same signals toggled again".

## Attribution

GitHub Copilot assisted with the evaluation backend contract and hardware
backend refactoring in this repository.
