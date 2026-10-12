//! Instruction builders for the operator-side steps: the trust accounts, attestations, market
//! creation and betting. Pure (no network), so they are the same on a local validator and on
//! mainnet. Every address derived here uses the trust roots compiled into this build, so build
//! with the same network feature as the deployed programs.

use bytemuck::bytes_of;
use metis_core::{
    amm::{
        instructions::{
            initialize_market::InitializeMarketArgs,
            place_bet::PlaceBetArgs,
            set_metadata::SetMetadataArgs,
        },
        metadata::{CATEGORY_MAX, LABEL_MAX, LEAGUE_MAX, QUESTION_MAX, RUBRIC_MAX},
        state::{INITIALIZE_MARKET, PLACE_BET, SET_METADATA},
    },
    config::{
        instructions::initialize_config::InitializeConfigArgs,
        state::{ATTEST_AUTHORITY, ConfigDials, INITIALIZE_CONFIG},
    },
    constants::{CONFIG_SEED, DELEGATED_AUTHORITY_SEED, GUARDIAN, MARKET_SEED, METADATA_SEED, OUTCOME_MINT_SEED, VAULT_SEED},
};
use oracle::{
    constants::{AUTHORITY_SEED as ORACLE_AUTHORITY_SEED, INDIVIDUAL_AUTHORITY_SEED},
    instructions::POracle,
};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

pub const SYSTEM: Pubkey = Pubkey::from_str_const("11111111111111111111111111111111");
pub const TOKEN: Pubkey = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const ATA_PROGRAM: Pubkey = Pubkey::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

pub fn core_program() -> Pubkey {
    Pubkey::new_from_array(metis_core::ID.to_bytes())
}
pub fn oracle_program() -> Pubkey {
    Pubkey::new_from_array(oracle::ID.to_bytes())
}

fn pda(seeds: &[&[u8]], program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(seeds, program).0
}

pub fn config_address() -> Pubkey {
    pda(&[CONFIG_SEED, GUARDIAN.as_ref()], &core_program())
}
pub fn oracle_authority_address() -> Pubkey {
    pda(&[ORACLE_AUTHORITY_SEED, GUARDIAN.as_ref()], &oracle_program())
}
pub fn core_attestation(who: &Pubkey) -> Pubkey {
    pda(&[DELEGATED_AUTHORITY_SEED, who.as_ref()], &core_program())
}
pub fn oracle_attestation(who: &Pubkey) -> Pubkey {
    pda(&[INDIVIDUAL_AUTHORITY_SEED, who.as_ref()], &oracle_program())
}
pub fn market_address(creator: &Pubkey, nonce: u64) -> Pubkey {
    pda(&[MARKET_SEED, creator.as_ref(), &nonce.to_le_bytes()], &core_program())
}
pub fn vault_address(market: &Pubkey) -> Pubkey {
    pda(&[VAULT_SEED, market.as_ref()], &core_program())
}
pub fn outcome_mint(market: &Pubkey, side: u8) -> Pubkey {
    pda(&[OUTCOME_MINT_SEED, market.as_ref(), &[side]], &core_program())
}
pub fn metadata_address(market: &Pubkey) -> Pubkey {
    pda(&[METADATA_SEED, market.as_ref()], &core_program())
}
pub fn ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[owner.as_ref(), TOKEN.as_ref(), mint.as_ref()], &ATA_PROGRAM).0
}

fn m(key: &Pubkey, signer: bool, writable: bool) -> AccountMeta {
    AccountMeta { pubkey: *key, is_signer: signer, is_writable: writable }
}

/// Oracle: create its root-authority account. `initializer` must be one of the roots.
pub fn oracle_init(initializer: &Pubkey) -> Instruction {
    Instruction {
        program_id: oracle_program(),
        accounts: vec![m(initializer, true, true), m(&oracle_authority_address(), false, true), m(&SYSTEM, false, false)],
        data: vec![POracle::InitializeAuthority as u8],
    }
}

/// Oracle: one root (or attested authority) attests `candidate`. Two roots doing it completes it.
pub fn oracle_attest(attester: &Pubkey, attester_attestation: Option<&Pubkey>, candidate: &Pubkey) -> Instruction {
    let mut accounts = vec![
        m(attester, true, true),
        m(&oracle_authority_address(), false, true),
        m(candidate, false, false),
        m(&oracle_attestation(candidate), false, true),
        m(&SYSTEM, false, false),
    ];
    accounts.extend(attester_attestation.map(|a| m(a, false, false)));
    Instruction { program_id: oracle_program(), accounts, data: vec![POracle::AttestAuthority as u8] }
}

/// Core: create the Config account (two distinct roots).
pub fn config_init(first_root: &Pubkey, second_root: &Pubkey, dials: ConfigDials, quote_mint: &Pubkey, treasury: &Pubkey) -> Instruction {
    let args = InitializeConfigArgs { dials, quote_mint: quote_mint.to_bytes().into(), treasury: treasury.to_bytes().into() };
    let mut data = INITIALIZE_CONFIG.to_vec();
    data.extend_from_slice(bytes_of(&args));
    Instruction {
        program_id: core_program(),
        accounts: vec![m(first_root, true, true), m(second_root, true, false), m(&SYSTEM, false, false), m(&config_address(), false, true)],
        data,
    }
}

/// Core: one root attests `candidate` as an operator. Two roots doing it completes it.
pub fn core_attest(attester: &Pubkey, attester_attestation: Option<&Pubkey>, candidate: &Pubkey) -> Instruction {
    let mut accounts = vec![
        m(attester, true, true),
        m(&config_address(), false, true),
        m(candidate, false, false),
        m(&core_attestation(candidate), false, true),
        m(&SYSTEM, false, false),
    ];
    accounts.extend(attester_attestation.map(|a| m(a, false, false)));
    Instruction { program_id: core_program(), accounts, data: ATTEST_AUTHORITY.to_vec() }
}

pub fn initialize_market(creator: &Pubkey, mint: &Pubkey, oracle: &Pubkey, nonce: u64, condition_kind: u8, condition_param: u8) -> Instruction {
    let market = market_address(creator, nonce);
    let args = InitializeMarketArgs { nonce, num_outcomes: 2, condition_kind, condition_param, _padding: [0; 5], platform: [0; 32] };
    let mut data = INITIALIZE_MARKET.to_vec();
    data.extend_from_slice(bytes_of(&args));
    let (m0, m1) = (outcome_mint(&market, 0), outcome_mint(&market, 1));
    Instruction {
        program_id: core_program(),
        accounts: vec![
            m(&market, false, true),
            m(&vault_address(&market), false, true),
            m(creator, true, true),
            m(&ata(creator, mint), false, true),
            m(mint, false, false),
            m(&config_address(), false, false),
            m(oracle, false, false),
            m(&SYSTEM, false, false),
            m(&TOKEN, false, false),
            m(&ATA_PROGRAM, false, false),
            m(&m0, false, true),
            m(&ata(creator, &m0), false, true),
            m(&m1, false, true),
            m(&ata(creator, &m1), false, true),
        ],
        data,
    }
}

pub struct Text<'a> {
    pub question: &'a str,
    pub rubric_yes: &'a str,
    pub rubric_no: &'a str,
    pub label_yes: &'a str,
    pub label_no: &'a str,
    pub category: &'a str,
    pub league: &'a str,
}

/// Whether every field fits its on-chain limit, with the reason if not.
pub fn check_text(text: &Text) -> Result<(), String> {
    for (name, s, max) in [
        ("question", text.question, QUESTION_MAX),
        ("rubric_yes", text.rubric_yes, RUBRIC_MAX),
        ("rubric_no", text.rubric_no, RUBRIC_MAX),
        ("label_yes", text.label_yes, LABEL_MAX),
        ("label_no", text.label_no, LABEL_MAX),
        ("category", text.category, CATEGORY_MAX),
        ("league", text.league, LEAGUE_MAX),
    ] {
        if s.is_empty() || s.len() > max {
            return Err(format!("{name} must be 1 to {max} bytes (it is {})", s.len()));
        }
    }
    Ok(())
}

pub fn set_metadata(creator: &Pubkey, market: &Pubkey, text: &Text) -> Result<Instruction, String> {
    check_text(text)?;
    fn put<const N: usize>(s: &str) -> ([u8; N], u16) {
        let mut buf = [0u8; N];
        buf[..s.len()].copy_from_slice(s.as_bytes());
        (buf, s.len() as u16)
    }
    let (question, a) = put::<QUESTION_MAX>(text.question);
    let (rubric_yes, b) = put::<RUBRIC_MAX>(text.rubric_yes);
    let (rubric_no, c) = put::<RUBRIC_MAX>(text.rubric_no);
    let (label_yes, d) = put::<LABEL_MAX>(text.label_yes);
    let (label_no, e) = put::<LABEL_MAX>(text.label_no);
    let (category, f) = put::<CATEGORY_MAX>(text.category);
    let (league, g) = put::<LEAGUE_MAX>(text.league);
    let args = SetMetadataArgs { question, rubric_yes, rubric_no, label_yes, label_no, category, league, lens: [a, b, c, d, e, f, g] };
    let mut data = SET_METADATA.to_vec();
    data.extend_from_slice(bytes_of(&args));
    Ok(Instruction {
        program_id: core_program(),
        accounts: vec![m(creator, true, true), m(market, false, false), m(&metadata_address(market), false, true), m(&SYSTEM, false, false)],
        data,
    })
}

pub fn place_bet(bettor: &Pubkey, market: &Pubkey, mint: &Pubkey, side: u8, deposit: u64, min_shares_out: u64) -> Instruction {
    let args = PlaceBetArgs { deposit_amount: deposit, min_shares_out, outcome_index: side, _padding: [0; 7] };
    let mut data = PLACE_BET.to_vec();
    data.extend_from_slice(bytes_of(&args));
    let out = outcome_mint(market, side);
    Instruction {
        program_id: core_program(),
        accounts: vec![
            m(market, false, true),
            m(&vault_address(market), false, true),
            m(bettor, true, true),
            m(&ata(bettor, mint), false, true),
            m(mint, false, false),
            m(&out, false, true),
            m(&ata(bettor, &out), false, true),
            m(&config_address(), false, false),
            m(&SYSTEM, false, false),
            m(&TOKEN, false, false),
            m(&ATA_PROGRAM, false, false),
        ],
        data,
    }
}

// ── Settlement-side instructions (for rehearsals and manual interventions) ──

use metis_core::settlement::{
    instructions::sweep_fees::{SWEEP_CREATOR, SweepFeesArgs},
    state::{CANCEL_MARKET, CLAIM, CLAIM_REFUND, FORFEIT_CREATOR_FEES, SETTLE_MARKET, SWEEP_FEES},
};

/// A winner cashes in `side`'s shares.
pub fn claim(holder: &Pubkey, market: &Pubkey, mint: &Pubkey, side: u8) -> Instruction {
    holder_ix(CLAIM, holder, market, mint, side)
}

/// A holder of either side's shares is refunded on a cancelled market.
pub fn claim_refund(holder: &Pubkey, market: &Pubkey, mint: &Pubkey, side: u8) -> Instruction {
    holder_ix(CLAIM_REFUND, holder, market, mint, side)
}

fn holder_ix(disc: [u8; 8], holder: &Pubkey, market: &Pubkey, mint: &Pubkey, side: u8) -> Instruction {
    let out = outcome_mint(market, side);
    Instruction {
        program_id: core_program(),
        accounts: vec![
            m(market, false, false),
            m(&vault_address(market), false, true),
            m(holder, true, false),
            m(&ata(holder, mint), false, true),
            m(mint, false, false),
            m(&out, false, true),
            m(&ata(holder, &out), false, true),
            m(&TOKEN, false, false),
        ],
        data: disc.to_vec(),
    }
}

/// The creator collects their own fees. Needs only their signature.
pub fn creator_claim(creator: &Pubkey, market: &Pubkey, mint: &Pubkey) -> Instruction {
    let mut data = SWEEP_FEES.to_vec();
    data.extend_from_slice(bytes_of(&SweepFeesArgs { which: SWEEP_CREATOR, _padding: [0; 7] }));
    Instruction {
        program_id: core_program(),
        accounts: vec![
            m(market, false, true),
            m(&vault_address(market), false, true),
            m(&ata(creator, mint), false, true),
            m(mint, false, false),
            m(&config_address(), false, false),
            m(&TOKEN, false, false),
            m(creator, true, false),
        ],
        data,
    }
}

/// Two roots move a market's unclaimed creator fees to the treasury.
pub fn forfeit(first_root: &Pubkey, second_root: &Pubkey, market: &Pubkey, mint: &Pubkey, treasury: &Pubkey) -> Instruction {
    Instruction {
        program_id: core_program(),
        accounts: vec![
            m(market, false, true),
            m(&vault_address(market), false, true),
            m(&ata(treasury, mint), false, true),
            m(mint, false, false),
            m(&config_address(), false, false),
            m(first_root, true, false),
            m(second_root, true, false),
            m(&TOKEN, false, false),
        ],
        data: FORFEIT_CREATOR_FEES.to_vec(),
    }
}

/// Settle by `signer` (an operator, or anyone once the grace period has passed).
pub fn settle(signer: &Pubkey, market: &Pubkey, oracle: &Pubkey) -> Instruction {
    Instruction {
        program_id: core_program(),
        accounts: vec![
            m(market, false, true),
            m(&vault_address(market), false, false),
            m(oracle, false, false),
            m(&config_address(), false, false),
            m(signer, true, false),
            m(&core_attestation(signer), false, false),
        ],
        data: SETTLE_MARKET.to_vec(),
    }
}

/// Cancel by one signer (an operator, or anyone once the grace period has passed).
pub fn cancel(signer: &Pubkey, market: &Pubkey, oracle: &Pubkey) -> Instruction {
    Instruction {
        program_id: core_program(),
        accounts: vec![
            m(market, false, true),
            m(oracle, false, false),
            m(&config_address(), false, false),
            m(signer, true, false),
            m(&SYSTEM, false, false),
            m(&core_attestation(signer), false, false),
        ],
        data: CANCEL_MARKET.to_vec(),
    }
}

/// One root pauses the market program: nothing that moves money or changes trust runs, except
/// claims, refunds and the two-root cancel.
pub fn pause(root: &Pubkey) -> Instruction {
    Instruction {
        program_id: core_program(),
        accounts: vec![m(root, true, false), m(&config_address(), false, true)],
        data: metis_core::config::state::PAUSE.to_vec(),
    }
}

/// The guardian plus the required roots, all in one transaction, unpause.
pub fn unpause(signers: &[Pubkey]) -> Instruction {
    let mut accounts = vec![m(&signers[0], true, false), m(&config_address(), false, true)];
    accounts.extend(signers[1..].iter().map(|k| m(k, true, false)));
    Instruction { program_id: core_program(), accounts, data: metis_core::config::state::UNPAUSE.to_vec() }
}

// ── Manifest order books ─────────────────────────────────────────────────

fn mp(key: &Pubkey) -> manifest_client::Pubkey {
    manifest_client::Pubkey::new_from_array(key.to_bytes())
}

/// The order book core created for one side of a market.
pub fn book_address(market: &Pubkey, side: u8) -> Pubkey {
    pda(&[metis_core::constants::BOOK_SEED, market.as_ref(), &[side]], &core_program())
}

/// A maker takes a seat on `side`'s book, deposits `base_atoms` of that side's shares and offers
/// them all at `cents` USDC cents per slice. Needs the maker to hold the shares.
pub fn maker_ask(maker: &Pubkey, market: &Pubkey, side: u8, base_atoms: u64, cents: u32) -> Vec<Instruction> {
    use manifest_client::{BatchUpdateParams, DepositParams, OrderType, PlaceOrderParams};
    let book = book_address(market, side);
    let shares = outcome_mint(market, side);
    let batch = BatchUpdateParams::new().add_order(PlaceOrderParams::new(base_atoms, cents, -2, false, OrderType::Limit));
    vec![
        manifest_client::claim_seat_instruction(mp(maker), mp(&book)),
        manifest_client::deposit_instruction(mp(maker), mp(&book), mp(&ata(maker, &shares)), mp(&shares), mp(&TOKEN), DepositParams::new(base_atoms)),
        manifest_client::batch_update_instruction(mp(maker), mp(&book), batch),
    ]
    .into_iter()
    .map(|i| Instruction {
        program_id: Pubkey::new_from_array(i.program_id.to_bytes()),
        accounts: i.accounts.into_iter().map(|a| AccountMeta { pubkey: Pubkey::new_from_array(a.pubkey.to_bytes()), is_signer: a.is_signer, is_writable: a.is_writable }).collect(),
        data: i.data,
    })
    .collect()
}

// ── DBC-backed markets ───────────────────────────────────────────────────

use metis_core::{
    amm::{
        instructions::initialize_market::InitializeBackedMarketArgs,
        state::{ACTIVATE_MARKET, INITIALIZE_BACKED_MARKET},
    },
    settlement::state::{CANCEL_PENDING_MARKET, CLAIM_BACKER},
};

/// Opens a market whose seed a Meteora DBC raise will deliver. It starts Pending. `backer_mint`
/// is the DBC base mint the keeper will launch the pool with; `seed_target` is what the raise must
/// put in the vault (USDC atoms); `raise_deadline` is unix seconds.
#[allow(clippy::too_many_arguments)]
pub fn initialize_backed_market(
    creator: &Pubkey,
    mint: &Pubkey,
    oracle: &Pubkey,
    nonce: u64,
    condition_kind: u8,
    condition_param: u8,
    platform: &Pubkey,
    backer_mint: &Pubkey,
    seed_target: u64,
    raise_deadline: i64,
) -> Instruction {
    let market = market_address(creator, nonce);
    let args = InitializeBackedMarketArgs {
        nonce,
        num_outcomes: 2,
        condition_kind,
        condition_param,
        _padding: [0; 5],
        platform: platform.to_bytes(),
        backer_mint: backer_mint.to_bytes(),
        seed_target,
        raise_deadline,
    };
    let mut data = INITIALIZE_BACKED_MARKET.to_vec();
    data.extend_from_slice(bytes_of(&args));
    Instruction {
        program_id: core_program(),
        accounts: vec![
            m(&market, false, true),
            m(&vault_address(&market), false, true),
            m(creator, true, true),
            m(mint, false, false),
            m(&config_address(), false, false),
            m(oracle, false, false),
            m(&SYSTEM, false, false),
            m(&TOKEN, false, false),
            m(&outcome_mint(&market, 0), false, true),
            m(&outcome_mint(&market, 1), false, true),
        ],
        data,
    }
}

/// An operator opens a Pending market once its raise has put the seed in the vault.
pub fn activate_market(operator: &Pubkey, market: &Pubkey, backer_mint: &Pubkey) -> Instruction {
    let (m0, m1) = (outcome_mint(market, 0), outcome_mint(market, 1));
    Instruction {
        program_id: core_program(),
        accounts: vec![
            m(market, false, true),
            m(&vault_address(market), false, false),
            m(operator, true, true),
            m(&config_address(), false, false),
            m(backer_mint, false, false),
            m(&m0, false, true),
            m(&m1, false, true),
            m(&ata(market, &m0), false, true),
            m(&ata(market, &m1), false, true),
            m(&SYSTEM, false, false),
            m(&TOKEN, false, false),
            m(&ATA_PROGRAM, false, false),
            m(&core_attestation(operator), false, false),
        ],
        data: ACTIVATE_MARKET.to_vec(),
    }
}

/// Gives up on a Pending market (an operator at any time, anyone after the raise deadline).
pub fn cancel_pending_market(signer: &Pubkey, market: &Pubkey) -> Instruction {
    Instruction {
        program_id: core_program(),
        accounts: vec![
            m(market, false, true),
            m(&config_address(), false, false),
            m(signer, true, false),
            m(&core_attestation(signer), false, false),
        ],
        data: CANCEL_PENDING_MARKET.to_vec(),
    }
}

/// A backer turns in all their DBC tokens after the market settled or was cancelled.
pub fn claim_backer(backer: &Pubkey, market: &Pubkey, quote_mint: &Pubkey, backer_mint: &Pubkey) -> Instruction {
    let (m0, m1) = (outcome_mint(market, 0), outcome_mint(market, 1));
    Instruction {
        program_id: core_program(),
        accounts: vec![
            m(market, false, true),
            m(&vault_address(market), false, true),
            m(backer, true, false),
            m(&ata(backer, quote_mint), false, true),
            m(quote_mint, false, false),
            m(backer_mint, false, true),
            m(&ata(backer, backer_mint), false, true),
            m(&m0, false, true),
            m(&m1, false, true),
            m(&ata(market, &m0), false, true),
            m(&ata(market, &m1), false, true),
            m(&TOKEN, false, false),
        ],
        data: CLAIM_BACKER.to_vec(),
    }
}
