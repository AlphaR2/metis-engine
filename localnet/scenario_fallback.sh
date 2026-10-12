#!/usr/bin/env bash
# A market whose result finalises while no operator is running: a stranger is refused until
# OPERATOR_GRACE_SECS after the dispute window ends, then allowed. (localnet build: 20s + 120s.)
set -uo pipefail
cd "$(dirname "$0")/.."
# Binaries built with --features localnet (a plain `cargo build` has different compiled-in roots).
BIN=${BIN:-target/debug}
export RPC_URL=http://127.0.0.1:8899 KEEPER_RPC_URL=http://127.0.0.1:8899
K=localnet/keys; OPS=$BIN/metis-ops; P() { solana-keygen pubkey $K/$1.json; }; MINT=$(P usdc-mint)
err() { grep -o "custom program error: 0x[0-9a-f]*" | head -1; }
INFO=$(./localnet/new_market.sh 6 4106 240 | tail -1); echo "$INFO"
MARKET=$(echo "$INFO" | sed 's/.*market=\([^ ]*\).*/\1/'); ORACLE=$(echo "$INFO" | sed 's/.* oracle=\([^ ]*\).*/\1/'); KICK=$(echo "$INFO" | sed 's/.*kickoff=\([^ ]*\).*/\1/')
for i in 1 2; do KEEPER_KEYPAIR=$K/reporter$i.json KEEPER_PROVIDER=football-data PROVIDER_API_KEY=x KEEPER_PROVIDER_BASE_URL=http://127.0.0.1:910$i/v4 \
  KEEPER_WATCH=localnet/watch-6.json KEEPER_POLL_SECS=2 RUST_LOG=info $BIN/oracle-keeper run > localnet/k6-$i.log 2>&1 & done
until [ $(date +%s) -ge $KICK ]; do sleep 2; done
for s in a b; do echo '{"status":"FINISHED","duration":"REGULAR","home":1,"away":0,"ht_home":1,"ht_away":0}' > localnet/state-$s.json; done
ROOT=$(sed 's/.*"fixture_root":"\([^"]*\)".*/\1/' localnet/watch-6.json)
until $BIN/oracle-keeper show $ROOT | grep -q "finalized:   true"; do sleep 3; done; FIN=$(date +%s); echo "finalized at $FIN"
pkill -f "watch-6.json"
sleep 25
echo "stranger settle just after the dispute window (expect 0x2d OperatorRequired):"; $OPS settle --signer $K/bettor2.json --market $MARKET --oracle $ORACLE 2>&1 | err
echo "operator settle would work now; not sent. waiting for the grace period..."
until $OPS settle --signer $K/bettor2.json --market $MARKET --oracle $ORACLE >/tmp/s6.out 2>&1; do sleep 10; done
echo "stranger settle after the grace period: OK at $(date +%s) (finalized+$(( $(date +%s) - FIN ))s)"
