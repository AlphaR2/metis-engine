use bytemuck::{Pod, Zeroable, from_bytes, from_bytes_mut};
use pinocchio::{
    AccountView, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
};
use pinocchio_token::{instructions::TransferChecked, state::Account};

use crate::{
    amm::state::{MARKET_DISCRIMINATOR, Market, MarketStatus},
    config::state::Config,
    constants::{MARKET_SEED, QUOTE_DECIMALS},
    error::MetisError,
    validation,
};

pub const SWEEP_PROTOCOL: u8 = 0;
pub const SWEEP_CREATOR: u8 = 1;
pub const SWEEP_PLATFORM: u8 = 2;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct SweepFeesArgs {
    /// SWEEP_PROTOCOL (the protocol's share of the revenue, to Config.treasury),
    /// SWEEP_CREATOR (the creator's share, to the market's creator) or
    /// SWEEP_PLATFORM (the platform's share, to the platform named when the market was created).
    pub which: u8,
    pub _padding: [u8; 7],
}

const _: () = assert!(core::mem::size_of::<SweepFeesArgs>() == 8);

/// Accounts:
///   0 market        (writable)
///   1 vault         (writable)
///   2 recipient_ata (writable - the treasury's or the creator's USDC account)
///   3 quote_mint
///   4 config
///   5 token_program
///   6 signer        (the protocol's sweep needs an operator or root; the creator's needs nobody)
///   7 operator_authority (optional - the signer's attestation, needed unless the signer is a root)
///
/// Pays out what settlement recorded as owed. The two payments are separate calls on purpose: a
/// creator who closes their token account can only stall their own share, never the protocol's,
/// and neither can touch a winner's claim.
///
/// The protocol's sweep is gated to operators: it is the protocol's housekeeping. The creator's and
/// the platform's are not: each is its owner collecting their own money, and the only place it can go is their own
/// token account, so nobody's permission is needed (or wanted).
pub fn process_sweep_fees(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    if data.len() != core::mem::size_of::<SweepFeesArgs>() {
        return Err(ProgramError::InvalidInstructionData);
    }
    let args: &SweepFeesArgs = from_bytes(data);

    let [market, vault, recipient_ata, quote_mint, config, token_program, signer, rest @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if rest.len() > 1 {
        return Err(ProgramError::InvalidInstructionData);
    }
    if args.which == SWEEP_PROTOCOL {
        validation::require_operator(signer, rest.first(), config)?;
    }

    validation::require_writable(market, MetisError::AccountNotWritable)?;
    validation::require_writable(vault, MetisError::AccountNotWritable)?;
    validation::require_writable(recipient_ata, MetisError::AccountNotWritable)?;
    validation::require_initialized_and_owned(market, &crate::ID, true, MetisError::InvalidMarketAccount)?;
    validation::require_config_account(config, false)?;
    validation::require_not_paused(config)?;
    if token_program.address() != &pinocchio_token::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    let treasury = {
        let data = config.try_borrow()?;
        validation::unpack::<Config>(&data, MetisError::InvalidConfigAccount)?.treasury
    };

    let (amount, expected_owner, market_bump, creator, nonce) = {
        let mut market_data = market.try_borrow_mut()?;
        let m: &mut Market = from_bytes_mut(&mut market_data);
        validation::require_discriminator(m.discriminator, MARKET_DISCRIMINATOR)?;
        // A cancelled market refunds fees along with everything else, so nothing is owed on it.
        if m.status != MarketStatus::Settled as u8 {
            return Err(MetisError::MarketNotSettled.into());
        }
        validation::require_addresses_match(vault.address(), &m.vault, MetisError::WrongAccountReference)?;
        validation::require_addresses_match(quote_mint.address(), &m.quote_mint, MetisError::WrongAccountReference)?;

        let (owed, owner) = match args.which {
            SWEEP_PROTOCOL => (&mut m.protocol_owed, treasury),
            SWEEP_CREATOR => (&mut m.creator_owed, m.creator),
            SWEEP_PLATFORM => (&mut m.platform_owed, m.platform),
            _ => return Err(ProgramError::InvalidInstructionData),
        };
        let amount = *owed;
        if amount == 0 {
            return Err(MetisError::NothingToSweep.into());
        }
        // Zeroed before the transfer: a failed transfer reverts this too, and a repeat call
        // can never pay twice.
        *owed = 0;
        (amount, owner, m.bump, m.creator, m.nonce)
    };

    {
        let ata = Account::from_account_view(recipient_ata)?;
        validation::require_addresses_match(ata.mint(), quote_mint.address(), MetisError::WrongAccountReference)?;
        validation::require_addresses_match(ata.owner(), &expected_owner, MetisError::WrongAccountReference)?;
    }

    let nonce_bytes = nonce.to_le_bytes();
    let bump_seed = [market_bump];
    let market_seeds = [
        Seed::from(MARKET_SEED),
        Seed::from(creator.as_ref()),
        Seed::from(nonce_bytes.as_ref()),
        Seed::from(bump_seed.as_ref()),
    ];
    TransferChecked::new(vault, quote_mint, recipient_ata, market, amount, QUOTE_DECIMALS)
        .invoke_signed(&[Signer::from(&market_seeds)])
}
