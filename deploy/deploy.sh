#!/usr/bin/env bash
# Deploys the two programs and sets up their trust accounts on devnet or mainnet.
# (A devnet DEMO, with short timers, uses devnet/00-build.sh .. 01-setup.sh instead: see DEVNET-RUNBOOK.md.)
#
#   deploy/deploy.sh devnet|mainnet <step>
#
# Steps (run in order; each is safe to read before running, and prints what it will do):
#   build      builds both programs with that network's trust roots compiled in
#   deploy     deploys them with the deploy key (the upgrade authority is that key: see below)
#   init       creates the oracle's authority account and the market program's Config (two roots sign)
#   attest     attests the reporters and the crank operator (two roots sign each)
#   verify     reads everything back and prints what is on chain
#
# Needs (environment):
#   RPC_URL            the cluster's RPC endpoint
#   DEPLOY_KEYPAIR     funded keypair that pays for deployment (and holds upgrade authority)
#   CORE_KEYPAIR, ORACLE_KEYPAIR  the program keypairs whose public keys are the compiled-in ids
#                      (2jt26iYK... and PorC1q7b...); the ones in target/deploy are NOT those
#   ROOT_A, ROOT_B     keypair files of two of the three root signers
#   QUOTE_MINT         the USDC mint (6 decimals) for this network
#   TREASURY           the treasury's wallet address (its USDC account is created on first sweep)
#   REPORTERS          space-separated addresses of the oracle reporters (one per data provider)
#   OPERATOR           the crank's address
#   and the dials, all explicit - there are no defaults here on purpose:
#   INIT_FEE FEE_BPS RAKE_BPS DISPUTE_SECS K MAX_IN_BPS
#
# The roots and the guardian are compiled into the programs (authority/src/network.rs). The build
# refuses until they are filled in for the network, which is the point.
#
# UPGRADE AUTHORITY: deployment leaves it with DEPLOY_KEYPAIR. Decide who holds it before mainnet
# (a multisig, or revoke it with `solana program set-upgrade-authority --final` once audited).
set -euo pipefail
cd "$(dirname "$0")/.."

NET=${1:?usage: deploy.sh devnet|mainnet <step>}; STEP=${2:?step: build|deploy|init|attest|verify}
case $NET in devnet|mainnet) ;; *) echo "network must be devnet or mainnet"; exit 2;; esac
: "${RPC_URL:?set RPC_URL}"
OPS=target/release/metis-ops
CORE=79uea7X34YHRUQFkLAR5rwupHQf6Z7ASbBGfzxRDu8ws
ORACLE=BM7jwCCXjBgRy5LPg1UG4iAPueAFK3bRxQRogD4Wzoub
need() { for v in "$@"; do [ -n "${!v:-}" ] || { echo "set $v"; exit 2; }; done; }

if [ "$NET" = mainnet ] && [ "${I_UNDERSTAND_THIS_IS_MAINNET:-}" != yes ]; then
  echo "refusing: mainnet needs I_UNDERSTAND_THIS_IS_MAINNET=yes"; exit 2
fi

case $STEP in
  build)
    cargo build-sbf --manifest-path programs/core/Cargo.toml   --features "$NET" --sbf-out-dir target/$NET
    cargo build-sbf --manifest-path programs/oracle/Cargo.toml --features "$NET" --sbf-out-dir target/$NET
    cargo build --release -p metis-ops -p oracle-keeper --features "metis-ops/$NET,oracle-keeper/$NET"
    ls -l target/$NET/*.so ;;
  deploy)
    need DEPLOY_KEYPAIR CORE_KEYPAIR ORACLE_KEYPAIR
    # A program can only be deployed at the address of its own keypair, and these programs have
    # their ids compiled in: the keypairs must be the ones whose public keys ARE those ids.
    [ "$(solana-keygen pubkey "$CORE_KEYPAIR")" = "$CORE" ]     || { echo "CORE_KEYPAIR is not the keypair for $CORE"; exit 2; }
    [ "$(solana-keygen pubkey "$ORACLE_KEYPAIR")" = "$ORACLE" ] || { echo "ORACLE_KEYPAIR is not the keypair for $ORACLE"; exit 2; }
    echo "deploying to $NET ($RPC_URL) with $(solana-keygen pubkey "$DEPLOY_KEYPAIR")"
    solana program deploy target/$NET/metis_core.so --program-id "$CORE_KEYPAIR" --keypair "$DEPLOY_KEYPAIR" -u "$RPC_URL"
    solana program deploy target/$NET/oracle.so     --program-id "$ORACLE_KEYPAIR" --keypair "$DEPLOY_KEYPAIR" -u "$RPC_URL" ;;
  init)
    need ROOT_A ROOT_B QUOTE_MINT TREASURY INIT_FEE FEE_BPS RAKE_BPS DISPUTE_SECS K MAX_IN_BPS
    export RPC_URL
    $OPS addresses
    $OPS oracle-init --root "$ROOT_A"
    $OPS config-init --root-a "$ROOT_A" --root-b "$ROOT_B" --mint "$QUOTE_MINT" --treasury "$TREASURY" \
      --init-fee "$INIT_FEE" --fee-bps "$FEE_BPS" --rake-bps "$RAKE_BPS" --dispute-secs "$DISPUTE_SECS" --k "$K" --max-in-bps "$MAX_IN_BPS" ;;
  attest)
    need ROOT_A ROOT_B REPORTERS OPERATOR
    export RPC_URL
    for who in $REPORTERS; do for r in "$ROOT_A" "$ROOT_B"; do $OPS attest --program oracle --root "$r" --candidate "$who"; done; done
    for r in "$ROOT_A" "$ROOT_B"; do $OPS attest --program core --root "$r" --candidate "$OPERATOR"; done ;;
  verify)
    $OPS addresses
    for p in $CORE $ORACLE; do solana program show $p -u "$RPC_URL"; done
    echo "config:"; solana account "$($OPS addresses | awk '/^config:/{print $2}')" -u "$RPC_URL" | head -5 ;;
  *) echo "unknown step $STEP"; exit 2 ;;
esac
