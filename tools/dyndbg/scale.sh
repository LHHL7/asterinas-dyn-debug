#!/bin/sh
# Test: P-03 (auxiliary scale observation; compare across update sizes)
set -eu

PROC=/proc/sys/kernel/dynamic_debug
STATS=/proc/sys/kernel/dyndbg_stats

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
UPDATE_ITERS=${UPDATE_ITERS:-1}
UPDATE_ITERS_LIST=${UPDATE_ITERS_LIST:-}
DESCRIPTORS=${DESCRIPTORS:-unknown}
RESULTS_DIR=${RESULTS_DIR:-/ext2/results}
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
    echo "run_id,case,descriptors,update_iters,elapsed_s,descriptors_recomputed,modules_repatched,sites_patched,expected_trend,actual_trend,status" > "$CSV_FILE"
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

normalize_points() {
  echo "$1" | tr ',' ' '
}

compare_elapsed() {
  previous_elapsed=$1
  current_elapsed=$2

  awk -v prev="$previous_elapsed" -v cur="$current_elapsed" 'BEGIN {
    if (prev == "") {
      print "baseline baseline pass";
    } else if (cur >= prev * 0.95) {
      print "grow grow pass";
    } else {
      print "grow shrink fail";
    }
  }'
}

run_scale_case() {
  update_iters=$1
  previous_elapsed=$2

  run_rule "clear"
  reset_stats

  start=$(read_uptime)

  i=0
  while [ "$i" -lt "$update_iters" ]; do
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
  descriptors_recomputed=$1
  modules_repatched=$2
  sites_patched=$3

  set -- $(compare_elapsed "$previous_elapsed" "$elapsed")
  expected_trend=$1
  actual_trend=$2
  status=$3

  ensure_csv
  emit_result "test=P-03 descriptors=$DESCRIPTORS update_iters=$update_iters elapsed_s=$elapsed descriptors_recomputed=$descriptors_recomputed modules_repatched=$modules_repatched sites_patched=$sites_patched expected_trend=$expected_trend actual_trend=$actual_trend status=$status run_id=$RUN_ID"
  echo "$RUN_ID,P-03,$DESCRIPTORS,$update_iters,$elapsed,$descriptors_recomputed,$modules_repatched,$sites_patched,$expected_trend,$actual_trend,$status" >> "$CSV_FILE"

  last_elapsed=$elapsed
}

previous_elapsed=""
points=$(normalize_points "${UPDATE_ITERS_LIST:-$UPDATE_ITERS}")
for update_iters in $points; do
  run_scale_case "$update_iters" "$previous_elapsed"
  previous_elapsed=$last_elapsed
done

echo "scale test finished"
