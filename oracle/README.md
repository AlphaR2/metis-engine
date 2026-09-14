# metis-oracle

The Oracle is a Pinocchio program that records football settlement data and
manages the authorities allowed to create and update it. Its public interface
is described in Rust with Shank derives; Shank extracts the program IDL.

## Build and generate the IDL
Today, `settle_market` (in `programs/core`) checks a single hardcoded
`constants::AUTHORITY` pubkey — a placeholder, not a real oracle. This crate
is where a real one gets built, as a genuinely external integration rather
than an internal module of `metis-core` (mirroring how a reference
implementation treats external price/result oracles as separate programs it
CPIs into, not code it owns).

Install the Shank CLI once. Version `0.4.9` is required because this crate
uses Rust edition 2024.

```bash
cargo install shank-cli --version 0.4.9 --locked
```

From this directory, build the program and extract the IDL:

```bash
cargo build-sbf
shank idl --crate-root . --out-dir ../idl
```

The generated IDL is committed project output. Do not edit it by hand; change
the Shank instruction schema in `src/instructions/mod.rs` or the Shank
account/type derives, then run Shank again.

## TypeScript SDK

The TypeScript client lives in `../clients/typescript`. Shank produces a
standard Anchor-style IDL, which Codama can consume and render for Solana Kit.
Its `codama.json` reads `../../idl/oracle.json` and renders the SDK into
`oracle-sdk/src/generated`.

Install its JavaScript dependencies once:

```bash
cd ../clients/typescript
pnpm install
```

Generate the SDK:

```bash
pnpm exec codama run js
```

The generated source is available at:

```text
clients/typescript/oracle-sdk/src/generated/
```

It contains typed instruction builders, account decoders, program constants,
and the public enums used by the Oracle. Treat this directory as generated
code: make interface changes in the Rust program, then regenerate it rather
than editing TypeScript output directly.

`CloseOracle` has a deliberate conditional signer: `initiator` signs only when
creating a close proposal, while `voter` signs the later quorum votes. A static
IDL cannot express that condition, so the generated builder marks `initiator`
as writable. For the first close transaction, the caller must mark its
initiator account meta as a signer; for the two vote transactions, use the
generated `voter` signer normally.

## Full regeneration workflow

When an instruction, account layout, or public enum changes:

```bash
# 1. From oracle/
cargo build-sbf
shank idl --crate-root . --out-dir ../idl

# 2. From clients/typescript/
pnpm exec codama run js

# 3. From the workspace root
cargo test -p oracle -- --nocapture
```

## Tests

Run the Oracle's fast LiteSVM tests from the workspace root:

```bash
cargo test -p oracle -- --nocapture
```
