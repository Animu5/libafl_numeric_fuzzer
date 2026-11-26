# Changelog

Notable changes are summarized here. Earlier history is available in Git.

## Unreleased

### Added

- Process-role tags in the log: every line now carries `[broker]`, `[single]`,
  `[worker-NNNN]` (0-based worker index), or `[supervisor]` after the level
  (e.g. `2026-11-25T12:34:56.789012Z  INFO [worker-0003] libafl_fuzzer: …`), so
  the processes sharing the central log file can be told apart without the pid.
  The tag is a per-process global set at startup (`set_log_tag`) and rendered by
  a custom `FormatEvent` implementation (`TaggedFormat`) in `main.rs`.
- Grammar-based fuzzing (work in progress on the `grammar` branch): the libafl
  `nautilus` feature is enabled so `NautilusInput`/`NautilusGenerator`/
  `NautilusRandomMutator`/`NautilusRecursionMutator` are available.
- The input pipeline no longer uses `atndecode`: inputs are `NautilusInput`
  parse trees generated from a Nautilus JSON grammar (`--grammar`, default
  `grammars/riscv64g.json`, converted from the vendored ANTLR grammar
  `grammars/RISCV64G.g4` by `tools/g4_to_nautilus.py`). The initial corpus is
  generated from the grammar (`--grammar-seeds`, default 64; `--seed` and
  `--atndecode-path` are removed), mutation uses the Nautilus random and
  recursion mutators instead of havoc, and the harness unparses each tree
  into a complete assembly program written as `src/snippet.S` (no `init.inc`
  include). `--grammar-depth` (default 512) must exceed the grammar's minimum
  derivation size (~350 for the bundled grammar).
- ISA gate is now built in: every grammar-generated program is validated and
  fixed in-process by the new `src/isagate/` module before it is built —
  `machine.rs` is a Rust port of the Go `RISCV64GMachine` ISA model
  (`atnwalk/grammars/emulator`), `gate.rs` classifies each program line and
  replays it on the machine, exactly like `atnwalk/cmd/isagate` in the
  asltgen-afl-proxy project did (memory offsets clamped into the valid range
  and realigned, `li xN, <valid address>` inserted when the base register
  cannot hold one, branches emitted as absolute target PCs, immediates wrapped
  to their field widths, including the `li` two-halves `Decode(2^32-1)`
  quirk). Unfixable programs are rejected and skipped (counted in
  `Shared::isa_gate_rejections`, logged at debug). The fixed program is what
  gets written to `src/snippet.S`, built, measured, and persisted as the
  global best. The external `--isa-gate`/`--isa-gate-timeout-ms` options and
  the stdin/stdout gate subprocess (`supervisor::run_process_with_stdin`) are
  removed, so no gate binary needs to be built or shipped.
- Branch labels: the ISA gate now emits a `.Lbr{j}:` label before every
  statement and rewrites each branch operand to `.Lbr{imm mod n}` (`imm` = the
  branch immediate carried by the Nautilus parse tree, `n` = statement count),
  so the fuzz input directly picks a forward or backward target — including
  loops back to `_start:` and self-loops. gas resolves labels against the real
  layout, which matters with compressed (RVC) instructions: the previous
  absolute-target-PC emission assumed 4-byte instructions and could land
  mid-instruction and trap. Statements skipped by a branch never execute,
  which is fine — the goal is many diverse snippets running without an
  exception. Deliberate divergence from the Go gate, which emitted the
  absolute target PC as a decimal operand.

### Fixed

- Memory operands in the grammar now carry an explicit offset:
  `grammars/RISCV64G.g4` emits `0(xN)` instead of `(xN)` for loads/stores
  (per the RISC-V specification a missing offset means 0), and
  `grammars/riscv64g.json` was regenerated accordingly. Previously the
  offset hook mapped to an empty string, the ISA gate rejected every
  `lw x2,(x22)`-style line ("bad offset"), and memory instructions never
  reached the target. The gate also accepts a missing offset as 0 now, so
  spec-valid assembly is no longer rejected.
- Generated programs now have multi-statement bodies instead of
  single-instruction loops. Two causes fixed: (1) the g4->Nautilus converter
  now emits the recursive alternative of `(...)*`/`(...)+` groups
  `RECURSION_BIAS` (8) times, so Nautilus recurses with 8/9 probability per
  level instead of stopping at 50%; (2) the default `--grammar-depth` rises
  from 512 to 16384 — Nautilus splits the depth budget fairly among a rule's
  sibling nonterminals, and the statement list is one of ~66 siblings of the
  start rule, so at 512 it got a budget of ~8 rule applications (one
  statement). Measured on 16 seeded generations: ~9 body statements on
  average (was ~1.25).

## 0.10.1

### Fixed

- Broker crash in parallel mode with `--monitor print`: ll_mp's broker timeout
  hook calls `monitor.display(..., ClientId(0))` (the broker's own id, absent
  from the per-client stats map), `SimplePrintingMonitor` propagates the
  resulting `KeyNotFound`, and ll_mp `.expect()`s the hook result — killing the
  broker and then every worker ("receiver/broker could not process our sent
  llmp messages"). `DynMonitor::display` now logs display errors at debug level
  and always returns `Ok`, so no monitor can take the broker down.

## 0.10.0

### Changed

- The `vcdtogglemeasure` project (`vcdtogglerate`) is integrated as the
  `src/vcdtoggle/` module instead of being run as an external analyzer binary:
  the VCD is analyzed in-process after the simulator finishes, so no
  `--analyzer-path`/`--analyzer-timeout-ms` are needed anymore (both removed;
  `exec_timeout_ms` now only has to exceed the simulator timeout plus build
  time). New dependencies: `wellen`, `memchr`, `memmap2`. The simulation
  supervisor child process now runs only the simulator; the parent worker owns
  the per-run VCD directory and analyzes the trace itself.
- Feedback reworked for the simulation backend: the analysis now produces a
  trace map (one saturating toggle count per traced signal) which is the
  primary feedback — an input is interesting when it raises any map entry above
  the history. Toggle activity (changes per simulated second) is the secondary
  score (`Metric::ToggleActivity`, replacing `Metric::ToggleCoverage`) used for
  `best.S` and `parallel-exchange/global-best.txt` (which now records
  `toggle_activity=` first, plus `toggle_coverage=`). The hardware backend is
  unchanged and still uses the power delta as score.
- The global-best record (`parallel-exchange/global-best.txt`) now also reports
  `map_coverage`, the fraction of trace-map entries (traced signals) that
  toggled at least once, and all coverages (`toggle_coverage`, `map_coverage`)
  are reported as percentages with two digits after the decimal point
  (e.g. `60.00%`) in the record and in log lines.
- Documentation and cleanup pass (no behavior change): every module now has a
  `//!` header and all public items are documented (including the vendored
  `src/vcdtoggle/` code); the supervisor's two bounded-output readers were
  unified into one `read_bounded(reader, keep_tail)`; the integration tests
  share an `executable` script helper via `tests/common/mod.rs`.
- `TODO.md` removed (all items are done) and the README no longer references it.
- README/RUNNING refreshed against the code: feedback description now covers the
  trace map, the file lists include `parallel.rs`/`supervisor.rs`, stale
  "not yet tested on final hardware" notes removed, the executor-timeout default
  corrected to 900000 ms, and the fuzzing-behaviour section describes what gets
  saved for both backends.

- Source files consolidated: `logging.rs` and `monitor.rs` merged into
  `main.rs`, `global_best.rs` merged into `parallel.rs`, and `simulation.rs`
  split into `simulation.rs` (backend) + `supervisor.rs` (supervisor process
  mechanism). 15 source files reduced to 13; the largest file dropped from
  1157 to 897 lines.
- Replace the custom parallel launcher (manual process spawning, stdin liveness
  pipe, process-group kill) with LibAFL's `Launcher` + LLMP shared-memory IPC.
  The broker process owns the monitor and aggregates per-client stats; each
  forked worker is pinned to a core from the process affinity mask (respects
  SLURM `--cpus-per-task`) and runs with its own `LlmpRestartingEventManager`.
  Logging is initialized before forking so the broker and all workers share
  one central log file. The direct `libafl_core` dependency was removed
  (`ClientId` comes from `libafl_bolts`); edition bumped to 2024.
- The crate is now explicitly Unix-only: a `compile_error!` on non-Unix replaces
  the 31 `#[cfg(unix)]`/`#[cfg(not(unix))]` gates that previously allowed a
  hardware-only build on other platforms.
- Filesystem input exchange between workers (1-second polling, `flock`-protected
  publish, seen-set dedup) is removed. The LLMP event manager inside `fuzz_one`
  now exchanges `NewTestcase` events between workers via shared memory, with no
  polling delay. `parallel-exchange/` now only holds `global-best.txt` and the
  best input/assembly files for persistence.
- `src/parallel.rs` now holds workspace setup (`prepare_workspace`) plus the
  global-best tracking (`prepare_exchange`, `update_global_best`); the custom
  process launcher, stdin liveness watch, and process-group kill are gone.
- `select_worker` no longer appends a `.worker-NNNN` suffix to the log file:
  logging is initialized once in the broker before forking, so the mutation
  was dead code and all workers share the one central log file.
- `--worker-index`, `--internal-worker-index`, and `--internal-launcher-child`
  are removed; the Launcher assigns worker IDs via `ClientDescription`.
- `slurm-fuzzing.example.sbatch` now uses the single-task pattern
  (`--ntasks=1 --cpus-per-task=N`) instead of a job array; the Launcher reads
  the SLURM affinity mask and pins workers to the allocated cores.
- README, RUNNING.md, and `fuzzer.simulation.example.toml` updated for the
  Launcher+LLMP single-node model.
- Integration tests rewritten for the LLMP Launcher (the old `--worker-index`
  SLURM-array and launcher-death tests are gone). The broker port can be set
  via the `LLMP_BROKER_PORT` env var (default 1337), which tests use to avoid
  port conflicts.

### Fixed

- Defuse a flaky launcher integration test: it now waits for all worker
  artifacts (both `best.S` files and both built ELFs) instead of stopping at
  the first global-best update, which worker-0001 could reach before
  worker-0000 had written its own best.

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
