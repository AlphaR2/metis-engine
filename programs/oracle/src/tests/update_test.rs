#[cfg(test)]
pub mod update_oracle {
    use crate::instructions::POracle;
    use crate::instructions::football::CFootballMatchPhase;
    use crate::state::UpdateKind;
    use crate::tests::tests::{ReusableState, program_id};
    use litesvm::LiteSVM;
    use solana_instruction::{AccountMeta, Instruction};
    use solana_message::Message;
    use solana_signer::Signer;
    use solana_transaction::Transaction;

    pub fn update_oracle_function(svm: &mut LiteSVM, state: &ReusableState) {
        let ReusableState {
            creator,
            oracle,
            system_program,
            authority,
            individual_authority,
            ..
        } = state;

        // let football = svm.get_account(oracle).expect("failed to get this account");

        let update_ix = Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new(creator.pubkey(), true),
                AccountMeta::new(*oracle, false),
                AccountMeta::new_readonly(*system_program, false),
                AccountMeta::new_readonly(*authority, false),
                AccountMeta::new_readonly(*individual_authority, false),
            ],
            data: [(POracle::UpdateOracle as u8)]
                .into_iter()
                .chain([UpdateKind::Score as u8])
                .chain([0x32, 0x10, CFootballMatchPhase::Finished as u8])
                .collect(),
        };

        let message = Message::new(&[update_ix], Some(&creator.pubkey()));
        let transaction = Transaction::new(&[creator], message, svm.latest_blockhash());

        let tx = svm
            .send_transaction(transaction)
            .expect("update oracle transaction succeeds");

        // println!("{:?}", tx.logs);
        println!("update cus consumed: {:?}", tx.compute_units_consumed);
    }
}
