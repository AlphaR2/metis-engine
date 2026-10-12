#!/usr/bin/env bash
# NET=local only: a clean validator with the demo programs, the real Manifest and the REAL Meteora
# programs (DBC, DAMM v2, Metaplex metadata, locker) cloned from mainnet, for rehearsing these scripts
# and the DBC flow without spending devnet SOL. Needs internet once, to clone. Its own ports (RPC 8999),
# so it cannot collide with another validator. Foreground: run it in its own terminal or with &.
set -euo pipefail
export NET=local
. "$(dirname "$0")/common.sh"
for f in "$SO/metis_core.so" "$SO/oracle.so" target/deploy/manifest.so; do [ -f "$f" ] || { echo "missing $f, run NET=local devnet/00-build.sh"; exit 1; }; done
rm -rf "$D/ledger"
CLONE=()
if [ "${NO_METEORA:-}" != 1 ]; then
  CLONE=(--url https://api.mainnet-beta.solana.com
    --clone-upgradeable-program dbcij3LWUppWqq96dh6gJWwBifmcGfLSB5D4DuSMaqN
    --clone-upgradeable-program cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG
    --clone-upgradeable-program metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s
    --clone-upgradeable-program LocpQgucEQHbqNABEYvBvwoxCPsSbG91A1QaQhQQqjn
    --clone 7F6dnUcRuyM2TwR8myT1dYypFXpPSxqwKNSFNkxyNESd --clone 2nHK1kju6XjphBLbNxpM5XRGFj7p9U8vvNzyZiha1z6k
    --clone Hv8Lmzmnju6m7kcokVKvwqz7QPmdX9XfKjJsXz8RXcjp --clone 2c4cYd4reUYVRAB9kUUkrq55VPyy2FNQ3FDL4o12JXmq
    --clone AkmQWebAwFvWk55wBoCr5D62C6VVDTzi84NJuD9H7cFD --clone DbCRBj8McvPYHJG1ukj8RE15h2dCNUdTAESG49XpQ44u
    --clone A8gMrEPJkacWkcb3DGwtJwTe16HktSEfvwtuDh2MCtck)
fi
exec solana-test-validator --reset --quiet --ledger "$D/ledger" --rpc-port 8999 --faucet-port 9999 --gossip-port 8990 --dynamic-port-range 8991-9060 \
  --bpf-program $CORE "$SO/metis_core.so" --bpf-program $ORACLE "$SO/oracle.so" --bpf-program $MANIFEST target/deploy/manifest.so "${CLONE[@]}"
