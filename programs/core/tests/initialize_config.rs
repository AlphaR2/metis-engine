mod common;
// Creating the singleton Config. Only the bootstrap admin may do it, and the
// values it writes are what every market afterwards is bound by.
//
// To see the printed output:
//   cargo test -p openstake-core --test initialize_config -- --nocapture --test-threads=1

use bytemuck::from_bytes;
use solana_sdk::signature::Signer;

use crate::common::*;
use openstake_core::config::state::Config;

const SOL: u64 = solana_sdk::native_token::LAMPORTS_PER_SOL;

#[test]
fn the_bootstrap_admin_can_create_it() {
    let mut svm = setup();
    let authority = authority_keypair(); // == constants::INITIAL_CONFIG_ADMIN
    svm.airdrop(&authority.pubkey(), 10 * SOL).expect("airdrop");

    let (config, _) = config_pda(&authority.pubkey());

    send(
        &mut svm,
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
        &authority,
        &[],
    )
    .expect("initialize_config by the bootstrap admin should succeed");

    let config_account = svm.get_account(&config).expect("config account exists");
    let config_state: &Config = from_bytes(&config_account.data);

    println!();
    println!("config {config}");
    println!(
        "  market_init_fee:     {} (${:.2})",
        config_state.market_init_fee,
        config_state.market_init_fee as f64 / 1e6
    );
    println!(
        "  fee_bps:             {} ({:.2}%)",
        config_state.fee_bps,
        config_state.fee_bps as f64 / 100.0
    );
    println!(
        "  slippage_limit_bps:  {} ({:.2}%)",
        config_state.slippage_limit_bps,
        config_state.slippage_limit_bps as f64 / 100.0
    );
    println!(
        "  max_in_bps_of_vault: {} ({:.2}%)",
        config_state.max_in_bps_of_vault,
        config_state.max_in_bps_of_vault as f64 / 100.0
    );
    println!("  max_outcomes:        {}", config_state.max_outcomes);
    println!("  admin:               {}", config_state.admin);
    println!("  quote_mint:          {}", config_state.quote_mint);
    println!();

    assert_eq!(config_state.market_init_fee, 10_000_000);
    assert_eq!(config_state.fee_bps, 25);
    assert_eq!(config_state.slippage_limit_bps, 500);
    assert_eq!(config_state.max_in_bps_of_vault, 2_000);
    assert_eq!(config_state.max_outcomes, 2);
    assert_eq!(config_state.admin, authority.pubkey());
    assert_eq!(
        config_state.quote_mint,
        usdc_mint(),
        "the stablecoin every market will be priced in"
    );
}

#[test]
fn anyone_else_is_rejected() {
    // Without this gate, creating the config would be a race - whoever's
    // transaction landed first would be admin forever.
    let mut svm = setup();
    let impostor = funded_keypair(&mut svm, 10 * SOL);

    let (config, _) = config_pda(&impostor.pubkey());

    let result = send(
        &mut svm,
        ix_initialize_config(
            &impostor.pubkey(),
            &config,
            10_000_000,
            25,
            500,
            2_000,
            &impostor.pubkey(),
            &usdc_mint(),
            2,
        ),
        &impostor,
        &[],
    );

    println!();
    println!("bootstrap admin: {}", authority_keypair().pubkey());
    println!("impostor:        {}", impostor.pubkey());
    println!(
        "initialize_config -> {}",
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!();

    assert!(
        result.is_err(),
        "only the bootstrap admin (constants::INITIAL_CONFIG_ADMIN) may call initialize_config"
    );
}

#[test]
fn out_of_range_bps_is_rejected() {
    // 10_000 bps is 100%. Anything above that isn't a percentage, and a typo
    // here would silently misconfigure every market created afterwards.
    let mut svm = setup();
    let authority = authority_keypair();
    svm.airdrop(&authority.pubkey(), 10 * SOL).expect("airdrop");
    let (config, _) = config_pda(&authority.pubkey());

    let result = send(
        &mut svm,
        ix_initialize_config(
            &authority.pubkey(),
            &config,
            10_000_000,
            25,
            50_000, // slippage_limit_bps way over 100%
            2_000,
            &authority.pubkey(),
            &usdc_mint(),
            2,
        ),
        &authority,
        &[],
    );

    println!();
    println!(
        "slippage_limit_bps = 50000 (500%) -> {}",
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!();

    assert!(result.is_err(), "bps values above 10_000 must be rejected");
}

#[test]
fn a_zeroed_quote_mint_is_rejected() {
    // No real mint is the zero address, so this would make every future
    // initialize_market fail with nothing to explain why.
    let mut svm = setup();
    let authority = authority_keypair();
    svm.airdrop(&authority.pubkey(), 10 * SOL).expect("airdrop");
    let (config, _) = config_pda(&authority.pubkey());

    let result = send(
        &mut svm,
        ix_initialize_config(
            &authority.pubkey(),
            &config,
            10_000_000,
            25,
            500,
            2_000,
            &authority.pubkey(),
            &solana_sdk::pubkey::Pubkey::default(),
            2,
        ),
        &authority,
        &[],
    );

    println!();
    println!(
        "quote_mint = all zeroes -> {}",
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!();

    assert!(result.is_err(), "a zeroed quote mint must be rejected");
}
