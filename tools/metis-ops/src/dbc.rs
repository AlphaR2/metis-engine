//! Open Pool: the operator-side builders for a DBC-backed market (a market whose opening pot a crowd raises on a
//! Meteora bonding curve). See `dbc/README.md` for the whole flow.
//!
//! Instruction layout comes from the `metis-dbc` crate, discriminators from `metis_core::dbc`.

use super::*;

use metis_core::dbc::{ACTIVATE_MARKET, CANCEL_PENDING_MARKET, CLAIM_BACKER, INITIALIZE_BACKED_MARKET};
use metis_dbc::InitializeBackedMarketArgs;

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
