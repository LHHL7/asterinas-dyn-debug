#!/bin/sh

# SPDX-License-Identifier: MPL-2.0

set -eu

ROOT_DIR=$(cd "$(dirname "$0")/../.." && pwd)
IMAGE_PATH=${IMAGE_PATH:-$ROOT_DIR/test/initramfs/build/ext2.img}
HOST_RESULTS_DIR=${HOST_RESULTS_DIR:-$ROOT_DIR/results}
MOUNT_DIR=$(mktemp -d)
CSV_HEADER='run_id,case,status,expected,actual,iters,mode,duration_us,details'

rewrite_csv_if_needed() {
	csv_file=$1
	first_line=$(head -n 1 "$csv_file" 2>/dev/null || true)
	case "$first_line" in
		"$CSV_HEADER")
			return 0
			;;
		"run_id,case,status,iters,mode,duration_us,details")
			tmp_file="$csv_file.tmp.$$"
			{
				echo "$CSV_HEADER"
				tail -n +2 "$csv_file"
			} > "$tmp_file"
			mv "$tmp_file" "$csv_file"
			return 0
			;;
		*)
			return 0
			;;
	esac
}

cleanup() {
	if grep -q " $MOUNT_DIR " /proc/mounts 2>/dev/null; then
		umount "$MOUNT_DIR"
	fi
	rmdir "$MOUNT_DIR" 2>/dev/null || true
}

trap cleanup EXIT INT TERM

if [ ! -f "$IMAGE_PATH" ]; then
	echo "image not found: $IMAGE_PATH" >&2
	exit 1
fi

mkdir -p "$HOST_RESULTS_DIR"
mount -o loop "$IMAGE_PATH" "$MOUNT_DIR"

if [ -d "$MOUNT_DIR/results" ]; then
	cp -a "$MOUNT_DIR/results/." "$HOST_RESULTS_DIR/"
	if [ -f "$HOST_RESULTS_DIR/functional/results.csv" ]; then
		rewrite_csv_if_needed "$HOST_RESULTS_DIR/functional/results.csv"
	fi
else
	echo "no results directory found in $IMAGE_PATH" >&2
fi

echo "collected results into $HOST_RESULTS_DIR"