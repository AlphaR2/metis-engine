#!/usr/bin/env bash
# Read-only. Reads the cluster and prints a PASS/FAIL line per check; sends nothing and needs no signer.
# Run it after 01-setup.sh and paste the output back. Exit status is non-zero if anything failed.
set -uo pipefail
. "$(dirname "$0")/common.sh"
FAIL=0
ok()   { echo "PASS  $*"; }
bad()  { echo "FAIL  $*"; FAIL=1; }
acct() { solana account "$1" -u "$RPC_URL" --output json 2>/dev/null; }
exists() { acct "$1" | python3 -I -c "import sys,json; d=json.load(sys.stdin)['account']; print('exec' if d['executable'] else 'data')" 2>/dev/null; }

echo "network $NET  rpc $(printf %s "$RPC_URL" | sed -E "s#^(https?://[^/?]+).*#\1#")"   # host only: provider URLs carry the API key
for row in "core:$CORE" "oracle:$ORACLE" "manifest:$MANIFEST"; do n=${row%%:*}; a=${row#*:}
  [ "$(exists "$a")" = exec ] && ok "$n program $a is deployed and executable" || bad "$n program $a is missing"
done

# upgrade authority of each program (who can change the code): decoded from the program-data account
for row in "core:$CORE" "oracle:$ORACLE"; do n=${row%%:*}; a=${row#*:}
  python3 -I - "$a" "$RPC_URL" "$n" <<'PY'
import base64, json, subprocess, sys
A = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
def b58e(b):
    n = int.from_bytes(b, "big"); s = ""
    while n: n, r = divmod(n, 58); s = A[r] + s
    return "1" * (len(b) - len(b.lstrip(b"\0"))) + s
def get(a, url):
    o = subprocess.run(["solana", "account", a, "-u", url, "--output", "json"], capture_output=True, text=True)
    return json.loads(o.stdout)["account"] if o.returncode == 0 else None
prog, url, name = sys.argv[1:]
acc = get(prog, url)
if not acc: print(f"FAIL  {name}: program account unreadable"); sys.exit(0)
if acc["owner"] != "BPFLoaderUpgradeab1e11111111111111111111111":
    print(f"INFO  {name}: not an upgradeable program (preloaded on a local validator)"); sys.exit(0)
d = acc["data"][0]
raw = base64.b64decode(d)                      # Program{ u32 tag=2, programdata: Pubkey }
pd = b58e(raw[4:36])
acc2 = get(pd, url)
if not acc2: print(f"FAIL  {name}: program-data unreadable"); sys.exit(0)
raw = base64.b64decode(acc2["data"][0])                     # ProgramData{ u32 tag=3, slot u64, Option<Pubkey> }
auth = b58e(raw[13:45]) if raw[12] == 1 and any(raw[13:45]) else None
print(f"INFO  {name} upgrade authority: {auth or 'NONE (immutable)'}")
PY
done

[ -f "$STATE" ] || { bad "no $STATE: 01-setup.sh has not run on this machine"; exit 1; }
[ -x "$OPS" ] || { bad "tools not built for $NET (run devnet/00-build.sh)"; exit 1; }
ADDR=$("$OPS" addresses 2>&1)
CFG=$(echo "$ADDR" | awk '/^config:/{print $2}'); AUTH=$(echo "$ADDR" | awk '/^oracle authority:/{print $3}')
[ -n "$CFG" ] && [ "$(exists "$CFG")" = data ] && ok "market-program Config $CFG exists" || bad "Config account missing"
[ -n "$AUTH" ] && [ "$(exists "$AUTH")" = data ] && ok "oracle authority account $AUTH exists" || bad "oracle authority account missing"
[ "$(exists "$MINT")" = data ] && ok "demo USDC mint $MINT exists" || bad "demo USDC mint missing"
for k in operator reporter1 reporter2; do
  [ -f "$D/$k.json" ] && B=$(solana balance "$(P $k)" -u "$RPC_URL" 2>/dev/null | awk '{print $1}') && ok "$k $(P $k) holds ${B:-?} SOL" || bad "$k key missing"
done
echo "$ADDR"
[ $FAIL = 0 ] && echo "ALL CHECKS PASSED" || echo "SOME CHECKS FAILED"
exit $FAIL
