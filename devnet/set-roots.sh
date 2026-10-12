#!/usr/bin/env bash
# Fills the devnet trust roots into authority/src/network.rs from PUBLIC keys only. Run once, before
# building the devnet programs. The private keys never touch this machine.
#
#   devnet/set-roots.sh <root1> <root2> <root3> <guardian>
#
# The three roots are the keys that, two at a time, can set up and change the programs (oracle-init,
# config-init, attesting operators and reporters, overrides, pause). The guardian can only co-sign an
# unpause and must NOT be one of the roots. The root set is compiled in: changing it later means
# upgrading the programs, so choose the four holders carefully.
set -euo pipefail
cd "$(dirname "$0")/.."
[ $# -eq 4 ] || { sed -n 2,13p "$0"; exit 2; }
python3 -I - "$@" <<'PY'
import re, sys
A = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
def b58(s):
    if not 32 <= len(s) <= 44: raise SystemExit(f"not a Solana address (32 to 44 characters): {s}")
    n = 0
    for c in s:
        if c not in A: raise SystemExit(f"not base58: {s}")
        n = n * 58 + A.index(c)
    raw = n.to_bytes(32, "big") if n < 2**256 else None
    lead = len(s) - len(s.lstrip("1"))
    if raw is None or lead and raw[:lead] != b"\0" * lead: raise SystemExit(f"not a 32-byte address: {s}")
    return raw
roots, guardian = sys.argv[1:4], sys.argv[4]
for k in sys.argv[1:]: b58(k)
if len(set(sys.argv[1:])) != 4: raise SystemExit("the 3 roots and the guardian must be 4 different keys")
path = "authority/src/network.rs"
src = open(path).read()
known = set(re.findall(r'from_str_const\("([1-9A-HJ-NP-Za-km-z]{32,44})"\)', src))
# the localnet and test keys are in this file already: none of them may be reused for devnet
dev_start = src.index('#[cfg(feature = "devnet")]\nmod keys {')
dev_end = src.index('#[cfg(feature = "mainnet")]')
others = set(re.findall(r'from_str_const\("([1-9A-HJ-NP-Za-km-z]{32,44})"\)', src[:dev_start] + src[dev_end:]))
clash = others & set(sys.argv[1:])
if clash: raise SystemExit(f"refusing: {sorted(clash)} is a localnet/test key that is not held by you")
block = '''#[cfg(feature = "devnet")]
mod keys {
    use pinocchio::Address;

    // The devnet roots and guardian, set by devnet/set-roots.sh. Public keys only: their holders keep the
    // private halves. The root set is compiled in on purpose: changing it means upgrading the programs.
    pub const ROOT_SIGNERS: [Address; 3] = [
        Address::from_str_const("%s"),
        Address::from_str_const("%s"),
        Address::from_str_const("%s"),
    ];
    pub const GUARDIAN: Address = Address::from_str_const("%s");
}

''' % (roots[0], roots[1], roots[2], guardian)
open(path, "w").write(src[:dev_start] + block + src[dev_end:])
print("devnet roots set:")
for i, r in enumerate(roots): print(f"  root{i+1}:   {r}")
print(f"  guardian: {guardian}")
PY
cargo check -p metis-authority --features devnet 2>&1 | tail -2
