#!/bin/sh
# Test: P-01 (disabled fast path baseline; optional enabled path)
set -eu

PROC=/proc/sys/kernel/dynamic_debug
BENCH=/proc/sys/kernel/dyndbg_bench

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
ITERS=${ITERS:-1000000}
RUNS=${RUNS:-3}
WARMUP=${WARMUP:-1}
RUN_COUNT=${RUN_COUNT:-1}
ENABLE_LOG=${ENABLE_LOG:-0}
RESULTS_DIR=${RESULTS_DIR:-results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
COMMIT=${COMMIT:-unknown}
PHASE=${PHASE:-unknown}
CSV_FILE="$RESULTS_DIR/perf/results.csv"
PERF=${PERF:-0}
PERF_EVENTS=${PERF_EVENTS:-cycles,instructions,branches,branch-misses}

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
    echo "run_id,commit,phase,state,mode,iters,runs,avg_us,min_us,max_us,cycles,instructions,branches,branch_misses" > "$CSV_FILE"
  fi
}

emit_result() {
  echo "RESULT $*"
}

clear_perf_metrics() {
  PERF_CYCLES=na
  PERF_INSTRUCTIONS=na
  PERF_BRANCHES=na
  PERF_BRANCH_MISSES=na
}

parse_perf() {
  perf_out=$1
  PERF_CYCLES=$(echo "$perf_out" | awk -F, '$3 ~ /^cycles/ {print $1; exit}')
  PERF_INSTRUCTIONS=$(echo "$perf_out" | awk -F, '$3 ~ /^instructions/ {print $1; exit}')
  PERF_BRANCHES=$(echo "$perf_out" | awk -F, '$3 ~ /^branches/ {print $1; exit}')
  PERF_BRANCH_MISSES=$(echo "$perf_out" | awk -F, '$3 ~ /^branch-misses/ {print $1; exit}')

  PERF_CYCLES=$(sanitize_metric "$PERF_CYCLES")
  PERF_INSTRUCTIONS=$(sanitize_metric "$PERF_INSTRUCTIONS")
  PERF_BRANCHES=$(sanitize_metric "$PERF_BRANCHES")
  PERF_BRANCH_MISSES=$(sanitize_metric "$PERF_BRANCH_MISSES")
}

sanitize_metric() {
  value=$1
  case "$value" in
    ''|*[!0-9]*)
      echo "na"
      ;;
    *)
      echo "$value"
      ;;
  esac
}

run_bench() {
  mode=$1
  if [ "$PERF" -ne 0 ] && command -v perf >/dev/null 2>&1; then
    bench_out=${TMPDIR:-/tmp}/dyndbg_bench_out.$$
    perf_out=${TMPDIR:-/tmp}/dyndbg_perf_out.$$
    if perf stat -x, -e "$PERF_EVENTS" -- sh -c "echo mode=$mode iters=$ITERS > $BENCH; cat $BENCH" 1>"$bench_out" 2>"$perf_out"; then
      output=$(cat "$bench_out")
      parse_perf "$(cat "$perf_out")"
    else
      output=$(echo "mode=$mode iters=$ITERS" > "$BENCH"; cat "$BENCH")
      clear_perf_metrics
    fi
    rm -f "$bench_out" "$perf_out"
    echo "$output"
    return
  fi

  output=$(echo "mode=$mode iters=$ITERS" > "$BENCH"; cat "$BENCH")
  clear_perf_metrics
  echo "$output"
}

duration_from_output() {
  echo "$1" | awk -F= '/^last_duration_us=/{print $2}'
}

run_series() {
  mode=$1
  state=$2
  i=0
  cycles_sum=0
  inst_sum=0
  branch_sum=0
  miss_sum=0
  perf_ok=0

  if [ "$WARMUP" -gt 0 ]; then
    while [ "$i" -lt "$WARMUP" ]; do
      run_bench "$mode" >/dev/null
      i=$((i + 1))
    done
  fi

  i=1
  sum=0
  min=0
  max=0

  while [ "$i" -le "$RUNS" ]; do
    output=$(run_bench "$mode")
    echo "$output"
    duration=$(duration_from_output "$output")
    if [ -z "$duration" ]; then
      echo "missing duration for mode=$mode" >&2
      return 1
    fi

    if [ "$i" -eq 1 ]; then
      min=$duration
      max=$duration
    else
      if [ "$duration" -lt "$min" ]; then
        min=$duration
      fi
      if [ "$duration" -gt "$max" ]; then
        max=$duration
      fi
    fi

    sum=$((sum + duration))
    if [ "$PERF_CYCLES" != "na" ]; then
      cycles_sum=$((cycles_sum + PERF_CYCLES))
      inst_sum=$((inst_sum + PERF_INSTRUCTIONS))
      branch_sum=$((branch_sum + PERF_BRANCHES))
      miss_sum=$((miss_sum + PERF_BRANCH_MISSES))
      perf_ok=1
    fi
    i=$((i + 1))
  done

  avg=$((sum / RUNS))
  if [ "$perf_ok" -ne 0 ]; then
    cycles_avg=$((cycles_sum / RUNS))
    inst_avg=$((inst_sum / RUNS))
    branch_avg=$((branch_sum / RUNS))
    miss_avg=$((miss_sum / RUNS))
  else
    cycles_avg=na
    inst_avg=na
    branch_avg=na
    miss_avg=na
  fi
  ensure_csv
  emit_result "test=P-01 state=$state mode=$mode iters=$ITERS runs=$RUNS avg_us=$avg min_us=$min max_us=$max cycles=$cycles_avg instructions=$inst_avg branches=$branch_avg branch_misses=$miss_avg run_id=$RUN_ID commit=$COMMIT phase=$PHASE"
  echo "$RUN_ID,$COMMIT,$PHASE,$state,$mode,$ITERS,$RUNS,$avg,$min,$max,$cycles_avg,$inst_avg,$branch_avg,$miss_avg" >> "$CSV_FILE"
}

run_rule "clear"

run_series "log" "disabled"

if [ "$RUN_COUNT" -ne 0 ]; then
  run_series "count" "count"
fi

if [ "$ENABLE_LOG" -ne 0 ]; then
  run_rule "module=$MODULE_KEY +p"
  run_series "log" "enabled"
  run_rule "clear"
fi

echo "perf test finished"
