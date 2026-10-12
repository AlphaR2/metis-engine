# metis-oracle

A standalone Pinocchio program, separate from `metis-core`. It holds match results on chain. The
market program reads its accounts directly (no CPI) through this crate's own types, so the two can
never disagree about a layout.

## The result rule

A football match is one `FootballSettled` account, found at a PDA of the fixture's Merkle root
(which commits to each data provider's own id for the match). It always starts `Scheduled`, with
the kickoff recorded. Reporters (keys that two roots attested) call `submit_result` with the phase,
score and penalties they see. **A phase or score is written only when two distinct reporters agree.**
No single reporter, and no single provider, can decide a result.

- The phase machine only allows valid steps: `Scheduled → FirstHalf → HalfTime → SecondHalf →
  Finished`, with an extra-time and penalties branch.
- The 90-minute (regulation) score is frozen by the program when normal time ends. Markets are
  judged on it, so extra time and penalties never leak in.
- Results before kickoff are refused (`ResultBeforeKickoff`).
- Once `Finished` is agreed it is **immutable**, except for a two-root override (`override_result`:
  set the result, or void it) within 24 h. An override restarts the dispute clock.
- `close_oracle` is guarded: 30 days after a result is final or voided, or after kickoff if the match
  never finished. A market whose oracle account is gone counts as having no result.

## Trust

Three root signers and a guardian, hardcoded per network in `authority/src/network.rs` (shared with
`metis-core`). Roots attest reporters (two distinct roots complete an attestation). One root can
`pause`; `unpause` needs the guardian plus two distinct roots in one transaction. While paused,
nothing that changes a result or who may report runs.

## Instructions

`create_oracle`, `submit_result`, `override_result`, `close_oracle`, `initialize_authority`,
`attest_authority`, `remove_authority`, `pause`, `unpause`. See `src/instructions/mod.rs` (`POracle`)
for the account lists.

## Tests

`cargo build-sbf --manifest-path programs/oracle/Cargo.toml` first (the litesvm suite loads
`target/deploy/oracle.so`), then `cargo test -p metis-oracle`: quorum (one report does not finalise,
two agreeing do, two disagreeing do not), phase transitions, the frozen regulation score,
immutability, override and void, pause, close, and the authority lifecycle. The keeper's end-to-end
tests (`tools/oracle-keeper/tests/e2e.rs`) run the real program against provider-shaped data.
