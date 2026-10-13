#!/usr/bin/env bash
# Packs the demo's HOT keys into one text blob for a hosting provider's secret variable
# (METIS_KEYS_B64), so the hosted worker can run the keepers and the crank without a terminal.
#
#   devnet/pack-secrets.sh
#
# What goes in: operator, the two reporters, creator, bettors, maker, treasury, the demo-USDC mint key,
# the Meteora keeper, the faucet, and state.env (addresses only). What stays OUT, always: the roots, the
# guardian and the deployer. Those can change or upgrade the programs; these can only run the demo.
# The blob is written to a file (not printed), because it is a secret.
set -euo pipefail
. "$(dirname "$0")/common.sh"
[ -f "$STATE" ] || { echo "run devnet/01-setup.sh first"; exit 1; }
OUT=$D/secrets.b64
FILES=(operator reporter1 reporter2 creator bettor1 bettor2 maker treasury usdc-mint dbc-keeper faucet)
for f in "${FILES[@]}"; do [ -f "$D/$f.json" ] || { echo "missing $D/$f.json"; exit 1; }; done
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
for f in "${FILES[@]}"; do cp "$D/$f.json" "$TMP/"; done
grep -v '^RPC_URL=' "$STATE" > "$TMP/state.env"     # the host sets its own RPC
# COPYFILE_DISABLE / --no-xattrs keep macOS metadata out of the bundle (it only produces warnings on the server)
COPYFILE_DISABLE=1 tar $(tar --help 2>&1 | grep -q -- --no-xattrs && echo --no-xattrs) $(tar --help 2>&1 | grep -q -- --no-mac-metadata && echo --no-mac-metadata) -czf - -C "$TMP" . | base64 | tr -d '\n' > "$OUT"
chmod 600 "$OUT"
echo "wrote $OUT ($(wc -c < "$OUT" | tr -d ' ') bytes). It contains the demo's hot keys: treat it as a secret."
echo "Set it as the worker's METIS_KEYS_B64 variable, then delete the file."
