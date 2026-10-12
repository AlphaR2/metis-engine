import { createServer } from "node:http";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { timingSafeEqual } from "node:crypto";
import { getAccount, getAssociatedTokenAddressSync } from "@solana/spl-token";
import { jobById, makeFaucet, processStatus, startJob, type JobAction } from "./demo.js";
import { PublicKey } from "@solana/web3.js";
import type { Env } from "./env.js";
import type { Store } from "./store.js";
import { buildBack, ensureConfig, log, progress } from "./dbc.js";
import { drive } from "./raise.js";
import { marketAddress } from "./market.js";
import { marketInfo } from "./ops.js";

/** One pass over every market this keeper has reserved a mint for. Errors are logged and retried next pass. */
export async function tick(env: Env, store: Store): Promise<void> {
  for (const m of store.markets()) {
    try {
      // A market that was reserved but never created on chain has nothing to drive yet.
      if (!(await env.conn.getAccountInfo(new PublicKey(m.market)))) continue;
      const stage = await drive(env, store, new PublicKey(m.market));
      if (stage !== "active" && stage !== "cancelled") log(`${m.market.slice(0, 8)} ${stage}`);
    } catch (e: any) {
      log(`${m.market.slice(0, 8)} will retry: ${String(e?.message ?? e).slice(0, 300)}`);
    }
  }
}

export async function watch(env: Env, store: Store, everyMs: number): Promise<never> {
  for (;;) {
    await tick(env, store);
    await new Promise((r) => setTimeout(r, everyMs));
  }
}

const json = (res: import("node:http").ServerResponse, code: number, body: unknown) => {
  res.writeHead(code, {
    "content-type": "application/json",
    "access-control-allow-origin": process.env.CORS_ORIGIN ?? "*",
    // a browser asks first (preflight) before it sends a JSON POST or a custom header
    "access-control-allow-methods": "GET, POST, OPTIONS",
    "access-control-allow-headers": "content-type, x-demo-token",
    "access-control-max-age": "600",
  });
  res.end(JSON.stringify(body));
};

/** Fixtures the demo scripts created (devnet/03-markets.sh), as the create form lists them. */
function fixtures(runDir: string) {
  const file = join(runDir, "fixtures.tsv");
  if (!existsSync(file)) return [];
  return readFileSync(file, "utf8").split("\n").filter(Boolean).map((line) => {
    const [matchId, , oracle, kickoff, home, away] = line.split("\t");
    return { matchId, oracle, kickoff: Number(kickoff), home, away, league: "Demo (mock match)" };
  });
}

/**
 * The demo's "play the match": writes the final score into the labelled MOCK providers that the two
 * oracle keepers read, the same as devnet/04-settle.sh. Only with DEMO_TOKEN set, only for fixtures the
 * demo scripts made, and the score is whatever is typed: the page that calls this says so.
 */
function playMatch(runDir: string, matchId: string, home: number, away: number) {
  if (!fixtures(runDir).some((f) => f.matchId === matchId)) throw new Error("unknown match");
  if (!Number.isInteger(home) || !Number.isInteger(away) || home < 0 || away < 0 || home > 15 || away > 15) throw new Error("scores are whole numbers from 0 to 15");
  for (const i of [1, 2]) {
    const file = join(runDir, `mock-${i}.json`);
    const state = existsSync(file) ? JSON.parse(readFileSync(file, "utf8")) : {};
    state[matchId] = { status: "FINISHED", duration: "REGULAR", home, away, ht_home: 0, ht_away: 0 };
    writeFileSync(file, JSON.stringify(state));
  }
}

/** Reserve a backer mint for a market about to be created, report raise progress, and build the 'back it' buy. */
export function serve(env: Env, store: Store, port: number) {
  const faucet = makeFaucet(env, {
    keypairFile: process.env.FAUCET_KEYPAIR,
    amountUsdc: Number(process.env.FAUCET_AMOUNT_USDC ?? 100),
    perDayWallets: Number(process.env.FAUCET_DAILY_WALLETS ?? 200),
    stateFile: process.env.FAUCET_STATE ?? `${env.stateFile}.faucet`,
  });
  const demoAllowed = (req: import("node:http").IncomingMessage): boolean => {
    const token = process.env.DEMO_TOKEN;
    if (!token) return false;
    const given = Buffer.from(String(req.headers["x-demo-token"] ?? ""));
    const want = Buffer.from(token);
    return given.length === want.length && timingSafeEqual(given, want);
  };
  const server = createServer(async (req, res) => {
    try {
      if (req.method === "OPTIONS") return json(res, 204, {});
      const url = new URL(req.url ?? "/", "http://x");
      if (req.method === "GET" && url.pathname === "/health") return json(res, 200, { ok: true });
      const body = req.method === "POST" ? JSON.parse(await new Promise<string>((ok) => { let s = ""; req.on("data", (c) => (s += c)); req.on("end", () => ok(s || "{}")); })) : {};

      const runDir = process.env.DEMO_RUN_DIR;
      if (req.method === "GET" && url.pathname === "/status") {
        const processes = processStatus(runDir);
        const slot = await env.conn.getSlot().catch(() => null);
        return json(res, 200, { ok: slot !== null && Object.values(processes).every(Boolean), chainSlot: slot, processes, faucet: faucet !== null, demoControls: Boolean(process.env.DEMO_TOKEN) });
      }
      if (req.method === "POST" && url.pathname === "/faucet") {
        if (!faucet) return json(res, 404, { error: "the demo faucet is not switched on" });
        const ip = String(req.headers["x-forwarded-for"] ?? req.socket.remoteAddress ?? "").split(",")[0]!.trim();
        return json(res, 200, await faucet.claim(String(body.wallet), ip));
      }
      if (url.pathname.startsWith("/demo/jobs") || (req.method === "POST" && url.pathname === "/demo/job")) {
        if (!demoAllowed(req)) return json(res, process.env.DEMO_TOKEN ? 401 : 404, { error: process.env.DEMO_TOKEN ? "wrong demo token" : "demo controls are not switched on" });
        const scripts = process.env.DEMO_SCRIPTS_DIR;
        if (!scripts) return json(res, 404, { error: "demo scripts are not available here" });
        if (req.method === "POST") return json(res, 202, startJob(scripts, body.action as JobAction, { seed: body.seed, raiseSecs: body.raiseSecs, leadSecs: body.leadSecs }));
        const job = jobById(url.pathname.split("/").pop() ?? "");
        return job ? json(res, 200, job) : json(res, 404, { error: "unknown job" });
      }
      if (req.method === "GET" && url.pathname === "/fixtures") return json(res, 200, runDir ? fixtures(runDir) : []);
      if (req.method === "POST" && url.pathname === "/demo/settle") {
        const token = process.env.DEMO_TOKEN;
        if (!runDir || !token) return json(res, 404, { error: "demo controls are not switched on" });
        const given = Buffer.from(String(req.headers["x-demo-token"] ?? ""));
        const want = Buffer.from(token);
        if (given.length !== want.length || !timingSafeEqual(given, want)) return json(res, 401, { error: "wrong demo token" });
        playMatch(runDir, String(body.matchId), Number(body.home), Number(body.away));
        return json(res, 200, { ok: true, note: "MOCK result set; the keepers report it and the crank settles after the dispute window" });
      }

      if (req.method === "POST" && url.pathname === "/reserve") {
        const creator = new PublicKey(body.creator);
        const seedAtoms = BigInt(body.seedAtoms);
        const market = marketAddress(env.coreProgram, creator, BigInt(body.nonce));
        const kp = store.reserve(market, creator, seedAtoms);
        const config = await ensureConfig(env, store, seedAtoms);
        return json(res, 200, { market: market.toBase58(), backerMint: kp.publicKey.toBase58(), config: config.toBase58() });
      }
      const m = url.pathname.match(/^\/markets\/([1-9A-HJ-NP-Za-km-z]+)$/);
      if (req.method === "GET" && m) {
        const entry = store.markets().find((x) => x.market === m[1]);
        if (!entry) return json(res, 404, { error: "unknown market" });
        const config = await ensureConfig(env, store, entry.seedAtoms);
        const prog = await progress(env, config, new PublicKey(entry.baseMint));
        const info = await marketInfo(env, new PublicKey(m[1])).catch(() => null);
        return json(res, 200, { market: m[1], backerMint: entry.baseMint, seedAtoms: entry.seedAtoms.toString(), status: info?.statusName ?? "NotCreated", raiseDeadline: info?.raiseDeadline ?? null, progress: prog });
      }
      if (req.method === "POST" && url.pathname === "/back") {
        const entry = store.markets().find((x) => x.market === body.market);
        if (!entry) return json(res, 404, { error: "unknown market" });
        const config = await ensureConfig(env, store, entry.seedAtoms);
        const prog = await progress(env, config, new PublicKey(entry.baseMint));
        if (!prog) return json(res, 409, { error: "the pool is not launched yet" });
        let amount = body.usdcAtoms !== undefined ? BigInt(body.usdcAtoms) : 0n;
        if (body.sell && body.all) {
          // sell back everything the wallet holds of the backer token
          const ata = getAssociatedTokenAddressSync(new PublicKey(entry.baseMint), new PublicKey(body.wallet), true);
          amount = (await getAccount(env.conn, ata).catch(() => null))?.amount ?? 0n;
          if (amount === 0n) return json(res, 409, { error: "this wallet holds none of the backer token" });
        }
        if (amount <= 0n) return json(res, 400, { error: "usdcAtoms must be positive" });
        const tx = await buildBack(env, new PublicKey(prog.pool), new PublicKey(body.wallet), amount, Boolean(body.sell));
        return json(res, 200, { transaction: tx.serialize({ requireAllSignatures: false }).toString("base64") });
      }
      return json(res, 404, { error: "not found" });
    } catch (e: any) {
      return json(res, 400, { error: String(e?.message ?? e).slice(0, 300) });
    }
  });
  server.listen(port, () => log(`keeper service on :${port}`));
  return server;
}
