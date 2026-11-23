# Changelog

Notable changes are summarized here. Earlier history is available in Git.

## Unreleased

### Changed

- Replace the custom parallel launcher (manual process spawning, stdin liveness
  pipe, process-group kill) with LibAFL's `Launcher` + LLMP shared-memory IPC.
  The broker process owns the monitor and aggregates per-client stats; each
  forked worker is pinned to a core from the process affinity mask (respects
  SLURM `--cpus-per-task`) and runs with its own `LlmpRestartingEventManager`.
  Logging is initialized before forking so the broker and all workers share
  one central log file. The direct `libafl_core` dependency was removed
  (`ClientId` comes from `libafl_bolts`); edition bumped to 2024.

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
- Set the log filter with `--log-level`/`log_level` (overrides `RUST_LOG`); LibAFL's
  `log` records are bridged into the log file so its targets can be filtered too.
- Select the progress monitor with `--monitor`/`monitor`: `tui` (default), `print`
  (stdout lines), `log` (periodic stats lines in the log file, for headless runs),
  or `none`.
- Add `--worker-index N` to run a single parallel worker as its own process (e.g.
  `--worker-index "$SLURM_ARRAY_TASK_ID"` in a SLURM job array); each such task
  prepares its own `--worker-workspace` directories and the input-exchange directory
  is created idempotently at startup. See `slurm-fuzzing.example.sbatch` and
  RUNNING.md. Launcher-spawned workers are unchanged and keep the liveness watch.

### Changed

- Run the decoder and `make` with `TMPDIR` set to the assembly environment, and the
  simulator and analyzer with `TMPDIR` set to the simulator working directory, so no
  toolchain or subprocess intermediate files are written to the system temp directory.
  The assembly environment is absolutized first: a relative `TMPDIR` is unresolvable
  from the child's working directory and the toolchain silently falls back to `/tmp`.
- Create per-run VCD trace directories under `vcd_dir`/`--vcd-dir` (default: the
  simulator working directory) instead of the system temp directory, which is often
  too small for multi-gigabyte traces. Directories are named
  `numeric-fuzzer-vcd-<host>-<pid>-*` and leftovers of dead same-host processes
  (e.g. after a SIGKILL) are swept at the start of each evaluation.
- Raise the default simulation timeouts to fit multi-minute 10M-cycle Verilator
  runs: simulator 60 s to 10 min, analyzer 30 s to 2 min, executor 2 min to 15 min;
  all remain configurable and the executor default must still exceed their sum.
- Make skipped-input warnings diagnostic: they now include the input length and a
  hex preview, the failing build step with its paths, and explicit errors for a
  missing assembly environment, `src/` directory, or decoder binary; simulation
  runs log the full simulator and analyzer command lines, including in the
  supervisor's captured stderr.
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
