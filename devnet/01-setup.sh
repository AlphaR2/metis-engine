#!/usr/bin/env bash
# One-time setup: preflight, deploy (devnet), fund the demo keys, create the demo USDC mint, then the
# oracle and market-program trust accounts. Re-runnable: each step leaves a marker and is skipped next
# time. Every transaction is real, so read it first. Run devnet/cost.sh first and fund the deployer.
#
#   NET=devnet (default)  needs, as environment variables naming key FILES (nothing secret is passed inline):
#                           DEPLOYER        pays for everything; becomes the programs' upgrade authority
#                           CORE_KEYPAIR    target/deploy/metis_core-keypair.json (default)
#                           ORACLE_KEYPAIR  target/deploy/oracle-keypair.json (default)
#                           ROOT_A, ROOT_B  key files of TWO of the three root signers compiled into the programs
#   NET=local             a validator from local-validator.sh; programs preloaded, throwaway keys, SOL by airdrop.
set -euo pipefail
. "$(dirname "$0")/common.sh"
DONE=$D/done; mkdir -p "$DONE"
step() { [ -f "$DONE/$1" ]; }
mark() { touch "$DONE/$1"; }

# ── amounts (SOL) sent to each demo key; the deployer keeps the rest ─────────────────────────────
declare -a FUND=( "operator 0.25" "creator 0.20" "reporter1 0.04" "reporter2 0.04" "bettor1 0.03" "bettor2 0.03" "treasury 0.005" "dbc-keeper ${DBC_KEEPER_SOL:-0.60}" "faucet ${FAUCET_SOL:-0.50}" )
ROOT_FUND=0.05   # each of root0 / root1 pays the fees of the trust-account transactions they sign (0.10 SOL)
# Budget on devnet (2.5 SOL): core deploy needs ~2.30 SOL at its peak (1.15 stays), oracle ~0.82 at its peak (0.41 stays),
# so deploy FIRST, then fund the demo keys from the ~0.94 left.
NEED_SOL=${NEED_SOL:-$("$ENGINE/devnet/cost.sh" --number)}

# ── preflight, before anything is spent ──────────────────────────────────────────────────────────
if [ "$NET" = devnet ]; then
  : "${DEPLOYER:?set DEPLOYER to your funded keypair file}" "${ROOT_A:?set ROOT_A to a root signer key file}" "${ROOT_B:?set ROOT_B to a second root signer key file}"
elif [ ! -f "$DEPLOYER" ]; then
  solana-keygen new --no-bip39-passphrase --silent -o "$DEPLOYER"; chmod 600 "$DEPLOYER"
fi
for f in "$SO/metis_core.so" "$SO/oracle.so" "$OPS" "$KEEPER" "$ROOT_A" "$ROOT_B" "$DEPLOYER"; do
  [ -f "$f" ] || { echo "missing $f (run devnet/00-build.sh)"; exit 1; }
done
[ "$(solana-keygen pubkey "$ROOT_A")" != "$(solana-keygen pubkey "$ROOT_B")" ] || { echo "ROOT_A and ROOT_B must be two different roots"; exit 1; }
if [ "$NET" = devnet ]; then
  # ROOT_A / ROOT_B must be two of the three roots compiled into the programs (devnet/set-roots.sh), or every
  # root-signed step fails on chain after the deploy has already been paid for.
  DEVBLOCK=$(awk '/feature = "devnet"\)\]$/{f=1} /feature = "mainnet"\)\]$/{f=0} f' authority/src/network.rs)
  for r in "$ROOT_A" "$ROOT_B"; do
    echo "$DEVBLOCK" | grep -q "$(solana-keygen pubkey "$r")" || { echo "$(solana-keygen pubkey "$r") is not one of the devnet roots in authority/src/network.rs (run devnet/set-roots.sh, then 00-build.sh again)"; exit 1; }
  done
fi
if [ "$NET" = devnet ] && ! step deploy; then
  : "${CORE_KEYPAIR:?set CORE_KEYPAIR to the keypair file for $CORE}" "${ORACLE_KEYPAIR:?set ORACLE_KEYPAIR to the keypair file for $ORACLE}"
  [ "$(solana-keygen pubkey "$CORE_KEYPAIR")" = "$CORE" ]     || { echo "CORE_KEYPAIR is not the keypair for $CORE"; exit 2; }
  [ "$(solana-keygen pubkey "$ORACLE_KEYPAIR")" = "$ORACLE" ] || { echo "ORACLE_KEYPAIR is not the keypair for $ORACLE"; exit 2; }
fi
DEP=$(solana-keygen pubkey "$DEPLOYER")
if [ "$NET" = local ]; then solana airdrop 100 "$DEP" -u "$RPC_URL" >/dev/null; fi
BAL=$(sol "$DEP")
echo "deployer $DEP has $BAL SOL on $NET"
if ! step funded && awk "BEGIN{exit !($BAL < $NEED_SOL)}"; then
  echo "need at least $NEED_SOL SOL (have $BAL). Top up $DEP and re-run."; exit 3
fi
solana program show "$MANIFEST" -u "$RPC_URL" -k "$DEPLOYER" >/dev/null 2>&1 \
  && echo "Manifest is deployed on $NET" || { echo "Manifest is NOT deployed on $NET: close_primary / the secondary market will not work"; [ "$NET" = devnet ] && exit 4; }

# ── deploy (devnet only; the local validator preloads the programs) ──────────────────────────────
if [ "$NET" = devnet ] && ! step deploy; then
  solana program deploy "$SO/metis_core.so" --program-id "$CORE_KEYPAIR" --keypair "$DEPLOYER" -u "$RPC_URL"
  solana program deploy "$SO/oracle.so"     --program-id "$ORACLE_KEYPAIR" --keypair "$DEPLOYER" -u "$RPC_URL"
  mark deploy
fi

# ── demo keys and funding ────────────────────────────────────────────────────────────────────────
for n in operator reporter1 reporter2 creator bettor1 bettor2 maker treasury usdc-mint dbc-keeper faucet; do mkkey "$n"; done
if ! step funded; then
  for row in "${FUND[@]}"; do set -- $row
    solana transfer --from "$DEPLOYER" --fee-payer "$DEPLOYER" "$(P "$1")" "$2" --allow-unfunded-recipient -u "$RPC_URL" >/dev/null
    echo "funded $1 $(P "$1") with $2 SOL"
  done; mark funded
fi

if ! step funded_roots; then
  for r in "$ROOT_A" "$ROOT_B"; do
    solana transfer --from "$DEPLOYER" --fee-payer "$DEPLOYER" "$(solana-keygen pubkey "$r")" $ROOT_FUND --allow-unfunded-recipient -u "$RPC_URL" >/dev/null
    echo "funded $(solana-keygen pubkey "$r") (a root) with $ROOT_FUND SOL"
  done; mark funded_roots
fi

# ── the demo USDC mint (6 decimals); a dedicated `faucet` key (not the deployer) is its mint authority ─
MINT=$(P usdc-mint)
if ! step mint; then
  spl-token create-token --decimals 6 "$D/usdc-mint.json" --fee-payer "$DEPLOYER" --mint-authority "$(P faucet)" -u "$RPC_URL" >/dev/null
  for n in creator bettor1 bettor2 maker treasury; do
    spl-token create-account "$MINT" --owner "$(P $n)" --fee-payer "$DEPLOYER" -u "$RPC_URL" >/dev/null
  done
  mark mint
fi
if ! step minted; then
  for row in "creator 2000" "bettor1 1000" "bettor2 1000" "maker 1000"; do set -- $row
    spl-token mint "$MINT" "$2" "$(ata "$(P "$1")")" --mint-authority "$D/faucet.json" --fee-payer "$DEPLOYER" -u "$RPC_URL" >/dev/null
  done; mark minted
fi
printf 'MINT=%s\nNET=%s\n' "$MINT" "$NET" > "$STATE"   # no RPC here: a provider URL carries an API key

# The tools send with `finalized` commitment: wait until the funding above is finalized, or the first
# root-signed transaction fails with "no record of a prior credit".
for t in $(seq 1 60); do
  [ "$(solana balance "$(solana-keygen pubkey "$ROOT_B")" -u "$RPC_URL" --commitment finalized | awk '{print ($1>0)}')" = 1 ] && break
  [ $t = 1 ] && echo "waiting for the funding to finalize..."; sleep 3
done

# ── trust accounts: real root signatures (the demo roots) ────────────────────────────────────────
export RPC_URL
if ! step oracle_init; then $OPS oracle-init --root "$ROOT_A" >/dev/null; mark oracle_init; fi
if ! step attest_reporters; then
  for who in reporter1 reporter2; do for r in "$ROOT_A" "$ROOT_B"; do
    $OPS attest --program oracle --root "$r" --candidate "$(P $who)" >/dev/null
  done; done; mark attest_reporters
fi
if ! step config; then
  $OPS config-init --root-a "$ROOT_A" --root-b "$ROOT_B" --mint "$MINT" --treasury "$(P treasury)" --dbc "${DBC:-1}" >/dev/null
  mark config
fi
if ! step attest_operator; then
  for r in "$ROOT_A" "$ROOT_B"; do $OPS attest --program core --root "$r" --candidate "$(P operator)" >/dev/null; done
  mark attest_operator
fi

echo; echo "setup done on $NET"; echo "mint (demo USDC): $MINT"; $OPS addresses
echo "balances: deployer $(sol "$DEP") SOL, operator $(sol "$(P operator)") SOL"
