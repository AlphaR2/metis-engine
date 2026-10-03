//! Shared book-walking math for order-book venues (Jupiter). A bonding-curve venue like
//! Panta gets one quote straight from its API; an order-book venue has to compute the effective
//! fill price/size itself by walking levels until the requested amount is exhausted.
//!
//! Uses `Decimal`, not `f64` - exact fixed-point arithmetic, no float rounding error creeping
//! into a fill estimate.

#![allow(dead_code)] // Kept for the live price check the UI will want; no route calls it today.

use rust_decimal::Decimal;

/// One price/size level of a book side, straight from the venue.
#[derive(Debug, Clone, Copy)]
pub struct Level {
    pub price: Decimal,
    pub size: Decimal,
}

/// The outcome of walking a book: what actually filled, and at what price.
#[derive(Debug, Clone, Copy)]
pub struct FillResult {
    pub avg_price: Decimal,
    pub filled_size: Decimal,
    /// true if the book had enough depth to fill the full requested notional.
    pub fully_filled: bool,
}

/// Walks `levels` (best price first) spending up to `notional_usdc`, returning the size actually
/// bought and the size-weighted average price. Mirrors what a real limit order would fill against
/// this snapshot of the book - it is not itself a guarantee of execution, since the book can move
/// between quoting and trading.
pub fn walk_book(levels: &[Level], notional_usdc: Decimal) -> FillResult {
    let mut remaining = notional_usdc;
    let mut filled_size = Decimal::ZERO;
    let mut filled_notional = Decimal::ZERO;

    for level in levels {
        if remaining <= Decimal::ZERO {
            break;
        }
        let level_notional = level.price * level.size;
        let take_notional = remaining.min(level_notional);
        let take_size = if level.price > Decimal::ZERO { take_notional / level.price } else { Decimal::ZERO };

        filled_size += take_size;
        filled_notional += take_notional;
        remaining -= take_notional;
    }

    let avg_price = if filled_size > Decimal::ZERO { filled_notional / filled_size } else { Decimal::ZERO };

    FillResult {
        avg_price,
        filled_size,
        fully_filled: remaining <= Decimal::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    #[test]
    fn fills_fully_within_one_level() {
        let levels = [Level { price: dec("0.5"), size: dec("1000") }];
        let result = walk_book(&levels, dec("100"));
        assert!(result.fully_filled);
        assert_eq!(result.filled_size, dec("200"));
        assert_eq!(result.avg_price, dec("0.5"));
    }

    #[test]
    fn walks_multiple_levels_and_reports_partial_fill() {
        let levels = [
            Level { price: dec("0.50"), size: dec("100") }, // $50 notional
            Level { price: dec("0.55"), size: dec("100") }, // $55 notional
        ];
        let result = walk_book(&levels, dec("200"));
        assert!(!result.fully_filled);
        assert_eq!(result.filled_size, dec("200"));
        assert!(result.avg_price > dec("0.50") && result.avg_price < dec("0.55"));
    }

    #[test]
    fn empty_book_fills_nothing() {
        let result = walk_book(&[], dec("100"));
        assert_eq!(result.filled_size, Decimal::ZERO);
        assert!(!result.fully_filled);
    }
}
