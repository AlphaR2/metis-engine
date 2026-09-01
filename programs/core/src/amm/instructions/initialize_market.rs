use bytemuck::{Pod, Zeroable, bytes_of, from_bytes};
use pinocchio::{
    AccountView, Address, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{Sysvar, clock::Clock, rent::Rent},
};
use pinocchio_associated_token_account::instructions::CreateIdempotent;
use pinocchio_system::instructions::CreateAccount;
use pinocchio_token::{
    instructions::{InitializeAccount3, InitializeMint2, MintTo, TransferChecked},
    state::{Account, Mint},
};

use crate::{
    amm::state::{MARKET_DISCRIMINATOR, Market, MarketStatus},
    config::state::{CONFIG_DISCRIMINATOR, Config},
    constants::{
        AUTHORITY, CONFIG_SEED, INITIAL_CONFIG_ADMIN, MARKET_SEED, MAX_OUTCOMES, OUTCOME_MINT_SEED,
        OUTCOME_UNRESOLVED, PRIMARY_WINDOW_SECS, QUOTE_DECIMALS, VAULT_SEED,
    },
    error::OpenStakeError,
    validation,
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct InitializeMarketArgs {
    /// Creator-chosen number that makes this market's address unique, so one
    /// creator can open many markets.
    pub nonce: u64,
    /// When the real-world event STARTS (kickoff), as a unix timestamp.
    /// Not when it ends. Betting stops PRIMARY_WINDOW_SECS before this.
    pub bet_close_ts: i64,
    pub num_outcomes: u8,
    pub _padding: [u8; 7],
}

impl InitializeMarketArgs {
    pub const LEN: usize = core::mem::size_of::<InitializeMarketArgs>();

    pub fn load(bytes: &[u8]) -> Result<&Self, ProgramError> {
        if bytes.len() != Self::LEN {
            return Err(ProgramError::InvalidInstructionData);
        }
        Ok(from_bytes::<Self>(bytes))
    }
}

const _: () = assert!(core::mem::size_of::<InitializeMarketArgs>() == 24);

// The account list below hardcodes one [mint, ata] pair per outcome. If
// MAX_OUTCOMES ever goes up, this file must be edited by hand: add the extra
// accounts to the pattern AND to the two arrays built from them. This assert
// makes forgetting a compile error instead of a market with missing mints.
const _: () = assert!(MAX_OUTCOMES == 2);

/// Accounts:
///   0 market            (writable, PDA)
///   1 vault             (writable, PDA token account, holds the USDC)
///   2 creator           (signer, writable)
///   3 creator_quote_ata (writable, the USDC account paying the creation stake)
///   4 quote_mint        (the USDC mint)
///   5 config
///   6 system_program
///   7 token_program
///   8 ata_program
///
/// Two accounts per outcome follow. This list is sized for MAX_OUTCOMES == 2
/// and must be extended by hand if that ever changes (see the assert above).
///   9  outcome_mint_0    (writable, PDA)
///   10 creator_ata_0     (writable, where the creator's outcome 0 shares land)
///   11 outcome_mint_1    (writable, PDA)
///   12 creator_ata_1     (writable, where the creator's outcome 1 shares land)
pub fn process_initialize_market(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let args = InitializeMarketArgs::load(data)?;

    // Every account is named explicitly rather than using a catch-all, so the
    // count is checked by the compiler and the list reads top to bottom.
    let [
        market,
        vault,
        creator,
        creator_quote_ata,
        quote_mint,
        config,
        system_program,
        token_program,
        ata_program,
        outcome_mint_0,
        creator_ata_0,
        outcome_mint_1,
        creator_ata_1,
    ] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // Put the per-outcome accounts into arrays so the loop further down can
    // just index by outcome number instead of repeating itself per outcome.
    let outcome_mints = [outcome_mint_0, outcome_mint_1];
    let creator_atas = [creator_ata_0, creator_ata_1];

    // The creator signs and pays for everything created here.
    validation::require_signer(creator)?;
    validation::require_writable(creator)?;
    validation::require_writable(market)?;
    validation::require_writable(vault)?;
    validation::require_writable(creator_quote_ata)?;
    // Mints get created and minted from; the ATAs get created and credited.
    for i in 0..MAX_OUTCOMES {
        validation::require_writable(outcome_mints[i])?;
        validation::require_writable(creator_atas[i])?;
    }

    // Make sure each "program" account really is the program we think it is,
    // otherwise a caller could point us at a lookalike that behaves differently.
    if system_program.address() != &pinocchio_system::ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    if token_program.address() != &pinocchio_token::ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    if ata_program.address() != &pinocchio_associated_token_account::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    // Config holds the protocol-wide settings every market must follow.
    // We rebuild its address ourselves so a caller can't hand us a fake one.
    let config_seeds: [&[u8]; 2] = [CONFIG_SEED, INITIAL_CONFIG_ADMIN.as_ref()];
    let (config_pda, _) = Address::derive_program_address(&config_seeds, &crate::ID)
        .ok_or(ProgramError::InvalidSeeds)?;

    validation::require_addresses_match(config.address(), &config_pda)?;
    validation::require_initialized_and_owned(config, &crate::ID, false)?;

    // Read the three settings this instruction needs, then drop the borrow.
    //   market_init_fee - what the creator stakes to open the market
    //   max_outcomes    - admin-set ceiling on outcomes per market
    //   quote_mint      - the only stablecoin markets may be priced in
    let (market_init_fee, max_outcomes, cfg_quote_mint) = {
        let config_data = config.try_borrow()?;
        let config_state: &Config = from_bytes(&config_data);
        validation::require_discriminator(config_state.discriminator, CONFIG_DISCRIMINATOR)?;
        (
            config_state.market_init_fee,
            config_state.max_outcomes,
            config_state.quote_mint,
        )
    };

    // Anyone can create a market, so without this check a creator could price
    // their market in a worthless token they minted themselves.
    validation::require_addresses_match(quote_mint.address(), &cfg_quote_mint)?;

    // Outcomes must be at least 2: a one-outcome market has a guaranteed
    // winner and nothing to price. The upper bound is whichever is stricter,
    // the admin's setting or the array size compiled into Market.
    let set_outcome_size = args.num_outcomes as usize;

    if set_outcome_size < 2
        || set_outcome_size > max_outcomes as usize
        || set_outcome_size > MAX_OUTCOMES
    {
        return Err(ProgramError::InvalidInstructionData);
    }

    // Kickoff must be at least PRIMARY_WINDOW_SECS away.
    //
    // Betting closes that long BEFORE kickoff, so if someone opened a market
    // 2 hours before a 5-hour window, primary_end would land in the past and
    // the market would be born already closed - stake locked, nobody able to
    // bet. checked_sub because bet_close_ts comes from the caller and a huge
    // negative value would otherwise overflow.
    let now = Clock::get()?.unix_timestamp;

    if args
        .bet_close_ts
        .checked_sub(now)
        .ok_or(ProgramError::ArithmeticOverflow)?
        < PRIMARY_WINDOW_SECS
    {
        return Err(ProgramError::InvalidInstructionData);
    }

    let primary_end = args
        .bet_close_ts
        .checked_sub(PRIMARY_WINDOW_SECS)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    {
        // The curve counts shares in millionths, same as USDC. A mint with
        // different decimals would silently throw every price off, so reject
        // it here rather than find out later.
        let quote = Mint::from_account_view(quote_mint)?;

        if quote.decimals() != QUOTE_DECIMALS {
            return Err(ProgramError::InvalidAccountData);
        }
    }

    // Work out the addresses of the accounts we're about to create. Deriving
    // them here (rather than trusting what was passed in) is what proves the
    // caller handed us the real accounts for this market.
    //
    // Market PDA: [market, creator, nonce]
    let nonce_bytes = args.nonce.to_le_bytes();
    let market_seeds: [&[u8]; 3] = [MARKET_SEED, creator.address().as_ref(), &nonce_bytes];
    let (market_pda, market_bump) = Address::derive_program_address(&market_seeds, &crate::ID)
        .ok_or(ProgramError::InvalidSeeds)?;
    validation::require_addresses_match(market.address(), &market_pda)?;

    // Vault PDA: [vault, market]
    let vault_seeds: [&[u8]; 2] = [VAULT_SEED, market.address().as_ref()];
    let (vault_pda, vault_bump) = Address::derive_program_address(&vault_seeds, &crate::ID)
        .ok_or(ProgramError::InvalidSeeds)?;
    validation::require_addresses_match(vault.address(), &vault_pda)?;

    let rent = Rent::get()?;
    let market_bump_seed = [market_bump];
    let vault_bump_seed = [vault_bump];

    // A PDA has no private key, so this is how it "signs": we hand the runtime
    // the seeds it was built from. Reused below whenever the market itself
    // needs to authorise something, e.g. minting outcome tokens.
    let market_signer_seeds = [
        Seed::from(MARKET_SEED),
        Seed::from(creator.address().as_ref()),
        Seed::from(nonce_bytes.as_ref()),
        Seed::from(market_bump_seed.as_ref()),
    ];

    // The market account itself: holds the curve state and everything else
    // this program needs to remember.
    CreateAccount {
        from: creator,
        to: market,
        lamports: rent.try_minimum_balance(core::mem::size_of::<Market>())?,
        space: core::mem::size_of::<Market>() as u64,
        owner: &crate::ID,
    }
    .invoke_signed(&[Signer::from(&market_signer_seeds)])?;

    // The vault: an ordinary SPL token account that holds every dollar staked
    // on this market. It's owned by the Token program (not us), with the market
    // PDA as its authority, so only this program can move money out of it.
    //
    // Rent is paid in SOL and sits outside the token balance, so the vault's
    // amount is pure USDC. That's why the solvency check elsewhere is a plain
    // "balance >= total_collected" with no rent term to subtract.
    let vault_signer_seeds = [
        Seed::from(VAULT_SEED),
        Seed::from(market.address().as_ref()),
        Seed::from(vault_bump_seed.as_ref()),
    ];

    CreateAccount {
        from: creator,
        to: vault,
        lamports: rent.try_minimum_balance(Account::LEN)?,
        space: Account::LEN as u64,
        owner: &pinocchio_token::ID,
    }
    .invoke_signed(&[Signer::from(&vault_signer_seeds)])?;

    // CreateAccount only allocated raw space; this tells the Token program the
    // account is a USDC account controlled by the market.
    InitializeAccount3::new(vault, quote_mint, &market_pda).invoke()?;

    // The market can't open at zero shares - the pricing formula divides by the
    // size of the share vector, which would be division by zero. So the
    // creator's stake is split evenly across every outcome, giving the market a
    // real starting shape with no outcome favoured.
    let seed_per_side = crate::amm::curve::symmetric_seed(market_init_fee, args.num_outcomes);

    // A stake too small to produce even one share per side would still leave us
    // at zero, so refuse rather than open a broken market.
    if seed_per_side == 0 {
        return Err(OpenStakeError::PaymentTooSmall.into());
    }

    // Fill one slot per real outcome; any unused slots stay at 0.
    let mut outcome_shares = [0u64; MAX_OUTCOMES];

    for slot in outcome_shares.iter_mut().take(set_outcome_size) {
        *slot = seed_per_side;
    }

    // Now give each outcome its own token. One SPL mint per outcome means a
    // position is just a token balance - transferable, tradeable on any
    // exchange, and burnable at settlement.
    for i in 0..set_outcome_size {
        let outcome_mint = &outcome_mints[i];
        let creator_outcome_ata = &creator_atas[i];

        // Each mint's address is derived from the market, which in itself is unique, plus the outcome
        // number, so outcome 0 and outcome 1 can never collide, and no other
        // market's mint can be passed in here.
        let index_byte = [i as u8];
        let mint_seeds: [&[u8]; 3] = [OUTCOME_MINT_SEED, market.address().as_ref(), &index_byte];

        let (mint_pda, mint_bump) = Address::derive_program_address(&mint_seeds, &crate::ID)
            .ok_or(ProgramError::InvalidSeeds)?;

        validation::require_addresses_match(outcome_mint.address(), &mint_pda)?;

        let mint_bump_seed = [mint_bump];

        // The mint signs its own creation, same PDA trick as the market above.
        let mint_signer_seeds = [
            Seed::from(OUTCOME_MINT_SEED),
            Seed::from(market.address().as_ref()),
            Seed::from(index_byte.as_ref()),
            Seed::from(mint_bump_seed.as_ref()),
        ];

        // Allocate space for the mint, owned by the Token program.
        CreateAccount {
            from: creator,
            to: outcome_mint,
            lamports: rent.try_minimum_balance(Mint::LEN)?,
            space: Mint::LEN as u64,
            owner: &pinocchio_token::ID,
        }
        .invoke_signed(&[Signer::from(&mint_signer_seeds)])?;

        // Turn that space into a real mint.
        //
        // Decimals match USDC so share amounts and dollar amounts use the same
        // scale. The market is mint authority (only it can create shares) and
        // also freeze authority - nothing uses freezing yet, but a mint made
        // without one can NEVER be given one later, so we keep the option open.
        InitializeMint2::new(outcome_mint, QUOTE_DECIMALS, &market_pda, Some(&market_pda))
            .invoke()?;

        // Create token account for each outcome mint. The creator needs somewhere to hold these shares. "Idempotent" means
        // it quietly does nothing if the account already exists.
        CreateIdempotent {
            funding_account: creator,
            account: creator_outcome_ata,
            wallet: creator,
            mint: outcome_mint,
            system_program,
            token_program,
        }
        .invoke()?;

        // Hand the creator their seed shares. This is what makes the stake a
        // real position rather than a fee: the creator holds outcome tokens
        // like any other bettor and gets paid out the same way if they win.
        MintTo::new(outcome_mint, creator_outcome_ata, market, seed_per_side)
            .invoke_signed(&[Signer::from(&market_signer_seeds)])?;
    }

    // Everything this program needs to remember about the market. Note
    // total_collected starts at the creation stake, which matches exactly what
    // gets transferred into the vault below - the two must always agree or the
    // solvency check at settlement will fail.
    let market_state = Market {
        outcome_shares,
        total_collected: market_init_fee,
        bet_close_ts: args.bet_close_ts,
        primary_end,
        nonce: args.nonce,
        creator: *creator.address(),
        authority: AUTHORITY,
        vault: *vault.address(),
        quote_mint: *quote_mint.address(),
        discriminator: MARKET_DISCRIMINATOR,
        num_outcomes: args.num_outcomes,
        status: MarketStatus::Open as u8,
        winning_outcome: OUTCOME_UNRESOLVED,
        bump: market_bump,
        _padding: [0u8; 4],
    };
    market
        .try_borrow_mut()?
        .copy_from_slice(bytes_of(&market_state));

    // Money moves last, once every account exists and the state is written.

    TransferChecked::new(
        creator_quote_ata,
        quote_mint,
        vault,
        creator,
        market_init_fee,
        QUOTE_DECIMALS,
    )
    .invoke()?;

    Ok(())
}
