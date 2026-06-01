#!/bin/sh

# SPDX-License-Identifier: MPL-2.0

# TODO: This script simulates the process of mounting filesystems as performed by 
# a generic init process. It should later be replaced by the actual init process.
mount -t sysfs none /sys
mount -t proc none /proc
mount -t cgroup2 none /sys/fs/cgroup
mount -t configfs none /sys/kernel/config

mount_ext2_results() {
	for device in /dev/vda /dev/vdb; do
		if mount -t ext2 "$device" /ext2 2>/dev/null; then
			return 0
		fi
		done
	return 1
}

mkdir -p /results /ext2 /ext2/results
if mount -t 9p -o trans=virtio,version=9p2000.L results /results 2>/dev/null; then
	export RESULTS_DIR=/results
elif mount_ext2_results; then
	export RESULTS_DIR=/ext2/results
else
	export RESULTS_DIR=/results
fi