#!/usr/bin/env bash
# The demo faucet: mints demo USDC (our mint) to any wallet, creating its token account if needed.
#   mint-to.sh <wallet address> [amount, default 100]
# Uses the faucet key (the mint authority, funded by 01-setup.sh) for the mint and the account rent.
set -euo pipefail
. "$(dirname "$0")/common.sh"
W=${1:?wallet address} AMT=${2:-100}
spl-token create-account "$MINT" --owner "$W" --fee-payer "$D/faucet.json" -u "$RPC_URL" >/dev/null 2>&1 || true
spl-token mint "$MINT" "$AMT" "$(ata "$W")" --mint-authority "$D/faucet.json" --fee-payer "$D/faucet.json" -u "$RPC_URL"
