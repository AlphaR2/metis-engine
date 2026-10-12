#!/usr/bin/env bash
# A mainnet-fork "surfnet" with our two localnet programs written at their real ids; the real Manifest
# program is fetched live from mainnet on first use. RPC on :8898.
set -euo pipefail
cd "$(dirname "$0")/.."
pkill -f "surfpool start" 2>/dev/null || true; sleep 1
# `-u` explicitly: with `-n mainnet` surfpool's own fetches were failing here.
(nohup surfpool start --no-tui --no-deploy -p 8898 -w 8902 -u https://api.mainnet-beta.solana.com > localnet/surfpool.log 2>&1 &)
for i in $(seq 1 40); do solana cluster-version -u http://127.0.0.1:8898 >/dev/null 2>&1 && break; sleep 2; done
python3 - <<'PY'
import json,urllib.request
def rpc(method,params):
    req=urllib.request.Request("http://127.0.0.1:8898",json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode(),{"content-type":"application/json"})
    return json.load(urllib.request.urlopen(req,timeout=60))
# Written as non-upgradeable programs at the ids compiled into the binaries (no program keypair needed).
for pid,so in [("79uea7X34YHRUQFkLAR5rwupHQf6Z7ASbBGfzxRDu8ws","target/localnet/metis_core.so"),("BM7jwCCXjBgRy5LPg1UG4iAPueAFK3bRxQRogD4Wzoub","target/localnet/oracle.so")]:
    rpc("surfnet_setAccount",[pid,{"lamports":10_000_000_000,"data":open(so,"rb").read().hex(),"owner":"BPFLoader2111111111111111111111111111111111","executable":True,"rentEpoch":0}])
print("programs written")
PY
