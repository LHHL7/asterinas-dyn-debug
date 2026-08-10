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

# MODULE_KEY: auto-detect multi-module synthetic mode (gen_synth_sites.py
# --modules M). In that mode `module=dyndbg_bench` would still match ALL
# sites (ancestor segment), defeating the non-uniform selectivity — the
# first synthetic module (bench_m0, ~N/M sites) is used instead. Env var
# always wins.
MODULE_KEY=${MODULE_KEY:-}
if [ -z "$MODULE_KEY" ]; then
  if cat "$PROC" 2>/dev/null | grep -q 'bench_m0'; then
    MODULE_KEY=bench_m0
  else
    MODULE_KEY=dyndbg_bench
  fi
fi
# FILE_KEY: synthetic sites use "bench_sites.rs"; real sites use "dyndbg_bench.rs".
# Prefer the env var; then try /etc/dyndbg_file.txt (set by Nix for synthetic sites);
# otherwise fall back to the original default.
_SYNTH_FILE=$(cat /etc/dyndbg_file.txt 2>/dev/null || echo "")
FILE_KEY=${FILE_KEY:-${_SYNTH_FILE:-dyndbg_bench.rs}}
# FUNC_KEY: descriptor stores the SHORT function name (macro strips the full
# path), so func is atomic exact on short names under the three-channel
# engine — a bare `bench_log_0` matches directly via channel 1 (O(log m)).
# Do NOT pass a wildcard here: it would switch the ablation to the
# wildcard-scan channel (O(m)) and distort the exact-lookup comparison.
FUNC_KEY=${FUNC_KEY:-bench_log_0}
LINE_KEY=${LINE_KEY:-$(cat /etc/dyndbg_line.txt 2>/dev/null || echo "215")}
INDEX_ITERS=${INDEX_ITERS:-10000}
# ROUNDS: number of independent repetitions of each ablation case.
# Each round runs the full INDEX_ITERS rule updates independently.
# Set to >=10 for statistically meaningful Table 1 data (mean/sd/CI95).
ROUNDS=${ROUNDS:-10}
RESULTS_DIR=${RESULTS_DIR:-/ext2/results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
COMMIT=${COMMIT:-unknown}
PHASE=${PHASE:-unknown}
CSV_FILE="$RESULTS_DIR/index_ablation/results.csv"
SUMMARY_CSV="$RESULTS_DIR/index_ablation/results_summary.csv"

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

set_recompute() {
  echo "recompute=$1" > "$BENCH"
}

# Also expose recompute for external control (default: incremental)
RECOMPUTE_MODE=${RECOMPUTE_MODE:-incremental}
export RECOMPUTE_MODE

read_uptime() {
  awk '{print $1}' /proc/uptime
}

ensure_csv() {
  if [ ! -e "$CSV_FILE" ]; then
    mkdir -p "$(dirname "$CSV_FILE")"
    echo "run_id,round,case,index_state,selector,update_iters,elapsed_s,descriptors_recomputed,modules_repatched,sites_patched,status" > "$CSV_FILE"
  fi
}

ensure_summary_csv() {
  if [ ! -e "$SUMMARY_CSV" ]; then
    mkdir -p "$(dirname "$SUMMARY_CSV")"
    echo "run_id,case,rounds,mean_elapsed_s,sd_elapsed_s,ci95_low_s,ci95_high_s,min_s,max_s,descriptors_recomputed,modules_repatched,sites_patched,status" > "$SUMMARY_CSV"
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

  round=1
  round_times=""
  all_pass=true

  while [ "$round" -le "$ROUNDS" ]; do
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
      all_pass=false
    else
      status="pass"
    fi

    ensure_csv
    emit_result "test=I-02 case=$case_label round=$round/$ROUNDS index=$index_state selector=\"$selector\" update_iters=$INDEX_ITERS elapsed_s=$elapsed descriptors_recomputed=$descriptors_recomputed modules_repatched=$modules_repatched sites_patched=$sites_patched status=$status run_id=$RUN_ID"
    echo "$RUN_ID,$round,$case_label,$index_state,\"$selector\",$INDEX_ITERS,$elapsed,$descriptors_recomputed,$modules_repatched,$sites_patched,$status" >> "$CSV_FILE"

    # Collect round times for summary stats (space-separated)
    if [ -z "$round_times" ]; then
      round_times="$elapsed"
    else
      round_times="$round_times $elapsed"
    fi

    round=$((round + 1))
  done

  # Compute summary statistics across all rounds
  _case_desc_recomputed="$descriptors_recomputed"
  _case_mods_repatched="$modules_repatched"
  _case_sites_patched="$sites_patched"
  _case_final_status="pass"
  if ! $all_pass; then
    _case_final_status="fail"
  fi

  summary=$(echo "$round_times" | awk '
    {
      n = NF
      sum = 0; sumsq = 0; min = $1; max = $1
      for (i = 1; i <= n; i++) {
        v = $i
        sum += v
        sumsq += v * v
        if (v < min) min = v
        if (v > max) max = v
      }
      mean = sum / n
      if (n > 1) {
        var = (sumsq - sum * sum / n) / (n - 1)
        sd = sqrt(var)
        # 95% CI using t-distribution approx (t_0.025 for df=9..29 ~ 2.26..2.05; use 2.0 as conservative proxy for >=10 rounds)
        t = (n >= 10) ? 2.262 : 2.776   # df=9 vs df=4
        if (n >= 30) t = 2.045
        if (n >= 60) t = 2.000
        ci_half = t * sd / sqrt(n)
        ci_low = mean - ci_half
        ci_high = mean + ci_half
      } else {
        sd = 0
        ci_low = mean
        ci_high = mean
      }
      printf "%.6f %.6f %.6f %.6f %.6f %.6f", mean, sd, ci_low, ci_high, min, max
    }')
  set -- $summary
  _mean=$1; _sd=$2; _ci_low=$3; _ci_high=$4; _min=$5; _max=$6

  ensure_summary_csv
  echo "$RUN_ID,$case_label,$ROUNDS,$_mean,$_sd,$_ci_low,$_ci_high,$_min,$_max,$_case_desc_recomputed,$_case_mods_repatched,$_case_sites_patched,$_case_final_status" >> "$SUMMARY_CSV"

  echo ""
  echo "=== Summary: $case_label ($ROUNDS rounds) ==="
  echo "  mean=${_mean}s  sd=${_sd}s  CI95=[${_ci_low}, ${_ci_high}]s  min=${_min}s  max=${_max}s"
  echo ""
}

echo "=== Index Ablation Test (-p mode, no patching) ==="
echo "iters=$INDEX_ITERS rounds=$ROUNDS module=$MODULE_KEY file=$FILE_KEY func=$FUNC_KEY line=$LINE_KEY"

# Ensure baseline state: recompute=incremental
set_recompute "incremental"

# --- Full recompute baseline: recompute=full bypasses candidate narrowing ---
# Uses the SAME module rule as L1/L2, but with recompute=full to force
# processing of all 283 descriptors (simulating Linux's fused O(n) behavior).
echo "--- full recompute baseline (recompute=full, module rule, all 283 descriptors) ---"
set_recompute "full"
run_ablation_case "off" "module=$MODULE_KEY -p"  "I-02-full-recompute"
set_recompute "$RECOMPUTE_MODE"   # restore default

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

# Restore defaults
set_recompute "incremental"
set_index "on"
run_rule "clear"

echo "=== Index Ablation Test Complete ==="
echo "Per-round CSV: $CSV_FILE"
echo "Summary CSV:   $SUMMARY_CSV"
