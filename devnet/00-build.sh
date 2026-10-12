#!/usr/bin/env bash
# Builds the programs and every tool/aggregator binary for $NET, with the matching compiled-in roots and
# timers. Run once, and again after any program change. For NET=devnet it also proves the programs hold
# none of the throwaway localnet roots, and prints the hashes to compare with what you deploy.
set -euo pipefail
. "$(dirname "$0")/common.sh"
mkdir -p "$SO"
FEAT=(--features "$FEATURES_PROG")
cargo build-sbf --manifest-path programs/core/Cargo.toml   "${FEAT[@]}" --sbf-out-dir "$SO"
cargo build-sbf --manifest-path programs/oracle/Cargo.toml "${FEAT[@]}" --sbf-out-dir "$SO"
cargo build -p metis-ops -p oracle-keeper --features "$TOOL_FEATURES"
cargo build -p metis-aggregator --features "$FEATURES_PROG" --bin metis-api
if [ "$NET" = local ] && [ ! -f target/deploy/manifest.so ]; then
  solana program dump "$MANIFEST" target/deploy/manifest.so --url mainnet-beta
fi

if [ "$NET" = devnet ]; then
  python3 -I - "$SO/metis_core.so" "$SO/oracle.so" <<'PY'
import re, sys
A = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
def b58(s):
    n = 0
    for c in s: n = n * 58 + A.index(c)
    return n.to_bytes(32, "big")
src = open("authority/src/network.rs").read()
local = src.split('#[cfg(feature = "localnet")]')[1].split("#[cfg(feature")[0]
bad = re.findall(r'from_str_const\("([1-9A-HJ-NP-Za-km-z]{32,44})"\)', local)
assert len(bad) == 4, "could not find the 4 localnet keys in authority/src/network.rs"
for path in sys.argv[1:]:
    blob = open(path, "rb").read()
    for k in bad:
        if b58(k) in blob:
            sys.exit(f"REFUSING: {path} contains the localnet key {k}. Do not deploy this build.")
print("ok: no localnet root or guardian inside the devnet programs")
PY
fi
echo; echo "addresses of this build (they follow from the program ids and the roots compiled in):"; "$OPS" addresses
echo
ls -l "$SO"/*.so
shasum -a 256 "$SO"/metis_core.so "$SO"/oracle.so
