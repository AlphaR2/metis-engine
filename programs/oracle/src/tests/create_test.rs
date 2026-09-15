#[cfg(test)]
pub mod create_oracle {
    use crate::instructions::POracle;
    use crate::instructions::football::FootballSettled;
    use crate::state::MarketKind;
    use crate::tests::tests::{FIXTURE_MERKLE_ROOT, ReusableState, program_id};
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
            authority,
            individual_authority,
            ..
        } = state;

        let football = FootballSettled::new(FIXTURE_MERKLE_ROOT);

        let create_ix = Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new(creator.pubkey(), true),
                AccountMeta::new(*oracle, false),
                AccountMeta::new_readonly(*system_program, false),
                AccountMeta::new_readonly(*authority, false),
                AccountMeta::new_readonly(*individual_authority, false),
            ],
            data: [(POracle::CreateOracle as u8)]
                .into_iter()
                .chain([MarketKind::Football as u8])
                .chain(football.as_bytes().iter().copied())
                .collect(),
        };

        let message = Message::new(&[create_ix], Some(&creator.pubkey()));
        let recent_blockhash = svm.latest_blockhash();
        let transaction = Transaction::new(&[creator], message, recent_blockhash);
        let tx = svm
            .send_transaction(transaction)
            .expect("create oracle transaction succeeds");
        let oracle_account = svm
            .get_account(oracle)
            .expect("oracle account exists after create");
        assert_eq!(oracle_account.data.as_slice(), football.as_bytes());
        // println!("{:?}", tx.logs);
        println!("create cus consumed: {:?}", tx.compute_units_consumed);
    }
}
