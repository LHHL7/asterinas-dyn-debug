#!/bin/sh
# Test: T-01..T-06 (dyndbg tracepoint mode: enable/disable, event content,
# log/trace dimension independence, reset, loss statistics) and
#       H-01..H-04 (hotspot statistics: ranking, cross-CPU sum, reset,
#       trace-event consistency)
# Requires: static/branch build with dyndbg feature (same build group as F).
set -eu

PROC=/proc/sys/kernel/dynamic_debug
BENCH=/proc/sys/kernel/dyndbg_bench
TRACE=/proc/sys/kernel/dyndbg_trace
HOT=/proc/sys/kernel/dyndbg_hotspots

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
TRACE_ITERS=${TRACE_ITERS:-1000}
FLOOD_ITERS=${FLOOD_ITERS:-1000000}
HOT_ITERS=${HOT_ITERS:-10000}
RESULTS_DIR=${RESULTS_DIR:-/ext2/results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
CSV_FILE="$RESULTS_DIR/trace/results.csv"
CSV_HEADER='run_id,case,status,expected,actual,details'

if [ ! -e "$PROC" ] || [ ! -e "$TRACE" ] || [ ! -e "$HOT" ]; then
  echo "dyndbg procfs files not found" >&2
  exit 1
fi

run_rule() {
  echo "$1" > "$PROC"
}

read_trace() {
  cat "$TRACE"
}

read_hot() {
  cat "$HOT"
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

trace_events() {
  read_trace | awk -F= '/^events=/{print $2; exit}'
}

trace_lost() {
  read_trace | awk -F= '/^lost=/{print $2; exit}'
}

hot_top_count() {
  # count of the rank-1 hotspot line (cross-CPU summed)
  read_hot | awk -F'count=' '/^1\. /{print $2; exit}'
}

hot_lines() {
  read_hot | grep -E '^[0-9]+\. ' || true
}

run_bench() {
  echo "mode=log iters=$1" > "$BENCH"
}

# T-01: +trace enables event recording; events resolve to the right site
run_rule "clear"
run_rule "module=$MODULE_KEY +trace"
echo "reset" > "$TRACE"
run_bench "$TRACE_ITERS"
trace_out=$(read_trace)
events=$(echo "$trace_out" | awk -F= '/^events=/{print $2; exit}')
event_lines=$(echo "$trace_out" | grep -c '^cpu=[0-9]' || true)
bad_site=$(echo "$trace_out" | grep '^cpu=' | grep -vc "dyndbg_bench" || true)
if [ "$events" = "$TRACE_ITERS" ] && [ "$event_lines" -gt 0 ] && [ "$bad_site" -eq 0 ]; then
  record_case "T-01" pass "events=$TRACE_ITERS site=dyndbg_bench" "events=$events lines=$event_lines" "trace enabled"
else
  record_case "T-01" fail "events=$TRACE_ITERS site=dyndbg_bench" "events=$events lines=$event_lines bad=$bad_site" "trace enabled"
fi

# T-02: -trace disables recording (last-match-wins overrides +trace)
run_rule "module=$MODULE_KEY -trace"
echo "reset" > "$TRACE"
run_bench "$TRACE_ITERS"
events=$(trace_events)
if [ "$events" = "0" ]; then
  record_case "T-02" pass "events=0" "events=$events" "trace disabled"
else
  record_case "T-02" fail "events=0" "events=$events" "trace disabled"
fi

# T-03: +trace alone must NOT enable log output (dimension independence).
# Judged via bench duration: with log off, duration stays near the disabled
# baseline (trace push overhead on 100 iters is far below the margin).
run_rule "clear"
run_bench "$TRACE_ITERS"
DISABLED_BASE_US=$(cat "$BENCH" | awk -F= '/^last_duration_us=/{print $2}')
run_rule "module=$MODULE_KEY +trace"
run_bench "$TRACE_ITERS"
trace_on_log_off_US=$(cat "$BENCH" | awk -F= '/^last_duration_us=/{print $2}')
events=$(trace_events)
if [ -n "$DISABLED_BASE_US" ] && [ "$trace_on_log_off_US" -le $((DISABLED_BASE_US + 1000)) ] && [ "$events" = "$TRACE_ITERS" ]; then
  record_case "T-03" pass "log=disabled trace=events=$TRACE_ITERS" "dur=$trace_on_log_off_US base=$DISABLED_BASE_US events=$events" "log/trace dims independent"
else
  record_case "T-03" fail "log=disabled trace=events=$TRACE_ITERS" "dur=$trace_on_log_off_US base=$DISABLED_BASE_US events=$events" "log/trace dims independent"
fi

# T-03b: +p alone must NOT produce trace events
run_rule "clear"
run_rule "module=$MODULE_KEY +p"
echo "reset" > "$TRACE"
run_bench "$TRACE_ITERS"
events=$(trace_events)
if [ "$events" = "0" ]; then
  record_case "T-03b" pass "events=0 with +p only" "events=$events" "+p does not trace"
else
  record_case "T-03b" fail "events=0 with +p only" "events=$events" "+p does not trace"
fi

# T-04: reset clears events and lost counters
run_rule "clear"
run_rule "module=$MODULE_KEY +trace"
echo "reset" > "$TRACE"
run_bench "$TRACE_ITERS"
echo "reset" > "$TRACE"
events=$(trace_events)
lost=$(trace_lost)
if [ "$events" = "0" ] && [ "$lost" = "0" ]; then
  record_case "T-04" pass "events=0 lost=0 after reset" "events=$events lost=$lost" "reset"
else
  record_case "T-04" fail "events=0 lost=0 after reset" "events=$events lost=$lost" "reset"
fi

# T-05: ring overflow -> lost statistics grow (flood faster than snapshot drain)
run_rule "clear"
run_rule "module=$MODULE_KEY +trace"
echo "reset" > "$TRACE"
run_bench "$FLOOD_ITERS"
trace_out=$(read_trace)
events=$(echo "$trace_out" | awk -F= '/^events=/{print $2; exit}')
lost=$(echo "$trace_out" | awk -F= '/^lost=/{print $2; exit}')
snap_lost=$(echo "$trace_out" | awk -F'[, ]' '/^snapshot:/{print $5; exit}')
if [ "$lost" -gt 0 ] && [ "$events" = "$FLOOD_ITERS" ]; then
  record_case "T-05" pass "lost>0 events=$FLOOD_ITERS" "events=$events lost=$lost snap_lost=$snap_lost" "ring overflow loss stats"
else
  record_case "T-05" fail "lost>0 events=$FLOOD_ITERS" "events=$events lost=$lost snap_lost=$snap_lost" "ring overflow loss stats"
fi

# T-06: per-CPU event attribution (cpu=N present and valid on every event line)
run_rule "clear"
run_rule "module=$MODULE_KEY +trace"
echo "reset" > "$TRACE"
run_bench "$TRACE_ITERS"
trace_out=$(read_trace)
events=$(echo "$trace_out" | awk -F= '/^events=/{print $2; exit}')
bad_cpu=$(echo "$trace_out" | grep '^cpu=' | grep -vc '^cpu=[0-9][0-9]* tsc=' || true)
if [ "$events" = "$TRACE_ITERS" ] && [ "$bad_cpu" -eq 0 ]; then
  record_case "T-06" pass "events=$TRACE_ITERS cpu=valid" "events=$events bad_cpu=$bad_cpu" "per-CPU attribution"
else
  record_case "T-06" fail "events=$TRACE_ITERS cpu=valid" "events=$events bad_cpu=$bad_cpu" "per-CPU attribution"
fi

# H-01: hotspot ranking — skewed workload puts the hottest site first
run_rule "clear"
run_rule "module=$MODULE_KEY +trace"
echo "reset" > "$HOT"
echo "reset" > "$TRACE"
run_bench "$HOT_ITERS"        # bench_log site: HOT_ITERS hits
run_bench "$TRACE_ITERS"      # more hits on the same site
run_bench "$TRACE_ITERS"      # and again (keep bench_log dominant)
run_bench "$TRACE_ITERS"
hot_out=$(read_hot)
top1=$(echo "$hot_out" | awk -F'count=' '/^1\. /{print $2; exit}')
top1_line=$(echo "$hot_out" | grep '^1\. ' || true)
top1_is_bench=$(echo "$top1_line" | grep -c 'dyndbg_bench\.rs' || true)
foreign=$(echo "$hot_out" | grep '^[0-9]' | grep -vc 'dyndbg_bench' || true)
if [ "$top1_is_bench" -eq 1 ] && [ "$top1" -ge "$HOT_ITERS" ] && [ "$foreign" -eq 0 ]; then
  record_case "H-01" pass "top1=dyndbg_bench.rs count>=$HOT_ITERS" "top1=$top1 foreign=$foreign" "hotspot ranking"
else
  record_case "H-01" fail "top1=dyndbg_bench.rs count>=$HOT_ITERS" "top1=$top1 top1_is_bench=$top1_is_bench foreign=$foreign" "hotspot ranking"
fi

# H-02: cross-CPU counts are summed — total hits == trace events for the site
run_rule "clear"
run_rule "module=$MODULE_KEY +trace"
echo "reset" > "$HOT"
echo "reset" > "$TRACE"
run_bench "$TRACE_ITERS"
hot_top=$(hot_top_count)
events=$(trace_events)
if [ "$hot_top" = "$TRACE_ITERS" ] && [ "$events" = "$TRACE_ITERS" ]; then
  record_case "H-02" pass "hotspot==events==$TRACE_ITERS" "hot=$hot_top events=$events" "hotspot/trace consistency"
else
  record_case "H-02" fail "hotspot==events==$TRACE_ITERS" "hot=$hot_top events=$events" "hotspot/trace consistency"
fi

# H-03: reset zeroes hotspot counters
echo "reset" > "$HOT"
lines=$(hot_lines | wc -l | tr -d ' ')
if [ "$lines" -eq 0 ]; then
  record_case "H-03" pass "no hotspot lines after reset" "lines=$lines" "hotspot reset"
else
  record_case "H-03" fail "no hotspot lines after reset" "lines=$lines" "hotspot reset"
fi

# H-04: log_batch spreads hits across all 65 bench_sites (multi-site ranking)
run_rule "clear"
run_rule "module=$MODULE_KEY +trace"
echo "reset" > "$HOT"
echo "mode=log_batch iters=1" > "$BENCH"
hot_out=$(read_hot)
count_lines=$(echo "$hot_out" | grep -c '^[0-9]' || true)
if [ "$count_lines" -ge 10 ]; then
  record_case "H-04" pass "top-10 filled by log_batch" "lines=$count_lines" "multi-site hotspots"
else
  record_case "H-04" fail "top-10 filled by log_batch" "lines=$count_lines" "multi-site hotspots"
fi

# cleanup: leave the system in a clean state
run_rule "clear"
echo "reset" > "$TRACE"
echo "reset" > "$HOT"

echo "trace test finished"
