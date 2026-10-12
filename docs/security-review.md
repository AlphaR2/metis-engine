# Security review — core, oracle, keeper, aggregator

A self-review by the author, not an audit. It lists what was checked, what was found, and what
needs a human decision. An outside audit is still required before mainnet.

Severity: **High** can lose or trap funds; **Medium** breaks a promise the design makes;
**Low** is a sharp edge; **Decision** is a choice, not a bug.

## Findings

### M1 — The oracle's override window can be longer than a market's dispute window (Medium, decision)
`OVERRIDE_WINDOW_SECS` is 24 h. A market's `dispute_secs` is a Config dial bounded 15 min to 24 h.
A market with a 15-minute dispute window can settle 15 minutes after the oracle finalises. A root
override at hour 2 then changes the oracle but cannot change a market that already settled, so the
override protects only markets with a long dispute window.
Options: (a) require `dispute_secs >= OVERRIDE_WINDOW_SECS` (a 24 h wait before every payout);
(b) shorten the override window to the minimum dispute window (roots get 15 min to notice);
(c) accept it and document that fast markets trade protection for speed. **Needs your call.**

### M2 — Winnings nobody claimed are swept to the treasury 90 days after settlement (Medium, decision)
`close_market` sends everything left in the vault to the treasury, which includes a winner's
unclaimed payout. This is by design (the claim window), but it is a forfeiture with no record.
Recommended: store the swept amount on the market before closing (as forfeit already does) or
emit it, so a late claimant can be made whole by hand; and say so in the user-facing rules.

### M3 — Refunds split the pot by share count, not by what each holder paid (Medium, known)
`claim_refund` pays `pot × shares / all shares`. Someone who bought the cheap side can get back
more than they paid and someone on the expensive side less. Simple and always solvent; not
perfectly fair. Visible in the local run: a 15 USDC NO bet refunded 15.16, a 20 USDC YES bet 19.75.
Alternative: refund each wallet its recorded deposit (needs per-position cost on chain).

### L1 — A creator can write text that contradicts the machine condition (Low)
The rubric is free text; the condition is what settles. A UI must show both. The aggregator puts
`condition` and `condition_param` in `extra`, and the rules text states the market is judged on the
90-minute score by the oracle. A mismatched rubric is a creator-reputation problem, not a funds one.

### L2 — An operator's key can only do routine, bounded things (Low, by design)
A stolen operator key can: open books early? No (needs `primary_end`). Settle? Only what the oracle
says. Cancel? Only when voided or overdue. Sweep? Only to the recorded treasury. Close a market?
Only after the claim window, and it keeps that market's reclaimed rent. It cannot move user funds
anywhere else or pick a winner. Revoke it with `remove_authority` (two roots).

### L3 — Order-book orders can be placed at any price (Low)
Anyone can rest an absurd ask or bid on a Manifest book. The aggregator quotes from real resting
orders only, refuses books with global orders, and enforces a floor in the swap it builds.

### L4 — `localnet` is a build feature that changes hardcoded values (Low)
It shortens every timer and swaps in throwaway roots. CI checks `devnet` and `mainnet` refuse to
compile until real keys exist; deployment (`deploy/deploy.sh`) only takes those two networks. Do
not let a `localnet` `.so` near a real cluster. Consider a startup assertion in the deploy script
that the binary on chain was built without the feature (compare against `target/devnet`).

## What was checked and found sound

- **Operator check.** `is_operator` requires the signer to have signed, and the delegate account to
  be owned by core, carry the right discriminator, record that exact signer, and be fully
  attested. A forged account cannot satisfy all four; only core can create accounts it owns.
- **Fallbacks.** `settle_market` and `cancel_market` open to anyone only after
  `OPERATOR_GRACE_SECS` from when the step first became possible: for a void, from the void time
  (the oracle resets `finished_at` when it voids); for no result, from the deadline. A vanished
  operator cannot strand funds.
- **Money out.** Every transfer from a vault has a recorded destination: winners (burn-for-payout),
  refunds (burn-for-refund), the creator's own account (validated owner and mint), the treasury
  (validated against Config). Each zeroes the owed amount before the transfer, so a failed transfer
  reverts the zeroing and a repeat call can never pay twice.
- **Forfeit.** Needs two distinct roots, a settled market, 30 days since settlement and something
  unclaimed; moves exactly `creator_owed`; leaves winners' pool and the protocol's share untouched
  (asserted in `programs/core/tests/gating.rs`).
- **Pause.** Does not block claims, refunds or the two-root cancel. Unpause needs the guardian plus
  two distinct roots in one transaction; one root alone is refused (error 30, seen on a real
  validator).
- **Guardian and roots** are hardcoded, distinct (const-asserted) and per network.
- **Config dials** are validated against hardcoded bounds on both initialise and update; the money
  routing fields need two roots; markets snapshot their dials.
- **Oracle.** A result needs two distinct attested reporters to agree; the 90-minute score is frozen
  by the program when normal time ends; results before kickoff are refused (error 6020, seen live);
  a finalised result is immutable except for the two-root override; a lying provider cannot finalise
  alone (run live: one reporter said 0–3, two said 2–1, the oracle finalised 2–1).

## Not covered by this review

Anything on a live network; the provider adapters against real provider responses (written from
docs only); economic attacks on the curve beyond the property tests (money conservation, pool never
exceeds the pot); Manifest's own code (external, audited by its authors); the aggregator's
non-Metis venues; denial of service against the RPC node.

## Before mainnet

1. Decide M1, M2, M3.
2. Real roots and guardian; decide who holds the programs' upgrade authority.
3. Outside audit of `programs/core`, `programs/oracle`, `authority`.
4. A devnet soak with the real providers, including a lying and a vanished provider.
