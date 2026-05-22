#!/bin/sh
# Test: P-02 (patch transaction benchmark; compare across commits)
set -eu

PROC=/proc/sys/kernel/dynamic_debug
STATS=/proc/sys/kernel/dyndbg_stats

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
PATCH_ITERS=${PATCH_ITERS:-1000}
RESULTS_DIR=${RESULTS_DIR:-results}
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
    echo "run_id,commit,phase,case,patch_iters,elapsed_s,modules_repatched,sites_patched" > "$CSV_FILE"
  fi
}

emit_result() {
  echo "RESULT $*"
}

parse_stats() {
  stats_text=$1
  mods=$(echo "$stats_text" | awk -F= '/^modules_repatched=/{print $2}')
  sites=$(echo "$stats_text" | awk -F= '/^sites_patched=/{print $2}')
  echo "$mods $sites"
}

run_rule "clear"
reset_stats

start=$(read_uptime)

i=0
while [ "$i" -lt "$PATCH_ITERS" ]; do
  run_rule "module=$MODULE_KEY +p"
  run_rule "clear"
  i=$((i + 1))
  done

end=$(read_uptime)

elapsed=$(awk -v s="$start" -v e="$end" 'BEGIN{printf "%.6f", e-s}')

echo "elapsed_s=$elapsed"
stats_out=$(cat "$STATS")
echo "$stats_out"
set -- $(parse_stats "$stats_out")

ensure_csv
emit_result "test=P-02 patch_iters=$PATCH_ITERS elapsed_s=$elapsed modules_repatched=$1 sites_patched=$2 run_id=$RUN_ID commit=$COMMIT phase=$PHASE"
echo "$RUN_ID,$COMMIT,$PHASE,P-02,$PATCH_ITERS,$elapsed,$1,$2" >> "$CSV_FILE"

echo "patch bench finished"
