import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { writeFileSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Keypair, PublicKey } from "@solana/web3.js";
import type { Env } from "./env.js";

const run = promisify(execFile);

/** Market fields the keeper needs, as printed by `metis-ops market-json`. */
export interface MarketInfo {
  market: string;
  status: number;
  statusName: string;
  seedSource: number;
  seedTarget: string;
  raiseDeadline: number;
  betCloseTs: number;
  primaryEnd: number;
  backerMint: string;
  backerSupply: string;
  vault: string;
  creator: string;
  quoteMint: string;
  oracle: string;
}

async function ops(env: Env, args: string[], extra: Record<string, string> = {}): Promise<string> {
  const { stdout } = await run(env.opsBin, args, { env: { ...process.env, RPC_URL: env.rpcUrl, ...extra }, maxBuffer: 1 << 20 });
  return stdout.trim();
}

export async function marketInfo(env: Env, market: PublicKey): Promise<MarketInfo> {
  return JSON.parse(await ops(env, ["market-json", "--market", market.toBase58()]));
}

/** metis-ops reads keypairs from files, so write a temporary one (0600) for the call and remove it. */
async function withKeyFile<T>(kp: Keypair, fn: (path: string) => Promise<T>): Promise<T> {
  const dir = mkdtempSync(join(tmpdir(), "metis-keeper-"));
  const path = join(dir, "k.json");
  writeFileSync(path, JSON.stringify(Array.from(kp.secretKey)), { mode: 0o600 });
  try {
    return await fn(path);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

export const activateMarket = (env: Env, market: PublicKey) =>
  withKeyFile(env.operator, (p) => ops(env, ["activate", "--operator", p, "--market", market.toBase58()]));

export const cancelPending = (env: Env, market: PublicKey) =>
  withKeyFile(env.operator, (p) => ops(env, ["cancel-pending", "--signer", p, "--market", market.toBase58()]));
