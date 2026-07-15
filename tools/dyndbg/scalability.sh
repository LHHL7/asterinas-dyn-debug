#!/bin/sh
# Scalability test: measure index ablation speedup for varying descriptor counts.
#
# Usage (run inside the project dir on the server):
#   sh tools/dyndbg/scalability.sh
#
# For each N in SCALE_POINTS, this script:
#   1. Generates bench_sites.rs with N synthetic descriptors
#   2. Copies it into the kernel source tree
#   3. Prints the build+boot+test commands you need to run
#
# After all builds complete, results are in /ext2/results/scalability/ on the guest.

set -eu

SCRIPT_DIR=$(dirname "$0")
PROJECT_DIR=$(cd "$SCRIPT_DIR/../.." && pwd)
SITES_FILE="$PROJECT_DIR/kernel/src/fs/fs_impls/procfs/sys/kernel/dyndbg_bench/bench_sites.rs"
BACKUP_FILE="$SITES_FILE.bak"

# Scale points to test (descriptor count)
SCALE_POINTS=${SCALE_POINTS:-"283 500 1000 2000"}
GEN_SCRIPT="$SCRIPT_DIR/gen_synth_sites.py"

echo "============================================"
echo "  Dyndbg Index Ablation — Scalability Test"
echo "============================================"
echo ""
echo "Scale points: $SCALE_POINTS"
echo "Project dir:  $PROJECT_DIR"
echo ""

# Backup original
if [ ! -f "$BACKUP_FILE" ]; then
    cp "$SITES_FILE" "$BACKUP_FILE"
    echo "[OK] Backed up: $SITES_FILE → $BACKUP_FILE"
else
    echo "[OK] Backup already exists: $BACKUP_FILE"
fi

# Phase 1: Generate bench_sites.rs for each N
echo ""
echo "=== Phase 1: Generate source files ==="
echo ""

for N in $SCALE_POINTS; do
    echo "--- N=$N descriptors ---"
    python3 "$GEN_SCRIPT" "$N" > "$SITES_FILE"

    # Count actual generated entries
    actual=$(grep -c "bench_log_" "$SITES_FILE" || true)
    echo "  Generated $actual entries (requested $N)"

    if [ "$actual" -ne "$N" ]; then
        echo "  WARNING: count mismatch!"
    fi
done

# Restore original
cp "$BACKUP_FILE" "$SITES_FILE"
echo ""
echo "[OK] Restored original bench_sites.rs"

# Phase 2: Print instructions
echo ""
echo "============================================"
echo "  How to run"
echo "============================================"
echo ""
echo "On the remote server (inside Docker container):"
echo ""

i=1
for N in $SCALE_POINTS; do
    echo "--- Step $i: N=$N ---"
    echo ""
    echo "  # 1. Generate bench_sites.rs"
    echo "  python3 tools/dyndbg/gen_synth_sites.py $N > kernel/src/fs/fs_impls/procfs/sys/kernel/dyndbg_bench/bench_sites.rs"
    echo ""
    echo "  # 2. Build and boot kernel"
    echo "  make run_kernel LOG_LEVEL=debug SYSCALL_INFO=off RELEASE=1 MEM=16G"
    echo ""
    echo "  # 3. Inside guest, run index ablation test"
    echo "  RESULTS_DIR=/ext2/results/scalability INDEX_ITERS=10000 sh /path/to/index_ablation.sh"
    echo ""
    echo "  # 4. Collect per-round data (optional)"
    echo "  cat /ext2/results/scalability/index_ablation/results.csv"
    echo ""
    echo "  # 5. Shutdown guest"
    echo "  poweroff"
    echo ""
    i=$((i + 1))
done

echo "--- After all runs ---"
echo ""
echo "  # Restore original bench_sites.rs"
echo "  cp kernel/src/fs/fs_impls/procfs/sys/kernel/dyndbg_bench/bench_sites.rs.bak \\"
echo "     kernel/src/fs/fs_impls/procfs/sys/kernel/dyndbg_bench/bench_sites.rs"
echo ""
echo "============================================"
echo "  Results location"
echo "============================================"
echo ""
echo "  Inside guest: /ext2/results/scalability/index_ablation/results.csv"
echo "  Expected columns: run_id, case, index_state, selector, update_iters, elapsed_s, ..."
echo ""
echo "  For speedup calculation (per N):"
echo "    speedup = elapsed_s(index=off) / elapsed_s(index=on)"
