# Manual deployment: GitHub, Railway, Vercel (all by hand)

Nothing in this document needs you to give anyone a key. You do each step yourself in a dashboard.

## The picture

```
GitHub repo "metis-engine"  ──►  Railway service 1: metis-worker   (oracle + crank + Open Pool + faucet + status)
                            └─►  Railway service 2: metis-api      (the aggregator: board, quotes, Fill Score)
GitHub repo "Metis-Markets" ──►  Vercel: the website
```

The website talks to the two Railway services. The two Railway services talk to Solana devnet (and the API talks to
Panta on mainnet). Your programs are already deployed on devnet.

---

## Part 1. Three things only your own computer can do

These use your private keys, so they happen on your machine and nothing is shared.

### 1.1 Give the demo USDC mint to a separate "faucet" key

Why: the website's "Get demo USDC" button must mint tokens. The server must not hold your deployer key (that key can
upgrade your programs), so the mint authority moves to a new key that can do nothing else.

Open a terminal in `metis-engine/` and run, with your own paths:

```bash
export RPC_URL='<your Helius devnet URL>'
export DEPLOYER=/absolute/path/to/your-deployer.json
devnet/make-faucet.sh
```

It must end with `done: the faucet key now controls the demo USDC mint`.

### 1.2 Make the one file the worker needs

Why: the worker (the thing that reports match scores and settles markets) needs the demo's hot keys. They go into one
text file that you paste into Railway.

```bash
devnet/pack-secrets.sh
```

This creates `devnet/keys-devnet/secrets.b64`. It contains the operator, reporters, creator, bettors, treasury, mint,
Meteora keeper and faucet keys. It does **not** contain your roots, guardian or deployer. You'll copy its contents
in Part 3, then delete it.

### 1.3 Look up two values you'll paste later

- The mint address: open `devnet/keys-devnet/state.env` in a text editor; the line `MINT=...`. (Public.)
- Your core program address and oracle program address: already in the code; for reference, they are the two
  `declare_id!` lines in `programs/core/src/lib.rs` and `programs/oracle/src/lib.rs`.

---

## Part 2. Put the code on GitHub

You have two repos already: `AlphaR2/metis-engine` and `AlphaR2/Metis-Markets`.

### 2.1 Check what is about to be uploaded (do this before every commit)

In GitHub Desktop (or `git status`), look at the list of changed files. You must **not** see any of these:
`devnet/keys-devnet/…`, `target/…`, `aggregator/.env`, `localnet/keys/…`, anything ending in `-keypair.json`, or
`.env.local` in the website. They are all already ignored, so they should not appear; this check is your safety net.

You **do** need these in `metis-engine`: `Dockerfile`, `Dockerfile.worker`, the whole `devnet/` folder, `keepers/`,
`authority/src/network.rs` (it holds your four **public** root/guardian addresses, and the build compiles them in),
`Cargo.lock`, and the `programs`, `tools`, `aggregator` folders. Everything that changed this session goes up.

### 2.2 Commit and push both repos

Commit with a message like "Hosted demo", then push. Do this for `metis-engine` and for `Metis-Markets`.
(I don't commit for you.)

If either repo is public, remember that the code is then public. The keys are not in it.

---

## Part 3. Railway: the worker

1. Go to railway.app → **New Project** → **Deploy from GitHub repo** → choose `metis-engine`.
   (First time: Railway asks to install its GitHub app. Allow access to this repo.)
2. It creates a service. Click it and rename it **metis-worker** (Settings → Service Name).
3. **Settings → Build → Dockerfile Path:** type `Dockerfile.worker`.
   (Alternative: add a variable `RAILWAY_DOCKERFILE_PATH` = `Dockerfile.worker`.)
4. **Volumes** (right-click on the canvas, or the service's Volumes tab) → add a volume, **Mount Path** `/data`.
   This keeps the keys and the demo's state when the service restarts.
5. **Variables** → add these four:

   | Name | Value | Where it comes from |
   |---|---|---|
   | `METIS_KEYS_B64` | the whole contents of `devnet/keys-devnet/secrets.b64` | Open that file in a text editor, select all, copy, paste. It is one very long line. |
   | `RPC_URL` | your Helius devnet URL | Your Helius dashboard |
   | `DEMO_TOKEN` | a password you choose | You. You'll type it on the `/demo` page. |
   | `CORS_ORIGIN` | leave out for now | You'll add it in Part 6 |

6. **Settings → Networking → Generate Domain.** Railway gives an address like `metis-worker-production.up.railway.app`.
   Copy it. This is your **worker URL**.
7. Railway builds and deploys by itself. The first build takes several minutes (it compiles Rust).
   Watch **Deployments → View logs**.
8. When it is running, open `https://<worker URL>/status` in your browser. Expect:
   `"ok":true` and `crank`, `keeper-1`, `keeper-2`, `mock-1`, `mock-2` all `true`.
9. Now delete `devnet/keys-devnet/secrets.b64` from your computer. It has served its purpose.

If the build fails, copy the last 40 lines of the deployment log and send them to me.

---

## Part 4. Railway: the API (aggregator)

1. In the same Railway project: **New** → **GitHub Repo** → `metis-engine` again. Rename it **metis-api**.
   (Same repo, a second service. Leave the Dockerfile path alone: the default `Dockerfile` is the right one.)
2. **Variables** (add these **before** the first build finishes, or redeploy after):

   | Name | Value |
   |---|---|
   | `FEATURES` | `devnet,short-timers` (this compiles your program id and roots into the API, so it finds your market config by itself) |
   | `ENABLED_VENUES` | `panta,metis` |
   | `METIS_RPC_URL` | your Helius devnet URL |
   | `PANTA_API_KEY` | your Panta key (you paste it; I never see it) |
   | `EXECUTION_ENABLED` | `true` |
   | `MARKET_CREATION_ENABLED` | `true` |
   | `REFRESH_INTERVAL_SECS` | `30` |
   | `CORS_ORIGIN` | leave out for now |

3. **Settings → Networking → Generate Domain.** Copy it. This is your **API URL**.
4. After the build, open `https://<API URL>/health`. Expect `{"status":"ok",...}`.

---

## Part 5. Vercel: the website

1. vercel.com → **Add New… → Project** → import `Metis-Markets`.
2. Framework: Next.js (detected). Leave the root directory as is.
3. **Environment Variables** (add all of these, for Production):

   | Name | Value |
   |---|---|
   | `NEXT_PUBLIC_API_URL` | `https://<API URL>` (no slash at the end) |
   | `NEXT_PUBLIC_DBC_KEEPER_URL` | `https://<worker URL>` |
   | `NEXT_PUBLIC_DEVNET_USDC_MINT` | the `MINT=` value from `state.env` |
   | `NEXT_PUBLIC_METIS_CLUSTER` | `devnet` |
   | `NEXT_PUBLIC_PANTA_CLUSTER` | `mainnet-beta` |
   | `SOLANA_RPC_URL_DEVNET` | your Helius devnet URL |
   | `SOLANA_RPC_URL` | your mainnet RPC URL |

   You do **not** need the program addresses: the website's built-in defaults are the programs you deployed.
4. **Deploy.** Copy your site address, for example `https://metis-markets.vercel.app`.

---

## Part 6. Tell the backend which website may talk to it

In Railway, add this variable to **both** services (metis-worker and metis-api):

| Name | Value |
|---|---|
| `CORS_ORIGIN` | `https://<your Vercel address>` (exactly as in the browser bar, no slash at the end) |

Each service restarts on its own. Then open `https://<your Vercel address>/status`.
Every row should be green, and the four programs should link to the Solana Explorer.

---

## Part 7. Put the demo on the board (all in the browser)

1. Open `https://<your Vercel address>/demo`. Type your `DEMO_TOKEN`.
2. Press **Create the standing board** and wait about a minute. The Events page now has 4 markets.
3. Press **Create an Open Pool market**.
4. Install a Solana wallet extension, switch it to **Devnet**, connect it on the site.
5. Press **Get demo USDC (devnet)**. Back the Open Pool market until its bar fills. When it fills,
   betting opens (about 30 seconds later).
6. Place a bet.
7. On `/demo`, under **Play a match**, press **Play to full time (mock)** for that match. After about a minute
   the market settles. Claim in **Portfolio**.

## Part 8. Panta (video 1)

In the website's Create page, choose the **Panta** tab and connect a **mainnet** wallet. The quote shows what
Panta charges before you sign: it is real money. Create one market, then check it appears on the board.
To get the Fill Score matching, make a Metis market with the exact same question.

## When something breaks

| Where to look | What it tells you |
|---|---|
| `https://<your Vercel address>/status` | Which service is down |
| Railway → the service → Deployments → logs | Why it is down |
| Browser (F12) → Console | Whether the site could reach the backend (CORS or wrong URL) |
| Vercel → project → Logs | Website errors |

Send me the log lines, never the keys.

## After you are done

- Rotate your Helius key (it was shown once in a terminal capture).
- Delete `secrets.b64` if it is still there.
- The programs use demo timers: markets live about 10 minutes. This deployment is for the demo, not real money.
