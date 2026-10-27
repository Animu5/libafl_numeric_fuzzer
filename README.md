# LibAFL Numeric Fuzzer with OpenOCD Integration

## Project Overview

This project is a prototype LibAFL-based fuzzer that uses a numeric measurement instead of traditional code coverage as feedback.

The intended use case is to generate or mutate inputs for a RISC-V target and maximize a measured value such as power consumption.

The program communicates with the target through the OpenOCD Tcl interface.

## Current Implementation

For every fuzz input the fuzzer:

1. pipes the bitstream into `atndecode`, which produces RISC-V assembly (`<assembly-environment>/src/snippet.S`);
2. runs `make` in the assembly environment (with `RISCV` set to `--riscv-toolpath`) to build `snippet.elf`;
3. resets the target through OpenOCD and loads the ELF;
4. measures the idle level on the oscilloscope (VISA) while the target is halted;
5. resumes the target (the programs are infinite loops), waits `--settle-ms` and measures the running level;
6. scores the input by `running - idle` and treats it as interesting if it is a new best.

Inputs that cannot be decoded or built are logged and skipped. If loading or resuming a program fails (e.g. it crashed the target), the target is reset with `reset halt` and the input is skipped. The run aborts if that reset fails (e.g. OpenOCD died), after 10 consecutive target failures, or after 5 consecutive measurement failures.

The corpus starts with a seed (`--seed` file or directory of files, default 16 zero bytes) that is mutated with LibAFL's havoc mutators. Inputs with a new best delta are saved to the solutions directory with the delta in the file name.

## Project Files

* `src/main.rs`: wiring of config, OpenOCD, scope and LibAFL;
* `src/cli.rs`: arguments and TOML config;
* `src/harness.rs`: per-input build, load and idle/run measurement;
* `src/snippet.rs`: `atndecode` and `make` steps;
* `src/feedback.rs`: numeric feedback;
* `src/measurement.rs`, `src/scope.rs`: scope commands and VISA connection;
* `src/openocd.rs`: OpenOCD Tcl client;
* `src/logging.rs`: file logging;
* `fuzzer.example.toml`, `RUNNING.md` (setup and execution), `TODO.md` (remaining work).

## Current Limitations

The full setup has not yet been tested end to end on the final hardware, and the OpenOCD interface and target configuration still need to be confirmed. See `TODO.md`.
