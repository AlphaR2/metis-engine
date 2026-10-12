import { Keypair, PublicKey } from "@solana/web3.js";
import { readFileSync } from "node:fs";
import { loadEnv } from "./env.js";
import { Store } from "./store.js";
import { buildBack, ensureConfig, log, progress } from "./dbc.js";
import { claimVested, drive } from "./raise.js";
import { marketAddress } from "./market.js";
import { serve, tick, watch } from "./service.js";

function flags(argv: string[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (let i = 0; i < argv.length; i += 2) {
    if (!argv[i].startsWith("--")) throw new Error(`unexpected argument ${argv[i]}`);
    out[argv[i].slice(2)] = argv[i + 1];
  }
  return out;
}
const need = (f: Record<string, string>, k: string) => {
  if (!f[k]) throw new Error(`--${k} is required`);
  return f[k];
};

const USAGE = `meteora-keeper <command>
  reserve --creator <pk> --nonce <n> --seed <usdc>   reserve the DBC base mint for a market about to be created; prints it
  progress --market <pk>                             raise progress from the DBC pool
  back --market <pk> --buyer <keypair file> --usdc <n> [--sell true]   buy (or sell back) the market's backer token
  drive --market <pk>                                one pass of the state machine for one market
  tick                                               one pass over every reserved market
  watch [--every <secs>]                             tick forever
  serve [--port <n>] [--every <secs>]                HTTP service (reserve, progress, back) plus the watch loop
  claim-vested --market <pk>                         after the cliff: vested LP slice -> treasury
env: RPC_URL KEEPER_KEYPAIR [OPERATOR_KEYPAIR] QUOTE_MINT TREASURY CORE_PROGRAM [OPS_BIN] [STATE_FILE] [CLIFF_SECS]`;

async function main() {
  const [cmd, ...rest] = process.argv.slice(2);
  if (!cmd) return console.log(USAGE);
  const f = flags(rest);
  const env = loadEnv();
  const store = new Store(env.stateFile);
  switch (cmd) {
    case "reserve": {
      const creator = new PublicKey(need(f, "creator"));
      const seedAtoms = BigInt(Math.round(Number(need(f, "seed")) * 1e6));
      const market = marketAddress(env.coreProgram, creator, BigInt(need(f, "nonce")));
      const kp = store.reserve(market, creator, seedAtoms);
      const config = await ensureConfig(env, store, seedAtoms);
      console.log(JSON.stringify({ market: market.toBase58(), backerMint: kp.publicKey.toBase58(), config: config.toBase58() }));
      break;
    }
    case "progress": {
      const entry = store.markets().find((m) => m.market === need(f, "market"));
      if (!entry) throw new Error("unknown market");
      const config = await ensureConfig(env, store, entry.seedAtoms);
      console.log(JSON.stringify(await progress(env, config, new PublicKey(entry.baseMint))));
      break;
    }
    case "back": {
      const entry = store.markets().find((m) => m.market === need(f, "market"));
      if (!entry) throw new Error("unknown market");
      const buyer = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(need(f, "buyer"), "utf8"))));
      const config = await ensureConfig(env, store, entry.seedAtoms);
      const prog = await progress(env, config, new PublicKey(entry.baseMint));
      if (!prog) throw new Error("the pool is not launched yet (run drive first)");
      const tx = await buildBack(env, new PublicKey(prog.pool), buyer.publicKey, BigInt(Math.round(Number(need(f, "usdc")) * 1e6)), f.sell === "true");
      const { sendAndConfirmTransaction } = await import("@solana/web3.js");
      const sig = await sendAndConfirmTransaction(env.conn, tx, [buyer], { commitment: "confirmed" });
      log("ok   back", sig.slice(0, 12), JSON.stringify(await progress(env, config, new PublicKey(entry.baseMint))));
      break;
    }
    case "drive": console.log(await drive(env, store, new PublicKey(need(f, "market")))); break;
    case "tick": await tick(env, store); break;
    case "watch": await watch(env, store, Number(f.every ?? 10) * 1000); break;
    case "serve": serve(env, store, Number(f.port ?? 8090)); await watch(env, store, Number(f.every ?? 10) * 1000); break;
    case "claim-vested": console.log(`sent ${Number(await claimVested(env, store, new PublicKey(need(f, "market")))) / 1e6} USDC to the treasury`); break;
    default: console.log(USAGE);
  }
}
main().catch((e) => { console.error("keeper:", String(e?.stack ?? e).slice(0, 2000)); process.exit(1); });
