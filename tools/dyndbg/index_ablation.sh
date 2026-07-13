#!/bin/sh
# Test: I-02 (Index ablation: compare index-based vs linear-scan candidate collection)
#
# Uses -p (disable) instead of +p to avoid static patching noise.
# With -p, all descriptors start disabled and stay disabled — no state transition,
# no NOP<->JMP patching. Only candidate collection + descriptor recomputation runs.
#
# Key insight: file/module/function indexes use full key-scan with substring match
# (O(num_unique_keys)), only line_index uses true BTreeMap O(log N) lookup.
# The ablation compares: index-guided collection vs O(all_descriptors) linear scan.
set -eu

PROC=/proc/sys/kernel/dynamic_debug
STATS=/proc/sys/kernel/dyndbg_stats
BENCH=/proc/sys/kernel/dyndbg_bench

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
FILE_KEY=${FILE_KEY:-dyndbg_bench.rs}
FUNC_KEY=${FUNC_KEY:-bench_log_0}
LINE_KEY=${LINE_KEY:-$(cat /etc/dyndbg_line.txt 2>/dev/null || echo "215")}
INDEX_ITERS=${INDEX_ITERS:-10000}
RESULTS_DIR=${RESULTS_DIR:-/ext2/results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
COMMIT=${COMMIT:-unknown}
PHASE=${PHASE:-unknown}
CSV_FILE="$RESULTS_DIR/index_ablation/results.csv"

if [ ! -e "$PROC" ] || [ ! -e "$STATS" ] || [ ! -e "$BENCH" ]; then
  echo "dyndbg procfs files not found" >&2
  exit 1
fi

run_rule() {
  echo "$1" > "$PROC"
}

reset_stats() {
  echo "reset" > "$STATS"
}

set_index() {
  echo "index=$1" > "$BENCH"
}

read_uptime() {
  awk '{print $1}' /proc/uptime
}

ensure_csv() {
  if [ ! -e "$CSV_FILE" ]; then
    mkdir -p "$(dirname "$CSV_FILE")"
    echo "run_id,case,index_state,selector,update_iters,elapsed_s,descriptors_recomputed,modules_repatched,sites_patched,status" > "$CSV_FILE"
  fi
}

emit_result() {
  echo "RESULT $*"
}

parse_stats() {
  stats_text=$1
  desc=$(echo "$stats_text" | awk -F= '/^descriptors_recomputed=/{print $2}')
  mods=$(echo "$stats_text" | awk -F= '/^modules_repatched=/{print $2}')
  sites=$(echo "$stats_text" | awk -F= '/^sites_patched=/{print $2}')
  echo "$desc $mods $sites"
}

run_ablation_case() {
  index_state=$1    # "on" or "off"
  selector=$2       # e.g. "module=dyndbg_bench -p"
  case_label=$3     # e.g. "I-02-index-on-module"

  set_index "$index_state"
  run_rule "clear"
  reset_stats

  start=$(read_uptime)

  i=0
  while [ "$i" -lt "$INDEX_ITERS" ]; do
    run_rule "$selector"
    run_rule "clear"
    i=$((i + 1))
  done

  end=$(read_uptime)
  elapsed=$(awk -v s="$start" -v e="$end" 'BEGIN{printf "%.6f", e-s}')

  stats_out=$(cat "$STATS")
  set -- $(parse_stats "$stats_out")
  descriptors_recomputed=${1:-na}
  modules_repatched=${2:-na}
  sites_patched=${3:-na}

  # With -p, no patching should occur: sites_patched should be 0.
  # descriptors_recomputed > 0 confirms the selector matched something.
  if [ "$descriptors_recomputed" = "na" ] || [ "$descriptors_recomputed" -eq 0 ] 2>/dev/null; then
    status="fail"
  else
    status="pass"
  fi

  ensure_csv
  emit_result "test=I-02 case=$case_label index=$index_state selector=\"$selector\" update_iters=$INDEX_ITERS elapsed_s=$elapsed descriptors_recomputed=$descriptors_recomputed modules_repatched=$modules_repatched sites_patched=$sites_patched status=$status run_id=$RUN_ID"
  echo "$RUN_ID,$case_label,$index_state,\"$selector\",$INDEX_ITERS,$elapsed,$descriptors_recomputed,$modules_repatched,$sites_patched,$status" >> "$CSV_FILE"
}

echo "=== Index Ablation Test (-p mode, no patching) ==="
echo "iters=$INDEX_ITERS module=$MODULE_KEY file=$FILE_KEY func=$FUNC_KEY line=$LINE_KEY"

# --- Full recompute baseline (selectorless -p): short-circuits to all_descriptors() ---
# A selectorless rule hits the fast-path in collect_candidates_for_rule_entries()
# and returns ALL descriptors (283), regardless of index on/off.
# This serves as the L0 baseline: full recompute, no candidate narrowing.
echo "--- full recompute baseline (selectorless -p, all descriptors) ---"
run_ablation_case "off" "-p"  "I-02-full-recompute"

# --- Line selector: true O(log N) BTreeMap lookup vs O(N) linear scan ---
echo "--- line selector (true BTreeMap lookup) ---"
run_ablation_case "on"  "line=$LINE_KEY -p"  "I-02-index-on-line"
run_ablation_case "off" "line=$LINE_KEY -p"  "I-02-index-off-line"

# --- File selector: key-scan index vs descriptor-scan ---
echo "--- file selector (key-scan, small match) ---"
run_ablation_case "on"  "file=$FILE_KEY -p"      "I-02-index-on-file"
run_ablation_case "off" "file=$FILE_KEY -p"      "I-02-index-off-file"

# --- Func selector: key-scan index vs descriptor-scan ---
# Uses a precise keyword (bench_log_0) to match a single descriptor,
# unlike module which matches all 65. Contrasts small vs large match sets
# under the same key-scan index mechanism.
echo "--- func selector (key-scan, small match) ---"
run_ablation_case "on"  "func=$FUNC_KEY -p"      "I-02-index-on-func"
run_ablation_case "off" "func=$FUNC_KEY -p"      "I-02-index-off-func"

# --- Module selector: key-scan index vs descriptor-scan ---
echo "--- module selector (key-scan, large match) ---"
run_ablation_case "on"  "module=$MODULE_KEY -p"  "I-02-index-on-module"
run_ablation_case "off" "module=$MODULE_KEY -p"  "I-02-index-off-module"

# Restore default
set_index "on"
run_rule "clear"

echo "=== Index Ablation Test Complete ==="
echo "CSV: $CSV_FILE"
