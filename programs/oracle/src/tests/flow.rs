// The oracle's life cycle through the real program in litesvm: creating a fixture, reporters
// agreeing on a result, the result going final, overrides, closing and pausing. One scenario per
// rule, each named for the rule it protects.
#[cfg(test)]
mod flow {
    use litesvm::LiteSVM;
    use solana_account::Account;
    use solana_clock::Clock;
    use solana_instruction::{AccountMeta, Instruction};
    use solana_keypair::Keypair;
    use solana_message::Message;
    use solana_pubkey::Pubkey;
    use solana_signer::Signer;
    use solana_transaction::Transaction;

    use crate::{
        constants::{
            AUTHORITY_SEED, CLOSER_QUORUM_SEED, CLOSE_GRACE_SECS, FOOTBALL_SEED, GUARDIAN,
            INDIVIDUAL_AUTHORITY_DISCRIMINATOR, INDIVIDUAL_AUTHORITY_SEED, MIN_KICKOFF_LEAD_SECS,
            ORACLE_AUTHORITY_DISCRIMINATOR, OVERRIDE_WINDOW_SECS, ROOT_SIGNERS, SETTLED_SEED,
        },
        instructions::{
            POracle,
            close_oracle::CloseArgs,
            football::{CFootballMatchPhase as Phase, FootballSettled},
        },
        state::{DelegatedAuthority, MarketKind, RootAuthority},
        tests::tests::program_id,
    };

    const T0: i64 = 1_000_000;
    const KICKOFF: i64 = T0 + 6 * 60 * 60;
    const FIXTURE: [u8; 32] = [27; 32];

    struct World {
        svm: LiteSVM,
        reporters: [Keypair; 3],
        individual: [Pubkey; 3],
        oracle: Pubkey,
        authority: Pubkey,
        system_program: Pubkey,
    }

    fn root(i: usize) -> Pubkey {
        Pubkey::new_from_array(ROOT_SIGNERS[i].to_bytes())
    }

    fn guardian() -> Pubkey {
        Pubkey::new_from_array(GUARDIAN.to_bytes())
    }

    fn set_clock(svm: &mut LiteSVM, ts: i64) {
        let mut clock = svm.get_sysvar::<Clock>();
        clock.unix_timestamp = ts;
        svm.set_sysvar(&clock);
    }

    /// Program loaded, authority seeded, three attested reporters, clock at T0. Signature
    /// checks are off because the root and guardian keys are placeholders nobody holds keys for.
    fn world() -> World {
        let mut svm = LiteSVM::new().with_sigverify(false);
        set_clock(&mut svm, T0);
        let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/deploy/oracle.so"))
            .expect("build the SBF program first: cargo build-sbf");
        svm.add_program(program_id(), &bytes).expect("program loads");

        let (authority, _) =
            solana_address::Address::find_program_address(&[AUTHORITY_SEED, GUARDIAN.as_ref()], &crate::ID);
        let authority = Pubkey::from(authority.to_bytes());
        svm.set_account(
            authority,
            Account {
                lamports: 1_000_000,
                data: bytemuck::bytes_of(&RootAuthority::new(&ROOT_SIGNERS, GUARDIAN, ORACLE_AUTHORITY_DISCRIMINATOR)).to_vec(),
                owner: program_id(),
                ..Default::default()
            },
        )
        .unwrap();

        let reporters = [
            Keypair::new_from_array([42; 32]),
            Keypair::new_from_array([43; 32]),
            Keypair::new_from_array([44; 32]),
        ];
        let mut individual = [Pubkey::default(); 3];
        for (i, kp) in reporters.iter().enumerate() {
            svm.airdrop(&kp.pubkey(), 10_000_000_000).unwrap();
            let (pda, _) = solana_address::Address::find_program_address(
                &[INDIVIDUAL_AUTHORITY_SEED, kp.pubkey().as_ref()],
                &crate::ID,
            );
            individual[i] = Pubkey::from(pda.to_bytes());
            let mut attested = DelegatedAuthority::new(
                pinocchio::Address::from(kp.pubkey().to_bytes()),
                INDIVIDUAL_AUTHORITY_DISCRIMINATOR,
            );
            attested.attesters = [ROOT_SIGNERS[0].to_bytes(), ROOT_SIGNERS[1].to_bytes()];
            attested.is_attest_complete = 1u64.to_le_bytes();
            svm.set_account(
                individual[i],
                Account {
                    lamports: 1_000_000,
                    data: bytemuck::bytes_of(&attested).to_vec(),
                    owner: program_id(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        for r in 0..3 {
            svm.airdrop(&root(r), 10_000_000_000).unwrap();
        }
        svm.airdrop(&guardian(), 10_000_000_000).unwrap();

        let (oracle, _) = solana_address::Address::find_program_address(
            &[FOOTBALL_SEED, SETTLED_SEED, &FIXTURE],
            &crate::ID,
        );
        World {
            svm,
            reporters,
            individual,
            oracle: Pubkey::from(oracle.to_bytes()),
            authority,
            system_program: "11111111111111111111111111111111".parse().unwrap(),
        }
    }

    fn send(payer: Pubkey, ix: Instruction, w: &mut World) -> bool {
        // A fresh blockhash each time: identical unsigned transactions would otherwise be
        // refused as already processed.
        w.svm.expire_blockhash();
        let msg = Message::new_with_blockhash(&[ix], Some(&payer), &w.svm.latest_blockhash());
        w.svm.send_transaction(Transaction::new_unsigned(msg)).is_ok()
    }

    fn create_ix(w: &World, who: usize, kickoff: i64) -> Instruction {
        let mut data = vec![POracle::CreateOracle as u8, MarketKind::Football as u8];
        data.extend_from_slice(&FIXTURE);
        data.extend_from_slice(&kickoff.to_le_bytes());
        Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new(w.reporters[who].pubkey(), true),
                AccountMeta::new(w.oracle, false),
                AccountMeta::new_readonly(w.system_program, false),
                AccountMeta::new_readonly(w.authority, false),
                AccountMeta::new_readonly(w.individual[who], false),
            ],
            data,
        }
    }

    fn create(w: &mut World) {
        let ix = create_ix(w, 0, KICKOFF);
        let payer = w.reporters[0].pubkey();
        assert!(send(payer, ix, w), "create should succeed");
    }

    fn submit_ix(w: &World, who: usize, status: Phase, home: u8, away: u8) -> Instruction {
        Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new_readonly(w.reporters[who].pubkey(), true),
                AccountMeta::new(w.oracle, false),
                AccountMeta::new_readonly(w.authority, false),
                AccountMeta::new_readonly(w.individual[who], false),
            ],
            data: vec![POracle::SubmitResult as u8, status as u8, FootballSettled::pack_score(home, away), 0],
        }
    }

    fn submit(w: &mut World, who: usize, status: Phase, home: u8, away: u8) -> bool {
        let ix = submit_ix(w, who, status, home, away);
        let payer = w.reporters[who].pubkey();
        send(payer, ix, w)
    }

    /// Two reporters say the same thing.
    fn agree(w: &mut World, status: Phase, home: u8, away: u8) {
        assert!(submit(w, 0, status, home, away), "first report is accepted");
        assert!(submit(w, 1, status, home, away), "second report is accepted");
    }

    fn state(w: &World) -> FootballSettled {
        let data = w.svm.get_account(&w.oracle).expect("oracle exists").data;
        *bytemuck::checked::try_from_bytes::<FootballSettled>(&data).expect("football data")
    }

    fn play_to_full_time(w: &mut World, home: u8, away: u8) {
        set_clock(&mut w.svm, KICKOFF + 1);
        agree(w, Phase::FirstHalf, 0, 0);
        agree(w, Phase::HalfTime, 0, 0);
        agree(w, Phase::SecondHalf, home, away);
        set_clock(&mut w.svm, KICKOFF + 7_000);
        agree(w, Phase::Finished, home, away);
    }

    fn override_ix(w: &World, first: Pubkey, second: Pubkey, action: u8, home: u8, away: u8) -> Instruction {
        let score = FootballSettled::pack_score(home, away);
        Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new_readonly(first, true),
                AccountMeta::new_readonly(second, true),
                AccountMeta::new(w.oracle, false),
                AccountMeta::new_readonly(w.authority, false),
            ],
            data: vec![POracle::OverrideResult as u8, action, Phase::Finished as u8, score, 0, score],
        }
    }

    // ── Creating ─────────────────────────────────────────────────────────

    #[test]
    fn create_starts_scheduled_scoreless_with_its_kickoff() {
        let mut w = world();
        create(&mut w);
        let s = state(&w);
        assert_eq!(s.kickoff(), KICKOFF);
        assert_eq!(s.fixture_merkle_root, FIXTURE);
        assert_eq!((s.status, s.score, s.finalized, s.voided), (Phase::Scheduled, 0, 0, 0));
        assert!(s.reports.iter().all(|r| r.used == 0));
    }

    #[test]
    fn create_refuses_a_kickoff_that_is_too_close_or_in_the_past() {
        let mut w = world();
        for kickoff in [T0 - 1, T0, T0 + MIN_KICKOFF_LEAD_SECS - 1] {
            let ix = create_ix(&w, 0, kickoff);
            let payer = w.reporters[0].pubkey();
            assert!(!send(payer, ix, &mut w), "kickoff {kickoff} should be refused");
        }
        let ix = create_ix(&w, 0, T0 + MIN_KICKOFF_LEAD_SECS);
        let payer = w.reporters[0].pubkey();
        assert!(send(payer, ix, &mut w), "exactly the minimum lead is fine");
    }

    #[test]
    fn create_refuses_an_unattested_caller_and_a_second_create() {
        let mut w = world();
        let outsider = Keypair::new_from_array([77; 32]);
        w.svm.airdrop(&outsider.pubkey(), 10_000_000_000).unwrap();
        let mut ix = create_ix(&w, 0, KICKOFF);
        ix.accounts[0] = AccountMeta::new(outsider.pubkey(), true);
        ix.accounts.pop(); // no attestation to show
        assert!(!send(outsider.pubkey(), ix, &mut w), "outsider cannot create");

        create(&mut w);
        let again = create_ix(&w, 1, KICKOFF);
        let payer = w.reporters[1].pubkey();
        assert!(!send(payer, again, &mut w), "an existing fixture cannot be created over");
    }

    // ── Reporting ────────────────────────────────────────────────────────

    #[test]
    fn one_reporter_alone_changes_nothing_two_agreeing_do() {
        let mut w = world();
        create(&mut w);
        set_clock(&mut w.svm, KICKOFF + 1);
        assert!(submit(&mut w, 0, Phase::FirstHalf, 0, 0));
        assert_eq!(state(&w).status, Phase::Scheduled, "one report is only recorded");
        assert_eq!(state(&w).reports.iter().filter(|r| r.used != 0).count(), 1);
        assert!(submit(&mut w, 1, Phase::FirstHalf, 0, 0));
        assert_eq!(state(&w).status, Phase::FirstHalf, "the second matching report applies it");
        assert!(state(&w).reports.iter().all(|r| r.used == 0), "slots clear after applying");
    }

    #[test]
    fn disagreeing_reporters_leave_the_result_alone_until_a_third_breaks_the_tie() {
        let mut w = world();
        create(&mut w);
        play_to_full_time_but_not_finished(&mut w);
        assert!(submit(&mut w, 0, Phase::SecondHalf, 1, 0));
        assert!(submit(&mut w, 1, Phase::SecondHalf, 0, 1));
        assert_eq!(state(&w).get_score(), (0, 0));
        assert!(submit(&mut w, 2, Phase::SecondHalf, 1, 0));
        assert_eq!(state(&w).get_score(), (1, 0));
    }

    fn play_to_full_time_but_not_finished(w: &mut World) {
        set_clock(&mut w.svm, KICKOFF + 1);
        agree(w, Phase::FirstHalf, 0, 0);
        agree(w, Phase::HalfTime, 0, 0);
        agree(w, Phase::SecondHalf, 0, 0);
    }

    #[test]
    fn an_unattested_reporter_cannot_report() {
        let mut w = world();
        create(&mut w);
        set_clock(&mut w.svm, KICKOFF + 1);
        let outsider = Keypair::new_from_array([78; 32]);
        w.svm.airdrop(&outsider.pubkey(), 10_000_000_000).unwrap();
        let mut ix = submit_ix(&w, 0, Phase::FirstHalf, 0, 0);
        ix.accounts[0] = AccountMeta::new_readonly(outsider.pubkey(), true);
        assert!(!send(outsider.pubkey(), ix, &mut w));
        // Borrowing another reporter's attestation doesn't work either: it names someone else.
        let mut borrowed = submit_ix(&w, 1, Phase::FirstHalf, 0, 0);
        borrowed.accounts[0] = AccountMeta::new_readonly(outsider.pubkey(), true);
        assert!(!send(outsider.pubkey(), borrowed, &mut w));
    }

    #[test]
    fn nothing_but_scheduled_can_be_reported_before_kickoff() {
        let mut w = world();
        create(&mut w);
        set_clock(&mut w.svm, KICKOFF - 1);
        assert!(!submit(&mut w, 0, Phase::FirstHalf, 0, 0));
        set_clock(&mut w.svm, KICKOFF);
        assert!(submit(&mut w, 0, Phase::FirstHalf, 0, 0));
    }

    #[test]
    fn a_phase_cannot_be_skipped_even_when_two_reporters_agree() {
        let mut w = world();
        create(&mut w);
        set_clock(&mut w.svm, KICKOFF + 1);
        assert!(submit(&mut w, 0, Phase::Finished, 3, 0), "recorded");
        assert!(!submit(&mut w, 1, Phase::Finished, 3, 0), "applying it is refused");
        assert_eq!(state(&w).status, Phase::Scheduled);
    }

    #[test]
    fn a_full_match_finalises_with_its_ninety_minute_score() {
        let mut w = world();
        create(&mut w);
        set_clock(&mut w.svm, KICKOFF + 1);
        agree(&mut w, Phase::FirstHalf, 0, 0);
        agree(&mut w, Phase::HalfTime, 0, 0);
        agree(&mut w, Phase::SecondHalf, 1, 1);
        agree(&mut w, Phase::ExtraTimeFirstHalf, 1, 1);
        agree(&mut w, Phase::ExtraTimeBreak, 1, 1);
        agree(&mut w, Phase::ExtraTimeSecondHalf, 2, 1);
        agree(&mut w, Phase::Finished, 2, 1);
        let s = state(&w);
        assert!(s.is_finalized());
        assert_eq!(s.get_score(), (2, 1));
        assert_eq!(s.regulation(), (1, 1), "extra time must not leak into the 90-minute score");
        assert_eq!(s.finished(), KICKOFF + 1);
    }

    #[test]
    fn a_finalised_result_cannot_be_changed_by_reporters() {
        let mut w = world();
        create(&mut w);
        play_to_full_time(&mut w, 2, 1);
        assert!(state(&w).is_finalized());
        assert!(!submit(&mut w, 0, Phase::Finished, 0, 9));
        assert!(!submit(&mut w, 2, Phase::Finished, 0, 9));
        assert_eq!(state(&w).get_score(), (2, 1));
    }

    // ── Override ─────────────────────────────────────────────────────────

    #[test]
    fn override_needs_two_distinct_roots() {
        let mut w = world();
        create(&mut w);
        play_to_full_time(&mut w, 2, 1);
        let (r0, r1) = (root(0), root(1));
        let outsider = w.reporters[0].pubkey();

        assert!(!send(r0, override_ix(&w, r0, r0, 0, 0, 3), &mut w), "one root twice");
        assert!(!send(r0, override_ix(&w, r0, outsider, 0, 0, 3), &mut w), "root + non-root");
        assert!(!send(outsider, override_ix(&w, outsider, r0, 0, 0, 3), &mut w), "non-root + root");
        assert_eq!(state(&w).get_score(), (2, 1), "nothing changed so far");

        assert!(send(r0, override_ix(&w, r0, r1, 0, 0, 3), &mut w), "two distinct roots");
        let s = state(&w);
        assert_eq!((s.get_score(), s.regulation()), ((0, 3), (0, 3)));
        assert!(s.overridden != 0 && s.is_finalized());
    }

    #[test]
    fn override_is_refused_after_the_window_and_a_void_is_final() {
        let mut w = world();
        create(&mut w);
        play_to_full_time(&mut w, 2, 1);
        let (r0, r1) = (root(0), root(1));

        set_clock(&mut w.svm, KICKOFF + 7_000 + OVERRIDE_WINDOW_SECS);
        assert!(!send(r0, override_ix(&w, r0, r1, 0, 5, 5), &mut w), "window closed");
        assert!(!send(r0, override_ix(&w, r0, r1, 1, 0, 0), &mut w), "void is inside the same window");

        set_clock(&mut w.svm, KICKOFF + 7_000 + OVERRIDE_WINDOW_SECS - 1);
        assert!(send(r0, override_ix(&w, r0, r1, 1, 0, 0), &mut w), "void inside the window");
        assert!(state(&w).is_voided());
        assert!(!submit(&mut w, 0, Phase::Finished, 1, 0), "no reports after a void");
        assert!(!send(r0, override_ix(&w, r0, r1, 0, 1, 0), &mut w), "nothing revives a void");
    }

    // ── Close ────────────────────────────────────────────────────────────

    #[test]
    fn an_oracle_cannot_be_closed_while_markets_may_still_need_it() {
        let mut w = world();
        create(&mut w);
        play_to_full_time(&mut w, 1, 0);
        let finished_at = state(&w).finished();

        let initiator = w.reporters[0].pubkey();
        let (quorum, _) = solana_address::Address::find_program_address(
            &[CLOSER_QUORUM_SEED, w.oracle.as_ref()],
            &crate::ID,
        );
        let quorum = Pubkey::from(quorum.to_bytes());
        let close = |w: &World, voter: Pubkey, initiator_signs: bool| Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new(initiator, initiator_signs),
                AccountMeta::new_readonly(voter, !initiator_signs),
                AccountMeta::new(w.oracle, false),
                AccountMeta::new(quorum, false),
                AccountMeta::new_readonly(w.authority, false),
                AccountMeta::new_readonly(w.system_program, false),
            ],
            data: [POracle::CloseOracle as u8]
                .into_iter()
                .chain(bytemuck::bytes_of(&CloseArgs { fixture_merkle_root: FIXTURE }).iter().copied())
                .collect(),
        };

        set_clock(&mut w.svm, finished_at + CLOSE_GRACE_SECS - 1);
        assert!(!send(initiator, close(&w, initiator, true), &mut w), "one second too early");

        set_clock(&mut w.svm, finished_at + CLOSE_GRACE_SECS);
        assert!(send(initiator, close(&w, initiator, true), &mut w), "proposal opens");
        assert!(send(root(0), close(&w, root(0), false), &mut w));
        assert!(send(root(1), close(&w, root(1), false), &mut w));
        assert!(w.svm.get_account(&w.oracle).is_none(), "closed after two root votes");
    }

    // ── Pause ────────────────────────────────────────────────────────────

    fn pause_ix(w: &World, who: Pubkey) -> Instruction {
        Instruction {
            program_id: program_id(),
            accounts: vec![AccountMeta::new_readonly(who, true), AccountMeta::new(w.authority, false)],
            data: vec![POracle::Pause as u8],
        }
    }

    fn unpause_ix(w: &World, signers: &[Pubkey]) -> Instruction {
        let mut accounts = vec![AccountMeta::new_readonly(signers[0], true), AccountMeta::new(w.authority, false)];
        accounts.extend(signers[1..].iter().map(|k| AccountMeta::new_readonly(*k, true)));
        Instruction { program_id: program_id(), accounts, data: vec![POracle::Unpause as u8] }
    }

    fn is_paused(w: &World) -> bool {
        let data = w.svm.get_account(&w.authority).unwrap().data;
        bytemuck::checked::try_from_bytes::<RootAuthority>(&data).unwrap().is_paused()
    }

    #[test]
    fn pause_stops_everything_and_unpause_needs_the_guardian_plus_two_roots() {
        let mut w = world();
        create(&mut w);
        set_clock(&mut w.svm, KICKOFF + 1);

        let outsider = w.reporters[2].pubkey();
        assert!(!send(outsider, pause_ix(&w, outsider), &mut w), "only a root can pause");
        assert!(send(root(2), pause_ix(&w, root(2)), &mut w), "any one root can pause");
        assert!(is_paused(&w));
        assert!(send(root(0), pause_ix(&w, root(0)), &mut w), "pausing again is harmless");

        assert!(!submit(&mut w, 0, Phase::FirstHalf, 0, 0), "no reports while paused");
        let ix = create_ix(&w, 1, KICKOFF + 100);
        let payer = w.reporters[1].pubkey();
        assert!(!send(payer, ix, &mut w), "no new fixtures while paused");
        assert!(!send(root(0), override_ix(&w, root(0), root(1), 0, 1, 0), &mut w), "no overrides while paused");

        let (g, r0, r1) = (guardian(), root(0), root(1));
        assert!(!send(g, unpause_ix(&w, &[g]), &mut w), "guardian alone");
        assert!(!send(r0, unpause_ix(&w, &[r0, r1]), &mut w), "roots alone, no guardian");
        assert!(!send(g, unpause_ix(&w, &[g, r0]), &mut w), "guardian + one root");
        assert!(!send(g, unpause_ix(&w, &[g, r0, r0]), &mut w), "the same root twice counts once");
        assert!(is_paused(&w), "still paused after every refused attempt");

        assert!(send(g, unpause_ix(&w, &[g, r0, r1]), &mut w), "guardian + two distinct roots");
        assert!(!is_paused(&w));
        assert!(submit(&mut w, 0, Phase::FirstHalf, 0, 0), "reporting works again");
    }
}
