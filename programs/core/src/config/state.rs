use bytemuck::{CheckedBitPattern, NoUninit, Pod, Zeroable};
use metis_authority::root::RootAuthority;
use pinocchio::{Address, error::ProgramError};

use crate::{
    constants::{
        BPS_DENOMINATOR, MAX_DISPUTE_SECS, MAX_FEE_PRIMARY_BPS, MAX_OUTCOMES, MAX_PRICE_IMPACT_BPS,
        MAX_PROTOCOL_SHARE_BPS, MAX_RAKE_BPS, MAX_VIRTUAL_LIQUIDITY_MULTIPLE, MIN_DISPUTE_SECS,
        MIN_SEED,
    },
    error::MetisError,
};

/// The protocol's business dials. Any single root signer may change these through
/// update_config, but only within the hardcoded bounds in constants.rs, checked by
/// `validate` - the one place both initialize_config and update_config go through, so the two
/// can never enforce different rules.
///
/// Every market snapshots the dials it uses at creation (see Market), so changing one here only
/// affects markets created afterwards.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct ConfigDials {
    /// The creator's seed, split evenly across outcomes at initialize_market. >= MIN_SEED.
    pub market_init_fee: u64,
    /// k in virtual_per_side = k x symmetric_seed. <= MAX_VIRTUAL_LIQUIDITY_MULTIPLE.
    pub virtual_liquidity_multiple: u64,
    /// Seconds after the oracle finalises a result before settlement may run.
    /// Within [MIN_DISPUTE_SECS, MAX_DISPUTE_SECS].
    pub dispute_secs: u32,
    /// Fee taken from each bet before it enters the curve. <= MAX_FEE_PRIMARY_BPS.
    pub fee_bps: u16,
    /// Cut of the pot taken at settlement. <= MAX_RAKE_BPS.
    pub rake_bps: u16,
    /// Largest single bet as a share of the market's priced depth. In (0, MAX_PRICE_IMPACT_BPS].
    pub max_in_bps_of_vault: u16,
    /// How a market's revenue (the rake plus the bet fees) is shared four ways: the protocol, the
    /// backers who seeded it, the market's creator, and the platform the market was created
    /// through. The four must sum to BPS_DENOMINATOR, and the protocol's share is capped at
    /// MAX_PROTOCOL_SHARE_BPS. With DBC backing off (or a creator-seeded market), the backers'
    /// share goes to whoever seeded the market (its creator). A market created with no platform
    /// leaves the platform's share with the protocol.
    pub fee_share_protocol_bps: u16,
    pub fee_share_backers_bps: u16,
    pub fee_share_creator_bps: u16,
    pub fee_share_platform_bps: u16,
    /// Admin's ceiling on outcomes per new market. <= MAX_OUTCOMES.
    pub max_outcomes: u8,
    /// 1 lets markets be seeded by a DBC backer raise instead of by their creator. 0 or 1 only.
    pub dbc_backing_enabled: u8,
    pub _padding: [u8; 4],
}

const _: () = assert!(core::mem::size_of::<ConfigDials>() == 40);

impl ConfigDials {
    /// Rejects any dial outside its hardcoded bound. Named errors rather than one generic
    /// failure, so whoever is setting the config can see which value was refused.
    pub fn validate(&self) -> Result<(), ProgramError> {
        let out_of_bounds = |ok: bool| {
            if ok {
                Ok(())
            } else {
                Err(ProgramError::from(MetisError::ConfigValueOutOfBounds))
            }
        };

        out_of_bounds(self.market_init_fee >= MIN_SEED)?;
        out_of_bounds(self.virtual_liquidity_multiple <= MAX_VIRTUAL_LIQUIDITY_MULTIPLE)?;
        out_of_bounds((MIN_DISPUTE_SECS..=MAX_DISPUTE_SECS).contains(&self.dispute_secs))?;
        out_of_bounds(self.fee_bps <= MAX_FEE_PRIMARY_BPS)?;
        out_of_bounds(self.rake_bps <= MAX_RAKE_BPS)?;
        // 0 used to mean "no trade cap", which let a single setting silently remove the
        // protection. A cap is now always on.
        out_of_bounds(self.max_in_bps_of_vault > 0 && self.max_in_bps_of_vault <= MAX_PRICE_IMPACT_BPS)?;
        out_of_bounds(self.fee_share_protocol_bps <= MAX_PROTOCOL_SHARE_BPS)?;
        let share_total = self.fee_share_protocol_bps as u32
            + self.fee_share_backers_bps as u32
            + self.fee_share_creator_bps as u32
            + self.fee_share_platform_bps as u32;
        out_of_bounds(share_total == BPS_DENOMINATOR as u32)?;
        out_of_bounds(self.max_outcomes >= 2 && self.max_outcomes as usize <= MAX_OUTCOMES)?;
        out_of_bounds(self.dbc_backing_enabled <= 1)?;
        Ok(())
    }
}

/// One singleton account, protocol-wide, shared by every market.
#[repr(C)]
#[derive(Clone, Copy, Debug, NoUninit, CheckedBitPattern)]
pub struct Config {
    /// Business dials - one root signer, bounded. See ConfigDials.
    pub dials: ConfigDials,
    /// The root authority. Its signer set comes from the hardcoded ROOT_SIGNERS and can't be
    /// changed through any config instruction.
    pub root: RootAuthority,
    /// High-friction fields: these decide where money goes and which token is accepted, so
    /// changing them needs two distinct root signers (update_config_critical).
    ///
    /// The stablecoin every NEW market is denominated in. Markets snapshot it at creation and
    /// stay bound to it for life.
    pub quote_mint: Address,
    /// Where the protocol's share of fees and the rake is paid.
    pub treasury: Address,
    /// The DBC config backer raises are launched from. Zero while DBC backing is unused.
    pub dbc_config: Address,
    /// Identifies this account as a Config to every instruction that reads it.
    pub discriminator: [u8; 8],
    /// This account's own PDA bump, cached at initialize_config.
    pub bump: u8,
    pub _padding: [u8; 5],
}

const _: () = assert!(core::mem::size_of::<Config>() == 288);
const _: () = assert!(core::mem::align_of::<Config>() == 8);

// ── Discriminators owned by this part
// Do not hand-edit. Regenerate with `cargo run --bin compute-discriminators`
// if a struct is renamed or a new account/instruction is added.

pub const CONFIG_DISCRIMINATOR: [u8; 8] = [155, 12, 170, 224, 30, 250, 204, 130];
pub const DELEGATED_AUTHORITY_DISCRIMINATOR: [u8; 8] = [89, 106, 242, 124, 175, 252, 140, 92];
pub const INITIALIZE_CONFIG: [u8; 8] = [208, 127, 21, 1, 194, 190, 196, 70];
pub const UPDATE_CONFIG: [u8; 8] = [29, 158, 252, 191, 10, 83, 219, 99];
pub const UPDATE_CONFIG_CRITICAL: [u8; 8] = [46, 164, 13, 116, 110, 129, 26, 221];
pub const PAUSE: [u8; 8] = [211, 22, 221, 251, 74, 121, 193, 47];
pub const UNPAUSE: [u8; 8] = [169, 144, 4, 38, 10, 141, 188, 255];
pub const ATTEST_AUTHORITY: [u8; 8] = [109, 185, 129, 174, 230, 153, 126, 135];
pub const REMOVE_AUTHORITY: [u8; 8] = [242, 104, 208, 132, 190, 250, 74, 216];

impl metis_authority::instructions::RootBearing for Config {
    #[inline]
    fn root(&self) -> &RootAuthority {
        &self.root
    }

    #[inline]
    fn root_mut(&mut self) -> &mut RootAuthority {
        &mut self.root
    }

    #[inline]
    fn discriminator(&self) -> [u8; 8] {
        self.discriminator
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> ConfigDials {
        ConfigDials {
            market_init_fee: MIN_SEED,
            virtual_liquidity_multiple: 30,
            dispute_secs: MIN_DISPUTE_SECS,
            fee_bps: MAX_FEE_PRIMARY_BPS,
            rake_bps: 100,
            max_in_bps_of_vault: MAX_PRICE_IMPACT_BPS,
            fee_share_protocol_bps: 4_000,
            fee_share_backers_bps: 3_000,
            fee_share_creator_bps: 2_000,
            fee_share_platform_bps: 1_000,
            max_outcomes: 2,
            dbc_backing_enabled: 0,
            _padding: [0; 4],
        }
    }

    #[test]
    fn valid_dials_pass() {
        assert!(valid().validate().is_ok());
    }

    // Each dial pushed one step past its hardcoded bound must be refused on its own.
    #[test]
    fn every_dial_is_bounded() {
        let cases: [(&str, fn(&mut ConfigDials)); 14] = [
            ("seed below MIN_SEED", |d| d.market_init_fee = MIN_SEED - 1),
            ("k above ceiling", |d| d.virtual_liquidity_multiple = MAX_VIRTUAL_LIQUIDITY_MULTIPLE + 1),
            ("dispute too short", |d| d.dispute_secs = MIN_DISPUTE_SECS - 1),
            ("dispute too long", |d| d.dispute_secs = MAX_DISPUTE_SECS + 1),
            ("fee above ceiling", |d| d.fee_bps = MAX_FEE_PRIMARY_BPS + 1),
            ("rake above ceiling", |d| d.rake_bps = MAX_RAKE_BPS + 1),
            ("trade cap of 0", |d| d.max_in_bps_of_vault = 0),
            ("trade cap above ceiling", |d| d.max_in_bps_of_vault = MAX_PRICE_IMPACT_BPS + 1),
            ("protocol share above ceiling", |d| {
                d.fee_share_protocol_bps = MAX_PROTOCOL_SHARE_BPS + 1;
                d.fee_share_backers_bps = BPS_DENOMINATOR - MAX_PROTOCOL_SHARE_BPS - 1;
                d.fee_share_creator_bps = 0;
                d.fee_share_platform_bps = 0;
            }),
            ("shares not summing to 100%", |d| d.fee_share_creator_bps -= 1),
            ("platform share pushing the total past 100%", |d| d.fee_share_platform_bps += 1),
            ("one outcome", |d| d.max_outcomes = 1),
            ("outcomes above MAX_OUTCOMES", |d| d.max_outcomes = MAX_OUTCOMES as u8 + 1),
            ("dbc flag not 0/1", |d| d.dbc_backing_enabled = 2),
        ];
        for (name, break_it) in cases {
            let mut dials = valid();
            break_it(&mut dials);
            assert!(dials.validate().is_err(), "{name} should be rejected");
        }
    }
}
