use bytemuck::from_bytes_mut;
use pinocchio::{
    AccountView, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{Sysvar, clock::Clock},
};
use pinocchio_token::{instructions::TransferChecked, state::Account};

use crate::{
    amm::state::{MARKET_DISCRIMINATOR, Market, MarketStatus},
    config::state::Config,
    constants::{CREATOR_CLAIM_SECS, MARKET_SEED, QUOTE_DECIMALS},
    error::MetisError,
    validation,
};

/// Accounts:
///   0 market       (writable)
///   1 vault        (writable)
///   2 treasury_ata (writable - Config.treasury's USDC account)
///   3 quote_mint
///   4 config
///   5 first_root   (signer)
///   6 second_root  (signer, a different root)
///   7 token_program
///
/// A creator who hasn't collected their fees CREATOR_CLAIM_SECS after settlement loses them to the
/// treasury. This is the one place a trust root moves money that isn't theirs, so it needs two
/// distinct roots, and it leaves a record on the market itself (`forfeited_creator_fees`,
/// `forfeited_at`), so a late claimant can be made whole by hand if it comes to
/// that. Nothing automatic ever forfeits.
pub fn process_forfeit_creator_fees(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    if !data.is_empty() {
        return Err(ProgramError::InvalidInstructionData);
    }
    let [market, vault, treasury_ata, quote_mint, config, first_root, second_root, token_program] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    validation::require_writable(market, MetisError::AccountNotWritable)?;
    validation::require_writable(vault, MetisError::AccountNotWritable)?;
    validation::require_writable(treasury_ata, MetisError::AccountNotWritable)?;
    validation::require_initialized_and_owned(market, &crate::ID, true, MetisError::InvalidMarketAccount)?;
    validation::require_config_account(config, false)?;
    validation::require_not_paused(config)?;
    if token_program.address() != &pinocchio_token::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    let (root, treasury) = {
        let d = config.try_borrow()?;
        let c = validation::unpack::<Config>(&d, MetisError::InvalidConfigAccount)?;
        (c.root, c.treasury)
    };
    validation::require_two_root_signers(&root, first_root, second_root)?;

    let now = Clock::get()?.unix_timestamp;
    let (amount, market_bump, creator, nonce) = {
        let mut market_data = market.try_borrow_mut()?;
        let m: &mut Market = from_bytes_mut(&mut market_data);
        validation::require_discriminator(m.discriminator, MARKET_DISCRIMINATOR)?;
        if m.status != MarketStatus::Settled as u8 {
            return Err(MetisError::MarketNotSettled.into());
        }
        validation::require_addresses_match(vault.address(), &m.vault, MetisError::WrongAccountReference)?;
        validation::require_addresses_match(quote_mint.address(), &m.quote_mint, MetisError::WrongAccountReference)?;
        if now < m.resolved_at.saturating_add(CREATOR_CLAIM_SECS) {
            return Err(MetisError::ForfeitTooEarly.into());
        }
        let amount = m.creator_owed;
        if amount == 0 {
            return Err(MetisError::NothingToForfeit.into());
        }
        // The record is written before the transfer; a failed transfer reverts it too.
        m.creator_owed = 0;
        m.forfeited_creator_fees = amount;
        m.forfeited_at = now;
        (amount, m.bump, m.creator, m.nonce)
    };

    {
        let ata = Account::from_account_view(treasury_ata)?;
        validation::require_addresses_match(ata.mint(), quote_mint.address(), MetisError::WrongAccountReference)?;
        validation::require_addresses_match(ata.owner(), &treasury, MetisError::WrongAccountReference)?;
    }

    let nonce_bytes = nonce.to_le_bytes();
    let bump_seed = [market_bump];
    let market_seeds = [
        Seed::from(MARKET_SEED),
        Seed::from(creator.as_ref()),
        Seed::from(nonce_bytes.as_ref()),
        Seed::from(bump_seed.as_ref()),
    ];
    TransferChecked::new(vault, quote_mint, treasury_ata, market, amount, QUOTE_DECIMALS)
        .invoke_signed(&[Signer::from(&market_seeds)])
}
