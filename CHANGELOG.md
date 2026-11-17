# Changelog

Notable changes are summarized here. Earlier history is available in Git.

## Unreleased

### Added

- Save the assembly for each new best input and keep `best.S` up to date.
- Define a candidate evaluation backend contract with validated power and toggle
  metrics.
- Add a Unix simulation backend that runs a supplied simulator and analyzes its
  VCD with `vcdtogglerate`, selectable through CLI or TOML.
- Allow simulation-only builds without NI-VISA via `--no-default-features`.
- Supervise simulation subprocesses and temporary traces so they are cleaned up
  if the fuzzer exits during simulation or analysis.
- Support independent parallel simulation workers with per-worker build,
  simulator working, corpus, log, and snippet archive locations.
- Create isolated worker build environments from a template and exchange inputs
  between workers, reporting the global best coverage and its input and assembly.

### Changed

- Move the OpenOCD and oscilloscope workflow behind a hardware evaluation
  backend while retaining the existing hardware measurement sequence.
- Generalize numeric feedback to distinguish power delta from toggle coverage.
- Analyze Chipyard cycle-limit termination by default, while rejecting other
  simulator errors and disabling simulator core dumps.

## 0.9.0

- Build a RISC-V ELF for each input using `atndecode` and `make`, then load it
  through OpenOCD.
- Measure running-minus-idle power with an oscilloscope over VISA and use the
  numeric delta as LibAFL feedback.
- Support TOML and command-line configuration, seed files and directories,
  optional snippet archives, logging, and configurable timeouts.
- Recover from target failures when possible and stop after repeated failures.
