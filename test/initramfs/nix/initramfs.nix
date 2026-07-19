{ lib, stdenvNoCC, fetchFromGitHub, hostPlatform, writeClosure, busybox, apps
, benchmark, syscall, dnsServer, pkgs }:
let
  boot_hello = builtins.path { path = ./../src/boot_hello.sh; };
  dyndbg_tools = builtins.path { path = ./../../../tools/dyndbg; };
  # Scan dyndbg_bench.rs for the first "aster_logger::dyndbg_debug!" call site.
  dyndbg_bench_rs = builtins.readFile ./../../../kernel/src/fs/fs_impls/procfs/sys/kernel/dyndbg_bench.rs;
  dyndbg_bench_lines = lib.splitString "\n" dyndbg_bench_rs;
  findLine = lines: lineNo:
    if lines == [] then null else if lib.hasInfix "aster_logger::dyndbg_debug!" (builtins.head lines) then
      lineNo
    else
      findLine (builtins.tail lines) (lineNo + 1);
  dyndbg_bench_line = findLine dyndbg_bench_lines 1;

  # Also scan bench_sites.rs for the first synthetic entry (bench_log_0).
  # This is always on a fixed line (32) regardless of N, but we detect it
  # dynamically to stay robust against HEADER changes.
  dyndbg_sites_rs = builtins.readFile ./../../../kernel/src/fs/fs_impls/procfs/sys/kernel/dyndbg_bench/bench_sites.rs;
  dyndbg_sites_lines = lib.splitString "\n" dyndbg_sites_rs;
  findSiteLine = lines: lineNo:
    if lines == [] then null else if lib.hasInfix "bench_log_0 =>" (builtins.head lines) then
      lineNo
    else
      findSiteLine (builtins.tail lines) (lineNo + 1);
  dyndbg_sites_line = findSiteLine dyndbg_sites_lines 1;

  # Prefer synthetic sites' line if present; otherwise fall back to dyndbg_bench.rs.
  dyndbg_line = if dyndbg_sites_line != null then dyndbg_sites_line else dyndbg_bench_line;
  dyndbg_file = if dyndbg_sites_line != null then "bench_sites.rs" else "dyndbg_bench.rs";
  etc = lib.fileset.toSource {
    root = ./../src/etc;
    fileset = ./../src/etc;
  };
  gvisor_libs = if syscall != null && syscall.testSuite == "gvisor" then
    builtins.path {
      name = "gvisor-libs";
      path = "/lib/x86_64-linux-gnu";
    }
  else
    null;
  resolv_conf = pkgs.callPackage ./resolv_conf.nix { dnsServer = dnsServer; };
  # Whether the initramfs should include evtest, a common tool to debug input devices (`/dev/input/eventX`)
  is_evtest_included = false;
  all_pkgs = [ busybox etc resolv_conf ]
    ++ lib.optionals (apps != null) [ apps.package ]
    ++ lib.optionals (benchmark != null) [ benchmark.package ]
    ++ lib.optionals (syscall != null) [ syscall.package ]
    ++ lib.optionals is_evtest_included [ pkgs.evtest ];
in stdenvNoCC.mkDerivation {
  name = "initramfs";
  buildCommand = ''
    mkdir -p $out/{dev,etc,root,usr,opt,tmp,var,proc,sys}
    mkdir -p $out/{benchmark,test,ext2,exfat,results}
    mkdir -p $out/usr/{bin,sbin,lib,lib64,local}
    ln -sfn usr/bin $out/bin
    ln -sfn usr/sbin $out/sbin
    ln -sfn usr/lib $out/lib
    ln -sfn usr/lib64 $out/lib64
    cp -r ${busybox}/bin/* $out/bin/
    ${lib.optionalString is_evtest_included ''
      cp -r ${pkgs.evtest}/bin/* $out/bin/
    ''}

    cp ${boot_hello} $out/test/boot_hello.sh
    mkdir -p $out/test/dyndbg
    cp -r ${dyndbg_tools}/* $out/test/dyndbg/
    echo ${toString (if dyndbg_line == null then 196 else dyndbg_line)} > $out/etc/dyndbg_line.txt
    echo ${toString dyndbg_file} > $out/etc/dyndbg_file.txt

    cp -r ${etc}/* $out/etc/

    cp ${resolv_conf}/resolv.conf $out/etc/

    ${lib.optionalString (apps != null) ''
      cp -r ${apps.package}/* $out/test/
    ''}

    ${lib.optionalString (benchmark != null) ''
      cp -r "${benchmark.package}"/* $out/benchmark/
    ''}

    ${lib.optionalString (syscall != null) ''
      cp -r "${syscall.package}"/opt/* $out/opt/
    ''}

    ${lib.optionalString (syscall != null && syscall.testSuite == "gvisor") ''
      # FIXME: Build gvisor syscall test with nix to avoid manual library copying.
      mkdir -p $out/lib/x86_64-linux-gnu
      cp -L ${gvisor_libs}/ld-linux-x86-64.so.2 $out/lib64/ld-linux-x86-64.so.2
      cp -L ${gvisor_libs}/libstdc++.so.6 $out/lib/x86_64-linux-gnu/libstdc++.so.6
      cp -L ${gvisor_libs}/libgcc_s.so.1 $out/lib/x86_64-linux-gnu/libgcc_s.so.1
      cp -L ${gvisor_libs}/libc.so.6 $out/lib/x86_64-linux-gnu/libc.so.6
      cp -L ${gvisor_libs}/libm.so.6 $out/lib/x86_64-linux-gnu/libm.so.6
    ''}

    # Use `writeClosure` to retrieve all dependencies of the specified packages.
    # This will generate a text file containing the complete closure of the packages,
    # including the packages themselves.
    # The output of `writeClosure` is equivalent to `nix-store -q --requisites`.
    mkdir -p $out/nix/store
    pkg_path=${lib.strings.concatStringsSep ":" all_pkgs}
    while IFS= read -r dep_path; do
      if [[ "$pkg_path" == *"$dep_path"* ]]; then
        continue
      fi
      cp -r $dep_path $out/nix/store/
    done < ${writeClosure all_pkgs}
  '';
}
