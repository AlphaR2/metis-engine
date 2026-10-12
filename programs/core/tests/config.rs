// Config rules, one scenario per rule: who may initialize and update, the hardcoded bounds on
// every dial, the two-signature path for the money-routing fields, and that a live market keeps
// the dials it was created under.

mod common;

use bytemuck::from_bytes;
use common::*;
use metis_core::{
    amm::state::Market,
    config::state::ConfigDials,
    constants::{MAX_FEE_PRIMARY_BPS, MAX_RAKE_BPS, MIN_SEED},
};
use solana_sdk::pubkey::Pubkey;

fn fresh() -> (litesvm::LiteSVM, Pubkey) {
    let mut svm = setup();
    let payer = root_signer(0);
    svm.airdrop(&payer, 10_000_000_000).unwrap();
    (svm, config_pda().0)
}

fn init(svm: &mut litesvm::LiteSVM, payer: Pubkey, second: Pubkey, dials: ConfigDials) -> bool {
    let (config, _) = config_pda();
    send_as(
        svm,
        ix_initialize_config(&payer, &second, &config, dials, &usdc_mint(), &treasury()),
        payer,
    )
    .is_ok()
}

#[test]
fn initialize_needs_two_distinct_root_signers() {
    let (mut svm, _) = fresh();
    let outsider = Pubkey::new_unique();
    svm.airdrop(&outsider, 1_000_000_000).unwrap();

    assert!(!init(&mut svm, root_signer(0), root_signer(0), default_dials()), "one root twice");
    assert!(!init(&mut svm, root_signer(0), outsider, default_dials()), "root + outsider");
    assert!(!init(&mut svm, outsider, root_signer(1), default_dials()), "outsider + root");
    assert!(init(&mut svm, root_signer(0), root_signer(1), default_dials()), "two different roots");
}

#[test]
fn initialize_rejects_out_of_bounds_dials() {
    let bad: Vec<(&str, ConfigDials)> = vec![
        ("seed below floor", ConfigDials { market_init_fee: MIN_SEED - 1, ..default_dials() }),
        ("fee above ceiling", ConfigDials { fee_bps: MAX_FEE_PRIMARY_BPS + 1, ..default_dials() }),
        ("rake above ceiling", ConfigDials { rake_bps: MAX_RAKE_BPS + 1, ..default_dials() }),
        ("trade cap off", ConfigDials { max_in_bps_of_vault: 0, ..default_dials() }),
        ("shares != 100%", ConfigDials { fee_share_creator_bps: 1_999, ..default_dials() }),
        ("platform share breaks the total", ConfigDials { fee_share_platform_bps: 1_001, ..default_dials() }),
    ];
    for (name, dials) in bad {
        let (mut svm, _) = fresh();
        assert!(!init(&mut svm, root_signer(0), root_signer(1), dials), "{name} should be refused");
    }
}

#[test]
fn update_config_is_one_root_but_still_bounded() {
    let mut svm = setup();
    let config = setup_with_config(&mut svm);
    let root = root_signer(2);
    svm.airdrop(&root, 1_000_000_000).unwrap();

    let ok = ConfigDials { fee_bps: 25, rake_bps: 100, ..default_dials() };
    send_as(&mut svm, ix_update_config(&config, &root, ok), root).expect("one root, in bounds");

    let too_high = ConfigDials { rake_bps: MAX_RAKE_BPS + 1, ..default_dials() };
    assert!(send_as(&mut svm, ix_update_config(&config, &root, too_high), root).is_err());

    let outsider = Pubkey::new_unique();
    svm.airdrop(&outsider, 1_000_000_000).unwrap();
    assert!(send_as(&mut svm, ix_update_config(&config, &outsider, ok), outsider).is_err());
}

#[test]
fn critical_fields_need_two_distinct_roots() {
    let mut svm = setup();
    let config = setup_with_config(&mut svm);
    let (a, b) = (root_signer(0), root_signer(1));
    svm.airdrop(&a, 1_000_000_000).unwrap();
    let new_treasury = Pubkey::new_from_array([9; 32]);
    let zero = Pubkey::default();

    let same = ix_update_config_critical(&config, &a, &a, &usdc_mint(), &new_treasury, &zero);
    assert!(send_as(&mut svm, same, a).is_err(), "one key passed twice");

    let outsider = Pubkey::new_unique();
    let mixed = ix_update_config_critical(&config, &a, &outsider, &usdc_mint(), &new_treasury, &zero);
    assert!(send_as(&mut svm, mixed, a).is_err(), "root + outsider");

    let zero_treasury = ix_update_config_critical(&config, &a, &b, &usdc_mint(), &zero, &zero);
    assert!(send_as(&mut svm, zero_treasury, a).is_err(), "zero treasury");

    let good = ix_update_config_critical(&config, &a, &b, &usdc_mint(), &new_treasury, &zero);
    send_as(&mut svm, good, a).expect("two distinct roots");
    let stored = svm.get_account(&config).unwrap();
    assert!(stored.data.windows(32).any(|w| w == new_treasury.as_ref()), "treasury was written");
}

#[test]
fn a_live_market_keeps_the_dials_it_was_created_under() {
    let mut svm = setup();
    let config = setup_with_dials(
        &mut svm,
        ConfigDials { fee_bps: 10, rake_bps: 50, dispute_secs: 900, ..default_dials() },
    );
    let ts = default_close_ts(&svm);
    let market = open_market_with_config(&mut svm, 0, ts, config);

    let root = root_signer(0);
    svm.airdrop(&root, 1_000_000_000).unwrap();
    let changed = ConfigDials { fee_bps: 40, rake_bps: 250, dispute_secs: 7_200, ..default_dials() };
    send_as(&mut svm, ix_update_config(&config, &root, changed), root).unwrap();

    let data = svm.get_account(&market.market).unwrap().data;
    let m: &Market = from_bytes(&data);
    assert_eq!((m.fee_bps, m.rake_bps, m.dispute_secs), (10, 50, 900), "snapshot must not move");
}
