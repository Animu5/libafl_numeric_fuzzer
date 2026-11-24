# Remaining Work

## Simulation and Parallel Fuzzing

- Run the complete pipeline with the real decoder and assembly environment:
  decode, build the ELF, simulate with Chipyard, analyze the VCD, and save
  interesting inputs.
- Replace the placeholder limit of 10,000,000 cycles with the count for one
  simulated millisecond. Derive it from the simulator's clock period and verify
  the resulting measurement window, including reset behavior.
- Configure and verify the target scope, signal exclusions, and the simulator
  and outer execution timeouts.
- Test parallel fuzzing with the real simulator, beginning with a small worker
  count and scaling to 200. Check resource usage, throughput, input exchange,
  trace-map coverage growth, global-best reporting, and workspace isolation.
The cycle-limit VCD was validated during analyzer validation.

## External Tool Timeouts

- Add individual timeouts for `atndecode` and `make`; a hang currently blocks
  the run.
- Measure decode and build costs during real fuzzing runs.
