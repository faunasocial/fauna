//! Integration tests for `MailSettingsMachine::apply_spam_model_write` — the
//! client-side tier-1 spam-model WRITE loop (fetch sealed → unwrap → mutate →
//! re-seal → write back opaque), driven against the in-memory fakes.
//!
//! The orchestrator's two nest-signal dispatch decisions (co-design
//! tracked internally) are exercised end-to-end here: the
//! feature-presence gate (`spam-model-sealed-at-rest` present ⇒ re-seal; absent ⇒
//! no sealed write possible, the model half is skipped). The reseal is always the X-Wing suite — no
//! capability token gates it since the 2026-09-24 ruling (`post-quantum.md`
//! § Capability negotiation). The pure decision table itself is unit-tested in
//! `fauna_mail::spam::model_write::dispatch_for`.

use std::sync::Arc;

use fauna_client_capabilities::grant_log;
use fauna_client_mail_settings::error::{DispatchError, NestError};
use fauna_client_mail_settings::machine::SealedModelWriter;
use fauna_client_mail_settings::testing::{
    FakeMailStore, FakeNestClient, FakeSigner, FakeSuccessionLedgerStore,
};
use fauna_client_mail_settings::{
    HistoryWrite, MailSettingsMachine, ModelWriteOp, MuaInstructions, SpamModelClientWrite,
    SpamModelWriteOutcome,
};
use fauna_core::grant_event::{GrantEventKind, GrantEventScope};
use fauna_core::identity::ActorId;
use fauna_mail::spam::model_write::unwrap_sealed_history_delta;
use fauna_mail::spam::{SpamLabel, SpamModel};
use fauna_mail::{open_sealed_inner_record, open_sealed_inner_record_hybrid};
use fauna_mls::wrapped_blob::{
    SpamModelCopyBlob, derive_recipient_hpke_keypair, derive_recipient_xwing_keypair,
    seal_to_recipient, unseal_spam_model_copy,
};
use fauna_protocol::bridge_routing::{SpamHistoryOp, SpamLabel as WireSpamLabel, TrainingSource};
use fauna_protocol::discovery::capability::SPAM_MODEL_SEALED_AT_REST;
use fauna_protocol::wrapped_blob::HolderSealTarget;

const ACTOR: [u8; 32] = [0x11; 32];
const SIGNER_SEED: [u8; 32] = [0x22; 32];
/// A deterministic MSEK stand-in the recipient keypairs derive from.
const MSEK: [u8; 32] = [0x37; 32];

/// Build a machine whose mail custody holds `MSEK` (mail enabled) unless
/// `mail_enabled` is false, and whose fake nest advertises `caps`.
fn build(
    caps: &[&str],
    mail_enabled: bool,
) -> (
    MailSettingsMachine,
    FakeNestClient,
    FakeSuccessionLedgerStore,
) {
    let nest = FakeNestClient::new();
    nest.state().advertised_capabilities = caps.iter().map(|c| c.to_string()).collect();

    // The grant log keeps only events the chain signed, so the ledger is the
    // signer's identity.
    let cfg = FakeSuccessionLedgerStore::empty(
        fauna_core::identity::ActorKeypair::from_secret(SIGNER_SEED).actor_id(),
    );
    let mail = FakeMailStore::empty();
    if mail_enabled {
        mail.seed(&fauna_core::data::MailConfig {
            msek: Some(MSEK.into()),
            ..Default::default()
        });
    }

    let signer = FakeSigner::new(SIGNER_SEED);
    let machine = MailSettingsMachine::new(
        ACTOR,
        Arc::new(nest.clone()),
        Arc::new(cfg.clone()),
        Arc::new(mail),
        Arc::new(signer),
        MuaInstructions::placeholder(),
    );
    (machine, nest, cfg)
}

fn recipient_keys() -> ([u8; 32], [u8; 32]) {
    derive_recipient_hpke_keypair(&MSEK)
}

/// Open a blob the machine sealed to the actor's own key. The machine always
/// reseals with the X-Wing suite; the hybrid opener reads either suite.
fn open_own(blob: &[u8]) -> Vec<u8> {
    let (secret, _) = recipient_keys();
    let xwing = derive_recipient_xwing_keypair(&MSEK);
    open_sealed_inner_record_hybrid(blob, &secret, xwing.secret.mlkem_decaps_key())
        .expect("unseal own-key blob")
}

#[tokio::test]
async fn train_seals_and_writes_back_when_capability_present() {
    // Seed an initial *classically* sealed model with one ham event, so the
    // orchestrator must fetch → unwrap → mutate → re-seal (a full read-mutate-write,
    // not just a fresh model).
    let (_secret, public) = recipient_keys();
    let mut start = SpamModel::new();
    start.train_ham("weekly team sync notes");
    let sealed_start = seal_to_recipient(&start.to_bytes(), &public)
        .expect("seal start")
        .to_canonical_bytes()
        .expect("encode");

    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    nest.state().spam_model = Some(sealed_start);

    let out = machine
        .apply_spam_model_write(
            ModelWriteOp::Train {
                text: "buy cheap pills now".into(),
                label: SpamLabel::Spam,
            },
            None,
        )
        .await
        .expect("client write");

    let delta = match out {
        SpamModelWriteOutcome::Sealed {
            sample_count,
            delta,
        } => {
            assert_eq!(sample_count, 2, "1 ham + 1 spam");
            assert!(!delta.is_empty(), "a Train carries its forward delta");
            delta
        }
        SpamModelWriteOutcome::ServerPath => panic!("expected Sealed, got ServerPath"),
        SpamModelWriteOutcome::Duplicate { .. } => panic!("expected Sealed, got Duplicate"),
    };
    assert_eq!(delta, SpamModel::delta_ngrams("buy cheap pills now"));

    // The nest now holds the re-sealed blob; it unwraps under the actor's own
    // secret to the mutated model (the nest never saw this plaintext).
    let stored = nest
        .state()
        .spam_model
        .clone()
        .expect("put_spam_model stored a blob");
    let plaintext = open_own(&stored);
    let round = SpamModel::from_bytes(&plaintext).expect("decode");
    assert_eq!(round.spam_messages, 1);
    assert_eq!(round.ham_messages, 1);
    assert_eq!(nest.state().put_spam_model_sample_count, Some(2));
}

#[tokio::test]
async fn train_starts_from_fresh_when_untrained() {
    // No seeded model (cold start) ⇒ the op applies to a fresh SpamModel.
    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    assert!(nest.state().spam_model.is_none());

    let out = machine
        .apply_spam_model_write(
            ModelWriteOp::Train {
                text: "act now limited offer".into(),
                label: SpamLabel::Spam,
            },
            None,
        )
        .await
        .expect("client write");
    assert!(matches!(
        out,
        SpamModelWriteOutcome::Sealed {
            sample_count: 1,
            ..
        }
    ));

    let stored = nest.state().spam_model.clone().expect("stored");
    let round = SpamModel::from_bytes(&open_own(&stored)).expect("decode");
    assert_eq!(round.spam_messages, 1);
}

#[tokio::test]
async fn degrades_to_server_path_without_capability() {
    // The load-bearing I1/I2 gate: a nest that hasn't advertised the sealed-at-rest
    // handling still seals-on-read, so re-sealing would double-seal → data loss.
    // The orchestrator must NOT write; the model half is skipped.
    let (machine, nest, _cfg) = build(&[], true);

    let out = machine
        .apply_spam_model_write(
            ModelWriteOp::Train {
                text: "buy cheap pills now".into(),
                label: SpamLabel::Spam,
            },
            None,
        )
        .await
        .expect("dispatch");
    assert_eq!(out, SpamModelWriteOutcome::ServerPath);
    // No sealed write happened.
    assert!(nest.state().spam_model.is_none());
    assert!(nest.state().put_spam_model_sample_count.is_none());
}

#[tokio::test]
async fn degrades_to_server_path_without_mail() {
    // Capability present but the actor has no MSEK (social-only edge): no key to
    // seal under ⇒ no per-user model, nothing written.
    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], false);

    let out = machine
        .apply_spam_model_write(
            ModelWriteOp::Train {
                text: "x".into(),
                label: SpamLabel::Ham,
            },
            None,
        )
        .await
        .expect("dispatch");
    assert_eq!(out, SpamModelWriteOutcome::ServerPath);
    assert!(nest.state().spam_model.is_none());
}

#[tokio::test]
async fn train_always_reseals_hybrid() {
    // The reseal targets the X-Wing suite with no capability token (the nest's
    // seal_recipient_blob gate keys on the published ek alone). Proof: the
    // *classical* opener refuses the stored blob, the *hybrid* opener reads it.
    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);

    let out = machine
        .apply_spam_model_write(
            ModelWriteOp::Train {
                text: "act now limited offer".into(),
                label: SpamLabel::Spam,
            },
            None,
        )
        .await
        .expect("client write");
    assert!(matches!(out, SpamModelWriteOutcome::Sealed { .. }));

    let stored = nest.state().spam_model.clone().expect("stored");
    let (secret, _) = recipient_keys();
    assert!(
        open_sealed_inner_record(&stored, &secret).is_err(),
        "hybrid reseal must NOT be openable classically (no downgrade)"
    );
    let xwing = derive_recipient_xwing_keypair(&MSEK);
    let plaintext =
        open_sealed_inner_record_hybrid(&stored, &secret, xwing.secret.mlkem_decaps_key())
            .expect("hybrid unseal");
    assert_eq!(
        SpamModel::from_bytes(&plaintext)
            .expect("decode")
            .spam_messages,
        1
    );
}

#[tokio::test]
async fn sealed_spam_write_available_reads_capability_and_msek() {
    // The fetch-avoidance pre-check mirrors the dispatch: capability + MSEK ⇒
    // true; either absent ⇒ false (the surface then skips the body fetch and
    // the model half).
    let (machine, _nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    assert!(machine.sealed_spam_write_available().await.expect("read"));

    let (machine, _nest, _cfg) = build(&[], true);
    assert!(!machine.sealed_spam_write_available().await.expect("read"));

    let (machine, _nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], false);
    assert!(!machine.sealed_spam_write_available().await.expect("read"));
}

#[tokio::test]
async fn train_facade_seals_and_reports_scalar_outcome() {
    // The FFI/wasm façade: same composed write as apply_spam_model_write, the
    // outcome flattened to {sealed, sample_count}.
    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);

    let out = machine
        .train_spam_model_client("buy cheap pills now".into(), true)
        .await
        .expect("train");
    assert_eq!(
        out,
        SpamModelClientWrite {
            sealed: true,
            sample_count: 1,
        }
    );

    let stored = nest.state().spam_model.clone().expect("stored");
    let round = SpamModel::from_bytes(&open_own(&stored)).expect("decode");
    assert_eq!(round.spam_messages, 1);
}

#[tokio::test]
async fn train_facade_degrades_to_server_path_without_capability() {
    let (machine, nest, _cfg) = build(&[], true);
    let out = machine
        .train_spam_model_client("x".into(), false)
        .await
        .expect("dispatch");
    assert_eq!(
        out,
        SpamModelClientWrite {
            sealed: false,
            sample_count: 0,
        }
    );
    assert!(nest.state().spam_model.is_none());
}

#[tokio::test]
async fn undo_through_the_seal_restores_prior_model() {
    // A client-path Train then Undo (with the captured forward delta — the 1c
    // delta source, here supplied directly) returns the model byte-for-byte.
    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);

    let trained = machine
        .apply_spam_model_write(
            ModelWriteOp::Train {
                text: "buy cheap pills now".into(),
                label: SpamLabel::Spam,
            },
            None,
        )
        .await
        .expect("train");
    let delta = match trained {
        SpamModelWriteOutcome::Sealed { delta, .. } => delta,
        SpamModelWriteOutcome::ServerPath => panic!("expected Sealed"),
        SpamModelWriteOutcome::Duplicate { .. } => panic!("expected Sealed, got Duplicate"),
    };

    let undone = machine
        .apply_spam_model_write(
            ModelWriteOp::Undo {
                delta,
                label: SpamLabel::Spam,
            },
            None,
        )
        .await
        .expect("undo");
    assert_eq!(
        undone,
        SpamModelWriteOutcome::Sealed {
            sample_count: 0,
            delta: Default::default(),
        }
    );

    let stored = nest.state().spam_model.clone().expect("stored");
    let round = SpamModel::from_bytes(&open_own(&stored)).expect("decode");
    assert_eq!(round, SpamModel::new());
}

// ── History-op composition (build-item 3 write side) ────────────────────────

#[tokio::test]
async fn train_insert_seals_subject_and_delta_into_history_op() {
    // A client-path mail-surface train rides an atomic `Insert`: the machine seals
    // the subject + the forward delta to the actor's OWN key (nest-opaque), and
    // carries the plaintext metadata (message_id / mailbox / label / source).
    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);

    let out = machine
        .apply_spam_model_write(
            ModelWriteOp::Train {
                text: "buy cheap pills now".into(),
                label: SpamLabel::Spam,
            },
            Some(HistoryWrite::Insert {
                message_id: vec![0xAB; 32],
                mailbox: "INBOX".into(),
                subject: "Cheap meds 90% off".into(),
                source: TrainingSource::ManualOther,
            }),
        )
        .await
        .expect("train+insert");
    assert!(matches!(out, SpamModelWriteOutcome::Sealed { .. }));

    let op = nest
        .state()
        .put_spam_model_history_op
        .clone()
        .expect("history op recorded")
        .expect("Some(Insert)");
    let (secret, _) = recipient_keys();
    match op {
        SpamHistoryOp::Insert {
            message_id,
            mailbox,
            sealed_subject,
            sealed_delta,
            label,
            source,
        } => {
            assert_eq!(message_id, vec![0xAB; 32]);
            assert_eq!(mailbox, "INBOX");
            assert_eq!(label, WireSpamLabel::Spam);
            assert_eq!(source, TrainingSource::ManualOther);
            // Opaque to the nest — but unwraps under the actor's own recipient key.
            let subj = open_own(&sealed_subject);
            assert_eq!(String::from_utf8(subj).unwrap(), "Cheap meds 90% off");
            let delta = unwrap_sealed_history_delta(
                &sealed_delta,
                &secret,
                Some(
                    derive_recipient_xwing_keypair(&MSEK)
                        .secret
                        .mlkem_decaps_key(),
                ),
            )
            .expect("unwrap delta");
            assert_eq!(delta, SpamModel::delta_ngrams("buy cheap pills now"));
        }
        other => panic!("expected Insert, got {other:?}"),
    }
}

#[tokio::test]
async fn hybrid_train_insert_seals_history_with_the_xwing_suite() {
    // A PQ-hybrid actor's history row rides the X-Wing suite too (anti-downgrade):
    // the classical opener refuses the sealed subject, the hybrid opener reads it.
    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);

    machine
        .apply_spam_model_write(
            ModelWriteOp::Train {
                text: "act now limited offer".into(),
                label: SpamLabel::Spam,
            },
            Some(HistoryWrite::Insert {
                message_id: vec![0x01; 32],
                mailbox: "Junk".into(),
                subject: "limited offer".into(),
                source: TrainingSource::ImapJunkFlag,
            }),
        )
        .await
        .expect("hybrid train+insert");

    let op = nest
        .state()
        .put_spam_model_history_op
        .clone()
        .flatten()
        .expect("Some(Insert)");
    let SpamHistoryOp::Insert {
        sealed_subject,
        sealed_delta,
        ..
    } = op
    else {
        panic!("expected Insert");
    };
    let (secret, _) = recipient_keys();
    let xwing = derive_recipient_xwing_keypair(&MSEK);
    let mdk = xwing.secret.mlkem_decaps_key();
    assert!(
        open_sealed_inner_record(&sealed_subject, &secret).is_err(),
        "hybrid history seal must NOT be openable classically (no downgrade)"
    );
    let delta = unwrap_sealed_history_delta(&sealed_delta, &secret, Some(mdk))
        .expect("hybrid unwrap delta");
    assert_eq!(delta, SpamModel::delta_ngrams("act now limited offer"));
}

#[tokio::test]
async fn undo_rides_an_atomic_delete_history_op() {
    // A client-side undo of a sealed row: the inverse delta rides `ModelWriteOp::Undo`
    // and the consumed audit row is removed atomically via `SpamHistoryOp::Delete`.
    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    let trained = machine
        .apply_spam_model_write(
            ModelWriteOp::Train {
                text: "buy cheap pills now".into(),
                label: SpamLabel::Spam,
            },
            None,
        )
        .await
        .expect("train");
    let delta = match trained {
        SpamModelWriteOutcome::Sealed { delta, .. } => delta,
        SpamModelWriteOutcome::ServerPath => panic!("expected Sealed"),
        SpamModelWriteOutcome::Duplicate { .. } => panic!("expected Sealed, got Duplicate"),
    };

    machine
        .apply_spam_model_write(
            ModelWriteOp::Undo {
                delta,
                label: SpamLabel::Spam,
            },
            Some(HistoryWrite::Delete {
                history_id: vec![0x5A; 16],
            }),
        )
        .await
        .expect("undo+delete");

    assert_eq!(
        nest.state().put_spam_model_history_op.clone().flatten(),
        Some(SpamHistoryOp::Delete {
            history_id: vec![0x5A; 16],
        })
    );
    // The model is back to empty (the inverse landed).
    let stored = nest.state().spam_model.clone().expect("stored");
    let round = SpamModel::from_bytes(&open_own(&stored)).expect("decode");
    assert_eq!(round, SpamModel::new());
}

#[tokio::test]
async fn unwrap_sealed_history_delta_round_trips_through_the_machine() {
    // The machine unwraps a sealed history delta under the MSEK-derived key (the
    // read half a client-side undo runs) without the key ever leaving Rust.
    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    machine
        .apply_spam_model_write(
            ModelWriteOp::Train {
                text: "win a free prize".into(),
                label: SpamLabel::Spam,
            },
            Some(HistoryWrite::Insert {
                message_id: vec![0x02; 32],
                mailbox: "INBOX".into(),
                subject: "prize".into(),
                source: TrainingSource::ManualOther,
            }),
        )
        .await
        .expect("train+insert");
    let SpamHistoryOp::Insert { sealed_delta, .. } = nest
        .state()
        .put_spam_model_history_op
        .clone()
        .flatten()
        .expect("Some(Insert)")
    else {
        panic!("expected Insert");
    };

    let delta = machine
        .unwrap_sealed_history_delta(&sealed_delta)
        .await
        .expect("unwrap")
        .expect("mail enabled ⇒ Some");
    assert_eq!(delta, SpamModel::delta_ngrams("win a free prize"));
}

// ── Mail-surface train façade (the live `Insert` consumer) ──────────────────

#[tokio::test]
async fn train_mail_facade_seals_an_insert_history_row() {
    // The mail-surface façade: a "mark as spam" gesture trains the model AND
    // seals an atomic audit row (subject + delta sealed to the actor's own key),
    // flattening the outcome to {sealed, sample_count}. This is the live `Insert`
    // consumer the moderation-queue social train (history_op: None) never drives.
    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);

    let out = machine
        .train_spam_model_client_mail(
            "buy cheap pills now".into(),
            true,
            b"thread-ref:7".to_vec(),
            "INBOX".into(),
            "Cheap meds 90% off".into(),
        )
        .await
        .expect("mail train");
    assert_eq!(
        out,
        SpamModelClientWrite {
            sealed: true,
            sample_count: 1,
        }
    );

    // The atomic history op carried the plaintext metadata + the sealed subject/delta.
    let op = nest
        .state()
        .put_spam_model_history_op
        .clone()
        .expect("history op recorded")
        .expect("Some(Insert)");
    let (secret, _) = recipient_keys();
    match op {
        SpamHistoryOp::Insert {
            message_id,
            mailbox,
            sealed_subject,
            sealed_delta,
            label,
            source,
        } => {
            assert_eq!(message_id, b"thread-ref:7".to_vec());
            assert_eq!(mailbox, "INBOX");
            assert_eq!(label, WireSpamLabel::Spam);
            // A first-party button ⇒ ManualOther (not an IMAP \Junk flag/move).
            assert_eq!(source, TrainingSource::ManualOther);
            let subj = open_own(&sealed_subject);
            assert_eq!(String::from_utf8(subj).unwrap(), "Cheap meds 90% off");
            let delta = unwrap_sealed_history_delta(
                &sealed_delta,
                &secret,
                Some(
                    derive_recipient_xwing_keypair(&MSEK)
                        .secret
                        .mlkem_decaps_key(),
                ),
            )
            .expect("unwrap delta");
            assert_eq!(delta, SpamModel::delta_ngrams("buy cheap pills now"));
        }
        other => panic!("expected Insert, got {other:?}"),
    }
}

#[tokio::test]
async fn train_mail_facade_reports_a_duplicate_as_sealed_with_the_count_on_record() {
    // The nest's one-lesson rule (`mail-spam.md` § 3) rejects the whole write:
    // the façade must report the SEALED path (no server-train fallback) with the
    // count still on record — the pre-mutation count — not the count of a write
    // that did not happen; and the fake nest's stored model is untouched.
    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    let (_secret, public) = recipient_keys();
    let mut model = SpamModel::new();
    model.train_spam("first lesson on record");
    model.train_ham("and a ham one");
    let sealed_start = seal_to_recipient(&model.to_bytes(), &public)
        .expect("seal")
        .to_canonical_bytes()
        .expect("canonical");
    nest.state().spam_model = Some(sealed_start.clone());
    nest.state().put_spam_model_outcome =
        fauna_protocol::wrapped_blob::PutSpamModelOutcome::DuplicateSignal;

    let out = machine
        .train_spam_model_client_mail(
            "first lesson on record".into(),
            true,
            b"thread-ref:1".to_vec(),
            "INBOX".into(),
            "Seen this one".into(),
        )
        .await
        .expect("a duplicate is not an error");
    assert_eq!(
        out,
        SpamModelClientWrite {
            sealed: true,
            sample_count: 2,
        },
        "sealed path, count on record (2), not the 3 a written train would report"
    );
    assert_eq!(
        nest.state().spam_model.as_deref(),
        Some(sealed_start.as_slice()),
        "the stored model is byte-identical to the last accepted write"
    );
    assert!(
        nest.state().put_spam_model_history_op.is_none(),
        "the fake, like the nest, recorded no history op for a rejected write"
    );
}

#[tokio::test]
async fn train_mail_facade_degrades_to_server_path_without_capability() {
    // A v-older nest (no sealed-at-rest handling) ⇒ the mail façade must NOT write
    // (a blind re-seal would double-seal → data loss); it reports ServerPath and
    // writes no sealed model AND no history row (there is no server-side train
    // to fall back to).
    let (machine, nest, _cfg) = build(&[], true);
    let out = machine
        .train_spam_model_client_mail(
            "x".into(),
            true,
            b"m:1".to_vec(),
            "INBOX".into(),
            "s".into(),
        )
        .await
        .expect("dispatch");
    assert_eq!(
        out,
        SpamModelClientWrite {
            sealed: false,
            sample_count: 0,
        }
    );
    assert!(nest.state().spam_model.is_none());
    assert!(nest.state().put_spam_model_history_op.is_none());
}

// ── Deployment-baseline holder copy (piece (b) write orchestrator) ──────────

/// A deterministic MSEK stand-in for the *aggregation holder*'s recipient
/// keypair (distinct from the contributor's `MSEK`), so a test can open the
/// sealed holder copy with the holder's own X25519 secret.
const HOLDER_MSEK: [u8; 32] = [0x5C; 32];

fn holder_keys() -> ([u8; 32], [u8; 32]) {
    derive_recipient_hpke_keypair(&HOLDER_MSEK)
}

#[tokio::test]
async fn opted_in_write_attaches_a_holder_copy_the_holder_can_open() {
    // While opted into the deployment baseline, every sealed-model write also
    // attaches a fresh copy sealed to the nest-volunteered aggregation holder —
    // the re-seal-on-every-write rule. The copy is opaque to the nest but opens
    // under the HOLDER's own X25519 secret to the exact post-mutation model.
    let (_holder_secret, holder_public) = holder_keys();
    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    {
        let mut s = nest.state();
        s.contribute_baseline = true;
        s.holder_seal_target = Some(HolderSealTarget {
            x25519_pubkey: holder_public.to_vec(),
            mlkem_ek: None,
            ..Default::default()
        });
    }

    machine
        .apply_spam_model_write(
            ModelWriteOp::Train {
                text: "buy cheap pills now".into(),
                label: SpamLabel::Spam,
            },
            None,
        )
        .await
        .expect("client write");

    let copy = nest
        .state()
        .put_spam_model_holder_copy
        .clone()
        .expect("holder_copy recorded")
        .expect("opted-in ⇒ Some(copy)");
    assert_eq!(
        copy.holder_pubkey,
        holder_public.to_vec(),
        "copy pairs on the volunteered holder pubkey"
    );

    // The nest never sees this plaintext; only the holder's secret opens it.
    let (holder_secret, _) = holder_keys();
    let blob =
        SpamModelCopyBlob::from_canonical_bytes(&copy.sealed_copy).expect("decode copy blob");
    let plaintext = unseal_spam_model_copy(&blob, &holder_secret, None).expect("holder opens copy");
    let model = SpamModel::from_bytes(&plaintext).expect("decode model");
    assert_eq!(
        model.spam_messages, 1,
        "copy carries the post-mutation model"
    );
    assert_eq!(model.ham_messages, 0);

    // And the copy's plaintext equals what the actor's own re-seal stored (same bytes).
    let stored = nest.state().spam_model.clone().expect("stored");
    let own_plain = open_own(&stored);
    assert_eq!(
        plaintext, own_plain,
        "holder copy == the re-sealed model bytes"
    );
}

#[tokio::test]
async fn opted_out_write_attaches_no_holder_copy() {
    // Default (opted out): the write carries no copy even when a holder is
    // enrolled — the toggle bit governs.
    let (_holder_secret, holder_public) = holder_keys();
    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    {
        let mut s = nest.state();
        s.contribute_baseline = false;
        s.holder_seal_target = Some(HolderSealTarget {
            x25519_pubkey: holder_public.to_vec(),
            mlkem_ek: None,
            ..Default::default()
        });
    }

    machine
        .apply_spam_model_write(
            ModelWriteOp::Train {
                text: "act now limited offer".into(),
                label: SpamLabel::Spam,
            },
            None,
        )
        .await
        .expect("client write");
    assert_eq!(
        nest.state().put_spam_model_holder_copy.clone().flatten(),
        None,
        "opted out ⇒ no holder copy"
    );
}

#[tokio::test]
async fn opted_in_without_holder_target_attaches_no_copy() {
    // Opted in, but the nest volunteered no holder (no holder enrolled,
    // `holder_seal_target` absent): the write sets no copy and
    // still succeeds — the plaintext-row merge counts a server-written model.
    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    {
        let mut s = nest.state();
        s.contribute_baseline = true;
        s.holder_seal_target = None;
    }

    machine
        .apply_spam_model_write(
            ModelWriteOp::Train {
                text: "win a free prize".into(),
                label: SpamLabel::Spam,
            },
            None,
        )
        .await
        .expect("client write");
    assert_eq!(
        nest.state().put_spam_model_holder_copy.clone().flatten(),
        None,
        "no volunteered holder ⇒ no copy"
    );
    // But the model itself was still re-sealed and written back.
    assert!(nest.state().spam_model.is_some());
}

#[tokio::test]
async fn insert_with_a_non_train_op_is_rejected() {
    // An `Insert` history variant only makes sense with a `Train` — a mismatched op
    // is a caller bug, surfaced as an error rather than a mislabelled row.
    let (machine, _nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    let err = machine
        .apply_spam_model_write(
            ModelWriteOp::Undo {
                delta: SpamModel::delta_ngrams("x"),
                label: SpamLabel::Spam,
            },
            Some(HistoryWrite::Insert {
                message_id: vec![0x03; 32],
                mailbox: "INBOX".into(),
                subject: "x".into(),
                source: TrainingSource::ManualOther,
            }),
        )
        .await;
    assert!(err.is_err(), "Insert + Undo must be rejected");
}

// ── b3: the deployment-baseline contribute toggle's grant seam ──────────────
//
// `MailSettingsMachine::{attach_and_mint_baseline_grant, revoke_baseline_grant}`
// (the `SealedModelWriter` seam the `mail-spam` toggle drives, `mail-spam.md`
// § Encrypted-mode interaction, piece (b)). The seam runs AFTER the nest bit is
// set (b1-nest volunteers `holder_seal_target` only then), so these tests seed
// `contribute_baseline = true` to model that state.

/// The `content.read{spam-model}` scope every baseline grant carries (keyless).
fn spam_model_scope() -> GrantEventScope {
    GrantEventScope {
        class: "content.read".into(),
        kind: Some("spam-model".into()),
        tier: None,
    }
}

/// The actor identity `FakeSigner(SIGNER_SEED)` signs grant events under.
fn signer_owner() -> ActorId {
    ActorId(FakeSigner::new(SIGNER_SEED).verifying_key().to_bytes())
}

#[tokio::test]
async fn opt_in_mints_keyless_grant_and_attaches_initial_copy() {
    // Opt-in state: a client-sealed model, an enrolled holder, and the bit set.
    let (_holder_secret, holder_public) = holder_keys();
    let (_secret, public) = recipient_keys();
    let mut start = SpamModel::new();
    start.train_spam("buy cheap pills now");
    let sealed_start = seal_to_recipient(&start.to_bytes(), &public)
        .expect("seal start")
        .to_canonical_bytes()
        .expect("encode");

    let (machine, nest, cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    {
        let mut s = nest.state();
        s.spam_model = Some(sealed_start);
        s.contribute_baseline = true;
        s.holder_seal_target = Some(HolderSealTarget {
            x25519_pubkey: holder_public.to_vec(),
            mlkem_ek: None,
            ..Default::default()
        });
    }

    machine
        .attach_and_mint_baseline_grant()
        .await
        .expect("opt-in");

    // (a) A grant blob was deposited on the nest.
    assert_eq!(nest.state().minted_grants.len(), 1, "one grant minted");
    assert!(
        nest.state().revoked_grants.is_empty(),
        "opt-in revokes nothing"
    );

    // (b) The initial holder copy rode a `put_spam_model`, opaque to the nest but
    // openable under the HOLDER's own secret to the current model.
    let copy = nest
        .state()
        .put_spam_model_holder_copy
        .clone()
        .expect("put_spam_model ran")
        .expect("opted-in ⇒ initial copy attached");
    assert_eq!(copy.holder_pubkey, holder_public.to_vec());
    let (holder_secret, _) = holder_keys();
    let blob = SpamModelCopyBlob::from_canonical_bytes(&copy.sealed_copy).expect("decode copy");
    let plaintext = unseal_spam_model_copy(&blob, &holder_secret, None).expect("holder opens copy");
    let model = SpamModel::from_bytes(&plaintext).expect("decode model");
    assert_eq!(model.spam_messages, 1, "copy carries the current model");

    // (c) A signed `Mint` event landed in the grant log, scoped keyless
    // `content.read{spam-model}`, verifying under the owner's identity.
    let stored = cfg.current();
    assert_eq!(stored.grant_events.len(), 1, "one mint event logged");
    let ev = &stored.grant_events[0];
    assert_eq!(ev.kind, GrantEventKind::Mint);
    assert_eq!(ev.scope, vec![spam_model_scope()]);
    assert_eq!(ev.holder, holder_public.to_vec(), "grant is to the holder");
    ev.verify(&signer_owner())
        .expect("mint event verifies under the owner identity");
}

/// The record-then-deposit order at this crate's own minting site — the finding's third call site, which had copied the unsafe order from
/// `fauna-client-pair` *citing it as canonical prior art*.
///
/// When the nest refuses the deposit, the `Mint` event must already be durable.
/// The asymmetry is the point: a grant the nest holds and the log does not is
/// undiscoverable forever (the page projects the log, `revoke` needs an id only
/// the log carries, and the wire has no owner-side enumerate), while a grant the
/// log holds and the nest does not is a visible row an idempotent revoke clears.
#[tokio::test]
async fn a_refused_deposit_finds_the_mint_already_recorded() {
    let (_holder_secret, holder_public) = holder_keys();
    let (_secret, public) = recipient_keys();
    let mut start = SpamModel::new();
    start.train_spam("buy cheap pills now");
    let sealed_start = seal_to_recipient(&start.to_bytes(), &public)
        .expect("seal start")
        .to_canonical_bytes()
        .expect("encode");

    let (machine, nest, cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    {
        let mut s = nest.state();
        s.spam_model = Some(sealed_start);
        s.contribute_baseline = true;
        s.holder_seal_target = Some(HolderSealTarget {
            x25519_pubkey: holder_public.to_vec(),
            mlkem_ek: None,
            ..Default::default()
        });
        s.fail_mint_grant_with = Some(fauna_client_mail_settings::NestError::Transient(
            "deposit refused".into(),
        ));
    }

    machine
        .attach_and_mint_baseline_grant()
        .await
        .expect_err("the refused deposit surfaces to the caller");

    assert!(
        nest.state().minted_grants.is_empty(),
        "the nest accepted nothing"
    );
    let stored = cfg.current();
    assert_eq!(
        stored.grant_events.len(),
        1,
        "and the Mint event is nonetheless durable — recorded BEFORE the blob \
         was offered, so no ordering of failures can strand a live capability \
         the user's app cannot show or revoke"
    );
    assert_eq!(stored.grant_events[0].kind, GrantEventKind::Mint);
}

#[tokio::test]
async fn opt_out_revokes_the_open_grant_and_logs_it() {
    // Opt in first (mint), then opt out (revoke) — the OFF half of the toggle.
    let (_holder_secret, holder_public) = holder_keys();
    let (_secret, public) = recipient_keys();
    let mut start = SpamModel::new();
    start.train_ham("weekly team sync notes");
    let sealed_start = seal_to_recipient(&start.to_bytes(), &public)
        .expect("seal")
        .to_canonical_bytes()
        .expect("encode");

    let (machine, nest, cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    {
        let mut s = nest.state();
        s.spam_model = Some(sealed_start);
        s.contribute_baseline = true;
        s.holder_seal_target = Some(HolderSealTarget {
            x25519_pubkey: holder_public.to_vec(),
            mlkem_ek: None,
            ..Default::default()
        });
    }
    machine
        .attach_and_mint_baseline_grant()
        .await
        .expect("opt-in");
    let minted_grant_id = cfg.current().grant_events[0].grant_id.clone();

    machine.revoke_baseline_grant().await.expect("opt-out");

    // The revoke reached the nest, naming the exact grant the mint logged.
    assert_eq!(
        nest.state()
            .revoked_grants
            .iter()
            .map(|g| g.to_vec())
            .collect::<Vec<_>>(),
        vec![minted_grant_id],
        "revoked the minted grant"
    );
    // A signed `Revoke` event landed; the grant is now dark in the Now lens.
    let stored = cfg.current();
    assert_eq!(stored.grant_events.len(), 2, "mint + revoke logged");
    assert!(
        grant_log::current_grants(&stored).is_empty(),
        "revoked grant goes dark"
    );
    stored.grant_events[1]
        .verify(&signer_owner())
        .expect("revoke event verifies under the owner identity");
}

#[tokio::test]
async fn opt_out_with_no_open_grant_is_a_noop() {
    // No prior mint (an opt-in with no holder enrolled (bit-only), or double-off): revoke is an idempotent
    // no-op — no nest call, no log entry.
    let (machine, nest, cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    machine.revoke_baseline_grant().await.expect("noop revoke");
    assert!(nest.state().revoked_grants.is_empty());
    assert!(cfg.current().grant_events.is_empty());
}

#[tokio::test]
async fn opt_in_without_holder_is_bit_only_no_mint() {
    // Opted in, but the nest volunteered no holder (none enrolled): bit-only — no grant, no copy write, no re-seal.
    let (machine, nest, cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    {
        let mut s = nest.state();
        s.contribute_baseline = true;
        s.holder_seal_target = None;
    }

    machine
        .attach_and_mint_baseline_grant()
        .await
        .expect("bit-only opt-in");

    assert!(
        nest.state().minted_grants.is_empty(),
        "no holder ⇒ no grant minted"
    );
    assert!(
        cfg.current().grant_events.is_empty(),
        "no holder ⇒ no grant logged"
    );
    assert!(
        nest.state().put_spam_model_holder_copy.is_none(),
        "no holder ⇒ no copy write"
    );
}

// ── the cold-start seed is never trained on ─────────────────────────────

/// A sealed blob the nest seals ON READ from the published baseline (no stored
/// model — `stored_sealed == false`) is the cold-start seed, not the user's
/// model: a train starts from a FRESH model, so the seed's documents never
/// land in the stored model (`mail-spam.md` § Cold start Path 2).
#[tokio::test]
async fn train_starts_fresh_over_the_cold_start_seed() {
    let (_secret, public) = recipient_keys();
    let mut seed = SpamModel::new();
    for _ in 0..5 {
        seed.train_ham("weekly team sync notes");
    }
    let sealed_seed = seal_to_recipient(&seed.to_bytes(), &public)
        .expect("seal seed")
        .to_canonical_bytes()
        .expect("encode");

    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    {
        let mut s = nest.state();
        s.spam_model = Some(sealed_seed);
        s.spam_model_is_cold_start_seed = true;
    }

    let out = machine
        .train_spam_model_client("buy cheap pills now".into(), true)
        .await
        .expect("train");
    assert_eq!(
        out,
        SpamModelClientWrite {
            sealed: true,
            sample_count: 1,
        },
        "the seed's 5 ham documents are not the user's — the train starts fresh"
    );
    let stored = nest.state().spam_model.clone().expect("stored");
    let round = SpamModel::from_bytes(&open_own(&stored)).expect("decode");
    assert_eq!(round.ham_messages, 0, "no seed document was persisted");
    assert_eq!(round.spam_messages, 1);
}

/// Opting in with only the cold-start seed on offer contributes NO initial
/// holder copy (the seed is the deployment baseline itself, not user
/// training) — the grant is still minted; the copy rides the first real write.
#[tokio::test]
async fn opt_in_over_the_cold_start_seed_attaches_no_initial_copy() {
    let (_holder_secret, holder_public) = holder_keys();
    let (_secret, public) = recipient_keys();
    let mut seed = SpamModel::new();
    seed.train_ham("weekly team sync notes");
    let sealed_seed = seal_to_recipient(&seed.to_bytes(), &public)
        .expect("seal seed")
        .to_canonical_bytes()
        .expect("encode");

    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    {
        let mut s = nest.state();
        s.spam_model = Some(sealed_seed);
        s.spam_model_is_cold_start_seed = true;
        s.contribute_baseline = true;
        s.holder_seal_target = Some(HolderSealTarget {
            x25519_pubkey: holder_public.to_vec(),
            mlkem_ek: None,
            ..Default::default()
        });
    }

    machine
        .attach_and_mint_baseline_grant()
        .await
        .expect("opt-in");
    assert_eq!(nest.state().minted_grants.len(), 1, "the grant is minted");
    assert!(
        nest.state().put_spam_model_holder_copy.is_none(),
        "no put_spam_model ran — the seed is never contributed"
    );
}

// ── the moderation-queue training correction (one shared flow) ──────────

const POST: &str = "ab12cd34";

/// Sealed write possible: `fauna.posts.get` → the sealed model train →
/// THEN `fauna.moderation.train` (the read gate + report capture) — the nest
/// half runs on the sealed path too.
#[tokio::test]
async fn correction_trains_sealed_then_reports_to_the_nest() {
    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    nest.state()
        .post_bodies
        .insert(POST.into(), "buy cheap pills now".into());

    let out = machine
        .train_moderation_correction(POST.into(), false)
        .await
        .expect("correction");

    assert_eq!(
        out,
        SpamModelClientWrite {
            sealed: true,
            sample_count: 1,
        }
    );
    let s = nest.state();
    assert_eq!(s.post_body_fetches, vec![POST.to_string()], "posts_get ran");
    let stored = s.spam_model.clone().expect("the sealed write landed");
    let round = SpamModel::from_bytes(&open_own(&stored)).expect("decode");
    assert_eq!(round.ham_messages, 1, "trained ham on the fetched body");
    assert_eq!(
        s.put_spam_model_history_op,
        Some(None),
        "a social train writes no history row"
    );
    assert_eq!(
        s.moderation_trains,
        vec![(POST.to_string(), "ham".to_string())],
        "the nest half ALWAYS runs — the report capture is not skipped"
    );
}

/// No sealed write possible (mail not enabled, or a nest without the token):
/// no body fetch, no model write — only `fauna.moderation.train`.
#[tokio::test]
async fn correction_without_a_sealed_write_runs_only_the_nest_half() {
    for (caps, mail) in [(&[SPAM_MODEL_SEALED_AT_REST][..], false), (&[][..], true)] {
        let (machine, nest, _cfg) = build(caps, mail);
        nest.state()
            .post_bodies
            .insert(POST.into(), "buy cheap pills now".into());

        let out = machine
            .train_moderation_correction(POST.into(), true)
            .await
            .expect("correction");

        assert_eq!(
            out,
            SpamModelClientWrite {
                sealed: false,
                sample_count: 0,
            }
        );
        let s = nest.state();
        assert!(s.post_body_fetches.is_empty(), "no body fetch");
        assert!(s.spam_model.is_none(), "no model write");
        assert_eq!(
            s.moderation_trains,
            vec![(POST.to_string(), "spam".to_string())]
        );
    }
}

/// A body-fetch miss (the read gate refused, or the post is gone) skips the
/// model half; the nest half still runs and owns the canonical answer.
#[tokio::test]
async fn correction_with_a_body_fetch_miss_runs_only_the_nest_half() {
    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);

    let out = machine
        .train_moderation_correction(POST.into(), false)
        .await
        .expect("correction");

    assert!(!out.sealed, "model half skipped");
    let s = nest.state();
    assert_eq!(
        s.post_body_fetches,
        vec![POST.to_string()],
        "fetch attempted"
    );
    assert!(s.spam_model.is_none(), "no model write");
    assert_eq!(
        s.moderation_trains,
        vec![(POST.to_string(), "ham".to_string())]
    );
}

/// The nest half's error is the flow's error.
#[tokio::test]
async fn correction_surfaces_the_nest_halfs_error() {
    let (machine, nest, _cfg) = build(&[SPAM_MODEL_SEALED_AT_REST], true);
    {
        let mut s = nest.state();
        s.post_bodies
            .insert(POST.into(), "buy cheap pills now".into());
        s.fail_moderation_train_with =
            Some(NestError::Rejected("fauna.moderation.not_found".into()));
    }

    let err = machine
        .train_moderation_correction(POST.into(), false)
        .await
        .expect_err("the nest half failed");
    assert!(matches!(err, DispatchError::Nest(_)), "got {err:?}");
    assert!(err.to_string().contains("not_found"), "got {err}");
    assert_eq!(nest.state().moderation_trains.len(), 1);
}
