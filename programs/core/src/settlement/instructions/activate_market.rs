use bytemuck::from_bytes_mut;
use pinocchio::{
    AccountView, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{Sysvar, clock::Clock},
};
use pinocchio_associated_token_account::instructions::CreateIdempotent;
use pinocchio_token::{
    instructions::MintTo,
    state::{Account, Mint},
};

use crate::{
    amm::{
        curve::symmetric_seed,
        state::{MARKET_DISCRIMINATOR, Market, MarketStatus, SeedSource},
    },
    constants::{MARKET_SEED, MAX_OUTCOMES},
    error::MetisError,
    settlement::backer::{ata_address, outcome_mint_address},
    validation,
};

/// Accounts:
///   0  market          (writable)
///   1  vault           (read: must already hold the seed)
///   2  operator        (signer, writable - pays the escrow accounts' rent)
///   3  config
///   4  backer_mint     (the DBC base mint the market recorded)
///   5  outcome_mint_0  (writable)
///   6  outcome_mint_1  (writable)
///   7  escrow_ata_0    (writable, the market's associated account for outcome 0 - created here)
///   8  escrow_ata_1    (writable)
///   9  system_program
///   10 token_program
///   11 ata_program
///   12 operator_authority (optional - the operator's attestation, needed unless it is a root)
///
/// Opens a DBC-backed market once its raise has funded the vault. Run by an operator: the raise
/// happens on Meteora, which this program does not read, so the operator's key is what says
/// "the raise graduated and its USDC is in the vault". What the program checks for itself:
///   - the market is Pending, before its raise deadline and before betting would close;
///   - the vault really holds the seed;
///   - the backer mint is the recorded one, has no mint or freeze authority left (so its supply
///     is fixed and nobody can mint a bigger claim), and has supply.
///
/// The seed shares are minted to escrow accounts owned by the market; backers redeem against them
/// with claim_backer. The market then behaves like any other, with the seed in its pot.
///
/// Anything the vault holds above the seed is not counted: it stays in the vault and reaches the
/// treasury when the market is closed.
pub fn process_activate_market(accounts: &mut [AccountView], _data: &[u8]) -> ProgramResult {
    let [
        market,
        vault,
        operator,
        config,
        backer_mint,
        outcome_mint_0,
        outcome_mint_1,
        escrow_ata_0,
        escrow_ata_1,
        system_program,
        token_program,
        ata_program,
        rest @ ..,
    ] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if rest.len() > 1 {
        return Err(ProgramError::InvalidInstructionData);
    }
    validation::require_operator(operator, rest.first(), config)?;
    validation::require_writable(operator, MetisError::AccountNotWritable)?;
    for writable in [&*market, &*outcome_mint_0, &*outcome_mint_1, &*escrow_ata_0, &*escrow_ata_1] {
        validation::require_writable(writable, MetisError::AccountNotWritable)?;
    }
    validation::require_initialized_and_owned(market, &crate::ID, true, MetisError::InvalidMarketAccount)?;
    validation::require_config_account(config, false)?;
    validation::require_not_paused(config)?;
    if system_program.address() != &pinocchio_system::ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    if token_program.address() != &pinocchio_token::ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    if ata_program.address() != &pinocchio_associated_token_account::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    let outcome_mints = [&*outcome_mint_0, &*outcome_mint_1];
    let escrows = [&*escrow_ata_0, &*escrow_ata_1];

    // The backer token must be final: fixed supply, nothing left that could mint or freeze it.
    let supply = {
        let mint = Mint::from_account_view(backer_mint)?;
        if mint.has_mint_authority() || mint.has_freeze_authority() || mint.supply() == 0 {
            return Err(MetisError::BackerMintInvalid.into());
        }
        mint.supply()
    };

    let market_addr = *market.address();
    let mut market_data = market.try_borrow_mut()?;
    let m: &mut Market = from_bytes_mut(&mut market_data);
    validation::require_discriminator(m.discriminator, MARKET_DISCRIMINATOR)?;
    if !MarketStatus::try_from(m.status)?.can_transition_to(MarketStatus::Open) || m.status != MarketStatus::Pending as u8 {
        return Err(MetisError::InvalidStatusTransition.into());
    }
    if m.seed_source != SeedSource::DbcBackers as u8 {
        return Err(MetisError::NotBackerMarket.into());
    }
    validation::require_addresses_match(backer_mint.address(), &m.backer_mint, MetisError::BackerMintInvalid)?;
    validation::require_addresses_match(vault.address(), &m.vault, MetisError::WrongAccountReference)?;

    let now = Clock::get()?.unix_timestamp;
    if now > m.raise_deadline || now >= m.primary_end {
        return Err(MetisError::RaiseDeadlinePassed.into());
    }
    if Account::from_account_view(vault)?.amount() < m.seed_target {
        return Err(MetisError::SeedNotFunded.into());
    }

    for i in 0..MAX_OUTCOMES {
        validation::require_addresses_match(
            outcome_mints[i].address(),
            &outcome_mint_address(&market_addr, i as u8)?,
            MetisError::InvalidPda,
        )?;
        validation::require_addresses_match(
            escrows[i].address(),
            &ata_address(&market_addr, outcome_mints[i].address())?,
            MetisError::InvalidPda,
        )?;
    }

    let seed_per_side = symmetric_seed(m.seed_target, m.num_outcomes);
    if seed_per_side == 0 {
        return Err(MetisError::PaymentTooSmall.into());
    }

    let nonce_bytes = m.nonce.to_le_bytes();
    let bump_seed = [m.bump];
    let creator = m.creator;
    let market_seeds = [
        Seed::from(MARKET_SEED),
        Seed::from(creator.as_ref()),
        Seed::from(nonce_bytes.as_ref()),
        Seed::from(bump_seed.as_ref()),
    ];

    // State first: the mints below only need the market PDA's signature, not its data, and a
    // failure anywhere reverts all of it.
    let num = m.num_outcomes as usize;
    for slot in m.outcome_shares.iter_mut().take(num) {
        *slot = seed_per_side;
    }
    m.total_collected = m.seed_target;
    m.backer_supply = supply;
    m.status = MarketStatus::Open as u8;
    let num_outcomes = m.num_outcomes as usize;
    drop(market_data);

    for i in 0..num_outcomes {
        CreateIdempotent {
            funding_account: operator,
            account: escrows[i],
            wallet: market,
            mint: outcome_mints[i],
            system_program,
            token_program,
        }
        .invoke()?;
        MintTo::new(outcome_mints[i], escrows[i], market, seed_per_side).invoke_signed(&[Signer::from(&market_seeds)])?;
    }
    Ok(())
}
