use bytemuck::{CheckedBitPattern, NoUninit};
use codama::CodamaType;

pub mod market;
pub use market::*;

pub mod authority;
pub use authority::*;

#[repr(u8)]
#[derive(NoUninit, CheckedBitPattern, Clone, Copy, PartialEq, CodamaType)]
pub enum UpdateKind {
    Score = 0,
    Penalties = 1,
    EventState = 2,
    // Reserved for MarketKind::Price, which has no handler yet (create_oracle.rs/update_oracle.rs
    // both reject any market_kind/update_kind other than Football today) hence this is for a 
    // future work
    #[allow(dead_code)]
    Price = 3,
    #[allow(dead_code)]
    Confidence = 4,
    #[allow(dead_code)]
    PublishTime = 5,
}
