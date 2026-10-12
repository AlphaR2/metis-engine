#!/usr/bin/env bash
# Prints how much SOL the DEPLOYER needs for the whole one-time setup, from the real size of the built
# programs and the cluster's rent. Read-only. Run after 00-build.sh, fund once, then run 01-setup.sh.
#   devnet/cost.sh            human readable
#   devnet/cost.sh --number   just the recommended amount (what 01-setup.sh checks)
set -euo pipefail
. "$(dirname "$0")/common.sh"
for f in "$SO/metis_core.so" "$SO/oracle.so"; do [ -f "$f" ] || { echo "missing $f (run devnet/00-build.sh)"; exit 1; }; done
rent() { solana rent "$1" -u "$RPC_URL" | awk '/Rent-exempt minimum/{print $3}'; }   # SOL
CL=$(stat -f%z "$SO/metis_core.so" 2>/dev/null || stat -c%s "$SO/metis_core.so")
OL=$(stat -f%z "$SO/oracle.so"     2>/dev/null || stat -c%s "$SO/oracle.so")
python3 -I - "$CL" "$OL" "$(rent $((45+CL)))" "$(rent $((45+OL)))" "$(rent $((37+CL)))" "$(rent $((37+OL)))" "$(rent 36)" "${1:-}" <<'PY'
import sys
cl, ol, pdc, pdo, bufc, bufo, prog, mode = sys.argv[1:]
cl, ol = int(cl), int(ol); pdc, pdo, bufc, bufo, prog = map(float, (pdc, pdo, bufc, bufo, prog))
stay = pdc + pdo + 2 * prog                 # programdata + program accounts stay locked
transient = max(bufc, bufo)                 # the upload buffer is refunded after each deploy
demo_keys = 0.25 + 0.20 + 0.04 + 0.04 + 0.03 + 0.03 + 0.005 + 0.60 + 0.50 + 0.10   # 01-setup.sh funding, incl. the two roots
mint_and_accounts = 0.03                    # demo USDC mint + 5 token accounts + config/authority/delegate PDAs
fees = 0.05                                 # ~2000 write transactions at 5000 lamports are covered here
need = stay + transient + demo_keys + mint_and_accounts + fees
rec = round(need * 1.10 + 0.05, 2)
if mode == "--number":
    print(rec)
else:
    print(f"core.so   {cl:>8} bytes  stays locked {pdc+prog:.3f} SOL")
    print(f"oracle.so {ol:>8} bytes  stays locked {pdo+prog:.3f} SOL")
    print(f"upload buffer (refunded)              {transient:.3f} SOL")
    print(f"demo keys, roots, mint, fees          {demo_keys+mint_and_accounts+fees:.3f} SOL")
    print(f"-> fund the DEPLOYER with at least    {rec} SOL")
PY
