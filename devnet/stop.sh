#!/usr/bin/env bash
# Stops the processes started by 02-start.sh.
. "$(dirname "$0")/common.sh"
for p in "$RUN"/*.pid; do [ -f "$p" ] || continue; kill "$(cat "$p")" 2>/dev/null || true; rm -f "$p"; done
echo stopped
