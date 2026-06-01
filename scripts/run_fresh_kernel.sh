#!/bin/sh
set -eu
# Helper: stop qemu, remove old initramfs artifacts, and start a fresh kernel run
# Usage: ./scripts/run_fresh_kernel.sh [make-args]

echo "Stopping any running qemu-system instances..."
pkill -f qemu-system || true

echo "Removing old boot artifacts..."
rm -rf test/initramfs/build/initramfs
rm -f test/initramfs/build/initramfs.cpio
rm -f test/initramfs/build/initramfs.cpio.gz
rm -f test/initramfs/build/ext2.img

echo "Starting fresh kernel (this may take several minutes)..."
make run_kernel "$@"

echo "Kernel start requested. Wait for guest to boot, then verify script versions inside guest:" \
     "md5sum /test/dyndbg/workload.sh  && md5sum tools/dyndbg/workload.sh"
