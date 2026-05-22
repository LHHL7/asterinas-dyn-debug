#!/bin/sh
# Test: I-01 (incremental recompute vs full recompute stats)
set -eu

PROC=/proc/sys/kernel/dynamic_debug
STATS=/proc/sys/kernel/dyndbg_stats

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
RESULTS_DIR=${RESULTS_DIR:-results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
COMMIT=${COMMIT:-unknown}
PHASE=${PHASE:-unknown}
CSV_FILE="$RESULTS_DIR/incremental/results.csv"

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
    echo "run_id,commit,phase,case,rule_type,descriptors_recomputed,modules_repatched,sites_patched" > "$CSV_FILE"
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

# Baseline: selectorless rule (full recompute)
run_rule "clear"
reset_stats
run_rule "+p"
full_stats=$(show_stats)
echo "$full_stats"
set -- $(parse_stats "$full_stats")
ensure_csv
emit_result "test=I-01 rule_type=full descriptors_recomputed=$1 modules_repatched=$2 sites_patched=$3 run_id=$RUN_ID commit=$COMMIT phase=$PHASE"
echo "$RUN_ID,$COMMIT,$PHASE,I-01,full,$1,$2,$3" >> "$CSV_FILE"

# Incremental: module selector
run_rule "clear"
reset_stats
run_rule "module=$MODULE_KEY +p"
inc_stats=$(show_stats)
echo "$inc_stats"
set -- $(parse_stats "$inc_stats")
emit_result "test=I-01 rule_type=module descriptors_recomputed=$1 modules_repatched=$2 sites_patched=$3 run_id=$RUN_ID commit=$COMMIT phase=$PHASE"
echo "$RUN_ID,$COMMIT,$PHASE,I-01,module,$1,$2,$3" >> "$CSV_FILE"

echo "incremental test finished"
