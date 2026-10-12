# Running the keeper and the crank

Both are one binary, `oracle-keeper`. Build it with the network feature that matches the deployed
programs (`--features devnet|mainnet|localnet`): the roots are compiled in and decide the config and
authority addresses.

## Keys

| Key | What it is | Attested by |
|---|---|---|
| Reporter (one per data provider) | signs result reports | two roots, on the **oracle** (`metis-ops attest --program oracle`) |
| Crank operator | signs `close_primary`, `settle`, `cancel`, protocol sweep, `close_market` | two roots, on the **market program** (`--program core`) |
| Fixture creator | creates oracle accounts | a root or an attested reporter |

A reporter and the crank operator should be different keys. Neither is ever a root.

## The keeper (one per provider)

```
KEEPER_RPC_URL=...  KEEPER_KEYPAIR=reporter.json
KEEPER_PROVIDER=football-data|api-football  PROVIDER_API_KEY=...
KEEPER_WATCH=watch.json    # [{"fixture_root": "<hex>", "external_id": "<provider's match id>"}]
KEEPER_POLL_SECS=30  KEEPER_HEALTH_FILE=/var/run/keeper.health
oracle-keeper run
```

Run at least two keepers on **different providers**. One provider alone can never finalise a result
(two agreeing reports are required), so a single wrong provider cannot decide a match; with three,
one wrong provider is outvoted. A keeper never guesses: if its provider withholds the 90-minute
score it waits.

Create a match's oracle account with `oracle-keeper create-fixture --kickoff <unix> --id
<provider>=<id> ...` (one `--id` per provider; the fixture root commits to all of them) and list the
fixture root and each provider's id in that keeper's watch file.

## The crank

```
KEEPER_RPC_URL=...  KEEPER_KEYPAIR=operator.json  KEEPER_POLL_SECS=30
oracle-keeper crank            # loop
oracle-keeper crank --once     # one pass, for cron
```

Each pass lists every market and sends what is due: open books, settle, cancel, sweep the protocol's
share, close. A failure on one market never stops the others and is retried next pass. It never pays
or forfeits a creator's fees. Its cost per step is in the log (`spent=... lamports`); keep the
operator funded (about 0.02 SOL per new market).

If the crank is down, nothing is lost: `settle` and `cancel` open to anyone a week after they became
possible, and creators and winners claim for themselves. Only the books, the protocol sweep and
closing wait for an operator.

## Health and stopping

- Retries: RPC reads are retried 4 times with growing pauses; a send is retried only if it never
  reached the node (a transaction the program rejected is not repeated).
- `KEEPER_HEALTH_FILE` gets `<unix time> ok|degraded` after every round. Alert on the file being
  older than a few poll intervals (loop wedged) or `degraded` for several rounds (provider or RPC
  failing). With systemd, `WatchdogSec` plus a small checker works; with Kubernetes, an exec probe.
- Ctrl-C or SIGTERM stops cleanly between rounds, never mid-transaction.

## Creator fees that were never collected

```
oracle-keeper forfeitable
oracle-keeper forfeit <market> --root-a root_a.json --root-b root_b.json
```

`forfeitable` is read only. `forfeit` needs two roots' keypairs on one machine, so for real roots
prefer building the instruction and signing it offline in whatever custody they use.

## Admin commands (`metis-ops`)

Setup and interventions: `oracle-init`, `config-init`, `attest`, `create-market`, `bet`,
`claim`, `refund`, `creator-claim`, `settle`, `cancel`, `forfeit`, `pause`, `unpause`,
`maker-ask`, `sign-send`. See the header of `tools/metis-ops/src/main.rs`.
