import { Keypair, PublicKey } from "@solana/web3.js";
import { existsSync, readFileSync, writeFileSync, renameSync, statSync } from "node:fs";

/** What the keeper remembers. Only what chain state cannot tell it: the DBC base-mint keys it reserved and the config per seed tier. */
export interface State {
  /** seed (USDC atoms) -> DBC partner config address */
  tiers: Record<string, string>;
  /** reserved backer mints: base mint address -> secret key and the market it was reserved for */
  mints: Record<string, { secret: number[]; market: string; seedAtoms: string; creator: string }>;
}

export class Store {
  state: State;
  private mtime = 0;
  constructor(private file: string) {
    this.state = { tiers: {}, mints: {} };
    this.reload();
  }
  /** The CLI (`reserve`) and the running service share this file; pick up what the other one wrote. */
  reload() {
    if (!existsSync(this.file)) return;
    const m = statSync(this.file).mtimeMs;
    if (m === this.mtime) return;
    this.state = JSON.parse(readFileSync(this.file, "utf8"));
    this.mtime = m;
  }
  save() {
    const tmp = `${this.file}.tmp`;
    writeFileSync(tmp, JSON.stringify(this.state, null, 2), { mode: 0o600 });
    renameSync(tmp, this.file);
    this.mtime = statSync(this.file).mtimeMs;
  }
  tier(seedAtoms: bigint): PublicKey | undefined {
    this.reload();
    const v = this.state.tiers[seedAtoms.toString()];
    return v ? new PublicKey(v) : undefined;
  }
  setTier(seedAtoms: bigint, config: PublicKey) {
    this.reload(); // pick up what another process added before writing ours on top
    this.state.tiers[seedAtoms.toString()] = config.toBase58();
    this.save();
  }
  reserve(market: PublicKey, creator: PublicKey, seedAtoms: bigint): Keypair {
    this.reload();
    for (const [mint, v] of Object.entries(this.state.mints)) {
      if (v.market === market.toBase58()) return Keypair.fromSecretKey(Uint8Array.from(v.secret)); // idempotent per market
      void mint;
    }
    const kp = Keypair.generate();
    this.state.mints[kp.publicKey.toBase58()] = { secret: Array.from(kp.secretKey), market: market.toBase58(), seedAtoms: seedAtoms.toString(), creator: creator.toBase58() };
    this.save();
    return kp;
  }
  baseMintKey(baseMint: string): Keypair | undefined {
    this.reload();
    const v = this.state.mints[baseMint];
    return v ? Keypair.fromSecretKey(Uint8Array.from(v.secret)) : undefined;
  }
  markets(): { baseMint: string; market: string; seedAtoms: bigint; creator: string }[] {
    this.reload();
    return Object.entries(this.state.mints).map(([baseMint, v]) => ({ baseMint, market: v.market, seedAtoms: BigInt(v.seedAtoms), creator: v.creator }));
  }
}
