#!/bin/sh
# Test: C-02 (high-frequency rule toggle stress)
set -eu

PROC=/proc/sys/kernel/dynamic_debug

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
TOGGLE_ITERS=${TOGGLE_ITERS:-100000}
RESULTS_DIR=${RESULTS_DIR:-results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
COMMIT=${COMMIT:-unknown}
PHASE=${PHASE:-unknown}
CSV_FILE="$RESULTS_DIR/stress/results.csv"
DMESG_CHECK=${DMESG_CHECK:-1}

if [ ! -e "$PROC" ]; then
  echo "dyndbg procfs files not found" >&2
  exit 1
fi

run_rule() {
  echo "$1" > "$PROC"
}

ensure_csv() {
  if [ ! -e "$CSV_FILE" ]; then
    mkdir -p "$(dirname "$CSV_FILE")"
    echo "run_id,commit,phase,case,toggle_iters,elapsed_s,dmesg_status,dmesg_hits" > "$CSV_FILE"
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

start=$(awk '{print $1}' /proc/uptime)

i=0
while [ "$i" -lt "$TOGGLE_ITERS" ]; do
  run_rule "module=$MODULE_KEY +p"
  run_rule "clear"
  i=$((i + 1))
  done

end=$(awk '{print $1}' /proc/uptime)

elapsed=$(awk -v s="$start" -v e="$end" 'BEGIN{printf "%.6f", e-s}')

echo "elapsed_s=$elapsed"

check_dmesg

ensure_csv
emit_result "test=C-02 toggle_iters=$TOGGLE_ITERS elapsed_s=$elapsed dmesg_status=$DMESG_STATUS dmesg_hits=$DMESG_HITS run_id=$RUN_ID commit=$COMMIT phase=$PHASE"
echo "$RUN_ID,$COMMIT,$PHASE,C-02,$TOGGLE_ITERS,$elapsed,$DMESG_STATUS,$DMESG_HITS" >> "$CSV_FILE"

echo "stress test finished"
