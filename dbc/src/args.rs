//! The data of the `initialize_backed_market` instruction.

use bytemuck::{Pod, Zeroable, from_bytes};
use pinocchio::error::ProgramError;

/// Arguments of `initialize_backed_market`: the common ones, then what the DBC raise needs.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct InitializeBackedMarketArgs {
    pub nonce: u64,
    pub num_outcomes: u8,
    pub condition_kind: u8,
    pub condition_param: u8,
    pub _padding: [u8; 5],
    pub platform: [u8; 32],
    /// The DBC base mint that backers hold. It need not exist yet (the keeper may create the pool
    /// after the market); activate_market checks it for real.
    pub backer_mint: [u8; 32],
    /// What the raise must deliver into the vault, in USDC atoms.
    pub seed_target: u64,
    /// Unix seconds. The raise must activate the market by then.
    pub raise_deadline: i64,
}

impl InitializeBackedMarketArgs {
    pub const LEN: usize = core::mem::size_of::<InitializeBackedMarketArgs>();

    pub fn load(bytes: &[u8]) -> Result<&Self, ProgramError> {
        if bytes.len() != Self::LEN {
            return Err(ProgramError::InvalidInstructionData);
        }
        Ok(from_bytes::<Self>(bytes))
    }
}

const _: () = assert!(core::mem::size_of::<InitializeBackedMarketArgs>() == 96);
