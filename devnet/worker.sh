#!/usr/bin/env bash
# The hosted worker: everything that must run in the background for the demo, in one container.
# Oracle keepers + crank + mock providers (02-start.sh) and the Open Pool / faucet / status service.
# It heals itself: if a keeper, the crank or a provider dies, it starts them again.
#
# Environment (set on the host, never in the image):
#   RPC_URL            a devnet RPC endpoint
#   METIS_KEYS_B64     from devnet/pack-secrets.sh (unpacked once, into DEMO_KEYS)
#   DEMO_TOKEN         password for the /demo controls
#   CORS_ORIGIN        the website allowed to call this service, e.g. https://metis.vercel.app
#   DEMO_KEYS          where keys and state live: a persistent volume, default /data/keys
#   PORT               set by the host; the service listens here
set -euo pipefail
cd "$(dirname "$0")/.."
export NET=${NET:-devnet}
export DEMO_KEYS=${DEMO_KEYS:-/data/keys}
: "${RPC_URL:?set RPC_URL to a devnet RPC endpoint}"
: "${DEMO_TOKEN:?set DEMO_TOKEN (the password for the demo controls)}"

mkdir -p "$DEMO_KEYS"; chmod 700 "$DEMO_KEYS"
if [ ! -f "$DEMO_KEYS/operator.json" ]; then
  : "${METIS_KEYS_B64:?set METIS_KEYS_B64 (from devnet/pack-secrets.sh) for the first start}"
  printf '%s' "$METIS_KEYS_B64" | base64 -d | tar -xzf - -C "$DEMO_KEYS"
  chmod 600 "$DEMO_KEYS"/*.json
  echo "keys unpacked into $DEMO_KEYS"
fi

devnet/02-start.sh
(
  # the watchdog: every 20 s, if any background process is gone, start the set again
  while sleep 20; do
    for p in "$DEMO_KEYS"/run/*.pid; do
      [ -f "$p" ] || continue
      kill -0 "$(cat "$p")" 2>/dev/null || { echo "$(basename "$p" .pid) died: restarting the set"; devnet/02-start.sh >/dev/null; break; }
    done
  done
) &
exec devnet/dbc.sh serve
