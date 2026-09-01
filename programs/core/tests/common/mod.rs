// Shared litesvm test harness. This file is included via `mod common;` by
// each integration test binary in tests/ — each one is its own separate
// crate, so everything from the real program is reached through the
// external `openstake_core` crate name, not `crate::`.
//
// allow(dead_code): each test binary only uses a subset of these helpers,
// so unused-in-this-binary warnings are expected and not worth silencing
// per call site.
#![allow(dead_code)]

use bytemuck::bytes_of;
use litesvm::LiteSVM;
use solana_sdk::{
    account::Account,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer, read_keypair_file},
    transaction::Transaction,
};
use std::str::FromStr;

use openstake_core::{
    amm::instructions::{initialize_market::InitializeMarketArgs, place_bet::PlaceBetArgs},
    config::instructions::{
        initialize_config::InitializeConfigArgs, update_config::UpdateConfigArgs,
    },
    constants::{
        CONFIG_SEED, MARKET_INIT_FEE, MARKET_SEED, MAX_OUTCOMES, OUTCOME_MINT_SEED,
        PRIMARY_WINDOW_SECS, QUOTE_DECIMALS, VAULT_SEED,
    },
    settlement::instructions::settle_market::SettleMarketArgs,
};

pub const PROGRAM_ID_STR: &str = "2jt26iYKHghHDFVWQ8rnCdQc5B144Cos6HyJVw68qiv6";

// The three programs our instructions CPI into. All of them already exist and
// are executable in a fresh LiteSVM, so nothing needs loading by hand.
pub const TOKEN_PROGRAM_STR: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const ATA_PROGRAM_STR: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

// Stand-in for USDC. Any 6-decimal mint works; using the real mainnet address
// just makes test output recognisable.
pub const USDC_MINT_STR: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

pub fn program_id() -> Pubkey {
    Pubkey::from_str(PROGRAM_ID_STR).expect("valid program id")
}

pub fn token_program_id() -> Pubkey {
    Pubkey::from_str(TOKEN_PROGRAM_STR).expect("valid token program id")
}

pub fn ata_program_id() -> Pubkey {
    Pubkey::from_str(ATA_PROGRAM_STR).expect("valid ata program id")
}

pub fn usdc_mint() -> Pubkey {
    Pubkey::from_str(USDC_MINT_STR).expect("valid usdc mint")
}

// Loads the compiled program into a fresh litesvm instance. In a workspace,
// `cargo build-sbf` puts the .so at the WORKSPACE root's target/deploy/, but
// an integration test's cwd is its own package directory (programs/core) —
// so this walks up from CARGO_MANIFEST_DIR (programs/core) two levels to
// the workspace root rather than assuming a relative "target/deploy/...".
pub fn setup() -> LiteSVM {
    let so_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/deploy/openstake_core.so");
    let mut svm = LiteSVM::new();
    svm.add_program_from_file(program_id(), &so_path)
        .unwrap_or_else(|e| panic!("load {so_path:?} — did you run `just build-core`? ({e:?})"));
    create_usdc_mint(&mut svm);
    svm
}

// ── SPL account plumbing ─────────────────────────────────────────────────
//
// Rather than sending real transactions to the token program to set up mints
// and balances, we write the account bytes straight into the SVM. It's faster
// and keeps test setup out of the way of what's actually being tested.
//
// The layouts below are SPL Token's, which have been fixed for years. Writing
// them by hand avoids pulling in an spl-token dependency that would have to be
// kept in version lockstep with solana-sdk.

const MINT_LEN: usize = 82;
const TOKEN_ACCOUNT_LEN: usize = 165;

// SPL Mint: 82 bytes
//   0..4    mint_authority present? (u32, 1 = yes)
//   4..36   mint_authority
//   36..44  supply
//   44      decimals
//   45      is_initialized
//   46..50  freeze_authority present? (u32)
//   50..82  freeze_authority
fn pack_mint(mint_authority: Option<&Pubkey>, supply: u64, decimals: u8) -> Vec<u8> {
    let mut data = vec![0u8; MINT_LEN];
    if let Some(auth) = mint_authority {
        data[0..4].copy_from_slice(&1u32.to_le_bytes());
        data[4..36].copy_from_slice(auth.as_ref());
    }
    data[36..44].copy_from_slice(&supply.to_le_bytes());
    data[44] = decimals;
    data[45] = 1; // initialized
    data
}

// SPL token account: 165 bytes
//   0..32    mint
//   32..64   owner
//   64..72   amount
//   72..108  delegate (COption)
//   108      state (1 = initialized)
//   109..121 is_native (COption<u64>)
//   121..129 delegated_amount
//   129..165 close_authority (COption)
fn pack_token_account(mint: &Pubkey, owner: &Pubkey, amount: u64) -> Vec<u8> {
    let mut data = vec![0u8; TOKEN_ACCOUNT_LEN];
    data[0..32].copy_from_slice(mint.as_ref());
    data[32..64].copy_from_slice(owner.as_ref());
    data[64..72].copy_from_slice(&amount.to_le_bytes());
    data[108] = 1; // initialized
    data
}

// Creates the USDC mint. No mint authority, because tests hand out balances by
// writing account bytes directly rather than minting.
pub fn create_usdc_mint(svm: &mut LiteSVM) {
    let data = pack_mint(None, 0, QUOTE_DECIMALS);
    svm.set_account(
        usdc_mint(),
        Account {
            lamports: 1_000_000_000,
            data,
            owner: token_program_id(),
            executable: false,
            rent_epoch: 0,
        },
    )
    .expect("create usdc mint");
}

// The canonical associated token account address for (owner, mint).
pub fn ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), token_program_id().as_ref(), mint.as_ref()],
        &ata_program_id(),
    )
    .0
}

// Gives `owner` a USDC balance by writing their ATA directly. Returns the ATA
// address, which is what gets passed to initialize_market/place_bet/claim.
pub fn fund_usdc(svm: &mut LiteSVM, owner: &Pubkey, amount: u64) -> Pubkey {
    let account = ata(owner, &usdc_mint());
    let data = pack_token_account(&usdc_mint(), owner, amount);
    svm.set_account(
        account,
        Account {
            lamports: svm.minimum_balance_for_rent_exemption(TOKEN_ACCOUNT_LEN),
            data,
            owner: token_program_id(),
            executable: false,
            rent_epoch: 0,
        },
    )
    .expect("fund usdc");
    account
}

// Token balance of any token account. Returns 0 if the account doesn't exist,
// which is the right answer for "they hold none of this".
pub fn token_balance(svm: &LiteSVM, account: &Pubkey) -> u64 {
    match svm.get_account(account) {
        Some(a) if a.data.len() >= TOKEN_ACCOUNT_LEN => {
            u64::from_le_bytes(a.data[64..72].try_into().unwrap())
        }
        _ => 0,
    }
}

// Total supply of a mint, read straight out of its account bytes. Useful for
// asserting the AMM minted exactly what the curve said it should.
pub fn mint_supply(svm: &LiteSVM, mint: &Pubkey) -> u64 {
    match svm.get_account(mint) {
        Some(a) if a.data.len() >= MINT_LEN => {
            u64::from_le_bytes(a.data[36..44].try_into().unwrap())
        }
        _ => 0,
    }
}

// ── Keypairs and clock ───────────────────────────────────────────────────

// A keypair funded with SOL for transaction fees and account rent. Note this
// is SOL only — betting needs USDC, which comes from fund_usdc.
pub fn funded_keypair(svm: &mut LiteSVM, lamports: u64) -> Keypair {
    let kp = Keypair::new();
    svm.airdrop(&kp.pubkey(), lamports).expect("airdrop");
    kp
}

// A bettor ready to go: SOL for fees/rent plus a USDC balance. Returns the
// keypair and their USDC account.
pub fn funded_bettor(svm: &mut LiteSVM, usdc: u64) -> (Keypair, Pubkey) {
    let kp = funded_keypair(svm, 10 * solana_sdk::native_token::LAMPORTS_PER_SOL);
    let quote_ata = fund_usdc(svm, &kp.pubkey(), usdc);
    (kp, quote_ata)
}

// Every market instruction reads Config, so it has to exist first. Creates the
// singleton with limits switched off (0 = uncapped), which is what most tests
// want — the ones testing the limits set their own via update_config.
pub fn setup_with_config(svm: &mut LiteSVM) -> Pubkey {
    setup_with_config_limits(svm, 0, 0)
}

pub fn setup_with_config_limits(
    svm: &mut LiteSVM,
    slippage_limit_bps: u16,
    max_in_bps_of_vault: u16,
) -> Pubkey {
    let authority = authority_keypair();
    svm.airdrop(
        &authority.pubkey(),
        10 * solana_sdk::native_token::LAMPORTS_PER_SOL,
    )
    .expect("airdrop bootstrap admin");

    let (config, _) = config_pda(&authority.pubkey());
    send(
        svm,
        ix_initialize_config(
            &authority.pubkey(),
            &config,
            MARKET_INIT_FEE,
            0, // fee_bps — no fee mechanism wired up yet
            slippage_limit_bps,
            max_in_bps_of_vault,
            &authority.pubkey(),
            &usdc_mint(),
            MAX_OUTCOMES as u8,
        ),
        &authority,
        &[],
    )
    .expect("setup_with_config: initialize_config should succeed");

    config
}

// Moves litesvm's Clock forward past `ts`. Needed by any test that reaches
// settle_market, which requires now >= market.bet_close_ts.
pub fn advance_clock_past(svm: &mut LiteSVM, ts: i64) {
    let mut clock = svm.get_sysvar::<solana_sdk::clock::Clock>();
    clock.unix_timestamp = ts + 1;
    svm.set_sysvar(&clock);
}

pub fn current_clock_ts(svm: &LiteSVM) -> i64 {
    svm.get_sysvar::<solana_sdk::clock::Clock>().unix_timestamp
}

// A kickoff time that leaves the full primary window plus an hour of betting.
// initialize_market rejects anything closer than PRIMARY_WINDOW_SECS, so tests
// can't just use "now + 60" any more.
pub fn default_close_ts(svm: &LiteSVM) -> i64 {
    current_clock_ts(svm) + PRIMARY_WINDOW_SECS + 3600
}

// Betting stops PRIMARY_WINDOW_SECS before kickoff, so this is when the AMM
// actually shuts — the timestamp tests advance past to prove bets get rejected.
pub fn primary_end_of(bet_close_ts: i64) -> i64 {
    bet_close_ts - PRIMARY_WINDOW_SECS
}

// The settle_market authority — constants::AUTHORITY in the real program.
// Tests that need to actually settle a market (not just prove the wrong
// signer gets rejected) need the private key for that exact pubkey, which
// nobody but whoever generated it holds — not something to bake into the
// repo. Point this at wherever that keypair file actually lives; never
// commit the file itself.
pub fn authority_keypair() -> Keypair {
    let path = std::env::var("OPENSTAKE_AUTHORITY_KEYPAIR")
        .unwrap_or_else(|_| "keys/authority.json".to_string());
    read_keypair_file(&path).unwrap_or_else(|_| {
        panic!(
            "couldn't load the authority keypair from {path} \
             (set OPENSTAKE_AUTHORITY_KEYPAIR, or place it at keys/authority.json) — \
             this must be the keypair for {}, matching constants::AUTHORITY",
            openstake_core::constants::AUTHORITY,
        )
    })
}

// ── PDAs ─────────────────────────────────────────────────────────────────

pub fn market_pda(creator: &Pubkey, nonce: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[MARKET_SEED, creator.as_ref(), &nonce.to_le_bytes()],
        &program_id(),
    )
}

// The market's USDC account. An SPL token account owned by the token program,
// with the market PDA as its authority.
pub fn vault_pda(market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[VAULT_SEED, market.as_ref()], &program_id())
}

// One mint per outcome. This is what replaced the old Position account: a
// bettor's stake is just a balance of these tokens.
pub fn outcome_mint_pda(market: &Pubkey, index: u8) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[OUTCOME_MINT_SEED, market.as_ref(), &[index]],
        &program_id(),
    )
}

// Where a given person's shares of a given outcome live.
pub fn outcome_ata(market: &Pubkey, owner: &Pubkey, index: u8) -> Pubkey {
    let (mint, _) = outcome_mint_pda(market, index);
    ata(owner, &mint)
}

// Per-authority PDA (see initialize_config.rs's own note on this) — in
// practice always called with the bootstrap admin's own pubkey, since only
// that signer can ever successfully create one.
pub fn config_pda(authority: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[CONFIG_SEED, authority.as_ref()], &program_id())
}

// ── Instruction builders ─────────────────────────────────────────────────
//
// Discriminator bytes followed by the bytemuck-serialized args, same layout
// the on-chain dispatcher expects (see lib.rs's split_first_chunk::<8>()).
// Account order must match the doc comment on each process_* function.

// Far enough out that no test's clock will reach it — for tests that don't
// care about the deadline and just want betting open throughout.
pub const FAR_FUTURE_CLOSE: i64 = i64::MAX / 2;

// One parameter per real on-chain argument/account, on purpose - it mirrors
// the instruction's own account list 1:1 so a mismatch is easy to spot,
// rather than being hidden inside a builder struct.
#[allow(clippy::too_many_arguments)]
pub fn ix_initialize_market(
    creator: &Pubkey,
    creator_quote_ata: &Pubkey,
    market: &Pubkey,
    vault: &Pubkey,
    nonce: u64,
    num_outcomes: u8,
    bet_close_ts: i64,
    config: &Pubkey,
) -> Instruction {
    let args = InitializeMarketArgs {
        nonce,
        bet_close_ts,
        num_outcomes,
        _padding: [0u8; 7],
    };
    let mut data = openstake_core::amm::state::INITIALIZE_MARKET.to_vec();
    data.extend_from_slice(bytes_of(&args));

    let (mint_0, _) = outcome_mint_pda(market, 0);
    let (mint_1, _) = outcome_mint_pda(market, 1);

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new(*vault, false),
            AccountMeta::new(*creator, true),
            AccountMeta::new(*creator_quote_ata, false),
            AccountMeta::new_readonly(usdc_mint(), false),
            AccountMeta::new_readonly(*config, false),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            AccountMeta::new_readonly(token_program_id(), false),
            AccountMeta::new_readonly(ata_program_id(), false),
            // Two per outcome, sized for MAX_OUTCOMES == 2.
            AccountMeta::new(mint_0, false),
            AccountMeta::new(ata(creator, &mint_0), false),
            AccountMeta::new(mint_1, false),
            AccountMeta::new(ata(creator, &mint_1), false),
        ],
        data,
    }
}

pub fn ix_place_bet(
    market: &Pubkey,
    vault: &Pubkey,
    bettor: &Pubkey,
    bettor_quote_ata: &Pubkey,
    outcome_index: u8,
    deposit_amount: u64,
    config: &Pubkey,
) -> Instruction {
    let args = PlaceBetArgs {
        deposit_amount,
        outcome_index,
        _padding: [0u8; 7],
    };
    let mut data = openstake_core::amm::state::PLACE_BET.to_vec();
    data.extend_from_slice(bytes_of(&args));

    let (outcome_mint, _) = outcome_mint_pda(market, outcome_index);

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new(*vault, false),
            AccountMeta::new(*bettor, true),
            AccountMeta::new(*bettor_quote_ata, false),
            AccountMeta::new_readonly(usdc_mint(), false),
            AccountMeta::new(outcome_mint, false),
            AccountMeta::new(ata(bettor, &outcome_mint), false),
            AccountMeta::new_readonly(*config, false),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            AccountMeta::new_readonly(token_program_id(), false),
            AccountMeta::new_readonly(ata_program_id(), false),
        ],
        data,
    }
}

pub fn ix_settle_market(
    market: &Pubkey,
    vault: &Pubkey,
    authority: &Pubkey,
    winning_outcome: u8,
) -> Instruction {
    let args = SettleMarketArgs {
        winning_outcome,
        _padding: [0u8; 7],
    };
    let mut data = openstake_core::settlement::state::SETTLE_MARKET.to_vec();
    data.extend_from_slice(bytes_of(&args));

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new_readonly(*vault, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data,
    }
}

pub fn ix_claim(
    market: &Pubkey,
    vault: &Pubkey,
    bettor: &Pubkey,
    bettor_quote_ata: &Pubkey,
    winning_outcome: u8,
) -> Instruction {
    let data = openstake_core::settlement::state::CLAIM.to_vec();
    let (winning_mint, _) = outcome_mint_pda(market, winning_outcome);

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new_readonly(*market, false),
            AccountMeta::new(*vault, false),
            AccountMeta::new_readonly(*bettor, true),
            AccountMeta::new(*bettor_quote_ata, false),
            AccountMeta::new_readonly(usdc_mint(), false),
            AccountMeta::new(winning_mint, false),
            AccountMeta::new(ata(bettor, &winning_mint), false),
            AccountMeta::new_readonly(token_program_id(), false),
        ],
        data,
    }
}

#[allow(clippy::too_many_arguments)] // see ix_initialize_market's note above
pub fn ix_initialize_config(
    authority: &Pubkey,
    config: &Pubkey,
    market_init_fee: u64,
    fee_bps: u16,
    slippage_limit_bps: u16,
    max_in_bps_of_vault: u16,
    admin: &Pubkey,
    quote_mint: &Pubkey,
    max_outcomes: u8,
) -> Instruction {
    let args = InitializeConfigArgs {
        market_init_fee,
        fee_bps,
        slippage_limit_bps,
        max_in_bps_of_vault,
        admin: admin.to_bytes().into(),
        quote_mint: quote_mint.to_bytes().into(),
        max_outcomes,
        _padding: [0u8; 1],
    };
    let mut data = openstake_core::config::state::INITIALIZE_CONFIG.to_vec();
    data.extend_from_slice(bytes_of(&args));

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*authority, true),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            AccountMeta::new(*config, false),
        ],
        data,
    }
}

#[allow(clippy::too_many_arguments)] // see ix_initialize_market's note above
pub fn ix_update_config(
    config: &Pubkey,
    admin: &Pubkey,
    market_init_fee: u64,
    fee_bps: u16,
    slippage_limit_bps: u16,
    max_in_bps_of_vault: u16,
    new_admin: &Pubkey,
    quote_mint: &Pubkey,
    max_outcomes: u8,
) -> Instruction {
    let args = UpdateConfigArgs {
        market_init_fee,
        fee_bps,
        slippage_limit_bps,
        max_in_bps_of_vault,
        new_admin: new_admin.to_bytes().into(),
        quote_mint: quote_mint.to_bytes().into(),
        max_outcomes,
        _padding: [0u8; 1],
    };
    let mut data = openstake_core::config::state::UPDATE_CONFIG.to_vec();
    data.extend_from_slice(bytes_of(&args));

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*config, false),
            AccountMeta::new_readonly(*admin, true),
        ],
        data,
    }
}

// ── Sending ──────────────────────────────────────────────────────────────

// Builds and sends a transaction signed by `payer` + any extra signers,
// returns the litesvm result so callers can assert success or a specific
// error.
// litesvm's own error type is what's large here (it carries full transaction
// logs for debugging test failures) - boxing it would ripple through every
// call site's `.expect()`/`.unwrap_or_else()` for no real benefit in test code.
#[allow(clippy::result_large_err)]
pub fn send(
    svm: &mut LiteSVM,
    ix: Instruction,
    payer: &Keypair,
    extra_signers: &[&Keypair],
) -> Result<(), litesvm::types::FailedTransactionMetadata> {
    let mut signers: Vec<&Keypair> = vec![payer];
    signers.extend_from_slice(extra_signers);

    let blockhash = svm.latest_blockhash();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &signers, blockhash);

    svm.send_transaction(tx).map(|_| ())
}

// One-call market setup used by most tests: a creator with USDC, a market
// opened with the given kickoff time, and everything needed to bet on it.
pub struct TestMarket {
    pub creator: Keypair,
    pub creator_quote_ata: Pubkey,
    pub market: Pubkey,
    pub vault: Pubkey,
    pub config: Pubkey,
    pub bet_close_ts: i64,
}

pub fn open_market(svm: &mut LiteSVM, nonce: u64, bet_close_ts: i64) -> TestMarket {
    let config = setup_with_config(svm);
    open_market_with_config(svm, nonce, bet_close_ts, config)
}

// Same, but works out the kickoff time itself. Saves callers from writing
// `default_close_ts(&svm)` inside a call that already borrows svm mutably,
// which the borrow checker won't allow.
pub fn open_market_default(svm: &mut LiteSVM, nonce: u64) -> TestMarket {
    let bet_close_ts = default_close_ts(svm);
    open_market(svm, nonce, bet_close_ts)
}

pub fn open_market_with_config(
    svm: &mut LiteSVM,
    nonce: u64,
    bet_close_ts: i64,
    config: Pubkey,
) -> TestMarket {
    let (creator, creator_quote_ata) = funded_bettor(svm, 100 * MARKET_INIT_FEE);
    let (market, _) = market_pda(&creator.pubkey(), nonce);
    let (vault, _) = vault_pda(&market);

    send(
        svm,
        ix_initialize_market(
            &creator.pubkey(),
            &creator_quote_ata,
            &market,
            &vault,
            nonce,
            2,
            bet_close_ts,
            &config,
        ),
        &creator,
        &[],
    )
    .expect("open_market: initialize_market should succeed");

    TestMarket {
        creator,
        creator_quote_ata,
        market,
        vault,
        config,
        bet_close_ts,
    }
}
