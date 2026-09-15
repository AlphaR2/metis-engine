//  TODO: Oracle work will be added here to make sure this is complete settlement

use bytemuck::{Pod, Zeroable, from_bytes, from_bytes_mut};
use pinocchio::{
    AccountView, Address, ProgramResult,
    error::ProgramError,
    sysvars::{Sysvar, clock::Clock},
};
use pinocchio_token::state::Account;

use crate::{
    amm::state::{MARKET_DISCRIMINATOR, Market, MarketStatus},
    config::state::{CONFIG_DISCRIMINATOR, Config},
    constants::{CONFIG_SEED, GUARDIAN},
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
///   2 authority (signer - Config.root, or this market's own recorded delegate)
///   3 config
///

///
/// winning_outcome is still a bare trusted argument here - reading it from the linked oracle
/// account (market_state.oracle) instead is deliberately deferred, real product/game logic
/// (how a condition maps to an outcome) not settled yet. What changed is only who's allowed to
/// call this: the old fixed constants::AUTHORITY single key is gone.
pub fn process_settle_market(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let args = SettleMarketArgs::load(data)?;

    let [market, vault, authority, config] = accounts else {
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

    let (config_pda, _) =
        Address::derive_program_address(&[CONFIG_SEED, GUARDIAN.as_ref()], &crate::ID)
            .ok_or(MetisError::InvalidPda)?;
    validation::require_addresses_match(config.address(), &config_pda, MetisError::InvalidPda)?;
    validation::require_initialized_and_owned(
        config,
        &crate::ID,
        false,
        MetisError::InvalidConfigAccount,
    )?;
    let config_data = config.try_borrow()?;
    let config_state =
        validation::unpack::<Config>(&config_data, MetisError::InvalidConfigAccount)?;
    validation::require_discriminator(config_state.discriminator, CONFIG_DISCRIMINATOR)?;

    let mut market_data = market.try_borrow_mut()?;
    let market_state: &mut Market = from_bytes_mut(&mut market_data);

    validation::require_discriminator(market_state.discriminator, MARKET_DISCRIMINATOR)?;

    // Root can always settle; otherwise the signer must be exactly this market's own recorded
    // delegate - not just any currently-attested key. Assignment already proved that address
    // was legitimate back at initialize_market, so no re-check against a DelegatedAuthority
    // account is needed here, just a direct match.
    if !config_state.root.is_root(*authority.address())
        && *authority.address() != market_state.delegate
    {
        return Err(MetisError::UnauthorizedAuthority.into());
    }

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
