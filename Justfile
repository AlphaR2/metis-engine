# Per-box build/test/bench ergonomics. Run `just` with no args to see this
# list. Every crate name below matches its Cargo.toml [package] name.

# Run a specific crate's unit tests (no VM needed for anything but core).
test crate:
    cargo test -p metis-{{crate}}

# Run every crate's unit tests.
test-all:
    cargo test --workspace --exclude metis-core
    cargo test -p metis-core --lib

# Run a specific crate's benchmarks.
bench crate:
    cargo bench -p metis-{{crate}}

# Build the one real deployable program.
build-core:
    cargo build-sbf --manifest-path programs/core/Cargo.toml

# Fetch the real Manifest program (read-only, from mainnet) so the close_primary tests can run
# against it. Without it those tests skip with a loud message.
fetch-manifest:
    solana program dump MNFSTqtC93rEfYHB6hF82sKdZpUDFWkViLByLd1k1Ms target/deploy/manifest.so --url mainnet-beta

# Build the oracle program (its litesvm tests load target/deploy/oracle.so).
build-oracle:
    cargo build-sbf --manifest-path programs/oracle/Cargo.toml

# Build every on-chain program.
build-all: build-core build-oracle

# Every on-chain program's full suite, unit + litesvm. Builds the .so files first.
test-programs: build-all
    cargo test -p metis-authority
    cargo test -p metis-core
    cargo test -p metis-oracle

# The oracle keeper and the crank: pure logic, provider parsers, and end-to-end runs against the
# real oracle and core programs in litesvm. The crank's tests also use the real Manifest program
# (`just fetch-manifest` once); without it they skip with a loud message.
test-keeper: build-oracle build-core
    cargo test -p oracle-keeper

# Prove the network key sets are guarded: these must FAIL to compile until the real
# devnet/mainnet keys are filled in authority/src/network.rs.
check-network-keys:
    ! cargo check -p metis-core --features devnet
    ! cargo check -p metis-core --features mainnet

# Full end-to-end litesvm integration suite — needs build-core first.
test-core: build-core
    cargo test -p metis-core

# Everything: unit tests across the workspace, then the full program build
# and integration suite.
verify: test-all test-core

# ── Local rehearsal (throwaway keys, shortened clocks; see localnet/README.md) ──

# Programs with the localnet roots and shortened timers, plus the operator tools.
localnet-build:
    cargo build-sbf --manifest-path programs/core/Cargo.toml   --features localnet --sbf-out-dir target/localnet
    cargo build-sbf --manifest-path programs/oracle/Cargo.toml --features localnet --sbf-out-dir target/localnet
    cargo build -p metis-ops -p oracle-keeper --features metis-ops/localnet,oracle-keeper/localnet

# A fresh local validator with both programs and the real Manifest (foreground).
localnet-validator:
    localnet/validator.sh

# One-time setup on that validator: keys, USDC mint, trust accounts, attestations.
localnet-setup:
    localnet/setup.sh

# The aggregator against the local validator, in-memory (no database).
localnet-aggregator:
    DATABASE_URL= ENABLED_VENUES=metis METIS_RPC_URL=http://127.0.0.1:8899 REFRESH_INTERVAL_SECS=30 \
      cargo run -p metis-aggregator --features localnet --bin metis-api

# ── Deploying (devnet/mainnet): see deploy/deploy.sh for the environment it needs ──

deploy network step:
    deploy/deploy.sh {{network}} {{step}}

# ── Everything CI runs ──
ci: build-all
    cargo clippy --workspace --all-targets
    cargo test --workspace
    cargo check -p metis-core -p metis-oracle --features metis-core/localnet,metis-oracle/localnet
