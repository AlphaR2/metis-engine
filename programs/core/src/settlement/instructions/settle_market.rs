use bytemuck::from_bytes_mut;
use pinocchio::{
    AccountView, Address, ProgramResult,
    error::ProgramError,
    sysvars::{Sysvar, clock::Clock},
};
use pinocchio_token::state::Account;

use crate::{
    amm::{
        condition,
        quote::rake_of,
        state::{MARKET_DISCRIMINATOR, Market, MarketStatus, SeedSource},
    },
    constants::OPERATOR_GRACE_SECS,
    error::MetisError,
    settlement::payout::split_revenue,
    validation,
};

/// Accounts:
///   0 market    (writable)
///   1 vault     (the market's USDC account, read only here)
///   2 oracle    (the oracle account this market was created against)
///   3 config
///   4 signer    (an operator or root - or anyone, once OPERATOR_GRACE_SECS have passed)
///   5 operator_authority (optional - the signer's attestation, needed unless the signer is a root)
///
/// Run by an operator, with a fallback open to anyone after a week. There is no winner argument:
/// the outcome is worked out from what the oracle holds, and the oracle only holds a result once
/// two independent reporters agreed on it. The gate is not about who gets to pick a winner (nobody
/// does); it is about who runs a routine step, and the fallback means a vanished operator can
/// never strand winners.
///
/// What it requires of the oracle:
///   - it is the very account this market recorded at creation;
///   - it is final (the match finished and two reporters agreed), and not voided;
///   - this market's own dispute window has passed since it went final, so a two-root override
///     had its chance first.
///
/// Settlement only records numbers; it moves no money. The rake and the fees are swept by
/// sweep_fees afterwards, so a closed or hostile recipient account can never stand between a
/// winner and their claim.
pub fn process_settle_market(accounts: &mut [AccountView], _data: &[u8]) -> ProgramResult {
    let [market, vault, oracle, config, signer, rest @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if rest.len() > 1 {
        return Err(ProgramError::InvalidInstructionData);
    }

    validation::require_writable(market, MetisError::AccountNotWritable)?;
    validation::require_initialized_and_owned(market, &crate::ID, true, MetisError::InvalidMarketAccount)?;
    validation::require_config_account(config, false)?;
    validation::require_not_paused(config)?;

    let oracle_state = validation::read_oracle(oracle)?;

    let mut market_data = market.try_borrow_mut()?;
    let m: &mut Market = from_bytes_mut(&mut market_data);
    validation::require_discriminator(m.discriminator, MARKET_DISCRIMINATOR)?;

    // Covers Uninitialized, already-Settled and Cancelled in one check.
    if !MarketStatus::try_from(m.status)?.can_transition_to(MarketStatus::Settled) {
        return Err(MetisError::InvalidStatusTransition.into());
    }

    validation::require_addresses_match(oracle.address(), &m.oracle, MetisError::OracleMismatch)?;
    if oracle_state.is_voided() {
        return Err(MetisError::OracleVoided.into());
    }
    if !oracle_state.is_finalized() {
        return Err(MetisError::OracleNotFinal.into());
    }
    let now = Clock::get()?.unix_timestamp;
    let dispute_ends = oracle_state
        .finished()
        .checked_add(m.dispute_secs as i64)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    if now < dispute_ends {
        return Err(MetisError::DisputeWindowOpen.into());
    }
    // Settling has been possible since `dispute_ends`. For a week only an operator may do it; after
    // that, anyone may, so holders are never stuck behind an operator that isn't there.
    let open_to_anyone = now >= dispute_ends.saturating_add(OPERATOR_GRACE_SECS);
    if !open_to_anyone {
        validation::require_operator(signer, rest.first(), config)?;
    }

    // Judged on the 90-minute score, always - extra time and penalties can't move it.
    let (home, away) = oracle_state.regulation();
    let winning_outcome = condition::winning_outcome(m.condition_kind, m.condition_param, home, away)
        .ok_or(MetisError::InvalidCondition)?;

    // Solvency: the vault must hold the pot AND the fees, or the payouts and sweeps we're about
    // to allow can't all be honoured. ">=" not "==": anyone can send tokens to any account, and a
    // stray dollar must not brick a market that has no other way out.
    {
        let vault_state = Account::from_account_view(vault)?;
        validation::require_addresses_match(vault.address(), &m.vault, MetisError::WrongAccountReference)?;
        validation::require_addresses_match(vault_state.mint(), &m.quote_mint, MetisError::WrongAccountReference)?;
        let needed = m.total_collected.checked_add(m.fees_accrued).ok_or(ProgramError::ArithmeticOverflow)?;
        if vault_state.amount() < needed {
            return Err(MetisError::VaultBalanceMismatch.into());
        }
    }

    // The rake comes out of the pot; winners share what's left. The fees were never in the pot.
    // The rake and the fees together are the market's revenue, and it is shared four ways by the
    // shares this market snapshotted at creation: the protocol, the backers, the creator and the
    // platform it was created through. Settlement only records the amounts; sweep_fees pays them.
    let rake = rake_of(m.total_collected, m.rake_bps);
    let revenue = rake.checked_add(m.fees_accrued).ok_or(ProgramError::ArithmeticOverflow)?;
    let split = split_revenue(
        revenue,
        [m.fee_share_protocol_bps, m.fee_share_backers_bps, m.fee_share_creator_bps, m.fee_share_platform_bps],
        m.seed_source == SeedSource::Creator as u8,
        m.platform != Address::default(),
    );

    m.payout_pool = m.total_collected - rake;
    m.protocol_owed = split.protocol;
    m.creator_owed = split.creator;
    m.platform_owed = split.platform;
    m.backers_owed = split.backers;
    m.winning_outcome = winning_outcome;
    m.status = MarketStatus::Settled as u8;
    m.resolved_at = now;

    Ok(())
}
