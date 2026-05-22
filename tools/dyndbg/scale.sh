#!/bin/sh
# Test: P-03 (descriptor scale benchmark; compare across builds)
set -eu

PROC=/proc/sys/kernel/dynamic_debug
STATS=/proc/sys/kernel/dyndbg_stats

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
UPDATE_ITERS=${UPDATE_ITERS:-1}
DESCRIPTORS=${DESCRIPTORS:-unknown}
RESULTS_DIR=${RESULTS_DIR:-results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
COMMIT=${COMMIT:-unknown}
PHASE=${PHASE:-unknown}
CSV_FILE="$RESULTS_DIR/scale/results.csv"

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
    echo "run_id,commit,phase,case,descriptors,update_iters,elapsed_s,descriptors_recomputed,modules_repatched,sites_patched" > "$CSV_FILE"
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

start=$(read_uptime)

i=0
while [ "$i" -lt "$UPDATE_ITERS" ]; do
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
emit_result "test=P-03 descriptors=$DESCRIPTORS update_iters=$UPDATE_ITERS elapsed_s=$elapsed descriptors_recomputed=$1 modules_repatched=$2 sites_patched=$3 run_id=$RUN_ID commit=$COMMIT phase=$PHASE"
echo "$RUN_ID,$COMMIT,$PHASE,P-03,$DESCRIPTORS,$UPDATE_ITERS,$elapsed,$1,$2,$3" >> "$CSV_FILE"

echo "scale test finished"
