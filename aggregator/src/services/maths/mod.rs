//! Pure numeric engines, no I/O, no repository/graph access - just the math that `matching` and
//! `routing` (and any future vendor needing book-walking) call into.

pub mod markets;
pub mod matching;
pub mod orderbook;
pub mod routing;
