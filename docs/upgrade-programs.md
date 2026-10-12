# Upgrading a deployed program in place

The devnet programs are deployed as upgradeable, with the deployer as upgrade authority. An upgrade replaces the code
at the **same program address**: accounts, markets and the config are untouched, and clients need no change.

Use this when the program's source changed but its interface did not (for example the code was reorganised, or a
fix was made). It is not a way to change account layouts.

## What changed in this upgrade

The Open Pool code moved into `programs/core/src/dbc/` and a shared `metis-dbc` crate. Behaviour, instruction ids and
account layouts are identical (a test pins the four Open Pool instruction ids). Only the **core** program's binary
changed; the oracle program's source did not.

## Steps (run from the repository root)

1. **Export your RPC and deployer** (the RPC URL contains an API key: do not paste it anywhere shared).

   ```bash
   export RPC_URL='<your devnet RPC>'
   export DEPLOYER=/absolute/path/to/your-deployer.json
   ```

2. **Build the devnet programs.**

   ```bash
   devnet/00-build.sh
   ```

   It ends with `ok: no localnet root or guardian inside the devnet programs` and prints a SHA-256 for each program.
   Keep the core program's hash: it is the file you are about to deploy.

3. **Check who can upgrade, and the current size.**

   ```bash
   solana program show <CORE_PROGRAM_ID> -u "$RPC_URL"
   ```

   `Authority` must be your deployer's address. If it is not, stop: that key cannot upgrade it.

4. **Back up what is deployed now** (so you can roll back).

   ```bash
   solana program dump <CORE_PROGRAM_ID> core-before-upgrade.so -u "$RPC_URL"
   ```

5. **Make sure the deployer has enough SOL.** An upgrade uploads a temporary buffer (about as much SOL as the first deploy's
   buffer, refunded afterwards) and, if the new binary is larger, pays a little extra rent. Check
   `solana balance "$(solana-keygen pubkey "$DEPLOYER")" -u "$RPC_URL"`; about 1.5 SOL is comfortable.

6. **Upgrade the core program.**

   ```bash
   solana program deploy target/devnet-build/metis_core.so \
     --program-id <CORE_PROGRAM_ID> \
     --upgrade-authority "$DEPLOYER" --keypair "$DEPLOYER" -u "$RPC_URL"
   ```

   If it reports that the program data account is too small, extend it by the shortfall and run it again:
   `solana program extend <CORE_PROGRAM_ID> <extra bytes> --keypair "$DEPLOYER" -u "$RPC_URL"`.

7. **Verify.** Dump the program again and compare it to the file you built (the dump may be zero-padded, so compare the
   first bytes):

   ```bash
   solana program dump <CORE_PROGRAM_ID> core-after-upgrade.so -u "$RPC_URL"
   shasum -a 256 core-after-upgrade.so target/devnet-build/metis_core.so
   ```

   Then `devnet/verify.sh` should still print `ALL CHECKS PASSED`.

8. **Redeploy the hosted services from the same commit** (the worker and the API embed tools built from this source).
   The program address and instruction ids are unchanged, so nothing else needs reconfiguring.

## Rolling back

`solana program deploy core-before-upgrade.so --program-id <CORE_PROGRAM_ID> --upgrade-authority "$DEPLOYER" --keypair "$DEPLOYER" -u "$RPC_URL"`

## The oracle program

Its source did not change in this upgrade, so there is nothing to do. To be sure, dump it, and compare with
`target/devnet-build/oracle.so`; if the hashes match, skip it.
