use pinocchio::error::ProgramError;

pub mod create_oracle;
pub use create_oracle::*;

pub mod update_oracle;
pub use update_oracle::*;

pub mod football;

pub mod authority;
pub use authority::*;

use codama::CodamaInstructions;

use crate::{
    instructions::football::{FootballSettled, UpdateFootballArgs},
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

/// Public instruction schema used only for Codama IDL extraction.
///
/// This preserves the wire format consumed by the runtime router: a one-byte
/// discriminator followed by the fields declared for the selected variant.
/// The discriminants intentionally mirror `POracle`.
#[repr(C, u8)]
#[allow(dead_code)]
#[derive(CodamaInstructions)]
#[rustfmt::skip]
pub enum OracleInstructionSchema {
    #[codama(account(name = "creator", signer, writable))]
    #[codama(account(name = "oracle", writable))]
    #[codama(account(name = "system_program"))]
    #[codama(account(name = "individual_authority", optional))]
    CreateOracle {
        market_kind: MarketKind,
        football: FootballSettled,
    } = 0,

    #[codama(account(name = "creator", signer, writable))]
    #[codama(account(name = "oracle", writable))]
    #[codama(account(name = "system_program"))]
    #[codama(account(name = "individual_authority", optional))]
    UpdateOracle {
        update_kind: UpdateKind,
        football_update: UpdateFootballArgs,
    } = 1,

    #[codama(account(name = "initializer", signer, writable))]
    #[codama(account(name = "authority", writable))]
    #[codama(account(name = "system_program"))]
    InitializeAuthority = 3,

    #[codama(account(name = "attester", signer, writable))]
    #[codama(account(name = "authority"))]
    #[codama(account(name = "candidate"))]
    #[codama(account(name = "individual_authority", writable))]
    #[codama(account(name = "system_program"))]
    #[codama(account(name = "attester_individual_authority", optional))]
    AttestAuthority = 4,

    #[codama(account(name = "root", signer))]
    #[codama(account(name = "authority"))]
    #[codama(account(name = "candidate"))]
    #[codama(account(name = "individual_authority", writable))]
    #[codama(account(name = "recipient", writable))]
    RemoveAuthority = 5,
}
