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
RUN_PATCH_BENCH=${RUN_PATCH_BENCH:-1}

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
  run_script "perf" "$SCRIPT_DIR/perf.sh" env BACKEND_MODE=disabled
fi

if [ "$RUN_WORKLOAD" -ne 0 ]; then
  run_script "workload" "$SCRIPT_DIR/workload.sh" env WORKLOAD_MODE=disabled
fi

if [ "$RUN_PATCH_BENCH" -ne 0 ]; then
  run_script "patch_bench" "$SCRIPT_DIR/patch_bench.sh"
fi

echo "all tests finished"
