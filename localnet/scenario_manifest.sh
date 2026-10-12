#!/usr/bin/env bash
# Trading between holders on the real Manifest books, end to end through the aggregator.
#   market -> crank opens the books -> a maker (bettor1) offers YES slices at 70c ->
#   the aggregator quotes and builds a buy for bettor2 -> bettor2 signs and sends it ->
#   bettor2 sells back into a bid. All before kickoff (the books are open for 120s on the localnet build).
# Needs: validator + setup.sh done, the aggregator on :8080 (localnet build, METIS_RPC_URL pointing at it),
# binaries built with `--features localnet` in $BIN (default target/ops2/debug).
set -uo pipefail
cd "$(dirname "$0")/.."
BIN=${BIN:-target/ops2/debug}; export BIN;export RPC_URL=http://127.0.0.1:8899 KEEPER_RPC_URL=http://127.0.0.1:8899
K=localnet/keys; OPS=$BIN/metis-ops; P() { solana-keygen pubkey $K/$1.json; }; MINT=$(P usdc-mint)
NONCE=${1:-8}; MATCH=${2:-6108}
bal() { spl-token balance --address "$(spl-token address --token $1 --owner "$(P $2)" --verbose -u $RPC_URL | awk '/Associated token address/{print $NF}')" -u $RPC_URL 2>/dev/null || echo 0; }
api() { curl -s -H 'content-type: application/json' "$@"; }

INFO=$(./localnet/new_market.sh $NONCE $MATCH 300 | tail -1); echo "$INFO"
MARKET=$(echo "$INFO" | sed 's/.*market=\([^ ]*\).*/\1/'); KICK=$(echo "$INFO" | sed 's/.*kickoff=\([^ ]*\).*/\1/')
echo "kickoff in $(( KICK - $(date +%s) ))s; books open at kickoff-120"
RUST_LOG=info KEEPER_KEYPAIR=$K/operator.json KEEPER_POLL_SECS=3 $BIN/oracle-keeper crank > localnet/crank-manifest.log 2>&1 &
CRANK=$!
until [[ "$($OPS show --market $MARKET 2>/dev/null)" == *"book 0:"*"(created)"* ]]; do sleep 2; done; echo "books created at $(date +%s)"
kill $CRANK 2>/dev/null

YES=$(spl-token address --token $MINT --owner $(P bettor1) >/dev/null 2>&1; echo)
echo "== maker offers 10 YES slices at 70c"
$OPS maker-ask --maker $K/bettor1.json --market $MARKET --side 0 --base 10000000 --cents 70 | cut -c1-20
api -X POST localhost:8080/refresh >/dev/null; sleep 6

echo "== catalog"
api "localhost:8080/v1/markets/metis/$MARKET" | python3 -c "
import sys,json
m=json.load(sys.stdin); print(' phase',m.get('phase'),'yes_price',m.get('yes_price'),'no_price',m.get('no_price'))"

B2=$(P bettor2)
echo "== bettor2 USDC before: $(bal $MINT bettor2)"
TX=$(api -X POST localhost:8080/execution/orders -d "{\"venue_id\":\"metis\",\"venue_market_id\":\"$MARKET\",\"side\":\"yes\",\"amount_usdc\":\"3\",\"wallet\":\"$B2\"}")
echo "$TX" | python3 -c "
import sys,json
d=json.load(sys.stdin); t=d.get('transaction')
if not t: print('NO TX:',d); sys.exit(1)
p=t.get('preview') or {}; c=t['cost']
print(' preview: contracts',p.get('contracts'),'avg_price',p.get('avg_price_usdc'),'| cost',c['total_usdc'],[l['label'] for l in c['lines']])
open('/tmp/buy.b64','w').write(t['transaction'])"
echo "== bettor2 signs and sends the aggregator's transaction"
$OPS sign-send --signer $K/bettor2.json --tx "$(cat /tmp/buy.b64)" | cut -c1-24; sleep 3
echo "   bettor2 USDC after: $(bal $MINT bettor2)"

echo "== positions"
for w in bettor1 bettor2; do echo " $w:"; api "localhost:8080/v1/positions?wallet=$(P $w)" | python3 -c "
import sys,json
for p in json.load(sys.stdin)['positions']:
    if p['venue_market_id']=='$MARKET': print('  ',p['side'],'contracts',p['contracts'],'open_orders',p['open_orders'],'can_sell',p['can_sell'],'payout',p['payout_usdc'])"; done
echo "== show"; $OPS show --market $MARKET | sed -n 2,3p
echo "done at $(date +%s) (kickoff $KICK)"
