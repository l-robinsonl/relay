#!/usr/bin/env bash
# Measures Relay's resident memory, including the WebKit processes the
# webview spawns.
#
# PSS is the figure to quote. RSS counts a shared library in full against
# every process mapping it, so summing RSS across a process tree
# double-counts the shared parts. PSS divides each shared page by the
# number of processes sharing it, so the total is what the machine
# actually gives up to run the application.
set -euo pipefail

PROCESS="${1:-relay-desktop}"

ROOT=$(pgrep -x "$PROCESS" | head -1 || true)
if [ -z "$ROOT" ]; then
  echo "No '$PROCESS' process is running." >&2
  exit 1
fi

# The main process plus its descendants. Collected into a unique set:
# WebKit's helpers can be reached by more than one route, and counting
# one twice would quietly inflate the total.
collect () {
  echo "$1"
  for child in $(pgrep -P "$1" 2>/dev/null || true); do
    collect "$child"
  done
}

mapfile -t PIDS < <(collect "$ROOT" | sort -u)

printf '%-34s %>10s %10s\n' "PROCESS" "RSS" "PSS" 2>/dev/null \
  || printf '%-34s %10s %10s\n' "PROCESS" "RSS" "PSS"

total_rss=0
total_pss=0

for pid in "${PIDS[@]}"; do
  [ -r "/proc/$pid/status" ] || continue
  name=$(tr -d '\0' < "/proc/$pid/comm" 2>/dev/null || echo '?')

  # A shell or editor spawned alongside is not part of the application.
  case "$name" in zsh|bash|sh|node) continue ;; esac

  rss=$(awk '/^VmRSS:/{print $2}' "/proc/$pid/status" 2>/dev/null || echo 0)
  pss=$(awk '/^Pss:/{s+=$2} END{print s+0}' "/proc/$pid/smaps_rollup" 2>/dev/null || echo 0)
  [ "${rss:-0}" -eq 0 ] && continue

  printf '  %-32s %7d MB %7d MB\n' "$name ($pid)" "$((rss / 1024))" "$((pss / 1024))"
  total_rss=$((total_rss + rss))
  total_pss=$((total_pss + pss))
done

echo "  --------------------------------------------------------"
printf '  %-32s %7d MB %7d MB\n' "TOTAL" "$((total_rss / 1024))" "$((total_pss / 1024))"
echo
echo "Quote the PSS total. RSS double-counts shared libraries."
