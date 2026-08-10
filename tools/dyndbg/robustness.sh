#!/bin/sh
# Test: R-01..R-06 (robustness of the new grammar — invalid inputs must be
# rejected with EINVAL and must not corrupt rule chain or descriptor state)
set -eu

PROC=/proc/sys/kernel/dynamic_debug
BENCH=/proc/sys/kernel/dyndbg_bench

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
ITERS=${ITERS:-100}
BENCH_ENABLED_MARGIN_US=${BENCH_ENABLED_MARGIN_US:-1000}
RESULTS_DIR=${RESULTS_DIR:-/ext2/results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
CSV_FILE="$RESULTS_DIR/robustness/results.csv"
CSV_HEADER='run_id,case,status,expected,actual,details'

if [ ! -e "$PROC" ]; then
  echo "dyndbg procfs files not found" >&2
  exit 1
fi

run_rule() {
  echo "$1" > "$PROC"
}

# Attempt an invalid write; the error is expected, never fatal.
try_invalid() {
  printf '%s\n' "$1" > "$PROC" 2>/dev/null || true
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

record_case() {
  case_id=$1
  status=$2
  expected=$3
  actual=$4
  details=$5
  details=$(echo "$details" | tr ' ' ';')
  ensure_csv
  echo "RESULT test=$case_id status=$status expected=$expected actual=$actual run_id=$RUN_ID details=$details"
  echo "$RUN_ID,$case_id,$status,$expected,$actual,$details" >> "$CSV_FILE"
}

rule_count() {
  cat "$PROC" | awk -F'[= ]' '/^rules=/{print $2; exit}'
}

bench_duration() {
  echo "mode=log iters=$ITERS" > "$BENCH"
  cat "$BENCH" | awk -F= '/^last_duration_us=/{print $2; exit}'
}

run_rule "clear"
DISABLED_BASE_US=$(bench_duration)

# A batch of invalid writes must leave the rule chain untouched.
invalid_batch() {
  for cmd in "$@"; do
    try_invalid "$cmd"
  done
}

# R-01: invalid action characters / unknown actions
before=$(rule_count)
invalid_batch "++p" "+q" "+pflx" "+tracee" "-trace2"
after=$(rule_count)
if [ "$before" = "$after" ]; then
  record_case "R-01" pass "rules unchanged after invalid actions" "before=$before after=$after" "invalid actions"
else
  record_case "R-01" fail "rules unchanged after invalid actions" "before=$before after=$after" "invalid actions"
fi

# R-02: invalid flag combinations
before=$(rule_count)
invalid_batch "+fq" "-z" "+=f" "+p_+"
after=$(rule_count)
if [ "$before" = "$after" ]; then
  record_case "R-02" pass "rules unchanged after invalid flags" "before=$before after=$after" "invalid flags"
else
  record_case "R-02" fail "rules unchanged after invalid flags" "before=$before after=$after" "invalid flags"
fi

# R-03: invalid del commands
run_rule "module=$MODULE_KEY +p"
before=$(rule_count)
invalid_batch "del abc" "del 999" "del -1" "del" "del 0 1"
after=$(rule_count)
if [ "$before" = "$after" ] && [ "$after" = "1" ]; then
  record_case "R-03" pass "rules unchanged after invalid del" "before=$before after=$after" "invalid del"
else
  record_case "R-03" fail "rules unchanged after invalid del" "before=$before after=$after" "invalid del"
fi

# R-04: overlong command (> 256 bytes)
long_cmd=$(awk 'BEGIN { for (i=0; i<300; i++) printf "a"; print "" }')
before=$(rule_count)
try_invalid "$long_cmd"
after=$(rule_count)
if [ "$before" = "$after" ]; then
  record_case "R-04" pass "rules unchanged after overlong cmd" "before=$before after=$after" "overlong"
else
  record_case "R-04" fail "rules unchanged after overlong cmd" "before=$before after=$after" "overlong"
fi

# R-05: empty / whitespace-only commands
before=$(rule_count)
try_invalid ""
try_invalid "   "
after=$(rule_count)
if [ "$before" = "$after" ]; then
  record_case "R-05" pass "rules unchanged after empty cmd" "before=$before after=$after" "empty"
else
  record_case "R-05" fail "rules unchanged after empty cmd" "before=$before after=$after" "empty"
fi

# R-06: system still fully functional after all invalid inputs
run_rule "clear"
run_rule "module=$MODULE_KEY +p"
d=$(bench_duration)
if [ -n "$d" ] && [ "$d" -gt $((DISABLED_BASE_US + BENCH_ENABLED_MARGIN_US)) ]; then
  record_case "R-06" pass "valid rule still works after errors" "dur=$d base=$DISABLED_BASE_US" "post-error health"
else
  record_case "R-06" fail "valid rule still works after errors" "dur=$d base=$DISABLED_BASE_US" "post-error health"
fi

run_rule "clear"
echo "robustness test finished"
