use bytemuck::{CheckedBitPattern, NoUninit};

#[allow(warnings)]
#[repr(u8)]
#[derive(Clone, Default, Copy, NoUninit, CheckedBitPattern)]
pub enum MarketKind {
    #[default]
    Football = 0,
    Price = 1,
}
