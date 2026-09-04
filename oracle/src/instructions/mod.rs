use pinocchio::error::ProgramError;

pub mod create_oracle;
pub use create_oracle::*;

pub mod update_oracle;
pub use update_oracle::*;

pub mod football;

pub mod authority;
pub use authority::*;

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
