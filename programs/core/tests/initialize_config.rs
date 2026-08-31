mod common;

use bytemuck::from_bytes;
use solana_sdk::signature::Signer;

use crate::common::{authority_keypair, config_pda, funded_keypair, ix_initialize_config, send, setup};
use openstake_core::config::state::Config;

#[test]
fn initialize_config_by_the_bootstrap_admin_succeeds() {
    let mut svm = setup();
    let authority = authority_keypair(); // == constants::INITIAL_CONFIG_ADMIN
    svm.airdrop(&authority.pubkey(), 10_000_000_000).expect("airdrop");

    let (config, _) = config_pda(&authority.pubkey());

    send(
        &mut svm,
        ix_initialize_config(&authority.pubkey(), &config, 10_000_000, 25, 500, 2_000, &authority.pubkey(), 2),
        &authority,
        &[],
    )
    .expect("initialize_config by the bootstrap admin should succeed");

    let config_account = svm.get_account(&config).expect("config account exists");
    let config_state: &Config = from_bytes(&config_account.data);

    assert_eq!(config_state.market_init_fee, 10_000_000);
    assert_eq!(config_state.fee_bps, 25);
    assert_eq!(config_state.slippage_limit_bps, 500);
    assert_eq!(config_state.max_in_bps_of_vault, 2_000);
    assert_eq!(config_state.max_outcomes, 2);
    assert_eq!(config_state.admin, authority.pubkey());
}

#[test]
fn initialize_config_by_anyone_else_is_rejected() {
    let mut svm = setup();
    // A funded wallet that is NOT the bootstrap admin.
    let impostor = funded_keypair(&mut svm, 10_000_000_000);

    let (config, _) = config_pda(&impostor.pubkey());

    let result = send(
        &mut svm,
        ix_initialize_config(&impostor.pubkey(), &config, 10_000_000, 25, 500, 2_000, &impostor.pubkey(), 2),
        &impostor,
        &[],
    );

    assert!(result.is_err(), "only the bootstrap admin (constants::INITIAL_CONFIG_ADMIN) may call initialize_config");
}
