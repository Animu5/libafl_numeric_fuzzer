# Changelog

Notable changes are summarized here. Earlier history is available in Git.

## Unreleased

### Added

- Save the assembly for each new best input and keep `best.S` up to date.
- Define a candidate evaluation backend contract with validated power and toggle
  metrics.

### Changed

- Move the OpenOCD and oscilloscope workflow behind a hardware evaluation
  backend while retaining the existing hardware measurement sequence.
- Generalize numeric feedback to distinguish power delta from toggle coverage.
  The simulation backend is not implemented yet.

## 0.9.0

- Build a RISC-V ELF for each input using `atndecode` and `make`, then load it
  through OpenOCD.
- Measure running-minus-idle power with an oscilloscope over VISA and use the
  numeric delta as LibAFL feedback.
- Support TOML and command-line configuration, seed files and directories,
  optional snippet archives, logging, and configurable timeouts.
- Recover from target failures when possible and stop after repeated failures.
