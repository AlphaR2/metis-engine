use bytemuck::from_bytes;
use pinocchio::{
    AccountView, Address, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{Sysvar, clock::Clock},
};
use pinocchio_token::{
    instructions::{CloseAccount, TransferChecked},
    state::Account,
};

use crate::{
    amm::state::{MARKET_DISCRIMINATOR, Market, MarketStatus},
    config::state::Config,
    constants::{CLAIM_WINDOW_SECS, MARKET_SEED, METADATA_SEED, QUOTE_DECIMALS},
    error::MetisError,
    validation,
};

/// Accounts:
///   0 market        (writable)
///   1 vault         (writable)
///   2 treasury_ata  (writable - the treasury's USDC account; receives what's left)
///   3 quote_mint
///   4 metadata      (writable, the market's metadata PDA - may not exist)
///   5 recipient     (signer, writable - receives the reclaimed rent; whoever cranks this)
///   6 config
///   7 token_program
///   8 operator_authority (optional - the recipient's attestation, needed unless it is a root)
///
/// The end of a market's life, CLAIM_WINDOW_SECS after it was settled or cancelled. Whatever is
/// left in the vault (rounding dust, winnings nobody claimed in the window, and any of the
/// protocol's fees nobody swept earlier) goes to the treasury, then the vault, metadata and
/// market accounts are closed and their rent goes to the caller - the incentive for someone to run
/// it. The outcome mints stay: they belong to the token program, and anything still holding them
/// is simply worthless by then.
///
/// The protocol's share needs no separate sweep: it is part of what is left, so it reaches the
/// treasury here at the latest. The creator's share is different. It is theirs, so it stays in the
/// vault until they claim it, and this refuses while any is owed - closing must never forfeit it.
pub fn process_close_market(accounts: &mut [AccountView], _data: &[u8]) -> ProgramResult {
    let [market, vault, treasury_ata, quote_mint, metadata, recipient, config, token_program, rest @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if rest.len() > 1 {
        return Err(ProgramError::InvalidInstructionData);
    }
    // Gated: the recipient (who also signs, and keeps the reclaimed rent) must be a root or an
    // attested operator. Nobody's money depends on a market being closed.
    validation::require_operator(recipient, rest.first(), config)?;
    validation::require_writable(recipient, MetisError::AccountNotWritable)?;
    validation::require_writable(market, MetisError::AccountNotWritable)?;
    validation::require_writable(vault, MetisError::AccountNotWritable)?;
    validation::require_writable(treasury_ata, MetisError::AccountNotWritable)?;
    validation::require_initialized_and_owned(market, &crate::ID, true, MetisError::InvalidMarketAccount)?;
    validation::require_config_account(config, false)?;
    validation::require_not_paused(config)?;
    if token_program.address() != &pinocchio_token::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    let treasury = {
        let data = config.try_borrow()?;
        validation::unpack::<Config>(&data, MetisError::InvalidConfigAccount)?.treasury
    };

    let (market_bump, creator, nonce) = {
        let market_data = market.try_borrow()?;
        let m: &Market = from_bytes(&market_data);
        validation::require_discriminator(m.discriminator, MARKET_DISCRIMINATOR)?;
        let settled = m.status == MarketStatus::Settled as u8;
        let cancelled = m.status == MarketStatus::Cancelled as u8;
        if !settled && !cancelled {
            return Err(MetisError::MarketNotSettled.into());
        }
        let opens_at = m.resolved_at.checked_add(CLAIM_WINDOW_SECS).ok_or(ProgramError::ArithmeticOverflow)?;
        if Clock::get()?.unix_timestamp < opens_at {
            return Err(MetisError::ClaimWindowOpen.into());
        }
        // Only the creator's fees block closing: the protocol's go to the treasury below anyway.
        if m.creator_owed != 0 {
            return Err(MetisError::FeesUnswept.into());
        }
        validation::require_addresses_match(vault.address(), &m.vault, MetisError::WrongAccountReference)?;
        validation::require_addresses_match(quote_mint.address(), &m.quote_mint, MetisError::WrongAccountReference)?;
        (m.bump, m.creator, m.nonce)
    };

    let remaining = {
        let ata = Account::from_account_view(treasury_ata)?;
        validation::require_addresses_match(ata.mint(), quote_mint.address(), MetisError::WrongAccountReference)?;
        validation::require_addresses_match(ata.owner(), &treasury, MetisError::WrongAccountReference)?;
        Account::from_account_view(vault)?.amount()
    };

    let nonce_bytes = nonce.to_le_bytes();
    let bump_seed = [market_bump];
    let seeds = [
        Seed::from(MARKET_SEED),
        Seed::from(creator.as_ref()),
        Seed::from(nonce_bytes.as_ref()),
        Seed::from(bump_seed.as_ref()),
    ];

    if remaining > 0 {
        TransferChecked::new(vault, quote_mint, treasury_ata, market, remaining, QUOTE_DECIMALS)
            .invoke_signed(&[Signer::from(&seeds)])?;
    }
    // The vault is empty now, so it can be closed; its rent goes to whoever ran this.
    CloseAccount::new(vault, recipient, market).invoke_signed(&[Signer::from(&seeds)])?;

    // Metadata, if the creator ever wrote it: only the real PDA for this market.
    let (metadata_pda, _) = Address::derive_program_address(&[METADATA_SEED, market.address().as_ref()], &crate::ID)
        .ok_or(ProgramError::InvalidSeeds)?;
    if !metadata.is_data_empty() {
        validation::require_addresses_match(metadata.address(), &metadata_pda, MetisError::InvalidPda)?;
        validation::require_owned_by(metadata, &crate::ID, MetisError::InvalidPda)?;
        recipient.set_lamports(recipient.lamports().checked_add(metadata.lamports()).ok_or(ProgramError::ArithmeticOverflow)?);
        metadata.set_lamports(0);
        metadata.close()?;
    }

    recipient.set_lamports(recipient.lamports().checked_add(market.lamports()).ok_or(ProgramError::ArithmeticOverflow)?);
    market.set_lamports(0);
    market.close()?;
    Ok(())
}
