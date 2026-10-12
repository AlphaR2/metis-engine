#!/usr/bin/env bash
# One-time setup on a fresh local validator (localnet/validator.sh): fund keys, make a test USDC
# mint, create the oracle and market-program trust accounts with real root signatures, attest the
# reporters and the crank operator. Safe to read as the runbook for any network: only the keys and
# RPC change.
set -euo pipefail
cd "$(dirname "$0")/.."
# Binaries built with --features localnet (a plain `cargo build` has different compiled-in roots).
BIN=${BIN:-target/debug}
export RPC_URL=${RPC_URL:-http://127.0.0.1:8899} KEEPER_RPC_URL=${RPC_URL:-http://127.0.0.1:8899}
K=localnet/keys; OPS=$BIN/metis-ops
P() { solana-keygen pubkey $K/$1.json; }
for n in root0 root1 root2 guardian operator reporter1 reporter2 creator bettor1 bettor2 maker payer; do
  [ -f $K/$n.json ] || solana-keygen new --no-bip39-passphrase --silent -o $K/$n.json
  solana airdrop 20 "$(P $n)" -u $RPC_URL >/dev/null
done
[ -f $K/usdc-mint.json ] || solana-keygen new --no-bip39-passphrase --silent -o $K/usdc-mint.json
MINT=$(P usdc-mint)
spl-token create-token --decimals 6 $K/usdc-mint.json --fee-payer $K/payer.json --mint-authority $K/payer.json -u $RPC_URL >/dev/null
for n in creator bettor1 bettor2 maker treasury; do
  spl-token create-account $MINT --owner "$(P $n)" --fee-payer $K/payer.json -u $RPC_URL >/dev/null
done
for n in creator bettor1 bettor2 maker; do
  spl-token mint $MINT 1000 "$(spl-token address --token $MINT --owner "$(P $n)" --verbose -u $RPC_URL | awk '/Associated token address/{print $NF}')" \
    --mint-authority $K/payer.json --fee-payer $K/payer.json -u $RPC_URL >/dev/null
done
$OPS oracle-init --root $K/root0.json >/dev/null
for who in reporter1 reporter2; do for r in root0 root1; do $OPS attest --program oracle --root $K/$r.json --candidate "$(P $who)" >/dev/null; done; done
$OPS config-init --root-a $K/root0.json --root-b $K/root1.json --mint "$MINT" --treasury "$(P treasury)" --dbc "${DBC:-0}" >/dev/null
for r in root0 root1; do $OPS attest --program core --root $K/$r.json --candidate "$(P operator)" >/dev/null; done
echo "setup done. mint=$MINT"; $OPS addresses
