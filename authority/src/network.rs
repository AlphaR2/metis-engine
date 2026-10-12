//! The trust roots every Metis program checks against, one set per network.
//!
//! This is the single source for these keys. Both programs re-export them from their own
//! `constants.rs`, so the oracle and core can never drift apart the way they once did (the
//! oracle's guardian was a copy of its own third root signer, which defeats the guardian's
//! whole purpose).
//!
//! Selected at compile time:
//!   - no feature: localnet / tests. Vanity placeholders that nobody holds a private key for;
//!     the litesvm suites send "as" these with signature checks disabled. Never deploy this.
//!   - `localnet`: throwaway keys whose private halves sit in `localnet/keys/`, for a real
//!     validator run.
//!   - `devnet`, `mainnet`: real keys. Each refuses to compile until its keys are filled in,
//!     so a build can never silently ship with placeholders.
//!
//! Hardcoded on purpose rather than stored in a Config account: a compromised root key must
//! not be able to replace the root set or the guardian.

use pinocchio::Address;

#[cfg(any(
    all(feature = "devnet", feature = "mainnet"),
    all(feature = "devnet", feature = "localnet"),
    all(feature = "mainnet", feature = "localnet"),
))]
compile_error!("enable at most one of the `devnet`, `mainnet` and `localnet` features");

#[cfg(not(any(feature = "devnet", feature = "mainnet", feature = "localnet")))]
mod keys {
    use pinocchio::Address;

    // test values, replace with mainnet/devnet values before deploying anywhere real. These are the same 3 keys for every Metis program, so they can be used to sign a transaction that unpauses all programs at once.
    
    pub const ROOT_SIGNERS: [Address; 3] = [
        Address::from_str_const("UPDTR3Mmf4qQatb5qy8rE7aEw34Jq2AUJknEgbTuw8B"),
        Address::from_str_const("UPdtRERKVQhyEEFmxTUmmezz1BStBS1wiXn1h74bhRG"),
        Address::from_str_const("upDTrFkAvEEk2N9JoxbeY6jg1KHJNzu8BAqdf7f2xVc"),
    ];
    pub const GUARDIAN: Address = Address::from_str_const("eaZoYS1Ej58sL64S3PtZbo7XS5euS7oTC1CzsEmP6VE");
}

/// Throwaway keys for a local validator run: their private halves are in `localnet/keys/`
/// (gitignored), made by `just localnet-keys`. Real signatures, unlike the placeholders above,
/// so a full run can exercise every root-gated step. Never deploy this build anywhere real.
#[cfg(feature = "localnet")]
mod keys {
    use pinocchio::Address;

    pub const ROOT_SIGNERS: [Address; 3] = [
        Address::from_str_const("3G3LM7udG7gpQCj1VotSfXm5CVaTQAAMsQRsifAYTWUN"),
        Address::from_str_const("4w1Xnox13KWMoEE1Vhy4cS8mhBuobfaonj3MVx4VUw1y"),
        Address::from_str_const("3AVR7fe4d7ahmYjRozEtZYcWK7g42MfEs9nDFYc7bXXQ"),
    ];
    pub const GUARDIAN: Address = Address::from_str_const("ASparSQ66wjmCDKpJahCqFLorWrqxCGCDxybmo5kFzVU");
}

#[cfg(feature = "devnet")]
mod keys {
    use pinocchio::Address;

    // The devnet roots and guardian, set by devnet/set-roots.sh. Public keys only: their holders keep the
    // private halves. The root set is compiled in on purpose: changing it means upgrading the programs.
    pub const ROOT_SIGNERS: [Address; 3] = [
        Address::from_str_const("mA8Aygar4s7ikwZymrWQyybhnaD8suNjn5jeeXPgRiA"),
        Address::from_str_const("2X1cAuMnvNZKqavK6d7yf6WVPUheSp12VujzKwKqeeGJ"),
        Address::from_str_const("8PW1DetybsK3KSYucLirZQWEqdcYtkZgHJeicrzuQ7GF"),
    ];
    pub const GUARDIAN: Address = Address::from_str_const("8dARy55GpAjLmnXTG63s7W3izWHxCbjht985Nhiw2N4J");
}

#[cfg(feature = "mainnet")]
mod keys {
    use pinocchio::Address;

    compile_error!("mainnet ROOT_SIGNERS and GUARDIAN are not set yet - fill them in authority/src/network.rs");
    pub const ROOT_SIGNERS: [Address; 3] = [Address::new_from_array([0; 32]); 3];
    pub const GUARDIAN: Address = Address::new_from_array([0; 32]);
}

/// Root authority keys - the same 3 signers for every Metis program.
pub const ROOT_SIGNERS: [Address; 3] = keys::ROOT_SIGNERS;

/// Its only power is co-signing Unpause, so it must be a key no root signer holds.
pub const GUARDIAN: Address = keys::GUARDIAN;

const fn same(a: &Address, b: &Address) -> bool {
    let (a, b) = (a.as_array(), b.as_array());
    let mut i = 0;
    while i < 32 {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

const fn guardian_is_distinct() -> bool {
    let mut i = 0;
    while i < ROOT_SIGNERS.len() {
        if same(&GUARDIAN, &ROOT_SIGNERS[i]) {
            return false;
        }
        i += 1;
    }
    true
}

const fn roots_are_distinct() -> bool {
    let mut i = 0;
    while i < ROOT_SIGNERS.len() {
        let mut j = i + 1;
        while j < ROOT_SIGNERS.len() {
            if same(&ROOT_SIGNERS[i], &ROOT_SIGNERS[j]) {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

const _: () = assert!(guardian_is_distinct(), "GUARDIAN must not be one of ROOT_SIGNERS");
const _: () = assert!(roots_are_distinct(), "ROOT_SIGNERS must be three distinct keys");
