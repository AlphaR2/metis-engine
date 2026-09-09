use bytemuck::{CheckedBitPattern, NoUninit};
use codama::CodamaType;

#[allow(warnings)]
#[repr(u8)]
#[derive(Clone, Default, Copy, NoUninit, CheckedBitPattern, CodamaType)]
pub enum MarketKind {
    #[default]
    Football = 0,
    Price = 1,
}
