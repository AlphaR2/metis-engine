import { Keypair, PublicKey, Transaction, sendAndConfirmTransaction } from "@solana/web3.js";
import BN from "bn.js";
import {
  DynamicBondingCurveClient, buildCurve, ActivationType, BaseFeeMode, CollectFeeMode, MigrationOption,
  MigrationFeeOption, TokenType, TokenDecimal, TokenAuthorityOption, SwapMode, deriveDbcPoolAddress,
} from "@meteora-ag/dynamic-bonding-curve-sdk";
import type { Env } from "./env.js";
import type { Store } from "./store.js";

/** Progress lines. To stdout normally; to stderr when a caller captures stdout for the result (KEEPER_LOG_STDERR=1). */
export const log = (...a: unknown[]) => (process.env.KEEPER_LOG_STDERR ? console.error : console.log)(new Date().toISOString().slice(11, 19), ...a);

export const dbcClient = (env: Env) => new DynamicBondingCurveClient(env.conn, "confirmed");

/** Fails with a clear message instead of hanging when the RPC never answers. */
export function withTimeout<T>(what: string, ms: number, work: Promise<T>): Promise<T> {
  let timer: NodeJS.Timeout;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${what} did not finish in ${Math.round(ms / 1000)}s (the RPC is not answering)`)), ms);
  });
  return Promise.race([work, timeout]).finally(() => clearTimeout(timer));
}

export async function send(env: Env, label: string, tx: Transaction, signers: Keypair[]): Promise<string> {
  log(`send ${label}...`);
  const sig = await withTimeout(label, 90_000, sendAndConfirmTransaction(env.conn, tx, signers, { commitment: "confirmed" }));
  log(`ok   ${label} ${sig.slice(0, 12)}`);
  return sig;
}

/** The USDC a raise must reach for `seedAtoms` to come back unlocked at migration (rounded up to a whole atom). */
export function raiseThresholdAtoms(env: Env, seedAtoms: bigint): bigint {
  return BigInt(Math.ceil(Number(seedAtoms) / env.unlockedFactor));
}

/**
 * One DBC partner config per seed tier. Meteora refuses 100% unlocked partner liquidity (at least 10%
 * must stay locked a day after migration), so 89% is unlocked and 11% vests behind `cliffSecs`.
 * The returned address is remembered in the store.
 */
export async function ensureConfig(env: Env, store: Store, seedAtoms: bigint): Promise<PublicKey> {
  const have = store.tier(seedAtoms);
  if (have) return have;
  const thresholdAtoms = raiseThresholdAtoms(env, seedAtoms);
  const params = buildCurve({
    token: {
      tokenType: TokenType.SPLToken, tokenBaseDecimal: TokenDecimal.SIX, tokenQuoteDecimal: 6,
      tokenAuthorityOption: TokenAuthorityOption.Immutable, totalTokenSupply: 1_000_000, leftover: 0,
    },
    fee: {
      baseFeeParams: {
        baseFeeMode: BaseFeeMode.FeeSchedulerLinear,
        feeSchedulerParam: { startingFeeBps: 100, endingFeeBps: 100, numberOfPeriod: 0, totalDuration: 0 },
      },
      dynamicFeeEnabled: false, collectFeeMode: CollectFeeMode.QuoteToken,
      creatorTradingFeePercentage: 50, poolCreationFee: 0, enableFirstSwapWithMinFee: false,
    },
    migration: {
      migrationOption: MigrationOption.MET_DAMM_V2,
      migrationFeeOption: MigrationFeeOption.FixedBps25,
      migrationFee: { feePercentage: 0, creatorFeePercentage: 0 },
    },
    liquidityDistribution: {
      partnerLiquidityPercentage: 89, partnerPermanentLockedLiquidityPercentage: 0,
      partnerLiquidityVestingInfoParams: {
        vestingPercentage: 11, bpsPerPeriod: 10000, numberOfPeriods: 1,
        cliffDurationFromMigrationTime: env.cliffSecs, totalDuration: env.cliffSecs + env.vestPeriodSecs,
      },
      creatorLiquidityPercentage: 0, creatorPermanentLockedLiquidityPercentage: 0,
    },
    lockedVesting: { totalLockedVestingAmount: 0, numberOfVestingPeriod: 0, cliffUnlockAmount: 0, totalVestingDuration: 0, cliffDurationFromMigrationTime: 0 },
    activationType: ActivationType.Timestamp,
    percentageSupplyOnMigration: 20,
    migrationQuoteThreshold: Number(thresholdAtoms) / 1e6,
  } as never);
  const config = Keypair.generate();
  const tx = await dbcClient(env).partner.createConfig({
    config: config.publicKey, feeClaimer: env.keeper.publicKey, leftoverReceiver: env.keeper.publicKey,
    quoteMint: env.quoteMint, payer: env.keeper.publicKey, ...params,
  } as never);
  await send(env, `createConfig(seed ${Number(seedAtoms) / 1e6} USDC, raise ${Number(thresholdAtoms) / 1e6})`, tx, [env.keeper, config]);
  store.setTier(seedAtoms, config.publicKey);
  return config.publicKey;
}

export const poolAddress = (env: Env, config: PublicKey, baseMint: PublicKey) => deriveDbcPoolAddress(env.quoteMint, baseMint, config);

export async function createPool(env: Env, config: PublicKey, baseMint: Keypair, label: string): Promise<PublicKey> {
  const tx = await dbcClient(env).creator.createPool({
    name: `Metis ${label}`.slice(0, 32), symbol: "MKT", uri: env.tokenUri,
    payer: env.keeper.publicKey, poolCreator: env.keeper.publicKey, config, baseMint: baseMint.publicKey,
  });
  await send(env, "createPool", tx, [env.keeper, baseMint]);
  return poolAddress(env, config, baseMint.publicKey);
}

export interface Progress {
  pool: string;
  quoteReserve: string;
  thresholdAtoms: string;
  pct: number;
  graduated: boolean;
  migrated: boolean;
}

/** Raise progress read straight from the DBC pool and its config. Null until the pool exists. */
export async function progress(env: Env, config: PublicKey, baseMint: PublicKey): Promise<Progress | null> {
  const client = dbcClient(env);
  const pool = poolAddress(env, config, baseMint);
  const raw: any = await client.state.getPool(pool).catch(() => null);
  if (!raw) return null;
  const ps = raw.poolState ?? raw; // the SDK wraps the account in { poolState }
  const cfg: any = await client.state.getPoolConfig(config);
  const reserve = BigInt(ps.quoteReserve.toString());
  const threshold = BigInt(cfg.migrationQuoteThreshold.toString());
  return {
    pool: pool.toBase58(),
    quoteReserve: reserve.toString(),
    thresholdAtoms: threshold.toString(),
    pct: threshold === 0n ? 0 : Math.min(100, Number((reserve * 10000n) / threshold) / 100),
    graduated: reserve >= threshold,
    migrated: Number(ps.isMigrated) === 1 || ps.isMigrated === true,
  };
}

/** An unsigned DBC buy (a backer putting `usdcAtoms` into the raise), for the wallet to sign. */
export async function buildBack(env: Env, pool: PublicKey, owner: PublicKey, usdcAtoms: bigint, sell = false): Promise<Transaction> {
  const tx = await dbcClient(env).pool.swap2({
    owner, pool, swapBaseForQuote: sell, referralTokenAccount: null,
    swapMode: SwapMode.PartialFill, amountIn: new BN(usdcAtoms.toString()), minimumAmountOut: new BN(0),
  } as never);
  tx.feePayer = owner;
  tx.recentBlockhash = (await env.conn.getLatestBlockhash()).blockhash;
  return tx;
}
