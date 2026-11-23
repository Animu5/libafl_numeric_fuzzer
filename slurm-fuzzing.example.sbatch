#!/bin/bash
# Example SLURM submission for parallel simulation fuzzing with libafl_fuzzer.
# Single node, launcher spawns local workers pinned to the allocated cores:
#   #SBATCH --ntasks=1 --cpus-per-task=16
# The Launcher reads the process affinity mask (set by SLURM from
# --cpus-per-task), trims it to --workers, and forks one LLMP client per core.
#
#SBATCH --job-name=numeric-fuzzer
#SBATCH --ntasks=1
#SBATCH --cpus-per-task=16
#SBATCH --time=24:00:00
#SBATCH --output=fuzzer-%A.out

set -euo pipefail

# Workspace and solutions must live on a filesystem local to the node
# (scratch, not /tmp) and must be fresh for each campaign.
SCRATCH=/path/to/scratch/run-${SLURM_JOB_ID}
WORKSPACE="${SCRATCH}/workspace"
SOLUTIONS="${SCRATCH}/solutions"

libafl_fuzzer --config /path/to/fuzzer.simulation.toml \
  --workers 16 \
  --worker-workspace "${WORKSPACE}" \
  --solutions-path "${SOLUTIONS}" \
  --log-file "${SCRATCH}/fuzzer.log" \
  --monitor log

# The log level is deliberately not set here: use log_level in the TOML or
# RUST_LOG in the environment (RUST_LOG=debug sbatch ...), which this script
# does not override.
# The broker and all workers share one central log file: ${SCRATCH}/fuzzer.log
# Shared global best: ${SOLUTIONS}/parallel-exchange/global-best.txt
# Per-worker solutions: ${SOLUTIONS}/worker-NNNN/
