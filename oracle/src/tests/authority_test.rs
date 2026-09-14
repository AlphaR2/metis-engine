#[cfg(test)]
pub mod authority {
    use litesvm::LiteSVM;
    use solana_clock::Clock;
    use solana_instruction::{AccountMeta, Instruction};
    use solana_message::Message;
    use solana_pubkey::Pubkey;
    use solana_transaction::Transaction;

    use crate::{
        constants::{AUTHORITY_SEED, INDIVIDUAL_AUTHORITY_SEED, SUPERMAJORITIES},
        instructions::POracle,
        state::{IndividualOAuthority, OracleAuthority},
        tests::tests::program_id,
    };

    fn send_unsigned(svm: &mut LiteSVM, label: &str, payer: Pubkey, instruction: Instruction) {
        let message =
            Message::new_with_blockhash(&[instruction], Some(&payer), &svm.latest_blockhash());
        let tx = svm
            .send_transaction(Transaction::new_unsigned(message))
            .expect("authority instruction succeeds");
        println!("{label} cus consumed: {:?}", tx.compute_units_consumed);
    }

    #[test]
    fn initialize_attest_and_remove_authority() {
        // Signature verification is disabled solely because the temporary root
        // addresses are public-key placeholders. The program still receives them
        // as signer accounts, exactly as it will with the real root keypairs.
        let mut svm = LiteSVM::new().with_sigverify(false);
        let mut clock = svm.get_sysvar::<Clock>();
        clock.unix_timestamp = 1;
        svm.set_sysvar(&clock);
        let program_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/deploy/oracle.so"
        ))
        .expect("build the SBF program first: cargo build-sbf");
        svm.add_program(program_id(), &program_bytes)
            .expect("program loads");

        let root_1 = Pubkey::new_from_array(SUPERMAJORITIES[0]);
        let root_2 = Pubkey::new_from_array(SUPERMAJORITIES[1]);
        let candidate = Pubkey::new_from_array([99; 32]);
        let recipient = Pubkey::new_from_array([100; 32]);
        svm.airdrop(&root_1, 10 * 1_000_000_000).unwrap();
        svm.airdrop(&root_2, 10 * 1_000_000_000).unwrap();
        svm.airdrop(&recipient, 1_000_000_000).unwrap();

        let system_program: Pubkey = "11111111111111111111111111111111".parse().unwrap();
        let (authority, _) =
            solana_address::Address::find_program_address(&[AUTHORITY_SEED], &crate::ID);
        let (individual_authority, _) = solana_address::Address::find_program_address(
            &[INDIVIDUAL_AUTHORITY_SEED, candidate.as_ref()],
            &crate::ID,
        );

        send_unsigned(
            &mut svm,
            "initialize authority",
            root_1,
            Instruction {
                program_id: program_id(),
                accounts: vec![
                    AccountMeta::new(root_1, true),
                    AccountMeta::new(authority, false),
                    AccountMeta::new_readonly(system_program, false),
                ],
                data: vec![POracle::InitializeAuthority as u8],
            },
        );
        let authority_account = svm
            .get_account(&authority)
            .expect("authority account exists");
        let authority_state =
            bytemuck::checked::try_from_bytes::<OracleAuthority>(&authority_account.data)
                .expect("authority account data is valid");
        assert_eq!(
            authority_state.supermajority,
            OracleAuthority::hardcoded().supermajority
        );

        for root in [root_1, root_2] {
            send_unsigned(
                &mut svm,
                "attest authority",
                root,
                Instruction {
                    program_id: program_id(),
                    accounts: vec![
                        AccountMeta::new(root, true),
                        AccountMeta::new_readonly(authority, false),
                        AccountMeta::new_readonly(candidate, false),
                        AccountMeta::new(individual_authority, false),
                        AccountMeta::new_readonly(system_program, false),
                    ],
                    data: vec![POracle::AttestAuthority as u8],
                },
            );
        }
        let individual_account = svm
            .get_account(&individual_authority)
            .expect("candidate authority account exists");
        let individual_state =
            bytemuck::checked::try_from_bytes::<IndividualOAuthority>(&individual_account.data)
                .expect("candidate authority data is valid");
        assert!(individual_state.is_attest_complete());
        assert_eq!(
            individual_state.attesters,
            [root_1.to_bytes(), root_2.to_bytes()]
        );

        let recipient_before = svm.get_account(&recipient).unwrap().lamports;
        let authority_lamports = individual_account.lamports;
        send_unsigned(
            &mut svm,
            "remove authority",
            root_1,
            Instruction {
                program_id: program_id(),
                accounts: vec![
                    AccountMeta::new(root_1, true),
                    AccountMeta::new_readonly(authority, false),
                    AccountMeta::new_readonly(candidate, false),
                    AccountMeta::new(individual_authority, false),
                    AccountMeta::new(recipient, false),
                ],
                data: vec![POracle::RemoveAuthority as u8],
            },
        );
        assert_eq!(
            svm.get_account(&recipient).unwrap().lamports,
            recipient_before + authority_lamports
        );
        assert!(svm.get_account(&individual_authority).is_none());
    }
}
