//! The `NestBackupKey` leg of the post-succession **aftermath**
//! (`succession-aftermath.md` § Re-key scope, the `NestBackupKey` row: *"Old
//! grant revoked; successor derives + grants the new one"*), and the
//! destination-mark raise that reports what it carried across.
//!
//! The succession transaction deletes the old grant outright
//! (`successions.rs`: `DELETE FROM nest_backup_keys WHERE owner_actor_id = ?1`),
//! and the nest's backup sweep enumerates owners *by* that grant
//! (`delegation_runner.rs::list_nest_backup_key_owners`) — so a successor
//! without one is skipped entirely and nothing is backed up. The
//! `backup_destinations` registry rows go the same way: the transaction
//! **deletes** them too (ruled `Burn` 2026-08-13 — `succession-aftermath.md`
//! § Re-key scope, the backup-enrollment row; a row is thief-writable directly
//! over `fauna.backup.destination.register`, so a *moved* one could name a
//! destination that appears in no list the owner keeps and that the
//! re-register-then-mark adjudication could therefore never raise). Either way
//! the successor starts from an empty nest-side projection: the bound box's
//! `fauna.state.backup` list is the authoritative list, and the projection is
//! rebuilt from it.
//!
//! These tests pin the driver that rebuilds both halves off a list handed in
//! (the post-store-ready pass reads it — `fauna_client_recovery`'s
//! `ledger_aftermath` tests pin that wiring), and the raise over the account's
//! lists ([`raise_succession_destination_marks`]) with the verdict helpers.

use std::sync::{Arc, Mutex};

use fauna_client_testkit::{ClassifiedError, block_on};

use fauna_client_backup::{KIND_DESTINATION_REGISTER, KIND_NEST_KEY_GRANT, KIND_STATUS};
use fauna_client_config::test_helpers::FakeBackupStateStore;
use fauna_client_config::{
    BackupRegrantOutcome, BackupRegrantProgress, keep_backup_destination, mutate_backup,
    raise_succession_destination_marks, regrant_nest_backup_key, remove_backup_destination,
};
use fauna_core::backup_state::BackupState;
use fauna_core::crypto::NestBackupKey;
use fauna_core::data::{
    BackupConfig, BackupDestination, DESTINATION_KIND_CLIENT_DEVICE, DESTINATION_KIND_NEST,
    UnattestedVerdict,
};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_protocol::RpcRequester;
use fauna_protocol::backup::{
    BackupDestinationStatusItem, BackupStatusReply, DestinationRegisterReply,
    DestinationRegisterRequest, NestKeyGrantReply, NestKeyGrantRequest,
};

/// The predecessor's seed — the key that must never be re-granted, and the
/// identity a raised mark names.
const PREDECESSOR_SEED: [u8; 32] = [1u8; 32];
/// The successor's own seed. Every assertion about *which* key was granted
/// anchors on `NestBackupKey::derive` over this.
const SUCCESSOR_SEED: [u8; 32] = [2u8; 32];
/// The box the successor's connection is bound to.
const BOUND_BOX: [u8; 32] = [0xaa; 32];
/// Another box the account keeps a list on.
const OTHER_BOX: [u8; 32] = [0xbb; 32];

/// In-memory source nest: the granted `NestBackupKey`, the destination rows it
/// has been told about, and the ordered call log the round-trip assertions
/// read.
#[derive(Default)]
struct FakeNest {
    kinds: Mutex<Vec<&'static str>>,
    granted_key: Mutex<Option<Vec<u8>>>,
    registered: Mutex<Vec<DestinationRegisterRequest>>,
    /// Mirrors the real nest: `enrolled` is true exactly when a `NestBackupKey`
    /// grant is stored, because that grant is what makes the coordinator
    /// openable (`backup_handlers.rs`: `enrolled = coordinator.is_some()`).
    enrolled: Mutex<bool>,
    /// Destination ids the nest already projects — what a partially-healed or
    /// fully-healed owner looks like.
    projected: Mutex<Vec<String>>,
}

impl RpcRequester for FakeNest {
    type Error = ClassifiedError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.kinds.lock().unwrap().push(kind);
        let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
        let reply = match kind {
            KIND_STATUS => fauna_protocol::encode_canonical(&BackupStatusReply {
                enrolled: *self.enrolled.lock().unwrap(),
                destinations: self
                    .projected
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|id| BackupDestinationStatusItem {
                        destination_id: id.clone(),
                        ..Default::default()
                    })
                    .collect(),
                extra: Default::default(),
            }),
            KIND_NEST_KEY_GRANT => {
                let req: NestKeyGrantRequest =
                    fauna_protocol::decode_strict(&bytes).expect("decode grant");
                *self.granted_key.lock().unwrap() = Some(req.nest_backup_key.into_vec());
                *self.enrolled.lock().unwrap() = true;
                fauna_protocol::encode_canonical(&NestKeyGrantReply {
                    ok: true,
                    extra: Default::default(),
                })
            }
            KIND_DESTINATION_REGISTER => {
                let req: DestinationRegisterRequest =
                    fauna_protocol::decode_strict(&bytes).expect("decode register");
                self.projected
                    .lock()
                    .unwrap()
                    .push(req.destination_id.clone());
                self.registered.lock().unwrap().push(req);
                fauna_protocol::encode_canonical(&DestinationRegisterReply {
                    ok: true,
                    extra: Default::default(),
                })
            }
            other => panic!("unexpected kind {other}"),
        }
        .expect("encode reply");
        Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
    }
}

fn destination(id: &str, kind: &str) -> BackupDestination {
    BackupDestination {
        destination_id: id.into(),
        destination_nest_url: format!("https://{id}.example/"),
        destination_actor_pubkey: [9u8; 32],
        kind: kind.into(),
        custodian_device_id: (kind == DESTINATION_KIND_CLIENT_DEVICE).then(|| "device-1".into()),
        added_at: 1_700_000_000,
        ..Default::default()
    }
}

fn predecessor() -> ActorId {
    ActorKeypair::from_secret(PREDECESSOR_SEED).actor_id()
}

/// Whether `id` is under review in `state` — an open mark names it.
fn unattested(state: &BackupState, id: &str) -> bool {
    state
        .marks
        .iter()
        .any(|m| m.destination_id == id && m.verdict.is_open())
}

/// A store whose bound box lists `destinations`, raised as carried across the
/// predecessor's succession — the state the pass leaves behind.
fn raised_store(destinations: Vec<BackupDestination>) -> FakeBackupStateStore {
    let store = FakeBackupStateStore::empty();
    store.seed_list(BOUND_BOX, destinations);
    assert!(block_on(raise_succession_destination_marks(&store, predecessor())).expect("raise"));
    store
}

/// The successor's key, never the predecessor's. The nest cannot catch a wrong
/// key here — it stores whatever it is handed — so a predecessor-derived grant
/// would leave every future segment sealed to a retired identity, silently.
#[test]
fn the_granted_key_is_derived_from_the_successors_own_seed() {
    let nest = Arc::new(FakeNest::default());

    let outcome = block_on(regrant_nest_backup_key(
        nest.clone(),
        SUCCESSOR_SEED,
        &[destination("dest-1", DESTINATION_KIND_NEST)],
    ))
    .expect("regrant");

    assert_eq!(outcome, BackupRegrantOutcome::Regranted { destinations: 1 });
    let granted = nest.granted_key.lock().unwrap().clone().expect("a grant");
    assert_eq!(
        granted,
        NestBackupKey::derive(&SUCCESSOR_SEED).to_bytes().to_vec(),
        "the re-grant must hand the nest the SUCCESSOR's derived key"
    );
    assert_ne!(
        granted,
        NestBackupKey::derive(&PREDECESSOR_SEED).to_bytes().to_vec(),
        "re-granting the retired identity's key would leave every future \
         segment sealed to an identity the nest refuses everywhere"
    );
}

/// The stranded-registry half. `backup_destinations` rows are not re-pointed by
/// the succession transaction, so the successor's projection starts empty and
/// every listed destination must be re-registered — not just the key.
#[test]
fn every_listed_destination_is_re_registered_for_the_successor() {
    let nest = Arc::new(FakeNest::default());

    let outcome = block_on(regrant_nest_backup_key(
        nest.clone(),
        SUCCESSOR_SEED,
        &[
            destination("dest-1", DESTINATION_KIND_NEST),
            destination("dest-2", DESTINATION_KIND_NEST),
        ],
    ))
    .expect("regrant");

    assert_eq!(outcome, BackupRegrantOutcome::Regranted { destinations: 2 });
    let ids: Vec<String> = nest
        .registered
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.destination_id.clone())
        .collect();
    assert_eq!(ids, vec!["dest-1".to_string(), "dest-2".to_string()]);
}

/// The idempotent arm. A second device arriving after the first, or the same
/// device at its next store-ready, must write nothing — otherwise every
/// store-ready re-issues a grant and N registrations forever.
#[test]
fn an_already_healed_owner_writes_nothing() {
    let nest = Arc::new(FakeNest::default());
    *nest.enrolled.lock().unwrap() = true;
    nest.projected.lock().unwrap().push("dest-1".into());

    let outcome = block_on(regrant_nest_backup_key(
        nest.clone(),
        SUCCESSOR_SEED,
        &[destination("dest-1", DESTINATION_KIND_NEST)],
    ))
    .expect("regrant");

    assert_eq!(outcome, BackupRegrantOutcome::AlreadyEnrolled);
    let kinds = nest.kinds.lock().unwrap().clone();
    assert!(
        !kinds.contains(&KIND_NEST_KEY_GRANT) && !kinds.contains(&KIND_DESTINATION_REGISTER),
        "a healed owner must not be re-granted or re-registered; kinds: {kinds:?}"
    );
    assert!(
        !BackupRegrantProgress::Settled(outcome).still_owed(),
        "a healed leg is settled, not owed"
    );
    assert!(
        BackupRegrantProgress::Settled(outcome)
            .status_line()
            .is_none(),
        "a no-op must render nothing — a line at every later sign-in trains \
         the user to ignore the one that matters"
    );
}

/// ⚠ The class the `enrolled` flag alone cannot see. An owner whose only
/// destination is their own device **never** grants a `NestBackupKey` (a client
/// custodian seals for itself), so `enrolled` is legitimately false forever. If
/// the idempotency check keyed on it alone, this owner would be re-registered on
/// every store-ready; if it keyed on the grant being owed, their stranded
/// custodian row would never be re-registered at all. Both halves are checked,
/// so a custodian-only owner heals exactly once.
#[test]
fn a_custodian_only_owner_heals_once_and_then_reports_settled() {
    let nest = Arc::new(FakeNest::default());
    let list = [destination("ipad", DESTINATION_KIND_CLIENT_DEVICE)];

    let first =
        block_on(regrant_nest_backup_key(nest.clone(), SUCCESSOR_SEED, &list)).expect("first");
    assert_eq!(first, BackupRegrantOutcome::Regranted { destinations: 1 });
    assert!(
        nest.granted_key.lock().unwrap().is_none(),
        "a custodian-only owner must NOT be handed a NestBackupKey — it would \
         claim an enrollment the owner never asked for"
    );

    nest.kinds.lock().unwrap().clear();
    let second =
        block_on(regrant_nest_backup_key(nest.clone(), SUCCESSOR_SEED, &list)).expect("second");

    assert_eq!(
        second,
        BackupRegrantOutcome::AlreadyEnrolled,
        "the second pass must see the re-registered custodian row and settle — \
         keying idempotency on `enrolled` alone would re-register forever"
    );
}

/// An owner who never configured backups on this box must not be handed an
/// enrollment as a side effect of succeeding — and the leg says so without a
/// round trip.
#[test]
fn an_owner_without_destinations_is_left_alone_without_a_round_trip() {
    let nest = Arc::new(FakeNest::default());

    let outcome =
        block_on(regrant_nest_backup_key(nest.clone(), SUCCESSOR_SEED, &[])).expect("regrant");

    assert_eq!(outcome, BackupRegrantOutcome::NothingConfigured);
    assert!(
        nest.kinds.lock().unwrap().is_empty(),
        "no destinations means no grant, and no question to the nest"
    );
    assert!(
        BackupRegrantProgress::Settled(outcome)
            .status_line()
            .is_none(),
        "nothing owed renders nothing"
    );
}

/// The one settled arm a user must actually see renders; the no-op arms do
/// not; a failure carries its reason.
#[test]
fn the_rendered_arms_are_exactly_the_ones_that_matter() {
    assert!(
        BackupRegrantProgress::Settled(BackupRegrantOutcome::Regranted { destinations: 1 })
            .status_line()
            .is_some(),
        "a completed re-grant tells the user their backups are running again"
    );
    for quiet in [
        BackupRegrantOutcome::NothingConfigured,
        BackupRegrantOutcome::AlreadyEnrolled,
    ] {
        assert!(
            BackupRegrantProgress::Settled(quiet)
                .status_line()
                .is_none(),
            "{quiet:?} renders nothing"
        );
    }

    let failed = BackupRegrantProgress::Failed("nest unreachable".into())
        .status_line()
        .expect("a failure renders");
    assert_eq!(
        failed.args.get("reason").map(String::as_str),
        Some("nest unreachable"),
        "the failure line carries the reason the caller already has"
    );
}

// ── Adjudicating what the aftermath carries across ────────────────────
//
// The seed is account access, so the seed thief this
// whole ceremony answers could have added a destination pointing at a box of
// their choosing at any point in the pre-succession window, indistinguishable
// from one the owner added. The aftermath then re-registers it with no user
// gesture, and the Settings line reports the leg done.
//
// The ratified answer (`succession-aftermath.md` § Re-key scope → *Adjudicating
// what the aftermath carries across*) mirrors the MLS sweep's treatment of
// leaves it cannot vouch for, two rows above in the same table: re-register
// immediately — backups restarting is the leg's whole point and must not become
// user-gated — then **report** every carried-across row until the owner keeps or
// removes it.

/// **The finding, as a test.** A destination the successor never approved is
/// both re-registered and marked. Either half alone is the bug: re-registering
/// without marking is the bug itself, and marking without
/// re-registering would gate the backups the leg exists to restart.
#[test]
fn a_destination_the_successor_never_approved_is_re_registered_and_marked() {
    let nest = Arc::new(FakeNest::default());
    let store = raised_store(vec![destination("attacker-box", DESTINATION_KIND_NEST)]);

    let state = store.state(BOUND_BOX);
    let outcome = block_on(regrant_nest_backup_key(
        nest.clone(),
        SUCCESSOR_SEED,
        &state.backup.destinations,
    ))
    .expect("leg 2");
    assert_eq!(
        outcome,
        BackupRegrantOutcome::Regranted { destinations: 1 },
        "the row must be re-registered — backups restarting is not user-gated"
    );
    assert_eq!(
        nest.registered
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.destination_id.as_str())
            .collect::<Vec<_>>(),
        vec!["attacker-box"],
        "the row reached the source nest, which is exactly why it must also be reported"
    );

    assert!(
        unattested(&state, "attacker-box"),
        "a destination carried across a succession must be marked un-adjudicated — \
         without it the owner is never asked about a row a seed thief could have planted"
    );
    let mark = state
        .marks
        .iter()
        .find(|m| m.destination_id == "attacker-box")
        .expect("the mark rides its own row, not the list");
    assert_eq!(
        mark.predecessor,
        predecessor(),
        "the mark names the raising event — the predecessor whose ledger it came from"
    );
    assert_eq!(mark.verdict, UnattestedVerdict::Open);
}

/// **The raise is the account's, not a box's**: a mark names no box, so every
/// destination listed on ANY of the account's boxes is raised.
#[test]
fn the_raise_marks_every_destination_listed_on_any_box() {
    let store = FakeBackupStateStore::empty();
    store.seed_list(BOUND_BOX, vec![destination("here", DESTINATION_KIND_NEST)]);
    store.seed_list(OTHER_BOX, vec![destination("there", DESTINATION_KIND_NEST)]);

    assert!(block_on(raise_succession_destination_marks(&store, predecessor())).expect("raise"));

    let mut marked: Vec<(String, UnattestedVerdict)> = store
        .marks()
        .into_iter()
        .filter(|m| m.predecessor == predecessor())
        .map(|m| (m.destination_id, m.verdict))
        .collect();
    marked.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        marked,
        vec![
            ("here".to_string(), UnattestedVerdict::Open),
            ("there".to_string(), UnattestedVerdict::Open),
        ]
    );
    // Each box still reads its own list, with the account's marks.
    assert!(unattested(&store.state(BOUND_BOX), "here"));
    assert!(unattested(&store.state(OTHER_BOX), "there"));
}

/// No list anywhere: nothing to raise, nothing written, and the raise says so.
#[test]
fn the_raise_with_no_lists_writes_nothing_and_answers_false() {
    let store = FakeBackupStateStore::empty();
    assert!(!block_on(raise_succession_destination_marks(&store, predecessor())).expect("raise"));
    assert_eq!(store.writes(), 0);
    assert!(store.marks().is_empty());
}

/// **Keep closes an item, never the row forever.** A verdict about one
/// compromise window cannot vouch for the row across the next one, so a
/// *second* succession legitimately re-raises a row the owner kept after the
/// first. A permanent `adjudicated: bool` would pass the test above and
/// silently fail this one.
#[test]
fn keep_closes_this_raising_event_and_a_later_succession_re_raises_the_row() {
    let store = raised_store(vec![destination("kept-box", DESTINATION_KIND_NEST)]);

    // The owner presses Keep.
    let (state, kept) = block_on(mutate_backup(&store, BOUND_BOX, |st| {
        keep_backup_destination(st, "kept-box")
    }))
    .expect("keep");
    assert!(
        kept,
        "Keep reports that it actually adjudicated a marked row"
    );
    assert!(
        !unattested(&state, "kept-box"),
        "after Keep the row renders clean"
    );
    assert_eq!(
        state
            .marks
            .iter()
            .map(|m| m.verdict.clone())
            .collect::<Vec<_>>(),
        vec![UnattestedVerdict::Kept],
        "the mark is KEPT at rest carrying the verdict, never deleted — a deleted \
         mark is indistinguishable from one never raised"
    );

    // A second succession: the successor is now itself a predecessor.
    let second_predecessor = ActorKeypair::from_secret(SUCCESSOR_SEED).actor_id();
    assert!(
        block_on(raise_succession_destination_marks(
            &store,
            second_predecessor
        ))
        .expect("second raise")
    );

    let state = store.state(BOUND_BOX);
    assert!(
        unattested(&state, "kept-box"),
        "a row kept after the FIRST compromise window is raised again by the SECOND — \
         Keep closed an item, not the row"
    );
    let mut events: Vec<_> = state
        .marks
        .iter()
        .map(|m| (m.predecessor, m.verdict.clone()))
        .collect();
    events.sort_by_key(|(p, _)| p.0);
    let mut expected = vec![
        (predecessor(), UnattestedVerdict::Kept),
        (second_predecessor, UnattestedVerdict::Open),
    ];
    expected.sort_by_key(|(p, _)| p.0);
    assert_eq!(
        events, expected,
        "two raising events are two marks: the first stays answered, the second \
         is its own open question"
    );
}

/// **The re-run half of the same keying rule.** A raise for the SAME event
/// never demotes a decided verdict: the mark row's join keeps `Kept`, so a
/// re-run re-asks nothing and stacks no second mark.
#[test]
fn a_re_run_of_the_same_raise_never_demotes_a_kept_verdict() {
    let store = raised_store(vec![destination("kept-box", DESTINATION_KIND_NEST)]);
    block_on(mutate_backup(&store, BOUND_BOX, |st| {
        keep_backup_destination(st, "kept-box")
    }))
    .expect("keep");

    block_on(raise_succession_destination_marks(&store, predecessor())).expect("re-run");

    let state = store.state(BOUND_BOX);
    assert!(
        !unattested(&state, "kept-box"),
        "a re-run of the SAME raising event re-asked a question the owner answered"
    );
    assert_eq!(
        state.marks.len(),
        1,
        "and it did not stack a second mark for the same event"
    );
    assert_eq!(state.marks[0].verdict, UnattestedVerdict::Kept);
}

/// **Remove is the other verdict, and it must be recorded** — the row is what
/// a stale peer's newer list carries back, so a removal that left nothing at
/// rest would be undone by an ordinary write on another device. The read fold
/// enforces this verdict; this pin is that it gets written at all.
#[test]
fn removing_a_raised_destination_records_the_verdict_the_fold_enforces() {
    let store = raised_store(vec![
        destination("thief-box", DESTINATION_KIND_NEST),
        destination("mine", DESTINATION_KIND_NEST),
    ]);

    let mut state = store.state(BOUND_BOX);
    assert!(remove_backup_destination(&mut state, "thief-box"));

    let recorded: Vec<_> = state
        .marks
        .iter()
        .map(|m| (m.destination_id.as_str(), m.verdict.clone()))
        .collect();
    assert!(
        recorded.contains(&("thief-box", UnattestedVerdict::Removed)),
        "the removal of a row under review IS the owner's verdict, and it has to \
         outlive the row: {recorded:?}"
    );
    assert!(
        recorded.contains(&("mine", UnattestedVerdict::Open)),
        "and it answers only the destination it was pressed on: {recorded:?}"
    );
}

/// Keep is scoped to the destination the owner actually pressed it on. A
/// destination backing up several reserved sets shares one `destination_id`
/// across rows, so Keep must clear every row of that id and nothing else — the
/// same nest-level-edit rule `edit_backup_destination` follows.
#[test]
fn keep_clears_every_row_of_that_destination_and_no_other() {
    let mut second_set = destination("kept-box", DESTINATION_KIND_NEST);
    second_set.folder_name = "__conv".into();
    let store = raised_store(vec![
        destination("kept-box", DESTINATION_KIND_NEST),
        second_set,
        destination("other-box", DESTINATION_KIND_NEST),
    ]);

    let mut state = store.state(BOUND_BOX);
    assert!(keep_backup_destination(&mut state, "kept-box"));

    let under_review: Vec<&str> = state
        .backup
        .destinations
        .iter()
        .filter(|d| unattested(&state, &d.destination_id))
        .map(|d| d.destination_id.as_str())
        .collect();
    assert_eq!(
        under_review,
        vec!["other-box"],
        "Keep clears both rows sharing the kept destination_id, and leaves the rest raised"
    );
    assert!(
        !keep_backup_destination(&mut state, "kept-box"),
        "a second Keep on an already-adjudicated row reports that it changed nothing"
    );
}

/// A row the **owner** added after the raise is never marked: the mark says
/// "this crossed a succession boundary", and the raise runs once per ceremony
/// (off the park). Without this, every destination would eventually render as
/// needing review and the mark would mean nothing.
#[test]
fn an_owner_added_destination_after_the_raise_is_never_marked() {
    let store = raised_store(vec![destination("carried", DESTINATION_KIND_NEST)]);

    let (state, ()) = block_on(mutate_backup(&store, BOUND_BOX, |st| {
        st.backup = BackupConfig {
            destinations: [
                st.backup.destinations.clone(),
                vec![destination("mine", DESTINATION_KIND_NEST)],
            ]
            .concat(),
        };
    }))
    .expect("the owner adds a destination");

    assert!(unattested(&state, "carried"));
    assert!(
        !unattested(&state, "mine"),
        "an owner's own row must never be raised for review"
    );
}
