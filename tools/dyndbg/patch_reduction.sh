#!/bin/sh
# Test: P-05 (module-level patch reduction using stats)
set -eu

PROC=/proc/sys/kernel/dynamic_debug
STATS=/proc/sys/kernel/dyndbg_stats

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
RESULTS_DIR=${RESULTS_DIR:-results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
COMMIT=${COMMIT:-unknown}
PHASE=${PHASE:-unknown}
CSV_FILE="$RESULTS_DIR/patch_reduction/results.csv"

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

show_stats() {
  cat "$STATS"
}

ensure_csv() {
  if [ ! -e "$CSV_FILE" ]; then
    mkdir -p "$(dirname "$CSV_FILE")"
    echo "run_id,commit,phase,case,modules_repatched,sites_patched,descriptors_recomputed" > "$CSV_FILE"
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

run_rule "clear"
reset_stats
run_rule "module=$MODULE_KEY +p"
stats_out=$(show_stats)
echo "$stats_out"
set -- $(parse_stats "$stats_out")

ensure_csv
emit_result "test=P-05 modules_repatched=$2 sites_patched=$3 descriptors_recomputed=$1 run_id=$RUN_ID commit=$COMMIT phase=$PHASE"
echo "$RUN_ID,$COMMIT,$PHASE,P-05,$2,$3,$1" >> "$CSV_FILE"

run_rule "clear"

echo "patch reduction test finished"
