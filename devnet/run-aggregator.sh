#!/usr/bin/env bash
# Runs the aggregator with Metis (devnet) + Panta (mainnet).
#
# The Panta key is read from aggregator/.env by THIS script and handed to the aggregator by variable name
# only: the script never prints it, and neither does the aggregator. Only PANTA_API_KEY is taken from that
# file. Everything else is set here, so a stale line in .env (an old venue list, a database URL) cannot
# change what the demo runs.
#
#   ENABLED_VENUES   default panta,metis         (NET=local: metis only, no Panta call is made)
#   USE_DATABASE=1   also use DATABASE_URL from aggregator/.env (default: in memory)
#   PORT             default 8080
#   EXECUTION_ENABLED / MARKET_CREATION_ENABLED   default true (see below)
set -euo pipefail
. "$(dirname "$0")/common.sh"
[ -f "$STATE" ] || { echo "run devnet/01-setup.sh first"; exit 1; }
CFG=$("$OPS" addresses | awk '/^config:/{print $2}')

ENV_FILE=$ENGINE/aggregator/.env
panta_key=""; database_url=""
if [ -f "$ENV_FILE" ]; then
  # read the named values in a subshell so nothing else from the file leaks into this environment
  panta_key=$(set -a; . "$ENV_FILE" >/dev/null 2>&1; printf '%s' "${PANTA_API_KEY:-}")
  [ "${USE_DATABASE:-}" = 1 ] && database_url=$(set -a; . "$ENV_FILE" >/dev/null 2>&1; printf '%s' "${DATABASE_URL:-}")
fi

DEFAULT_VENUES=panta,metis; [ "$NET" = local ] && DEFAULT_VENUES=metis
export ENABLED_VENUES=${ENABLED_VENUES:-$DEFAULT_VENUES}
case ",$ENABLED_VENUES," in *,panta,*) [ -n "$panta_key" ] || { echo "panta is enabled but PANTA_API_KEY is not in aggregator/.env"; exit 1; } ;; esac
export PANTA_API_KEY=$panta_key
export METIS_RPC_URL=$RPC_URL METIS_PROGRAM_ID=$CORE METIS_CONFIG=$CFG
export REFRESH_INTERVAL_SECS=${REFRESH_INTERVAL_SECS:-30}
export PORT=${PORT:-8080}
# The demo is about building and signing real transactions, so these default ON here whatever the .env
# says (a .env written for read-only browsing has them false). Set them to false to run read-only.
export EXECUTION_ENABLED=${EXECUTION_ENABLED:-true}
export MARKET_CREATION_ENABLED=${MARKET_CREATION_ENABLED:-true}
export DATABASE_URL=$database_url      # empty = in memory
exec "$BIN/metis-api"
