# Devnet setup (on-chain)

How the two Metis programs are deployed to Solana devnet and initialised. This is the part that needs your own keys,
so it is run from your machine. Hosting the services around it is in [`deployment.md`](deployment.md).

Run commands from the repository root. The scripts default to `NET=devnet`.

## The build model

- **Program ids** come from the keypairs `cargo build-sbf` writes to `target/deploy/` the first time it runs
  (`metis_core-keypair.json`, `oracle-keypair.json`). Back them up: deleting `target/` regenerates different ids.
- **Trust roots** (3 root keys and 1 guardian) are compiled into the programs and tools. Two roots together can
  initialise and override; the guardian can only co-sign an unpause. Only public keys are ever put in the code.
- **Demo timers** are a separate feature (`short-timers`), so a market's whole life takes about ten minutes. The
  devnet build is `devnet,short-timers`. It must never be combined with `mainnet` (a compile error enforces that).
- A localnet build with throwaway roots exists for rehearsal on a local validator; the build script refuses to
  leave any localnet key inside a devnet binary.

## Steps

1. **Choose the keys.** Three root pubkeys and one guardian pubkey, all different. Two root *key files* are needed
   on the deploying machine for step 5.
2. **Fill them in:** `devnet/set-roots.sh <root1> <root2> <root3> <guardian>`. It rejects duplicates, invalid
   addresses and the throwaway test keys.
3. **Build:** `devnet/00-build.sh`. It ends with `ok: no localnet root or guardian inside the devnet programs`,
   prints the config and oracle-authority addresses and SHA-256 hashes of both programs.
4. **Fund the deployer:** `devnet/cost.sh` prints the SOL needed from the real program sizes and devnet rent.
   Export `RPC_URL` (a dedicated devnet RPC) and `DEPLOYER` (a keypair file).
5. **Deploy and initialise:** export `ROOT_A` and `ROOT_B` (two root key files), then `devnet/01-setup.sh`. It checks
   everything first, deploys both programs, creates the demo USDC mint, the oracle authority and the market
   config, and attests the reporters and the operator. It is re-runnable; finished steps are skipped.
6. **Verify (read-only):** `devnet/verify.sh` prints a PASS/FAIL line per check and never prints an RPC key.
7. **Faucet key:** `devnet/make-faucet.sh` hands the demo USDC mint to a dedicated key so a hosted faucet never
   needs the deployer (which is the programs' upgrade authority).

## What is verified, and what is not

- Verified on a local validator running the real Meteora and Manifest programs: the whole Open Pool flow
  (raise, migrate, remove liquidity, fund the market, activate, settle, backer claim) and the cancel path.
- Present on devnet (checked read-only): the Meteora DBC, DAMM v2, Metaplex and locker programs, the seven DAMM v2
  migration configs, a funded DBC pool authority, and Manifest.
- **Not verified on devnet:** a live Open Pool migration. Meteora requires a vesting cliff of about two days for
  the locked slice, so that slice cannot be claimed during a short demo.

## Notes

- The demo programs use short timers: a real football match would be cancelled about twelve minutes after kickoff.
  A live real-oracle fixture needs the same programs upgraded in place to real timers (same addresses).
- Backers can lose money: the curve takes a 1% fee and 11% of every raise stays locked in the pool (a Meteora rule)
  and goes to the treasury.
