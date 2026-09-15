use bytemuck::{CheckedBitPattern, NoUninit};
use shank::ShankType;

#[allow(warnings)]
#[repr(u8)]
#[derive(Clone, Default, Copy, NoUninit, CheckedBitPattern, ShankType)]
pub enum MarketKind {
    #[default]
    Football = 0,
    Price = 1,
}
