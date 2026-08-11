#!/bin/sh
# Test: P-01R (real workload build-variant comparison)
# Timing: TSC-based via /proc/sys/kernel/tsc (high-precision)
# Statistics: per-round data + mean ± sd + 95% CI
set -eu

PROC=/proc/sys/kernel/dynamic_debug
TSC=/proc/sys/kernel/tsc
RESULTS_DIR=${RESULTS_DIR:-/ext2/results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
CSV_FILE="$RESULTS_DIR/workload/results.csv"
PER_ROUND_CSV="$RESULTS_DIR/workload/per_round.csv"

ROOT_DIR=${ROOT_DIR:-/ext2}
WORKDIR=${WORKDIR:-$ROOT_DIR/dyndbg_workload}
ITERS=${ITERS:-1000}
RUNS=${RUNS:-20}
WARMUP=${WARMUP:-10}
WORKLOAD_MODE=${WORKLOAD_MODE:-static}
CLK_TCK=${CLK_TCK:-}

TASK_CLOCK_MS=na
CONTEXT_SWITCHES=na
PAGE_FAULTS=na

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
      if (fields[12] ~ /^[0-9]+$/ && fields[13] ~ /^[0-9]+$/ && fields[14] ~ /^[0-9]+$/ && fields[15] ~ /^[0-9]+$/) {
        print fields[12] + fields[13] + fields[14] + fields[15];
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

# ── TSC-based timing ──────────────────────────────────────────────
# /proc/sys/kernel/tsc returns "<freq_hz> <current_tsc>" on one line.

read_tsc() {
  if [ ! -r "$TSC" ]; then
    echo "ERROR: $TSC not found (kernel may lack CONFIG_TSC procfs support)" >&2
    TSC_FREQ=0
    TSC_VAL=0
    return 1
  fi
  read -r TSC_FREQ TSC_VAL < "$TSC" 2>/dev/null || {
    TSC_FREQ=0
    TSC_VAL=0
    return 1
  }
}

tsc_to_us() {
  # Usage: tsc_to_us <tsc_start> <tsc_end> <freq>
  # Returns microseconds as integer (stdout).
  awk -v s="$1" -v e="$2" -v f="$3" '
    BEGIN {
      if (f > 0 && e > s)
        printf "%.0f", (e - s) * 1000000 / f;
      else
        printf "0";
    }'
}

# ── Statistics (computed from per-round data file) ─────────────────

compute_stats() {
  # Reads a file with one integer per line; outputs space-separated values
  # in fixed order:  n sum avg sd ci95 min max
  rounds_file=$1

  awk '
    {
      vals[NR] = $1 + 0;
      sum += $1;
      sumsq += $1 * $1;
      if (NR == 1) { min = $1; max = $1; }
      else {
        if ($1 < min) min = $1;
        if ($1 > max) max = $1;
      }
    }
    END {
      n = NR;
      if (n == 0) { print "0 0 0 0 0 0 0"; exit; }
      avg = sum / n;
      sd = 0;
      if (n > 1) {
        variance = (sumsq - sum * sum / n) / (n - 1);
        if (variance < 0) variance = 0;
        sd = sqrt(variance);
      }
      ci95 = 1.96 * sd / sqrt(n);
      printf "%d %.0f %.0f %.0f %.0f %.0f %.0f", n, sum, avg, sd, ci95, min, max;
    }' "$rounds_file"
}

run_rule() {
  echo "$1" > "$PROC"
}

ensure_csv() {
  if [ ! -e "$CSV_FILE" ]; then
    mkdir -p "$(dirname "$CSV_FILE")"
    echo "run_id,workload,workload_mode,avg_us,sd_us,ci95_us,min_us,max_us,task_clock_ms,context_switches,page_faults,iters,runs,rules" > "$CSV_FILE"
  fi
  if [ ! -e "$PER_ROUND_CSV" ]; then
    mkdir -p "$(dirname "$PER_ROUND_CSV")"
    echo "run_id,workload,workload_mode,round,duration_us" > "$PER_ROUND_CSV"
  fi
}

emit_result() {
  echo "RESULT $*"
}

read_uptime() {
  awk '{print $1}' /proc/uptime
}

prepare_workdir() {
  rm -rf "$WORKDIR"
  mkdir -p "$WORKDIR"
}

scenario_create_delete() {
  dir="$WORKDIR/create_delete"
  mkdir -p "$dir"

  i=1
  while [ "$i" -le "$ITERS" ]; do
    file="$dir/file_$i"
    : > "$file"
    rm -f "$file"
    i=$((i + 1))
  done
}

scenario_rename() {
  dir="$WORKDIR/rename"
  mkdir -p "$dir"

  i=1
  while [ "$i" -le "$ITERS" ]; do
    src="$dir/file_$i.a"
    dst="$dir/file_$i.b"
    : > "$src"
    mv "$src" "$dst"
    mv "$dst" "$src"
    rm -f "$src"
    i=$((i + 1))
  done
}

scenario_mkdir_rmdir() {
  dir="$WORKDIR/mkdir_rmdir"
  mkdir -p "$dir"

  i=1
  while [ "$i" -le "$ITERS" ]; do
    subdir="$dir/dir_$i"
    mkdir "$subdir"
    rmdir "$subdir"
    i=$((i + 1))
  done
}

scenario_pipe_comm() {
  i=1
  while [ "$i" -le "$ITERS" ]; do
    (
      printf '%s\n' "$i"
    ) | (
      read _payload
      :
    )
    i=$((i + 1))
  done
}

scenario_fork_wait() {
  i=1
  while [ "$i" -le "$ITERS" ]; do
    ( : ) &
    child_pid=$!
    wait "$child_pid"
    i=$((i + 1))
  done
}

scenario_dup_close() {
  i=1
  while [ "$i" -le "$ITERS" ]; do
    (
      exec 3>&1
      exec 4>&2
      exec 1>&3
      exec 2>&4
      exec 3>&-
      exec 4>&-
    )
    i=$((i + 1))
  done
}

workload_rules() {
  workload=$1

  case "$workload" in
    create_delete)
      echo "file=open.rs -p,file=close.rs -p,file=unlink.rs -p"
      ;;
    rename)
      echo "file=rename.rs -p"
      ;;
    mkdir_rmdir)
      echo "file=mkdir.rs -p,file=rmdir.rs -p"
      ;;
    pipe_comm)
      echo "file=pipe.rs -p,file=read.rs -p,file=write.rs -p"
      ;;
    fork_wait)
      echo "file=clone.rs -p,file=wait4.rs -p"
      ;;
    fd_dup_close)
      echo "file=dup.rs -p"
      ;;
    *)
      echo "unknown"
      ;;
  esac
}

apply_static_rules() {
  scenario=$1
  run_rule "clear"
  rules=$(workload_rules "$scenario")
  if [ "$rules" = "unknown" ]; then
    echo "unknown scenario: $scenario" >&2
    exit 1
  fi

  old_ifs=$IFS
  IFS=,
  set -- $rules
  IFS=$old_ifs
  for rule in "$@"; do
    run_rule "$rule"
  done
}

run_one_series() {
  scenario=$1
  runner=$2

  TASK_CLOCK_MS=na
  CONTEXT_SWITCHES=na
  PAGE_FAULTS=na
  total_task_clock_ms=0
  total_context_switches=0
  total_page_faults=0
  task_clock_ok=0
  context_ok=0
  page_fault_ok=0
  i=0

  # ── Warmup rounds (output discarded) ──────────────────────────
  if [ "$WARMUP" -gt 0 ]; then
    while [ "$i" -lt "$WARMUP" ]; do
      prepare_workdir
      ( "$runner" )
      i=$((i + 1))
    done
  fi

  # ── Measurement rounds: TSC timing per round ──────────────────
  rounds_tmp="${TMPDIR:-/tmp}/dyndbg_workload_rounds_$$.tmp"
  : > "$rounds_tmp"

  i=1
  while [ "$i" -le "$RUNS" ]; do
    prepare_workdir

    # Read TSC before
    if ! read_tsc; then
      echo "TSC read failed, falling back to uptime" >&2
      break
    fi
    tsc_start=$TSC_VAL
    tsc_freq=$TSC_FREQ

    # Soft metrics (optional, best-effort)
    collect_soft_metrics_begin

    # Run the workload
    ( "$runner" )

    collect_soft_metrics_end
    if [ "$TASK_CLOCK_MS" != "na" ]; then
      total_task_clock_ms=$((total_task_clock_ms + TASK_CLOCK_MS))
      task_clock_ok=1
    fi
    if [ "$CONTEXT_SWITCHES" != "na" ]; then
      total_context_switches=$((total_context_switches + CONTEXT_SWITCHES))
      context_ok=1
    fi
    if [ "$PAGE_FAULTS" != "na" ]; then
      total_page_faults=$((total_page_faults + PAGE_FAULTS))
      page_fault_ok=1
    fi

    # Read TSC after
    read_tsc || true
    tsc_end=$TSC_VAL

    # Duration in microseconds
    duration_us=$(tsc_to_us "$tsc_start" "$tsc_end" "$tsc_freq")

    echo "$duration_us" >> "$rounds_tmp"
    echo "$RUN_ID,$scenario,$WORKLOAD_MODE,$i,$duration_us" >> "$PER_ROUND_CSV"

    i=$((i + 1))
  done

  # ── Compute statistics ─────────────────────────────────────────
  if [ -s "$rounds_tmp" ]; then
    stats=$(compute_stats "$rounds_tmp")
    # Parse: n sum avg sd ci95 min max
    read -r _n _sum avg_us sd_us ci95_us min_us max_us <<STATS_EOF
$stats
STATS_EOF

    echo "STATS $scenario $WORKLOAD_MODE n=$_n avg_us=$avg_us sd_us=$sd_us ci95_us=$ci95_us min_us=$min_us max_us=$max_us"
  else
    avg_us=na sd_us=na ci95_us=na min_us=na max_us=na
    echo "STATS $scenario $WORKLOAD_MODE (no TSC data – falling back to uptime)"

    # Fallback: use uptime-based timing for backward compatibility
    start=$(read_uptime)
    i=1
    while [ "$i" -le "$RUNS" ]; do
      prepare_workdir
      ( "$runner" )
      i=$((i + 1))
    done
    end=$(read_uptime)
    avg_us=$(awk -v s="$start" -v e="$end" -v r="$RUNS" 'BEGIN{printf "%.0f", (e-s)*1e6/r}')
    sd_us=na ci95_us=na min_us=na max_us=na
  fi

  rm -f "$rounds_tmp"

  # Soft metric averages
  if [ "$task_clock_ok" -ne 0 ]; then
    TASK_CLOCK_MS=$((total_task_clock_ms / RUNS))
  else
    TASK_CLOCK_MS=na
  fi
  if [ "$context_ok" -ne 0 ]; then
    CONTEXT_SWITCHES=$((total_context_switches / RUNS))
  else
    CONTEXT_SWITCHES=na
  fi
  if [ "$page_fault_ok" -ne 0 ]; then
    PAGE_FAULTS=$((total_page_faults / RUNS))
  else
    PAGE_FAULTS=na
  fi
}

run_workload_case() {
  workload=$1
  runner=$2

  run_rule "clear"
  # 建结果目录必须在 run_one_series 写入 per_round.csv 之前（否则首轮
  # echo >> "$PER_ROUND_CSV" 报 nonexistent directory）。
  ensure_csv

  case "$WORKLOAD_MODE" in
    baseline)
      run_one_series "$workload" "$runner"
      rules="compiled_out"
      ;;
    branch)
      apply_static_rules "$workload"
      run_one_series "$workload" "$runner"
      rules=$(workload_rules "$workload")
      ;;
    static)
      apply_static_rules "$workload"
      run_one_series "$workload" "$runner"
      rules=$(workload_rules "$workload")
      ;;
    *)
      echo "unknown WORKLOAD_MODE: $WORKLOAD_MODE" >&2
      exit 1
      ;;
  esac

  emit_result "test=P-01R workload=$workload workload_mode=$WORKLOAD_MODE avg_us=$avg_us sd_us=$sd_us ci95_us=$ci95_us min_us=$min_us max_us=$max_us task_clock_ms=$TASK_CLOCK_MS context_switches=$CONTEXT_SWITCHES page_faults=$PAGE_FAULTS iters=$ITERS runs=$RUNS rules=$rules run_id=$RUN_ID"
  echo "$RUN_ID,$workload,$WORKLOAD_MODE,$avg_us,$sd_us,$ci95_us,$min_us,$max_us,$TASK_CLOCK_MS,$CONTEXT_SWITCHES,$PAGE_FAULTS,$ITERS,$RUNS,$rules" >> "$CSV_FILE"
  run_rule "clear"
}

init_clk_tck

if [ ! -e "$PROC" ]; then
  echo "dynamic_debug procfs file not found" >&2
  exit 1
fi

run_workload_case "create_delete" scenario_create_delete
run_workload_case "rename" scenario_rename
run_workload_case "mkdir_rmdir" scenario_mkdir_rmdir
run_workload_case "pipe_comm" scenario_pipe_comm
run_workload_case "fork_wait" scenario_fork_wait
run_workload_case "fd_dup_close" scenario_dup_close

echo "workload test finished"
