mod common;
// Changing protocol settings. Only the recorded admin may do it, and the
// guards stop an update that would lock everyone out.
//
// To see the printed output:
//   cargo test -p openstake-core --test update_config -- --nocapture --test-threads=1

use bytemuck::from_bytes;
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signature::Signer};

use crate::common::*;
use openstake_core::config::state::Config;

const SOL: u64 = solana_sdk::native_token::LAMPORTS_PER_SOL;

// Creates the config with known starting values and returns its address.
fn config_with_defaults(svm: &mut litesvm::LiteSVM, authority: &Keypair) -> Pubkey {
    svm.airdrop(&authority.pubkey(), 10 * SOL).expect("airdrop");
    let (config, _) = config_pda(&authority.pubkey());
    send(
        svm,
        ix_initialize_config(
            &authority.pubkey(),
            &config,
            10_000_000,
            25,
            500,
            2_000,
            &authority.pubkey(),
            &usdc_mint(),
            2,
        ),
        authority,
        &[],
    )
    .expect("initialize_config should succeed");
    config
}

#[test]
fn the_recorded_admin_can_update() {
    let mut svm = setup();
    let authority = authority_keypair();
    let config = config_with_defaults(&mut svm, &authority);

    send(
        &mut svm,
        ix_update_config(
            &config,
            &authority.pubkey(),
            20_000_000,
            50,
            1_000,
            4_000,
            &authority.pubkey(),
            &usdc_mint(),
            2,
        ),
        &authority,
        &[],
    )
    .expect("update_config by the recorded admin should succeed");

    let config_account = svm.get_account(&config).expect("config account exists");
    let config_state: &Config = from_bytes(&config_account.data);

    println!();
    println!("settings after update:");
    println!(
        "  market_init_fee:     {} (was 10000000)",
        config_state.market_init_fee
    );
    println!("  fee_bps:             {} (was 25)", config_state.fee_bps);
    println!(
        "  slippage_limit_bps:  {} (was 500)",
        config_state.slippage_limit_bps
    );
    println!(
        "  max_in_bps_of_vault: {} (was 2000)",
        config_state.max_in_bps_of_vault
    );
    println!();

    assert_eq!(config_state.market_init_fee, 20_000_000);
    assert_eq!(config_state.fee_bps, 50);
    assert_eq!(config_state.slippage_limit_bps, 1_000);
    assert_eq!(config_state.max_in_bps_of_vault, 4_000);
}

#[test]
fn a_non_admin_is_rejected() {
    let mut svm = setup();
    let authority = authority_keypair();
    let config = config_with_defaults(&mut svm, &authority);

    let impostor = funded_keypair(&mut svm, 10 * SOL);
    let result = send(
        &mut svm,
        ix_update_config(
            &config,
            &impostor.pubkey(),
            20_000_000,
            50,
            1_000,
            4_000,
            &impostor.pubkey(),
            &usdc_mint(),
            2,
        ),
        &impostor,
        &[],
    );

    println!();
    println!("recorded admin: {}", authority.pubkey());
    println!("impostor:       {}", impostor.pubkey());
    println!(
        "update_config -> {}",
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!();

    assert!(
        result.is_err(),
        "only the config's recorded admin may update it"
    );
}

#[test]
fn admin_can_be_rotated_and_the_old_one_loses_access() {
    let mut svm = setup();
    let authority = authority_keypair();
    let config = config_with_defaults(&mut svm, &authority);

    // Standing in for handing control to a real Squads multisig vault.
    let new_admin = Keypair::new();
    send(
        &mut svm,
        ix_update_config(
            &config,
            &authority.pubkey(),
            10_000_000,
            25,
            500,
            2_000,
            &new_admin.pubkey(),
            &usdc_mint(),
            2,
        ),
        &authority,
        &[],
    )
    .expect("admin rotation should succeed");

    let config_account = svm.get_account(&config).expect("config account exists");
    let config_state: &Config = from_bytes(&config_account.data);

    println!();
    println!("admin rotated:");
    println!("  was: {}", authority.pubkey());
    println!("  now: {}", config_state.admin);

    assert_eq!(config_state.admin, new_admin.pubkey());

    let result = send(
        &mut svm,
        ix_update_config(
            &config,
            &authority.pubkey(),
            30_000_000,
            75,
            1_500,
            6_000,
            &authority.pubkey(),
            &usdc_mint(),
            2,
        ),
        &authority,
        &[],
    );
    println!(
        "old admin trying again -> {}",
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!();

    assert!(
        result.is_err(),
        "the old admin must lose access once a new one is recorded"
    );
}

#[test]
fn a_zeroed_admin_is_rejected() {
    // Nobody holds the key for the zero address, so this would lock the config
    // permanently - there's no fallback, since the bootstrap admin is only
    // ever consulted at creation.
    let mut svm = setup();
    let authority = authority_keypair();
    let config = config_with_defaults(&mut svm, &authority);

    let result = send(
        &mut svm,
        ix_update_config(
            &config,
            &authority.pubkey(),
            10_000_000,
            25,
            500,
            2_000,
            &Pubkey::default(),
            &usdc_mint(),
            2,
        ),
        &authority,
        &[],
    );

    println!();
    println!(
        "new_admin = all zeroes -> {}",
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!("(nobody holds that key, so this would lock the config forever)");
    println!();

    assert!(
        result.is_err(),
        "setting the admin to the zero address must be rejected"
    );
}

#[test]
fn out_of_range_bps_is_rejected() {
    let mut svm = setup();
    let authority = authority_keypair();
    let config = config_with_defaults(&mut svm, &authority);

    let result = send(
        &mut svm,
        ix_update_config(
            &config,
            &authority.pubkey(),
            10_000_000,
            25,
            500,
            50_000, // max_in_bps_of_vault over 100%
            &authority.pubkey(),
            &usdc_mint(),
            2,
        ),
        &authority,
        &[],
    );

    println!();
    println!(
        "max_in_bps_of_vault = 50000 (500%) -> {}",
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!();

    assert!(result.is_err(), "bps values above 10_000 must be rejected");
}
