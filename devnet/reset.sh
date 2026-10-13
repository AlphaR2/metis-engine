#!/usr/bin/env bash
# Demo "reset": creates a fresh standing board (new matches, new markets) and one live match, then restarts the
# keepers so they pick the new matches up. Markets already on chain stay there (a chain cannot be wiped); the new ones
# get new match ids and nonces. It does NOT forget the earlier matches: the keepers keep watching them, so any market
# still open from before can still settle.
set -euo pipefail
. "$(dirname "$0")/common.sh"
NO_RESTART=1 "$ENGINE/devnet/03-markets.sh" seed
"$ENGINE/devnet/02-start.sh"
[ "${LIVE:-1}" = 1 ] && NO_RESTART=1 "$ENGINE/devnet/03-markets.sh" live 330 && "$ENGINE/devnet/02-start.sh" >/dev/null
echo "reset done"
