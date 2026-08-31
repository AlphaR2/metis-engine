//! The CLOB part — secondary-market order matching. Not built yet.
//!
//! Reserved as its own top-level part alongside amm/ and settlement/ per
//! the four-part architecture (AMM, CLOB, Settlement, State Engine) — each
//! part owns its own state.rs rather than sharing one state_engine/ folder.
//! See the outcome-tokens-as-tradeable-assets prerequisite
//! discussed for this to be workable at all, and the primary-to-secondary
//! trigger design (liquidity/volume or time based, config-driven) noted
//! separately — neither is designed yet.
