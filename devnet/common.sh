#!/usr/bin/env bash
# Shared settings for the devnet/ scripts. Source it; it prints nothing and holds no secrets.
#
# Network: NET=devnet (default) or NET=local (a local validator, for rehearsing these scripts).
#
# BUILDS (the trust roots and the timers are compiled into the programs and the tools):
#   NET=devnet  features `devnet,short-timers`: YOUR roots + guardian (authority/src/network.rs) with demo
#               timers, so a whole market life fits in minutes. Never the localnet roots.
#   NET=local   feature `localnet`: throwaway roots whose private halves are in localnet/keys/ (gitignored).
#               Only ever for a local validator. 00-build.sh refuses to leave a localnet root in a devnet .so.
# Each network builds into its own directories so one can never be mistaken for the other.
#
# KEYS: nothing is baked in. On devnet you provide
#   DEPLOYER       keypair file that pays for everything and is the mint authority of the demo USDC
#   ROOT_A,ROOT_B  keypair files of two of the three root signers (needed by 01-setup.sh only)
#   CORE_KEYPAIR / ORACLE_KEYPAIR   default to target/deploy/metis_core-keypair.json and oracle-keypair.json
ENGINE=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ENGINE"

NET=${NET:-devnet}
case $NET in
  devnet) export RPC_URL=${RPC_URL:-https://api.devnet.solana.com}
          FEATURES_PROG="devnet,short-timers"
          TOOL_FEATURES="metis-ops/devnet,metis-ops/short-timers,oracle-keeper/devnet,oracle-keeper/short-timers" ;;
  local)  export RPC_URL=${RPC_URL:-http://127.0.0.1:8999}
          FEATURES_PROG="localnet"
          TOOL_FEATURES="metis-ops/localnet,oracle-keeper/localnet" ;;
  *) echo "NET must be devnet or local"; exit 2 ;;
esac
export KEEPER_RPC_URL=$RPC_URL

SO=$ENGINE/target/$NET-build                       # the programs for this network
export CARGO_TARGET_DIR=$ENGINE/target/bin-$NET   # tools + aggregator for this network
BIN=$CARGO_TARGET_DIR/debug

KEYS=$ENGINE/target/deploy                         # program keypairs: made by cargo build-sbf the first time, then reused
D=${DEMO_KEYS:-$ENGINE/devnet/keys-$NET}           # demo keys: operator, reporters, creator, bettors, mint (gitignored)
RUN=$D/run                                         # pidfiles, logs, mock-provider state, watch file
STATE=$D/state.env                                 # non-secret addresses written by 01-setup.sh
mkdir -p "$D" "$RUN"; chmod 700 "$D"

CORE_KEYPAIR=${CORE_KEYPAIR:-$KEYS/metis_core-keypair.json}
ORACLE_KEYPAIR=${ORACLE_KEYPAIR:-$KEYS/oracle-keypair.json}
if [ "$NET" = local ]; then
  # rehearsal only: throwaway roots, a throwaway deployer, program ids from the deploy keypairs
  LOCAL_ROOTS=$ENGINE/localnet/keys
  ROOT_A=${ROOT_A:-$LOCAL_ROOTS/root0.json}
  ROOT_B=${ROOT_B:-$LOCAL_ROOTS/root1.json}
  DEPLOYER=${DEPLOYER:-$D/deployer.json}
  CORE_KEYPAIR=${CORE_KEYPAIR:-$KEYS/metis_core-keypair.json}
  ORACLE_KEYPAIR=${ORACLE_KEYPAIR:-$KEYS/oracle-keypair.json}
fi

CORE=79uea7X34YHRUQFkLAR5rwupHQf6Z7ASbBGfzxRDu8ws
ORACLE=BM7jwCCXjBgRy5LPg1UG4iAPueAFK3bRxQRogD4Wzoub
MANIFEST=MNFSTqtC93rEfYHB6hF82sKdZpUDFWkViLByLd1k1Ms
OPS=$BIN/metis-ops
KEEPER=$BIN/oracle-keeper

# address of a demo key: the Solana CLI if there is one (this machine), else metis-ops (a slim server image)
P() { if command -v solana-keygen >/dev/null 2>&1; then solana-keygen pubkey "$D/$1.json"; else "$OPS" pubkey --keypair "$D/$1.json"; fi; }
mkkey() { [ -f "$D/$1.json" ] || { solana-keygen new --no-bip39-passphrase --silent -o "$D/$1.json"; chmod 600 "$D/$1.json"; }; }
sol() { solana balance "$1" -u "$RPC_URL" | awk '{print $1}'; }
ata() { spl-token address --token "$MINT" --owner "$1" --verbose -u "$RPC_URL" | awk '/Associated token address/{print $NF}'; }
# shellcheck disable=SC1090
# state.env holds addresses only; the RPC of the machine that wrote it must not override this machine's
_rpc=$RPC_URL
[ -f "$STATE" ] && . "$STATE"
# an older state.env stored the RPC (with its key): never let it override the one in the environment
export RPC_URL=$_rpc KEEPER_RPC_URL=$_rpc
true
