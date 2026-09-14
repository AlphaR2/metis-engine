use bytemuck::{CheckedBitPattern, NoUninit};
use shank::ShankType;

pub mod market;
pub use market::*;

pub mod authority;
pub use authority::*;

pub mod close_quorum;
pub use close_quorum::*;

#[repr(u8)]
#[derive(NoUninit, CheckedBitPattern, Clone, Copy, PartialEq, ShankType)]
pub enum UpdateKind {
    Score = 0,
    Penalties = 1,
    EventState = 2,
    Price = 3,
    Confidence = 4,
    PublishTime = 5,
}
