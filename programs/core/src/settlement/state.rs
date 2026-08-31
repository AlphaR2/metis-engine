use bytemuck::{Pod, Zeroable};
use pinocchio::Address;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Vault {
    /// The market this vault belongs to.
    pub market: Address,
    pub discriminator: [u8; 8],
    pub bump: u8,
}

const _: () = assert!(core::mem::size_of::<Vault>() == 41);
const _: () = assert!(core::mem::align_of::<Vault>() == 1);

// ── Discriminators owned by this part 
// Do not hand-edit. Regenerate with `cargo run --bin compute-discriminators`
// if a struct is renamed or a new account/instruction is added.
pub const VAULT_DISCRIMINATOR: [u8; 8] = [211, 8, 232, 43, 2, 152, 117, 119];
pub const SETTLE_MARKET:       [u8; 8] = [193, 153, 95, 216, 166, 6, 144, 217];
pub const CLAIM:               [u8; 8] = [62, 198, 214, 193, 213, 159, 108, 210];

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::{bytes_of, from_bytes};

    #[test]
    fn vault_round_trips_through_zero_copy() {
        let vault = Vault {
            market: Default::default(),
            discriminator: VAULT_DISCRIMINATOR,
            bump: 252,
        };
        let bytes = bytes_of(&vault);
        let recovered: &Vault = from_bytes(bytes);
        assert_eq!(*recovered, vault);
    }
}
