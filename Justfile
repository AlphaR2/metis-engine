# Per-box build/test/bench ergonomics. Run `just` with no args to see this
# list. Every crate name below matches its Cargo.toml [package] name.

# Run a specific crate's unit tests (no VM needed for anything but core).
test crate:
    cargo test -p openstake-{{crate}}

# Run every crate's unit tests.
test-all:
    cargo test --workspace --exclude openstake-core
    cargo test -p openstake-core --lib

# Run a specific crate's benchmarks.
bench crate:
    cargo bench -p openstake-{{crate}}

# Build the one real deployable program.
build-core:
    cargo build-sbf --manifest-path programs/core/Cargo.toml

# Full end-to-end litesvm integration suite — needs build-core first.
test-core: build-core
    cargo test -p openstake-core

# Everything: unit tests across the workspace, then the full program build
# and integration suite.
verify: test-all test-core
