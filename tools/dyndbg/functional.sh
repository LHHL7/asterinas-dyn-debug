#!/bin/sh
# Test: F-01..F-07 (selector match, last-match-wins, toggle, invalid input)
set -eu

PROC=/proc/sys/kernel/dynamic_debug
BENCH=/proc/sys/kernel/dyndbg_bench
BENCH_ENABLED_THRESHOLD_US=${BENCH_ENABLED_THRESHOLD_US:-}
BENCH_ENABLED_MARGIN_US=${BENCH_ENABLED_MARGIN_US:-1000}

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
FILE_KEY=${FILE_KEY:-dyndbg_bench.rs}
# Three-channel engine: func is atomic exact on the SHORT function name
# (descriptor stores it without the module path). Partial names need the
# wildcard channel (func=*bench_lo*).
FUNC_KEY=${FUNC_KEY:-bench_log}
ITERS=${ITERS:-100}
LINE_KEY=${LINE_KEY:-}
LINE_KEY_FILE=${LINE_KEY_FILE:-/etc/dyndbg_line.txt}
LINE_FALLBACK_KEY=${LINE_FALLBACK_KEY:-196}
RESULTS_DIR=${RESULTS_DIR:-/ext2/results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
CSV_FILE="$RESULTS_DIR/functional/results.csv"
CSV_HEADER='run_id,case,status,expected,actual,iters,mode,duration_us,details'

if [ ! -e "$PROC" ] || [ ! -e "$BENCH" ]; then
  echo "dyndbg procfs files not found" >&2
  exit 1
fi

run_rule() {
  echo "$1" > "$PROC"
}

ensure_csv() {
  mkdir -p "$(dirname "$CSV_FILE")"

  if [ ! -e "$CSV_FILE" ]; then
    echo "$CSV_HEADER" > "$CSV_FILE"
    return
  fi

  if [ "$(head -n 1 "$CSV_FILE")" != "$CSV_HEADER" ]; then
    mv "$CSV_FILE" "$CSV_FILE.legacy.$RUN_ID" 2>/dev/null || rm -f "$CSV_FILE"
    echo "$CSV_HEADER" > "$CSV_FILE"
  fi
}

emit_result() {
  echo "RESULT $*"
}

record_case() {
  case_id=$1
  status=$2
  expected=$3
  actual=$4
  details=$5
  details=$(echo "$details" | tr ' ' ';')
  ensure_csv
  emit_result "test=$case_id status=$status expected=$expected actual=$actual iters=$ITERS mode=log duration_us=$LAST_DURATION_US run_id=$RUN_ID details=$details"
  echo "$RUN_ID,$case_id,$status,$expected,$actual,$ITERS,log,$LAST_DURATION_US,$details" >> "$CSV_FILE"
}

run_bench() {
  output=$(echo "mode=log iters=$ITERS" > "$BENCH"; cat "$BENCH")
  echo "$output"
  LAST_DURATION_US=$(echo "$output" | awk -F= '/^last_duration_us=/{print $2}')
  if [ -z "$LAST_DURATION_US" ]; then
    LAST_DURATION_US=0
  fi
}

# log_batch runs all bench_sites functions — needed for assertions that
# target a bench_sites site (e.g. line= targets bench_log_0 via /etc line key).
run_bench_batch() {
  output=$(echo "mode=log_batch iters=$ITERS" > "$BENCH"; cat "$BENCH")
  echo "$output"
  LAST_DURATION_US=$(echo "$output" | awk -F= '/^last_duration_us=/{print $2}')
  if [ -z "$LAST_DURATION_US" ]; then
    LAST_DURATION_US=0
  fi
}

get_rule_count() {
  cat "$PROC" | awk -F'[= ]' '/^rules=/{print $2; exit}'
}

measure_disabled_baseline() {
  run_rule "clear"
  run_bench
  DISABLED_BASE_US=$LAST_DURATION_US
  run_bench_batch
  DISABLED_BATCH_BASE_US=$LAST_DURATION_US
}

detect_line_key() {
  if [ -n "$LINE_KEY" ]; then
    echo "$LINE_KEY"
    return
  fi

  # Prefer the live line of bench_log_0 from the status listing: the site
  # lives in the generated macro expansion (bench_sites.rs:34 today), so a
  # hardcoded line drifts whenever that file changes.
  line=$(cat "$PROC" 2>/dev/null | grep ' bench_log_0 ' | head -n 1 |
    sed -n 's/.*:\([0-9][0-9]*\) \[.*/\1/p')
  case "$line" in
    ''|*[!0-9]*) ;;
    *)
      echo "$line"
      return
      ;;
  esac

  if [ -r "$LINE_KEY_FILE" ]; then
    line=$(awk 'NR == 1 { gsub(/[[:space:]]+$/, ""); print; exit }' "$LINE_KEY_FILE" 2>/dev/null || true)
    case "$line" in
      ''|*[!0-9]*) ;;
      *)
        echo "$line"
        return
        ;;
    esac
  fi
  if [ -n "$LINE_FALLBACK_KEY" ]; then
    echo "$LINE_FALLBACK_KEY"
    return
  fi

  echo "failed to detect line key from $LINE_KEY_FILE" >&2
  return 1
}

enabled_threshold_us() {
  if [ -n "$BENCH_ENABLED_THRESHOLD_US" ]; then
    echo "$BENCH_ENABLED_THRESHOLD_US"
    return
  fi

  # Use an adaptive threshold so quick runs (e.g. ITERS=10/20) are not misclassified.
  echo $((DISABLED_BASE_US + BENCH_ENABLED_MARGIN_US))
}

assert_enabled() {
  threshold_us=$(enabled_threshold_us)
  if [ "$LAST_DURATION_US" -le "$threshold_us" ]; then
    ASSERT_STATUS=fail
    ASSERT_ACTUAL=disabled
  else
    ASSERT_STATUS=pass
    ASSERT_ACTUAL=enabled
  fi
}

assert_disabled() {
  threshold_us=$(enabled_threshold_us)
  if [ "$LAST_DURATION_US" -gt "$threshold_us" ]; then
    ASSERT_STATUS=fail
    ASSERT_ACTUAL=enabled
  else
    ASSERT_STATUS=pass
    ASSERT_ACTUAL=disabled
  fi
}

assert_invalid_input() {
  invalid_status=pass
  before_rules=$(get_rule_count)

  # Some shells may not propagate procfs write errors reliably for redirects.
  printf '%s\n' "++++" > "$PROC" 2>/dev/null || true
  printf '%s\n' "line=abc +p" > "$PROC" 2>/dev/null || true
  printf '%s\n' "func= +p" > "$PROC" 2>/dev/null || true
  printf '%s\n' "unknown=foo +p" > "$PROC" 2>/dev/null || true

  after_rules=$(get_rule_count)
  if [ "$after_rules" -ne "$before_rules" ]; then
    invalid_status=fail
  fi

  ASSERT_STATUS=$invalid_status
  ASSERT_ACTUAL=invalid-input
}

measure_disabled_baseline
LINE_KEY=$(detect_line_key)

# F-01: module selector
run_rule "clear"
run_rule "module=$MODULE_KEY +p"
run_bench
assert_enabled
record_case "F-01" "$ASSERT_STATUS" "enabled" "$ASSERT_ACTUAL" "module=$MODULE_KEY"

# F-02: file selector
run_rule "clear"
run_rule "file=$FILE_KEY +p"
run_bench
assert_enabled
record_case "F-02" "$ASSERT_STATUS" "enabled" "$ASSERT_ACTUAL" "file=$FILE_KEY"

# F-03: function selector
run_rule "clear"
run_rule "func=$FUNC_KEY +p"
run_bench
assert_enabled
record_case "F-03" "$ASSERT_STATUS" "enabled" "$ASSERT_ACTUAL" "func=$FUNC_KEY"

# F-04: line selector — /etc/dyndbg_line.txt holds bench_log_0's line
# (a bench_sites site), so the enabled/disabled verdict uses log_batch
# (which executes all bench_sites functions) with a batch-disabled baseline.
run_rule "clear"
run_rule "line=$LINE_KEY +p"
run_bench_batch
threshold_batch_us=$((DISABLED_BATCH_BASE_US + BENCH_ENABLED_MARGIN_US))
if [ "$LAST_DURATION_US" -le "$threshold_batch_us" ]; then
  ASSERT_STATUS=fail
  ASSERT_ACTUAL=disabled
else
  ASSERT_STATUS=pass
  ASSERT_ACTUAL=enabled
fi
record_case "F-04" "$ASSERT_STATUS" "enabled" "$ASSERT_ACTUAL" "line=$LINE_KEY (log_batch)"

# F-05: last-match-wins
run_rule "clear"
run_rule "module=$MODULE_KEY +p"
run_rule "func=$FUNC_KEY -p"
run_bench
assert_disabled
record_case "F-05" "$ASSERT_STATUS" "disabled" "$ASSERT_ACTUAL" "module=$MODULE_KEY func=$FUNC_KEY"

run_rule "clear"
run_rule "func=$FUNC_KEY -p"
run_rule "module=$MODULE_KEY +p"
run_bench
assert_enabled
record_case "F-05" "$ASSERT_STATUS" "enabled" "$ASSERT_ACTUAL" "func=$FUNC_KEY module=$MODULE_KEY"

# F-06: dynamic toggle
run_rule "clear"
run_rule "module=$MODULE_KEY +p"
run_bench
run_rule "clear"
run_bench
assert_disabled
record_case "F-06" "$ASSERT_STATUS" "disabled" "$ASSERT_ACTUAL" "toggle module=$MODULE_KEY"

# F-08: multi-layer rule coverage — final winner is line= (bench_log_0),
# so the verdict uses log_batch (same rationale as F-04).
run_rule "clear"
run_rule "module=$MODULE_KEY +p"
run_rule "file=$FILE_KEY +p"
run_rule "func=$FUNC_KEY -p"
run_rule "line=$LINE_KEY +p"
run_bench_batch
run_rule "clear"
threshold_batch_us=$((DISABLED_BATCH_BASE_US + BENCH_ENABLED_MARGIN_US))
if [ "$LAST_DURATION_US" -le "$threshold_batch_us" ]; then
  ASSERT_STATUS=fail
  ASSERT_ACTUAL=disabled
else
  ASSERT_STATUS=pass
  ASSERT_ACTUAL=enabled
fi
record_case "F-08" "$ASSERT_STATUS" "enabled" "$ASSERT_ACTUAL" "module=$MODULE_KEY file=$FILE_KEY func=$FUNC_KEY line=$LINE_KEY (log_batch)"

# F-07: invalid inputs (best-effort)
assert_invalid_input
record_case "F-07" "$ASSERT_STATUS" "invalid-input" "$ASSERT_ACTUAL" "invalid inputs"

echo "functional test finished"
