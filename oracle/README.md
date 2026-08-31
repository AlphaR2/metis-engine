# openstake-oracle

Scaffold only. Explicitly deferred — see `../README.md` and the restructure
plan this crate was created under.

Today, `settle_market` (in `programs/core`) checks a single hardcoded
`constants::AUTHORITY` pubkey — a placeholder, not a real oracle. This crate
is where a real one gets built, as a genuinely external integration rather
than an internal module of `openstake-core` (mirroring how a reference
implementation treats external price/result oracles as separate programs it
CPIs into, not code it owns).

Not started.
