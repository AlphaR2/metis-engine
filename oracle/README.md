# metis-oracle

A standalone Pinocchio program, genuinely separate from `metis-core` — mirrors how a reference
implementation treats external price/result oracles as separate programs it CPIs into, not code
it owns. `settle_market` (in `programs/core`) still checks a single hardcoded
`constants::AUTHORITY` pubkey today; this crate is where the real oracle this program will
eventually defer to lives, not yet wired into `settle_market` itself (see `docs/plan.md` item 2
at the workspace root).

## What's here

Two subsystems:

**A supermajority + delegated-attestation authority system** (`state/authority.rs`,
`instructions/authority/`). Three hardcoded root keys (`constants::SUPERMAJORITIES` — vanity
placeholder addresses, swap for real production keys before any real deploy, same caveat as
`programs/core`'s own `constants::AUTHORITY`) can act directly, or delegate update rights to other
addresses via an `IndividualOAuthority` account that requires two distinct root attestations
before it's considered valid. `initialize_authority` creates the root `OracleAuthority` PDA,
`attest_authority` lets a root (or an already-attested delegate) vouch for a new candidate,
`remove_authority` lets a root revoke a delegate and reclaim rent.

**A football-match oracle** (`instructions/football/`) — one `MarketKind` variant of what's meant
to eventually be several (`MarketKind::Price` exists in the enum but has no handler yet).
`FootballSettled` tracks a match by its fixture Merkle root (the account's own PDA identity),
bitpacked score/penalties, and a match-phase state machine (`Scheduled` → `FirstHalf` → `HalfTime`
→ `SecondHalf` → `Finished`, with an extra-time/penalties branch) that only allows valid
transitions.

## Instructions

`CreateOracle`, `UpdateOracle`, `InitializeAuthority`, `AttestAuthority`, `RemoveAuthority` — see
`src/instructions/mod.rs`'s `POracle` enum and `OracleInstructionSchema` for the exact account
lists per instruction.

## IDL

`build.rs` uses [Codama](https://github.com/codama-idl/codama) to generate `../idl/oracle.json` at
build time from the `#[derive(CodamaAccount)]`/`#[derive(CodamaType)]`/`#[derive(CodamaInstructions)]`
annotations in `state/` and `instructions/`.

## Test coverage

Covered today: create/update oracle happy paths, one negative case (an unattested caller cannot
create), the full `initialize → attest (both roots) → remove` authority lifecycle end-to-end, and
pure unit tests for `IndividualOAuthority::record_attestation`, `OracleAuthority::is_supermajority`,
and `CFootballMatchPhase::can_transition_to`.

Not yet covered: most of `process_attest_authority`'s validation surface beyond the root-attester
happy path (wrong PDAs, unauthorized attesters, double-attestation, the non-root-attester branch),
`process_remove_authority` beyond its one happy path, and the individual guard-branch rejections
on `create_oracle`/`update_oracle` in isolation. Tracked as its own item in the workspace root's
`docs/plan.md`.
