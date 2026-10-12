# Prior work and what is new

Colosseum judges only work done after **14 September 2026**. This page separates what existed before that date from
what was built after it. It is derived from the git history of this repository (first commit 24 August 2026); the
history is the record, and the author should confirm anything it cannot show.

## Before 14 September 2026 (prior work)

| Date | What the history shows |
|---|---|
| 2026-08-24 | Repository started. |
| 2026-08-31 to 2026-09-01 | Core market program work; outcome shares implemented as SPL tokens, one mint per outcome. |
| 2026-09-04 to 2026-09-09 | An existing football-oracle setup was brought in from an earlier project (`stake-engine`, per the branch and merge history), then adapted. |
| 2026-09-11 | "AMM v0.1": the pricing curve, primary market and settlement as a first version. |

In short: the market program's first version (curve, SPL outcome tokens, settlement) and the starting point of the oracle.

## After 14 September 2026 (the judged work)

| Area | What was built |
|---|---|
| Oracle | Merged and reworked into `programs/oracle`: two independent attested reporters must agree before a result counts; a dispute/override window; per-network trust roots shared with the market program. |
| Market program | Config with bounded dials, a hard-coded root and guardian authority, pause, permissionless fallbacks after an operator grace period, settlement from the oracle, the four-way revenue split (protocol, backers, creator, platform), creator fee claims, and Manifest order books for trading after betting closes. |
| **Open Pool** | A market whose opening pot is raised by a crowd on a Meteora Dynamic Bonding Curve instead of by the creator or a market maker: `initialize_backed_market`, `activate_market`, `claim_backer`, `cancel_pending_market`; the Meteora keeper (`keepers/meteora`) that launches the pool, watches the raise, migrates to DAMM v2, removes the unlocked liquidity, funds the market and activates it. Proven against the real Meteora programs on a local validator. |
| Aggregator | `metis-aggregator` (`aggregator/`): one API over several venues. Panta (mainnet) and Metis adapters, transaction building for both, market creation, claims, positions, and the **Fill Score** (computed only from each venue's own quote and liquidity). |
| Tools | `metis-ops` (setup and operator commands) and `oracle-keeper` (reporters and the settlement crank). |
| Devnet tooling | `devnet/` scripts: root-key fill-in, build with a guard against localnet keys, cost estimate, deploy and initialise, read-only verification, faucet key, and a hosted worker. Container images for the aggregator and the worker. |
| Tests | 359 aggregator tests (several run the adapter's transactions against the real compiled programs in litesvm), the program suites, and keeper tests. |

## What is deliberately not claimed

- The live Open Pool migration has not been run on devnet; it has run on a local validator with the real Meteora programs.
- The deployed demo programs use short timers and a labelled mock result feed so a market can be shown in minutes.
- Panta markets live on mainnet; everything else runs on devnet.
