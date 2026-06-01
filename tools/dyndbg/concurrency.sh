#!/bin/sh
# Test: C-01 (logging + rule toggle concurrency)
set -eu

PROC=/proc/sys/kernel/dynamic_debug
BENCH=/proc/sys/kernel/dyndbg_bench

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
TOGGLE_ITERS=${TOGGLE_ITERS:-5000}
LOG_ITERS=${LOG_ITERS:-50000}
BENCH_MODE=${BENCH_MODE:-count}
RESULTS_DIR=${RESULTS_DIR:-/ext2/results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
CSV_FILE="$RESULTS_DIR/concurrency/results.csv"
DMESG_CHECK=${DMESG_CHECK:-1}

if [ ! -e "$PROC" ] || [ ! -e "$BENCH" ]; then
  echo "dyndbg procfs files not found" >&2
  exit 1
fi

run_rule() {
  echo "$1" > "$PROC"
}

ensure_csv() {
  if [ ! -e "$CSV_FILE" ]; then
    mkdir -p "$(dirname "$CSV_FILE")"
    echo "run_id,case,toggle_iters,log_iters,bench_mode,duration_us,dmesg_status,dmesg_hits,expected_outcome,actual_outcome,status" > "$CSV_FILE"
  fi
}

emit_result() {
  echo "RESULT $*"
}

check_dmesg() {
  DMESG_STATUS=skipped
  DMESG_HITS=0

  if [ "$DMESG_CHECK" -eq 0 ]; then
    return
  fi

  if ! command -v dmesg >/dev/null 2>&1; then
    DMESG_STATUS=unknown
    return
  fi

  hits=$(dmesg 2>/dev/null | grep -Ei 'panic|oops|bug' | wc -l | tr -d ' ')
  if [ -n "$hits" ]; then
    DMESG_HITS=$hits
  fi

  if [ "$DMESG_HITS" -gt 0 ]; then
    DMESG_STATUS=fail
  else
    DMESG_STATUS=pass
  fi
}

toggle_loop() {
  i=0
  while [ "$i" -lt "$TOGGLE_ITERS" ]; do
    run_rule "module=$MODULE_KEY +p"
    run_rule "clear"
    i=$((i + 1))
  done
}

run_rule "clear"
run_rule "module=$MODULE_KEY +p"

toggle_loop &
TOGGLE_PID=$!

echo "mode=$BENCH_MODE iters=$LOG_ITERS" > "$BENCH"
wait "$TOGGLE_PID"
output=$(cat "$BENCH")
echo "$output"
duration=$(echo "$output" | awk -F= '/^last_duration_us=/{print $2}')
if [ -z "$duration" ]; then
  duration=0
fi

check_dmesg

expected_outcome=no_panic
actual_outcome=$DMESG_STATUS
STATUS=pass
if [ "$DMESG_STATUS" = "fail" ]; then
  STATUS=fail
fi

ensure_csv
emit_result "test=C-01 toggle_iters=$TOGGLE_ITERS log_iters=$LOG_ITERS bench_mode=$BENCH_MODE duration_us=$duration dmesg_status=$DMESG_STATUS dmesg_hits=$DMESG_HITS expected_outcome=$expected_outcome actual_outcome=$actual_outcome status=$STATUS run_id=$RUN_ID"
echo "$RUN_ID,C-01,$TOGGLE_ITERS,$LOG_ITERS,$BENCH_MODE,$duration,$DMESG_STATUS,$DMESG_HITS,$expected_outcome,$actual_outcome,$STATUS" >> "$CSV_FILE"

echo "concurrency test finished"
