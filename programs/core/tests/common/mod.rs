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
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair, Signer},
    transaction::Transaction,
};
use std::str::FromStr;

use openstake_core::{
    amm::instructions::{initialize_market::InitializeMarketArgs, place_bet::PlaceBetArgs},
    config::instructions::{initialize_config::InitializeConfigArgs, update_config::UpdateConfigArgs},
    constants::{CONFIG_SEED, MARKET_INIT_FEE, MARKET_SEED, MAX_OUTCOMES, POSITION_SEED, VAULT_SEED},
    settlement::instructions::settle_market::SettleMarketArgs,
};

pub const PROGRAM_ID_STR: &str = "2jt26iYKHghHDFVWQ8rnCdQc5B144Cos6HyJVw68qiv6";

pub fn program_id() -> Pubkey {
    Pubkey::from_str(PROGRAM_ID_STR).expect("valid program id")
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
    svm
}

// A funded keypair, ready to sign and pay for transactions/rent.
pub fn funded_keypair(svm: &mut LiteSVM, lamports: u64) -> Keypair {
    let kp = Keypair::new();
    svm.airdrop(&kp.pubkey(), lamports).expect("airdrop");
    kp
}

// initialize_market and place_bet both now read Config, so every test that
// calls either one needs a real Config account to exist first. This does
// that once: funds the bootstrap admin, creates the singleton Config with
// defaults that reproduce today's pre-Config behavior exactly (slippage and
// max_in both 0 = uncapped, market_init_fee/max_outcomes matching the
// existing constants), and returns the config pubkey to pass into every
// ix_initialize_market/ix_place_bet call.
pub fn setup_with_config(svm: &mut LiteSVM) -> Pubkey {
    let authority = authority_keypair();
    svm.airdrop(&authority.pubkey(), 10 * MARKET_INIT_FEE)
        .expect("airdrop bootstrap admin");

    let (config, _) = config_pda(&authority.pubkey());
    send(
        svm,
        ix_initialize_config(
            &authority.pubkey(),
            &config,
            MARKET_INIT_FEE,
            0,    // fee_bps — no fee mechanism wired up yet
            0,    // slippage_limit_bps — 0 = uncapped, matches pre-Config behavior
            0,    // max_in_bps_of_vault — 0 = uncapped, matches pre-Config behavior
            &authority.pubkey(),
            MAX_OUTCOMES as u8,
        ),
        &authority,
        &[],
    )
    .expect("setup_with_config: initialize_config should succeed");

    config
}

// Moves litesvm's Clock sysvar forward past `ts` — used by tests that need
// to actually reach settle_market, since it now requires
// now >= market.bet_close_ts (see settlement::instructions::settle_market).
pub fn advance_clock_past(svm: &mut LiteSVM, ts: i64) {
    let mut clock = svm.get_sysvar::<solana_sdk::clock::Clock>();
    clock.unix_timestamp = ts + 1;
    svm.set_sysvar(&clock);
}

// current litesvm clock time, for computing a near-future bet_close_ts.
pub fn current_clock_ts(svm: &LiteSVM) -> i64 {
    svm.get_sysvar::<solana_sdk::clock::Clock>().unix_timestamp
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

pub fn market_pda(creator: &Pubkey, nonce: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[MARKET_SEED, creator.as_ref(), &nonce.to_le_bytes()],
        &program_id(),
    )
}

pub fn vault_pda(market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[VAULT_SEED, market.as_ref()], &program_id())
}

pub fn position_pda(market: &Pubkey, owner: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[POSITION_SEED, market.as_ref(), owner.as_ref()],
        &program_id(),
    )
}

// Per-authority PDA (see initialize_config.rs's own note on this) — in
// practice always called with the bootstrap admin's own pubkey, since only
// that signer can ever successfully create one.
pub fn config_pda(authority: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[CONFIG_SEED, authority.as_ref()], &program_id())
}

// One instruction-builder per instruction — discriminator bytes followed
// by the bytemuck-serialized args, same layout the on-chain dispatcher
// expects (see lib.rs's split_first_chunk::<8>()).

// Far enough in the future that no test's litesvm clock will ever reach it —
// most tests aren't exercising the close-timestamp behavior itself and just
// want betting to stay open for the whole test.
pub const FAR_FUTURE_CLOSE: i64 = i64::MAX / 2;

pub fn ix_initialize_market(
    creator: &Pubkey,
    market: &Pubkey,
    vault: &Pubkey,
    position: &Pubkey,
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

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new(*vault, false),
            AccountMeta::new(*position, false),
            AccountMeta::new(*creator, true),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            AccountMeta::new_readonly(*config, false),
        ],
        data,
    }
}

pub fn ix_place_bet(
    market: &Pubkey,
    vault: &Pubkey,
    position: &Pubkey,
    bettor: &Pubkey,
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

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new(*vault, false),
            AccountMeta::new(*position, false),
            AccountMeta::new(*bettor, true),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            AccountMeta::new_readonly(*config, false),
        ],
        data,
    }
}

pub fn ix_settle_market(market: &Pubkey, vault: &Pubkey, authority: &Pubkey, winning_outcome: u8) -> Instruction {
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

pub fn ix_initialize_config(
    authority: &Pubkey,
    config: &Pubkey,
    market_init_fee: u64,
    fee_bps: u16,
    slippage_limit_bps: u16,
    max_in_bps_of_vault: u16,
    admin: &Pubkey,
    max_outcomes: u8,
) -> Instruction {
    let args = InitializeConfigArgs {
        market_init_fee,
        fee_bps,
        slippage_limit_bps,
        max_in_bps_of_vault,
        admin: admin.to_bytes().into(),
        max_outcomes,
        _padding: [0u8; 1],
    };
    let mut data = openstake_core::config::state::INITIALIZE_CONFIG.to_vec();
    data.extend_from_slice(bytes_of(&args));

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new_readonly(*authority, true),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            AccountMeta::new(*config, false),
        ],
        data,
    }
}

pub fn ix_update_config(
    config: &Pubkey,
    admin: &Pubkey,
    market_init_fee: u64,
    fee_bps: u16,
    slippage_limit_bps: u16,
    max_in_bps_of_vault: u16,
    new_admin: &Pubkey,
    max_outcomes: u8,
) -> Instruction {
    let args = UpdateConfigArgs {
        market_init_fee,
        fee_bps,
        slippage_limit_bps,
        max_in_bps_of_vault,
        new_admin: new_admin.to_bytes().into(),
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

pub fn ix_claim(market: &Pubkey, vault: &Pubkey, position: &Pubkey, bettor: &Pubkey) -> Instruction {
    let data = openstake_core::settlement::state::CLAIM.to_vec();

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new_readonly(*market, false),
            AccountMeta::new(*vault, false),
            AccountMeta::new(*position, false),
            AccountMeta::new(*bettor, true),
        ],
        data,
    }
}

// Builds and sends a transaction signed by `payer` + any extra signers,
// returns the litesvm result so callers can assert success or a specific
// error.
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

    svm.send_transaction(tx).map(|_| ()).map_err(|e| e)
}
