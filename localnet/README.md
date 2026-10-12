# Local run (validator + programs + operator tools + aggregator)

Scripts: `validator.sh` (clean validator), `setup.sh` (keys, mint, trust accounts, attestations),
`new_market.sh <nonce> <match id> [lead secs]` (fixture, market, two bets), `mock_provider.py`
(stands in for football-data.org, driven by a state file), `scenario_fallback.sh` (a stranger
settling after the operator grace). `just localnet-build | localnet-validator | localnet-setup |
localnet-aggregator`. The `localnet` build shortens every timer (see docs/architecture.md), so a
whole market life takes about ten minutes. Betting is open only for `lead - 120` seconds, and
`new_market.sh` takes about 70 seconds, so use a lead of at least 240.

Everything here uses throwaway keys in `localnet/keys/` (gitignored). The `localnet` build feature
compiles those keys in as the trust roots; never deploy that build anywhere real.

```bash
# 0. once: build the programs with the localnet roots, and fetch the real Manifest
cargo build-sbf --manifest-path programs/core/Cargo.toml   --features localnet --sbf-out-dir target/localnet
cargo build-sbf --manifest-path programs/oracle/Cargo.toml --features localnet --sbf-out-dir target/localnet
solana program dump MNFSTqtC93rEfYHB6hF82sKdZpUDFWkViLByLd1k1Ms target/deploy/manifest.so --url mainnet-beta
cargo build -p metis-ops -p oracle-keeper --features metis-ops/localnet,oracle-keeper/localnet

# 1. validator (own terminal)
localnet/validator.sh

# 2. set up (each command is one real transaction)
export RPC_URL=http://127.0.0.1:8899 KEEPER_RPC_URL=http://127.0.0.1:8899
K=localnet/keys; P() { solana-keygen pubkey $K/$1.json; }
# fund keys with `solana airdrop`, create a 6-decimals USDC mint (localnet/keys/usdc-mint.json) with spl-token
target/debug/metis-ops oracle-init --root $K/root0.json
for who in reporter1 reporter2; do for r in root0 root1; do
  target/debug/metis-ops attest --program oracle --root $K/$r.json --candidate $(P $who); done; done
target/debug/metis-ops config-init --root-a $K/root0.json --root-b $K/root1.json --mint $(P usdc-mint) --treasury $(P treasury)
for r in root0 root1; do target/debug/metis-ops attest --program core --root $K/$r.json --candidate $(P operator); done

# 3. a match and a market (kickoff must be 5h+ away; betting is open only until kickoff-5h)
KEEPER_KEYPAIR=$K/reporter1.json target/debug/oracle-keeper create-fixture --kickoff $(( $(date +%s) + 5*3600 + 150 )) --id football-data=1001
target/debug/metis-ops create-market --creator $K/creator.json --mint $(P usdc-mint) --oracle <oracle from above> --nonce 1 \
  --condition home-win --question "..." --rubric-yes "..." --rubric-no "..." --label-yes "..." --label-no "..."
target/debug/metis-ops bet --bettor $K/bettor1.json --market <market> --mint $(P usdc-mint) --side 0 --usdc 20

# 4. the crank (attested operator key) opens the order books once betting ends
KEEPER_KEYPAIR=$K/operator.json target/debug/oracle-keeper crank --once

# 5. the aggregator, in-memory (no database), pointed at the validator
DATABASE_URL= ENABLED_VENUES=metis METIS_RPC_URL=http://127.0.0.1:8899 REFRESH_INTERVAL_SECS=30 \
  cargo run -p metis-aggregator --features localnet --bin metis-api
curl localhost:8080/v1/markets?venue=metis     # POST /refresh to refresh at once
```
