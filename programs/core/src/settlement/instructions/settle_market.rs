//  TODO: Oracle work will be added here to make sure this is complete settlement

use bytemuck::{Pod, Zeroable, from_bytes, from_bytes_mut};
use pinocchio::{
    AccountView, ProgramResult,
    error::ProgramError,
    sysvars::{Sysvar, clock::Clock},
};
use pinocchio_token::state::Account;

use crate::{
    amm::state::{MARKET_DISCRIMINATOR, Market, MarketStatus},
    error::MetisError,
    validation,
};

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

/// Accounts:
///   0 market    (writable)
///   1 vault     (the market's USDC account, read only here)
///   2 authority (signer)
///   3 oracle
///
/// Records which outcome actually happened. No pricing runs here - the curve
/// is finished by this point and never looked at again. All this does is write
/// down the winner so claims can start.
pub fn process_settle_market(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let args = SettleMarketArgs::load(data)?;

    let [market, vault, authority] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    validation::require_signer(authority, MetisError::AuthorityMustSign)?;
    validation::require_writable(market, MetisError::AccountNotWritable)?;
    validation::require_initialized_and_owned(
        market,
        &crate::ID,
        false,
        MetisError::InvalidMarketAccount,
    )?;

    let mut market_data = market.try_borrow_mut()?;
    let market_state: &mut Market = from_bytes_mut(&mut market_data);

    validation::require_discriminator(market_state.discriminator, MARKET_DISCRIMINATOR)?;

    // Only the market's own recorded authority can settle it.
    // TODO(single-point-of-failure): this single-signer check is the part
    // planned to be replaced by an oracle system
    // for now use authority
    validation::require_addresses_match(
        authority.address(),
        &market_state.authority,
        MetisError::WrongAccountReference,
    )?;

    // Covers Uninitialized and already-Settled in one check.
    let current_status = MarketStatus::try_from(market_state.status)?;
    if !current_status.can_transition_to(MarketStatus::Settled) {
        return Err(MetisError::InvalidStatusTransition.into());
    }
    if args.winning_outcome >= market_state.num_outcomes {
        return Err(MetisError::InvalidOutcome.into());
    }

    // The event must have started before anyone can say who won. bet_close_ts
    // is kickoff, and settling is only honest once there's a result to report.
    let now = Clock::get()?.unix_timestamp;
    if now < market_state.bet_close_ts {
        return Err(MetisError::MarketClosed.into());
    }

    // Solvency: the vault must still hold everything that was ever paid in,
    // otherwise the payouts we're about to allow can't all be honoured.
    //
    // A ">=" not an "==". Anyone can send tokens to any account, so an
    // unexpected extra dollar landing in the vault must not brick the market
    // forever - there'd be no way to recover, since money only leaves through
    // claim, and claim needs the market Settled first.
    //
    // Note there's no rent term here. The vault is an SPL token account: its
    // rent is paid in SOL and sits outside the token balance, so the amount we
    // read is pure USDC.
    {
        let vault_state = Account::from_account_view(vault)?;
        validation::require_addresses_match(
            vault.address(),
            &market_state.vault,
            MetisError::WrongAccountReference,
        )?;
        validation::require_addresses_match(
            vault_state.mint(),
            &market_state.quote_mint,
            MetisError::WrongAccountReference,
        )?;

        if vault_state.amount() < market_state.total_collected {
            return Err(MetisError::VaultBalanceMismatch.into());
        }
    }

    market_state.winning_outcome = args.winning_outcome;
    market_state.status = MarketStatus::Settled as u8;

    Ok(())
}
