#!/usr/bin/env bash
# Dry run of Panta market creation against the live API.
#
#     bash scripts/dry-run-create.sh
#
# Builds a REAL creation quote and a REAL unsigned transaction, then STOPS. Nothing is signed and
# nothing is broadcast, so no market is created and no fee is paid — this exercises every step
# except the irreversible one.
#
# Needs a metis-api running with MARKET_CREATION_ENABLED=true and a working PANTA_API_KEY:
#
#     cd aggregator
#     MARKET_CREATION_ENABLED=true cargo run -p metis-aggregator --bin metis-api
#
# Reads nothing from your .env itself — it only talks to the local API.
set -uo pipefail

API="${METIS_API:-http://localhost:8080}"
NOW=$(date +%s)

command -v python3 >/dev/null || { echo "python3 required"; exit 1; }

echo "=============================================================="
echo " PANTA CREATE — DRY RUN (nothing is signed or broadcast)"
echo "=============================================================="
echo "  api: $API"

curl -s -m 5 "$API/health" >/dev/null 2>&1 || {
  echo "  metis-api is not answering at $API"; exit 1; }

echo
echo "[1] Is creation switched on?"
curl -s -m 10 "$API/execution/abilities" | python3 -c "
import json,sys
d=json.load(sys.stdin)
print(f\"    market_creation_enabled : {d['market_creation_enabled']}\")
for v in d['venues']:
    print(f\"    {v['venue_id']:<8} create={v['can_create_markets']} execute={v['can_execute']}\")
if not d['market_creation_enabled']:
    print('    → off. Restart with MARKET_CREATION_ENABLED=true to run the rest.')
    raise SystemExit(1)
" || exit 1

echo
echo "[2] Build a real unsigned creation transaction"
BODY=$(python3 -c "
import json
now=$NOW
print(json.dumps({
  'venue_id':'panta',
  'question':'[DRY RUN — do not sign] Will this transaction remain unsigned?',
  'resolution_rule':'This market exists only to verify the build pipeline and is never broadcast.',
  'sources_of_truth':['https://github.com/'],
  'category':'other',
  'start_time':  now + 2*3600,
  'end_time':    now + 24*3600,
  'resolution_time': now + 26*3600,
  'image_url':'https://placehold.co/1024x1024.png',
  # Any valid base58 pubkey works: we never sign, so this wallet is never charged.
  'wallet':'11111111111111111111111111111111',
}))")

curl -s -m 60 -X POST "$API/execution/markets" \
  -H 'content-type: application/json' -d "$BODY" \
| python3 -c "
import base64, json, sys
d = json.load(sys.stdin)

if 'error' in d:
    print('    REFUSED:', d['error'][:300]); raise SystemExit(1)

tx = d.get('transaction','')
print(f\"    venue_ref       : {d.get('venue_ref')}\")
print(f\"    expires_in_secs : {d.get('expires_in_secs')}  <- the real constraint on signing\")
print(f\"    signers         : {d.get('required_signers')}\")

cost = d.get('cost', {})
print(f\"    total cost      : {cost.get('total_usdc')} USDC\")
for line in cost.get('lines', []):
    print(f\"      - {line['label']:<22} {line['amount_usdc']:>12} USDC\")

# The one thing worth checking hard: is it really a decodable Solana transaction?
try:
    raw = base64.b64decode(tx, validate=True)
    print(f'    transaction     : {len(raw)} bytes, base64 decodes cleanly')
    ok = len(raw) > 100
except Exception as e:
    print('    transaction     : NOT VALID BASE64 —', e); raise SystemExit(1)

print()
print('    VERDICT:', 'build pipeline works end to end.' if ok else 'transaction looks too small.')
print('    Nothing was signed. No market was created. No fee was paid.')
"
echo
echo "=============================================================="
