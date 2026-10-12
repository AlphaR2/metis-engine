#!/usr/bin/env bash
# new_market.sh <nonce> <provider match id> [kickoff-in-seconds, default 180]
# Creates a fixture on the oracle and a market on it, then two bets. With the localnet build the
# betting window is 120s shorter than the lead, so keep the lead >= 150.
set -euo pipefail
cd "$(dirname "$0")/.."
# Binaries built with --features localnet (a plain `cargo build` has different compiled-in roots).
BIN=${BIN:-target/debug}
export RPC_URL=${RPC_URL:-http://127.0.0.1:8899} KEEPER_RPC_URL=${RPC_URL:-http://127.0.0.1:8899}
K=localnet/keys; OPS=$BIN/metis-ops; P() { solana-keygen pubkey $K/$1.json; }
NONCE=$1; MATCH=$2; LEAD=${3:-180}
KICK=$(( $(date +%s) + LEAD ))
OUT=$(KEEPER_KEYPAIR=$K/reporter1.json $BIN/oracle-keeper create-fixture --kickoff $KICK --id football-data=$MATCH)
ROOT=$(echo "$OUT" | awk '/^fixture_root:/{print $2}'); ORACLE=$(echo "$OUT" | awk '/^oracle:/{print $2}')
$OPS create-market --creator $K/creator.json --mint "$(P usdc-mint)" --oracle "$ORACLE" --nonce $NONCE --condition home-win \
  --question "Will Team A beat Team B? (#$NONCE)" --rubric-yes "YES if Team A leads after 90 minutes, stoppage time included." \
  --rubric-no "NO for a draw or a Team B win after 90 minutes. Extra time and penalties do not count." \
  --label-yes "Team A wins" --label-no "Draw or Team B" | tee localnet/market-$NONCE.txt
MARKET=$(awk '/^market:/{print $2}' localnet/market-$NONCE.txt)
$OPS bet --bettor $K/bettor1.json --market $MARKET --mint "$(P usdc-mint)" --side 0 --usdc 20 >/dev/null
$OPS bet --bettor $K/bettor2.json --market $MARKET --mint "$(P usdc-mint)" --side 1 --usdc 15 >/dev/null
echo "[{\"fixture_root\":\"$ROOT\",\"external_id\":\"$MATCH\"}]" > localnet/watch-$NONCE.json
echo "market=$MARKET oracle=$ORACLE kickoff=$KICK root=$ROOT"
