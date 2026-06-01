#!/bin/sh
# Test: C-03 (patch storm with mixed rule operations)
set -eu

PROC=/proc/sys/kernel/dynamic_debug
BENCH=/proc/sys/kernel/dyndbg_bench

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
FILE_KEY=${FILE_KEY:-dyndbg_bench.rs}
FUNC_KEY=${FUNC_KEY:-bench_log}
LINE_KEY=${LINE_KEY:-}
STORM_ITERS=${STORM_ITERS:-2000}
LOG_ITERS=${LOG_ITERS:-50000}
CLEAR_INTERVAL=${CLEAR_INTERVAL:-50}
SLEEP_US=${SLEEP_US:-0}
BENCH_MODE=${BENCH_MODE:-log}
RESULTS_DIR=${RESULTS_DIR:-/ext2/results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
COMMIT=${COMMIT:-unknown}
PHASE=${PHASE:-unknown}
CSV_FILE="$RESULTS_DIR/patch_storm/results.csv"
DMESG_CHECK=${DMESG_CHECK:-1}

if [ ! -e "$PROC" ] || [ ! -e "$BENCH" ]; then
  echo "dyndbg procfs files not found" >&2
  exit 1
fi

run_rule() {
  echo "$1" > "$PROC"
}

read_uptime() {
  awk '{print $1}' /proc/uptime
}

ensure_csv() {
  if [ ! -e "$CSV_FILE" ]; then
    mkdir -p "$(dirname "$CSV_FILE")"
    echo "run_id,case,bench_mode,storm_iters,log_iters,elapsed_s,clear_interval,sleep_us,dmesg_status,dmesg_hits" > "$CSV_FILE"
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

maybe_sleep() {
  if [ "$SLEEP_US" -gt 0 ]; then
    if command -v usleep >/dev/null 2>&1; then
      usleep "$SLEEP_US"
    else
      sleep 0
    fi
  fi
}

storm_loop() {
  key=$1
  i=0
  while [ "$i" -lt "$STORM_ITERS" ]; do
    run_rule "$key +p"
    if [ "$CLEAR_INTERVAL" -gt 0 ] && [ $((i % CLEAR_INTERVAL)) -eq 0 ]; then
      run_rule "clear"
    fi
    maybe_sleep
    i=$((i + 1))
  done
}

storm_neg_loop() {
  key=$1
  i=0
  while [ "$i" -lt "$STORM_ITERS" ]; do
    run_rule "$key -p"
    if [ "$CLEAR_INTERVAL" -gt 0 ] && [ $((i % CLEAR_INTERVAL)) -eq 0 ]; then
      run_rule "clear"
    fi
    maybe_sleep
    i=$((i + 1))
  done
}

start=$(read_uptime)

storm_loop "module=$MODULE_KEY" &
PID1=$!

storm_loop "file=$FILE_KEY" &
PID2=$!

storm_neg_loop "func=$FUNC_KEY" &
PID3=$!

if [ -n "$LINE_KEY" ]; then
  storm_loop "line=$LINE_KEY" &
  PID4=$!
fi

echo "mode=$BENCH_MODE iters=$LOG_ITERS" > "$BENCH" &
PID5=$!

wait "$PID1" "$PID2" "$PID3" 2>/dev/null || true
if [ -n "${PID4:-}" ]; then
  wait "$PID4" 2>/dev/null || true
fi
wait "$PID5" 2>/dev/null || true

run_rule "clear"

end=$(read_uptime)
elapsed=$(awk -v s="$start" -v e="$end" 'BEGIN{printf "%.6f", e-s}')

check_dmesg

ensure_csv
emit_result "test=C-03 bench_mode=$BENCH_MODE storm_iters=$STORM_ITERS log_iters=$LOG_ITERS elapsed_s=$elapsed clear_interval=$CLEAR_INTERVAL sleep_us=$SLEEP_US dmesg_status=$DMESG_STATUS dmesg_hits=$DMESG_HITS run_id=$RUN_ID"
echo "$RUN_ID,C-03,$BENCH_MODE,$STORM_ITERS,$LOG_ITERS,$elapsed,$CLEAR_INTERVAL,$SLEEP_US,$DMESG_STATUS,$DMESG_HITS" >> "$CSV_FILE"

echo "patch storm test finished"
