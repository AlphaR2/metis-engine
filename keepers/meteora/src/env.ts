import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

/** Everything the keeper reads from the environment, in one place. Paths to key files only: no secret is ever read from a variable. */
export interface Env {
  rpcUrl: string;
  conn: Connection;
  /** Partner and payer: owns the DBC config, the DAMM v2 positions, and pays rent. */
  keeper: Keypair;
  /** The attested Metis operator that may call activate_market. Defaults to the keeper key. */
  operator: Keypair;
  quoteMint: PublicKey;
  /** Who gets the vested liquidity slice (claim-vested). */
  treasury: PublicKey;
  coreProgram: PublicKey;
  opsBin: string;
  stateFile: string;
  /** Vesting cliff of the locked partner slice, in seconds. 2 days in production; shorten to test. */
  cliffSecs: number;
  /** Length of the vesting period after the cliff, in seconds (one day in production). */
  vestPeriodSecs: number;
  tokenUri: string;
  /** Fraction of a raise that comes back unlocked at migration (measured 0.88822 on the real programs). */
  unlockedFactor: number;
}

function key(path: string): Keypair {
  return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(path, "utf8"))));
}
function need(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`${name} is required`);
  return v;
}

export function loadEnv(): Env {
  const rpcUrl = process.env.RPC_URL ?? "http://127.0.0.1:8899";
  const keeper = key(need("KEEPER_KEYPAIR"));
  return {
    rpcUrl,
    conn: new Connection(rpcUrl, "confirmed"),
    keeper,
    operator: process.env.OPERATOR_KEYPAIR ? key(process.env.OPERATOR_KEYPAIR) : keeper,
    quoteMint: new PublicKey(need("QUOTE_MINT")),
    treasury: new PublicKey(need("TREASURY")),
    coreProgram: new PublicKey(need("CORE_PROGRAM")),
    opsBin: resolve(process.env.OPS_BIN ?? "../../target/debug/metis-ops"),
    stateFile: resolve(process.env.STATE_FILE ?? "keeper-state.json"),
    cliffSecs: Number(process.env.CLIFF_SECS ?? 2 * 86400),
    vestPeriodSecs: Number(process.env.VEST_PERIOD_SECS ?? 86400),
    tokenUri: process.env.TOKEN_URI ?? "https://metis.example/backer-token.json",
    unlockedFactor: Number(process.env.UNLOCKED_FACTOR ?? 0.888),
  };
}
