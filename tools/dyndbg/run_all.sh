#!/bin/sh
# Test: run all dyndbg cases in guest
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
RUN_INCREMENTAL=${RUN_INCREMENTAL:-1}
RUN_PERF=${RUN_PERF:-1}
RUN_WORKLOAD=${RUN_WORKLOAD:-1}
RUN_PATCH_BENCH=${RUN_PATCH_BENCH:-1}
RUN_SCALE=${RUN_SCALE:-1}
RUN_CONCURRENCY=${RUN_CONCURRENCY:-1}
RUN_STRESS=${RUN_STRESS:-1}
RUN_PATCH_STORM=${RUN_PATCH_STORM:-1}

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

if [ "$RUN_INCREMENTAL" -ne 0 ]; then
  run_script "incremental" "$SCRIPT_DIR/incremental.sh" env
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

if [ "$RUN_SCALE" -ne 0 ]; then
  run_script "scale" "$SCRIPT_DIR/scale.sh"
fi

if [ "$RUN_CONCURRENCY" -ne 0 ]; then
  run_script "concurrency" "$SCRIPT_DIR/concurrency.sh"
fi

if [ "$RUN_STRESS" -ne 0 ]; then
  run_script "stress" "$SCRIPT_DIR/stress.sh"
fi

if [ "$RUN_PATCH_STORM" -ne 0 ]; then
  run_script "patch_storm" "$SCRIPT_DIR/patch_storm.sh"
fi

echo "all tests finished"
