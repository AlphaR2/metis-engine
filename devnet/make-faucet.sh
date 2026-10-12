#!/usr/bin/env bash
# One-off, for a deployment whose demo-USDC mint is still controlled by the DEPLOYER (01-setup.sh used to do
# that): creates a dedicated `faucet` key, funds it a little SOL, and hands it the mint authority. The hosted
# demo then carries only the faucet key, never the deployer (which is the programs' upgrade authority).
# Safe to re-run: it does nothing if the faucet already holds the mint authority.
#
#   export DEPLOYER=/path/to/your-deployer.json
#   devnet/make-faucet.sh
set -euo pipefail
. "$(dirname "$0")/common.sh"
[ -f "$STATE" ] || { echo "run devnet/01-setup.sh first"; exit 1; }
: "${DEPLOYER:?set DEPLOYER to the keypair file that created the mint}"
mkkey faucet
FAUCET=$(P faucet); DEP=$(P_FILE() { if command -v solana-keygen >/dev/null 2>&1; then solana-keygen pubkey "$1"; else "$OPS" pubkey --keypair "$1"; fi; }; P_FILE "$DEPLOYER")
CURRENT=$(spl-token display "$MINT" -u "$RPC_URL" | awk '/Mint authority/{print $NF}')
echo "mint $MINT"; echo "  current mint authority: $CURRENT"; echo "  faucet key:             $FAUCET"
if [ "$CURRENT" = "$FAUCET" ]; then echo "the faucet already holds the mint authority: nothing to do"; exit 0; fi
[ "$CURRENT" = "$DEP" ] || { echo "the mint authority is not your DEPLOYER ($DEP): cannot change it from here"; exit 1; }
BAL=$(sol "$FAUCET")
if awk "BEGIN{exit !($BAL < 0.3)}"; then
  solana transfer --from "$DEPLOYER" --fee-payer "$DEPLOYER" "$FAUCET" 0.5 --allow-unfunded-recipient -u "$RPC_URL" >/dev/null
  echo "funded the faucet with 0.5 SOL"
fi
spl-token authorize "$MINT" mint "$FAUCET" --authority "$DEPLOYER" --fee-payer "$DEPLOYER" -u "$RPC_URL" >/dev/null
echo "done: the faucet key now controls the demo USDC mint"
