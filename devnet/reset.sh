#!/usr/bin/env bash
# One-command demo reset: stop the processes, clear the demo's local state (fixtures, watch list, mock
# scores, market log), restart the processes and create a fresh standing board. Markets already on
# chain stay there (a chain cannot be wiped); the new ones use fresh fixtures and nonces.
set -euo pipefail
. "$(dirname "$0")/common.sh"
"$ENGINE/devnet/stop.sh" || true
rm -f "$RUN"/fixtures.tsv "$RUN"/markets.tsv "$RUN"/watch.json "$RUN"/mock-*.json "$RUN"/*.log
echo '[]' > "$RUN/watch.json"
NO_RESTART=1 "$ENGINE/devnet/03-markets.sh" seed
"$ENGINE/devnet/02-start.sh"
[ "${LIVE:-1}" = 1 ] && NO_RESTART=1 "$ENGINE/devnet/03-markets.sh" live 330 && "$ENGINE/devnet/02-start.sh" >/dev/null
echo "reset done"
