#!/usr/bin/env bash
# Dry run of a Jupiter buy against the live API.
#
#     bash scripts/dry-run-order.sh
#
# Builds a REAL order and a REAL unsigned transaction, then STOPS. Nothing is signed, nothing is
# broadcast, no USDC moves. This exercises every step except the irreversible one.
#
# Needs a metis-api running with EXECUTION_ENABLED=true and a working JUPITER_API_KEY:
#
#     cd aggregator
#     EXECUTION_ENABLED=true cargo run -p metis-aggregator --bin metis-api
#
# Reads nothing from your .env — it only talks to the local API.
set -uo pipefail

API="${METIS_API:-http://localhost:8080}"
SIZE="${SIZE:-5}"          # Jupiter's documented minimum is $5
CATEGORY="${CATEGORY:-sports}"

command -v python3 >/dev/null || { echo "python3 required"; exit 1; }
curl -s -m 5 "$API/health" >/dev/null 2>&1 || { echo "metis-api not answering at $API"; exit 1; }

echo "=============================================================="
echo " JUPITER BUY — DRY RUN (nothing is signed or broadcast)"
echo "=============================================================="

echo
echo "[1] Is trading switched on?"
curl -s -m 10 "$API/execution/abilities" | python3 -c "
import json,sys
d=json.load(sys.stdin)
print(f\"    execution_enabled : {d['execution_enabled']}\")
if not d['execution_enabled']:
    print('    → off. Restart with EXECUTION_ENABLED=true to run the rest.')
    raise SystemExit(1)
" || exit 1

echo
echo "[2] Pick a real, liquid market from the catalogue"
MARKET=$(curl -s -m 30 "$API/markets?category=$CATEGORY&venue=jupiter&sort=best_liquidity&limit=1" \
  | python3 -c "
import json,sys
m=(json.load(sys.stdin).get('markets') or [None])[0]
if not m: raise SystemExit(1)
print(m['venue_market_id'])
print((m.get('event') or {}).get('title') or m['title'])
" ) || { echo "    no $CATEGORY markets in the catalogue — has a refresh run?"; exit 1; }

MARKET_ID=$(echo "$MARKET" | head -1)
MARKET_TITLE=$(echo "$MARKET" | tail -1)
echo "    market : $MARKET_ID"
echo "    title  : ${MARKET_TITLE:0:70}"

echo
echo "[3] Build a real unsigned buy for \$$SIZE"
curl -s -m 60 -X POST "$API/execution/orders" -H 'content-type: application/json' -d "$(python3 -c "
import json
print(json.dumps({
  'venue_id':'jupiter',
  'venue_market_id':'$MARKET_ID',
  'side':'yes',
  'amount_usdc':'$SIZE',
  # Never signed, so this wallet is never charged.
  'wallet':'11111111111111111111111111111111',
}))")" | python3 -c "
import base64, json, sys
d = json.load(sys.stdin)
if 'error' in d:
    print('    REFUSED:', d['error'][:300]); raise SystemExit(1)

print(f\"    venue_ref       : {d.get('venue_ref')}\")
print(f\"    expires_in_secs : {d.get('expires_in_secs')}\")
print(f\"    last_valid_block: {d.get('last_valid_block_height')}\")

cost = d.get('cost', {})
print(f\"    total cost      : {cost.get('total_usdc')} USDC\")
for line in cost.get('lines', []):
    print(f\"      - {line['label']:<26} {line['amount_usdc']:>12}\")

p = d.get('preview') or {}
if p:
    print('    you would receive:')
    print(f\"      contracts       : {p.get('contracts')}\")
    print(f\"      avg price       : {p.get('avg_price_usdc')} USDC\")
    print(f\"      payout if win   : {p.get('payout_if_win_usdc')} USDC\")
    print(f\"      slippage        : {p.get('slippage_bps')} bps (Jupiter's cap: {p.get('max_slippage_bps')} bps)\")

try:
    raw = base64.b64decode(d.get('transaction',''), validate=True)
    print(f'    transaction     : {len(raw)} bytes, base64 decodes cleanly')
except Exception as e:
    print('    transaction     : NOT VALID BASE64 —', e); raise SystemExit(1)

print()
print('    VERDICT: order build works end to end.')
print('    Nothing was signed. No order was placed. No USDC moved.')
"
echo
echo "=============================================================="
