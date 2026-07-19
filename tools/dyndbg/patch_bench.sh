#!/bin/sh
# Test: P-02 (patch transaction benchmark; compare per-site vs batch in one run)
set -eu

PROC=/proc/sys/kernel/dynamic_debug
STATS=/proc/sys/kernel/dyndbg_stats

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
PATCH_ITERS=${PATCH_ITERS:-1000}
PATCH_MODE=${PATCH_MODE:-toggle}
PREPARE_BENCH=${PREPARE_BENCH:-0}
RESULTS_DIR=${RESULTS_DIR:-/ext2/results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
COMMIT=${COMMIT:-unknown}
PHASE=${PHASE:-unknown}
CSV_FILE="$RESULTS_DIR/patch_bench/results.csv"

if [ ! -e "$PROC" ] || [ ! -e "$STATS" ]; then
  echo "dyndbg procfs files not found" >&2
  exit 1
fi

run_rule() {
  echo "$1" > "$PROC"
}

reset_stats() {
  echo "reset" > "$STATS"
}

read_uptime() {
  awk '{print $1}' /proc/uptime
}

ensure_csv() {
  if [ ! -e "$CSV_FILE" ]; then
    mkdir -p "$(dirname "$CSV_FILE")"
    echo "run_id,case,patch_backend,patch_mode,patch_iters,expected_behavior,actual_behavior,status,elapsed_s,enable_elapsed_s,clear_elapsed_s,modules_repatched,sites_patched,patch_transactions" > "$CSV_FILE"
  fi
}

emit_result() {
  echo "RESULT $*"
}

expected_behavior=patch-transaction-completes
actual_behavior=patch-transaction-completes
status=pass

parse_stats() {
  stats_text=$1
  mods=$(echo "$stats_text" | awk -F= '/^modules_repatched=/{print $2}')
  sites=$(echo "$stats_text" | awk -F= '/^sites_patched=/{print $2}')
  txs=$(echo "$stats_text" | awk -F= '/^patch_transactions=/{print $2}')
  echo "$mods $sites $txs"
}

run_rule "clear"
reset_stats

run_case() {
  patch_backend=$1
  case_name=$2

  
  echo "backend=$patch_backend" > /proc/sys/kernel/dyndbg_bench

  if [ "$PREPARE_BENCH" -ne 0 ]; then
    echo "P-02 case=$case_name backend=$patch_backend: preparing bench via log_batch"
    echo "mode=log_batch iters=1" > /proc/sys/kernel/dyndbg_bench || true
  fi

  reset_stats
  start=$(read_uptime)
  enable_elapsed=0.000000
  clear_elapsed=0.000000

  i=0
  while [ "$i" -lt "$PATCH_ITERS" ]; do
    case "$PATCH_MODE" in
      enable)
        op_start=$(read_uptime)
        run_rule "module=$MODULE_KEY +p"
        op_end=$(read_uptime)
        op_elapsed=$(awk -v s="$op_start" -v e="$op_end" 'BEGIN{printf "%.6f", e-s}')
        enable_elapsed=$(awk -v a="$enable_elapsed" -v b="$op_elapsed" 'BEGIN{printf "%.6f", a+b}')
        ;;
      clear)
        op_start=$(read_uptime)
        run_rule "clear"
        op_end=$(read_uptime)
        op_elapsed=$(awk -v s="$op_start" -v e="$op_end" 'BEGIN{printf "%.6f", e-s}')
        clear_elapsed=$(awk -v a="$clear_elapsed" -v b="$op_elapsed" 'BEGIN{printf "%.6f", a+b}')
        ;;
      toggle)
        op_start=$(read_uptime)
        run_rule "module=$MODULE_KEY +p"
        op_end=$(read_uptime)
        op_elapsed=$(awk -v s="$op_start" -v e="$op_end" 'BEGIN{printf "%.6f", e-s}')
        enable_elapsed=$(awk -v a="$enable_elapsed" -v b="$op_elapsed" 'BEGIN{printf "%.6f", a+b}')
        op_start=$(read_uptime)
        run_rule "clear"
        op_end=$(read_uptime)
        op_elapsed=$(awk -v s="$op_start" -v e="$op_end" 'BEGIN{printf "%.6f", e-s}')
        clear_elapsed=$(awk -v a="$clear_elapsed" -v b="$op_elapsed" 'BEGIN{printf "%.6f", a+b}')
        ;;
      *)
        echo "invalid PATCH_MODE: $PATCH_MODE" >&2
        exit 1
        ;;
    esac
    i=$((i + 1))
  done

  end=$(read_uptime)
  elapsed=$(awk -v s="$start" -v e="$end" 'BEGIN{printf "%.6f", e-s}')

  stats_out=$(cat "$STATS")
  stats_values=$(parse_stats "$stats_out")
  set -- $stats_values
  modules_repatched=${1:-na}
  sites_patched=${2:-na}
  patch_transactions=${3:-na}

  if [ "$modules_repatched" = "na" ] || [ "$sites_patched" = "na" ] || [ "$patch_transactions" = "na" ]; then
    actual_behavior=stats-unavailable
    status=fail
  else
    actual_behavior=patch-transaction-completes
    status=pass
  fi

  ensure_csv
  emit_result "test=P-02 case=$case_name backend=$patch_backend patch_mode=$PATCH_MODE patch_iters=$PATCH_ITERS expected_behavior=$expected_behavior actual_behavior=$actual_behavior status=$status elapsed_s=$elapsed enable_elapsed_s=$enable_elapsed clear_elapsed_s=$clear_elapsed modules_repatched=$modules_repatched sites_patched=$sites_patched patch_transactions=$patch_transactions run_id=$RUN_ID"
  echo "$RUN_ID,$case_name,$patch_backend,$PATCH_MODE,$PATCH_ITERS,$expected_behavior,$actual_behavior,$status,$elapsed,$enable_elapsed,$clear_elapsed,$modules_repatched,$sites_patched,$patch_transactions" >> "$CSV_FILE"
}

PATCH_RUNS=${PATCH_RUNS:-10}
run_num=1
while [ "$run_num" -le "$PATCH_RUNS" ]; do
    run_case per_site "P-02-per-site-run${run_num}"
    run_case batch   "P-02-batch-run${run_num}"
    run_num=$((run_num + 1))
done

echo "patch bench finished (runs: $PATCH_RUNS)"
