import { spawn } from "node:child_process";
import { existsSync, readFileSync, readdirSync, writeFileSync, renameSync } from "node:fs";
import { join, resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { Keypair, PublicKey, Transaction } from "@solana/web3.js";
import { createAssociatedTokenAccountIdempotentInstruction, createMintToInstruction, getAssociatedTokenAddressSync } from "@solana/spl-token";
import type { Env } from "./env.js";
import { send } from "./dbc.js";

/**
 * What a hosted demo needs besides the raise itself, so nobody has to open a terminal:
 *
 *   faucet   mints demo USDC to a wallet (public, limited per wallet and per day)
 *   status   which background processes are alive (public: a judge can see the system is running)
 *   jobs     create the demo markets, or reset the board (guarded by DEMO_TOKEN)
 *
 * Everything runs the same scripts the terminal runs (devnet/03-markets.sh, reset.sh), so there is one
 * way to do each thing. The faucet mints on devnet's demo USDC only: its key is a dedicated one that
 * has no other power, not the deployer.
 */

const DAY = 24 * 3600 * 1000;

export interface Faucet {
  claim(wallet: string, ip: string): Promise<{ amount: number; signature: string }>;
}

export function makeFaucet(env: Env, opts: { keypairFile?: string; amountUsdc: number; perDayWallets: number; stateFile: string }): Faucet | null {
  if (!opts.keypairFile || !existsSync(opts.keypairFile)) return null;
  const authority = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(opts.keypairFile, "utf8"))));
  let seen: Record<string, number> = existsSync(opts.stateFile) ? JSON.parse(readFileSync(opts.stateFile, "utf8")) : {};
  const byIp = new Map<string, number[]>();
  const persist = () => {
    const tmp = `${opts.stateFile}.tmp`;
    writeFileSync(tmp, JSON.stringify(seen), { mode: 0o600 });
    renameSync(tmp, opts.stateFile);
  };
  return {
    async claim(walletText, ip) {
      let wallet: PublicKey;
      try {
        wallet = new PublicKey(walletText);
      } catch {
        throw new Error("that is not a wallet address");
      }
      const now = Date.now();
      seen = Object.fromEntries(Object.entries(seen).filter(([, t]) => now - t < DAY));
      if (seen[wallet.toBase58()]) throw new Error("this wallet already got demo USDC today");
      if (Object.keys(seen).length >= opts.perDayWallets) throw new Error("the demo faucet is out of USDC for today. Try tomorrow, or ask the team.");
      const recent = (byIp.get(ip) ?? []).filter((t) => now - t < 3600 * 1000);
      if (recent.length >= 5) throw new Error("too many requests from here. Try again later.");
      const ata = getAssociatedTokenAddressSync(env.quoteMint, wallet, true);
      const atoms = BigInt(opts.amountUsdc) * 1_000_000n;
      const tx = new Transaction()
        .add(createAssociatedTokenAccountIdempotentInstruction(authority.publicKey, ata, wallet, env.quoteMint))
        .add(createMintToInstruction(env.quoteMint, ata, authority.publicKey, atoms));
      const signature = await send(env, `faucet ${opts.amountUsdc} USDC to ${wallet.toBase58().slice(0, 6)}`, tx, [authority]);
      seen[wallet.toBase58()] = now;
      byIp.set(ip, [...recent, now]);
      persist();
      return { amount: opts.amountUsdc, signature };
    },
  };
}

/** Which of the demo's background processes are alive, from their pidfiles. Public and read-only. */
export function processStatus(runDir: string | undefined): Record<string, boolean> {
  if (!runDir || !existsSync(runDir)) return {};
  const out: Record<string, boolean> = {};
  for (const f of readdirSync(runDir).filter((n) => n.endsWith(".pid"))) {
    const pid = Number(readFileSync(join(runDir, f), "utf8").trim());
    let alive = false;
    try {
      process.kill(pid, 0);
      alive = true;
    } catch {
      alive = false;
    }
    out[f.replace(/\.pid$/, "")] = alive;
  }
  return out;
}

export type JobAction = "seed" | "live" | "backed" | "reset";
export interface Job {
  id: string;
  action: JobAction;
  status: "running" | "done" | "failed";
  output: string;
  startedAt: number;
}

const jobs = new Map<string, Job>();

/**
 * Runs one of the demo scripts. Only one at a time (they share a nonce counter and restart the
 * keepers), and the arguments are checked here: nothing from the caller is ever put in a shell string.
 */
export function startJob(scriptsDir: string, action: JobAction, args: { seed?: number; raiseSecs?: number; leadSecs?: number }): Job {
  if ([...jobs.values()].some((j) => j.status === "running")) throw new Error("another demo job is still running");
  const int = (v: unknown, lo: number, hi: number, d: number) => {
    const n = v === undefined ? d : Number(v);
    if (!Number.isInteger(n) || n < lo || n > hi) throw new Error(`value out of range (${lo} to ${hi})`);
    return n;
  };
  let argv: string[];
  switch (action) {
    case "seed": argv = [join(scriptsDir, "03-markets.sh"), "seed"]; break;
    case "live": argv = [join(scriptsDir, "03-markets.sh"), "live", String(int(args.leadSecs, 150, 900, 330))]; break;
    case "backed": argv = [join(scriptsDir, "03-markets.sh"), "backed", String(int(args.seed, 30, 1000, 30)), String(int(args.raiseSecs, 90, 600, 300))]; break;
    case "reset": argv = [join(scriptsDir, "reset.sh")]; break;
    default: throw new Error("unknown action");
  }
  const job: Job = { id: randomUUID().slice(0, 8), action, status: "running", output: "", startedAt: Date.now() };
  jobs.set(job.id, job);
  const child = spawn("bash", argv, { env: process.env, cwd: resolve(scriptsDir, "..") });
  const add = (b: Buffer) => { job.output = (job.output + b.toString()).slice(-6000); };
  child.stdout.on("data", add);
  child.stderr.on("data", add);
  // A demo script that has not finished in 8 minutes is stuck (a market normally takes 1 to 3): stop it and say so.
  const killer = setTimeout(() => {
    job.output += "\n[stopped: this job ran longer than 8 minutes. Check the worker logs and the RPC.]";
    job.status = "failed";
    child.kill("SIGKILL");
  }, 8 * 60 * 1000);
  child.on("close", (code) => { clearTimeout(killer); if (job.status === "running") job.status = code === 0 ? "done" : "failed"; });
  child.on("error", (e) => { job.output += String(e); job.status = "failed"; });
  return job;
}

export const jobById = (id: string): Job | undefined => jobs.get(id);
