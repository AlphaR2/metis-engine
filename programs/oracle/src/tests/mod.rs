pub mod authority_test;
pub mod close_test;
pub mod flow;

#[cfg(test)]
mod tests {
    use solana_pubkey::Pubkey;

    pub fn program_id() -> Pubkey {
        Pubkey::from(crate::ID)
    }
}
