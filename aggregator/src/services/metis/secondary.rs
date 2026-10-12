//! Trading between holders, after the primary window: reading a market's Manifest order books and
//! working out what a buy or a sell would fill. Pure functions over decoded books - no network.
//!
//! Each outcome has its own book, slice/USDC, at an address derived from the market (core's
//! `close_primary` creates them), so there is nothing to look up and nothing a stranger can point
//! us at instead. A price on a book is quote atoms per base atom scaled by 10^18, and both tokens
//! have 6 decimals, so it is exactly dollars per slice.
//!
//! Rounding always goes against the trader: what a buy costs rounds up, what a sale fetches rounds
//! down. A quote can come in slightly under what the program delivers, never over.
//!
//! One thing it refuses outright: a book with *global* orders. Those are backed by a balance held
//! outside the book, and filling one needs accounts the swap we build doesn't carry, so quoting
//! past one would promise a fill the transaction could not make.

use manifest_client::Market as Book;
use solana_pubkey::Pubkey;

/// Price scale: raw = quote atoms per base atom x 10^18.
pub const D18: u128 = 1_000_000_000_000_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Level {
    /// Quote atoms per base atom, x 10^18.
    pub price: u128,
    pub base_atoms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Depth {
    /// Best (cheapest) first.
    pub asks: Vec<Level>,
    /// Best (highest) first.
    pub bids: Vec<Level>,
    /// Whether the book holds any global order. If it does it is not tradable through this
    /// adapter.
    pub has_global: bool,
}

impl Depth {
    pub fn tradable(&self) -> bool {
        !self.has_global
    }

    pub fn best_ask(&self) -> Option<u128> {
        self.asks.first().map(|l| l.price)
    }

    pub fn best_bid(&self) -> Option<u128> {
        self.bids.first().map(|l| l.price)
    }
}

/// The two books of a market read together: index 0 is YES, 1 is NO.
pub type Books = [Option<Depth>; 2];

/// Reads a book account. `None` if it isn't a Manifest market for exactly this pair - a stranger's
/// account at the right address is impossible (it is a PDA of our program), but a wrong pair here
/// would mean something is badly off, and quoting from it would be worse than not quoting.
///
/// Orders past their expiry are dropped: they are still in the account but can't be filled.
pub fn read_depth(data: &[u8], base_mint: &Pubkey, quote_mint: &Pubkey, current_slot: u32) -> Option<Depth> {
    let book = Book::try_from_bytes(data)?;
    if book.get_base_mint().to_bytes() != base_mint.to_bytes() || book.get_quote_mint().to_bytes() != quote_mint.to_bytes() {
        return None;
    }
    let mut depth = Depth::default();
    let collect = |orders: &mut dyn Iterator<Item = (u32, manifest_client::RestingOrder)>, out: &mut Vec<Level>, has_global: &mut bool| {
        for (_, order) in orders {
            if order.is_expired(current_slot) {
                continue;
            }
            if order.is_global() {
                *has_global = true;
                continue;
            }
            out.push(Level { price: order.get_price_raw(), base_atoms: order.num_base_atoms });
        }
    };
    collect(&mut book.iter_asks(), &mut depth.asks, &mut depth.has_global);
    collect(&mut book.iter_bids(), &mut depth.bids, &mut depth.has_global);
    // The iterators walk best-first already; sorting makes that an enforced property, not an
    // assumption about someone else's tree.
    depth.asks.sort_by_key(|l| l.price);
    depth.bids.sort_by_key(|l| core::cmp::Reverse(l.price));
    Some(depth)
}

/// What a wallet has on one book besides what sits in its token account: the balances parked in
/// its Manifest seat (withdrawable at any time) and the orders it has resting.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeatHolding {
    /// Slices (base) the seat holds that are not in an order: withdrawable.
    pub base_withdrawable: u64,
    /// USDC (quote) the seat holds: proceeds of its fills, unused bid funds. Withdrawable.
    pub quote_withdrawable: u64,
    /// Slices the wallet is offering in resting asks (not withdrawable until the order is gone).
    pub resting_ask_base: u64,
    /// How many orders (bids and asks) the wallet has resting.
    pub open_orders: u32,
    /// The wallet's resting orders as (sequence number, index in the book), so a claim can cancel
    /// them and bring what they lock back to the seat.
    pub orders: Vec<(u64, u32)>,
}

impl SeatHolding {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Every slice of this side the wallet still owns on the book.
    pub fn slices(&self) -> u64 {
        self.base_withdrawable.saturating_add(self.resting_ask_base)
    }
}

/// The wallet's seat and orders on a book, or `None` if it has no seat there (or the account is not
/// the book for exactly this pair).
pub fn seat_holding(data: &[u8], trader: &Pubkey, base_mint: &Pubkey, quote_mint: &Pubkey) -> Option<SeatHolding> {
    let book = Book::try_from_bytes(data)?;
    if book.get_base_mint().to_bytes() != base_mint.to_bytes() || book.get_quote_mint().to_bytes() != quote_mint.to_bytes() {
        return None;
    }
    let trader = manifest_client::Pubkey::new_from_array(trader.to_bytes());
    let (index, seat) = book.find_trader_seat(&trader)?;
    let mut holding = SeatHolding { base_withdrawable: seat.base_withdrawable_balance, quote_withdrawable: seat.quote_withdrawable_balance, ..Default::default() };
    for (at, order) in book.iter_asks() {
        if order.trader_index == index {
            holding.resting_ask_base = holding.resting_ask_base.saturating_add(order.num_base_atoms);
            holding.open_orders += 1;
            holding.orders.push((order.sequence_number, at));
        }
    }
    for (at, order) in book.iter_bids() {
        if order.trader_index == index {
            holding.open_orders += 1;
            holding.orders.push((order.sequence_number, at));
        }
    }
    Some(holding)
}

/// What a fill came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Fill {
    /// Slices bought or sold.
    pub base_atoms: u64,
    /// USDC spent or received.
    pub quote_atoms: u64,
    /// Whether the whole request could be met (up to a rounding atom).
    pub complete: bool,
}

fn cost_up(base: u128, price: u128) -> u128 {
    (base * price).div_ceil(D18)
}

fn proceeds_down(base: u128, price: u128) -> u128 {
    base * price / D18
}

/// What `budget` USDC atoms buys, walking the asks from the cheapest.
pub fn buy_with(depth: &Depth, budget: u64) -> Fill {
    let mut remaining = budget as u128;
    let mut bought: u128 = 0;
    for level in &depth.asks {
        if level.price == 0 || remaining == 0 {
            continue;
        }
        let whole = cost_up(level.base_atoms as u128, level.price);
        if whole <= remaining {
            bought += level.base_atoms as u128;
            remaining -= whole;
        } else {
            // Only part of this level: as many slices as the remaining budget covers, rounded down.
            let affordable = (remaining * D18 / level.price).min(level.base_atoms as u128);
            if affordable > 0 {
                bought += affordable;
                remaining -= cost_up(affordable, level.price);
            }
            break;
        }
    }
    Fill {
        base_atoms: bought as u64,
        quote_atoms: (budget as u128 - remaining) as u64,
        complete: remaining <= 1,
    }
}

/// What selling `base_atoms` slices fetches, walking the bids from the highest.
pub fn sell_into(depth: &Depth, base_atoms: u64) -> Fill {
    let mut remaining = base_atoms as u128;
    let mut received: u128 = 0;
    for level in &depth.bids {
        if remaining == 0 {
            break;
        }
        let take = remaining.min(level.base_atoms as u128);
        received += proceeds_down(take, level.price);
        remaining -= take;
    }
    Fill {
        base_atoms: (base_atoms as u128 - remaining) as u64,
        quote_atoms: received as u64,
        complete: remaining == 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A price of `cents` cents per slice, in the book's scale.
    fn p(cents: u128) -> u128 {
        cents * D18 / 100
    }

    fn depth(asks: &[(u128, u64)], bids: &[(u128, u64)]) -> Depth {
        Depth {
            asks: asks.iter().map(|&(c, b)| Level { price: p(c), base_atoms: b }).collect(),
            bids: bids.iter().map(|&(c, b)| Level { price: p(c), base_atoms: b }).collect(),
            has_global: false,
        }
    }

    #[test]
    fn a_buy_walks_the_asks_from_the_cheapest() {
        // 10 slices at 50c, then 20 at 60c.
        let d = depth(&[(50, 10_000_000), (60, 20_000_000)], &[]);
        // $5 buys exactly the first level.
        let f = buy_with(&d, 5_000_000);
        assert_eq!((f.base_atoms, f.quote_atoms, f.complete), (10_000_000, 5_000_000, true));
        // $11 buys the first level ($5) and then $6 of the second: 10 slices at 60c.
        let f = buy_with(&d, 11_000_000);
        assert_eq!((f.base_atoms, f.quote_atoms), (20_000_000, 11_000_000));
        assert!(f.complete);
    }

    #[test]
    fn a_buy_bigger_than_the_book_is_filled_as_far_as_it_goes_and_says_so() {
        let d = depth(&[(50, 10_000_000)], &[]);
        let f = buy_with(&d, 50_000_000);
        assert_eq!((f.base_atoms, f.quote_atoms), (10_000_000, 5_000_000));
        assert!(!f.complete, "only $5 of the $50 could be spent");
        assert_eq!(buy_with(&Depth::default(), 5_000_000), Fill { base_atoms: 0, quote_atoms: 0, complete: false });
    }

    #[test]
    fn a_buy_that_ends_inside_a_level_never_spends_more_than_its_budget() {
        let d = depth(&[(70, 1_000_000_000)], &[]);
        for budget in [1u64, 7, 99, 1_000_001, 12_345_678] {
            let f = buy_with(&d, budget);
            assert!(f.quote_atoms <= budget, "spent {} of {budget}", f.quote_atoms);
            // And the slices it claims are really paid for at that price, rounded up.
            assert!(cost_up(f.base_atoms as u128, p(70)) <= budget as u128);
        }
    }

    #[test]
    fn a_sale_walks_the_bids_from_the_highest() {
        // Bids: 8 slices at 40c, then 10 at 30c.
        let d = depth(&[], &[(40, 8_000_000), (30, 10_000_000)]);
        let f = sell_into(&d, 8_000_000);
        assert_eq!((f.base_atoms, f.quote_atoms, f.complete), (8_000_000, 3_200_000, true));
        // Selling 12: 8 at 40c ($3.20) and 4 at 30c ($1.20).
        let f = sell_into(&d, 12_000_000);
        assert_eq!(f.quote_atoms, 4_400_000);
        assert!(f.complete);
    }

    #[test]
    fn a_sale_bigger_than_the_bids_sells_what_they_cover() {
        let d = depth(&[], &[(40, 5_000_000)]);
        let f = sell_into(&d, 9_000_000);
        assert_eq!((f.base_atoms, f.quote_atoms, f.complete), (5_000_000, 2_000_000, false));
        assert_eq!(sell_into(&Depth::default(), 1_000_000).base_atoms, 0);
    }

    #[test]
    fn rounding_goes_against_the_trader_both_ways() {
        // 1 slice atom at 0.5 atoms per atom: a buy costs 1 (0.5 rounded up), a sale fetches 0.
        let d = Depth {
            asks: vec![Level { price: D18 / 2, base_atoms: 10 }],
            bids: vec![Level { price: D18 / 2, base_atoms: 10 }],
            has_global: false,
        };
        assert_eq!(cost_up(1, D18 / 2), 1);
        assert_eq!(proceeds_down(1, D18 / 2), 0);
        assert_eq!(sell_into(&d, 1).quote_atoms, 0);
        // Ten of them: costs exactly 5, and fetches exactly 5.
        assert_eq!(buy_with(&d, 5).base_atoms, 10);
        assert_eq!(sell_into(&d, 10).quote_atoms, 5);
    }

    #[test]
    fn best_prices_and_the_global_flag() {
        let d = depth(&[(50, 1), (60, 1)], &[(40, 1), (30, 1)]);
        assert_eq!((d.best_ask(), d.best_bid()), (Some(p(50)), Some(p(40))));
        assert!(d.tradable());
        let g = Depth { has_global: true, ..d };
        assert!(!g.tradable(), "a book with global orders isn't routed");
        assert_eq!(Depth::default().best_ask(), None);
    }

    #[test]
    fn something_that_is_not_a_book_is_not_read() {
        let (a, b) = (Pubkey::new_unique(), Pubkey::new_unique());
        assert!(read_depth(&[], &a, &b, 0).is_none());
        assert!(read_depth(&[0u8; 256], &a, &b, 0).is_none(), "right size, wrong discriminant");
    }
}
