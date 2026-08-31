mod common;

use bytemuck::from_bytes;
use solana_sdk::{signature::Keypair, signature::Signer};

use crate::common::{authority_keypair, config_pda, funded_keypair, ix_initialize_config, ix_update_config, send, setup};
use openstake_core::config::state::Config;

#[test]
fn update_config_by_the_recorded_admin_succeeds() {
    let mut svm = setup();
    let authority = authority_keypair();
    svm.airdrop(&authority.pubkey(), 10_000_000_000).expect("airdrop");

    let (config, _) = config_pda(&authority.pubkey());
    send(
        &mut svm,
        ix_initialize_config(&authority.pubkey(), &config, 10_000_000, 25, 500, 2_000, &authority.pubkey(), 2),
        &authority,
        &[],
    )
    .expect("initialize_config should succeed");

    // authority was set as Config.admin at init — it can now update its own config.
    send(
        &mut svm,
        ix_update_config(&config, &authority.pubkey(), 20_000_000, 50, 1_000, 4_000, &authority.pubkey(), 2),
        &authority,
        &[],
    )
    .expect("update_config by the recorded admin should succeed");

    let config_account = svm.get_account(&config).expect("config account exists");
    let config_state: &Config = from_bytes(&config_account.data);

    assert_eq!(config_state.market_init_fee, 20_000_000);
    assert_eq!(config_state.fee_bps, 50);
    assert_eq!(config_state.slippage_limit_bps, 1_000);
    assert_eq!(config_state.max_in_bps_of_vault, 4_000);
}

#[test]
fn update_config_by_a_non_admin_is_rejected() {
    let mut svm = setup();
    let authority = authority_keypair();
    svm.airdrop(&authority.pubkey(), 10_000_000_000).expect("airdrop");

    let (config, _) = config_pda(&authority.pubkey());
    send(
        &mut svm,
        ix_initialize_config(&authority.pubkey(), &config, 10_000_000, 25, 500, 2_000, &authority.pubkey(), 2),
        &authority,
        &[],
    )
    .expect("initialize_config should succeed");

    let impostor = funded_keypair(&mut svm, 10_000_000_000);
    let result = send(
        &mut svm,
        ix_update_config(&config, &impostor.pubkey(), 20_000_000, 50, 1_000, 4_000, &impostor.pubkey(), 2),
        &impostor,
        &[],
    );

    assert!(result.is_err(), "only the config's own recorded admin may call update_config");
}

#[test]
fn update_config_can_rotate_admin_and_the_old_admin_loses_access() {
    let mut svm = setup();
    let authority = authority_keypair();
    svm.airdrop(&authority.pubkey(), 10_000_000_000).expect("airdrop");

    let (config, _) = config_pda(&authority.pubkey());
    send(
        &mut svm,
        ix_initialize_config(&authority.pubkey(), &config, 10_000_000, 25, 500, 2_000, &authority.pubkey(), 2),
        &authority,
        &[],
    )
    .expect("initialize_config should succeed");

    // Rotate admin to a fresh keypair — standing in for handing control to a
    // real Squads multisig vault.
    let new_admin = Keypair::new();
    send(
        &mut svm,
        ix_update_config(&config, &authority.pubkey(), 10_000_000, 25, 500, 2_000, &new_admin.pubkey(), 2),
        &authority,
        &[],
    )
    .expect("admin rotation should succeed");

    let config_account = svm.get_account(&config).expect("config account exists");
    let config_state: &Config = from_bytes(&config_account.data);
    assert_eq!(config_state.admin, new_admin.pubkey());

    // The old admin (authority) no longer has any special standing.
    let result = send(
        &mut svm,
        ix_update_config(&config, &authority.pubkey(), 30_000_000, 75, 1_500, 6_000, &authority.pubkey(), 2),
        &authority,
        &[],
    );
    assert!(result.is_err(), "the old admin must lose access once a new admin is recorded");
}
