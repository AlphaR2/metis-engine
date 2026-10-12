// Shared litesvm test harness. This file is included via `mod common;` by
// each integration test binary in tests/ — each one is its own separate
// crate, so everything from the real program is reached through the
// external `metis_core` crate name, not `crate::`.
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
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::str::FromStr;

use metis_core::{
    amm::instructions::{
        initialize_market::InitializeMarketArgs, place_bet::PlaceBetArgs,
        set_metadata::SetMetadataArgs,
    },
    amm::metadata::{CATEGORY_MAX, LABEL_MAX, LEAGUE_MAX, QUESTION_MAX, RUBRIC_MAX},
    config::{
        instructions::{
            initialize_config::InitializeConfigArgs,
            update_config_critical::UpdateConfigCriticalArgs,
        },
        state::ConfigDials,
    },
    constants::{
        BOOK_SEED, CONFIG_SEED, MANIFEST_PROGRAM_ID, MARKET_SEED, MAX_OUTCOMES,
        MAX_PRICE_IMPACT_BPS, METADATA_SEED, MIN_DISPUTE_SECS, MIN_SEED, OUTCOME_MINT_SEED,
        PRIMARY_WINDOW_SECS, QUOTE_DECIMALS, ROOT_SIGNERS, VAULT_SEED, VIRTUAL_LIQUIDITY_MULTIPLE,
    },
    settlement::instructions::sweep_fees::SweepFeesArgs,
};
use oracle::instructions::football::{CFootballMatchPhase, FootballSettled};

pub const PROGRAM_ID_STR: &str = "79uea7X34YHRUQFkLAR5rwupHQf6Z7ASbBGfzxRDu8ws";

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
        .join("../../target/deploy/metis_core.so");
    // Signature verification disabled program-wide, same as oracle/'s test harness - needed so
    // root-authorized calls (initialize_config, update_config) can be sent "as" one of
    // ROOT_SIGNERS, whose vanity addresses have no known matching private key anyone holds.
    // Real, correctly-signed transactions (bettor/creator, using genuinely generated Keypairs)
    // still work exactly as before - this only skips rejecting missing/invalid signatures, it
    // doesn't affect valid ones.
    let mut svm = LiteSVM::new().with_sigverify(false);
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

// Every market instruction reads Config, so it has to exist first. Creates the singleton with
// default_dials(): no fee or rake yet (those tests set their own), the trade cap at its
// hardcoded ceiling.
pub fn setup_with_config(svm: &mut LiteSVM) -> Pubkey {
    setup_with_config_limits(svm, MAX_PRICE_IMPACT_BPS)
}

pub fn setup_with_config_limits(svm: &mut LiteSVM, max_in_bps_of_vault: u16) -> Pubkey {
    setup_with_config_fee(svm, MIN_SEED, max_in_bps_of_vault)
}

// Valid dials at their gentlest setting. Tests that care about one dial change just that field.
pub fn default_dials() -> ConfigDials {
    ConfigDials {
        market_init_fee: MIN_SEED,
        virtual_liquidity_multiple: VIRTUAL_LIQUIDITY_MULTIPLE,
        dispute_secs: MIN_DISPUTE_SECS,
        fee_bps: 0,
        rake_bps: 0,
        max_in_bps_of_vault: MAX_PRICE_IMPACT_BPS,
        fee_share_protocol_bps: 4_000,
        fee_share_backers_bps: 3_000,
        fee_share_creator_bps: 2_000,
        fee_share_platform_bps: 1_000,
        max_outcomes: MAX_OUTCOMES as u8,
        dbc_backing_enabled: 0,
        _padding: [0; 4],
    }
}

// Stand-in protocol treasury. Any non-zero address passes Config validation.
pub fn treasury() -> Pubkey {
    Pubkey::new_from_array([7; 32])
}

pub fn setup_with_config_fee(
    svm: &mut LiteSVM,
    market_init_fee: u64,
    max_in_bps_of_vault: u16,
) -> Pubkey {
    let dials = ConfigDials { market_init_fee, max_in_bps_of_vault, ..default_dials() };
    setup_with_dials(svm, dials)
}

pub fn setup_with_dials(svm: &mut LiteSVM, dials: ConfigDials) -> Pubkey {
    let payer = root_signer(0);
    svm.airdrop(&payer, 10 * solana_sdk::native_token::LAMPORTS_PER_SOL)
        .expect("airdrop bootstrap admin");

    let (config, _) = config_pda();
    send_as(
        svm,
        ix_initialize_config(&payer, &root_signer(1), &config, dials, &usdc_mint(), &treasury()),
        payer,
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

// One of Config.root's own ROOT_SIGNERS - a vanity address with no known matching private key,
// same situation as oracle/'s tests. No Keypair needed at all: send_as() builds an unsigned
// transaction naming this pubkey as signer, and setup()'s LiteSVM has sigverify disabled to
// accept it - mirrors oracle/'s test pattern of using Pubkey::new_from_array(ROOT_SIGNERS[n])
// directly rather than a real Keypair.
pub fn root_signer(index: usize) -> Pubkey {
    Pubkey::new_from_array(ROOT_SIGNERS[index].to_bytes())
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
// Config is a protocol-wide singleton, so its PDA is seeded from the fixed GUARDIAN constant,
// not from whichever root signer happens to call initialize_config - see the real program's
// config/instructions/initialize_config.rs for why.
pub fn config_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[CONFIG_SEED, metis_core::constants::GUARDIAN.as_ref()],
        &program_id(),
    )
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
    condition_kind: u8,
    condition_param: u8,
    config: &Pubkey,
    oracle: &Pubkey,
) -> Instruction {
    ix_initialize_market_on_platform(
        creator, creator_quote_ata, market, vault, nonce, num_outcomes, condition_kind, condition_param, config, oracle, [0u8; 32],
    )
}

/// The same, naming the platform the market is created through (all zeroes for none).
#[allow(clippy::too_many_arguments)]
pub fn ix_initialize_market_on_platform(
    creator: &Pubkey,
    creator_quote_ata: &Pubkey,
    market: &Pubkey,
    vault: &Pubkey,
    nonce: u64,
    num_outcomes: u8,
    condition_kind: u8,
    condition_param: u8,
    config: &Pubkey,
    oracle: &Pubkey,
    platform: [u8; 32],
) -> Instruction {
    let args = InitializeMarketArgs {
        nonce,
        num_outcomes,
        condition_kind,
        condition_param,
        _padding: [0u8; 5],
        platform,
    };
    let mut data = metis_core::amm::state::INITIALIZE_MARKET.to_vec();
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
            AccountMeta::new_readonly(*oracle, false),
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

#[allow(clippy::too_many_arguments)] // see ix_initialize_market's note above
pub fn ix_place_bet(
    market: &Pubkey,
    vault: &Pubkey,
    bettor: &Pubkey,
    bettor_quote_ata: &Pubkey,
    outcome_index: u8,
    deposit_amount: u64,
    min_shares_out: u64,
    config: &Pubkey,
) -> Instruction {
    let args = PlaceBetArgs {
        deposit_amount,
        min_shares_out,
        outcome_index,
        _padding: [0u8; 7],
    };
    let mut data = metis_core::amm::state::PLACE_BET.to_vec();
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

// Anyone can settle: no authority, no winner argument. The oracle decides.
pub fn ix_settle_market(market: &Pubkey, vault: &Pubkey, oracle: &Pubkey, config: &Pubkey, signer: &Pubkey) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new_readonly(*vault, false),
            AccountMeta::new_readonly(*oracle, false),
            AccountMeta::new_readonly(*config, false),
            AccountMeta::new_readonly(*signer, true),
            AccountMeta::new_readonly(delegate_pda(signer), false),
        ],
        data: metis_core::settlement::state::SETTLE_MARKET.to_vec(),
    }
}

pub fn ix_claim(
    market: &Pubkey,
    vault: &Pubkey,
    bettor: &Pubkey,
    bettor_quote_ata: &Pubkey,
    winning_outcome: u8,
) -> Instruction {
    let data = metis_core::settlement::state::CLAIM.to_vec();
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

// Needs two distinct root signers: `payer` (fee payer, pays rent) and `second_root`.
pub fn ix_initialize_config(
    payer: &Pubkey,
    second_root: &Pubkey,
    config: &Pubkey,
    dials: ConfigDials,
    quote_mint: &Pubkey,
    treasury: &Pubkey,
) -> Instruction {
    let args = InitializeConfigArgs {
        dials,
        quote_mint: quote_mint.to_bytes().into(),
        treasury: treasury.to_bytes().into(),
    };
    let mut data = metis_core::config::state::INITIALIZE_CONFIG.to_vec();
    data.extend_from_slice(bytes_of(&args));

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(*second_root, true),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            AccountMeta::new(*config, false),
        ],
        data,
    }
}

pub fn ix_update_config(config: &Pubkey, admin: &Pubkey, dials: ConfigDials) -> Instruction {
    let mut data = metis_core::config::state::UPDATE_CONFIG.to_vec();
    data.extend_from_slice(bytes_of(&dials));

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*config, false),
            AccountMeta::new_readonly(*admin, true),
        ],
        data,
    }
}

pub fn ix_update_config_critical(
    config: &Pubkey,
    first_root: &Pubkey,
    second_root: &Pubkey,
    quote_mint: &Pubkey,
    treasury: &Pubkey,
    dbc_config: &Pubkey,
) -> Instruction {
    let args = UpdateConfigCriticalArgs {
        quote_mint: quote_mint.to_bytes().into(),
        treasury: treasury.to_bytes().into(),
        dbc_config: dbc_config.to_bytes().into(),
    };
    let mut data = metis_core::config::state::UPDATE_CONFIG_CRITICAL.to_vec();
    data.extend_from_slice(bytes_of(&args));

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*config, false),
            AccountMeta::new_readonly(*first_root, true),
            AccountMeta::new_readonly(*second_root, true),
        ],
        data,
    }
}


pub fn ix_set_metadata(creator: &Pubkey, market: &Pubkey, text: &Text) -> Instruction {
    let args = text.to_args();
    let mut data = metis_core::amm::state::SET_METADATA.to_vec();
    data.extend_from_slice(bytes_of(&args));
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*creator, true),
            AccountMeta::new_readonly(*market, false),
            AccountMeta::new(metadata_pda(market).0, false),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
        ],
        data,
    }
}

/// `which`: 0 = the protocol's share to the treasury, 1 = the seed holder's share to the creator.
pub fn ix_sweep_fees(market: &Pubkey, vault: &Pubkey, recipient_ata: &Pubkey, config: &Pubkey, which: u8, signer: &Pubkey) -> Instruction {
    let args = SweepFeesArgs { which, _padding: [0; 7] };
    let mut data = metis_core::settlement::state::SWEEP_FEES.to_vec();
    data.extend_from_slice(bytes_of(&args));
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new(*vault, false),
            AccountMeta::new(*recipient_ata, false),
            AccountMeta::new_readonly(usdc_mint(), false),
            AccountMeta::new_readonly(*config, false),
            AccountMeta::new_readonly(token_program_id(), false),
            AccountMeta::new_readonly(*signer, true),
            AccountMeta::new_readonly(delegate_pda(signer), false),
        ],
        data,
    }
}

/// Roots are only needed (and only signed) when cancelling by root; pass the same placeholder
/// keys and `signed = false` otherwise.
pub fn ix_cancel_market(market: &Pubkey, oracle: &Pubkey, config: &Pubkey, signer: &Pubkey, roots: Option<(Pubkey, Pubkey)>) -> Instruction {
    // Cancelling by root: both roots sign. Otherwise the signer is the first signature and the
    // second is an unsigned placeholder.
    let (first, second) = roots.unwrap_or((*signer, Pubkey::new_unique()));
    let signed = roots.is_some();
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new_readonly(*oracle, false),
            AccountMeta::new_readonly(*config, false),
            AccountMeta::new_readonly(first, true),
            AccountMeta::new_readonly(second, signed),
            AccountMeta::new_readonly(delegate_pda(&first), false),
        ],
        data: metis_core::settlement::state::CANCEL_MARKET.to_vec(),
    }
}

pub fn ix_claim_refund(market: &Pubkey, vault: &Pubkey, holder: &Pubkey, holder_quote_ata: &Pubkey, outcome_index: u8) -> Instruction {
    let (mint, _) = outcome_mint_pda(market, outcome_index);
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new_readonly(*market, false),
            AccountMeta::new(*vault, false),
            AccountMeta::new_readonly(*holder, true),
            AccountMeta::new(*holder_quote_ata, false),
            AccountMeta::new_readonly(usdc_mint(), false),
            AccountMeta::new(mint, false),
            AccountMeta::new(ata(holder, &mint), false),
            AccountMeta::new_readonly(token_program_id(), false),
        ],
        data: metis_core::settlement::state::CLAIM_REFUND.to_vec(),
    }
}

pub fn ix_close_market(market: &Pubkey, vault: &Pubkey, treasury_ata: &Pubkey, recipient: &Pubkey, config: &Pubkey) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new(*vault, false),
            AccountMeta::new(*treasury_ata, false),
            AccountMeta::new_readonly(usdc_mint(), false),
            AccountMeta::new(metadata_pda(market).0, false),
            AccountMeta::new(*recipient, true),
            AccountMeta::new_readonly(*config, false),
            AccountMeta::new_readonly(token_program_id(), false),
            AccountMeta::new_readonly(delegate_pda(recipient), false),
        ],
        data: metis_core::settlement::state::CLOSE_MARKET.to_vec(),
    }
}

pub fn manifest_program_id() -> Pubkey {
    Pubkey::new_from_array(MANIFEST_PROGRAM_ID.to_bytes())
}

pub fn token_2022_program_id() -> Pubkey {
    Pubkey::from_str("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb").unwrap()
}

pub fn book_pda(market: &Pubkey, index: u8) -> Pubkey {
    Pubkey::find_program_address(&[BOOK_SEED, market.as_ref(), &[index]], &program_id()).0
}

pub fn metadata_pda(market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[METADATA_SEED, market.as_ref()], &program_id())
}

/// Manifest's own vault PDA for a book and one of its mints.
pub fn manifest_vault(book: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"vault", book.as_ref(), mint.as_ref()], &manifest_program_id()).0
}

pub fn ix_close_primary(payer: &Pubkey, market: &Pubkey, config: &Pubkey) -> Instruction {
    let mints = [outcome_mint_pda(market, 0).0, outcome_mint_pda(market, 1).0];
    let books = [book_pda(market, 0), book_pda(market, 1)];
    let q = usdc_mint();
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(q, false),
            AccountMeta::new(mints[0], false),
            AccountMeta::new(mints[1], false),
            AccountMeta::new(books[0], false),
            AccountMeta::new(books[1], false),
            AccountMeta::new(manifest_vault(&books[0], &mints[0]), false),
            AccountMeta::new(manifest_vault(&books[0], &q), false),
            AccountMeta::new(manifest_vault(&books[1], &mints[1]), false),
            AccountMeta::new(manifest_vault(&books[1], &q), false),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            AccountMeta::new_readonly(token_program_id(), false),
            AccountMeta::new_readonly(token_2022_program_id(), false),
            AccountMeta::new_readonly(manifest_program_id(), false),
            AccountMeta::new_readonly(*config, false),
            AccountMeta::new_readonly(delegate_pda(payer), false),
        ],
        data: metis_core::amm::state::CLOSE_PRIMARY.to_vec(),
    }
}

pub fn ix_pause(root: &Pubkey, config: &Pubkey) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![AccountMeta::new_readonly(*root, true), AccountMeta::new(*config, false)],
        data: metis_core::config::state::PAUSE.to_vec(),
    }
}

pub fn ix_unpause(signers: &[Pubkey], config: &Pubkey) -> Instruction {
    let mut accounts = vec![AccountMeta::new_readonly(signers[0], true), AccountMeta::new(*config, false)];
    accounts.extend(signers[1..].iter().map(|k| AccountMeta::new_readonly(*k, true)));
    Instruction { program_id: program_id(), accounts, data: metis_core::config::state::UNPAUSE.to_vec() }
}

pub fn guardian_signer() -> Pubkey {
    Pubkey::new_from_array(metis_core::constants::GUARDIAN.to_bytes())
}

/// Market text in plain strings; `to_args` packs it the way the instruction wants.
pub struct Text {
    pub question: &'static str,
    pub rubric_yes: &'static str,
    pub rubric_no: &'static str,
    pub label_yes: &'static str,
    pub label_no: &'static str,
    pub category: &'static str,
    pub league: &'static str,
}

impl Text {
    pub fn sample() -> Self {
        Text {
            question: "Will Team A beat Team B?",
            rubric_yes: "Resolves YES if Team A leads Team B after 90 minutes of play, stoppage time included.",
            rubric_no: "Resolves NO for a draw or a Team B win after 90 minutes. Extra time and penalties do not count.",
            label_yes: "Team A wins",
            label_no: "Draw or Team B",
            category: "Sports",
            league: "Example League",
        }
    }

    pub fn to_args(&self) -> SetMetadataArgs {
        fn put<const N: usize>(s: &str) -> ([u8; N], u16) {
            let mut buf = [0u8; N];
            let n = s.len().min(N);
            buf[..n].copy_from_slice(&s.as_bytes()[..n]);
            (buf, s.len() as u16)
        }
        let (question, a) = put::<QUESTION_MAX>(self.question);
        let (rubric_yes, b) = put::<RUBRIC_MAX>(self.rubric_yes);
        let (rubric_no, c) = put::<RUBRIC_MAX>(self.rubric_no);
        let (label_yes, d) = put::<LABEL_MAX>(self.label_yes);
        let (label_no, e) = put::<LABEL_MAX>(self.label_no);
        let (category, f) = put::<CATEGORY_MAX>(self.category);
        let (league, g) = put::<LEAGUE_MAX>(self.league);
        SetMetadataArgs { question, rubric_yes, rubric_no, label_yes, label_no, category, league, lens: [a, b, c, d, e, f, g] }
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

// For a root-authorized call (initialize_config, update_config) sent "as" one of ROOT_SIGNERS -
// no real Keypair exists for those vanity addresses, so this builds an unsigned transaction
// with `payer` as the fee payer, same pattern as oracle/'s test send_unsigned. Only works
// because setup()'s LiteSVM has sigverify disabled.
#[allow(clippy::result_large_err)] // see send()'s note above
pub fn send_as(
    svm: &mut LiteSVM,
    ix: Instruction,
    payer: Pubkey,
) -> Result<(), litesvm::types::FailedTransactionMetadata> {
    let blockhash = svm.latest_blockhash();
    let message = solana_sdk::message::Message::new_with_blockhash(&[ix], Some(&payer), &blockhash);
    let tx = Transaction::new_unsigned(message);
    svm.send_transaction(tx).map(|_| ())
}

#[allow(clippy::result_large_err)] // see send()'s note above
pub fn send_as_meta(
    svm: &mut LiteSVM,
    ix: Instruction,
    payer: Pubkey,
) -> Result<litesvm::types::TransactionMetadata, litesvm::types::FailedTransactionMetadata> {
    let blockhash = svm.latest_blockhash();
    let message = solana_sdk::message::Message::new_with_blockhash(&[ix], Some(&payer), &blockhash);
    svm.send_transaction(Transaction::new_unsigned(message))
}

// Same as send(), but keeps the transaction metadata instead of throwing it
// away - this is where compute_units_consumed and the real lamport fee live.
// Split out as its own function rather than changing send()'s return type,
// since almost every call site only cares whether the transaction succeeded.
#[allow(clippy::result_large_err)] // see send()'s note above
pub fn send_with_meta(
    svm: &mut LiteSVM,
    ix: Instruction,
    payer: &Keypair,
    extra_signers: &[&Keypair],
) -> Result<litesvm::types::TransactionMetadata, litesvm::types::FailedTransactionMetadata> {
    let mut signers: Vec<&Keypair> = vec![payer];
    signers.extend_from_slice(extra_signers);

    let blockhash = svm.latest_blockhash();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &signers, blockhash);

    svm.send_transaction(tx)
}

// One-call market setup used by most tests: a creator with USDC, a market on a freshly created
// oracle, and everything needed to bet on it.
pub struct TestMarket {
    pub creator: Keypair,
    pub creator_quote_ata: Pubkey,
    pub market: Pubkey,
    pub vault: Pubkey,
    pub config: Pubkey,
    pub oracle: Pubkey,
    /// Kickoff, which is also the market's `bet_close_ts` (it comes from the oracle).
    pub bet_close_ts: i64,
}

pub fn open_market(svm: &mut LiteSVM, nonce: u64, kickoff: i64) -> TestMarket {
    let config = setup_with_config(svm);
    open_market_with_config(svm, nonce, kickoff, config)
}

// Same, but works out the kickoff itself. Saves callers from writing `default_close_ts(&svm)`
// inside a call that already borrows svm mutably, which the borrow checker won't allow.
pub fn open_market_default(svm: &mut LiteSVM, nonce: u64) -> TestMarket {
    let kickoff = default_close_ts(svm);
    open_market(svm, nonce, kickoff)
}

// ── The oracle, as the program really reads it
//
// Core tests don't load the oracle program: they write the oracle's account bytes directly, in
// the real layout, owned by the real oracle program id. That is exactly what core reads.

pub fn oracle_program_id() -> Pubkey {
    Pubkey::new_from_array(oracle::ID.to_bytes())
}

/// A fresh, scheduled fixture with the given kickoff.
pub fn oracle_account(svm: &mut LiteSVM, kickoff: i64) -> Pubkey {
    let address = Pubkey::new_unique();
    let now = current_clock_ts(svm);
    let state = FootballSettled::new(address.to_bytes(), kickoff, now);
    svm.set_account(
        address,
        Account {
            lamports: 1_000_000,
            data: state.as_bytes().to_vec(),
            owner: oracle_program_id(),
            ..Default::default()
        },
    )
    .expect("oracle account is seeded");
    address
}

pub fn edit_oracle(svm: &mut LiteSVM, oracle: &Pubkey, edit: impl FnOnce(&mut FootballSettled)) {
    let mut account = svm.get_account(oracle).expect("oracle exists");
    let mut state = *bytemuck::checked::try_from_bytes::<FootballSettled>(&account.data).unwrap();
    edit(&mut state);
    account.data = state.as_bytes().to_vec();
    svm.set_account(*oracle, account).unwrap();
}

/// Makes the oracle's result final: a finished match with this 90-minute score, final at `at`.
pub fn finalize_oracle(svm: &mut LiteSVM, oracle: &Pubkey, home: u8, away: u8, at: i64) {
    edit_oracle(svm, oracle, |s| {
        let score = FootballSettled::pack_score(home, away);
        s.status = CFootballMatchPhase::Finished;
        s.score = score;
        s.regulation_score = score;
        s.finalized = 1;
        s.finished_at = at.to_le_bytes();
    });
}

pub fn void_oracle(svm: &mut LiteSVM, oracle: &Pubkey, at: i64) {
    edit_oracle(svm, oracle, |s| {
        s.voided = 1;
        s.finished_at = at.to_le_bytes();
    });
}

pub fn set_clock(svm: &mut LiteSVM, ts: i64) {
    let mut clock = svm.get_sysvar::<solana_sdk::clock::Clock>();
    clock.unix_timestamp = ts;
    svm.set_sysvar(&clock);
}

pub fn open_market_with_config(svm: &mut LiteSVM, nonce: u64, kickoff: i64, config: Pubkey) -> TestMarket {
    open_market_with_condition(svm, nonce, kickoff, config, metis_core::amm::condition::HOME_WIN, 0)
}

pub fn open_market_with_condition(
    svm: &mut LiteSVM,
    nonce: u64,
    kickoff: i64,
    config: Pubkey,
    condition_kind: u8,
    condition_param: u8,
) -> TestMarket {
    let (creator, creator_quote_ata) = funded_bettor(svm, 100 * MIN_SEED);
    let (market, _) = market_pda(&creator.pubkey(), nonce);
    let (vault, _) = vault_pda(&market);
    let oracle = oracle_account(svm, kickoff);

    send(
        svm,
        ix_initialize_market(
            &creator.pubkey(),
            &creator_quote_ata,
            &market,
            &vault,
            nonce,
            2,
            condition_kind,
            condition_param,
            &config,
            &oracle,
        ),
        &creator,
        &[],
    )
    .expect("open_market: initialize_market should succeed");

    TestMarket { creator, creator_quote_ata, market, vault, config, oracle, bet_close_ts: kickoff }
}

/// A football market created through `platform`, which earns the platform's share of its revenue.
pub fn open_market_on_platform(svm: &mut LiteSVM, nonce: u64, kickoff: i64, config: Pubkey, platform: &Pubkey) -> TestMarket {
    let (creator, creator_quote_ata) = funded_bettor(svm, 100 * MIN_SEED);
    let (market, _) = market_pda(&creator.pubkey(), nonce);
    let (vault, _) = vault_pda(&market);
    let oracle = oracle_account(svm, kickoff);
    send(
        svm,
        ix_initialize_market_on_platform(
            &creator.pubkey(), &creator_quote_ata, &market, &vault, nonce, 2,
            metis_core::amm::condition::HOME_WIN, 0, &config, &oracle, platform.to_bytes(),
        ),
        &creator,
        &[],
    )
    .expect("open_market_on_platform: initialize_market should succeed");
    TestMarket { creator, creator_quote_ata, market, vault, config, oracle, bet_close_ts: kickoff }
}

/// Loads the real Manifest program, if it has been fetched (`just fetch-manifest`). Tests that
/// need it skip, loudly, when it isn't there rather than failing for an unrelated reason.
pub fn load_manifest(svm: &mut LiteSVM) -> bool {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/deploy/manifest.so");
    match svm.add_program_from_file(manifest_program_id(), &path) {
        Ok(()) => true,
        Err(_) => {
            eprintln!("SKIPPED: {path:?} not found - run `just fetch-manifest` to enable the Manifest tests");
            false
        }
    }
}

// ── Assertions and reads ─────────────────────────────────────────────────

/// Asserts a transaction failed with exactly this program error - not merely "failed", so a test
/// can't pass because of an unrelated mistake in how it was built.
#[track_caller]
pub fn assert_fails_with(result: Result<(), litesvm::types::FailedTransactionMetadata>, expected: metis_core::error::MetisError) {
    use solana_sdk::{instruction::InstructionError, transaction::TransactionError};
    match result {
        Ok(()) => panic!("expected {expected:?} but the transaction succeeded"),
        Err(f) => match f.err {
            TransactionError::InstructionError(_, InstructionError::Custom(code)) => {
                assert_eq!(code, expected as u32, "expected {expected:?} ({}), got custom error {code}", expected as u32)
            }
            other => panic!("expected {expected:?} but got {other:?}\nlogs: {:#?}", f.meta.logs),
        },
    }
}

pub fn read_market(svm: &LiteSVM, market: &Pubkey) -> metis_core::amm::state::Market {
    let data = svm.get_account(market).expect("market exists").data;
    *bytemuck::from_bytes::<metis_core::amm::state::Market>(&data)
}

pub fn market_exists(svm: &LiteSVM, market: &Pubkey) -> bool {
    svm.get_account(market).is_some_and(|a| !a.data.is_empty())
}

/// Places a bet and returns the shares received. Panics if the bet fails.
pub fn bet(svm: &mut LiteSVM, m: &TestMarket, who: &Keypair, who_quote_ata: &Pubkey, side: u8, amount: u64) -> u64 {
    let before = token_balance(svm, &outcome_ata(&m.market, &who.pubkey(), side));
    send(svm, ix_place_bet(&m.market, &m.vault, &who.pubkey(), who_quote_ata, side, amount, 0, &m.config), who, &[])
        .expect("bet should succeed");
    token_balance(svm, &outcome_ata(&m.market, &who.pubkey(), side)) - before
}

/// Everything a settled-market test needs: the config with a chosen set of dials, a market with a
/// chosen condition, and bettors on both sides.
pub fn treasury_ata(svm: &mut LiteSVM) -> Pubkey {
    // The treasury's USDC account; created empty.
    let account = ata(&treasury(), &usdc_mint());
    svm.set_account(
        account,
        Account {
            lamports: svm.minimum_balance_for_rent_exemption(165),
            data: pack_token_account(&usdc_mint(), &treasury(), 0),
            owner: token_program_id(),
            ..Default::default()
        },
    )
    .unwrap();
    account
}

// ── Operators ────────────────────────────────────────────────────────────

pub fn delegate_pda(who: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[metis_core::constants::DELEGATED_AUTHORITY_SEED, who.as_ref()], &program_id()).0
}

/// Writes a completed attestation of `who`, as two roots attesting would leave it.
pub fn attest_operator(svm: &mut LiteSVM, who: &Pubkey) {
    let mut a = metis_authority::delegated::DelegatedAuthority::new(
        pinocchio::Address::from(who.to_bytes()),
        metis_core::config::state::DELEGATED_AUTHORITY_DISCRIMINATOR,
    );
    a.attesters = [root_signer(0).to_bytes(), root_signer(1).to_bytes()];
    a.is_attest_complete = 1u64.to_le_bytes();
    svm.set_account(
        delegate_pda(who),
        solana_sdk::account::Account {
            lamports: 1_000_000_000,
            data: bytemuck::bytes_of(&a).to_vec(),
            owner: program_id(),
            executable: false,
            rent_epoch: 0,
        },
    )
    .expect("write attestation");
}

/// Removes an attestation, as `remove_authority` would.
pub fn revoke_operator(svm: &mut LiteSVM, who: &Pubkey) {
    svm.set_account(delegate_pda(who), solana_sdk::account::Account::default()).expect("clear attestation");
}

/// A keypair with SOL that is an attested operator.
pub fn funded_operator(svm: &mut LiteSVM, lamports: u64) -> Keypair {
    let kp = funded_keypair(svm, lamports);
    attest_operator(svm, &kp.pubkey());
    kp
}

pub fn ix_forfeit_creator_fees(
    market: &Pubkey,
    vault: &Pubkey,
    treasury_ata: &Pubkey,
    config: &Pubkey,
    first: &Pubkey,
    second: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new(*vault, false),
            AccountMeta::new(*treasury_ata, false),
            AccountMeta::new_readonly(usdc_mint(), false),
            AccountMeta::new_readonly(*config, false),
            AccountMeta::new_readonly(*first, true),
            AccountMeta::new_readonly(*second, true),
            AccountMeta::new_readonly(token_program_id(), false),
        ],
        data: metis_core::settlement::state::FORFEIT_CREATOR_FEES.to_vec(),
    }
}

// ── DBC-backed markets ───────────────────────────────────────────────────

/// Writes an SPL mint with no mint or freeze authority and the given supply, standing in for what
/// a Meteora DBC base mint looks like once its pool has been created with an immutable authority
/// option (verified against a real DBC pool on a local validator).
pub fn set_final_mint(svm: &mut LiteSVM, mint: &Pubkey, supply: u64) {
    svm.set_account(
        *mint,
        Account { lamports: 1_000_000_000, data: pack_mint(None, supply, QUOTE_DECIMALS), owner: token_program_id(), executable: false, rent_epoch: 0 },
    )
    .expect("write backer mint");
}

/// Writes a mint that STILL has a mint authority (an unfinished raise).
pub fn set_mint_with_authority(svm: &mut LiteSVM, mint: &Pubkey, authority: &Pubkey, supply: u64) {
    svm.set_account(
        *mint,
        Account { lamports: 1_000_000_000, data: pack_mint(Some(authority), supply, QUOTE_DECIMALS), owner: token_program_id(), executable: false, rent_epoch: 0 },
    )
    .expect("write backer mint");
}

/// Gives `owner` `amount` of `mint` in their associated account.
pub fn set_token_balance(svm: &mut LiteSVM, mint: &Pubkey, owner: &Pubkey, amount: u64) -> Pubkey {
    let account = ata(owner, mint);
    svm.set_account(
        account,
        Account {
            lamports: svm.minimum_balance_for_rent_exemption(TOKEN_ACCOUNT_LEN),
            data: pack_token_account(mint, owner, amount),
            owner: token_program_id(),
            executable: false,
            rent_epoch: 0,
        },
    )
    .expect("write token account");
    account
}

/// Sets the amount held by an existing token account (the vault, as the raise would fund it).
pub fn set_vault_amount(svm: &mut LiteSVM, vault: &Pubkey, amount: u64) {
    let mut a = svm.get_account(vault).expect("vault exists");
    a.data[64..72].copy_from_slice(&amount.to_le_bytes());
    svm.set_account(*vault, a).expect("write vault");
}

#[allow(clippy::too_many_arguments)]
pub fn ix_initialize_backed_market(
    creator: &Pubkey,
    market: &Pubkey,
    vault: &Pubkey,
    nonce: u64,
    condition_kind: u8,
    condition_param: u8,
    config: &Pubkey,
    oracle: &Pubkey,
    platform: [u8; 32],
    backer_mint: &Pubkey,
    seed_target: u64,
    raise_deadline: i64,
) -> Instruction {
    let args = metis_dbc::InitializeBackedMarketArgs {
        nonce,
        num_outcomes: 2,
        condition_kind,
        condition_param,
        _padding: [0u8; 5],
        platform,
        backer_mint: backer_mint.to_bytes(),
        seed_target,
        raise_deadline,
    };
    let mut data = metis_core::amm::state::INITIALIZE_BACKED_MARKET.to_vec();
    data.extend_from_slice(bytes_of(&args));
    let (mint_0, _) = outcome_mint_pda(market, 0);
    let (mint_1, _) = outcome_mint_pda(market, 1);
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new(*vault, false),
            AccountMeta::new(*creator, true),
            AccountMeta::new_readonly(usdc_mint(), false),
            AccountMeta::new_readonly(*config, false),
            AccountMeta::new_readonly(*oracle, false),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            AccountMeta::new_readonly(token_program_id(), false),
            AccountMeta::new(mint_0, false),
            AccountMeta::new(mint_1, false),
        ],
        data,
    }
}

pub fn ix_activate_market(market: &Pubkey, vault: &Pubkey, operator: &Pubkey, config: &Pubkey, backer_mint: &Pubkey) -> Instruction {
    let (mint_0, _) = outcome_mint_pda(market, 0);
    let (mint_1, _) = outcome_mint_pda(market, 1);
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new_readonly(*vault, false),
            AccountMeta::new(*operator, true),
            AccountMeta::new_readonly(*config, false),
            AccountMeta::new_readonly(*backer_mint, false),
            AccountMeta::new(mint_0, false),
            AccountMeta::new(mint_1, false),
            AccountMeta::new(ata(market, &mint_0), false),
            AccountMeta::new(ata(market, &mint_1), false),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            AccountMeta::new_readonly(token_program_id(), false),
            AccountMeta::new_readonly(ata_program_id(), false),
            AccountMeta::new_readonly(delegate_pda(operator), false),
        ],
        data: metis_core::amm::state::ACTIVATE_MARKET.to_vec(),
    }
}

pub fn ix_claim_backer(market: &Pubkey, vault: &Pubkey, backer: &Pubkey, backer_quote_ata: &Pubkey, backer_mint: &Pubkey) -> Instruction {
    let (mint_0, _) = outcome_mint_pda(market, 0);
    let (mint_1, _) = outcome_mint_pda(market, 1);
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new(*vault, false),
            AccountMeta::new_readonly(*backer, true),
            AccountMeta::new(*backer_quote_ata, false),
            AccountMeta::new_readonly(usdc_mint(), false),
            AccountMeta::new(*backer_mint, false),
            AccountMeta::new(ata(backer, backer_mint), false),
            AccountMeta::new(mint_0, false),
            AccountMeta::new(mint_1, false),
            AccountMeta::new(ata(market, &mint_0), false),
            AccountMeta::new(ata(market, &mint_1), false),
            AccountMeta::new_readonly(token_program_id(), false),
        ],
        data: metis_core::settlement::state::CLAIM_BACKER.to_vec(),
    }
}

pub fn ix_cancel_pending_market(market: &Pubkey, config: &Pubkey, signer: &Pubkey) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new_readonly(*config, false),
            AccountMeta::new_readonly(*signer, true),
            AccountMeta::new_readonly(delegate_pda(signer), false),
        ],
        data: metis_core::settlement::state::CANCEL_PENDING_MARKET.to_vec(),
    }
}
