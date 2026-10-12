# DBC spike: does a Meteora raise fund a Metis vault?

`spike.ts` runs the whole raise against the REAL Meteora programs (DBC, DAMM v2, Metaplex metadata,
locker) cloned from mainnet onto a local validator, so it needs no faucet and no real funds:

```bash
solana-test-validator --reset --ledger /tmp/dbc-ledger --url https://api.mainnet-beta.solana.com \
  --clone-upgradeable-program=dbcij3LWUppWqq96dh6gJWwBifmcGfLSB5D4DuSMaqN \
  --clone-upgradeable-program=cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG \
  --clone-upgradeable-program=metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s \
  --clone-upgradeable-program=LocpQgucEQHbqNABEYvBvwoxCPsSbG91A1QaQhQQqjn \
  --clone=7F6dnUcRuyM2TwR8myT1dYypFXpPSxqwKNSFNkxyNESd --clone=2nHK1kju6XjphBLbNxpM5XRGFj7p9U8vvNzyZiha1z6k \
  --clone=Hv8Lmzmnju6m7kcokVKvwqz7QPmdX9XfKjJsXz8RXcjp --clone=2c4cYd4reUYVRAB9kUUkrq55VPyy2FNQ3FDL4o12JXmq \
  --clone=AkmQWebAwFvWk55wBoCr5D62C6VVDTzi84NJuD9H7cFD --clone=DbCRBj8McvPYHJG1ukj8RE15h2dCNUdTAESG49XpQ44u \
  --clone=A8gMrEPJkacWkcb3DGwtJwTe16HktSEfvwtuDh2MCtck   # the seven DAMM v2 migration configs
npm install && npx tsx spike.ts
```

## What it does, step by step (all confirmed by running it)

1. `createConfig` (SPL base token, 6 decimals, immutable authorities, flat 1% fee, 50% of trading
   fees to the creator, migration to DAMM v2, 89% unlocked partner liquidity + 11% vested with a
   2-day cliff, 20% of supply on migration, 40 USDC migration threshold).
2. `createPool`, then `swap2` (a buy) until the pool reaches the threshold.
3. `migrateToDammV2`, then `removeAllLiquidity` on the partner's unlocked position.

## Findings (the numbers are from a real run of this script)

| Fact | Value |
|---|---|
| Raise threshold | 40 USDC (quote reserve 40,000,000 atoms at graduation) |
| USDC the authority gets out right after migration | **35.528799** (88.8% of the raise) |
| Base tokens the authority also gets back | 177,644 of 1,000,000 (burn these before activation) |
| Base mint after pool creation | supply 1,000,000, **mint authority revoked, no freeze authority** |
| Tokens the buyer (the backer side) received | 799,999.98 |
| What stays locked | the 11% vested position (about 4.5 USDC and 22,000 base tokens), claimable after the 2-day cliff |

### Two things that differ from the plan's hypothesis

1. **100% unlocked partner liquidity is NOT allowed.** The SDK (`validateMinimumLockedLiquidity`,
   `MIN_LOCKED_LIQUIDITY_BPS = 1000`) requires at least 10% of the migrated liquidity to still be
   locked one day after migration, through permanent lock or vesting. (The configured 11%, not 10%,
   is deliberate: rounding in the vesting maths put 10% at 999 bps and was refused.) So a raise
   returns about 89% of the USDC at migration, and the rest only after the vesting cliff.
   `docs/dbc-backer-switch.md` already sized the raise so that 90% of it equals the seed; that rule
   stands, with 88.8% as the measured figure.
2. **The pool authority pays the DAMM v2 pool's rent (about 0.00277 SOL) during migration.** On a
   fresh local validator that PDA has 0 SOL and migration fails with "insufficient lamports"; the
   spike airdrops to it. On mainnet and devnet Meteora funds it. Check on devnet before relying on it.

### Risk to design around

Between migration and the partner removing its liquidity, the DAMM v2 pool is tradable. A holder
selling backer tokens into it drains USDC from the pool and leaves the partner with fewer USDC and
more tokens. The keeper should migrate and remove liquidity back to back, and pass a minimum
USDC out (`tokenAAmountThreshold`/`tokenBAmountThreshold`) so it fails safe instead of activating
an under-funded market. Not tested: running both in one transaction.

## What this does not prove

* Mainnet or devnet behaviour (only the local clone). Devnet migration is manual (the creator or
  an authorised migrator calls it).
* The vested 11% being claimable after the cliff (needs clock warping the cloned programs).
* Any trading fee or creator-fee claim path.
