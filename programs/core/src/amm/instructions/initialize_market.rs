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
    amm::curve::virtual_liquidity_per_side,
    amm::{
        condition,
        state::{MARKET_DISCRIMINATOR, Market, MarketStatus, SeedSource},
    },
    config::state::{CONFIG_DISCRIMINATOR, Config},
    constants::{
        MARKET_SEED, MAX_BACKED_SEED, MAX_OUTCOMES, MAX_RAISE_SECS, MIN_RAISE_SECS, MIN_SEED,
        OUTCOME_MINT_SEED, OUTCOME_UNRESOLVED, PRIMARY_WINDOW_SECS, QUOTE_DECIMALS, VAULT_SEED,
    },
    error::MetisError,
    validation,
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct InitializeMarketArgs {
    /// Creator-chosen number that makes this market's address unique, so one
    /// creator can open many markets.
    pub nonce: u64,
    pub num_outcomes: u8,
    /// What makes YES win - see amm/condition.rs. The creator's rubric (written separately by
    /// set_metadata) must say the same thing in words.
    pub condition_kind: u8,
    pub condition_param: u8,
    pub _padding: [u8; 5],
    /// The product this market is created through, which earns the platform's share of its
    /// revenue. All zeroes for none (the share then stays with the protocol). Not a signer: it is
    /// only a payee, and a creator may name themselves.
    pub platform: [u8; 32],
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

const _: () = assert!(core::mem::size_of::<InitializeMarketArgs>() == 48);

// The account list below hardcodes one [mint, ata] pair per outcome. If
// MAX_OUTCOMES ever goes up, this file must be edited by hand: add the extra
// accounts to the pattern AND to the two arrays built from them. This assert
// makes forgetting a compile error instead of a market with missing mints.
const _: () = assert!(MAX_OUTCOMES == 2);

/// Arguments of `initialize_backed_market`: the common ones, then what the DBC raise needs.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct InitializeBackedMarketArgs {
    pub nonce: u64,
    pub num_outcomes: u8,
    pub condition_kind: u8,
    pub condition_param: u8,
    pub _padding: [u8; 5],
    pub platform: [u8; 32],
    /// The DBC base mint that backers hold. It need not exist yet (the keeper may create the pool
    /// after the market); activate_market checks it for real.
    pub backer_mint: [u8; 32],
    /// What the raise must deliver into the vault, in USDC atoms.
    pub seed_target: u64,
    /// Unix seconds. The raise must activate the market by then.
    pub raise_deadline: i64,
}

impl InitializeBackedMarketArgs {
    pub const LEN: usize = core::mem::size_of::<InitializeBackedMarketArgs>();

    pub fn load(bytes: &[u8]) -> Result<&Self, ProgramError> {
        if bytes.len() != Self::LEN {
            return Err(ProgramError::InvalidInstructionData);
        }
        Ok(from_bytes::<Self>(bytes))
    }
}

const _: () = assert!(core::mem::size_of::<InitializeBackedMarketArgs>() == 96);

// The account list below hardcodes one [mint, ata] pair per outcome. If
// MAX_OUTCOMES ever goes up, this file must be edited by hand: add the extra
// accounts to the pattern AND to the two arrays built from them. This assert
// makes forgetting a compile error instead of a market with missing mints.
const _: () = assert!(MAX_OUTCOMES == 2);

/// What differs between a creator-seeded market and a DBC-backed one.
struct Backed {
    backer_mint: Address,
    seed_target: u64,
    raise_deadline: i64,
}

/// The arguments both variants share.
struct Common {
    nonce: u64,
    num_outcomes: u8,
    condition_kind: u8,
    condition_param: u8,
    platform: [u8; 32],
}

/// The accounts both variants create or read.
struct Accounts<'a> {
    market: &'a AccountView,
    vault: &'a AccountView,
    creator: &'a AccountView,
    quote_mint: &'a AccountView,
    config: &'a AccountView,
    oracle: &'a AccountView,
    system_program: &'a AccountView,
    token_program: &'a AccountView,
    outcome_mints: [&'a AccountView; MAX_OUTCOMES],
}

/// What the creator-seeded variant needs back from the shared part to finish its own work.
struct Created {
    seed_per_side: u64,
    seed_usdc: u64,
}

/// Accounts:
///   0 market            (writable, PDA)
///   1 vault             (writable, PDA token account, holds the USDC)
///   2 creator           (signer, writable)
///   3 creator_quote_ata (writable, the USDC account paying the creation stake)
///   4 quote_mint        (the USDC mint)
///   5 config
///   6 oracle            (the event this market settles against: a real, still-scheduled
///                         football oracle. Its kickoff becomes the market's kickoff.)
///   7 system_program
///   8 token_program
///   9 ata_program
///
/// Two accounts per outcome follow. This list is sized for MAX_OUTCOMES == 2
/// and must be extended by hand if that ever changes (see the assert above).
///   10 outcome_mint_0    (writable, PDA)
///   11 creator_ata_0     (writable, where the creator's outcome 0 shares land)
///   12 outcome_mint_1    (writable, PDA)
///   13 creator_ata_1     (writable, where the creator's outcome 1 shares land)
///
/// There is no per-market delegate any more: settlement reads the oracle and anyone may run
/// it, so no key needs to be trusted per market.
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
        oracle,
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
    let creator_atas = [&*creator_ata_0, &*creator_ata_1];

    validation::require_writable(creator_quote_ata, MetisError::AccountNotWritable)?;
    for ata in creator_atas {
        validation::require_writable(ata, MetisError::AccountNotWritable)?;
    }
    if ata_program.address() != &pinocchio_associated_token_account::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    let outcome_mints = [&*outcome_mint_0, &*outcome_mint_1];
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
        outcome_mints,
    };
    let (state, created) = create_market(&accts, &common, None)?;
    market.try_borrow_mut()?.copy_from_slice(bytes_of(&state));

    // The creator's seed shares, minted to them: this is what makes the stake a real position
    // rather than a fee. They hold outcome tokens like any other bettor and are paid out the
    // same way if they win.
    let nonce_bytes = args.nonce.to_le_bytes();
    let market_bump_seed = [state.bump];
    let market_signer_seeds = [
        Seed::from(MARKET_SEED),
        Seed::from(creator.address().as_ref()),
        Seed::from(nonce_bytes.as_ref()),
        Seed::from(market_bump_seed.as_ref()),
    ];
    for i in 0..args.num_outcomes as usize {
        // Create token account for each outcome mint. "Idempotent" means it quietly does nothing
        // if the account already exists.
        CreateIdempotent {
            funding_account: creator,
            account: creator_atas[i],
            wallet: creator,
            mint: outcome_mints[i],
            system_program,
            token_program,
        }
        .invoke()?;
        MintTo::new(outcome_mints[i], creator_atas[i], market, created.seed_per_side)
            .invoke_signed(&[Signer::from(&market_signer_seeds)])?;
    }

    // Money moves last, once every account exists and the state is written.
    TransferChecked::new(creator_quote_ata, quote_mint, vault, creator, created.seed_usdc, QUOTE_DECIMALS)
        .invoke()?;

    Ok(())
}

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

/// Everything creation does except moving the creator's seed: validates, creates the market, the
/// vault and the outcome mints, and writes the market's state. `backed` is `Some` for a DBC raise.
fn create_market(a: &Accounts, c: &Common, backed: Option<Backed>) -> Result<(Market, Created), ProgramError> {
    let Accounts { market, vault, creator, quote_mint, config, oracle, system_program, token_program, outcome_mints } =
        a;

    // The creator signs and pays for everything created here.
    validation::require_signer(creator, MetisError::CreatorMustSign)?;
    validation::require_writable(creator, MetisError::AccountNotWritable)?;
    validation::require_writable(market, MetisError::AccountNotWritable)?;
    validation::require_writable(vault, MetisError::AccountNotWritable)?;
    // Mints get created; in the creator-seeded variant they are also minted from.
    for mint in outcome_mints {
        validation::require_writable(mint, MetisError::AccountNotWritable)?;
    }

    // Make sure each "program" account really is the program we think it is,
    // otherwise a caller could point us at a lookalike that behaves differently.
    if system_program.address() != &pinocchio_system::ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    if token_program.address() != &pinocchio_token::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    // Config holds the protocol-wide settings every market must follow.
    // We rebuild its address ourselves so a caller can't hand us a fake one.
    validation::require_config_account(config, false)?;
    validation::require_not_paused(config)?;

    // Copy out the dials and the quote mint, then drop the borrow. The dials are snapshotted
    // onto the market below, so a later Config change never affects this market.
    let (dials, cfg_quote_mint) = {
        let config_data = config.try_borrow()?;
        let config_state =
            validation::unpack::<Config>(&config_data, MetisError::InvalidConfigAccount)?;
        validation::require_discriminator(config_state.discriminator, CONFIG_DISCRIMINATOR)?;
        (config_state.dials, config_state.quote_mint)
    };
    let max_outcomes = dials.max_outcomes;
    let virtual_liquidity_multiple = dials.virtual_liquidity_multiple;

    // What this market is seeded with, in USDC atoms: the Config stake for a creator-seeded
    // market, or the raise's target for a backed one.
    let seed_usdc = match &backed {
        None => dials.market_init_fee,
        Some(b) => {
            if dials.dbc_backing_enabled != 1 {
                return Err(MetisError::BackingDisabled.into());
            }
            if !(MIN_SEED..=MAX_BACKED_SEED).contains(&b.seed_target) {
                return Err(MetisError::ConfigValueOutOfBounds.into());
            }
            if b.backer_mint == Address::default() {
                return Err(MetisError::BackerMintInvalid.into());
            }
            b.seed_target
        }
    };

    // Anyone can create a market, so without this check a creator could price
    // their market in a worthless token they minted themselves.
    validation::require_addresses_match(
        quote_mint.address(),
        &cfg_quote_mint,
        MetisError::WrongAccountReference,
    )?;

    // The oracle must be a real football oracle (owned by the oracle program, right size and
    // discriminator), and the match must not have started: a market on a finished or voided
    // match, or one that is about to kick off, can't be created.
    let oracle_state = validation::read_oracle(oracle)?;
    if oracle_state.status != oracle::instructions::football::CFootballMatchPhase::Scheduled
        || oracle_state.is_finalized()
        || oracle_state.is_voided()
    {
        return Err(MetisError::OracleNotScheduled.into());
    }
    if !condition::is_valid(c.condition_kind, c.condition_param) {
        return Err(MetisError::InvalidCondition.into());
    }

    // Outcomes must be at least 2: a one-outcome market has a guaranteed
    // winner and nothing to price. The upper bound is whichever is stricter,
    // the admin's setting or the array size compiled into Market.
    let set_outcome_size = c.num_outcomes as usize;

    if set_outcome_size < 2
        || set_outcome_size > max_outcomes as usize
        || set_outcome_size > MAX_OUTCOMES
    {
        return Err(ProgramError::InvalidInstructionData);
    }

    // Kickoff comes from the oracle, not the caller, and must be at least PRIMARY_WINDOW_SECS away.
    //
    // Betting closes that long BEFORE kickoff, so a market opened 2 hours before a 5-hour window
    // would be born already closed - seed locked, nobody able to bet.
    let now = Clock::get()?.unix_timestamp;
    let kickoff = oracle_state.kickoff();

    if kickoff.checked_sub(now).ok_or(ProgramError::ArithmeticOverflow)? < PRIMARY_WINDOW_SECS {
        return Err(MetisError::OracleNotScheduled.into());
    }

    let primary_end = kickoff
        .checked_sub(PRIMARY_WINDOW_SECS)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    // A raise must finish inside a bounded window, and before betting would close - a market that
    // activates after primary_end would be born with nobody able to bet on it.
    if let Some(b) = &backed {
        let earliest = now.checked_add(MIN_RAISE_SECS).ok_or(ProgramError::ArithmeticOverflow)?;
        let latest = now.checked_add(MAX_RAISE_SECS).ok_or(ProgramError::ArithmeticOverflow)?;
        if b.raise_deadline < earliest || b.raise_deadline > latest || b.raise_deadline > primary_end {
            return Err(MetisError::RaiseDeadlineInvalid.into());
        }
    }

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
    let nonce_bytes = c.nonce.to_le_bytes();
    let market_seeds: [&[u8]; 3] = [MARKET_SEED, creator.address().as_ref(), &nonce_bytes];
    let (market_pda, market_bump) = Address::derive_program_address(&market_seeds, &crate::ID)
        .ok_or(ProgramError::InvalidSeeds)?;
    validation::require_addresses_match(market.address(), &market_pda, MetisError::InvalidPda)?;

    // Vault PDA: [vault, market]
    let vault_seeds: [&[u8]; 2] = [VAULT_SEED, market.address().as_ref()];
    let (vault_pda, vault_bump) = Address::derive_program_address(&vault_seeds, &crate::ID)
        .ok_or(ProgramError::InvalidSeeds)?;
    validation::require_addresses_match(vault.address(), &vault_pda, MetisError::InvalidPda)?;

    let rent = Rent::get()?;
    let market_bump_seed = [market_bump];
    let vault_bump_seed = [vault_bump];

    // A PDA has no private key, so this is how it "signs": we hand the runtime
    // the seeds it was built from.
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
    // size of the share vector, which would be division by zero. So the seed is split evenly
    // across every outcome, giving the market a real starting shape with no outcome favoured.
    let seed_per_side = crate::amm::curve::symmetric_seed(seed_usdc, c.num_outcomes);

    // A stake too small to produce even one share per side would still leave us
    // at zero, so refuse rather than open a broken market.
    if seed_per_side == 0 {
        return Err(MetisError::PaymentTooSmall.into());
    }

    // docs/mvp.md 1.4: frozen on the market for life, computed once from
    // whatever Config.virtual_liquidity_multiple happens to be right now -
    // like quote_mint, a later admin change only affects markets created
    // after that change, never this one.
    let virtual_per_side = virtual_liquidity_per_side(seed_per_side, virtual_liquidity_multiple)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    // Fill one slot per real outcome; any unused slots stay at 0. A backed market has no shares
    // until activation mints its seed.
    let mut outcome_shares = [0u64; MAX_OUTCOMES];
    if backed.is_none() {
        for slot in outcome_shares.iter_mut().take(set_outcome_size) {
            *slot = seed_per_side;
        }
    }

    // Now give each outcome its own token. One SPL mint per outcome means a
    // position is just a token balance - transferable, tradeable on any
    // exchange, and burnable at settlement.
    for (i, outcome_mint) in outcome_mints.iter().enumerate().take(set_outcome_size) {
        // Each mint's address is derived from the market, which in itself is unique, plus the outcome
        // number, so outcome 0 and outcome 1 can never collide, and no other
        // market's mint can be passed in here.
        let index_byte = [i as u8];
        let mint_seeds: [&[u8]; 3] = [OUTCOME_MINT_SEED, market.address().as_ref(), &index_byte];

        let (mint_pda, mint_bump) = Address::derive_program_address(&mint_seeds, &crate::ID)
            .ok_or(ProgramError::InvalidSeeds)?;

        validation::require_addresses_match(outcome_mint.address(), &mint_pda, MetisError::InvalidPda)?;

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
        InitializeMint2::new(outcome_mint, QUOTE_DECIMALS, &market_pda, Some(&market_pda)).invoke()?;
    }

    // Everything this program needs to remember about the market. Note
    // total_collected starts at the seed, which matches exactly what ends up in the vault - the
    // two must always agree or the solvency check at settlement will fail. A backed market holds
    // nothing yet, so it starts at zero and activation sets it.
    let (status, seed_source, total_collected, seed_target, raise_deadline, backer_mint) = match &backed {
        None => (MarketStatus::Open, SeedSource::Creator, seed_usdc, 0, 0, Address::default()),
        Some(b) => (MarketStatus::Pending, SeedSource::DbcBackers, 0, b.seed_target, b.raise_deadline, b.backer_mint),
    };
    let market_state = Market {
        outcome_shares,
        virtual_per_side, //frozen in at creation, never changes for this market
        total_collected,
        fees_accrued: 0,
        payout_pool: 0,
        protocol_owed: 0,
        creator_owed: 0,
        platform_owed: 0,
        backers_owed: 0,
        forfeited_creator_fees: 0,
        forfeited_at: 0,
        seed_target,
        backer_supply: 0,
        raise_deadline,
        bet_close_ts: kickoff,
        primary_end,
        resolved_at: 0,
        nonce: c.nonce,
        // Snapshot of Config's dials: these govern this market for life.
        dispute_secs: dials.dispute_secs,
        fee_bps: dials.fee_bps,
        rake_bps: dials.rake_bps,
        fee_share_protocol_bps: dials.fee_share_protocol_bps,
        fee_share_backers_bps: dials.fee_share_backers_bps,
        fee_share_creator_bps: dials.fee_share_creator_bps,
        fee_share_platform_bps: dials.fee_share_platform_bps,
        creator: *creator.address(),
        platform: Address::new_from_array(c.platform),
        oracle: *oracle.address(),
        vault: *vault.address(),
        quote_mint: *quote_mint.address(),
        backer_mint,
        discriminator: MARKET_DISCRIMINATOR,
        num_outcomes: c.num_outcomes,
        status: status as u8,
        winning_outcome: OUTCOME_UNRESOLVED,
        bump: market_bump,
        condition_kind: c.condition_kind,
        condition_param: c.condition_param,
        seed_source: seed_source as u8,
        _padding: [0u8; 1],
    };
    Ok((market_state, Created { seed_per_side, seed_usdc }))
}
