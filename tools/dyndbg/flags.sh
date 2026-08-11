#!/bin/sh
# Test: FL-01..FL-08 (output format flags +f/+l/+m/+t)
# Verifies the log prefix rendered by `format_dyndbg_log`:
#   file:line [module] function [task=0x...] <args>
# Requires: dmesg (busybox), LOG_LEVEL=debug build, same build group as F.
set -eu

PROC=/proc/sys/kernel/dynamic_debug
BENCH=/proc/sys/kernel/dyndbg_bench

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
FL_ITERS=${FL_ITERS:-3}
RESULTS_DIR=${RESULTS_DIR:-/ext2/results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
CSV_FILE="$RESULTS_DIR/flags/results.csv"
CSV_HEADER='run_id,case,status,expected,actual,details'

if [ ! -e "$PROC" ] || [ ! -e "$BENCH" ]; then
  echo "dyndbg procfs files not found" >&2
  exit 1
fi

if ! command -v dmesg >/dev/null 2>&1; then
  echo "dmesg not available; FL tests require dmesg" >&2
  exit 1
fi

run_rule() {
  echo "$1" > "$PROC"
}

run_bench() {
  echo "mode=log iters=$FL_ITERS" > "$BENCH"
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

# Emit the log lines produced since the previous dmesg marker.
# `dmesg` (via syslog(3)) is non-consuming: it copies the whole kernel log
# ring without clearing it, so `dmesg_mark` must use `dmesg -c`
# (syslog(4) READ_CLEAR) to reset the ring; otherwise every later `dmesg`
# shows all historical lines again.
new_log_lines() {
  dmesg 2>/dev/null | grep 'dyndbg bench log' || true
}

dmesg_mark() {
  dmesg -c >/dev/null 2>&1 || true
}

run_with_marker() {
  run_rule "clear"
  dmesg_mark
  run_rule "$1"
  run_bench
  new_log_lines
}

# FL-01: +pflm renders "file:line [module] function <args>"
lines=$(run_with_marker "module=$MODULE_KEY +pflm")
has_file_line=$(echo "$lines" | grep -c 'dyndbg_bench\.rs:[0-9]' || true)
has_module=$(echo "$lines" | grep -c '\[.*dyndbg_bench\]' || true)
has_func=$(echo "$lines" | grep -c 'bench_log .*dyndbg bench log' || true)
if [ "$has_file_line" -gt 0 ] && [ "$has_module" -gt 0 ] && [ "$has_func" -gt 0 ]; then
  record_case "FL-01" pass "file:line [module] function prefix" "fl=$has_file_line m=$has_module f=$has_func" "+pfl"
else
  record_case "FL-01" fail "file:line [module] function prefix" "fl=$has_file_line m=$has_module f=$has_func" "+pfl"
fi

# FL-02: flags-only command (+f) must NOT flip the log switch
lines=$(run_with_marker "module=$MODULE_KEY +f")
# `echo "$lines" | wc -l` would count 1 for an empty capture (echo adds a
# newline); count matched lines instead so an empty result is 0.
count=$(printf '%s' "$lines" | grep -c '^' || true)
if [ "$count" -eq 0 ]; then
  record_case "FL-02" pass "no log output with flags-only +f" "lines=$count" "flags-only keeps switch off"
else
  record_case "FL-02" fail "no log output with flags-only +f" "lines=$count" "flags-only keeps switch off"
fi

# FL-03: -f clears only the function bit (file:line + module remain)
run_rule "clear"
dmesg_mark
run_rule "module=$MODULE_KEY +pflm"
run_rule "module=$MODULE_KEY -f"
run_bench
lines=$(new_log_lines)
has_file_line=$(echo "$lines" | grep -c 'dyndbg_bench\.rs:[0-9]' || true)
has_module=$(echo "$lines" | grep -c '\[.*dyndbg_bench\]' || true)
has_func=$(echo "$lines" | grep -c 'bench_log' || true)
if [ "$has_file_line" -gt 0 ] && [ "$has_module" -gt 0 ] && [ "$has_func" -eq 0 ]; then
  record_case "FL-03" pass "-f clears function bit only" "fl=$has_file_line m=$has_module f=$has_func" "+pfl then -f"
else
  record_case "FL-03" fail "-f clears function bit only" "fl=$has_file_line m=$has_module f=$has_func" "+pfl then -f"
fi

# FL-04: =fl overwrites previous flags (module bit removed)
run_rule "clear"
dmesg_mark
run_rule "module=$MODULE_KEY +pflm"
run_rule "module=$MODULE_KEY =fl"
run_bench
lines=$(new_log_lines)
has_func=$(echo "$lines" | grep -c 'bench_log .*dyndbg bench log' || true)
has_module=$(echo "$lines" | grep -c '\[.*dyndbg_bench\]' || true)
if [ "$has_func" -gt 0 ] && [ "$has_module" -eq 0 ]; then
  record_case "FL-04" pass "=fl overwrites to func+line" "f=$has_func m=$has_module" "+pflm then =fl"
else
  record_case "FL-04" fail "=fl overwrites to func+line" "f=$has_func m=$has_module" "+pflm then =fl"
fi

# FL-05: +_ clears all flags (bare message, no file:line prefix)
run_rule "clear"
dmesg_mark
run_rule "module=$MODULE_KEY +pfl"
run_rule "module=$MODULE_KEY +_"
run_bench
lines=$(new_log_lines)
has_prefix=$(echo "$lines" | grep -c 'dyndbg_bench\.rs:' || true)
has_plain=$(echo "$lines" | grep -c 'dyndbg bench log' || true)
if [ "$has_prefix" -eq 0 ] && [ "$has_plain" -gt 0 ]; then
  record_case "FL-05" pass "+_ clears all flags" "prefix=$has_prefix plain=$has_plain" "+pfl then +_"
else
  record_case "FL-05" fail "+_ clears all flags" "prefix=$has_prefix plain=$has_plain" "+pfl then +_"
fi

# FL-06: +t renders [task=0x...] thread prefix
lines=$(run_with_marker "module=$MODULE_KEY +pt")
has_task=$(echo "$lines" | grep -c '\[task=0x[0-9a-f]' || true)
if [ "$has_task" -gt 0 ]; then
  record_case "FL-06" pass "[task=0x...] prefix" "task=$has_task" "+pt"
else
  record_case "FL-06" fail "[task=0x...] prefix" "task=$has_task" "+pt"
fi

# FL-07: flags accumulate across rules (last-match-wins on flags)
run_rule "clear"
dmesg_mark
run_rule "module=$MODULE_KEY +pfl"
run_rule "module=$MODULE_KEY +m"
run_bench
lines=$(new_log_lines)
has_all=$(echo "$lines" | grep -c 'dyndbg_bench\.rs:[0-9].*\[.*dyndbg_bench\].*bench_log ' || true)
if [ "$has_all" -gt 0 ]; then
  record_case "FL-07" pass "flags accumulate (fl then +m)" "all=$has_all" "multi-rule flags"
else
  record_case "FL-07" fail "flags accumulate (fl then +m)" "all=$has_all" "multi-rule flags"
fi

# FL-08: default +p (no flags) renders bare message (zero-extra-cost path)
lines=$(run_with_marker "module=$MODULE_KEY +p")
has_prefix=$(echo "$lines" | grep -c 'dyndbg_bench\.rs:' || true)
has_plain=$(echo "$lines" | grep -c 'dyndbg bench log' || true)
if [ "$has_prefix" -eq 0 ] && [ "$has_plain" -gt 0 ]; then
  record_case "FL-08" pass "bare message with flags==0" "prefix=$has_prefix plain=$has_plain" "+p no flags"
else
  record_case "FL-08" fail "bare message with flags==0" "prefix=$has_prefix plain=$has_plain" "+p no flags"
fi

run_rule "clear"
echo "flags test finished"
