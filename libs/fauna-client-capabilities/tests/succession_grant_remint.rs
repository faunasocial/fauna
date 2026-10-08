//! The capability-grant re-mint leg of the post-succession **aftermath**
//! (`succession-aftermath.md` § Re-key scope, the capability-grants row:
//! *"Revoke all; successor re-mints from the grant ledger …, each re-minted
//! entry marked un-adjudicated"*). The grant ledger is the succession ledger
//! (`fauna.state.succession-ledger`); the leg runs in the post-store-ready
//! pass, after the chain re-point and the grant-mark raise.
//!
//! These tests pin the driver end to end over a fake nest (the holder roster
//! and the deposits) and the shared in-memory ledger double: the derived
//! idempotency (the ledger is its own progress record — a second pass finds
//! only successor-signed grants and does nothing), the
//! holder-must-be-on-the-live-roster rule (owed, never minted
//! classical-blind), the verbatim event-scope carry (the bounded regime marker
//! survives), the preserved window (a re-mint must never extend what the owner
//! consented to), and the marks' carry onto the replacement grant id.

use std::sync::{Arc, Mutex};

use fauna_client_testkit::{ClassifiedError, block_on};

use fauna_client_capabilities::grant_log::{
    self, bounded_mail_event_scope, current_grants, is_bounded_mail_grant,
};
use fauna_client_capabilities::{
    GrantRemintOutcome, GrantRemintProgress, remint_capability_grants,
};
use fauna_client_config::test_helpers::FakeSuccessionLedgerStore;
use fauna_client_config::{SuccessionLedgerStore, keep_grant_mark};
use fauna_core::data::{GrantUnattestedMark, MailConfig, Timestamp};
use fauna_core::grant_event::GrantEventScope;
use fauna_core::identity::ActorKeypair;
use fauna_core::succession_ledger::SuccessionLedger;
use fauna_mls::wrapped_blob::{GrantBlob, ScopeTuple};
use fauna_protocol::RpcRequester;
use fauna_protocol::wrapped_blob::{
    FetchBridgePubkeyReply, ListServiceUsersReply, MintGrantReply, MintGrantRequest,
    ServiceUserInfo,
};

/// The predecessor's seed — the identity whose ledger the grants were minted
/// under, retired by the succession.
const PREDECESSOR_SEED: [u8; 32] = [1u8; 32];
/// The successor's own seed — the identity every re-mint must be signed and
/// owned by.
const SUCCESSOR_SEED: [u8; 32] = [2u8; 32];
/// The content-processor holder's X25519 pubkey, as the ledger records it and
/// as the live roster serves it.
const HOLDER: [u8; 32] = [7u8; 32];

/// In-memory nest: the live holder roster and the minted grant blobs the
/// assertions decode. The grant log is not here — it is the ledger double's.
#[derive(Default)]
struct FakeNest {
    kinds: Mutex<Vec<&'static str>>,
    /// The live roster `list_service_users` answers with. Empty = the holder
    /// vanished.
    roster: Mutex<Vec<(String, String)>>, // (bridge_id, role)
    /// Canonical `GrantBlob` bytes each `fauna.capabilities.mint` deposited.
    minted: Mutex<Vec<Vec<u8>>>,
    /// `Some(n)`: accept `n` more `fauna.capabilities.mint`s, then refuse every
    /// further one (`fauna-client-pair`'s `mint_accepts_before_refusing` shape).
    mint_accepts_before_refusing: Mutex<Option<usize>>,
}

impl FakeNest {
    /// What the nest actually HOLDS: the real deposit is an
    /// `INSERT OR REPLACE` on `(owner, grant_id)` (`bridge_blob_handlers.rs`),
    /// so a re-deposited id replaces its row rather than adding one.
    fn held_blobs(&self) -> Vec<Vec<u8>> {
        /// One row as the nest holds it: the `(owner, grant_id)` replace key
        /// beside the blob bytes stored under it.
        type HeldRow = ((Vec<u8>, Vec<u8>), Vec<u8>);
        let mut held: Vec<HeldRow> = Vec::new();
        for bytes in self.minted.lock().unwrap().iter() {
            let blob = GrantBlob::from_canonical_bytes(bytes).expect("decode held blob");
            let key = (blob.index.0.to_vec(), blob.index.1.to_vec());
            if let Some(row) = held.iter_mut().find(|(k, _)| *k == key) {
                row.1 = bytes.clone();
            } else {
                held.push((key, bytes.clone()));
            }
        }
        held.into_iter().map(|(_, b)| b).collect()
    }
}

/// The record-then-deposit invariant (`ui/nests.md` § Trust facet — grants):
/// **whatever the nest ends up holding, the stored log already names.** A held
/// grant with no `Mint` event is on no page, nameable by no revoke call,
/// discoverable by nothing.
fn assert_every_held_grant_is_named(nest: &Arc<FakeNest>, ledger: &FakeSuccessionLedgerStore) {
    let log = ledger.current();
    for bytes in nest.held_blobs() {
        let blob = GrantBlob::from_canonical_bytes(&bytes).expect("decode");
        assert!(
            log.grant_events.iter().any(|e| {
                e.kind == fauna_core::grant_event::GrantEventKind::Mint
                    && e.grant_id == blob.index.1.clone()
            }),
            "the nest holds grant {:02x?} but the stored log records no Mint for it \
             — unreachable by any page or revoke call",
            blob.index.1
        );
    }
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
            "fauna.bridges.list_service_users" => {
                fauna_protocol::encode_canonical(&ListServiceUsersReply {
                    service_users: self
                        .roster
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|(bridge_id, role)| ServiceUserInfo {
                            bridge_id: bridge_id.clone(),
                            role: role.clone(),
                            status: "approved".into(),
                            ed25519_pubkey: vec![0u8; 32],
                            has_x25519: true,
                            ..Default::default()
                        })
                        .collect(),
                    enrollment_strict: None,
                    extra: Default::default(),
                })
            }
            "fauna.bridges.fetch_bridge_pubkey" => {
                fauna_protocol::encode_canonical(&FetchBridgePubkeyReply {
                    ed25519_pubkey: vec![0u8; 32],
                    x25519_pubkey: HOLDER.to_vec(),
                    mlkem_ek: None,
                    extra: Default::default(),
                })
            }
            "fauna.capabilities.mint" => {
                if let Some(remaining) = self.mint_accepts_before_refusing.lock().unwrap().as_mut()
                {
                    if *remaining == 0 {
                        return Err(ClassifiedError::Transport(
                            "mint refused (test knob)".into(),
                        ));
                    }
                    *remaining -= 1;
                }
                let req: MintGrantRequest =
                    fauna_protocol::decode_strict(&bytes).expect("decode mint");
                let blob_bytes = req.grant_blob.into_vec();
                let blob =
                    GrantBlob::from_canonical_bytes(&blob_bytes).expect("a decodable grant blob");
                let grant_id = fauna_protocol::ByteBuf::from(blob.index.1.clone());
                self.minted.lock().unwrap().push(blob_bytes);
                fauna_protocol::encode_canonical(&MintGrantReply {
                    grant_id,
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

fn master_mail_scope() -> GrantEventScope {
    GrantEventScope {
        class: ScopeTuple::CLASS_CONTENT_READ.to_string(),
        kind: Some(ScopeTuple::KIND_MAIL.to_string()),
        tier: None,
    }
}

fn predecessor_kp() -> ActorKeypair {
    ActorKeypair::from_secret(PREDECESSOR_SEED)
}

fn successor_kp() -> ActorKeypair {
    ActorKeypair::from_secret(SUCCESSOR_SEED)
}

/// The account's mail custody as the successor's device reads it — mail
/// enabled, so the mint payloads derive from the MSEK (`fauna.state.mail`,
/// passed to the re-mint the way the aftermath hands it the store's fold).
fn mail() -> MailConfig {
    MailConfig {
        msek: Some([0x5Au8; 32].into()),
        ..MailConfig::default()
    }
}

/// One predecessor-minted grant to seed: `(grant id, holder, scope, window end)`.
type PredecessorGrant = ([u8; 16], [u8; 32], Vec<GrantEventScope>, u64);

/// The successor's ledger after the pass's legs (a) and (b): the chain
/// re-pointed from the predecessor, the predecessor's live grant `grant` in
/// the log with `scope` and a window ending at `window_end`, and its
/// carried-across mark raised `Open`.
fn successor_ledger_with(grants: &[PredecessorGrant]) -> FakeSuccessionLedgerStore {
    let mut log = SuccessionLedger::empty(successor_kp().actor_id());
    log.prior_actor_ids = vec![predecessor_kp().actor_id()];
    let now = Timestamp::now_secs().max(0) as u64;
    for (i, (grant, holder, scope, window_end)) in grants.iter().enumerate() {
        grant_log::record_mint(
            &mut log,
            predecessor_kp().signing_key(),
            *grant,
            *holder,
            scope.clone(),
            now.saturating_sub(1000),
            *window_end,
            now.saturating_sub(1000) + i as u64,
        )
        .expect("record the predecessor's mint");
    }
    let store = FakeSuccessionLedgerStore::serving(successor_kp().actor_id(), log);
    block_on(store.raise_grant_marks(predecessor_kp().actor_id())).expect("the pass's raise");
    store
}

/// The common shape: one predecessor grant to [`HOLDER`].
fn successor_ledger(scope: Vec<GrantEventScope>, window_end: u64) -> FakeSuccessionLedgerStore {
    successor_ledger_with(&[([0xABu8; 16], HOLDER, scope, window_end)])
}

fn far_future() -> u64 {
    (Timestamp::now_secs().max(0) as u64) + 1_000_000
}

/// An identity that never succeeded holds no prior owners: the pass reads the
/// ledger once, finds nothing predecessor-signed, and spends no round trip at
/// all. This is what every ordinary store-ready pays.
#[test]
fn an_identity_that_never_succeeded_reads_once_and_does_nothing() {
    let nest = Arc::new(FakeNest::default());
    // A ledger the successor owns outright, with a live grant IT minted.
    let mut log = SuccessionLedger::empty(successor_kp().actor_id());
    grant_log::record_mint(
        &mut log,
        successor_kp().signing_key(),
        [0xCDu8; 16],
        HOLDER,
        vec![master_mail_scope()],
        0,
        far_future(),
        1,
    )
    .expect("record own mint");
    let ledger = FakeSuccessionLedgerStore::with(log);

    let outcome = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ))
    .expect("run");
    assert_eq!(outcome, GrantRemintOutcome::NothingToRemint);
    let kinds = nest.kinds.lock().unwrap().clone();
    assert!(
        kinds.is_empty(),
        "no round trip on the nothing-owed path, got {kinds:?}"
    );
    assert!(!GrantRemintProgress::Settled(outcome).still_owed());
    assert!(
        GrantRemintProgress::Settled(outcome)
            .status_line()
            .is_none(),
        "the every-store-ready no-op renders nothing"
    );
}

/// The end-to-end leg: a predecessor-minted grant is re-minted under the
/// successor — new derived id, same holder, same scope, same window — the
/// ledger records the succession's revoke and the successor's mint, the mark
/// is carried onto the replacement id, and a SECOND pass finds nothing owed
/// (the ledger is its own progress record).
#[test]
fn a_predecessor_minted_grant_is_reminted_marked_and_idempotent() {
    let nest = Arc::new(FakeNest::default());
    let window_end = far_future();
    let ledger = successor_ledger(vec![master_mail_scope()], window_end);
    *nest.roster.lock().unwrap() = vec![("mda-1".into(), "mda".into())];

    let outcome = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ))
    .expect("run");
    assert_eq!(
        outcome,
        GrantRemintOutcome::Reminted {
            reminted: 1,
            owed: 0
        }
    );
    assert!(!GrantRemintProgress::Settled(outcome).still_owed());

    // The minted blob: owned by the SUCCESSOR, sealed to the same holder, the
    // window preserved verbatim (never extended past the owner's consent).
    let minted = nest.minted.lock().unwrap().clone();
    assert_eq!(minted.len(), 1);
    let blob = GrantBlob::from_canonical_bytes(&minted[0]).expect("decode");
    assert_eq!(blob.index.0.as_slice(), successor_kp().actor_id().0);
    assert_eq!(blob.holder.as_ref(), HOLDER);
    assert_eq!(blob.window.1, window_end, "the window is the ORIGINAL's");
    assert_ne!(
        blob.index.1.as_slice(),
        [0xABu8; 16],
        "a fresh (derived) id — revocation is terminal, ids are never reused"
    );

    // The ledger: the old grant is dark (its succession revocation recorded),
    // exactly one live grant remains — the replacement — and the mark was
    // carried onto it with the raising predecessor intact.
    let log = ledger.current();
    let live = current_grants(&log);
    assert_eq!(live.len(), 1, "old id dark, replacement live");
    assert_eq!(live[0].grant_id, blob.index.1.clone());
    assert_eq!(live[0].scope, vec![master_mail_scope()], "scope verbatim");
    let carried: Vec<&GrantUnattestedMark> = log
        .unattested_grant_marks
        .iter()
        .filter(|m| m.grant_id == blob.index.1)
        .collect();
    assert_eq!(
        carried.len(),
        1,
        "the mark follows the row the Nests page renders"
    );
    assert_eq!(
        carried[0].predecessor,
        predecessor_kp().actor_id(),
        "the raising event survives the carry"
    );

    // Second pass: everything live is successor-signed — nothing owed, no
    // roster RPC, no second mint.
    nest.kinds.lock().unwrap().clear();
    let second = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ))
    .expect("second run");
    assert_eq!(second, GrantRemintOutcome::NothingToRemint);
    assert_eq!(nest.minted.lock().unwrap().len(), 1, "no duplicate mint");
    let kinds = nest.kinds.lock().unwrap().clone();
    assert!(!kinds.contains(&"fauna.bridges.list_service_users"));

    // Keep RECORDS the verdict and reports a real adjudication happened; it
    // never deletes the mark (an answered mark that vanished would be
    // indistinguishable from one never raised — `UnattestedVerdict`).
    let mark_id = blob.index.1.to_vec();
    assert!(block_on(keep_grant_mark(&ledger, &mark_id)).expect("keep"));
    let log = ledger.current();
    assert!(
        !GrantUnattestedMark::any_open(&log.unattested_grant_marks, &mark_id),
        "the row must stop asking"
    );
    assert!(
        !block_on(keep_grant_mark(&ledger, &mark_id)).expect("second keep"),
        "a second Keep is a no-op"
    );

    // The property the verdict-at-rest shape is for: re-running the raiser —
    // which the post-store-ready pass does at every store-ready — never
    // re-asks an answered question.
    block_on(ledger.raise_grant_marks(predecessor_kp().actor_id())).expect("re-raise");
    assert!(
        !GrantUnattestedMark::any_open(&ledger.current().unattested_grant_marks, &mark_id),
        "a re-run of the raising pass must not re-ask a question already answered"
    );
}

/// A holder absent from the live roster stays OWED — the grant is never minted
/// classical-blind (re-resolving the seal target from the roster is what stops
/// a PQ downgrade), and the pass reports itself unfinished so it retries.
#[test]
fn a_holder_absent_from_the_roster_stays_owed_and_nothing_is_minted() {
    let nest = Arc::new(FakeNest::default());
    let ledger = successor_ledger(vec![master_mail_scope()], far_future());
    // The roster is EMPTY: the holder vanished (revoked bridge, wrong nest).

    let outcome = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ))
    .expect("run");
    assert_eq!(
        outcome,
        GrantRemintOutcome::Reminted {
            reminted: 0,
            owed: 1
        }
    );
    assert!(
        nest.minted.lock().unwrap().is_empty(),
        "nothing minted blind"
    );
    assert!(GrantRemintProgress::Settled(outcome).still_owed());
    assert!(
        GrantRemintProgress::Settled(outcome)
            .status_line()
            .is_some(),
        "partly-owed renders — progress was real AND the pass is unfinished"
    );

    // The predecessor-era grant is still live in the ledger (no revoke was
    // recorded for a grant that was not replaced), so the next pass retries.
    assert_eq!(current_grants(&ledger.current()).len(), 1);
}

/// An expired grant is not re-minted: its window ended, so there is no consent
/// left to restore — re-minting it would silently extend the owner's grant.
#[test]
fn an_expired_grant_is_left_dead() {
    let nest = Arc::new(FakeNest::default());
    // Window ended long ago.
    let ledger = successor_ledger(vec![master_mail_scope()], 10);
    *nest.roster.lock().unwrap() = vec![("mda-1".into(), "mda".into())];

    let outcome = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ))
    .expect("run");
    assert_eq!(outcome, GrantRemintOutcome::NothingToRemint);
    assert!(nest.minted.lock().unwrap().is_empty());
}

/// A bounded mail grant re-mints through the bounded path — per-epoch wraps,
/// never the standing secret — and its event scope (the log-side regime
/// marker included) is carried VERBATIM onto the replacement's mint event.
#[test]
fn a_bounded_mail_grant_reminted_keeps_its_regime_marker_and_epoch_wraps() {
    let nest = Arc::new(FakeNest::default());
    let ledger = successor_ledger(vec![bounded_mail_event_scope()], far_future());
    *nest.roster.lock().unwrap() = vec![("mda-1".into(), "mda".into())];

    let outcome = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ))
    .expect("run");
    assert_eq!(
        outcome,
        GrantRemintOutcome::Reminted {
            reminted: 1,
            owed: 0
        }
    );
    let minted = nest.minted.lock().unwrap().clone();
    let blob = GrantBlob::from_canonical_bytes(&minted[0]).expect("decode");
    assert!(
        blob.wrapped_keys.iter().all(|w| w.epoch.is_some()),
        "every wrap is per-epoch — the bounded path, never the standing secret"
    );
    let live = current_grants(&ledger.current());
    assert_eq!(live.len(), 1);
    assert!(
        is_bounded_mail_grant(&live[0].scope),
        "the log-side regime marker survives the re-mint verbatim"
    );
}

/// A per-labeler grant (a `wasm` mail-labeler subscription's twin) re-mints
/// **per labeler**: every replacement tuple and wrap stays confined to the
/// labeler's factor, and the folded factor survives on the replacement's
/// event so the subscription still finds its grant. A re-mint that dropped
/// the factor would hand the holder the composed mail read the owner never
/// consented to.
#[test]
fn a_per_labeler_grant_reminted_stays_confined_to_its_labeler() {
    let nest = Arc::new(FakeNest::default());
    let labeler = fauna_core::identity::ActorId([0xCDu8; 32]);
    let factor = fauna_core::scoring::labeler_factor(&labeler);
    let ledger = successor_ledger(
        grant_log::bounded_mail_labeler_event_scope(&labeler),
        far_future(),
    );
    *nest.roster.lock().unwrap() = vec![("mda-1".into(), "mda".into())];

    let outcome = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ))
    .expect("run");
    assert_eq!(
        outcome,
        GrantRemintOutcome::Reminted {
            reminted: 1,
            owed: 0
        }
    );
    let minted = nest.minted.lock().unwrap().clone();
    let blob = GrantBlob::from_canonical_bytes(&minted[0]).expect("decode");
    assert!(
        blob.scope
            .iter()
            .all(|t| t.factor.as_deref() == Some(factor.as_str())),
        "every replacement tuple names the labeler's factor"
    );
    assert!(
        blob.wrapped_keys
            .iter()
            .all(|w| w.epoch.is_some() && w.scope.factor.as_deref() == Some(factor.as_str())),
        "every replacement wrap is per-epoch AND confined to the labeler"
    );
    let live = grant_log::current_labeler_grant(&ledger.current(), &labeler)
        .expect("the subscription still finds its re-minted grant");
    assert_eq!(live.grant_id, blob.index.1.to_vec());
}

/// **Record-then-deposit, over the batch.**
/// A pass whose intent write is refused (the door's no-tip refusal, or the
/// crash between a landed deposit and the write that would have named it)
/// must NEVER leave the nest holding a grant the stored log does not name:
/// such a grant is on no page, nameable by no revoke call, discoverable by
/// nothing (there is deliberately no owner-side enumerate). And the failure
/// must not cost the re-mint either: the next pass converges on the same
/// derived replacement id and finishes the job.
#[test]
fn a_refused_ledger_write_never_strands_a_deposit_the_log_cannot_name() {
    let nest = Arc::new(FakeNest::default());
    let ledger = successor_ledger(vec![master_mail_scope()], far_future());
    *nest.roster.lock().unwrap() = vec![("mda-1".into(), "mda".into())];

    // Every ledger write from here on is refused — the crash window, as a knob.
    ledger.refuse_after(0);
    let first = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ));
    assert!(
        first.is_err(),
        "the pass must report the refused ledger write"
    );

    // The invariant, at the worst moment: whatever the nest ended up holding,
    // the stored log already names.
    assert_every_held_grant_is_named(&nest, &ledger);

    // The failure is transient, not a stranding: the un-replaced grant is
    // still predecessor-signed-live in the ledger, so the next pass retries
    // and converges on the same derived replacement.
    ledger.stop_refusing();
    let second = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ))
    .expect("the retry pass runs");
    assert_eq!(
        second,
        GrantRemintOutcome::Reminted {
            reminted: 1,
            owed: 0
        }
    );
    assert_every_held_grant_is_named(&nest, &ledger);

    // Converged: one held replacement, one live ledger row for it, one mark on
    // it, and a third pass finds nothing owed.
    let held = nest.held_blobs();
    assert_eq!(held.len(), 1, "one replacement grant, however many retries");
    let log = ledger.current();
    let live = current_grants(&log);
    assert_eq!(live.len(), 1, "old id dark, replacement live");
    let new_id = GrantBlob::from_canonical_bytes(&held[0])
        .expect("decode")
        .index
        .1
        .clone();
    assert_eq!(live[0].grant_id, new_id);
    assert_eq!(
        log.unattested_grant_marks
            .iter()
            .filter(|m| m.grant_id == new_id)
            .count(),
        1,
        "exactly one mark follows the replacement — never a duplicate re-ask"
    );
    let third = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ))
    .expect("third run");
    assert_eq!(third, GrantRemintOutcome::NothingToRemint);
}

/// **Record, publish, then deposit** (`ui/nests.md` § Trust facet — grants →
/// *Record-then-deposit*, the published form). The intent write lands locally
/// but the bound nest does not acknowledge it: no replacement is deposited — a
/// sibling replica could not yet read the `Mint` its reconcile sweep judges
/// the row by — and the next pass, the nest taking the publish, converges.
#[test]
fn an_unpublished_intent_write_deposits_nothing_and_the_next_pass_converges() {
    let nest = Arc::new(FakeNest::default());
    let ledger = successor_ledger(vec![master_mail_scope()], far_future());
    *nest.roster.lock().unwrap() = vec![("mda-1".into(), "mda".into())];

    ledger.publish_refuses(true);
    let first = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ));
    assert!(
        first.is_err(),
        "the pass reports the unacknowledged publish"
    );
    assert!(nest.held_blobs().is_empty(), "no replacement deposited");

    ledger.publish_refuses(false);
    let second = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ))
    .expect("the retry pass runs");
    assert_eq!(
        second,
        GrantRemintOutcome::Reminted {
            reminted: 1,
            owed: 0
        }
    );
    assert_eq!(
        nest.held_blobs().len(),
        1,
        "one replacement, once published"
    );
    assert_every_held_grant_is_named(&nest, &ledger);
}

/// **The retry the naive inversion would have destroyed.** A refused deposit
/// must leave its candidate selectable — the old event stays
/// latest-live-predecessor-signed until the replacement is known-deposited —
/// so the next pass converges on the same derived id and finishes. (Recording
/// the old id's revoke before the deposit would remove the candidate
/// permanently: the log is append-only and `Revoke` terminal, so the holder
/// would go dark forever, silently.)
#[test]
fn a_refused_deposit_stays_owed_and_the_next_pass_converges() {
    let nest = Arc::new(FakeNest::default());
    let window_end = far_future();
    // Two predecessor grants, so one deposit can land and one be refused.
    let ledger = successor_ledger_with(&[
        ([0xABu8; 16], HOLDER, vec![master_mail_scope()], window_end),
        ([0xACu8; 16], HOLDER, vec![master_mail_scope()], window_end),
    ]);
    *nest.roster.lock().unwrap() = vec![("mda-1".into(), "mda".into())];

    // The nest accepts ONE deposit, then refuses.
    *nest.mint_accepts_before_refusing.lock().unwrap() = Some(1);
    let first = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ))
    .expect("a refused deposit is owed, not an error");
    assert_eq!(
        first,
        GrantRemintOutcome::Reminted {
            reminted: 1,
            owed: 1
        }
    );
    assert!(GrantRemintProgress::Settled(first).still_owed());
    assert_every_held_grant_is_named(&nest, &ledger);

    // Next store-ready, nest healthy again: the owed grant converges.
    *nest.mint_accepts_before_refusing.lock().unwrap() = None;
    let second = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ))
    .expect("retry pass");
    assert_eq!(
        second,
        GrantRemintOutcome::Reminted {
            reminted: 1,
            owed: 0
        }
    );
    assert_every_held_grant_is_named(&nest, &ledger);
    assert_eq!(nest.held_blobs().len(), 2, "both replacements live");

    let log = ledger.current();
    let live = current_grants(&log);
    assert_eq!(live.len(), 2, "old ids dark, both live");
    for grant in &live {
        assert_eq!(
            log.unattested_grant_marks
                .iter()
                .filter(|m| m.grant_id == grant.grant_id)
                .count(),
            1,
            "one mark per replacement, none duplicated, none stranded"
        );
    }

    let third = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ))
    .expect("third run");
    assert_eq!(third, GrantRemintOutcome::NothingToRemint);
}

/// **A failed retirement write is safe, and its retry is clean.** The write
/// that records the old ids' revokes and carries the marks can fail AFTER the
/// deposits landed; the replacements are then live AND recorded (the
/// invariant holds), and the next pass finishes the retirement without
/// recording a duplicate `Mint` or re-raising an answered mark.
#[test]
fn a_failed_retirement_write_retries_without_duplicating_events_or_marks() {
    let nest = Arc::new(FakeNest::default());
    let ledger = successor_ledger(vec![master_mail_scope()], far_future());
    *nest.roster.lock().unwrap() = vec![("mda-1".into(), "mda".into())];

    // Accept the pre-deposit intent write, refuse the retirement write.
    ledger.refuse_after(1);
    let first = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ));
    assert!(first.is_err(), "the failed retirement write must surface");
    assert_every_held_grant_is_named(&nest, &ledger);

    // Next store-ready: the old id is still predecessor-signed-live (its
    // revoke never landed), so the pass re-runs and completes the retirement.
    ledger.stop_refusing();
    let second = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ))
    .expect("retry pass");
    assert_eq!(
        second,
        GrantRemintOutcome::Reminted {
            reminted: 1,
            owed: 0
        }
    );
    assert_every_held_grant_is_named(&nest, &ledger);

    let held = nest.held_blobs();
    assert_eq!(held.len(), 1, "the retries converged on ONE replacement");
    let new_id = GrantBlob::from_canonical_bytes(&held[0])
        .expect("decode")
        .index
        .1
        .clone();
    let log = ledger.current();
    assert_eq!(
        log.grant_events
            .iter()
            .filter(|e| {
                e.kind == fauna_core::grant_event::GrantEventKind::Mint && e.grant_id == new_id
            })
            .count(),
        1,
        "the interrupted pass's Mint is reused, never re-recorded"
    );
    assert_eq!(
        log.unattested_grant_marks
            .iter()
            .filter(|m| m.grant_id == new_id)
            .count(),
        1,
        "exactly one mark — a retried retirement never re-raises the question"
    );
    let live = current_grants(&log);
    assert_eq!(live.len(), 1, "old id dark, replacement live");

    let third = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ))
    .expect("third run");
    assert_eq!(third, GrantRemintOutcome::NothingToRemint);
}

/// The custody kind's succession arm (W8.2 (account-data-plane.md § Workstreams), T13's succession bullet): a
/// custody grant under a retired key re-mints its keyless nest row + event
/// pair even though its holder — the ceremony-pinned custodian device key —
/// is on NO bridge-holder roster (the roster gate is a PQ-downgrade guard for
/// key-bearing wraps; a keyless grant has nothing to downgrade). The
/// re-signed admission WITNESS is deliberately not this sweep's product —
/// the ceremony's re-offer delivers it, and until then the re-minted row
/// admits nothing.
#[test]
fn a_custody_grant_reminted_keyless_without_any_roster_membership() {
    use fauna_client_capabilities::custody_grants::{
        custody_event_scopes, custody_set_from_scopes,
    };
    use fauna_core::custody_grant::CustodyScopeSet;

    const CUSTODIAN: [u8; 32] = [0xC5u8; 32];
    let nest = Arc::new(FakeNest::default());
    let window_end = far_future();
    let set = CustodyScopeSet::Scopes(vec![
        "state".to_string(),
        format!("content:conv:{}", "2b".repeat(32)),
    ]);
    // The predecessor's custody grant, minted to the custodian device key
    // (never a roster holder).
    let ledger = successor_ledger_with(&[(
        [0xCDu8; 16],
        CUSTODIAN,
        custody_event_scopes(&set),
        window_end,
    )]);
    // The roster stays EMPTY — the point of the test.

    let outcome = block_on(remint_capability_grants(
        nest.clone(),
        SUCCESSOR_SEED,
        &ledger,
        None,
        &mail(),
    ))
    .expect("run");
    assert_eq!(
        outcome,
        GrantRemintOutcome::Reminted {
            reminted: 1,
            owed: 0
        }
    );

    // The deposited row: keyless, custody-shaped, holder carried from the log.
    let minted = nest.minted.lock().unwrap().clone();
    assert_eq!(minted.len(), 1);
    let blob = GrantBlob::from_canonical_bytes(&minted[0]).expect("decode");
    assert!(blob.wrapped_keys.is_empty(), "custody is keyless always");
    assert_eq!(blob.holder, CUSTODIAN);
    assert!(
        blob.scope
            .iter()
            .all(|t| t.class == ScopeTuple::CLASS_CUSTODY),
        "custody never mixes classes: {:?}",
        blob.scope
    );

    // The ledger: the replacement is live, successor-signed, with the
    // declared set carried verbatim (reconstructible), and the window
    // preserved.
    let live = current_grants(&ledger.current());
    assert_eq!(live.len(), 1);
    assert_eq!(custody_set_from_scopes(&live[0].scope), Some(set));
    assert_eq!(
        live[0].window_end, window_end,
        "a re-mint must never extend what the owner consented to"
    );
    assert_ne!(live[0].grant_id.as_slice(), &[0xCDu8; 16]);
}
