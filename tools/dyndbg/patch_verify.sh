#!/bin/sh
# Test: P-04 (patch binary verification with symbol lookup)
set -eu

PROC=/proc/sys/kernel/dynamic_debug
KALLSYMS=/proc/kallsyms

MODULE_KEY=${MODULE_KEY:-dyndbg_bench}
SYMBOL_FILTER=${SYMBOL_FILTER:-dyndbg_bench}
KEEP_STATE=${KEEP_STATE:-0}
RESULTS_DIR=${RESULTS_DIR:-results}
RUN_ID=${RUN_ID:-$(date +%Y%m%d_%H%M%S 2>/dev/null || echo "run_$$")}
COMMIT=${COMMIT:-unknown}
PHASE=${PHASE:-unknown}
CSV_FILE="$RESULTS_DIR/patch_verify/results.csv"
BYTES_DISABLED=${BYTES_DISABLED:-}
BYTES_ENABLED=${BYTES_ENABLED:-}

if [ ! -e "$PROC" ] || [ ! -e "$KALLSYMS" ]; then
  echo "dyndbg procfs files not found" >&2
  exit 1
fi

run_rule() {
  echo "$1" > "$PROC"
}

ensure_csv() {
  if [ ! -e "$CSV_FILE" ]; then
    mkdir -p "$(dirname "$CSV_FILE")"
    echo "run_id,commit,phase,case,state,site_addr,method,bytes" > "$CSV_FILE"
  fi
}

emit_result() {
  echo "RESULT $*"
}

format_bytes() {
  echo "$1" | tr ' ' '_'
}

find_site_symbol() {
  grep "__dyndbg_site_" "$KALLSYMS" | grep "$SYMBOL_FILTER" | head -n 1 | awk '{print $1}'
}

site_addr=$(find_site_symbol)

if [ -z "$site_addr" ]; then
  echo "no __dyndbg_site_ symbol found; set SYMBOL_FILTER" >&2
  exit 1
fi

echo "site_addr=0x$site_addr"
ensure_csv

run_rule "clear"
echo "state=disabled"
echo "GDB: x/5xb 0x$site_addr"
bytes_disabled=$(format_bytes "$BYTES_DISABLED")
emit_result "test=P-04 state=disabled site_addr=0x$site_addr method=gdb bytes=$bytes_disabled run_id=$RUN_ID commit=$COMMIT phase=$PHASE"
echo "$RUN_ID,$COMMIT,$PHASE,P-04,disabled,0x$site_addr,gdb,$bytes_disabled" >> "$CSV_FILE"

run_rule "module=$MODULE_KEY +p"
echo "state=enabled"
echo "GDB: x/5xb 0x$site_addr"
bytes_enabled=$(format_bytes "$BYTES_ENABLED")
emit_result "test=P-04 state=enabled site_addr=0x$site_addr method=gdb bytes=$bytes_enabled run_id=$RUN_ID commit=$COMMIT phase=$PHASE"
echo "$RUN_ID,$COMMIT,$PHASE,P-04,enabled,0x$site_addr,gdb,$bytes_enabled" >> "$CSV_FILE"

if [ "$KEEP_STATE" -eq 0 ]; then
  run_rule "clear"
  echo "state=cleared"
else
  echo "state=kept (manual clear required)"
fi
