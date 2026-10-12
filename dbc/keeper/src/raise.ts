import { Keypair, LAMPORTS_PER_SOL, PublicKey, SystemProgram, Transaction } from "@solana/web3.js";
import {
  TOKEN_PROGRAM_ID, createAssociatedTokenAccountIdempotentInstruction, createBurnInstruction, createTransferCheckedInstruction,
  getAccount, getAssociatedTokenAddressSync,
} from "@solana/spl-token";
import BN from "bn.js";
import { DAMM_V2_MIGRATION_FEE_ADDRESS, deriveDammV2PoolAddress, deriveDbcPoolAuthority } from "@meteora-ag/dynamic-bonding-curve-sdk";
import { CpAmm } from "@meteora-ag/cp-amm-sdk";
import type { Env } from "./env.js";
import type { Store } from "./store.js";
import { createPool, dbcClient, ensureConfig, log, poolAddress, progress, send } from "./dbc.js";
import { activateMarket, cancelPending, marketInfo } from "./ops.js";

const POOL_AUTHORITY_MIN_LAMPORTS = 40_000_000; // one migration spends ~20M lamports of DAMM v2 pool and token-account rent (pool alone 8.63M) is paid by this PDA during migration

const balance = async (env: Env, mint: PublicKey, owner: PublicKey): Promise<bigint> => {
  try {
    return (await getAccount(env.conn, getAssociatedTokenAddressSync(mint, owner, true))).amount;
  } catch {
    return 0n;
  }
};

/** The partner's DAMM v2 position holding the unlocked slice, with the accounts needed to remove it. */
async function dammContext(env: Env, config: PublicKey, baseMint: PublicKey) {
  const cpAmm = new CpAmm(env.conn);
  const dammConfig = DAMM_V2_MIGRATION_FEE_ADDRESS[0];
  const dammPool = deriveDammV2PoolAddress(dammConfig, baseMint, env.quoteMint);
  const poolState: any = await cpAmm.fetchPoolState(dammPool).catch(() => null);
  void config;
  return { cpAmm, dammPool, poolState };
}

/**
 * Removes the liquidity in the partner's DAMM v2 position(s). `vested` picks the vested slice
 * (after its cliff) instead of the unlocked one. Fails safe: `minQuoteOut` is enforced by the pool,
 * so an under-filled removal reverts instead of funding a market short. Returns the quote received.
 */
async function removeLiquidity(env: Env, config: PublicKey, baseMint: PublicKey, minQuoteOut: bigint, vested: boolean): Promise<bigint> {
  const { cpAmm, dammPool, poolState } = await dammContext(env, config, baseMint);
  if (!poolState) return 0n;
  const quoteIsA = poolState.tokenAMint.equals(env.quoteMint);
  const before = await balance(env, env.quoteMint, env.keeper.publicKey);
  const positions = await cpAmm.getUserPositionByPool(dammPool, env.keeper.publicKey);
  for (const p of positions) {
    const st: any = p.positionState;
    const liquid = BigInt(st.unlockedLiquidity.toString());
    const lockedVest = BigInt(st.vestedLiquidity.toString());
    if (!vested && liquid === 0n) continue;
    if (vested && lockedVest === 0n && liquid === 0n) continue;
    const vestings = vested ? await cpAmm.getAllVestingsByPosition(p.position) : [];
    const currentPoint = new BN(Math.floor(Date.now() / 1000));
    if (vested && vestings.length) {
      const refresh = await cpAmm.refreshVesting({
        owner: env.keeper.publicKey, position: p.position, positionNftAccount: p.positionNftAccount, pool: dammPool,
        vestingAccounts: vestings.map((v) => v.publicKey),
      } as never);
      await send(env, "refreshVesting", refresh, [env.keeper]);
    }
    const tx = await cpAmm.removeAllLiquidity({
      owner: env.keeper.publicKey, position: p.position, pool: dammPool, positionNftAccount: p.positionNftAccount,
      tokenAAmountThreshold: new BN(quoteIsA ? minQuoteOut.toString() : 0),
      tokenBAmountThreshold: new BN(quoteIsA ? 0 : minQuoteOut.toString()),
      tokenAMint: poolState.tokenAMint, tokenBMint: poolState.tokenBMint,
      tokenAVault: poolState.tokenAVault, tokenBVault: poolState.tokenBVault,
      tokenAProgram: TOKEN_PROGRAM_ID, tokenBProgram: TOKEN_PROGRAM_ID,
      vestings: vestings.map((v) => ({ account: v.publicKey, vestingState: v.account })),
      currentPoint,
    } as never);
    await send(env, vested ? "removeLiquidity(vested)" : "removeLiquidity(unlocked)", tx, [env.keeper]);
  }
  return (await balance(env, env.quoteMint, env.keeper.publicKey)) - before;
}

async function burnLeftoverBase(env: Env, baseMint: PublicKey) {
  const amount = await balance(env, baseMint, env.keeper.publicKey);
  if (amount === 0n) return;
  const ata = getAssociatedTokenAddressSync(baseMint, env.keeper.publicKey);
  await send(env, `burn ${Number(amount) / 1e6} leftover base tokens`, new Transaction().add(createBurnInstruction(ata, baseMint, env.keeper.publicKey, amount)), [env.keeper]);
}

async function fundVault(env: Env, vault: PublicKey, seedAtoms: bigint) {
  const have = (await getAccount(env.conn, vault)).amount;
  if (have >= seedAtoms) return;
  const src = getAssociatedTokenAddressSync(env.quoteMint, env.keeper.publicKey);
  const tx = new Transaction().add(createTransferCheckedInstruction(src, env.quoteMint, vault, env.keeper.publicKey, seedAtoms - have, 6));
  await send(env, `fund vault with ${Number(seedAtoms - have) / 1e6} USDC`, tx, [env.keeper]);
}

/** metis-ops reads the vault at `finalized`; activating before the funding finalizes fails with SeedNotFunded, so wait for it (about 13s on a validator, ~13s on devnet). */
async function waitFinalized(env: Env, vault: PublicKey, seed: bigint) {
  for (let i = 0; i < 90; i++) {
    const bal = await env.conn.getTokenAccountBalance(vault, "finalized").catch(() => null);
    if (bal && BigInt(bal.value.amount) >= seed) return;
    await new Promise((r) => setTimeout(r, 2000));
  }
  throw new Error("the vault funding did not finalize in 3 minutes");
}

/** Tops up the DBC pool-authority PDA if it cannot pay the DAMM v2 pool's rent. On Meteora's networks it is funded already. */
async function ensurePoolAuthorityFunded(env: Env) {
  const auth = deriveDbcPoolAuthority();
  const lamports = await env.conn.getBalance(auth);
  if (lamports >= POOL_AUTHORITY_MIN_LAMPORTS) return;
  log(`pool authority holds ${lamports} lamports; topping up to ${POOL_AUTHORITY_MIN_LAMPORTS}`);
  await send(env, "fund DBC pool authority", new Transaction().add(SystemProgram.transfer({ fromPubkey: env.keeper.publicKey, toPubkey: auth, lamports: POOL_AUTHORITY_MIN_LAMPORTS - lamports })), [env.keeper]);
}

export type Stage = "no-pool" | "raising" | "graduated" | "migrated" | "funded" | "active" | "cancelled" | "closed";

/**
 * One pass over one market. Every step decides from chain state, never from memory, so a crash or a
 * restart in the middle of a raise just resumes. Returns the stage the market is in afterwards.
 */
export async function drive(env: Env, store: Store, market: PublicKey): Promise<Stage> {
  const entry = store.markets().find((m) => m.market === market.toBase58());
  if (!entry) throw new Error(`market ${market.toBase58()} was not reserved by this keeper`);
  const info = await marketInfo(env, market);
  const baseMintKey = store.baseMintKey(entry.baseMint)!;
  const baseMint = baseMintKey.publicKey;
  if (info.statusName === "Cancelled") return "cancelled";
  if (info.statusName !== "Pending") return "active";

  const now = Math.floor(Date.now() / 1000);
  const config = await ensureConfig(env, store, entry.seedAtoms);
  let prog = await progress(env, config, baseMint);

  if (now > info.raiseDeadline) {
    log(`raise deadline passed for ${market.toBase58()}: cancelling (backers sell back on the curve)`);
    await cancelPending(env, market);
    return "cancelled";
  }
  if (!prog) {
    log(`launching pool for ${market.toBase58()}`);
    await createPool(env, config, baseMintKey, market.toBase58().slice(0, 6));
    return "raising";
  }
  if (!prog.graduated) return "raising";

  if (!prog.migrated) {
    await ensurePoolAuthorityFunded(env);
    const dammConfig = DAMM_V2_MIGRATION_FEE_ADDRESS[0];
    const mig = await dbcClient(env).migration.migrateToDammV2({ payer: env.keeper.publicKey, pool: new PublicKey(prog.pool), dammConfig });
    await send(env, "migrateToDammV2", mig.transaction, [env.keeper, mig.firstPositionNftKeypair, mig.secondPositionNftKeypair]);
  }

  // Remove the unlocked slice at once and demand at least the seed back, so a pool that was drained
  // between migration and now reverts here instead of activating an under-funded market.
  const seed = BigInt(info.seedTarget);
  const vaultHas = (await getAccount(env.conn, new PublicKey(info.vault))).amount;
  if (vaultHas < seed) {
    const held = await balance(env, env.quoteMint, env.keeper.publicKey);
    if (held < seed) await removeLiquidity(env, config, baseMint, seed - held, false);
    await burnLeftoverBase(env, baseMint);
    await fundVault(env, new PublicKey(info.vault), seed);
  }
  await waitFinalized(env, new PublicKey(info.vault), seed);
  await activateMarket(env, market);
  log(`market ${market.toBase58()} activated`);
  return "active";
}

/**
 * Claims the vested partner slice (after the cliff) to the treasury, and burns the base tokens that
 * come with it. Decision: the locked slice goes 100% to the treasury; a backers' claim path is a later item.
 */
export async function claimVested(env: Env, store: Store, market: PublicKey): Promise<bigint> {
  const entry = store.markets().find((m) => m.market === market.toBase58());
  if (!entry) throw new Error("market not reserved by this keeper");
  const config = await ensureConfig(env, store, entry.seedAtoms);
  const baseMint = new PublicKey(entry.baseMint);
  const before = await balance(env, env.quoteMint, env.keeper.publicKey);
  const got = await removeLiquidity(env, config, baseMint, 0n, true);
  void before;
  await burnLeftoverBase(env, baseMint);
  if (got > 0n) {
    const dest = getAssociatedTokenAddressSync(env.quoteMint, env.treasury, true);
    const src = getAssociatedTokenAddressSync(env.quoteMint, env.keeper.publicKey);
    const tx = new Transaction()
      .add(createAssociatedTokenAccountIdempotentInstruction(env.keeper.publicKey, dest, env.treasury, env.quoteMint))
      .add(createTransferCheckedInstruction(src, env.quoteMint, dest, env.keeper.publicKey, got, 6));
    await send(env, `send ${Number(got) / 1e6} USDC (vested slice) to the treasury`, tx, [env.keeper]);
  }
  void Keypair;
  void LAMPORTS_PER_SOL;
  return got;
}

export { poolAddress };
