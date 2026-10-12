import {
  Connection, Keypair, LAMPORTS_PER_SOL, PublicKey, sendAndConfirmTransaction, Transaction,
} from "@solana/web3.js";
import {
  getMint, createMint, getOrCreateAssociatedTokenAccount, mintTo, getAccount, TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import BN from "bn.js";
import {
  DynamicBondingCurveClient, buildCurve, ActivationType, BaseFeeMode, CollectFeeMode,
  MigrationOption, MigrationFeeOption, MigratedCollectFeeMode, DammV2DynamicFeeMode,
  TokenType, TokenDecimal, TokenAuthorityOption, SwapMode, getLockedVestingParams,
  getLiquidityVestingInfoParams, DAMM_V2_MIGRATION_FEE_ADDRESS, deriveDbcPoolAddress, deriveDbcPoolAuthority,
  validateConfigParameters,
} from "@meteora-ag/dynamic-bonding-curve-sdk";
import { CpAmm } from "@meteora-ag/cp-amm-sdk";

const conn = new Connection("http://127.0.0.1:8899", "confirmed");
const log = (...a: any[]) => console.log(...a);
const send = async (label: string, tx: Transaction, signers: Keypair[]) => {
  try {
    const sig = await sendAndConfirmTransaction(conn, tx, signers, { commitment: "confirmed" });
    log(`OK   ${label} ${sig.slice(0, 12)}…`);
    return sig;
  } catch (e: any) {
    log(`FAIL ${label}:`, e.message?.slice(0, 600));
    if (e.logs) log(e.logs.slice(-12).join("\n"));
    throw e;
  }
};
const fund = async (k: Keypair, sol = 20) => {
  const s = await conn.requestAirdrop(k.publicKey, sol * LAMPORTS_PER_SOL);
  await conn.confirmTransaction(s, "confirmed");
};

(async () => {
  const payer = Keypair.generate(), authority = Keypair.generate(), buyer = Keypair.generate();
  const creator = Keypair.generate();
  for (const k of [payer, authority, buyer, creator]) await fund(k);

  // USDC-like quote mint, 6 decimals.
  const quoteMint = await createMint(conn, payer, payer.publicKey, null, 6);
  const buyerQuote = await getOrCreateAssociatedTokenAccount(conn, payer, quoteMint, buyer.publicKey);
  await mintTo(conn, payer, quoteMint, buyerQuote.address, payer, 1_000_000n * 1_000_000n);
  log("quoteMint", quoteMint.toBase58());

  const THRESHOLD = Number(process.env.THRESHOLD ?? 40); // quote units (USDC) needed to graduate
  const vesting = { vestingPercentage: 11, bpsPerPeriod: 10000, numberOfPeriods: 1, cliffDurationFromMigrationTime: 2 * 86400, totalDuration: 3 * 86400 };
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
      partnerLiquidityVestingInfoParams: vesting,
      creatorLiquidityPercentage: 0, creatorPermanentLockedLiquidityPercentage: 0,
    },
    lockedVesting: { totalLockedVestingAmount: 0, numberOfVestingPeriod: 0, cliffUnlockAmount: 0, totalVestingDuration: 0, cliffDurationFromMigrationTime: 0 },
    activationType: ActivationType.Timestamp,
    percentageSupplyOnMigration: 20,
    migrationQuoteThreshold: THRESHOLD,
  } as any);
  log("config built; vesting =", JSON.stringify(vesting));

  const client = new DynamicBondingCurveClient(conn, "confirmed");
  const config = Keypair.generate();
  const cfgTx = await client.partner.createConfig({
    config: config.publicKey, feeClaimer: authority.publicKey, leftoverReceiver: authority.publicKey,
    quoteMint, payer: payer.publicKey, ...params,
  } as any);
  await send("createConfig", cfgTx, [payer, config]);

  const baseMint = Keypair.generate();
  const poolTx = await client.creator.createPool({
    name: "METIS-TEST", symbol: "MTS", uri: "https://example.com/m.json",
    payer: payer.publicKey, poolCreator: creator.publicKey, config: config.publicKey,
    baseMint: baseMint.publicKey,
  });
  await send("createPool", poolTx, [payer, creator, baseMint]);
  const pool = deriveDbcPoolAddress(quoteMint, baseMint.publicKey, config.publicKey);
  log("pool", pool.toBase58());

  // Buy until graduation: one big exact-in buy, then check state.
  const buy = await client.pool.swap2({
    owner: buyer.publicKey, pool, swapBaseForQuote: false, referralTokenAccount: null,
    swapMode: SwapMode.PartialFill, amountIn: new BN(THRESHOLD * 3 * 1_000_000), minimumAmountOut: new BN(0),
  } as any);
  await send("swap2 buy", buy, [buyer]);
  const ps: any = await client.state.getPool(pool);
  const pp: any = ps.poolState ?? ps; log("isMigrated", pp.isMigrated, "quoteReserve", String(pp.quoteReserve), "migrationProgress", String(pp.migrationProgress));

  const poolAuth = deriveDbcPoolAuthority();
  log("pool authority lamports before:", await conn.getBalance(poolAuth));
  await conn.confirmTransaction(await conn.requestAirdrop(poolAuth, 5 * LAMPORTS_PER_SOL), "confirmed");
  const dammConfig = DAMM_V2_MIGRATION_FEE_ADDRESS[0];
  const mig = await client.migration.migrateToDammV2({ payer: payer.publicKey, pool, dammConfig });
  log("migrate tx built; position NFTs", mig.firstPositionNftKeypair.publicKey.toBase58(), mig.secondPositionNftKeypair.publicKey.toBase58());
  await send("migrateToDammV2", mig.transaction, [payer, mig.firstPositionNftKeypair, mig.secondPositionNftKeypair]);

  // Remove the partner's unlocked liquidity from DAMM v2.
  const cpAmm = new CpAmm(conn);
  const pst: any = await client.state.getPool(pool);
  log("after migrate: isMigrated", pst.isMigrated);
  const dammPool = (await cpAmm.fetchPoolStatesByTokenAMint(quoteMint)).concat(await cpAmm.fetchPoolStatesByTokenBMint(quoteMint));
  log("damm pools for quote mint:", dammPool.length);
  const dp = dammPool[0];
  const poolState: any = dp.account;
  const positions = await cpAmm.getUserPositionByPool(dp.publicKey, authority.publicKey);
  log("authority positions:", positions.length);
  for (const p of positions) {
    const st: any = p.positionState;
    log(" pos", p.position.toBase58(), "unlocked", st.unlockedLiquidity.toString(), "vestedLiq", st.vestedLiquidity.toString(), "permLocked", st.permanentLockedLiquidity.toString());
  }
  const before = await getOrCreateAssociatedTokenAccount(conn, payer, quoteMint, authority.publicKey);
  const b0 = (await getAccount(conn, before.address)).amount;
  const pos = positions[0];
  const rm = await cpAmm.removeAllLiquidity({
    owner: authority.publicKey, position: pos.position, pool: dp.publicKey,
    positionNftAccount: pos.positionNftAccount,
    tokenAAmountThreshold: new BN(0), tokenBAmountThreshold: new BN(0),
    tokenAMint: poolState.tokenAMint, tokenBMint: poolState.tokenBMint,
    tokenAVault: poolState.tokenAVault, tokenBVault: poolState.tokenBVault,
    tokenAProgram: TOKEN_PROGRAM_ID, tokenBProgram: TOKEN_PROGRAM_ID,
    vestings: [], currentPoint: new BN(Math.floor(Date.now() / 1000)),
  } as any);
  const rmTx = await (rm as any).transaction?.() ?? rm;
  await send("removeAllLiquidity", rmTx, [authority]);
  const b1 = (await getAccount(conn, before.address)).amount;
  log("authority USDC before", b0.toString(), "after", b1.toString(), "gained", (b1 - b0).toString());
  const baseAta = await getOrCreateAssociatedTokenAccount(conn, payer, baseMint.publicKey, authority.publicKey);
  log("authority base tokens", (await getAccount(conn, baseAta.address)).amount.toString());
  const bm = await getMint(conn, baseMint.publicKey);
  log("base mint: supply", bm.supply.toString(), "mintAuthority", bm.mintAuthority?.toBase58() ?? "NONE (revoked)", "freezeAuthority", bm.freezeAuthority?.toBase58() ?? "NONE");
  const buyerBase = await getOrCreateAssociatedTokenAccount(conn, payer, baseMint.publicKey, buyer.publicKey);
  log("buyer base tokens", (await getAccount(conn, buyerBase.address)).amount.toString());
  log("raised (threshold) USDC =", THRESHOLD);
})().catch((e) => { console.error("SPIKE ABORTED:", String(e?.stack ?? e).slice(0, 2500)); process.exit(1); });
