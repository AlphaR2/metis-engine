use bytemuck::from_bytes;
use pinocchio::{
    AccountView, Address, ProgramResult,
    cpi::{Seed, Signer, invoke_signed},
    error::ProgramError,
    instruction::{InstructionAccount, InstructionView},
    sysvars::{Sysvar, clock::Clock, rent::Rent},
};
use pinocchio_system::instructions::CreateAccount;
use pinocchio_token::instructions::{AuthorityType, SetAuthority};

use crate::{
    amm::state::{MARKET_DISCRIMINATOR, Market, MarketStatus},
    constants::{BOOK_SEED, MANIFEST_PROGRAM_ID, MARKET_SEED, MAX_OUTCOMES, OUTCOME_MINT_SEED},
    error::MetisError,
    validation,
};

/// Manifest's CreateMarket instruction is index 0 of its instruction enum.
const MANIFEST_CREATE_MARKET: u8 = 0;

/// A Manifest market account is exactly one `MarketFixed` (256 bytes); it grows as orders arrive.
const MANIFEST_MARKET_SIZE: u64 = 256;

/// The Token-2022 program. Manifest wants it present in CreateMarket even when, as here, both
/// mints are classic SPL tokens.
const TOKEN_2022: Address = Address::new_from_array([
    6, 221, 246, 225, 238, 117, 143, 222, 24, 66, 93, 188, 228, 108, 205, 218, 182, 26, 252, 77,
    131, 185, 13, 39, 254, 189, 249, 40, 216, 161, 139, 252,
]);

/// Accounts:
///   0  market          (writable)
///   1  payer           (signer, writable - pays the order books' rent, about 0.01 SOL)
///   2  quote_mint
///   3  outcome_mint_0  (writable)
///   4  outcome_mint_1  (writable)
///   5  book_0          (writable, PDA [book, market, 0] - created here, owned by Manifest)
///   6  book_1          (writable, PDA [book, market, 1])
///   7  book_0_base_vault   (writable, Manifest PDA [vault, book_0, outcome_mint_0])
///   8  book_0_quote_vault  (writable, Manifest PDA [vault, book_0, quote_mint])
///   9  book_1_base_vault   (writable)
///   10 book_1_quote_vault  (writable)
///   11 system_program
///   12 token_program
///   13 token_2022_program
///   14 manifest_program
///   15 config
///   16 operator_authority (optional - the payer's attestation, needed unless the payer is a root)
///
/// The end of the primary window, and the start of trading between holders. An operator (or a root)
/// calls it once `primary_end` has passed, and pays for it. It does two things, atomically:
///   1. revokes the mint and freeze authority on both outcome mints, so the supply is verifiably
///      fixed and the tokens look clean to anyone listing them;
///   2. creates the market's two order books on Manifest - slice/USDC for each outcome - at
///      addresses derived from the market, so nobody has to be told where they are.
pub fn process_close_primary(accounts_mut: &mut [AccountView], _data: &[u8]) -> ProgramResult {
    // Shared references throughout: the CPI below borrows several accounts at once. The one
    // mutable write (the status) happens at the very end, once these are no longer in use.
    let accounts: &[AccountView] = &*accounts_mut;
    let [
        market,
        payer,
        quote_mint,
        outcome_mint_0,
        outcome_mint_1,
        book_0,
        book_1,
        base_vault_0,
        quote_vault_0,
        base_vault_1,
        quote_vault_1,
        system_program,
        token_program,
        token_2022_program,
        manifest_program,
        config,
        rest @ ..,
    ] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    validation::require_signer(payer, MetisError::BettorMustSign)?;
    validation::require_writable(payer, MetisError::AccountNotWritable)?;
    validation::require_writable(market, MetisError::AccountNotWritable)?;
    validation::require_initialized_and_owned(market, &crate::ID, true, MetisError::InvalidMarketAccount)?;
    validation::require_config_account(config, false)?;
    validation::require_not_paused(config)?;
    if rest.len() > 1 {
        return Err(ProgramError::InvalidInstructionData);
    }
    // Gated: the payer must be a root or an attested operator. Nobody's money depends on the books
    // existing, so unlike settle and cancel there is no fallback for anyone else.
    validation::require_operator(payer, rest.first(), config)?;

    if system_program.address() != &pinocchio_system::ID
        || token_program.address() != &pinocchio_token::ID
        || token_2022_program.address() != &TOKEN_2022
        || manifest_program.address() != &MANIFEST_PROGRAM_ID
    {
        return Err(ProgramError::IncorrectProgramId);
    }

    let (market_bump, creator, nonce) = {
        let market_data = market.try_borrow()?;
        let m: &Market = from_bytes(&market_data);
        validation::require_discriminator(m.discriminator, MARKET_DISCRIMINATOR)?;
        if !MarketStatus::try_from(m.status)?.can_transition_to(MarketStatus::Closed) {
            return Err(MetisError::InvalidStatusTransition.into());
        }
        if Clock::get()?.unix_timestamp < m.primary_end {
            return Err(MetisError::PrimaryStillOpen.into());
        }
        validation::require_addresses_match(quote_mint.address(), &m.quote_mint, MetisError::WrongAccountReference)?;
        (m.bump, m.creator, m.nonce)
    };

    let nonce_bytes = nonce.to_le_bytes();
    let bump_seed = [market_bump];
    let market_seeds = [
        Seed::from(MARKET_SEED),
        Seed::from(creator.as_ref()),
        Seed::from(nonce_bytes.as_ref()),
        Seed::from(bump_seed.as_ref()),
    ];

    let mints = [outcome_mint_0, outcome_mint_1];
    let books = [book_0, book_1];
    let base_vaults = [base_vault_0, base_vault_1];
    let quote_vaults = [quote_vault_0, quote_vault_1];

    let rent = Rent::get()?;
    for i in 0..MAX_OUTCOMES {
        let index = [i as u8];

        // The mint must be this market's mint for this outcome - otherwise a caller could point a
        // book at some other token.
        let (mint_pda, _) = Address::derive_program_address(
            &[OUTCOME_MINT_SEED, market.address().as_ref(), &index],
            &crate::ID,
        )
        .ok_or(ProgramError::InvalidSeeds)?;
        validation::require_addresses_match(mints[i].address(), &mint_pda, MetisError::InvalidPda)?;
        validation::require_writable(mints[i], MetisError::AccountNotWritable)?;

        // 1. Fix the supply: nobody - not even this program - can mint or freeze these again.
        SetAuthority::new(mints[i], market, AuthorityType::MintTokens, None)
            .invoke_signed(&[Signer::from(&market_seeds)])?;
        SetAuthority::new(mints[i], market, AuthorityType::FreezeAccount, None)
            .invoke_signed(&[Signer::from(&market_seeds)])?;

        // 2. The order book. Its address is ours to derive; Manifest only needs it to be empty,
        // the right size and owned by Manifest, which is exactly what we create here.
        let (book_pda, book_bump) = Address::derive_program_address(
            &[BOOK_SEED, market.address().as_ref(), &index],
            &crate::ID,
        )
        .ok_or(ProgramError::InvalidSeeds)?;
        validation::require_addresses_match(books[i].address(), &book_pda, MetisError::InvalidPda)?;
        validation::require_writable(books[i], MetisError::AccountNotWritable)?;

        let book_bump_seed = [book_bump];
        let book_seeds = [
            Seed::from(BOOK_SEED),
            Seed::from(market.address().as_ref()),
            Seed::from(index.as_ref()),
            Seed::from(book_bump_seed.as_ref()),
        ];
        CreateAccount {
            from: payer,
            to: books[i],
            lamports: rent.try_minimum_balance(MANIFEST_MARKET_SIZE as usize)?,
            space: MANIFEST_MARKET_SIZE,
            owner: &MANIFEST_PROGRAM_ID,
        }
        .invoke_signed(&[Signer::from(&book_seeds)])?;

        // Manifest's CreateMarket: payer, market, system, base mint, quote mint, the two vaults
        // (which it creates), and both token programs. The payer's signature carries through.
        let data = [MANIFEST_CREATE_MARKET];
        let instruction = InstructionView {
            program_id: &MANIFEST_PROGRAM_ID,
            data: &data,
            accounts: &[
                InstructionAccount::writable_signer(payer.address()),
                InstructionAccount::writable(books[i].address()),
                InstructionAccount::readonly(system_program.address()),
                InstructionAccount::readonly(mints[i].address()),
                InstructionAccount::readonly(quote_mint.address()),
                InstructionAccount::writable(base_vaults[i].address()),
                InstructionAccount::writable(quote_vaults[i].address()),
                InstructionAccount::readonly(token_program.address()),
                InstructionAccount::readonly(token_2022_program.address()),
            ],
        };
        invoke_signed(
            &instruction,
            &[
                payer,
                books[i],
                system_program,
                mints[i],
                quote_mint,
                base_vaults[i],
                quote_vaults[i],
                token_program,
                token_2022_program,
            ],
            &[],
        )?;
    }

    let mut market_data = accounts_mut[0].try_borrow_mut()?;
    let m: &mut Market = bytemuck::from_bytes_mut(&mut market_data);
    m.status = MarketStatus::Closed as u8;
    Ok(())
}
