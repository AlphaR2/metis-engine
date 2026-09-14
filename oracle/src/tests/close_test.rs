#[cfg(test)]
pub mod close_oracle {
    use litesvm::LiteSVM;
    use solana_account::Account;
    use solana_clock::Clock;
    use solana_instruction::{AccountMeta, Instruction};
    use solana_message::Message;
    use solana_pubkey::Pubkey;
    use solana_transaction::Transaction;

    use crate::{
        instructions::{POracle, authority::AUTHORITY_SEED, close_oracle::CloseArgs},
        state::{CLOSER_QUORUM_SEED, CloserQuorum, OracleAuthority, SUPERMAJORITIES},
        tests::tests::program_id,
    };

    const FIXTURE_MERKLE_ROOT: [u8; 32] = [91; 32];

    fn send(svm: &mut LiteSVM, payer: Pubkey, instruction: Instruction) {
        let message =
            Message::new_with_blockhash(&[instruction], Some(&payer), &svm.latest_blockhash());
        svm.send_transaction(Transaction::new_unsigned(message))
            .expect("close instruction succeeds");
    }

    fn close_data() -> Vec<u8> {
        let args = CloseArgs {
            fixture_merkle_root: FIXTURE_MERKLE_ROOT,
        };
        [POracle::CloseOracle as u8]
            .into_iter()
            .chain(bytemuck::bytes_of(&args).iter().copied())
            .collect()
    }

    #[test]
    fn closes_generic_oracle_and_quorum_after_two_root_votes() {
        let mut svm = LiteSVM::new().with_sigverify(false);
        let mut clock = svm.get_sysvar::<Clock>();
        clock.unix_timestamp = 1;
        svm.set_sysvar(&clock);
        let program_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/deploy/metis-oracle.so"
        ))
        .expect("build the SBF program first: cargo build-sbf");
        svm.add_program(program_id(), &program_bytes)
            .expect("program loads");

        let root_1 = Pubkey::new_from_array(SUPERMAJORITIES[0]);
        let root_2 = Pubkey::new_from_array(SUPERMAJORITIES[1]);
        let initiator = Pubkey::new_from_array([55; 32]);
        for account in [root_1, root_2, initiator] {
            svm.airdrop(&account, 10 * 1_000_000_000).unwrap();
        }
        let system_program: Pubkey = "11111111111111111111111111111111".parse().unwrap();
        // The close path only knows the shared Oracle header: market kind is
        // byte 0 and the fixture root occupies bytes 1..33. It does not
        // derive a football PDA.
        let oracle = Pubkey::new_from_array([88; 32]);
        let (closer_quorum, _) = solana_address::Address::find_program_address(
            &[CLOSER_QUORUM_SEED, oracle.as_ref()],
            &crate::ID,
        );
        let (authority, _) =
            solana_address::Address::find_program_address(&[AUTHORITY_SEED], &crate::ID);
        svm.set_account(
            authority,
            Account {
                lamports: 1_000_000,
                data: bytemuck::bytes_of(&OracleAuthority::hardcoded()).to_vec(),
                owner: program_id(),
                ..Default::default()
            },
        )
        .unwrap();
        svm.set_account(
            oracle,
            Account {
                lamports: 2_000_000,
                data: [0u8].into_iter().chain(FIXTURE_MERKLE_ROOT).collect(),
                owner: program_id(),
                ..Default::default()
            },
        )
        .unwrap();

        let close_accounts = |initiator_signer, voter, voter_signer| {
            vec![
                AccountMeta::new(initiator, initiator_signer),
                AccountMeta::new_readonly(voter, voter_signer),
                AccountMeta::new(oracle, false),
                AccountMeta::new(closer_quorum, false),
                AccountMeta::new_readonly(authority, false),
                AccountMeta::new_readonly(system_program, false),
            ]
        };

        send(
            &mut svm,
            initiator,
            Instruction {
                program_id: program_id(),
                accounts: close_accounts(true, initiator, false),
                data: close_data(),
            },
        );
        let quorum_state = svm.get_account(&closer_quorum).expect("proposal exists");
        let quorum = bytemuck::checked::try_from_bytes::<CloserQuorum>(&quorum_state.data).unwrap();
        assert_eq!(quorum.initiator, initiator.to_bytes());
        assert!(!quorum.is_quorum_complete());

        send(
            &mut svm,
            root_1,
            Instruction {
                program_id: program_id(),
                accounts: close_accounts(false, root_1, true),
                data: close_data(),
            },
        );
        assert!(svm.get_account(&oracle).is_some());
        assert!(svm.get_account(&closer_quorum).is_some());

        let initiator_before = svm.get_account(&initiator).unwrap().lamports;
        let returned_rent = svm.get_account(&oracle).unwrap().lamports
            + svm.get_account(&closer_quorum).unwrap().lamports;
        send(
            &mut svm,
            root_2,
            Instruction {
                program_id: program_id(),
                accounts: close_accounts(false, root_2, true),
                data: close_data(),
            },
        );
        assert!(svm.get_account(&oracle).is_none());
        assert!(svm.get_account(&closer_quorum).is_none());
        assert_eq!(
            svm.get_account(&initiator).unwrap().lamports,
            initiator_before + returned_rent
        );
    }
}
