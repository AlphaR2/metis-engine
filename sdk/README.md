# openstake-sdk

Scaffold only. Off-chain client crate for third-party providers to plug
into Openstake — never compiled to SBF, never a dependency of
`programs/core`.

A provider is a front-end/app that lets its own users: (a) see Openstake's
markets, (b) create Openstake markets while collecting their own fee, (c)
place primary or secondary bets, (d) cash out / settle. See
`src/lib.rs`'s `OpenstakeProvider` trait for the current stub shape.

Not started.
