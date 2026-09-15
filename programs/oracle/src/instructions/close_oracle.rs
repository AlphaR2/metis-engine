use bytemuck::{CheckedBitPattern, NoUninit};
use pinocchio::{
    AccountView, Address, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{Sysvar, rent::Rent},
};
use pinocchio_system::instructions::CreateAccount;
use shank::ShankType;

use crate::{
    constants::{AUTHORITY_SEED, CLOSER_QUORUM_SEED, GUARDIAN},
    error::OracleError,
    state::{CloserQuorum, RootAuthority},
};

#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Clone, Copy, ShankType)]
pub struct CloseArgs {
    pub fixture_merkle_root: [u8; 32],
}

const FIXTURE_MERKLE_ROOT_OFFSET: usize = 1;

impl CloseArgs {
    pub const LEN: usize = core::mem::size_of::<Self>();

    fn load(data: &[u8]) -> Result<&Self, ProgramError> {
        if data.len() != Self::LEN {
            return Err(ProgramError::InvalidInstructionData);
        }
        bytemuck::checked::try_from_bytes(data).map_err(|_| ProgramError::InvalidInstructionData)
    }
}

/// Creates a permissionless close proposal, then records two independent
/// supermajority votes. The second vote atomically closes both program-owned
/// accounts and refunds all rent to the original proposal initiator.
///
/// Accounts:
///   0 initiator       (writable; signer only while creating the proposal)
///   1 voter           (signer only while voting; ignored while creating)
///   2 oracle          (writable Oracle account)
///   3 closer_quorum   (writable PDA: [closer_quorum, oracle])
///   4 authority       (RootAuthority PDA)
///   5 system_program
pub fn process_close_oracle(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let args = CloseArgs::load(data)?;

    let [
        initiator,
        voter,
        oracle,
        closer_quorum,
        authority,
        system_program,
    ] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !initiator.is_writable() || !oracle.is_writable() || !closer_quorum.is_writable() {
        return Err(ProgramError::InvalidArgument);
    }
    if system_program.address() != &pinocchio_system::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    // check the oracle account to close
    if !oracle.owned_by(&crate::ID) {
        return Err(OracleError::InvalidOraclePda.into());
    }

    {
        let oracle_data = oracle.try_borrow()?;
        let fixture_merkle_root_end = FIXTURE_MERKLE_ROOT_OFFSET + args.fixture_merkle_root.len();
        if oracle_data.get(FIXTURE_MERKLE_ROOT_OFFSET..fixture_merkle_root_end)
            != Some(args.fixture_merkle_root.as_slice())
        {
            return Err(OracleError::InvalidOraclePda.into());
        }
    }

    let (expected_authority, _) =
        Address::derive_program_address(&[AUTHORITY_SEED, GUARDIAN.as_ref()], &crate::ID)
            .ok_or(OracleError::InvalidAuthorityPda)?;
    if expected_authority != *authority.address() || !authority.owned_by(&crate::ID) {
        return Err(OracleError::InvalidAuthorityPda.into());
    }

    let authority_state = *bytemuck::checked::try_from_bytes::<RootAuthority>(unsafe {
        authority.borrow_unchecked()
    })
    .map_err(|_| OracleError::InvalidAuthority)?;

    let quorum_seeds: [&[u8]; 2] = [CLOSER_QUORUM_SEED, oracle.address().as_ref()];

    let (expected_quorum, quorum_bump) = Address::derive_program_address(&quorum_seeds, &crate::ID)
        .ok_or(OracleError::InvalidOraclePda)?;

    if expected_quorum != *closer_quorum.address() {
        return Err(OracleError::InvalidOraclePda.into());
    }

    if closer_quorum.is_data_empty() {
        if !initiator.is_signer() || !closer_quorum.owned_by(&pinocchio_system::ID) {
            return Err(OracleError::UnauthorizedAuthority.into());
        }

        let quorum_bump_seed = [quorum_bump];
        let quorum_signer_seeds = [
            Seed::from(CLOSER_QUORUM_SEED),
            Seed::from(oracle.address().as_ref()),
            Seed::from(quorum_bump_seed.as_ref()),
        ];
        let rent = Rent::get()?;
        let rent_exempt = rent
            .try_minimum_balance(CloserQuorum::LEN)
            .map_err(|_| OracleError::RentCalculationFailed)?;

        CreateAccount {
            from: initiator,
            to: closer_quorum,
            lamports: rent_exempt,
            owner: &crate::ID,
            space: CloserQuorum::LEN as u64,
        }
        .invoke_signed(&[Signer::from(&quorum_signer_seeds)])?;

        let quorum_data = unsafe { closer_quorum.borrow_unchecked_mut() };
        quorum_data.copy_from_slice(bytemuck::bytes_of(&CloserQuorum::new(*initiator.address())));
        return Ok(());
    }

    if !closer_quorum.owned_by(&crate::ID) || !voter.is_signer() {
        return Err(OracleError::UnauthorizedAuthority.into());
    }

    let quorum_complete = {
        let quorum_state = bytemuck::checked::try_from_bytes_mut::<CloserQuorum>(unsafe {
            closer_quorum.borrow_unchecked_mut()
        })
        .map_err(|_| OracleError::InvalidAuthority)?;
        if quorum_state.initiator != initiator.address().to_bytes() {
            return Err(OracleError::UnauthorizedAuthority.into());
        }
        quorum_state.vote(*voter.address(), &authority_state)?;
        quorum_state.is_quorum_complete()
    };

    if !quorum_complete {
        return Ok(());
    }

    let close_lamports = oracle
        .lamports()
        .checked_add(closer_quorum.lamports())
        .ok_or(ProgramError::InvalidArgument)?;
    let recipient_lamports = initiator
        .lamports()
        .checked_add(close_lamports)
        .ok_or(ProgramError::InvalidArgument)?;

    initiator.set_lamports(recipient_lamports);
    oracle.set_lamports(0);
    closer_quorum.set_lamports(0);
    oracle.close()?;
    closer_quorum.close()?;

    Ok(())
}
