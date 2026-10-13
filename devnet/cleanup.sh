#!/usr/bin/env bash
# Cancels demo markets that are still unsettled (old boards, markets whose match will never report), using two of
# your ROOT keys. A cancelled market refunds everyone and drops off the "open" board. Nothing is deleted from the
# chain (a chain cannot be wiped), but a cancelled market no longer shows as bettable.
#
#   devnet/cleanup.sh                        preview only: lists what WOULD be cancelled
#   devnet/cleanup.sh --yes                  cancels all of them
#   devnet/cleanup.sh --yes --keep A,B       cancels all except the market addresses A and B
#
# Needs: RPC_URL, and for --yes: ROOT_A and ROOT_B (two different root key files). Run from your own machine, after
# devnet/00-build.sh. Never put root keys on a server.
set -euo pipefail
. "$(dirname "$0")/common.sh"
YES=0; KEEP=","
while [ $# -gt 0 ]; do
  case $1 in
    --yes) YES=1 ;;
    --keep) shift; KEEP=",${1:?--keep needs a comma-separated list},"; ;;
    *) echo "usage: cleanup.sh [--yes] [--keep addr,addr]"; exit 2 ;;
  esac; shift
done
[ -x "$OPS" ] || { echo "tools not built for $NET: run devnet/00-build.sh"; exit 1; }
if [ $YES = 1 ]; then
  : "${ROOT_A:?set ROOT_A (a root key file)}" "${ROOT_B:?set ROOT_B (a different root key file)}"
fi
TODO=0
while read -r MARKET STATUS ORACLE CLOSE; do
  case $STATUS in Open|Closed|Pending) ;; *) continue ;; esac
  case $KEEP in *",$MARKET,"*) echo "keep    $MARKET ($STATUS)"; continue ;; esac
  TODO=$((TODO + 1))
  if [ $YES = 1 ]; then
    if "$OPS" cancel-roots --root-a "$ROOT_A" --root-b "$ROOT_B" --market "$MARKET" --oracle "$ORACLE" >/dev/null 2>&1; then echo "cancelled $MARKET (was $STATUS)"; else echo "FAILED   $MARKET (was $STATUS): see below"; "$OPS" cancel-roots --root-a "$ROOT_A" --root-b "$ROOT_B" --market "$MARKET" --oracle "$ORACLE" 2>&1 | tail -2 || true; fi
  else
    echo "would cancel $MARKET ($STATUS)"
  fi
done < <("$OPS" markets)
[ $YES = 1 ] || echo "-- preview only: $TODO market(s). Run again with --yes to cancel them."
