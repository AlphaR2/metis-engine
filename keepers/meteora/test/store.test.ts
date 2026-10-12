import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { Keypair } from "@solana/web3.js";
import { raiseThresholdAtoms } from "../src/dbc.js";
import type { Env } from "../src/env.js";
import { Store } from "../src/store.js";

const dir = mkdtempSync(join(tmpdir(), "keeper-test-"));
process.on("exit", () => rmSync(dir, { recursive: true, force: true }));

test("a market reserved by another process (the CLI) is seen by the running service", () => {
  const file = join(dir, "state.json");
  const service = new Store(file); // started first, like `serve`
  assert.equal(service.markets().length, 0);

  const cli = new Store(file); // `03-markets.sh` calling `reserve`
  const market = Keypair.generate().publicKey;
  const creator = Keypair.generate().publicKey;
  cli.reserve(market, creator, 30_000_000n);

  const seen = service.markets();
  assert.equal(seen.length, 1, "the service must pick it up without a restart");
  assert.equal(seen[0]!.market, market.toBase58());
  assert.equal(seen[0]!.seedAtoms, 30_000_000n);
});

test("reserving the same market twice gives the same backer token", () => {
  const store = new Store(join(dir, "idempotent.json"));
  const market = Keypair.generate().publicKey;
  const creator = Keypair.generate().publicKey;
  const a = store.reserve(market, creator, 30_000_000n);
  const b = store.reserve(market, creator, 30_000_000n);
  assert.equal(a.publicKey.toBase58(), b.publicKey.toBase58());
  assert.equal(store.markets().length, 1);
});

test("two writers do not erase each other's markets", () => {
  const file = join(dir, "two.json");
  const one = new Store(file);
  const two = new Store(file);
  one.reserve(Keypair.generate().publicKey, Keypair.generate().publicKey, 30_000_000n);
  two.reserve(Keypair.generate().publicKey, Keypair.generate().publicKey, 40_000_000n);
  assert.equal(new Store(file).markets().length, 2);
});

test("a raise is sized so that 88.8% of it covers the seed", () => {
  const env = { unlockedFactor: 0.888 } as Env;
  const raise = raiseThresholdAtoms(env, 30_000_000n);
  assert.equal(raise, 33_783_784n, "30 USDC / 0.888, rounded up to an atom");
  assert.ok(Number(raise) * 0.888 >= 30_000_000, "the unlocked part covers the seed");
});
