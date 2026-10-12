# Metis devnet runbook

> **Hosting the demo (Railway + Vercel) is in `HOSTED-RUNBOOK.md`.** Steps 1-6 below (keys, build, fund, deploy,
> verify) are the on-chain setup you have already done. Steps 7 onward describe running everything on one
> machine and are only needed for a local rehearsal.

You deploy once, with your own deployer and your own root keys. Everything here is a command you run;
after each step, check the **Expect** line and tell me the result before the next one.

Run every command from `metis-engine/` unless it says otherwise. The scripts default to `NET=devnet`.

## What is fixed for you already

| | |
|---|---|
| Metis core program id | `79uea7X34YHRUQFkLAR5rwupHQf6Z7ASbBGfzxRDu8ws` |
| Oracle program id | `BM7jwCCXjBgRy5LPg1UG4iAPueAFK3bRxQRogD4Wzoub` |
| Their keypairs | `target/deploy/metis_core-keypair.json`, `target/deploy/oracle-keypair.json`, made by `cargo build-sbf` (gitignored; **back them up**: `cargo clean` or deleting `target/` loses them and the ids with them) |
| Config / oracle-authority addresses | printed by `devnet/00-build.sh` (they depend on your guardian key, so they appear after step 2) |

A program can only be deployed at the address of its own keypair, so these two files are what let you deploy
to those ids. Lose them and the ids are gone.

No test wallet exists any more. Nothing in this repo holds a funded key.

## Before you start

You need: `solana` + `spl-token` CLI (3.x), Rust with `cargo build-sbf`, Node 20+, Python 3, and the `vercel`
CLI. For the Panta key: `aggregator/.env` must contain `PANTA_API_KEY=` (you fill it; I never read it).

Use a paid/dedicated devnet RPC, not the public one (it throttles hard):

```bash
export RPC_URL='https://<your-devnet-rpc>'
```

---

## 1. Choose the four keys (you)

Metis hard-codes **3 root keys** and **1 guardian** into the programs. Any two roots together can set up and
override; the guardian can only co-sign an unpause and must not be a root.

You said teammates may bring their own keys. Do that with **public keys only** (their private keys never
touch this machine). One practical limit: during step 5 two root *key files* have to be on the machine that
runs the setup. The simplest split:

- you hold **root1 and root2** (two separate keys of yours) and run the setup yourself;
- a teammate holds **root3**, another holds the **guardian**.

After setup you can move root files off the machine. Changing the root set later needs a program upgrade.

## 2. Put the public keys in the programs (me or you, 1 command)

```bash
devnet/set-roots.sh <root1-pubkey> <root2-pubkey> <root3-pubkey> <guardian-pubkey>
```

**Expect:** it prints the four keys back. It refuses duplicates, a guardian that is also a root, anything that
is not a real address, and any of the throwaway localnet/test keys. (I tested all four refusals.)

## 3. Build

```bash
devnet/00-build.sh
```

**Expect:** ends with `ok: no localnet root or guardian inside the devnet programs`, then the four addresses
(core, oracle, config, oracle authority) and two SHA-256 hashes. **Send me the hashes**; after deploying I
compare them with what is on chain.

This build has your roots and the demo timers (windows of minutes, not hours/days). The timers are fixed
into the program, so a market's whole life fits in about 10 minutes. That is what the demo needs; it also
means these programs are for the demo, not for real money.

## 4. Fund the deployer (you)

```bash
devnet/cost.sh
```

**Expect:** a line `fund the DEPLOYER with at least N SOL` (about 3.7 SOL at today's devnet rent). Fund your
deployer wallet with that once (faucet.solana.com, or your usual route). Most of it is the two program
accounts' rent; the upload buffer is refunded.

```bash
export DEPLOYER=/absolute/path/to/your-deployer.json
solana balance "$(solana-keygen pubkey "$DEPLOYER")" -u "$RPC_URL"
```

The deployer becomes the programs' upgrade authority and the mint authority of the demo USDC.

## 5. Deploy and initialise (you)

```bash
export ROOT_A=/absolute/path/to/root1.json
export ROOT_B=/absolute/path/to/root2.json
devnet/01-setup.sh
```

It checks before spending anything: the program keypairs match the compiled ids, ROOT_A/ROOT_B are two of
the compiled-in roots, the balance covers the cost, and Manifest exists on devnet. Then it deploys both
programs, creates the demo keys (operator, two reporters, creator, bettors, treasury, DBC keeper), funds
them, creates the demo USDC mint (6 decimals), creates the oracle authority and the market config
(crowd-seeding switched on), and attests the reporters and the operator with your two roots.

It is re-runnable: each finished step leaves a marker in `devnet/keys-devnet/done/`.

**Expect:** `setup done on devnet`, the mint address, and the four addresses. If a step fails, send me the
output; re-running skips what is done.

## 6. Verify (read-only)

```bash
devnet/verify.sh
```

**Expect:** `ALL CHECKS PASSED`. It reads the cluster: both programs deployed and executable, Manifest present,
who holds the upgrade authority, the config and oracle-authority accounts, the mint, the demo keys' balances.
**Paste me the output.**

Also keep the mint address: `grep ^MINT= devnet/keys-devnet/state.env`.

## 7. Start the demo machinery

```bash
devnet/dbc.sh install                      # once: installs the Meteora keeper's node packages
devnet/02-start.sh                         # two mock football providers, two oracle keepers, the crank
export DEMO_TOKEN='<a password you choose>'   # guards the "play the match" endpoint
export CORS_ORIGIN='https://<your-vercel-url>' # the only site allowed to call the raise service
devnet/dbc.sh run                          # the raise service, :8090
devnet/03-markets.sh seed                  # 4 standing markets on 2 mock fixtures
devnet/03-markets.sh backed 30 300         # one crowd-seeded market: 30 USDC pot, 300 s (5 min) raise
```

**Expect:** `up crank / keeper-1 / keeper-2 / mock-1 / mock-2`, `up dbc-keeper`, then market addresses. Logs are
in `devnet/keys-devnet/run/*.log`.

The fixtures are **mock matches**: the result comes from the labelled mock provider, never real football data.
The web UI says so.

## 8. Run the aggregator and give it, and the raise service, a public address

Fill `PANTA_API_KEY` in `aggregator/.env` yourself, and set `CORS_ORIGIN` there to your Vercel URL.

```bash
devnet/run-aggregator.sh
```

It uses Metis on devnet plus Panta (mainnet), keeps everything in memory (set `USE_DATABASE=1` to use the
`DATABASE_URL` from `.env`), and turns trading and market creation **on** (your `.env` has them off). It only
takes `PANTA_API_KEY` from `aggregator/.env`; a stale venue list there (it still names Jupiter) is ignored.

Vercel must reach **two** servers from the browser: the aggregator (`:8080`) and the raise service (`:8090`).
Quickest, for a demo: two tunnels from this machine.

```bash
cloudflared tunnel --url http://localhost:8080     # -> NEXT_PUBLIC_API_URL
cloudflared tunnel --url http://localhost:8090     # -> NEXT_PUBLIC_DBC_KEEPER_URL
```

For anything that has to stay up while judges look, run these on a small always-on machine instead
(see "Staying alive" below).

## 9. Deploy the web app to Vercel

From `Metis-Markets/`, set these in the Vercel project (Settings → Environment Variables), then deploy:

| Variable | Value |
|---|---|
| `NEXT_PUBLIC_API_URL` | the aggregator's public URL |
| `NEXT_PUBLIC_DBC_KEEPER_URL` | the raise service's public URL |
| `NEXT_PUBLIC_DEVNET_USDC_MINT` | the mint from step 6 |
| `NEXT_PUBLIC_METIS_CLUSTER` | `devnet` |
| `NEXT_PUBLIC_PANTA_CLUSTER` | `mainnet-beta` |
| `SOLANA_RPC_URL_DEVNET` | your devnet RPC (server-side only) |
| `SOLANA_RPC_URL` | your mainnet RPC (for Panta) |
| `NEXT_PUBLIC_PLATFORM_WALLET` | optional: a wallet that earns the platform share |
| `NEXT_PUBLIC_RAISE_MIN_MINS` / `_MAX_MINS` | leave unset: defaults 2 / 10 match this build |

`NEXT_PUBLIC_METIS_PROGRAM` is not needed: its default is already `Hrgr9Y3S…`.

```bash
cd ../Metis-Markets && vercel --prod
```

**Expect:** the page loads, the network badge says Devnet on Metis markets and Mainnet on Panta ones, and
`/demo` asks for the token. Send me the URL.

Go back and put that URL in `CORS_ORIGIN` for both the raise service (`devnet/dbc.sh run`) and the aggregator
if you used a placeholder before.

## 10. The same question on Panta (the Fill Score pair)

Panta is mainnet and creating there spends **real** SOL/USDC (Panta's creation fee is shown in the quote).
The helper only builds the transaction; you sign it in the web app.

```bash
SITE_URL=https://<your-vercel-url> devnet/panta-create.sh <your-mainnet-wallet> \
  "Will Nigeria beat Ghana?" "YES if Nigeria lead after 90 minutes, stoppage time included."
```

Use the exact question of a Metis market so the aggregator matches them. Panta fetches the cover image from
`SITE_URL/skins/sports.svg`, so the site must already be public.

## 11. The demo flows, to try yourself

1. **Crowd seed.** Create → Metis → "Back it (crowd seed)". Connect a devnet wallet; mint it demo USDC:
   `devnet/mint-to.sh <wallet> 100`. Back the raise on the market page; when the bar fills the keeper
   migrates it, funds the market and opens betting (about 20-40 s after the last buy).
   *If nobody fills it before the deadline the keeper cancels the market and the card offers "Sell my tokens back".*
2. **Live settle.** `devnet/03-markets.sh live 330` creates a market that kicks off in 5.5 minutes; at full
   time use the `/demo` page (enter the token and a score) or `devnet/04-settle.sh <matchId> 2 0`.
   Two reporters report, the oracle finalises, and after the 20 s dispute window the crank settles.
3. **Backer claim.** After a crowd-seeded market settles, the backer's position appears in Portfolio as
   "BACKER" and claims through the normal Claim button.
4. **Reset for a clean take.** `devnet/reset.sh` (new fixtures, new standing board, one live market).

## What I did not verify (needs a real cluster)

- Anything with a real wallet in a browser (I drove the pages headlessly and ran the transactions with keys).
- **DBC migration on devnet.** It works on a local validator running the real Meteora programs cloned from
  mainnet. The devnet Meteora deployment can differ. I confirmed all five Meteora/Manifest programs, the
  seven DAMM v2 migration configs and a funded DBC pool authority exist on devnet.
- Whether the first migration needs a SOL top-up of the DBC pool authority: on devnet it holds ~99 SOL, so no.
- The vested 11% becoming claimable: Meteora needs a cliff of ~2 days, so it cannot be shown live. It is
  built (`devnet/dbc.sh claim-vested <market>`) but only runnable after the cliff.
- That `migrate` and `remove liquidity` can share one transaction (they run back to back, with a minimum-out).

## Things you should know before you demo

- **Backers can lose money.** The curve takes 1% on the way in, and 11% of every raise stays locked in the
  pool (Meteora's rule) and goes to the treasury. If nobody else bets, a backer gets back about 85% of
  what they put in (measured on the local rehearsal). The raise card says so. Say so in the deck too.
- **Demo timers.** A raise lasts 2-10 minutes and a market's whole life about 10, so the standing board goes
  stale quickly. `devnet/reset.sh` refreshes it.
- **Keys on this machine.** `devnet/keys-devnet/` holds hot demo keys (operator, reporters, creator, bettors,
  mint, DBC keeper) and `dbc-state.json` holds the backer-token keys. Gitignored; demo only.
- **Aggregator `.env`.** It had `EXECUTION_ENABLED=false` and `ENABLED_VENUES=panta,jupiter`; the run script
  overrides both, but fix the file so nothing else trips on it.

## Staying alive

The oracle keepers, the crank, the raise service and the aggregator are plain processes: if they stop, no
market settles and no raise completes. For judging, run them on an always-on machine (a small VM) with
`devnet/02-start.sh`, `devnet/dbc.sh run`, `devnet/run-aggregator.sh`, and re-run `devnet/reset.sh` before
each viewing. Say if you want me to write a systemd/Docker setup for that.

## If something fails

| Symptom | Cause / fix |
|---|---|
| `01-setup.sh`: "is not one of the devnet roots" | ROOT_A/B are not the keys you gave `set-roots.sh`. |
| `error 6020` in a keeper log | A result arrived before kickoff. Expected until the match starts. |
| Setup stops at "waiting for the funding to finalize" | Devnet is slow; it retries for 3 minutes. Re-run if it gives up. |
| Raise card says "Crowd seeding is not switched on" | `NEXT_PUBLIC_DBC_KEEPER_URL` unset, or the service is down / CORS mismatch. |
| Migration fails | `devnet/dbc.sh logs 80`; send me the lines. |
| Everything quiet after a laptop sleep | Restart `02-start.sh` and `dbc.sh run`. |

Stop everything: `devnet/dbc.sh stop; devnet/stop.sh`.
