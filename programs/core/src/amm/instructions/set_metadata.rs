use bytemuck::{Pod, Zeroable, bytes_of, from_bytes};
use pinocchio::{
    AccountView, Address, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{Sysvar, rent::Rent},
};
use pinocchio_system::instructions::CreateAccount;

use crate::{
    amm::{
        metadata::{
            CATEGORY_MAX, LABEL_MAX, LEAGUE_MAX, MARKET_METADATA_DISCRIMINATOR, MarketMetadata,
            QUESTION_MAX, RUBRIC_MAX, valid_text,
        },
        state::{MARKET_DISCRIMINATOR, Market},
    },
    constants::METADATA_SEED,
    error::MetisError,
    validation,
};

/// The text, as the creator wrote it. `lens` says how many bytes of each buffer are used, in the
/// order question, rubric_yes, rubric_no, label_yes, label_no, category, league.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct SetMetadataArgs {
    pub question: [u8; QUESTION_MAX],
    pub rubric_yes: [u8; RUBRIC_MAX],
    pub rubric_no: [u8; RUBRIC_MAX],
    pub label_yes: [u8; LABEL_MAX],
    pub label_no: [u8; LABEL_MAX],
    pub category: [u8; CATEGORY_MAX],
    pub league: [u8; LEAGUE_MAX],
    pub lens: [u16; 7],
}

impl SetMetadataArgs {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn load(bytes: &[u8]) -> Result<&Self, ProgramError> {
        if bytes.len() != Self::LEN {
            return Err(ProgramError::InvalidInstructionData);
        }
        Ok(from_bytes::<Self>(bytes))
    }
}

/// Accounts:
///   0 creator   (signer, writable - the market's creator, pays the rent)
///   1 market
///   2 metadata  (writable, PDA [metadata, market] - must not exist yet)
///   3 system_program
///
/// Written once, by the creator, and the PDA can't be created twice - so the question and
/// rubrics a bettor read when they bet are the ones settlement is judged against. Kept out of
/// initialize_market because the text doesn't fit in a transaction beside everything else
/// that instruction needs.
pub fn process_set_metadata(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let args = SetMetadataArgs::load(data)?;

    let [creator, market, metadata, system_program] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    validation::require_signer(creator, MetisError::CreatorMustSign)?;
    validation::require_writable(creator, MetisError::AccountNotWritable)?;
    validation::require_writable(metadata, MetisError::AccountNotWritable)?;
    validation::require_initialized_and_owned(market, &crate::ID, false, MetisError::InvalidMarketAccount)?;
    if system_program.address() != &pinocchio_system::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    {
        let market_data = market.try_borrow()?;
        let market_state: &Market = from_bytes(&market_data);
        validation::require_discriminator(market_state.discriminator, MARKET_DISCRIMINATOR)?;
        validation::require_addresses_match(creator.address(), &market_state.creator, MetisError::UnauthorizedAuthority)?;
    }

    // Every field must be real text of a sane length. Checked per field, with its own cap.
    let fields: [(&[u8], usize); 7] = [
        (&args.question, QUESTION_MAX),
        (&args.rubric_yes, RUBRIC_MAX),
        (&args.rubric_no, RUBRIC_MAX),
        (&args.label_yes, LABEL_MAX),
        (&args.label_no, LABEL_MAX),
        (&args.category, CATEGORY_MAX),
        (&args.league, LEAGUE_MAX),
    ];
    for (i, (buf, max)) in fields.iter().enumerate() {
        let used = args.lens[i] as usize;
        let text = buf.get(..used).ok_or(MetisError::InvalidMetadata)?;
        if !valid_text(text, *max) {
            return Err(MetisError::InvalidMetadata.into());
        }
    }

    let seeds: [&[u8]; 2] = [METADATA_SEED, market.address().as_ref()];
    let (pda, bump) = Address::derive_program_address(&seeds, &crate::ID).ok_or(ProgramError::InvalidSeeds)?;
    validation::require_addresses_match(metadata.address(), &pda, MetisError::InvalidPda)?;

    let bump_seed = [bump];
    let signer_seeds = [
        Seed::from(METADATA_SEED),
        Seed::from(market.address().as_ref()),
        Seed::from(bump_seed.as_ref()),
    ];
    // CreateAccount fails if the account already holds lamports or data, which is what makes
    // this one-shot: a second call can't create it again.
    CreateAccount {
        from: creator,
        to: metadata,
        lamports: Rent::get()?.try_minimum_balance(MarketMetadata::LEN)?,
        space: MarketMetadata::LEN as u64,
        owner: &crate::ID,
    }
    .invoke_signed(&[Signer::from(&signer_seeds)])?;

    let record = MarketMetadata {
        discriminator: MARKET_METADATA_DISCRIMINATOR,
        market: *market.address(),
        question: args.question,
        rubric_yes: args.rubric_yes,
        rubric_no: args.rubric_no,
        label_yes: args.label_yes,
        label_no: args.label_no,
        category: args.category,
        league: args.league,
        lens: args.lens,
    };
    metadata.try_borrow_mut()?.copy_from_slice(bytes_of(&record));
    Ok(())
}
