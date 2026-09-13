#[cfg(test)]
pub mod wincode_oracle {
    use crate::instructions::POracle;
    use crate::state::{
        MarketKind, UpdateKind,
        football_settled::{Football, FootballMatchPhase},
    };
    use crate::tests::tests::{ReusableState, program_id};
    use litesvm::LiteSVM;
    use solana_instruction::{AccountMeta, Instruction};
    use solana_message::Message;
    use solana_signer::Signer;
    use solana_transaction::Transaction;

    pub fn create_oracle_function(svm: &mut LiteSVM, state: &ReusableState) {
        let ReusableState {
            creator,
            oracle,
            system_program,
        } = state;

        let football = Football {
            match_id: 27034u32.to_le_bytes(),
            score: 0,
            penalties: 0,
            status: FootballMatchPhase::Scheduled,
        };
        let football_data = wincode::serialize(&football).expect("football serializes");

        let create_ix = Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new(creator.pubkey(), true),
                AccountMeta::new(*oracle, false),
                AccountMeta::new_readonly(*system_program, false),
            ],
            data: [(POracle::WincodeCreateOracle as u8)]
                .into_iter()
                .chain([MarketKind::Football as u8])
                .chain(football_data)
                .collect(),
        };

        let transaction = Transaction::new(
            &[creator],
            Message::new(&[create_ix], Some(&creator.pubkey())),
            svm.latest_blockhash(),
        );
        let tx = svm
            .send_transaction(transaction)
            .expect("wincode create oracle transaction succeeds");

        println!("wincode create CUs consumed: {}", tx.compute_units_consumed);
    }

    pub fn update_oracle_function(svm: &mut LiteSVM, state: &ReusableState) {
        let ReusableState {
            creator,
            oracle,
            system_program,
        } = state;

        let football = Football {
            match_id: 27034u32.to_le_bytes(),
            score: 0x32,
            penalties: 0x10,
            status: FootballMatchPhase::Finished,
        };
        let football_data = wincode::serialize(&football).expect("football serializes");

        let update_ix = Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new(creator.pubkey(), true),
                AccountMeta::new(*oracle, false),
                AccountMeta::new_readonly(*system_program, false),
            ],
            data: [(POracle::WincodeUpdateOracle as u8)]
                .into_iter()
                .chain([MarketKind::Football as u8, UpdateKind::Score as u8])
                .chain(football_data)
                .collect(),
        };

        let transaction = Transaction::new(
            &[creator],
            Message::new(&[update_ix], Some(&creator.pubkey())),
            svm.latest_blockhash(),
        );
        let tx = svm
            .send_transaction(transaction)
            .expect("wincode update oracle transaction succeeds");

        let account = svm
            .get_account(oracle)
            .expect("oracle account exists after update");
        let updated = wincode::deserialize::<Football>(&account.data)
            .expect("oracle account contains wincode football data");
        assert_eq!(updated.score, 0x32);
        assert_eq!(updated.penalties, 0);
        assert!(matches!(updated.status, FootballMatchPhase::Scheduled));

        println!("wincode update CUs consumed: {}", tx.compute_units_consumed);
    }
}
