pub mod authority_test;
pub mod close_test;
pub mod create_test;
pub mod update_test;

#[cfg(test)]
mod tests {

    use litesvm::LiteSVM;
    use solana_account::Account;
    use solana_instruction::{AccountMeta, Instruction};
    use solana_keypair::Keypair;
    use solana_message::Message;
    use solana_pubkey::Pubkey;
    use solana_signer::Signer;
    use solana_transaction::Transaction;

    use crate::tests::create_test::create_oracle::create_oracle_function;
    use crate::tests::update_test::update_oracle::update_oracle_function;
    use crate::{
        instructions::{
            POracle,
            authority::{AUTHORITY_SEED, INDIVIDUAL_AUTHORITY_SEED},
            football::FootballSettled,
        },
        state::{IndividualOAuthority, MarketKind, OracleAuthority},
    };

    pub struct ReusableState {
        pub creator: Keypair,
        pub oracle: Pubkey,
        pub system_program: Pubkey,
        pub authority: Pubkey,
        pub individual_authority: Pubkey,
    }

    pub const FIXTURE_MERKLE_ROOT: [u8; 32] = [27; 32];

    pub fn program_id() -> Pubkey {
        Pubkey::from(crate::ID)
    }

    fn setup() -> (LiteSVM, ReusableState) {
        let mut svm = LiteSVM::new();
        let creator = Keypair::new_from_array([42; 32]);

        svm.airdrop(&creator.pubkey(), 10 * 1_000_000_000)
            .expect("Airdrop failed");

        let program_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../target/deploy/oracle.so");
        let program_bytes =
            std::fs::read(program_path).expect("build the SBF program first: cargo build-sbf");
        svm.add_program(program_id(), &program_bytes)
            .expect("Failed to add program");

        let (oracle, _) = solana_address::Address::find_program_address(
            &[b"football", b"settled", &FIXTURE_MERKLE_ROOT],
            &crate::ID,
        );
        let (authority, _) =
            solana_address::Address::find_program_address(&[AUTHORITY_SEED], &crate::ID);
        let (individual_authority, _) = solana_address::Address::find_program_address(
            &[INDIVIDUAL_AUTHORITY_SEED, creator.pubkey().as_ref()],
            &crate::ID,
        );

        svm.set_account(
            authority,
            Account {
                lamports: 1_000_000,
                data: bytemuck::bytes_of(&OracleAuthority::hardcoded()).to_vec(),
                owner: program_id(),
                ..Default::default()
            },
        )
        .expect("authority configuration is seeded");

        let mut creator_authority =
            IndividualOAuthority::new(pinocchio::Address::from(creator.pubkey().to_bytes()));
        creator_authority.attesters = OracleAuthority::hardcoded().supermajority[..2]
            .try_into()
            .expect("two hardcoded roots");
        creator_authority.is_attest_complete = 1u64.to_le_bytes();
        svm.set_account(
            individual_authority,
            Account {
                lamports: 1_000_000,
                data: bytemuck::bytes_of(&creator_authority).to_vec(),
                owner: program_id(),
                ..Default::default()
            },
        )
        .expect("completed individual authority is seeded");

        let reusable_state = ReusableState {
            creator,
            oracle,
            system_program: "11111111111111111111111111111111"
                .parse()
                .expect("valid system program id"),
            authority,
            individual_authority,
        };

        (svm, reusable_state)
    }

    #[test]
    pub fn test_create_oracle_instruction() {
        let (mut svm, reusable_state) = setup();
        create_oracle_function(&mut svm, &reusable_state);
    }

    #[test]
    pub fn test_update_oracle_instruction() {
        let (mut svm, reusable_state) = setup();
        create_oracle_function(&mut svm, &reusable_state);
        update_oracle_function(&mut svm, &reusable_state);

        let oracle_account = svm
            .get_account(&reusable_state.oracle)
            .expect("oracle account exists after create");
        let football = bytemuck::checked::try_from_bytes::<FootballSettled>(&oracle_account.data)
            .expect("oracle account contains football data");

        assert_eq!(football.fixture_merkle_root, FIXTURE_MERKLE_ROOT);
        assert_eq!(football.score, 0x32, "invalid score");
        assert_eq!(
            football.penalties, 0,
            "a score update must not alter penalties"
        );
    }

    #[test]
    pub fn test_unattested_caller_cannot_create_oracle() {
        let (mut svm, reusable_state) = setup();
        let outsider = Keypair::new_from_array([77; 32]);
        svm.airdrop(&outsider.pubkey(), 10 * 1_000_000_000)
            .expect("outsider is funded");

        let football = FootballSettled::new(FIXTURE_MERKLE_ROOT);
        let create_ix = Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new(outsider.pubkey(), true),
                AccountMeta::new(reusable_state.oracle, false),
                AccountMeta::new_readonly(reusable_state.system_program, false),
            ],
            data: [(POracle::CreateOracle as u8)]
                .into_iter()
                .chain([MarketKind::Football as u8])
                .chain(football.to_bytes().iter().copied())
                .collect(),
        };
        let transaction = Transaction::new(
            &[&outsider],
            Message::new(&[create_ix], Some(&outsider.pubkey())),
            svm.latest_blockhash(),
        );
        let failure = svm
            .send_transaction(transaction)
            .expect_err("unattested caller must be rejected");
        println!(
            "unattested create cus consumed: {:?}",
            failure.meta.compute_units_consumed
        );
    }
}
