# Remaining Work

The pipeline (decode, build, load, measure) is implemented but has not been tested end to end on the final hardware.

## End-to-End Test

1. start OpenOCD;
2. start the fuzzer with a config file;
3. check that `atndecode` and `make` produce `snippet.elf`;
4. check that the ELF loads and the scope returns plausible idle and running values;
5. check that new best inputs are saved in the solutions directory.

## Confirm the OpenOCD Configuration

The OpenOCD interface and target configuration files still need to be identified for the final hardware.

## Timeouts for External Tools

`atndecode` and `make` have no timeout, so a hang blocks the run.

## Build Time

Every input runs a decode, a `make -B` and a program load. Measure the cost once real runs exist.

## More Tests

Scope response parsing edge cases and the harness call order are not covered by tests.
