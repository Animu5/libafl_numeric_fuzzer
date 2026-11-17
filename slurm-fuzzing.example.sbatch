#!/bin/bash
# Example SLURM submission for parallel simulation fuzzing with libafl_fuzzer.
# Pattern 2 (job array, one fuzzer worker per task) is shown; for pattern 1
# (single job, launcher spawns local workers) use instead:
#   #SBATCH --ntasks=1 --cpus-per-task=16
# and drop --worker-index "$SLURM_ARRAY_TASK_ID" from the command below.
#
#SBATCH --job-name=numeric-fuzzer
#SBATCH --array=0-15
#SBATCH --ntasks=1
#SBATCH --cpus-per-task=1
#SBATCH --time=24:00:00
#SBATCH --output=fuzzer-%A_%a.out

set -euo pipefail

# Workspace and solutions must live on a filesystem shared by all array tasks
# (scratch/NFS, not /tmp) and must be fresh for each campaign.
SCRATCH=/path/to/scratch/run-${SLURM_JOB_ID}
WORKSPACE="${SCRATCH}/workspace"
SOLUTIONS="${SCRATCH}/solutions"

libafl_fuzzer --config /path/to/fuzzer.simulation.toml \
  --workers 16 \
  --worker-workspace "${WORKSPACE}" \
  --worker-index "${SLURM_ARRAY_TASK_ID}" \
  --solutions-path "${SOLUTIONS}" \
  --log-file "${SCRATCH}/fuzzer.log" \
  --monitor log

# The log level is deliberately not set here: use log_level in the TOML or
# RUST_LOG in the environment (RUST_LOG=debug sbatch ...), which this script
# does not override.
# Per-task logs, including the --monitor log progress lines (every 60 s):
#   tail -f ${SCRATCH}/fuzzer.log.worker-*
# Shared global best: ${SOLUTIONS}/parallel-exchange/global-best.txt
