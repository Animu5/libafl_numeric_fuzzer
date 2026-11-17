# LibAFL Numeric Fuzzer with OpenOCD Integration

## Project Overview

This project is a prototype LibAFL-based fuzzer that uses a numeric measurement instead of traditional code coverage as feedback.

The intended use case is to generate or mutate inputs for a RISC-V target and maximize a measured value such as power consumption.

The default hardware backend communicates with the target through OpenOCD. A
simulation backend runs a Chipyard-built Verilator simulator and scores VCD toggle
coverage using `vcdtogglerate`.

## Current Implementation

For every fuzz input, both backends first build an ELF:

1. pipes the bitstream into `atndecode`, which produces RISC-V assembly (`<assembly-environment>/src/snippet.S`);
2. runs `make` in the assembly environment (with `RISCV` set to `--riscv-toolpath`) to build `snippet.elf`;
For the default hardware backend it then:

1. resets the target through OpenOCD and loads the ELF;
2. captures the idle level on the oscilloscope (VISA) into memory M1 while the target is halted;
3. resumes the target (the programs are infinite loops), waits `--settle-ms`, captures the running level and reads F1 (running minus idle, computed by the scope);
4. scores the input by that delta and treats it as interesting if it is a new best.

The simulation backend instead runs the ELF in the supplied simulator, analyzes
its VCD, and maximizes toggle coverage while logging toggle activity. See
`fuzzer.simulation.example.toml` and `RUNNING.md` for configuration.
Parallel simulation workers can automatically copy a build template into isolated
workspaces. Workers exchange locally interesting inputs and maintain separate
corpora alongside a shared global best; see `RUNNING.md`.

Inputs that cannot be decoded or built are logged and skipped. If loading or resuming a program fails (e.g. it crashed the target), the target is reset with `reset halt` and the input is skipped. The run aborts if that reset fails (e.g. OpenOCD died), after 10 consecutive target failures, or after 5 consecutive measurement failures.

The corpus starts with a seed (`--seed` file or directory of files, default 16 zero bytes) that is mutated with LibAFL's havoc mutators. Inputs with a new best delta are saved to the solutions directory with the delta in the file name. Every saved input is accompanied by its decoded assembly as `<name>.S`, and the assembly of the current best input is kept up to date as `best.S`, so the best snippet survives an interrupt (Ctrl-C).

## Project Files

* `src/main.rs`: wiring of config, OpenOCD, scope and LibAFL;
* `src/cli.rs`: arguments and TOML config;
* `src/harness.rs`: per-input build and evaluation dispatch;
* `src/evaluation.rs`, `src/hardware.rs`, `src/simulation.rs`: backend contract,
  hardware execution and the simulation backend;
* `src/snippet.rs`: `atndecode` and `make` steps;
* `src/feedback.rs`: numeric feedback;
* `src/measurement.rs`, `src/scope.rs`: scope commands and VISA connection;
* `src/openocd.rs`: OpenOCD Tcl client;
* `src/logging.rs`: file logging;
* `fuzzer.example.toml`, `fuzzer.simulation.example.toml`, `RUNNING.md` (setup and execution), `TODO.md` (remaining work), `CHANGELOG.md` (notable changes).

## Current Limitations

The full setup has not yet been tested end to end on the final hardware, and the OpenOCD interface and target configuration still need to be confirmed. See `TODO.md`.

The simulation score is toggle coverage, a fraction rather than watts.
`vcdtogglerate` may report zero when no counted bits or usable change time exist,
so zero alone does not establish that the intended signals were measured.

## Attribution

GitHub Copilot assisted with the evaluation backend contract and hardware
backend refactoring in this repository.
