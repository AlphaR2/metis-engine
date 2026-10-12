//! The Fill Score: how likely a call is to fill, venue by venue, from what each venue actually says.
//!
//! The same question can be listed on more than one venue (the event graph finds those). For a
//! bet of a given size on a given side, each listing is asked for a real quote, and its own
//! depth is read off the catalog. Nothing here is estimated from anything but those two answers:
//!
//! * **coverage**: how much of the amount the venue's quote says it can fill, from 0 to 1 (a quote
//!   that fills it all is 1; a partial fill counts the share filled);
//! * **depth**: the market's own liquidity against ten times the amount, capped at 1, so a bet that
//!   is a rounding error against the market scores as fully backed and a bet that is a large share
//!   of the whole market does not. A venue that reports no liquidity is not penalised, and says so
//!   by leaving `depth_usdc` empty.
//!
//! `score = round(100 x coverage x depth)`. A listing that cannot be traded through the API
//! (view only, closed, or its venue failing) scores 0 and says why.
//!
//! `other_side_usdc` is the part of a win's payout that is not your own stake: a share pays $1, so
//! `shares - amount`. It is what the opposite position puts up against your call, taken from the
//! quote, not guessed.

use rust_decimal::Decimal;
use serde::Serialize;
use utoipa::ToSchema;

use crate::models::venue::VenueQuote;

/// A bet counts as fully backed when the market already holds at least this many times its size.
pub const DEPTH_MULTIPLE: i64 = 10;

/// What the score is made of, in words, for the UI to show next to the number.
pub const METHOD: &str = "Fill Score = 100 x (share of your amount the venue's quote can fill) x (the market's liquidity against 10 times your amount, at most 1). From each venue's own quote and liquidity; nothing is estimated.";

#[derive(Debug, Clone, Serialize, ToSchema, PartialEq)]
pub struct FillOption {
    pub venue_id: String,
    pub venue_market_id: String,
    pub title: String,
    /// Whether an order can be built for it through this API.
    pub tradable: bool,
    /// Whether the venue answered a quote.
    pub quoted: bool,
    /// Why it scores 0 or 'partial', when it does.
    pub note: Option<String>,
    /// What one share would cost on average, 0 to 1.
    #[schema(value_type = Option<String>)]
    pub avg_price: Option<Decimal>,
    /// Shares the amount would buy.
    #[schema(value_type = Option<String>)]
    pub shares: Option<Decimal>,
    /// Whether the quote fills the whole amount.
    pub fully_filled: bool,
    #[schema(value_type = Option<String>)]
    pub fee_usdc: Option<Decimal>,
    /// The market's own liquidity as the venue reports it.
    #[schema(value_type = Option<String>)]
    pub depth_usdc: Option<Decimal>,
    /// What the opposite position puts against this call: `shares - amount`, never below 0.
    #[schema(value_type = Option<String>)]
    pub other_side_usdc: Option<Decimal>,
    /// 0 to 100.
    pub score: u8,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FillScore {
    pub side: String,
    #[schema(value_type = String)]
    pub amount_usdc: Decimal,
    /// How the score is made.
    pub method: String,
    /// Best first.
    pub options: Vec<FillOption>,
}

/// What is known about one listing before it is scored.
pub struct Listing {
    pub venue_id: String,
    pub venue_market_id: String,
    pub title: String,
    pub tradable: bool,
    pub depth: Option<Decimal>,
    /// The venue's quote, or why it could not give one.
    pub quote: Result<VenueQuote, String>,
}

fn clamp01(value: Decimal) -> Decimal {
    value.max(Decimal::ZERO).min(Decimal::ONE)
}

/// Scores one listing for a bet of `amount`.
pub fn score(amount: Decimal, listing: Listing) -> FillOption {
    let Listing { venue_id, venue_market_id, title, tradable, depth, quote } = listing;
    let base = |note: Option<String>, quoted: bool| FillOption {
        venue_id: venue_id.clone(),
        venue_market_id: venue_market_id.clone(),
        title: title.clone(),
        tradable,
        quoted,
        note,
        avg_price: None,
        shares: None,
        fully_filled: false,
        fee_usdc: None,
        depth_usdc: depth,
        other_side_usdc: None,
        score: 0,
    };

    if !tradable {
        return base(Some("View only: this listing can't be traded through Metis.".to_string()), false);
    }
    let quote = match quote {
        Ok(quote) => quote,
        Err(reason) => return base(Some(format!("No quote: {reason}")), false),
    };
    if amount <= Decimal::ZERO || quote.avg_price <= Decimal::ZERO || quote.effective_shares_or_size <= Decimal::ZERO {
        return FillOption { quoted: true, note: Some("The venue quoted nothing for this amount.".to_string()), ..base(None, true) };
    }

    // How much of the amount the quote covers: all of it if the venue says so, otherwise the cost of
    // the shares it will fill against the amount.
    let coverage = if quote.fully_filled { Decimal::ONE } else { clamp01(quote.effective_shares_or_size * quote.avg_price / amount) };
    let depth_factor = match depth {
        Some(depth) => clamp01(depth / (amount * Decimal::from(DEPTH_MULTIPLE))),
        None => Decimal::ONE,
    };
    let value = (Decimal::from(100) * coverage * depth_factor).round().to_string().parse::<u8>().unwrap_or(0).min(100);
    let other_side = (quote.effective_shares_or_size - amount).max(Decimal::ZERO);
    let note = if !quote.fully_filled { Some("The venue can fill only part of this amount.".to_string()) } else { None };

    FillOption {
        venue_id,
        venue_market_id,
        title,
        tradable,
        quoted: true,
        note,
        avg_price: Some(quote.avg_price),
        shares: Some(quote.effective_shares_or_size),
        fully_filled: quote.fully_filled,
        fee_usdc: Some(quote.fee_usdc),
        depth_usdc: depth,
        other_side_usdc: Some(other_side),
        score: value,
    }
}

/// Best first: by score, then by the lower price (a cheaper share is a better fill), then by venue id
/// so the order is the same on every call.
pub fn rank(options: &mut [FillOption]) {
    options.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.avg_price.unwrap_or(Decimal::MAX).cmp(&b.avg_price.unwrap_or(Decimal::MAX)))
            .then_with(|| a.venue_id.cmp(&b.venue_id))
            .then_with(|| a.venue_market_id.cmp(&b.venue_market_id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn quote(shares: &str, price: &str, full: bool) -> VenueQuote {
        VenueQuote {
            venue: "panta",
            venue_market_id: "m".to_string(),
            side: "yes".to_string(),
            amount_usdc: d("10"),
            effective_shares_or_size: d(shares),
            avg_price: d(price),
            fee_usdc: d("0.05"),
            fully_filled: full,
            expires_at: None,
        }
    }

    fn listing(venue: &str, depth: Option<&str>, q: Result<VenueQuote, String>) -> Listing {
        Listing {
            venue_id: venue.to_string(),
            venue_market_id: "m".to_string(),
            title: "Will X?".to_string(),
            tradable: true,
            depth: depth.map(d),
            quote: q,
        }
    }

    #[test]
    fn a_full_fill_in_a_deep_market_scores_100() {
        let o = score(d("10"), listing("panta", Some("76529"), Ok(quote("22.7", "0.44", true))));
        assert_eq!(o.score, 100);
        assert!(o.fully_filled && o.quoted);
        assert_eq!(o.other_side_usdc, Some(d("12.7")), "a share pays 1, so 22.7 shares put 12.7 against the 10 staked");
    }

    #[test]
    fn depth_only_counts_when_the_market_is_small_next_to_the_bet() {
        // 30 of liquidity against 10x a 10 bet (100): 0.3.
        let thin = score(d("10"), listing("metis", Some("30"), Ok(quote("22.7", "0.44", true))));
        assert_eq!(thin.score, 30);
        // Exactly ten times the bet is fully backed, and more does not raise it above 100.
        assert_eq!(score(d("10"), listing("metis", Some("100"), Ok(quote("22.7", "0.44", true)))).score, 100);
        assert_eq!(score(d("10"), listing("metis", Some("100000"), Ok(quote("22.7", "0.44", true)))).score, 100);
    }

    #[test]
    fn unknown_depth_is_not_a_penalty_and_says_so() {
        let o = score(d("10"), listing("panta", None, Ok(quote("22.7", "0.44", true))));
        assert_eq!(o.score, 100);
        assert_eq!(o.depth_usdc, None);
    }

    #[test]
    fn a_partial_fill_scores_the_share_it_covers() {
        // 10 shares at 0.5 cost 5 of the 10: half.
        let o = score(d("10"), listing("panta", Some("1000"), Ok(quote("10", "0.5", false))));
        assert_eq!(o.score, 50);
        assert!(o.note.is_some());
    }

    #[test]
    fn a_view_only_listing_scores_zero_without_asking_for_a_quote() {
        let mut l = listing("panta", Some("1000"), Ok(quote("22.7", "0.44", true)));
        l.tradable = false;
        let o = score(d("10"), l);
        assert_eq!((o.score, o.quoted), (0, false));
        assert!(o.note.unwrap().contains("View only"));
    }

    #[test]
    fn a_failed_quote_scores_zero_and_keeps_the_reason() {
        let o = score(d("10"), listing("metis", Some("500"), Err("timed out".to_string())));
        assert_eq!(o.score, 0);
        assert!(o.note.unwrap().contains("timed out"));
        assert_eq!(o.depth_usdc, Some(d("500")), "depth is still reported");
    }

    #[test]
    fn nothing_quoted_scores_zero() {
        let o = score(d("10"), listing("panta", Some("1000"), Ok(quote("0", "0.44", true))));
        assert_eq!(o.score, 0);
        assert!(o.quoted);
    }

    #[test]
    fn a_stake_bigger_than_its_payout_never_reports_a_negative_other_side() {
        let o = score(d("10"), listing("panta", None, Ok(quote("8", "1", true))));
        assert_eq!(o.other_side_usdc, Some(Decimal::ZERO));
    }

    #[test]
    fn ranking_is_by_score_then_price_then_venue() {
        let mk = |venue: &str, score: u8, price: &str| FillOption {
            venue_id: venue.to_string(),
            venue_market_id: "m".to_string(),
            title: String::new(),
            tradable: true,
            quoted: true,
            note: None,
            avg_price: Some(d(price)),
            shares: None,
            fully_filled: true,
            fee_usdc: None,
            depth_usdc: None,
            other_side_usdc: None,
            score,
        };
        let mut options = vec![mk("metis", 80, "0.40"), mk("panta", 100, "0.50"), mk("zeta", 80, "0.30")];
        rank(&mut options);
        let order: Vec<&str> = options.iter().map(|o| o.venue_id.as_str()).collect();
        assert_eq!(order, ["panta", "zeta", "metis"]);
    }
}
