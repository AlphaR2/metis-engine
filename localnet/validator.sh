#!/usr/bin/env bash
# Starts a clean local validator with the localnet builds of both programs and the real Manifest.
# Foreground; run it in its own terminal (or with &). Ledger: localnet/ledger (wiped each start).
set -euo pipefail
cd "$(dirname "$0")/.."
for f in target/localnet/metis_core.so target/localnet/oracle.so target/deploy/manifest.so; do
  [ -f "$f" ] || { echo "missing $f - run: just localnet-build (and just fetch-manifest)"; exit 1; }
done
CORE=79uea7X34YHRUQFkLAR5rwupHQf6Z7ASbBGfzxRDu8ws
ORACLE=BM7jwCCXjBgRy5LPg1UG4iAPueAFK3bRxQRogD4Wzoub
MANIFEST=MNFSTqtC93rEfYHB6hF82sKdZpUDFWkViLByLd1k1Ms
rm -rf localnet/ledger
exec solana-test-validator --reset --quiet --ledger localnet/ledger \
  --bpf-program $CORE target/localnet/metis_core.so \
  --bpf-program $ORACLE target/localnet/oracle.so \
  --bpf-program $MANIFEST target/deploy/manifest.so
