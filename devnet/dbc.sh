#!/usr/bin/env bash
# The Meteora (DBC) keeper for crowd-seeded markets. Wraps dbc/keeper with the keys and addresses
# 01-setup.sh saved. Nothing secret is passed on a command line: only key FILE paths.
#
#   dbc.sh install                     npm install the keeper once
#   dbc.sh reserve <nonce> <seed_usdc> reserve the backer token for a market about to be created
#   dbc.sh run                         start the keeper service in the background (watch loop + HTTP :8090)
#   dbc.sh stop
#   dbc.sh serve                       the same service in the foreground (a container's main process)
#   dbc.sh status <market>             raise progress and market status
#   dbc.sh back <market> <usdc> [key]  buy the backer token with a demo wallet (default bettor1)
#   dbc.sh drive <market>              one pass of the state machine for one market
#   dbc.sh claim-vested <market>       after the cliff: the vested liquidity slice -> treasury
#   dbc.sh logs
# Meteora's DBC and DAMM v2 are the REAL programs on devnet. The raise is sized seed / 0.888 so the
# unlocked 89% of the migrated liquidity covers the seed.
set -euo pipefail
. "$(dirname "$0")/common.sh"
[ -f "$STATE" ] || { echo "run devnet/01-setup.sh first"; exit 1; }
cd "$ENGINE/dbc/keeper"
export RPC_URL KEEPER_KEYPAIR=$D/dbc-keeper.json OPERATOR_KEYPAIR=$D/operator.json QUOTE_MINT=$MINT \
  TREASURY=$(P treasury) CORE_PROGRAM=$CORE OPS_BIN=$OPS STATE_FILE=$D/dbc-state.json \
  CLIFF_SECS=${CLIFF_SECS:-172800} VEST_PERIOD_SECS=${VEST_PERIOD_SECS:-86400} \
  TOKEN_URI=${TOKEN_URI:-https://metis.example/backer-token.json} DEMO_RUN_DIR=$RUN DEMO_SCRIPTS_DIR=$ENGINE/devnet FAUCET_KEYPAIR=$D/faucet.json
case ${1:?command} in
  install) npm ci --no-audit --no-fund ;;
  reserve) npx tsx src/cli.ts reserve --creator "$(P creator)" --nonce "${2:?nonce}" --seed "${3:?seed usdc}" ;;
  run) "$0" stop >/dev/null 2>&1 || true
       nohup npx tsx src/cli.ts serve --port "${PORT_DBC:-8090}" --every "${EVERY:-8}" > "$RUN/dbc-keeper.log" 2>&1 & echo $! > "$RUN/dbc-keeper.pid"
       sleep 3; kill -0 "$(cat "$RUN/dbc-keeper.pid")" 2>/dev/null && echo "up   dbc-keeper (log: $RUN/dbc-keeper.log)" || { echo "DOWN dbc-keeper"; tail -5 "$RUN/dbc-keeper.log"; exit 1; } ;;
  stop) [ -f "$RUN/dbc-keeper.pid" ] && kill "$(cat "$RUN/dbc-keeper.pid")" 2>/dev/null || true; rm -f "$RUN/dbc-keeper.pid"; pkill -f "src/cli.ts serve" 2>/dev/null || true; echo stopped ;;
  status) npx tsx src/cli.ts progress --market "${2:?market}"; "$OPS" show --market "$2" | sed -n 1,3p ;;
  back) npx tsx src/cli.ts back --market "${2:?market}" --buyer "$D/${4:-bettor1}.json" --usdc "${3:?usdc}" ;;
  drive) npx tsx src/cli.ts drive --market "${2:?market}" ;;
  claim-vested) npx tsx src/cli.ts claim-vested --market "${2:?market}" ;;
  serve) exec npx tsx src/cli.ts serve --port "${PORT:-8090}" --every "${EVERY:-8}" ;;   # foreground: what a container runs
  logs) tail -n "${2:-40}" "$RUN/dbc-keeper.log" ;;
  *) sed -n 2,16p "$0"; exit 2 ;;
esac
