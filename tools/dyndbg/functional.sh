#!/bin/sh
# Test: F-01..F-07 (selector match, last-match-wins, toggle, invalid input)
set -eu

PROC=/proc/sys/kernel/dynamic_debug
BENCH=/proc/sys/kernel/dyndbg_bench

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
FILE_KEY=${FILE_KEY:-dyndbg_bench.rs}
FUNC_KEY=${FUNC_KEY:-bench_log}
LINE_KEY=${LINE_KEY:-}
ITERS=${ITERS:-100000}
RESULTS_DIR=${RESULTS_DIR:-results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
COMMIT=${COMMIT:-unknown}
PHASE=${PHASE:-unknown}
CSV_FILE="$RESULTS_DIR/functional/results.csv"

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
    echo "run_id,commit,phase,case,status,iters,mode,duration_us,details" > "$CSV_FILE"
  fi
}

emit_result() {
  echo "RESULT $*"
}

record_case() {
  case_id=$1
  status=$2
  details=$3
  details=$(echo "$details" | tr ' ' ';')
  ensure_csv
  emit_result "test=$case_id status=$status iters=$ITERS mode=log duration_us=$LAST_DURATION_US run_id=$RUN_ID commit=$COMMIT phase=$PHASE details=$details"
  echo "$RUN_ID,$COMMIT,$PHASE,$case_id,$status,$ITERS,log,$LAST_DURATION_US,$details" >> "$CSV_FILE"
}

run_bench() {
  output=$(echo "mode=log iters=$ITERS" > "$BENCH"; cat "$BENCH")
  echo "$output"
  LAST_DURATION_US=$(echo "$output" | awk -F= '/^last_duration_us=/{print $2}')
  if [ -z "$LAST_DURATION_US" ]; then
    LAST_DURATION_US=0
  fi
}

# F-01: module selector
run_rule "clear"
run_rule "module=$MODULE_KEY +p"
run_bench
record_case "F-01" "executed" "module=$MODULE_KEY"

# F-02: file selector
run_rule "clear"
run_rule "file=$FILE_KEY +p"
run_bench
record_case "F-02" "executed" "file=$FILE_KEY"

# F-03: function selector
run_rule "clear"
run_rule "func=$FUNC_KEY +p"
run_bench
record_case "F-03" "executed" "func=$FUNC_KEY"

# F-04: line selector (optional)
if [ -n "$LINE_KEY" ]; then
  run_rule "clear"
  run_rule "line=$LINE_KEY +p"
  run_bench
  record_case "F-04" "executed" "line=$LINE_KEY"
fi

# F-05: last-match-wins
run_rule "clear"
run_rule "module=$MODULE_KEY +p"
run_rule "func=$FUNC_KEY -p"
run_bench
record_case "F-05" "executed" "module=$MODULE_KEY func=$FUNC_KEY"

run_rule "clear"
run_rule "func=$FUNC_KEY -p"
run_rule "module=$MODULE_KEY +p"
run_bench
record_case "F-05" "executed" "func=$FUNC_KEY module=$MODULE_KEY"

# F-06: dynamic toggle
run_rule "clear"
run_rule "module=$MODULE_KEY +p"
run_bench
run_rule "clear"
run_bench
record_case "F-06" "executed" "toggle module=$MODULE_KEY"

# F-08: multi-layer rule coverage (optional line)
run_rule "clear"
run_rule "module=$MODULE_KEY +p"
run_rule "file=$FILE_KEY +p"
run_rule "func=$FUNC_KEY -p"
if [ -n "$LINE_KEY" ]; then
  run_rule "line=$LINE_KEY +p"
fi
run_bench
run_rule "clear"
record_case "F-08" "executed" "module=$MODULE_KEY file=$FILE_KEY func=$FUNC_KEY"

# F-07: invalid inputs (best-effort)
echo "++++" > "$PROC" || true
echo "line=abc +p" > "$PROC" || true
echo "func=== +p" > "$PROC" || true
record_case "F-07" "executed" "invalid inputs"

echo "functional test finished"
