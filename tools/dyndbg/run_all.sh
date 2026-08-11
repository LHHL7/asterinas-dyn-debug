#!/bin/sh
# Test: run same-build dyndbg cases in guest
#
# This script covers tests that share one build command:
#   make run_kernel LOG_LEVEL=debug SYSCALL_INFO=off RELEASE=1 MEM=16G
#
# Tests requiring other builds (different FEATURES, no RELEASE, SMP=4) must be
# run separately from their own guest sessions.
set -eu

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
ROOT_DIR=$(cd "$SCRIPT_DIR/../.." && pwd)

RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
RESULTS_DIR=${RESULTS_DIR:-$ROOT_DIR/results/$RUN_ID}

if command -v git >/dev/null 2>&1; then
  COMMIT=${COMMIT:-$(git -C "$ROOT_DIR" rev-parse --short HEAD 2>/dev/null || echo unknown)}
else
  COMMIT=${COMMIT:-unknown}
fi

export RESULTS_DIR RUN_ID COMMIT

RUN_FUNCTIONAL=${RUN_FUNCTIONAL:-1}
RUN_INDEX_ABLATION=${RUN_INDEX_ABLATION:-1}
RUN_PERF=${RUN_PERF:-1}
RUN_WORKLOAD=${RUN_WORKLOAD:-1}
RUN_TRACE=${RUN_TRACE:-1}
RUN_FLAGS=${RUN_FLAGS:-1}
RUN_STATUS=${RUN_STATUS:-1}
RUN_MATCH3=${RUN_MATCH3:-1}
RUN_ROBUSTNESS=${RUN_ROBUSTNESS:-1}
RUN_INCREMENTAL=${RUN_INCREMENTAL:-1}

run_script() {
  name=$1
  script=$2
  shift 2
  if [ ! -f "$script" ]; then
    echo "skip $name (missing $script)" >&2
    return
  fi

  echo "==== $name ===="
  "$@" sh "$script"
}

echo "run_id=$RUN_ID results_dir=$RESULTS_DIR"

if [ "$RUN_FUNCTIONAL" -ne 0 ]; then
  run_script "functional" "$SCRIPT_DIR/functional.sh"
fi

if [ "$RUN_INDEX_ABLATION" -ne 0 ]; then
  run_script "index_ablation" "$SCRIPT_DIR/index_ablation.sh"
fi

if [ "$RUN_PERF" -ne 0 ]; then
  run_script "perf" "$SCRIPT_DIR/perf.sh" env BACKEND_MODE=static
fi

if [ "$RUN_WORKLOAD" -ne 0 ]; then
  run_script "workload" "$SCRIPT_DIR/workload.sh" env WORKLOAD_MODE=static
fi

# New-feature suites (T/FL/S/M/R + I-03/EQ): same build group as F.
if [ "$RUN_TRACE" -ne 0 ]; then
  run_script "trace" "$SCRIPT_DIR/trace.sh"
fi

if [ "$RUN_FLAGS" -ne 0 ]; then
  run_script "flags" "$SCRIPT_DIR/flags.sh"
fi

if [ "$RUN_STATUS" -ne 0 ]; then
  run_script "status" "$SCRIPT_DIR/status.sh"
fi

if [ "$RUN_MATCH3" -ne 0 ]; then
  run_script "match3" "$SCRIPT_DIR/match3.sh"
fi

if [ "$RUN_ROBUSTNESS" -ne 0 ]; then
  run_script "robustness" "$SCRIPT_DIR/robustness.sh"
fi

if [ "$RUN_INCREMENTAL" -ne 0 ]; then
  run_script "incremental" "$SCRIPT_DIR/incremental.sh"
fi

echo "all tests finished"
