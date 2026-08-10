#!/bin/sh
# Test: I-03 (refresh-algorithm chain-length decoupling) + EQ-01..EQ-04
#       (incremental append vs full-replay state equivalence)
#
# Positioning: with the append-only incremental refresh path (48a02707),
# appending a rule costs O(k) regardless of the rule-chain length L,
# while deleting a rule forces a full chain replay costing O(L*k).
# Same rule (module=$MODULE_KEY -p), same hit set k — only the refresh
# algorithm and the chain length vary.
#
# Replaces the retired I-01 (whose hit-set-conflating design was abandoned;
# see 测试文档.md history note).
set -eu

PROC=/proc/sys/kernel/dynamic_debug
BENCH=/proc/sys/kernel/dyndbg_bench
STATS=/proc/sys/kernel/dyndbg_stats

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
DUMMY_MODULE=${DUMMY_MODULE:-__nonexistent__}
CHAIN_LENS=${CHAIN_LENS:-"1 10 100 1000 5000"}
ROUNDS=${ROUNDS:-10}
ITERS=${ITERS:-100}
BENCH_ENABLED_MARGIN_US=${BENCH_ENABLED_MARGIN_US:-1000}
APPEND_GROWTH_MAX=${APPEND_GROWTH_MAX:-3}
DEL_GROWTH_MIN=${DEL_GROWTH_MIN:-2}
RESULTS_DIR=${RESULTS_DIR:-/ext2/results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
CSV_FILE="$RESULTS_DIR/incremental/results.csv"
CSV_HEADER='run_id,case,chain_len,append_desc,del_desc,append_lat_avg_us,del_lat_avg_us,full_desc,full_lat_us,status,details'

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

stats_field() {
  cat "$STATS" | awk -F= "/^$1=/{print \$2; exit}"
}

ensure_csv() {
  mkdir -p "$(dirname "$CSV_FILE")"
  if [ ! -e "$CSV_FILE" ]; then
    echo "$CSV_HEADER" > "$CSV_FILE"
    return
  fi
  if [ "$(head -n 1 "$CSV_FILE")" != "$CSV_HEADER" ]; then
    mv "$CSV_FILE" "$CSV_FILE.legacy.$RUN_ID" 2>/dev/null || rm -f "$CSV_FILE"
    echo "$CSV_HEADER" > "$CSV_FILE"
  fi
}

record_case() {
  case_id=$1
  chain_len=$2
  status=$3
  details=$4
  append_desc=$5
  del_desc=$6
  append_lat=$7
  del_lat=$8
  full_desc=$9
  full_lat=$10
  ensure_csv
  echo "RESULT test=$case_id chain_len=$chain_len status=$status append_desc=$append_desc del_desc=$del_desc append_lat_avg_us=$append_lat del_lat_avg_us=$del_lat full_desc=$full_desc full_lat_us=$full_lat run_id=$RUN_ID details=$details"
  echo "$RUN_ID,$case_id,$chain_len,$append_desc,$del_desc,$append_lat,$del_lat,$full_desc,$full_lat,$status,$details" >> "$CSV_FILE"
}

bench_duration() {
  echo "mode=log iters=$ITERS" > "$BENCH"
  cat "$BENCH" | awk -F= '/^last_duration_us=/{print $2; exit}'
}

desc_section() {
  cat "$PROC" | sed -n '/^descriptors=/,$p' | sed '/^usage:/,$d'
}

# ---------- EQ: state equivalence between incremental append and full replay ----------

run_rule "clear"
DISABLED_BASE_US=$(bench_duration)

enabled() {
  d=$(bench_duration)
  [ -n "$d" ] && [ "$d" -gt $((DISABLED_BASE_US + BENCH_ENABLED_MARGIN_US)) ]
}

# EQ-01: last-match-wins preserved on append
run_rule "clear"
run_rule "module=$MODULE_KEY +p"
run_rule "module=$MODULE_KEY -p"
if enabled; then st1=fail; else st1=pass; fi
run_rule "module=$MODULE_KEY +p"
if enabled; then st2=pass; else st2=fail; fi
if [ "$st1" = "pass" ] && [ "$st2" = "pass" ]; then
  record_case "EQ-01" "-" pass "last-match-wins on append" "-" "-" "-" "-" "-" "-"
else
  record_case "EQ-01" "-" fail "last-match-wins on append (-p then +p)" "-" "-" "-" "-" "-" "-"
fi

# EQ-02: flags-only append must not flip the switch
run_rule "clear"
run_rule "module=$MODULE_KEY +p"
run_rule "module=$MODULE_KEY +f"
if enabled; then
  record_case "EQ-02" "-" pass "flags-only keeps switch" "-" "-" "-" "-" "-" "-"
else
  record_case "EQ-02" "-" fail "flags-only keeps switch" "-" "-" "-" "-" "-" "-"
fi

# EQ-03: del (replay path) produces the correct new state
run_rule "clear"
run_rule "module=$MODULE_KEY +p"
run_rule "module=$MODULE_KEY -p"
run_rule "del 1"
if enabled; then
  record_case "EQ-03" "-" pass "del restores +p" "-" "-" "-" "-" "-" "-"
else
  record_case "EQ-03" "-" fail "del restores +p" "-" "-" "-" "-" "-" "-"
fi

# EQ-04: incremental and full recompute end with identical descriptor states
apply_chain() {
  run_rule "clear"
  run_rule "module=$MODULE_KEY +p"
  run_rule "func=bench_log -p"
  run_rule "module=$MODULE_KEY +trace"
}
echo "recompute=incremental" > "$BENCH"
apply_chain
inc_state=$(desc_section)
echo "recompute=full" > "$BENCH"
apply_chain
full_state=$(desc_section)
echo "recompute=incremental" > "$BENCH"
if [ "$inc_state" = "$full_state" ]; then
  record_case "EQ-04" "-" pass "incremental == full state" "-" "-" "-" "-" "-" "-"
else
  record_case "EQ-04" "-" fail "incremental == full state (diff in descriptor columns)" "-" "-" "-" "-" "-" "-"
fi

# ---------- I-03: chain-length decoupling ----------

# Reference k: hit-set size for the real rule (measured at chain length 0).
run_rule "clear"
reset_stats
run_rule "module=$MODULE_KEY -p"
K=$(stats_field descriptors_recomputed)

# Full-recompute control (candidate set inflates to all descriptors).
run_rule "clear"
echo "recompute=full" > "$BENCH"
reset_stats
run_rule "module=$MODULE_KEY -p"
FULL_DESC=$(stats_field descriptors_recomputed)
FULL_LAT=$(stats_field last_update_latency_us)
echo "recompute=incremental" > "$BENCH"

first_append_lat=""
first_del_lat=""
first_iter=1
overall=pass
for L in $CHAIN_LENS; do
  run_rule "clear"
  i=0
  while [ "$i" -lt "$L" ]; do
    run_rule "module=$DUMMY_MODULE -p"
    i=$((i + 1))
  done

  append_lat_sum=0
  del_lat_sum=0
  append_desc_all="$K"
  del_desc_all="$K"
  round=0
  while [ "$round" -lt "$ROUNDS" ]; do
    # recomputed is a cumulative counter: reset before each measured op
    reset_stats
    run_rule "module=$MODULE_KEY -p"        # append real rule (id = L)
    append_desc_all=$(stats_field descriptors_recomputed)
    a_lat=$(stats_field last_update_latency_us)
    reset_stats
    run_rule "del $L"                        # replay path over the same k
    del_desc_all=$(stats_field descriptors_recomputed)
    d_lat=$(stats_field last_update_latency_us)
    append_lat_sum=$((append_lat_sum + a_lat))
    del_lat_sum=$((del_lat_sum + d_lat))
    round=$((round + 1))
  done
  append_lat_avg=$((append_lat_sum / ROUNDS))
  del_lat_avg=$((del_lat_sum / ROUNDS))

  if [ "$first_iter" -eq 1 ]; then
    first_append_lat=$append_lat_avg
    first_del_lat=$del_lat_avg
    first_iter=0
  fi

  # Assertions
  if [ "$append_desc_all" != "$K" ] || [ "$del_desc_all" != "$K" ]; then
    st=fail
    detail="desc mismatch k=$K append=$append_desc_all del=$del_desc_all"
  elif [ "$append_lat_avg" -gt $((first_append_lat * APPEND_GROWTH_MAX)) ]; then
    st=fail
    detail="append latency grew with chain (lat=$append_lat_avg first=$first_append_lat)"
  else
    st=pass
    detail="append flat (k=$K recomputed, lat=$append_lat_avg) del grows (lat=$del_lat_avg)"
  fi
  if [ "$st" = "fail" ]; then
    overall=fail
  fi
  record_case "I-03" "$L" "$st" "$detail" "$append_desc_all" "$del_desc_all" "$append_lat_avg" "$del_lat_avg" "$FULL_DESC" "$FULL_LAT"
done

# Cross-chain-len trend: del must grow with L (compare last vs first chain length)
if [ "$overall" = "pass" ]; then
  last_del_lat=""
  for L in $CHAIN_LENS; do :; done
  # re-derive last del latency from CSV rows of this run
  last_del_lat=$(grep "^$RUN_ID,I-03,$L," "$CSV_FILE" | tail -n 1 | cut -d, -f7)
  if [ -n "$last_del_lat" ] && [ -n "$first_del_lat" ] && [ "$last_del_lat" -gt $((first_del_lat * DEL_GROWTH_MIN)) ]; then
    record_case "I-03-trend" "-" pass "del latency grows with chain length" "-" "-" "$first_del_lat" "$last_del_lat" "$FULL_DESC" "$FULL_LAT"
  else
    record_case "I-03-trend" "-" fail "del latency grows with chain length (first=$first_del_lat last=$last_del_lat)" "-" "-" "$first_del_lat" "$last_del_lat" "$FULL_DESC" "$FULL_LAT"
  fi
fi

run_rule "clear"
echo "incremental test finished"
