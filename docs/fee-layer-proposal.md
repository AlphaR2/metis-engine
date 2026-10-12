# Fee layer — recovering what the crank spends (proposal, for review)

Nothing here is built. It is the options and a recommendation, for you to decide.

## The cost

The crank pays real SOL. Measured per step (litesvm, and the local validator agrees on the first):

| Step | Cost | Notes |
|---|---|---|
| `close_primary` | about 0.0146 SOL | two Manifest books and four vault accounts' rent; the big one |
| `settle_market` | 5,000 lamports | just the transaction fee |
| first protocol `sweep_fees` | about 0.002 SOL | creates the treasury's token account once |
| later sweeps | 5,000 lamports | |
| `cancel_market` | 5,000 lamports | |
| `close_market` | negative | returns more rent than it costs |

Per market, roughly **0.015 SOL** (about $2–3 at current prices) before any rent comes back,
dominated by the books. At 100 markets a day that is about 1.5 SOL a day.

## Where it could come from

1. **A market-creation fee**, taken by `initialize_market` into the treasury, sized to cover
   `close_primary`. Pays for the cost where it is caused. Needs a new Config dial with a hardcoded
   ceiling, snapshotted per market. Discourages spam markets too.
2. **A slice of the rake.** The protocol already gets 50% of fees. Cost is covered out of revenue the
   treasury already earns, no new charge. But a market that is cancelled earns no rake and still
   cost the books.
3. **A small fee on bets.** Already exists (`fee_bps`); raising it to cover the crank taxes every
   user for what a few markets cost.
4. **The rent `close_market` returns.** Already goes to the operator that closes. Covers the
   settle/sweep/close costs and part of `close_primary`. Free, but arrives 90 days later.
5. **Defer the books.** Open the order books only if someone asks (a "make this market tradable"
   instruction that the requester pays for). Most markets might never need them. Changes the product:
   no secondary market until someone pays.

## Recommendation

Combine **1 and 4**: a flat creation fee (a dial, ceiling set in code, starting near the measured
0.015 SOL's USDC value) pays for the books up front, and the rent that comes back at `close_market`
is the operator's float. Keep the rake for protocol revenue. Revisit option 5 if most markets turn
out never to trade.

## What it needs if you choose it

- Config: `creation_fee` (USDC atoms), hardcoded `MAX_CREATION_FEE`, snapshotted on `Market`.
- `initialize_market`: transfer it to the treasury's USDC account in the same transaction.
- Tests: bounds, snapshot, treasury balance, a creator who cannot afford both seed and fee.
- Aggregator: show it as a cost line in the create preview.
- `Report::spent` already measures each step, so the fee can be checked against real cost.

## Decision needed

Which option (or combination), and a starting value.
