#!/bin/sh
# Test: I-01 (incremental recompute vs full recompute stats)
set -eu

PROC=/proc/sys/kernel/dynamic_debug
STATS=/proc/sys/kernel/dyndbg_stats

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
RESULTS_DIR=${RESULTS_DIR:-/ext2/results}
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
    echo "run_id,case,full_desc,module_desc,full_mods,module_mods,full_sites,module_sites,incremental_status,patch_reduction_status" > "$CSV_FILE"
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
  inc_desc=$2

  if [ "$inc_desc" -le "$full_desc" ]; then
    echo "less-or-equal less-or-equal pass"
  else
    echo "greater greater fail"
  fi
}

relation_to_full_three() {
  full_desc=$1
  full_mods=$2
  full_sites=$3
  cur_desc=$4
  cur_mods=$5
  cur_sites=$6

  if [ "$cur_desc" -le "$full_desc" ] && [ "$cur_mods" -le "$full_mods" ] && [ "$cur_sites" -le "$full_sites" ]; then
    echo "less-or-equal less-or-equal pass"
  else
    echo "greater greater fail"
  fi
}

# Baseline: selectorless rule (full recompute)
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
emit_result "baseline: descriptors_recomputed=$full_desc modules_repatched=$full_mods sites_patched=$full_sites run_id=$RUN_ID"
# baseline recorded; CSV line will be emitted after module run (combined record)

# Incremental: module selector
run_rule "clear"
reset_stats
run_rule "module=$MODULE_KEY +p"
inc_stats=$(show_stats)
echo "$inc_stats"
set -- $(parse_stats "$inc_stats")
inc_desc=$1
inc_mods=$2
inc_sites=$3

# Compute I-01 (incremental) status
set -- $(relation_to_full "$full_desc" "$inc_desc")
inc_expected_relation=$1
inc_actual_relation=$2
inc_status=$3

# Compute P-05 (patch reduction) status (three-field comparison)
set -- $(relation_to_full_three "$full_desc" "$full_mods" "$full_sites" "$inc_desc" "$inc_mods" "$inc_sites")
p05_expected_relation=$1
p05_actual_relation=$2
p05_status=$3


# Emit combined human-readable summary and write single CSV row
emit_result "combined: full_desc=$full_desc module_desc=$inc_desc full_mods=$full_mods module_mods=$inc_mods full_sites=$full_sites module_sites=$inc_sites incremental_status=$inc_status patch_reduction_status=$p05_status run_id=$RUN_ID"
echo "$RUN_ID,I-01,$full_desc,$inc_desc,$full_mods,$inc_mods,$full_sites,$inc_sites,$inc_status,$p05_status" >> "$CSV_FILE"

echo "incremental test finished"
