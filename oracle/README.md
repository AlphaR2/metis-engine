# Openstake Oracle

The Oracle is a Pinocchio program that records football settlement data and
manages the authorities allowed to create and update it. Its public interface
is described in Rust with Codama derives, not an Anchor IDL or Shank.

## Build and generate the IDL

`build.rs` is a Cargo build script; do not run it as `cargo build.rs`. Cargo
runs it automatically whenever the Oracle is built. It reads the Codama
metadata from the program source and writes the generated IDL to
`../idl/oracle.json`.

From this directory:

```bash
cargo build-sbf
```

Verify that the IDL was generated:

```bash
ls -lh ../idl/oracle.json
```

The generated IDL is committed project output. Do not edit it by hand; change
the Codama schema in `src/instructions/mod.rs` or the Codama account/type
derives, then rebuild the program.

## TypeScript SDK

The TypeScript client lives in `../clients/typescript`. Its `codama.json`
consumes `../../idl/oracle.json` and renders the SDK into
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

## Full regeneration workflow

When an instruction, account layout, or public enum changes:

```bash
# 1. From oracle/
cargo build-sbf

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
