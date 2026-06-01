# Dyndbg Fresh Run

Use the helper script to keep the workflow stable and avoid stale guest scripts:

```sh
./scripts/run_fresh_kernel.sh ENABLE_KVM=0 SMP=4 INITRAMFS_SKIP_GZIP=1 LOG_LEVEL=debug SYSCALL_LOG=off NO_DEFAULT_FEATURES=1 FEATURES=cvm_guest
```

After the guest boots, verify the packaged script before running tests:

```sh
md5sum /test/dyndbg/workload.sh
```

It should match the repository copy:

```sh
md5sum tools/dyndbg/workload.sh
```

If the values differ, stop and rebuild with the helper script again before collecting results.

The helper only removes cached boot artifacts, not the whole build tree, so it stays fast while still forcing a fresh packaged script.
