#!/bin/sh
# Test: P-05 (module-level patch reduction using stats)
set -eu

PROC=/proc/sys/kernel/dynamic_debug
STATS=/proc/sys/kernel/dyndbg_stats

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
RESULTS_DIR=${RESULTS_DIR:-/ext2/results}
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
    echo "run_id,case,rule_type,descriptors_recomputed,modules_repatched,sites_patched,expected_relation,actual_relation,status" > "$CSV_FILE"
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

relation_to_full() {
  full_desc=$1
  full_mods=$2
  full_sites=$3
  cur_desc=$4
  cur_mods=$5
  cur_sites=$6

  if [ "$cur_desc" -lt "$full_desc" ] && [ "$cur_mods" -lt "$full_mods" ] && [ "$cur_sites" -lt "$full_sites" ]; then
    echo "smaller smaller pass"
  else
    echo "not-smaller not-smaller fail"
  fi
}

run_rule "clear"
reset_stats

run_rule "+p"
full_stats=$(show_stats)
echo "$full_stats"
set -- $(parse_stats "$full_stats")
full_desc=$1
full_mods=$2
full_sites=$3

ensure_csv
emit_result "test=P-05 rule_type=full descriptors_recomputed=$full_desc modules_repatched=$full_mods sites_patched=$full_sites expected_relation=baseline actual_relation=baseline status=pass run_id=$RUN_ID"
echo "$RUN_ID,P-05,full,$full_desc,$full_mods,$full_sites,baseline,baseline,pass" >> "$CSV_FILE"

run_rule "clear"
reset_stats
run_rule "module=$MODULE_KEY +p"
module_stats=$(show_stats)
echo "$module_stats"
set -- $(parse_stats "$module_stats")
module_desc=$1
module_mods=$2
module_sites=$3

set -- $(relation_to_full "$full_desc" "$full_mods" "$full_sites" "$module_desc" "$module_mods" "$module_sites")
expected_relation=$1
actual_relation=$2
status=$3

emit_result "test=P-05 rule_type=module descriptors_recomputed=$module_desc modules_repatched=$module_mods sites_patched=$module_sites expected_relation=$expected_relation actual_relation=$actual_relation status=$status run_id=$RUN_ID"
echo "$RUN_ID,P-05,module,$module_desc,$module_mods,$module_sites,$expected_relation,$actual_relation,$status" >> "$CSV_FILE"

run_rule "clear"

echo "patch reduction test finished"
