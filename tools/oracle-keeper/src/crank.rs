//! The crank: sends the permissionless transactions that move a market through its life.
//!
//! A Solana program can't wake itself, so something has to send these. Every one of them is
//! permissionless - the program checks all the rules itself - which makes the crank an ordinary
//! program holding a key that only pays fees. It has no authority, and a leaked key can do nothing
//! beyond spend what it holds.
//!
//! Each tick it lists the market program's markets and, for each, works out what (if anything) is
//! due: `due` is a pure function of the market, its oracle and the clock, so every rule and every
//! boundary is a unit test. Stateless and safe to run twice: it re-reads the chain every time, and
//! the program refuses anything already done.
//!
//! The five steps, in the order a market meets them:
//!   close_primary  once betting ends, create the order books and fix the supply
//!   settle_market  once the oracle's result is final and the dispute window has passed
//!   cancel_market  when the oracle voided the result, or never produced one
//!   sweep_fees     pay the PROTOCOL its share after settlement (earlier revenue; close_market
//!                  would carry it at the latest)
//!   close_market   once the claim window has passed, reclaim the rent and sweep the dust
//!
//! What it deliberately does not do: pay the creator. The creator's fees are theirs - they stay in
//! the market's vault until the creator claims them, and the market stays open until they do. A
//! crank that pushed them out would be spending its own SOL to create token accounts for people who
//! may not want them, and choosing when they get paid.
//!
//! Cost, and the fee layer. The crank spends real SOL: the order books' rent in close_primary (about
//! 0.007 SOL a market), the treasury's token account the first time, and a transaction fee per step.
//! close_market gives the rent of everything it closes back, so a market's life nets out cheaper than
//! it looks. Recovering what remains is meant to come from a fee layer that still has to be designed
//! and reviewed; until then `Report::spent` records what each step really cost, so that review works
//! from measurements, not guesses.

use core::mem::{offset_of, size_of};

use metis_core::{
    amm::state::{CLOSE_PRIMARY, MARKET_DISCRIMINATOR, Market, MarketStatus},
    config::state::Config,
    constants::{
        BOOK_SEED, CANCEL_AFTER_SECS, CLAIM_WINDOW_SECS, CREATOR_CLAIM_SECS, DELEGATED_AUTHORITY_SEED, MANIFEST_PROGRAM_ID,
        METADATA_SEED, OUTCOME_MINT_SEED,
    },
    settlement::{
        instructions::sweep_fees::{SWEEP_PROTOCOL, SweepFeesArgs},
        state::{CANCEL_MARKET, CLOSE_MARKET, FORFEIT_CREATOR_FEES, SETTLE_MARKET, SWEEP_FEES},
    },
};
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use crate::{
    chain::{Chain, ChainError, decode_oracle},
    model::FootballSettled,
};

const SYSTEM_PROGRAM: Pubkey = Pubkey::from_str_const("11111111111111111111111111111111");
const TOKEN_PROGRAM: Pubkey = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const TOKEN_2022_PROGRAM: Pubkey = Pubkey::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
const ATA_PROGRAM: Pubkey = Pubkey::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    ClosePrimary,
    Settle,
    Cancel,
    SweepProtocol,
    CloseMarket,
}

/// What is due for one market right now. At most one *kind* of step per market per tick; the next
/// step is found on the next tick, after the chain has moved.
///
/// `oracle` is the market's oracle account: `None` if it no longer exists (closed after its grace
/// period), which counts as "no result".
pub fn due(market: &Market, oracle: Option<&FootballSettled>, now: i64) -> Vec<Action> {
    let Ok(status) = MarketStatus::try_from(market.status) else { return Vec::new() };

    match status {
        // A DBC-backed market waiting on its raise: activating it (or giving up on it) is the
        // Meteora keeper's job, not this crank's, so nothing is due here.
        MarketStatus::Pending => Vec::new(),
        MarketStatus::Open | MarketStatus::Closed => {
            // A final, undisputed result settles - and a settled market never needs books, so
            // settling comes before opening them.
            if let Some(o) = oracle {
                let ready = o.is_finalized()
                    && !o.is_voided()
                    && o.finished().checked_add(market.dispute_secs as i64).is_some_and(|end| now >= end);
                if ready {
                    return vec![Action::Settle];
                }
            }
            // Anyone may cancel when the result was voided, or none has come CANCEL_AFTER_SECS after
            // kickoff. A final result is never cancelled here: only two roots can overrule it.
            let voided = oracle.is_some_and(FootballSettled::is_voided);
            let finalized = oracle.is_some_and(|o| o.is_finalized() && !o.is_voided());
            let overdue = !finalized && now >= market.bet_close_ts.saturating_add(CANCEL_AFTER_SECS);
            if voided || overdue {
                return vec![Action::Cancel];
            }
            // Betting is over and the market is still live: open the order books.
            if status == MarketStatus::Open && now >= market.primary_end {
                return vec![Action::ClosePrimary];
            }
            Vec::new()
        }
        MarketStatus::Settled => {
            // Once the claim window is over, close: that sweeps the protocol's share along with the
            // dust, so there is no point sweeping it separately first. It waits only for the
            // creator, whose fees are theirs to claim - the crank never pays them.
            if closable(market, now) {
                return if market.creator_owed == 0 { vec![Action::CloseMarket] } else { Vec::new() };
            }
            // Before then, the protocol's share is swept early, so revenue doesn't wait 90 days.
            if market.protocol_owed > 0 { vec![Action::SweepProtocol] } else { Vec::new() }
        }
        MarketStatus::Cancelled => {
            if closable(market, now) { vec![Action::CloseMarket] } else { Vec::new() }
        }
        MarketStatus::Uninitialized => Vec::new(),
    }
}

fn closable(market: &Market, now: i64) -> bool {
    market.resolved_at.checked_add(CLAIM_WINDOW_SECS).is_some_and(|end| now >= end)
}

// ── Addresses ────────────────────────────────────────────────────────────

fn pda(seeds: &[&[u8]], program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(seeds, program).0
}

fn addr(a: &metis_core::amm::state::Market) -> (Pubkey, Pubkey, Pubkey) {
    (
        Pubkey::new_from_array(a.oracle.to_bytes()),
        Pubkey::new_from_array(a.vault.to_bytes()),
        Pubkey::new_from_array(a.quote_mint.to_bytes()),
    )
}

pub fn associated_token_account(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    pda(&[owner.as_ref(), TOKEN_PROGRAM.as_ref(), mint.as_ref()], &ATA_PROGRAM)
}

/// The attestation account of an operator: the program's own PDA for that key.
pub fn delegate_of(operator: &Pubkey, program: &Pubkey) -> Pubkey {
    pda(&[DELEGATED_AUTHORITY_SEED, operator.as_ref()], program)
}

/// Whether a market's creator fees have sat unclaimed long enough to be forfeitable. Read only:
/// the crank never forfeits (that takes two roots, see `forfeit_instruction`); this lets an
/// operator list what is waiting so the roots can decide.
pub fn forfeitable(market: &Market, now: i64) -> bool {
    market.status == MarketStatus::Settled as u8
        && market.creator_owed > 0
        && market.resolved_at.checked_add(CREATOR_CLAIM_SECS).is_some_and(|end| now >= end)
}

/// The instruction two roots sign to move a market's unclaimed creator fees to the treasury.
pub fn forfeit_instruction(
    deployment: &Deployment,
    market_address: &Pubkey,
    market: &Market,
    treasury: &Pubkey,
    first_root: &Pubkey,
    second_root: &Pubkey,
) -> Instruction {
    let (_, vault, quote_mint) = addr(market);
    let treasury_ata = associated_token_account(treasury, &quote_mint);
    market_data_instruction(
        &deployment.program,
        vec![
            meta(market_address, false, true),
            meta(&vault, false, true),
            meta(&treasury_ata, false, true),
            meta(&quote_mint, false, false),
            meta(&deployment.config, false, false),
            meta(first_root, true, false),
            meta(second_root, true, false),
            meta(&TOKEN_PROGRAM, false, false),
        ],
        FORFEIT_CREATOR_FEES.to_vec(),
    )
}

fn meta(key: &Pubkey, signer: bool, writable: bool) -> AccountMeta {
    AccountMeta { pubkey: *key, is_signer: signer, is_writable: writable }
}

/// Creates a token account if missing, harmlessly and idempotently, paid for by `payer`. Used so a
/// sweep never fails just because its recipient has no account yet.
fn create_ata(payer: &Pubkey, owner: &Pubkey, mint: &Pubkey) -> Instruction {
    Instruction {
        program_id: ATA_PROGRAM,
        accounts: vec![
            meta(payer, true, true),
            meta(&associated_token_account(owner, mint), false, true),
            meta(owner, false, false),
            meta(mint, false, false),
            meta(&SYSTEM_PROGRAM, false, false),
            meta(&TOKEN_PROGRAM, false, false),
        ],
        data: vec![1],
    }
}

/// Where everything lives: the deployed market program and the Config account it reads.
#[derive(Debug, Clone)]
pub struct Deployment {
    pub program: Pubkey,
    pub config: Pubkey,
}

impl Deployment {
    /// The deployment this build was compiled for. The Config address is derived from the trust
    /// roots compiled in, so build with `--features devnet` or `mainnet` to match - or pass it
    /// explicitly.
    pub fn for_this_build() -> Self {
        let program = Pubkey::new_from_array(metis_core::ID.to_bytes());
        Self { program, config: Self::default_config(&program) }
    }

    pub fn default_config(program: &Pubkey) -> Pubkey {
        pda(&[metis_core::constants::CONFIG_SEED, metis_core::constants::GUARDIAN.as_ref()], program)
    }
}

// ── Instructions ─────────────────────────────────────────────────────────

fn market_data_instruction(program: &Pubkey, accounts: Vec<AccountMeta>, data: Vec<u8>) -> Instruction {
    Instruction { program_id: *program, accounts, data }
}

/// The instructions (in order) that carry out one action. `payer` pays any rent they need.
pub fn instructions(
    action: Action,
    deployment: &Deployment,
    market_address: &Pubkey,
    market: &Market,
    treasury: &Pubkey,
    payer: &Pubkey,
) -> Vec<Instruction> {
    let program = &deployment.program;
    let (oracle, vault, quote_mint) = addr(market);
    let config = &deployment.config;
    // Every step is gated to operators: the payer signs, and its attestation rides along as the
    // last account (a root signer wouldn't need it; an attested operator does).
    let delegate = delegate_of(payer, program);

    match action {
        Action::Settle => vec![market_data_instruction(
            program,
            vec![
                meta(market_address, false, true),
                meta(&vault, false, false),
                meta(&oracle, false, false),
                meta(config, false, false),
                meta(payer, true, false),
                meta(&delegate, false, false),
            ],
            SETTLE_MARKET.to_vec(),
        )],
        Action::Cancel => {
            // The operator signs as the first signer; the second is only used when two roots cancel,
            // so it is an unsigned placeholder here.
            vec![market_data_instruction(
                program,
                vec![
                    meta(market_address, false, true),
                    meta(&oracle, false, false),
                    meta(config, false, false),
                    meta(payer, true, false),
                    meta(&SYSTEM_PROGRAM, false, false),
                    meta(&delegate, false, false),
                ],
                CANCEL_MARKET.to_vec(),
            )]
        }
        Action::ClosePrimary => {
            let mints = [0u8, 1].map(|i| pda(&[OUTCOME_MINT_SEED, market_address.as_ref(), &[i]], program));
            let books = [0u8, 1].map(|i| pda(&[BOOK_SEED, market_address.as_ref(), &[i]], program));
            let manifest = Pubkey::new_from_array(MANIFEST_PROGRAM_ID.to_bytes());
            // Manifest's own vault for a book and one of its mints.
            let vault_of = |book: &Pubkey, mint: &Pubkey| pda(&[b"vault", book.as_ref(), mint.as_ref()], &manifest);
            vec![market_data_instruction(
                program,
                vec![
                    meta(market_address, false, true),
                    meta(payer, true, true),
                    meta(&quote_mint, false, false),
                    meta(&mints[0], false, true),
                    meta(&mints[1], false, true),
                    meta(&books[0], false, true),
                    meta(&books[1], false, true),
                    meta(&vault_of(&books[0], &mints[0]), false, true),
                    meta(&vault_of(&books[0], &quote_mint), false, true),
                    meta(&vault_of(&books[1], &mints[1]), false, true),
                    meta(&vault_of(&books[1], &quote_mint), false, true),
                    meta(&SYSTEM_PROGRAM, false, false),
                    meta(&TOKEN_PROGRAM, false, false),
                    meta(&TOKEN_2022_PROGRAM, false, false),
                    meta(&manifest, false, false),
                    meta(config, false, false),
                    meta(&delegate, false, false),
                ],
                CLOSE_PRIMARY.to_vec(),
            )]
        }
        Action::SweepProtocol => {
            let recipient = associated_token_account(treasury, &quote_mint);
            let args = SweepFeesArgs { which: SWEEP_PROTOCOL, _padding: [0; 7] };
            vec![
                // The treasury might never have held USDC: make sure its account exists first.
                create_ata(payer, treasury, &quote_mint),
                market_data_instruction(
                    program,
                    vec![
                        meta(market_address, false, true),
                        meta(&vault, false, true),
                        meta(&recipient, false, true),
                        meta(&quote_mint, false, false),
                        meta(config, false, false),
                        meta(&TOKEN_PROGRAM, false, false),
                        meta(payer, true, false),
                        meta(&delegate, false, false),
                    ],
                    [SWEEP_FEES.as_slice(), bytemuck::bytes_of(&args)].concat(),
                ),
            ]
        }
        Action::CloseMarket => {
            let treasury_ata = associated_token_account(treasury, &quote_mint);
            let metadata = pda(&[METADATA_SEED, market_address.as_ref()], program);
            vec![
                create_ata(payer, treasury, &quote_mint),
                market_data_instruction(
                    program,
                    vec![
                        meta(market_address, false, true),
                        meta(&vault, false, true),
                        meta(&treasury_ata, false, true),
                        meta(&quote_mint, false, false),
                        meta(&metadata, false, true),
                        // The rent of everything closed comes back to whoever sent this.
                        meta(payer, true, true),
                        meta(config, false, false),
                        meta(&TOKEN_PROGRAM, false, false),
                        meta(&delegate, false, false),
                    ],
                    CLOSE_MARKET.to_vec(),
                ),
            ]
        }
    }
}

// ── Running it ───────────────────────────────────────────────────────────

pub fn decode_market(data: &[u8]) -> Option<Market> {
    if data.len() != size_of::<Market>() {
        return None;
    }
    let market: Market = bytemuck::pod_read_unaligned(data);
    (market.discriminator == MARKET_DISCRIMINATOR).then_some(market)
}

/// The treasury, out of the Config account.
pub fn treasury_of(data: &[u8]) -> Option<Pubkey> {
    read_treasury(data)
}

fn read_treasury(data: &[u8]) -> Option<Pubkey> {
    let config: Config = bytemuck::checked::try_pod_read_unaligned(data).ok()?;
    (config.discriminator == metis_core::config::state::CONFIG_DISCRIMINATOR).then(|| Pubkey::new_from_array(config.treasury.to_bytes()))
}

/// What one step of one market came to.
#[derive(Debug)]
pub struct Report {
    pub market: Pubkey,
    pub action: Action,
    /// The signature, or why it didn't go through.
    pub outcome: Result<String, String>,
    /// What the step cost the payer in lamports: fee plus any rent it fronted. Negative when the
    /// step returned more than it spent (closing a market gives its rent back). `None` if the
    /// balance couldn't be read. This is the measurement the fee-layer review works from.
    pub spent: Option<i64>,
}

pub struct Crank<C: Chain> {
    pub chain: C,
    /// Pays fees (and the rent a few steps need). Has no authority of any kind.
    pub payer: Keypair,
    pub deployment: Deployment,
}

impl<C: Chain> Crank<C> {
    pub fn new(chain: C, payer: Keypair, deployment: Deployment) -> Self {
        Self { chain, payer, deployment }
    }

    /// One pass over every market: does whatever is due, and reports what happened. A step that
    /// fails (a transient RPC error, a race with another crank) is reported and does not stop
    /// the others; the next pass finds it again.
    pub async fn run_once(&self) -> Result<Vec<Report>, ChainError> {
        let now = self.chain.now().await?;
        let rows = self
            .chain
            .program_accounts(&self.deployment.program, size_of::<Market>(), offset_of!(Market, discriminator), &MARKET_DISCRIMINATOR)
            .await?;
        let treasury = self
            .chain
            .account(&self.deployment.config)
            .await?
            .as_deref()
            .and_then(read_treasury)
            .ok_or_else(|| ChainError::Rpc(format!("{} is not a Metis config account", self.deployment.config)))?;

        let mut reports = Vec::new();
        for (address, data) in rows {
            let Some(market) = decode_market(&data) else { continue };

            // The market's oracle: gone entirely counts as "no result"; present but not a football
            // oracle means something is wrong, and that market is left alone.
            let oracle_address = Pubkey::new_from_array(market.oracle.to_bytes());
            let oracle = match self.chain.account(&oracle_address).await {
                Ok(None) => None,
                Ok(Some(bytes)) => match decode_oracle(&oracle_address, &bytes) {
                    Ok(o) => Some(o),
                    Err(_) => continue,
                },
                Err(_) => continue,
            };

            for action in due(&market, oracle.as_ref(), now) {
                let ixs = instructions(action, &self.deployment, &address, &market, &treasury, &self.payer.pubkey());
                let before = self.chain.balance(&self.payer.pubkey()).await.ok();
                let outcome = self.chain.send(&self.payer, &ixs).await.map_err(|e| e.to_string());
                let after = self.chain.balance(&self.payer.pubkey()).await.ok();
                let spent = before.zip(after).map(|(b, a)| b as i64 - a as i64);
                reports.push(Report { market: address, action, outcome, spent });
            }
        }
        Ok(reports)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Phase;
    use metis_core::amm::state::MarketStatus as S;

    const KICKOFF: i64 = 1_000_000;
    const PRIMARY_END: i64 = KICKOFF - 5 * 3600;
    const DISPUTE: u32 = 900;

    fn market(status: S) -> Market {
        Market {
            status: status as u8,
            bet_close_ts: KICKOFF,
            primary_end: PRIMARY_END,
            dispute_secs: DISPUTE,
            discriminator: MARKET_DISCRIMINATOR,
            ..Default::default()
        }
    }

    fn oracle() -> FootballSettled {
        FootballSettled::new([1; 32], KICKOFF, 0)
    }

    fn finalized(at: i64) -> FootballSettled {
        let mut o = oracle();
        o.status = Phase::Finished;
        o.finalized = 1;
        o.finished_at = at.to_le_bytes();
        o
    }

    fn voided() -> FootballSettled {
        let mut o = oracle();
        o.voided = 1;
        o
    }

    // ── close_primary ──

    #[test]
    fn the_books_are_opened_exactly_when_betting_ends() {
        let m = market(S::Open);
        let o = oracle();
        assert!(due(&m, Some(&o), PRIMARY_END - 1).is_empty());
        assert_eq!(due(&m, Some(&o), PRIMARY_END), vec![Action::ClosePrimary]);
        assert_eq!(due(&m, Some(&o), KICKOFF), vec![Action::ClosePrimary], "and until something else becomes due");
    }

    #[test]
    fn a_market_whose_books_are_open_is_not_closed_twice() {
        assert!(due(&market(S::Closed), Some(&oracle()), PRIMARY_END + 100).is_empty());
    }

    // ── settle ──

    #[test]
    fn a_final_result_settles_only_once_this_markets_dispute_window_has_passed() {
        let m = market(S::Open);
        let at = KICKOFF + 7_000;
        let o = finalized(at);
        assert_eq!(due(&m, Some(&o), at + DISPUTE as i64 - 1), vec![Action::ClosePrimary], "still inside the window: nothing to settle yet");
        assert_eq!(due(&m, Some(&o), at + DISPUTE as i64), vec![Action::Settle]);
        assert_eq!(due(&market(S::Closed), Some(&o), at + DISPUTE as i64), vec![Action::Settle], "settles from Closed too");
    }

    #[test]
    fn settling_takes_priority_over_opening_books_that_would_never_be_used() {
        let m = market(S::Open);
        let at = KICKOFF + 7_000;
        assert_eq!(due(&m, Some(&finalized(at)), at + 10_000), vec![Action::Settle], "not close_primary");
    }

    #[test]
    fn a_voided_oracle_never_settles() {
        let m = market(S::Open);
        assert!(!due(&m, Some(&voided()), KICKOFF + 10).contains(&Action::Settle));
    }

    // ── cancel ──

    #[test]
    fn a_voided_result_cancels_the_market_at_once() {
        assert_eq!(due(&market(S::Open), Some(&voided()), KICKOFF - 100), vec![Action::Cancel]);
        assert_eq!(due(&market(S::Closed), Some(&voided()), KICKOFF + 100), vec![Action::Cancel]);
    }

    #[test]
    fn no_result_by_the_deadline_cancels_and_a_missing_oracle_counts_as_no_result() {
        let m = market(S::Closed);
        let deadline = KICKOFF + CANCEL_AFTER_SECS;
        assert!(due(&m, Some(&oracle()), deadline - 1).is_empty());
        assert_eq!(due(&m, Some(&oracle()), deadline), vec![Action::Cancel]);
        assert_eq!(due(&m, None, deadline), vec![Action::Cancel], "the oracle account is gone");
        assert!(due(&m, None, deadline - 1).is_empty());
    }

    #[test]
    fn a_final_result_is_never_cancelled_by_the_crank_however_late() {
        // Only two roots can overrule a final result; the crank must not.
        let m = market(S::Closed);
        let at = KICKOFF + 7_000;
        let actions = due(&m, Some(&finalized(at)), KICKOFF + 10 * CANCEL_AFTER_SECS);
        assert_eq!(actions, vec![Action::Settle]);
        assert!(!actions.contains(&Action::Cancel));
    }

    // ── sweep and close ──

    #[test]
    fn the_protocol_is_swept_early_and_the_creator_is_never_swept_by_the_crank() {
        let mut m = market(S::Settled);
        m.resolved_at = KICKOFF + 8_000;
        m.protocol_owed = 5;
        m.creator_owed = 7;
        assert_eq!(due(&m, None, m.resolved_at + 1), vec![Action::SweepProtocol], "only the protocol's share, and not the creator's");
        m.protocol_owed = 0;
        assert!(due(&m, None, m.resolved_at + 1).is_empty(), "the creator's fees wait for the creator");
    }

    #[test]
    fn a_market_closes_after_the_claim_window_and_that_carries_the_protocols_share() {
        let mut m = market(S::Settled);
        m.resolved_at = KICKOFF + 8_000;
        let opens = m.resolved_at + CLAIM_WINDOW_SECS;
        assert!(due(&m, None, opens - 1).is_empty());
        assert_eq!(due(&m, None, opens), vec![Action::CloseMarket]);

        // Unswept protocol fees don't hold it up: closing sweeps them to the treasury.
        m.protocol_owed = 1;
        assert_eq!(due(&m, None, opens), vec![Action::CloseMarket], "no separate sweep first");
        assert_eq!(due(&m, None, opens - 1), vec![Action::SweepProtocol], "though before the window it is swept early");
    }

    #[test]
    fn the_creators_unclaimed_fees_hold_the_market_open_without_the_crank_ever_paying_them() {
        let mut m = market(S::Settled);
        m.resolved_at = KICKOFF + 8_000;
        m.creator_owed = 9;
        assert!(due(&m, None, m.resolved_at + 100 * CLAIM_WINDOW_SECS).is_empty(), "however long it waits");
        m.creator_owed = 0;
        assert_eq!(due(&m, None, m.resolved_at + CLAIM_WINDOW_SECS), vec![Action::CloseMarket], "once they claim");
    }

    #[test]
    fn a_cancelled_market_closes_after_the_claim_window() {
        let mut m = market(S::Cancelled);
        m.resolved_at = KICKOFF + CANCEL_AFTER_SECS;
        assert!(due(&m, None, m.resolved_at + CLAIM_WINDOW_SECS - 1).is_empty());
        assert_eq!(due(&m, None, m.resolved_at + CLAIM_WINDOW_SECS), vec![Action::CloseMarket]);
    }

    #[test]
    fn an_unrecognised_status_is_left_alone() {
        let mut m = market(S::Open);
        m.status = 99;
        assert!(due(&m, None, i64::MAX / 2).is_empty());
        assert!(due(&market(S::Uninitialized), None, i64::MAX / 2).is_empty());
    }

    #[test]
    fn nothing_overflows_at_extreme_times() {
        let mut m = market(S::Settled);
        m.resolved_at = i64::MAX;
        assert!(due(&m, None, i64::MAX).is_empty());
        m.status = S::Open as u8;
        m.bet_close_ts = i64::MAX;
        let _ = due(&m, Some(&finalized(i64::MAX)), i64::MAX);
    }

    // ── properties: whatever the market and the clock, the crank only ever does what is lawful ──

    use proptest::prelude::*;

    fn any_status() -> impl Strategy<Value = S> {
        prop_oneof![Just(S::Open), Just(S::Closed), Just(S::Settled), Just(S::Cancelled), Just(S::Uninitialized)]
    }

    proptest! {
        #[test]
        fn the_crank_only_does_lawful_things(
            status in any_status(),
            now in 0i64..4_000_000_000,
            resolved_at in 0i64..4_000_000_000,
            dispute in 900u32..86_400,
            creator_owed in 0u64..1_000,
            protocol_owed in 0u64..1_000,
            oracle_kind in 0u8..4,
            finished in 0i64..4_000_000_000,
        ) {
            let mut m = market(status);
            m.resolved_at = resolved_at;
            m.dispute_secs = dispute;
            m.creator_owed = creator_owed;
            m.protocol_owed = protocol_owed;
            let o = match oracle_kind {
                0 => None,
                1 => Some(oracle()),
                2 => Some(finalized(finished)),
                _ => Some(voided()),
            };
            let actions = due(&m, o.as_ref(), now);

            // At most one step per market per pass.
            prop_assert!(actions.len() <= 1);
            for a in actions {
                match a {
                    Action::ClosePrimary => {
                        prop_assert!(status == S::Open && now >= m.primary_end);
                    }
                    Action::Settle => {
                        let o = o.as_ref().expect("settling needs an oracle");
                        prop_assert!(matches!(status, S::Open | S::Closed));
                        prop_assert!(o.is_finalized() && !o.is_voided());
                        prop_assert!(now >= o.finished() + dispute as i64, "never inside the dispute window");
                    }
                    Action::Cancel => {
                        prop_assert!(matches!(status, S::Open | S::Closed));
                        // A final result is settled, never cancelled by the crank.
                        prop_assert!(!o.as_ref().is_some_and(|o| o.is_finalized() && !o.is_voided()));
                    }
                    Action::SweepProtocol => {
                        prop_assert!(status == S::Settled && protocol_owed > 0);
                    }
                    Action::CloseMarket => {
                        prop_assert!(matches!(status, S::Settled | S::Cancelled));
                        prop_assert!(now >= resolved_at + CLAIM_WINDOW_SECS, "never inside the claim window");
                        // The creator's fees are theirs to collect: closing waits for them.
                        prop_assert!(status != S::Settled || creator_owed == 0);
                    }
                }
            }
        }

        #[test]
        fn forfeitable_means_settled_unclaimed_and_old_enough(
            status in any_status(),
            now in 0i64..4_000_000_000,
            resolved_at in 0i64..4_000_000_000,
            creator_owed in 0u64..1_000,
        ) {
            let mut m = market(status);
            m.resolved_at = resolved_at;
            m.creator_owed = creator_owed;
            if forfeitable(&m, now) {
                prop_assert!(status == S::Settled && creator_owed > 0);
                prop_assert!(now >= resolved_at + metis_core::constants::CREATOR_CLAIM_SECS);
            }
        }
    }
}
