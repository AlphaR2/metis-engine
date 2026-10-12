//! `initialize_backed_market`: opens a market whose opening pot will be raised by a crowd through a
//! Meteora Dynamic Bonding Curve (DBC) pool, instead of being paid by its creator.
//!
//! This is the DBC-specific entry point only. The market, the vault and the outcome mints are created
//! by the same shared code as a creator-seeded market (`amm::instructions::initialize_market`), so the
//! engine has a single creation path; what differs is that the market starts `Pending`, with a seed
//! target, a deadline and the backer token recorded.

use bytemuck::bytes_of;
use pinocchio::{AccountView, Address, ProgramResult, error::ProgramError};

pub use metis_dbc::InitializeBackedMarketArgs;

use crate::amm::instructions::initialize_market::{Accounts, Backed, Common, create_market};

/// Accounts:
///   0 market          (writable, PDA)
///   1 vault           (writable, PDA token account; stays empty until the raise funds it)
///   2 creator         (signer, writable - pays rent only; the seed comes from the raise)
///   3 quote_mint
///   4 config
///   5 oracle
///   6 system_program
///   7 token_program
///   8 outcome_mint_0  (writable, PDA)
///   9 outcome_mint_1  (writable, PDA)
///
/// Opens a market whose seed will be raised from backers through a Meteora DBC pool, instead of
/// paid by its creator. It is created `Pending`: no seed shares exist and nobody can bet until
/// activate_market runs. Needs Config.dbc_backing_enabled.
pub fn process_initialize_backed_market(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let args = InitializeBackedMarketArgs::load(data)?;
    let [
        market,
        vault,
        creator,
        quote_mint,
        config,
        oracle,
        system_program,
        token_program,
        outcome_mint_0,
        outcome_mint_1,
    ] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let common = Common {
        nonce: args.nonce,
        num_outcomes: args.num_outcomes,
        condition_kind: args.condition_kind,
        condition_param: args.condition_param,
        platform: args.platform,
    };
    let accts = Accounts {
        market,
        vault,
        creator,
        quote_mint,
        config,
        oracle,
        system_program,
        token_program,
        outcome_mints: [&*outcome_mint_0, &*outcome_mint_1],
    };
    let (state, _) = create_market(
        &accts,
        &common,
        Some(Backed {
            backer_mint: Address::new_from_array(args.backer_mint),
            seed_target: args.seed_target,
            raise_deadline: args.raise_deadline,
        }),
    )?;
    market.try_borrow_mut()?.copy_from_slice(bytes_of(&state));
    Ok(())
}
