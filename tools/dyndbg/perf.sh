#!/bin/sh
# Test: P-01 (static fast path baseline; optional enabled path)
set -eu

PROC=/proc/sys/kernel/dynamic_debug
BENCH=/proc/sys/kernel/dyndbg_bench

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
ITERS=${ITERS:-10000000}
RUNS=${RUNS:-50}
WARMUP=${WARMUP:-5}
RUN_COUNT=${RUN_COUNT:-1}
ENABLE_LOG=${ENABLE_LOG:-0}
BACKEND_MODE=${BACKEND_MODE:-static}
RESULTS_DIR=${RESULTS_DIR:-/ext2/results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
COMMIT=${COMMIT:-unknown}
PHASE=${PHASE:-unknown}
CSV_FILE="$RESULTS_DIR/perf/results.csv"
PER_ROUND_CSV="$RESULTS_DIR/perf/per_round.csv"
PERF=${PERF:-1}
PERF_EVENTS=${PERF_EVENTS:-cycles,instructions,branches,branch-misses}
CLK_TCK=${CLK_TCK:-}
PERF_HARDWARE_SUPPORTED=0

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
    if [ "$PERF_HARDWARE_SUPPORTED" -ne 0 ]; then
      echo "run_id,state,mode,expected_behavior,actual_behavior,status,iters,runs,avg_us,min_us,max_us,mode_label,task_clock_ms,context_switches,page_faults,cycles,instructions,branches,branch_misses" > "$CSV_FILE"
    else
      echo "run_id,state,mode,expected_behavior,actual_behavior,status,iters,runs,avg_us,min_us,max_us,mode_label,task_clock_ms,context_switches,page_faults" > "$CSV_FILE"
    fi
  fi
  if [ ! -e "$PER_ROUND_CSV" ]; then
    echo "run_id,state,mode,round,duration_us" > "$PER_ROUND_CSV"
  fi
}

emit_result() {
  echo "RESULT $*"
}

clear_perf_metrics() {
  TASK_CLOCK_MS=na
  CONTEXT_SWITCHES=na
  PAGE_FAULTS=na
  PERF_CYCLES=
  PERF_INSTRUCTIONS=
  PERF_BRANCHES=
  PERF_BRANCH_MISSES=
}

probe_perf_hardware_support() {
  if [ "$PERF" -eq 0 ] || ! command -v perf >/dev/null 2>&1; then
    PERF_HARDWARE_SUPPORTED=0
    return
  fi

  probe_out=${TMPDIR:-/tmp}/dyndbg_perf_probe.$$
  if perf stat -x, -e "$PERF_EVENTS" -- sh -c 'true' >/dev/null 2>"$probe_out"; then
    PERF_HARDWARE_SUPPORTED=1
  else
    PERF_HARDWARE_SUPPORTED=0
  fi
  rm -f "$probe_out"
}

init_clk_tck() {
  if [ -n "$CLK_TCK" ]; then
    return
  fi

  CLK_TCK=$(getconf CLK_TCK 2>/dev/null || echo "")
  case "$CLK_TCK" in
    ''|*[!0-9]*)
      CLK_TCK=100
      ;;
  esac
}

read_proc_stat_field() {
  key=$1
  if [ ! -r /proc/stat ]; then
    echo "na"
    return
  fi
  value=$(awk -v key="$key" '$1 == key {print $2; exit}' /proc/stat)
  sanitize_metric "$value"
}

read_task_ticks() {
  if [ ! -r /proc/self/stat ]; then
    echo "na"
    return
  fi

  ticks=$(awk '
    {
      sub(/^[^)]*\) /, "");
      split($0, fields, " ");
      if (fields[12] ~ /^[0-9]+$/ && fields[13] ~ /^[0-9]+$/) {
        print fields[12] + fields[13];
      }
    }
  ' /proc/self/stat)
  sanitize_metric "$ticks"
}

collect_soft_metrics_begin() {
  SOFT_TASK_TICKS_BEGIN=$(read_task_ticks)
  SOFT_CONTEXT_BEGIN=$(read_proc_stat_field "ctxt")
  SOFT_PAGE_FAULTS_BEGIN=$(read_proc_stat_field "page_faults")
}

collect_soft_metrics_end() {
  init_clk_tck
  end_ticks=$(read_task_ticks)
  end_context=$(read_proc_stat_field "ctxt")
  end_page_faults=$(read_proc_stat_field "page_faults")

  TASK_CLOCK_MS=na
  CONTEXT_SWITCHES=na
  PAGE_FAULTS=na

  if [ "$SOFT_TASK_TICKS_BEGIN" != "na" ] && [ "$end_ticks" != "na" ] && [ "$end_ticks" -ge "$SOFT_TASK_TICKS_BEGIN" ]; then
    delta_ticks=$((end_ticks - SOFT_TASK_TICKS_BEGIN))
    TASK_CLOCK_MS=$((delta_ticks * 1000 / CLK_TCK))
  fi

  if [ "$SOFT_CONTEXT_BEGIN" != "na" ] && [ "$end_context" != "na" ] && [ "$end_context" -ge "$SOFT_CONTEXT_BEGIN" ]; then
    CONTEXT_SWITCHES=$((end_context - SOFT_CONTEXT_BEGIN))
  fi

  if [ "$SOFT_PAGE_FAULTS_BEGIN" != "na" ] && [ "$end_page_faults" != "na" ] && [ "$end_page_faults" -ge "$SOFT_PAGE_FAULTS_BEGIN" ]; then
    PAGE_FAULTS=$((end_page_faults - SOFT_PAGE_FAULTS_BEGIN))
  fi
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
  value=$(printf '%s' "$value" | tr -d '[:space:]')
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
  collect_soft_metrics_begin

  if [ "$PERF_HARDWARE_SUPPORTED" -ne 0 ]; then
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
    collect_soft_metrics_end
    echo "$output"
    return
  fi

  output=$(echo "mode=$mode iters=$ITERS" > "$BENCH"; cat "$BENCH")
  collect_soft_metrics_end
  echo "$output"
}

duration_from_output() {
  echo "$1" | awk -F= '/^last_duration_us=/{print $2}'
}

run_series() {
  mode=$1
  state=$2
  clear_perf_metrics
  i=0
  cycles_sum=0
  inst_sum=0
  branch_sum=0
  miss_sum=0
  task_clock_sum=0
  context_sum=0
  page_fault_sum=0
  perf_ok=0
  task_clock_ok=0
  context_ok=0
  page_fault_ok=0

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

    echo "$RUN_ID,$state,$mode,$i,$duration" >> "$PER_ROUND_CSV"

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
    if [ "$PERF_HARDWARE_SUPPORTED" -ne 0 ] && [ -n "$PERF_CYCLES" ]; then
      cycles_sum=$((cycles_sum + PERF_CYCLES))
      inst_sum=$((inst_sum + PERF_INSTRUCTIONS))
      branch_sum=$((branch_sum + PERF_BRANCHES))
      miss_sum=$((miss_sum + PERF_BRANCH_MISSES))
      perf_ok=1
    fi
    if [ "$TASK_CLOCK_MS" != "na" ]; then
      task_clock_sum=$((task_clock_sum + TASK_CLOCK_MS))
      task_clock_ok=1
    fi
    if [ "$CONTEXT_SWITCHES" != "na" ]; then
      context_sum=$((context_sum + CONTEXT_SWITCHES))
      context_ok=1
    fi
    if [ "$PAGE_FAULTS" != "na" ]; then
      page_fault_sum=$((page_fault_sum + PAGE_FAULTS))
      page_fault_ok=1
    fi
    i=$((i + 1))
  done

  avg=$((sum / RUNS))
  cycles_avg=na
  inst_avg=na
  branch_avg=na
  miss_avg=na
  task_clock_avg=na
  context_avg=na
  page_fault_avg=na

  if [ "$perf_ok" -ne 0 ]; then
    cycles_avg=$((cycles_sum / RUNS))
    inst_avg=$((inst_sum / RUNS))
    branch_avg=$((branch_sum / RUNS))
    miss_avg=$((miss_sum / RUNS))
  fi
  if [ "$task_clock_ok" -ne 0 ]; then
    task_clock_avg=$((task_clock_sum / RUNS))
  fi
  if [ "$context_ok" -ne 0 ]; then
    context_avg=$((context_sum / RUNS))
  fi
  if [ "$page_fault_ok" -ne 0 ]; then
    page_fault_avg=$((page_fault_sum / RUNS))
  fi

  expected_behavior=$state
  actual_behavior=$state
  status=pass
  if [ "$avg" -lt 0 ]; then
    status=fail
  fi
  if [ "$PERF_HARDWARE_SUPPORTED" -ne 0 ]; then
    emit_result "test=P-01 state=$state mode=$mode expected_behavior=$expected_behavior actual_behavior=$actual_behavior status=$status iters=$ITERS runs=$RUNS avg_us=$avg min_us=$min max_us=$max mode_label=$mode task_clock_ms=$task_clock_avg context_switches=$context_avg page_faults=$page_fault_avg cycles=$cycles_avg instructions=$inst_avg branches=$branch_avg branch_misses=$miss_avg run_id=$RUN_ID"
    echo "$RUN_ID,$state,$mode,$expected_behavior,$actual_behavior,$status,$ITERS,$RUNS,$avg,$min,$max,$mode,$task_clock_avg,$context_avg,$page_fault_avg,$cycles_avg,$inst_avg,$branch_avg,$miss_avg" >> "$CSV_FILE"
  else
    emit_result "test=P-01 state=$state mode=$mode expected_behavior=$expected_behavior actual_behavior=$actual_behavior status=$status iters=$ITERS runs=$RUNS avg_us=$avg min_us=$min max_us=$max mode_label=$mode task_clock_ms=$task_clock_avg context_switches=$context_avg page_faults=$page_fault_avg run_id=$RUN_ID"
    echo "$RUN_ID,$state,$mode,$expected_behavior,$actual_behavior,$status,$ITERS,$RUNS,$avg,$min,$max,$mode,$task_clock_avg,$context_avg,$page_fault_avg" >> "$CSV_FILE"
  fi
}

run_rule "clear"

probe_perf_hardware_support
ensure_csv

case "$BACKEND_MODE" in
  baseline)
    run_series "log" "baseline"
    ;;
  branch)
    run_series "log" "branch"
    ;;
  static)
    run_series "log" "static"
    ;;
  *)
    echo "unknown BACKEND_MODE: $BACKEND_MODE" >&2
    exit 1
    ;;
esac

if [ "$ENABLE_LOG" -ne 0 ]; then
  run_rule "module=$MODULE_KEY +p"
  run_series "log" "enabled"
  run_rule "clear"
fi

echo "perf test finished"
