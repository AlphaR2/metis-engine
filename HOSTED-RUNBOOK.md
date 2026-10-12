# Hosted deployment: Railway (backend) + Vercel (website)

After this, nothing runs on your laptop. Judges open the website, and everything behind it is live on
devnet. Panta stays on mainnet. Your two videos are recorded from the website.

Run laptop commands from `metis-engine/`. `RPC_URL` is your Helius (or other) devnet URL; export it in each new
shell, because the scripts no longer store it (it contains your API key):

```bash
export RPC_URL='https://devnet.helius-rpc.com/?api-key=...'
```

## What you already have

You ran `devnet/01-setup.sh` on devnet: programs deployed, demo USDC mint created, oracle and market config
created, reporters and operator attested. Check it any time (read-only, prints no secrets):

```bash
devnet/verify.sh
```

## What runs where

| Service | Where | What it is |
|---|---|---|
| **metis-worker** | Railway (`Dockerfile.worker`) | Oracle keepers x2, the crank, the mock result feeds, the Open Pool service, the demo faucet, the status endpoint |
| **metis-api** | Railway (`Dockerfile`) | The aggregator: the board, quotes, the Fill Score, building Metis and Panta transactions |
| **website** | Vercel (`Metis-Markets/`) | What judges see |

Hot demo keys live only in the worker. The roots, the guardian and the deployer **never** leave your laptop.

---

## 1. Give the mint to a dedicated faucet key (once)

Your demo USDC mint is controlled by your deployer. The hosted faucet must not hold the deployer (it can
upgrade the programs), so this hands the mint to a new key that can only mint demo USDC:

```bash
export DEPLOYER=/absolute/path/to/your-deployer.json
```

```bash
devnet/make-faucet.sh
```

**Expect:** `done: the faucet key now controls the demo USDC mint` (or "nothing to do" if re-run).

## 2. Pack the hot keys

```bash
devnet/pack-secrets.sh
```

**Expect:** `wrote devnet/keys-devnet/secrets.b64 (... bytes)`. It holds operator, reporters, creator, bettors,
treasury, the mint key, the Meteora keeper and the faucet. It does **not** hold roots, guardian or deployer.
Treat the file as a secret; delete it after step 4.

## 3. Install the Railway CLI and create the project

(Railway CLI flags change between versions. If a command below is rejected, run it with `--help`, or do the same
thing in the dashboard: new service, variables, volume, domain. The values to set are what matter.)

```bash
npm i -g @railway/cli
```

```bash
railway login
```

```bash
railway init
```

Name the project `metis`.

## 4. The worker

Create the service and give it a volume (it keeps keys, fixtures and the faucet's daily limits across restarts):

```bash
railway add --service metis-worker
```

```bash
railway volume add --service metis-worker --mount-path /data
```

Set its variables (the first one reads the file from step 2):

```bash
railway variables --service metis-worker \
  --set "METIS_KEYS_B64=$(cat devnet/keys-devnet/secrets.b64)" \
  --set "RPC_URL=$RPC_URL" \
  --set "DEMO_TOKEN=<choose a password>" \
  --set "RAILWAY_DOCKERFILE_PATH=Dockerfile.worker"
```

Deploy it from this directory, then give it a public address:

```bash
railway up --service metis-worker --detach
```

```bash
railway domain --service metis-worker
```

Delete the secrets file once it is deployed: `rm devnet/keys-devnet/secrets.b64`.

**Expect:** after the build finishes, open `https://<worker-domain>/status`. It should show `"ok":true` and
`crank`, `keeper-1`, `keeper-2`, `mock-1`, `mock-2` all `true`. If not, read the logs: `railway logs --service metis-worker`.

> The image runs the Rust build on Railway, so the first deploy takes several minutes.
> It compiles your devnet roots into the tools (`devnet/set-roots.sh` must have been run before `railway up`).

## 5. The API (aggregator)

Get the config address of your build:

```bash
target/bin-devnet/debug/metis-ops addresses
```

Then:

```bash
railway add --service metis-api
```

```bash
railway variables --service metis-api \
  --set "ENABLED_VENUES=panta,metis" \
  --set "METIS_RPC_URL=$RPC_URL" \
  --set "METIS_PROGRAM_ID=<the core program id>" \
  --set "METIS_CONFIG=<the config address printed above>" \
  --set "PANTA_API_KEY=<your Panta key>" \
  --set "EXECUTION_ENABLED=true" \
  --set "MARKET_CREATION_ENABLED=true" \
  --set "REFRESH_INTERVAL_SECS=30"
```

Paste the Panta key into that command yourself (or set it in the Railway dashboard). I never read it.

```bash
railway up --service metis-api --detach
```

```bash
railway domain --service metis-api
```

**Expect:** `https://<api-domain>/health` returns `{"status":"ok",...}`.

## 6. The website on Vercel

From `Metis-Markets/`, set these in the Vercel project, then deploy:

| Variable | Value |
|---|---|
| `NEXT_PUBLIC_API_URL` | `https://<api-domain>` |
| `NEXT_PUBLIC_DBC_KEEPER_URL` | `https://<worker-domain>` (the Open Pool service) |
| `NEXT_PUBLIC_DEVNET_USDC_MINT` | `grep ^MINT= devnet/keys-devnet/state.env` |
| `NEXT_PUBLIC_METIS_CLUSTER` | `devnet` |
| `NEXT_PUBLIC_PANTA_CLUSTER` | `mainnet-beta` |
| `SOLANA_RPC_URL_DEVNET` | your devnet RPC (server-side only) |
| `SOLANA_RPC_URL` | your mainnet RPC (for Panta) |
| `NEXT_PUBLIC_PLATFORM_WALLET` | optional |

`NEXT_PUBLIC_METIS_PROGRAM` and `NEXT_PUBLIC_ORACLE_PROGRAM` are not needed: their defaults are already the
programs you deployed.

```bash
cd ../Metis-Markets && vercel --prod
```

## 7. Let the website call the backend

Now that you have the Vercel address, allow it on both services:

```bash
railway variables --service metis-worker --set "CORS_ORIGIN=https://<your-vercel-domain>"
```

```bash
railway variables --service metis-api --set "CORS_ORIGIN=https://<your-vercel-domain>"
```

Each service restarts by itself. Then open `https://<your-vercel-domain>/status`: every row should be green.

## 8. Put the demo on the board (from the browser)

Open `https://<your-vercel-domain>/demo`, enter your demo token, and press:

1. **Create the standing board** (about a minute): 4 markets on 2 mock matches.
2. **Create an Open Pool market**: a 30 USDC pot, a 5-minute pool.

Connect a devnet wallet. Any "Get demo USDC (devnet)" button gives you 100 demo USDC (once a day per wallet).
Back the Open Pool; when it fills, betting opens. To settle one, use **Play a match** on `/demo`.

## 9. Panta on mainnet (video 1)

Panta costs real mainnet SOL/USDC to create on: it is shown in the quote before you sign. On the website's
Create page, use the **Panta** tab with a mainnet wallet. To see the price first without spending anything:

```bash
SITE_URL=https://<your-vercel-domain> API=https://<api-domain> devnet/panta-create.sh <your-mainnet-wallet> "Will Nigeria beat Ghana?" "YES if Nigeria lead after 90 minutes, stoppage time included."
```

Use the exact same question as a Metis market to see them matched on the board, with the Fill Score.

---

## Your two videos

**Video 1: Panta (side track).** On the website with a mainnet wallet: the board showing Panta's markets
(fetch), then create a market on the Panta tab (quote, build, sign, register), and show it appear. Say what
the creation payment is.

**Video 2: the main submission.** One story, in this order:
1. The same question on Panta and Metis, "Matched across venues", with the Fill Score. (30 s)
2. Create a Metis market with **Open Pool**: the creator puts in $0. Back it from a second wallet until the
   bar fills; the market opens. Say "powered by Meteora DBC". (90 s)
3. Place a bet. (20 s)
4. `/demo`: play the match to full time (say it is a mock feed). Show the oracle finalise and settle on
   the explorer. (60 s)
5. Claim as bettor and as backer in Portfolio. (30 s)
6. `/status`: everything running, the program addresses on the explorer. (15 s)

## Not verified (I can't from here)

- **The Docker images and the Railway deploy.** I don't have Docker or a Railway account in this session. I
  tested the worker's whole startup, faucet, status and demo jobs locally with the same scripts, but the image
  builds themselves only run on Railway. The first failure will show in `railway logs`; send it to me.
- **DBC on devnet:** migration has only run on a local validator with the real Meteora programs.
- **A real wallet:** I drove the pages and the transactions headlessly.

## Things to know

- **Cost:** Railway bills usage (two small always-on services and a volume). Check your plan.
- **Timers:** the programs use demo timers (markets live ~10 minutes), so a real football match would be
  cancelled about 12 minutes after kickoff. For a live real-oracle fixture after your recordings, the same
  deployed programs can be upgraded in place to real timers (same addresses, config's dispute window raised
  with two roots). Tell me when you want that and I will prepare the script.
- **Rotate keys that were shown:** your Helius key was printed once in a terminal capture during this
  session. Rotate it when convenient.
- **Secrets:** the demo keys are devnet hot keys; Railway stores them as variables. Never put the roots,
  guardian or deployer there.
