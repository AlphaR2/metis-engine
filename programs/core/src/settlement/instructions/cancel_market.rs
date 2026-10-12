use bytemuck::from_bytes_mut;
use pinocchio::{
    AccountView, ProgramResult,
    error::ProgramError,
    sysvars::{Sysvar, clock::Clock},
};

use crate::{
    amm::state::{MARKET_DISCRIMINATOR, Market, MarketStatus},
    config::state::Config,
    constants::{CANCEL_AFTER_SECS, OPERATOR_GRACE_SECS},
    error::MetisError,
    validation,
};

/// Accounts:
///   0 market      (writable)
///   1 oracle      (the market's oracle account - or its address, if it has been closed)
///   2 config
///   3 signer      (an operator or root; anyone, once the fallback opens; the first root when two
///                  roots are cancelling)
///   4 second_root (a different root's signature - only when two roots are cancelling)
///   5 operator_authority (optional - the signer's attestation, needed unless the signer is a root)
///
/// Everyone gets their money back, fees included, pro rata over every share (see claim_refund).
///
/// A market can be cancelled when the match can no longer produce a result for it:
///   - the oracle was voided by two roots; or
///   - there is still no final result CANCEL_AFTER_SECS after kickoff (or the oracle account
///     is gone), so funds are never stuck behind an oracle that stopped reporting.
/// An operator (or a root) does it as soon as that is true; anyone may once OPERATOR_GRACE_SECS
/// more have passed, so a vanished operator can't leave holders without a refund.
///
/// Otherwise two distinct root signers can cancel any unsettled market - an emergency tool that
/// can only ever return money to the people who paid it in, never choose a winner. It works
/// while the program is paused, because pausing must never trap anyone's money.
pub fn process_cancel_market(accounts: &mut [AccountView], _data: &[u8]) -> ProgramResult {
    let [market, oracle, config, first_root, second_root, rest @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if rest.len() > 1 {
        return Err(ProgramError::InvalidInstructionData);
    }

    validation::require_writable(market, MetisError::AccountNotWritable)?;
    validation::require_initialized_and_owned(market, &crate::ID, true, MetisError::InvalidMarketAccount)?;
    validation::require_config_account(config, false)?;

    // The oracle can be missing (closed after its grace period), but if it exists it must be real.
    let oracle_state = if oracle.is_data_empty() {
        None
    } else {
        Some(validation::read_oracle(oracle)?)
    };

    let mut market_data = market.try_borrow_mut()?;
    let m: &mut Market = from_bytes_mut(&mut market_data);
    validation::require_discriminator(m.discriminator, MARKET_DISCRIMINATOR)?;
    if !MarketStatus::try_from(m.status)?.can_transition_to(MarketStatus::Cancelled) {
        return Err(MetisError::InvalidStatusTransition.into());
    }
    validation::require_addresses_match(oracle.address(), &m.oracle, MetisError::OracleMismatch)?;

    let now = Clock::get()?.unix_timestamp;
    // When cancelling first became possible without a root override, if it has.
    let possible_since = match &oracle_state {
        Some(o) if o.is_voided() => Some(o.finished()),
        // A final result means settle, not cancel - only two roots can override that.
        Some(o) if o.is_finalized() => None,
        _ => {
            let deadline = m.bet_close_ts.saturating_add(CANCEL_AFTER_SECS);
            (now >= deadline).then_some(deadline)
        }
    };

    let roots = {
        let config_data = config.try_borrow()?;
        validation::unpack::<Config>(&config_data, MetisError::InvalidConfigAccount)?.root
    };
    let two_roots = validation::require_two_root_signers(&roots, first_root, second_root).is_ok();
    let open_to_anyone = possible_since.is_some_and(|since| now >= since.saturating_add(OPERATOR_GRACE_SECS));
    let operator = possible_since.is_some() && first_root.is_signer() && validation::is_operator(first_root, rest.first(), config)?;

    if !(two_roots || open_to_anyone || operator) {
        // Say why: a market that could be cancelled but whose signer isn't allowed is a different
        // problem from one that can't be cancelled at all.
        return Err(if possible_since.is_some() { MetisError::OperatorRequired } else { MetisError::CancelNotAllowed }.into());
    }

    // Everything in the vault goes back to holders - the pot and the fees.
    m.payout_pool = m
        .total_collected
        .checked_add(m.fees_accrued)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    m.protocol_owed = 0;
    m.creator_owed = 0;
    m.platform_owed = 0;
    m.backers_owed = 0;
    m.status = MarketStatus::Cancelled as u8;
    m.resolved_at = now;
    Ok(())
}
