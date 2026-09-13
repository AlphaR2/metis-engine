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
    Price = 3,
    Confidence = 4,
    PublishTime = 5,
}
