#!/bin/sh
# Test: S-01..S-07 (`cat dynamic_debug` status listing)
# Verifies the rule chain listing and per-descriptor state columns
# (file:line [module] func +p|-p +trace|-trace <flags>).
set -eu

PROC=/proc/sys/kernel/dynamic_debug
MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
RESULTS_DIR=${RESULTS_DIR:-/ext2/results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
CSV_FILE="$RESULTS_DIR/status/results.csv"
CSV_HEADER='run_id,case,status,expected,actual,details'

if [ ! -e "$PROC" ]; then
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

rule_count() {
  cat "$PROC" | awk -F'[= ]' '/^rules=/{print $2; exit}'
}

# Descriptor section of the listing (between "descriptors=N" and "usage:"),
# excluding the "descriptors=N" header line itself.
desc_section() {
  cat "$PROC" | sed -n '/^descriptors=/,$p' | sed '/^descriptors=/d' | sed '/^usage:/,$d'
}

desc_count_printed() {
  cat "$PROC" | awk -F= '/^descriptors=/{print $2; exit}'
}

# Number of descriptor lines whose rendered state column matches the pattern.
desc_matching() {
  pattern=$1
  desc_section | grep -c "$pattern" || true
}

# S-01: clear -> rules=0, descriptors=N consistent, all sites -p
run_rule "clear"
r=$(rule_count)
n=$(desc_count_printed)
lines=$(desc_section | wc -l | tr -d ' ')
enabled=$(desc_matching ' +p ')
if [ "$r" = "0" ] && [ "$n" = "$lines" ] && [ "$n" -gt 0 ] && [ "$enabled" = "0" ]; then
  record_case "S-01" pass "rules=0 all -p descriptors=$n" "rules=$r desc=$n lines=$lines enabled=$enabled" "clear"
else
  record_case "S-01" fail "rules=0 all -p descriptors=$n" "rules=$r desc=$n lines=$lines enabled=$enabled" "clear"
fi

# S-02: rule chain listing shows appended rules with indices
run_rule "module=$MODULE_KEY +p"
run_rule "func=bench_log -p"
r=$(rule_count)
rule_lines=$(cat "$PROC" | sed -n '/^0: /,/^descriptors=/p' | grep -c '^[0-9]*: ' || true)
if [ "$r" = "2" ] && [ "$rule_lines" = "2" ]; then
  record_case "S-02" pass "rules=2 with indices" "rules=$r rule_lines=$rule_lines" "chain listing"
else
  record_case "S-02" fail "rules=2 with indices" "rules=$r rule_lines=$rule_lines" "chain listing"
fi

# S-03: module +p -> every dyndbg_bench descriptor line shows +p
run_rule "clear"
run_rule "module=$MODULE_KEY +p"
total=$(desc_matching 'dyndbg_bench')
enabled=$(desc_matching ' +p ')
if [ "$total" -gt 0 ] && [ "$total" = "$enabled" ]; then
  record_case "S-03" pass "all $total dyndbg_bench sites +p" "total=$total enabled=$enabled" "state column +p"
else
  record_case "S-03" fail "all $total dyndbg_bench sites +p" "total=$total enabled=$enabled" "state column +p"
fi

# S-04: +trace shows in the trace state column
run_rule "clear"
run_rule "module=$MODULE_KEY +trace"
total=$(desc_matching 'dyndbg_bench')
traced=$(desc_matching ' +trace ')
if [ "$total" -gt 0 ] && [ "$total" = "$traced" ]; then
  record_case "S-04" pass "all $total sites +trace" "total=$total traced=$traced" "state column +trace"
else
  record_case "S-04" fail "all $total sites +trace" "total=$total traced=$traced" "state column +trace"
fi

# S-05: del <id> removes the rule; remaining chain state is reflected
# (r1=module +p removed -> only func=bench_log -p remains. Replay semantics
#  (Linux): non-matching descriptors fall back to the default disabled state,
#  so enabled=0 is the correct result, NOT total-1.)
run_rule "clear"
run_rule "module=$MODULE_KEY +p"
run_rule "func=bench_log -p"
run_rule "del 0"
r=$(rule_count)
enabled=$(desc_matching ' +p ')
total=$(desc_matching 'dyndbg_bench')
if [ "$r" = "1" ] && [ "$enabled" = "0" ]; then
  record_case "S-05" pass "after del 0: rules=1, replay resets to default -p" "rules=$r enabled=$enabled total=$total" "del"
else
  record_case "S-05" fail "after del 0: rules=1, replay resets to default -p" "rules=$r enabled=$enabled total=$total" "del"
fi

# S-06: del updates the state column (remove -p, last winner becomes +p)
# Note: after module=-p, ALL descriptors are -p (65 bench disabled by the
# rule + the other 218 default-disabled), so disabled must equal the total
# descriptor count, not the module hit set.
run_rule "clear"
run_rule "module=$MODULE_KEY +p"
run_rule "module=$MODULE_KEY -p"
disabled=$(desc_matching ' -p ')
run_rule "del 1"
enabled=$(desc_matching ' +p ')
total=$(desc_matching 'dyndbg_bench')
n=$(desc_count_printed)
if [ "$disabled" = "$n" ] && [ "$enabled" = "$total" ]; then
  record_case "S-06" pass "del -p flips state back to +p" "disabled=$disabled enabled=$enabled total=$total desc=$n" "del state update"
else
  record_case "S-06" fail "del -p flips state back to +p" "disabled=$disabled enabled=$enabled total=$total desc=$n" "del state update"
fi

# S-07: clear resets every site to -p
run_rule "clear"
enabled=$(desc_matching ' +p ')
if [ "$enabled" = "0" ]; then
  record_case "S-07" pass "all sites -p after clear" "enabled=$enabled" "clear state reset"
else
  record_case "S-07" fail "all sites -p after clear" "enabled=$enabled" "clear state reset"
fi

echo "status test finished"
