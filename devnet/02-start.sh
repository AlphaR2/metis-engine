#!/usr/bin/env bash
# (Re)starts the demo's background processes: two MOCK football providers, two keepers (one per
# reporter, each reading its own mock), and the crank (the attested operator). Safe to re-run: it
# stops the old ones first. Logs and pidfiles are in $RUN. Nothing here prints a secret.
set -euo pipefail
. "$(dirname "$0")/common.sh"
[ -f "$STATE" ] || { echo "run devnet/01-setup.sh first"; exit 1; }
POLL=${POLL_SECS:-6}   # devnet's public RPC is rate limited; local can use 2
[ "$NET" = local ] && POLL=${POLL_SECS:-2}
"$ENGINE/devnet/stop.sh" >/dev/null 2>&1 || true
touch "$RUN/watch.json"; [ -s "$RUN/watch.json" ] || echo '[]' > "$RUN/watch.json"
for i in 1 2; do
  [ -s "$RUN/mock-$i.json" ] || echo '{"default":{"status":"SCHEDULED","home":null,"away":null}}' > "$RUN/mock-$i.json"
  python3 -I "$ENGINE/devnet/mock_provider.py" $((9100 + i)) "$RUN/mock-$i.json" > "$RUN/mock-$i.log" 2>&1 & echo $! > "$RUN/mock-$i.pid"
  KEEPER_KEYPAIR=$D/reporter$i.json KEEPER_PROVIDER=football-data PROVIDER_API_KEY=mock \
    KEEPER_PROVIDER_BASE_URL=http://127.0.0.1:$((9100 + i))/v4 KEEPER_WATCH=$RUN/watch.json KEEPER_POLL_SECS=$POLL \
    RUST_LOG=info "$KEEPER" run > "$RUN/keeper-$i.log" 2>&1 & echo $! > "$RUN/keeper-$i.pid"
done
KEEPER_KEYPAIR=$D/operator.json KEEPER_POLL_SECS=$POLL RUST_LOG=info "$KEEPER" crank > "$RUN/crank.log" 2>&1 & echo $! > "$RUN/crank.pid"
sleep 2
for p in "$RUN"/*.pid; do kill -0 "$(cat "$p")" 2>/dev/null && echo "up   $(basename "$p" .pid)" || echo "DOWN $(basename "$p" .pid) (see its .log)"; done
