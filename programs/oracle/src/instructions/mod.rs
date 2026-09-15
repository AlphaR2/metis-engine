use pinocchio::error::ProgramError;

pub mod create_oracle;
pub use create_oracle::*;

pub mod update_oracle;
pub use update_oracle::*;

pub mod close_oracle;
pub use close_oracle::*;

pub mod football;

pub mod authority;
pub use authority::*;

use shank::ShankInstruction;

use crate::{
    instructions::{
        close_oracle::CloseArgs,
        football::{FootballSettled, UpdateFootballArgs},
    },
    state::{MarketKind, UpdateKind},
};

/// Runtime-only instruction discriminator used by the program entrypoint.
pub enum POracle {
    CreateOracle = 0,
    UpdateOracle = 1,
    CloseOracle = 2,
    InitializeAuthority = 3,
    AttestAuthority = 4,
    RemoveAuthority = 5,
}

impl TryFrom<&u8> for POracle {
    type Error = ProgramError;

    fn try_from(value: &u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(POracle::CreateOracle),
            1 => Ok(POracle::UpdateOracle),
            2 => Ok(POracle::CloseOracle),
            3 => Ok(POracle::InitializeAuthority),
            4 => Ok(POracle::AttestAuthority),
            5 => Ok(POracle::RemoveAuthority),

            _ => Err(ProgramError::InvalidInstructionData),
        }
    }
}

/// Public instruction schema used only for Shank IDL extraction.
///
/// This preserves the wire format consumed by the runtime router: a one-byte
/// discriminator followed by the fields declared for the selected variant.
/// The discriminants intentionally mirror `POracle`.
#[repr(C, u8)]
#[allow(dead_code)]
#[derive(ShankInstruction)]
#[rustfmt::skip]
pub enum OracleInstructionSchema {
    #[account(0, name = "creator", sig, mut)]
    #[account(1, name = "oracle", mut)]
    #[account(2, name = "system_program")]
    #[account(3, name = "individual_authority", optional)]
    CreateOracle {
        market_kind: MarketKind,
        football: FootballSettled,
    } = 0,

    #[account(0, name = "creator", sig, mut)]
    #[account(1, name = "oracle", mut)]
    #[account(2, name = "system_program")]
    #[account(3, name = "individual_authority", optional)]
    UpdateOracle {
        update_kind: UpdateKind,
        football_update: UpdateFootballArgs,
    } = 1,

    #[account(0, name = "initiator", mut)]
    #[account(1, name = "voter", sig)]
    #[account(2, name = "oracle", mut)]
    #[account(3, name = "closer_quorum", mut)]
    #[account(4, name = "authority")]
    #[account(5, name = "system_program")]
    CloseOracle {
        close: CloseArgs,
    } = 2,

    #[account(0, name = "initializer", sig, mut)]
    #[account(1, name = "authority", mut)]
    #[account(2, name = "system_program")]
    InitializeAuthority = 3,

    #[account(0, name = "attester", sig, mut)]
    #[account(1, name = "authority")]
    #[account(2, name = "candidate")]
    #[account(3, name = "individual_authority", mut)]
    #[account(4, name = "system_program")]
    #[account(5, name = "attester_individual_authority", optional)]
    AttestAuthority = 4,

    #[account(0, name = "root", sig)]
    #[account(1, name = "authority")]
    #[account(2, name = "candidate")]
    #[account(3, name = "individual_authority", mut)]
    #[account(4, name = "recipient", mut)]
    RemoveAuthority = 5,
}
