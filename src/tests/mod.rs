pub mod claim;
pub mod initialize_market;
pub mod place_bet;
pub mod scenario_bulk_bettors;
pub mod scenario_bulk_solvency;
pub mod settle_market;

use bytemuck::bytes_of;
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair, Signer},
    transaction::Transaction,
};
use std::str::FromStr;

use crate::{
    constants::{MARKET_SEED, POSITION_SEED, VAULT_SEED},
    discriminator,
    instructions::{
        initialize_market::InitializeMarketArgs, place_bet::PlaceBetArgs,
        settle_market::SettleMarketArgs,
    },
};

pub const PROGRAM_ID_STR: &str = "2jt26iYKHghHDFVWQ8rnCdQc5B144Cos6HyJVw68qiv6";

pub fn program_id() -> Pubkey {
    Pubkey::from_str(PROGRAM_ID_STR).expect("valid program id")
}

// Loads the compiled program into a fresh litesvm instance. 
pub fn setup() -> LiteSVM {
    let mut svm = LiteSVM::new();
    svm.add_program_from_file(program_id(), "target/deploy/engine.so")
        .expect("load engine.so — did you run `cargo build-sbf`?");
    svm
}

// A funded keypair, ready to sign and pay for transactions/rent.
pub fn funded_keypair(svm: &mut LiteSVM, lamports: u64) -> Keypair {
    let kp = Keypair::new();
    svm.airdrop(&kp.pubkey(), lamports).expect("airdrop");
    kp
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
            crate::constants::AUTHORITY,
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

// One instruction-builder per instruction — discriminator bytes followed
// by the bytemuck-serialized args, same layout the on-chain dispatcher
// expects (see lib.rs's split_first_chunk::<8>()).

pub fn ix_initialize_market(
    creator: &Pubkey,
    market: &Pubkey,
    vault: &Pubkey,
    position: &Pubkey,
    nonce: u64,
    num_outcomes: u8,
) -> Instruction {
    let args = InitializeMarketArgs {
        nonce,
        num_outcomes,
        _padding: [0u8; 7],
    };
    let mut data = discriminator::INITIALIZE_MARKET.to_vec();
    data.extend_from_slice(bytes_of(&args));

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new(*vault, false),
            AccountMeta::new(*position, false),
            AccountMeta::new(*creator, true),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
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
) -> Instruction {
    let args = PlaceBetArgs {
        deposit_amount,
        outcome_index,
        _padding: [0u8; 7],
    };
    let mut data = discriminator::PLACE_BET.to_vec();
    data.extend_from_slice(bytes_of(&args));

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new(*vault, false),
            AccountMeta::new(*position, false),
            AccountMeta::new(*bettor, true),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
        ],
        data,
    }
}

pub fn ix_settle_market(market: &Pubkey, vault: &Pubkey, authority: &Pubkey, winning_outcome: u8) -> Instruction {
    let args = SettleMarketArgs {
        winning_outcome,
        _padding: [0u8; 7],
    };
    let mut data = discriminator::SETTLE_MARKET.to_vec();
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

pub fn ix_claim(market: &Pubkey, vault: &Pubkey, position: &Pubkey, bettor: &Pubkey) -> Instruction {
    let data = discriminator::CLAIM.to_vec();

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
