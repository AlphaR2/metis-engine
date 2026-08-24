use bytemuck::{from_bytes, from_bytes_mut, Pod, Zeroable};
use pinocchio::{
    error::ProgramError,
    sysvars::{rent::Rent, Sysvar},
    AccountView, ProgramResult,
};

use crate::discriminator::{MARKET_DISCRIMINATOR, VAULT_DISCRIMINATOR};
use crate::error::OpenStakeError;
use crate::state::{Market, MarketStatus, Vault};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct SettleMarketArgs {
    pub winning_outcome: u8,
    pub _padding: [u8; 7],
}

impl SettleMarketArgs {
    pub const LEN: usize = core::mem::size_of::<SettleMarketArgs>();

    pub fn load(bytes: &[u8]) -> Result<&Self, ProgramError> {
        if bytes.len() != Self::LEN {
            return Err(ProgramError::InvalidInstructionData);
        }
        Ok(from_bytes::<Self>(bytes))
    }
}

const _: () = assert!(core::mem::size_of::<SettleMarketArgs>() == 8);

// # Accounts
//
// | # | Account   | Writable | Signer |
// |---|-----------|----------|--------|
// | 0 | market    | yes      | no     |
// | 1 | vault     | no       | no     |
// | 2 | authority | no       | yes    |
pub fn process_settle_market(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let args = SettleMarketArgs::load(data)?;

    let [market, vault, authority] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !authority.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if !market.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }
    if market.is_data_empty() || vault.is_data_empty() {
        return Err(ProgramError::UninitializedAccount);
    }
    if !market.owned_by(&crate::ID) || !vault.owned_by(&crate::ID) {
        return Err(ProgramError::IncorrectProgramId);
    }

    // Cached before the mutable borrow below, since try_borrow_mut holds
    // &mut market for its whole lifetime — market.address() can't be
    // called again while market_data is alive.
    let market_address = *market.address();

    let mut market_data = market.try_borrow_mut()?;
    let market_state: &mut Market = from_bytes_mut(&mut market_data);

    if market_state.discriminator != MARKET_DISCRIMINATOR {
        return Err(OpenStakeError::DiscriminatorMismatch.into());
    }

    // Only the market's own recorded authority can settle it
    if &market_state.authority != authority.address() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    // Covers Uninitialized and already-Settled in one check
    if market_state.status != MarketStatus::Open as u8 {
        return Err(OpenStakeError::InvalidStatusTransition.into());
    }

    if args.winning_outcome as u8 >= market_state.num_outcomes {
        return Err(OpenStakeError::InvalidOutcome.into());
    }

    // Vault sanity check — this is the one moment it can actually hold:
    // right before settlement, no claim has paid anyone yet, so the
    // vault's real lamport balance must equal exactly rent_minimum +
    // total_collected. Checking this here, once, catches drift (a bug,
    // or funds moved outside this program) before it's ever locked in
    // and used to compute payouts.
    {
        let vault_data = vault.try_borrow()?;
        let vault_state: &Vault = from_bytes(&vault_data);
        if vault_state.discriminator != VAULT_DISCRIMINATOR {
            return Err(OpenStakeError::DiscriminatorMismatch.into());
        }
        if vault_state.market != market_address {
            return Err(ProgramError::InvalidAccountData);
        }

        let rent = Rent::get()?;
        let rent_minimum = rent.try_minimum_balance(core::mem::size_of::<Vault>())?;
        let expected = rent_minimum
            .checked_add(market_state.total_collected)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        if vault.lamports() != expected {
            return Err(OpenStakeError::VaultBalanceMismatch.into());
        }
    }

    market_state.winning_outcome = args.winning_outcome;
    market_state.status = MarketStatus::Settled as u8;

    Ok(())
}
