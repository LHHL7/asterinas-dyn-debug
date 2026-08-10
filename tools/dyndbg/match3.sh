#!/bin/sh
# Test: M-01..M-08 (three-channel matching engine semantics)
# Channels: 1) exact value  2) path segment(s)  3) wildcard (*/?)
# No substring matching — `file=ench_sites` must NOT match bench_sites.rs.
# Includes index=on/off candidate-set equivalence (mirror property).
# Enable/disable judged via dyndbg_bench duration (same method as functional.sh).
set -eu

PROC=/proc/sys/kernel/dynamic_debug
BENCH=/proc/sys/kernel/dyndbg_bench

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
ITERS=${ITERS:-100}
BENCH_ENABLED_MARGIN_US=${BENCH_ENABLED_MARGIN_US:-1000}
RESULTS_DIR=${RESULTS_DIR:-/ext2/results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
CSV_FILE="$RESULTS_DIR/match3/results.csv"
CSV_HEADER='run_id,case,status,expected,actual,details'

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

# Run one bench mode and return last_duration_us.
bench_duration() {
  mode=$1
  echo "mode=$mode iters=$ITERS" > "$BENCH"
  cat "$BENCH" | awk -F= '/^last_duration_us=/{print $2; exit}'
}

# 1/0 verdicts for a given bench mode, relative to the disabled baseline.
bench_enabled() {
  mode=$1
  d=$(bench_duration "$mode")
  if [ -n "$d" ] && [ "$d" -gt $((DISABLED_BASE_US + BENCH_ENABLED_MARGIN_US)) ]; then
    echo "1"
  else
    echo "0"
  fi
}

run_rule "clear"
DISABLED_BASE_US=$(bench_duration "log")

# M-01: exact module path (bench_sites only, bench_log excluded)
run_rule "module=dyndbg_bench::bench_sites +p"
log_on=$(bench_enabled "log")        # bench_log site not matched -> disabled
batch_on=$(bench_enabled "log_batch") # 65 bench_sites matched -> enabled
if [ "$log_on" = "0" ] && [ "$batch_on" = "1" ]; then
  record_case "M-01" pass "exact module excludes bench_log" "log=$log_on batch=$batch_on" "module=dyndbg_bench::bench_sites"
else
  record_case "M-01" fail "exact module excludes bench_log" "log=$log_on batch=$batch_on" "module=dyndbg_bench::bench_sites"
fi

# M-02: ancestor segment matches all descendants
run_rule "clear"
run_rule "module=$MODULE_KEY +p"
log_on=$(bench_enabled "log")
batch_on=$(bench_enabled "log_batch")
if [ "$log_on" = "1" ] && [ "$batch_on" = "1" ]; then
  record_case "M-02" pass "ancestor segment hits all" "log=$log_on batch=$batch_on" "module=$MODULE_KEY"
else
  record_case "M-02" fail "ancestor segment hits all" "log=$log_on batch=$batch_on" "module=$MODULE_KEY"
fi

# M-03: file basename segment match
run_rule "clear"
run_rule "file=bench_sites.rs +p"
log_on=$(bench_enabled "log")
batch_on=$(bench_enabled "log_batch")
if [ "$log_on" = "0" ] && [ "$batch_on" = "1" ]; then
  record_case "M-03" pass "file segment hits bench_sites only" "log=$log_on batch=$batch_on" "file=bench_sites.rs"
else
  record_case "M-03" fail "file segment hits bench_sites only" "log=$log_on batch=$batch_on" "file=bench_sites.rs"
fi

# M-04: wildcard glob
run_rule "clear"
run_rule "module=*ndbg* +p"
log_on=$(bench_enabled "log")
if [ "$log_on" = "1" ]; then
  record_case "M-04" pass "wildcard matches" "log=$log_on" "module=*ndbg*"
else
  record_case "M-04" fail "wildcard matches" "log=$log_on" "module=*ndbg*"
fi

# M-05: negative cases — plain substring without wildcard must NOT match
run_rule "clear"
run_rule "module=ndbg +p"
log_on=$(bench_enabled "log")
batch_on=$(bench_enabled "log_batch")
run_rule "clear"
run_rule "file=ench_sites +p"
log_f2=$(bench_enabled "log")
batch_f2=$(bench_enabled "log_batch")
if [ "$log_on" = "0" ] && [ "$batch_on" = "0" ] && [ "$log_f2" = "0" ] && [ "$batch_f2" = "0" ]; then
  record_case "M-05" pass "no substring match (behavior change)" "m=0/0 f=0/0" "module=ndbg file=ench_sites"
else
  record_case "M-05" fail "no substring match (behavior change)" "m=$log_on/$batch_on f=$log_f2/$batch_f2" "module=ndbg file=ench_sites"
fi

# M-06: function is atomic exact on the SHORT name (descriptor stores
# `bench_log`, not the full path) — no substring, no segment channel.
run_rule "clear"
run_rule "func=bench_log +p"
log_exact=$(bench_enabled "log")
run_rule "clear"
run_rule "func=bench_lo +p"
log_partial=$(bench_enabled "log")
run_rule "clear"
run_rule "func=*bench_log* +p"
log_wild=$(bench_enabled "log")
if [ "$log_exact" = "1" ] && [ "$log_partial" = "0" ] && [ "$log_wild" = "1" ]; then
  record_case "M-06" pass "func atomic: short-name exact, wildcard hit, partial miss" "exact=$log_exact partial=$log_partial wild=$log_wild" "func semantics"
else
  record_case "M-06" fail "func atomic: short-name exact, wildcard hit, partial miss" "exact=$log_exact partial=$log_partial wild=$log_wild" "func semantics"
fi

# M-07: index=on and index=off produce identical candidate sets (enabled counts)
enabled_count() {
  cat "$PROC" | sed -n '/^descriptors=/,$p' | sed '/^usage:/,$d' | grep -c ' +p ' || true
}
check_equivalence() {
  rule=$1
  run_rule "clear"
  echo "index=on" > "$BENCH"
  run_rule "$rule +p"
  on_count=$(enabled_count)
  run_rule "clear"
  echo "index=off" > "$BENCH"
  run_rule "$rule +p"
  off_count=$(enabled_count)
  echo "$on_count $off_count"
}
m_eq=$(check_equivalence "module=$MODULE_KEY")
f_eq=$(check_equivalence "file=bench_sites.rs")
w_eq=$(check_equivalence "module=*ndbg*")
echo "index=on" > "$BENCH"
m_on=$(echo "$m_eq" | awk '{print $1}')
m_off=$(echo "$m_eq" | awk '{print $2}')
f_on=$(echo "$f_eq" | awk '{print $1}')
f_off=$(echo "$f_eq" | awk '{print $2}')
w_on=$(echo "$w_eq" | awk '{print $1}')
w_off=$(echo "$w_eq" | awk '{print $2}')
if [ "$m_on" = "$m_off" ] && [ "$m_on" -gt 0 ] && [ "$f_on" = "$f_off" ] && [ "$f_on" -gt 0 ] && [ "$w_on" = "$w_off" ] && [ "$w_on" -gt 0 ]; then
  record_case "M-07" pass "index on/off candidate sets equal" "m=$m_on/$m_off f=$f_on/$f_off w=$w_on/$w_off" "index equivalence"
else
  record_case "M-07" fail "index on/off candidate sets equal" "m=$m_on/$m_off f=$f_on/$f_off w=$w_on/$w_off" "index equivalence"
fi

# M-08: multi-selector AND semantics
run_rule "clear"
run_rule "module=$MODULE_KEY file=bench_sites.rs +p"
log_on=$(bench_enabled "log")
batch_on=$(bench_enabled "log_batch")
if [ "$log_on" = "0" ] && [ "$batch_on" = "1" ]; then
  record_case "M-08" pass "AND narrows to bench_sites" "log=$log_on batch=$batch_on" "module+file AND"
else
  record_case "M-08" fail "AND narrows to bench_sites" "log=$log_on batch=$batch_on" "module+file AND"
fi

run_rule "clear"
echo "match3 test finished"
