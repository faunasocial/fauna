//! `FfiMailIndexSession` — the MDA bridge's **query-only** mail/calendar index
//! leg (`content-index.md` § Where the index is built — the 2026-08-10 carrier
//! ruling retired its build half).
//!
//! **These are cross-leg tests, deliberately.** The pre-ruling tier staged
//! *and* asked through this same session, so all 17 cases were green while
//! production was disjoint — every test spoke one self-consistent spelling,
//! and the defect was precisely that the two legs did not
//! (the 107th-pass finding this file's docs used to carry). So the fixture now
//! IS the production shape: the **client leg** (`fauna_client_index`'s real
//! `IndexBuilder`, the ships-first builder) stages and publishes the slice —
//! stamping each doc's `secondary_id` from the nest message id, as
//! `doc_for` does — and the **MDA session** opens the same rail bytes and
//! answers `SEARCH` coverage in the only spelling the Go side holds: lowercase
//! hex of the nest id.
//!
//! The rail is one in-memory store worn by both legs through their own
//! adapters (async `SegmentRail` for the builder, sync read-only
//! `FfiIndexRail` for the session), content-addressed like the real one.

use std::sync::{Arc, Mutex};

use fauna_client_index::{IndexBuildError, IndexBuilder, RailEntry, SegmentRail};
use fauna_conversations::index_sink::{IndexableKind, IndexableMessage};
use fauna_conversations::message::MessageId;
use fauna_conversations::thread::ThreadId;
use fauna_ffi::{FfiError, FfiIndexRail, FfiIndexRailEntry, KdfKind, unwrap_msek_blob};
use fauna_mls::wrapped_blob::{
    Argon2idParams, CredentialInput, KdfParams, MlsSnapshotPlaintext, derive_index_segment_key,
    seal_wrapped_msek,
};
use serde_bytes::ByteBuf;

const ACTOR: [u8; 32] = [0x42u8; 32];
const CRED_ID: &str = "cred-1";
const PASSWORD: &[u8] = b"correct-password";
const MSEK: [u8; 32] = [0x11u8; 32];
/// A generation this actor has rotated away from.
const PRIOR_MSEK: [u8; 32] = [0x22u8; 32];

/// The nest message ids the "nest" assigned — raw 32 bytes, exactly what
/// `wsrpc.IndexSegment.MessageID` carries on the Go side.
const NEST_ID_A: [u8; 32] = [0xA1u8; 32];
const NEST_ID_B: [u8; 32] = [0xB2u8; 32];

fn capability(msek: &[u8; 32]) -> Arc<fauna_ffi::MlsCapability> {
    let blob = seal_wrapped_msek(
        msek,
        &ACTOR,
        CRED_ID,
        &CredentialInput::Plain(PASSWORD),
        KdfParams::Argon2id(Argon2idParams {
            m: 4096,
            t: 1,
            p: 1,
        }),
    )
    .expect("seal wrapped msek")
    .to_canonical_bytes()
    .expect("encode blob");
    unwrap_msek_blob(
        blob,
        PASSWORD.to_vec(),
        ACTOR.to_vec(),
        CRED_ID.to_string(),
        KdfKind::Argon2id,
    )
    .expect("unwrap msek")
}

/// A snapshot as the nest publishes it. `priors` become the
/// `index_seg_grace_keys` the session reads segments from generations it has
/// rotated away from.
fn snapshot(priors: &[[u8; 32]]) -> Vec<u8> {
    MlsSnapshotPlaintext {
        index_seg_grace_keys: priors
            .iter()
            .map(|m| ByteBuf::from(derive_index_segment_key(m).to_vec()))
            .collect(),
        ..Default::default()
    }
    .to_canonical_bytes()
    .expect("encode snapshot")
}

/// The `__index` rail's storage, in memory — one copy of the published bytes,
/// worn by both legs. "Last write wins per path" because the manifest is
/// rewritten every flush.
#[derive(Default)]
struct Store {
    published: Mutex<Vec<(String, Vec<u8>)>>,
}

impl Store {
    fn hash(bytes: &[u8]) -> String {
        fauna_core::hex32::encode(&fauna_cbor::Cid::of_raw(bytes).digest())
    }
    fn latest(&self) -> Vec<(String, Vec<u8>)> {
        let published = self.published.lock().unwrap();
        let mut latest: Vec<(String, Vec<u8>)> = Vec::new();
        for (path, bytes) in published.iter() {
            match latest.iter_mut().find(|(p, _)| p == path) {
                Some(slot) => slot.1 = bytes.clone(),
                None => latest.push((path.clone(), bytes.clone())),
            }
        }
        latest
    }
    fn fetch(&self, blob_hash: &str) -> Option<Vec<u8>> {
        self.published
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(_, b)| Self::hash(b) == blob_hash)
            .map(|(_, b)| b.clone())
    }
}

/// The client leg's view of the store — the async publish-capable rail the
/// real builder is written against.
struct ClientRail(Arc<Store>);

#[async_trait::async_trait]
impl SegmentRail for ClientRail {
    async fn list_entries(&self) -> Result<Vec<RailEntry>, IndexBuildError> {
        Ok(self
            .0
            .latest()
            .into_iter()
            .map(|(path, bytes)| RailEntry {
                blob_hash: Store::hash(&bytes),
                size_bytes: bytes.len() as u64,
                path,
            })
            .collect())
    }
    async fn fetch_blob(&self, blob_hash: &str) -> Result<Vec<u8>, IndexBuildError> {
        self.0
            .fetch(blob_hash)
            .ok_or_else(|| IndexBuildError::Publish {
                path: blob_hash.to_string(),
                reason: "no such blob".into(),
            })
    }
    async fn publish(&self, path: &str, bytes: &[u8]) -> Result<(), IndexBuildError> {
        self.0
            .published
            .lock()
            .unwrap()
            .push((path.to_string(), bytes.to_vec()));
        Ok(())
    }
}

/// The MDA's view of the same store — sync and read-only, like the Go
/// implementation over `fauna.bridges.index_list` + the blob GET route.
struct MdaRail(Arc<Store>);

impl FfiIndexRail for MdaRail {
    fn list_entries(&self) -> Result<Vec<FfiIndexRailEntry>, FfiError> {
        Ok(self
            .0
            .latest()
            .into_iter()
            .map(|(path, bytes)| FfiIndexRailEntry {
                blob_hash: Store::hash(&bytes),
                size_bytes: bytes.len() as u64,
                path,
            })
            .collect())
    }
    fn fetch_blob(&self, blob_hash: String) -> Result<Vec<u8>, FfiError> {
        self.0.fetch(&blob_hash).ok_or_else(|| FfiError::General {
            msg: format!("no blob {blob_hash}"),
        })
    }
}

/// A rail that refuses everything — the transient-nest-outage shape the caller
/// is told to treat as recoverable.
struct DeadRail;

impl FfiIndexRail for DeadRail {
    fn list_entries(&self) -> Result<Vec<FfiIndexRailEntry>, FfiError> {
        Err(FfiError::General {
            msg: "nest unreachable".into(),
        })
    }
    fn fetch_blob(&self, _blob_hash: String) -> Result<Vec<u8>, FfiError> {
        Err(FfiError::General {
            msg: "nest unreachable".into(),
        })
    }
}

/// Stage one message through the real client builder exactly as the production
/// ingest frame does: RFC `Message-ID` as the content id, the raw nest id as
/// the doc's secondary identity.
fn stage_as_the_client_leg(
    builder: &IndexBuilder,
    rfc_id: &str,
    nest_id: Option<&[u8]>,
    subject: &str,
    body: &str,
) {
    let thread = ThreadId("t-1".into());
    let message = MessageId(rfc_id.into());
    builder.stage(&IndexableMessage {
        kind: IndexableKind::Mail,
        thread_id: &thread,
        message_id: &message,
        subject: Some(subject),
        body,
        sender_actor_id: None,
        nest_message_id: nest_id,
        timestamp_ms: 1_700_000_000_000,
        is_own: false,
    });
}

/// Build and publish a slice with the client leg under `msek`.
fn client_publishes(store: &Arc<Store>, msek: &[u8; 32], docs: &[(&str, Option<&[u8]>, &str)]) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("rt");
    let builder = IndexBuilder::mail(msek, Arc::new(ClientRail(store.clone())));
    for (rfc_id, nest_id, body) in docs {
        stage_as_the_client_leg(&builder, rfc_id, *nest_id, "Subject", body);
    }
    let sealed = rt.block_on(builder.flush()).expect("client flush");
    assert!(!sealed.is_empty(), "the client leg published a segment");
}

/// **The cross-leg pin this file owes** (`content-index.md` § Where the index
/// is built — the carrier ruling's test clause): stage as the client leg keys,
/// ask as the MDA asks, assert coverage. Before the carrier this exact shape
/// answered `covered=[]` in production while the old single-leg tier stayed
/// green — removing the `secondary_id` stamp in `doc_for`, or re-keying
/// coverage back onto content ids, turns this red.
#[test]
fn a_client_built_slice_answers_the_mdas_hex_candidates() {
    let store = Arc::new(Store::default());
    client_publishes(
        &store,
        &MSEK,
        &[
            ("<a@x>", Some(&NEST_ID_A), "the quarterly invoice body"),
            ("<b@x>", Some(&NEST_ID_B), "lunch plans for tuesday"),
        ],
    );

    let session = capability(&MSEK)
        .resume_mail_index_session(snapshot(&[]), Arc::new(MdaRail(store)))
        .expect("resume");

    let answer = session
        .answer_body_search(
            vec!["invoice".into()],
            vec![hex::encode(NEST_ID_A), hex::encode(NEST_ID_B)],
        )
        .expect("answer");
    let mut covered = answer.covered.clone();
    covered.sort();
    let mut expected = vec![hex::encode(NEST_ID_A), hex::encode(NEST_ID_B)];
    expected.sort();
    assert_eq!(
        covered, expected,
        "both client-staged messages must be covered in the MDA's own spelling"
    );
    assert_eq!(
        answer.matched,
        vec![hex::encode(NEST_ID_A)],
        "only the invoice matches, and the verdict comes back in the caller's spelling"
    );
}

/// The ONE-document half of the same pin: the two legs agree on a message's
/// identity, so the slice holds exactly one doc per message — never the
/// duplicate pair the pre-carrier corpus accrued.
#[test]
fn one_message_is_one_document_across_both_legs() {
    let store = Arc::new(Store::default());
    client_publishes(
        &store,
        &MSEK,
        &[("<a@x>", Some(&NEST_ID_A), "the quarterly invoice body")],
    );

    let session = capability(&MSEK)
        .resume_mail_index_session(snapshot(&[]), Arc::new(MdaRail(store)))
        .expect("resume");

    // The session's whole view of the slice: query by content, count hits.
    let hits = session.query("invoice".into(), 10).expect("query");
    assert_eq!(hits.len(), 1, "one message, one document");
    assert_eq!(
        hits[0].content_id.0,
        b"<a@x>".to_vec(),
        "the content id stays the ratified RFC Message-ID"
    );
}

/// Coverage keys on the secondary identity and nothing else. A doc staged
/// without one (no `nest_message_id` at ingest) is honestly uncovered — the scan arm
/// answers for it — and the doc's own content id is NOT a coverage key, so the
/// two spellings can never silently re-converge on byte equality.
#[test]
fn coverage_is_by_secondary_identity_only() {
    let store = Arc::new(Store::default());
    client_publishes(
        &store,
        &MSEK,
        &[
            ("<old@x>", None, "a doc without a nest id"),
            ("<new@x>", Some(&NEST_ID_A), "a doc with a nest id"),
        ],
    );

    let session = capability(&MSEK)
        .resume_mail_index_session(snapshot(&[]), Arc::new(MdaRail(store)))
        .expect("resume");

    // Asking with the doc's CONTENT id (the id a caller without the secondary identity
    // would hold) covers nothing — not even hex-shaped input may match a content id.
    let by_content = session
        .answer_body_search(vec!["doc".into()], vec!["<old@x>".into(), "<new@x>".into()])
        .expect("answer");
    assert!(
        by_content.covered.is_empty(),
        "a content id is never a coverage key: got {:?}",
        by_content.covered
    );

    // Asking with nest ids covers exactly the doc that carries one.
    let by_secondary = session
        .answer_body_search(
            vec!["doc".into()],
            vec![hex::encode(NEST_ID_A), hex::encode(NEST_ID_B)],
        )
        .expect("answer");
    assert_eq!(
        by_secondary.covered,
        vec![hex::encode(NEST_ID_A)],
        "only the doc carrying a secondary identity is coverable"
    );
}

/// Terms that tokenize to nothing impose no constraint: every covered
/// candidate matches, which is the parity rule that keeps the index path and
/// the scan path answering punctuation searches identically.
#[test]
fn untokenizable_terms_match_every_covered_candidate() {
    let store = Arc::new(Store::default());
    client_publishes(
        &store,
        &MSEK,
        &[("<a@x>", Some(&NEST_ID_A), "anything at all")],
    );
    let session = capability(&MSEK)
        .resume_mail_index_session(snapshot(&[]), Arc::new(MdaRail(store)))
        .expect("resume");

    let answer = session
        .answer_body_search(vec!["!".into()], vec![hex::encode(NEST_ID_A)])
        .expect("answer");
    assert_eq!(answer.covered, vec![hex::encode(NEST_ID_A)]);
    assert_eq!(
        answer.matched, answer.covered,
        "no tokens ⇒ no constraint ⇒ every covered candidate matches"
    );
}

/// A candidate the slice does not know is simply absent from `covered` — a
/// normal state, not an error — and non-hex input can never be covered.
#[test]
fn unknown_and_non_hex_candidates_are_uncovered_not_errors() {
    let store = Arc::new(Store::default());
    client_publishes(&store, &MSEK, &[("<a@x>", Some(&NEST_ID_A), "the body")]);
    let session = capability(&MSEK)
        .resume_mail_index_session(snapshot(&[]), Arc::new(MdaRail(store)))
        .expect("resume");

    let answer = session
        .answer_body_search(
            vec!["body".into()],
            vec![hex::encode(NEST_ID_B), "not-hex-at-all".into()],
        )
        .expect("uncovered candidates are a normal state");
    assert!(answer.covered.is_empty());
    assert!(answer.matched.is_empty());
}

/// The MSEK-rotation reach: a slice the client built under a PRIOR generation
/// still answers, because the session's snapshot carries the grace keys
/// (`index_seg_grace_keys` — Path B-sibling-4's snapshot carriage).
#[test]
fn a_prior_generation_slice_answers_through_the_grace_ring() {
    let store = Arc::new(Store::default());
    client_publishes(
        &store,
        &PRIOR_MSEK,
        &[("<a@x>", Some(&NEST_ID_A), "the quarterly invoice body")],
    );

    let session = capability(&MSEK)
        .resume_mail_index_session(snapshot(&[PRIOR_MSEK]), Arc::new(MdaRail(store)))
        .expect("resume");
    let answer = session
        .answer_body_search(vec!["invoice".into()], vec![hex::encode(NEST_ID_A)])
        .expect("answer");
    assert_eq!(answer.covered, vec![hex::encode(NEST_ID_A)]);
    assert_eq!(answer.matched, vec![hex::encode(NEST_ID_A)]);
}

/// Without the grace key, the rotated-away slice is unreadable and the session
/// degrades to answering nothing — never a wrong answer, never a crash.
#[test]
fn a_prior_generation_slice_without_grace_keys_answers_nothing() {
    let store = Arc::new(Store::default());
    client_publishes(
        &store,
        &PRIOR_MSEK,
        &[("<a@x>", Some(&NEST_ID_A), "the quarterly invoice body")],
    );

    let session = capability(&MSEK)
        .resume_mail_index_session(snapshot(&[]), Arc::new(MdaRail(store)))
        .expect("resume is cheap and cannot fail on rail state");
    let answer = session.answer_body_search(vec!["invoice".into()], vec![hex::encode(NEST_ID_A)]);
    // Either shape is a degradation, not a wrong answer: an error the caller
    // scans on, or an empty coverage set. What is forbidden is a covered
    // verdict for bytes the session cannot actually open.
    if let Ok(answer) = answer {
        assert!(
            answer.covered.is_empty(),
            "an unopenable slice must not claim coverage"
        );
    }
}

/// An empty rail answers empty — the fresh-actor path.
#[test]
fn an_empty_rail_covers_nothing() {
    let store = Arc::new(Store::default());
    let session = capability(&MSEK)
        .resume_mail_index_session(snapshot(&[]), Arc::new(MdaRail(store)))
        .expect("resume");
    let answer = session
        .answer_body_search(vec!["anything".into()], vec![hex::encode(NEST_ID_A)])
        .expect("an empty rail is a normal state");
    assert!(answer.covered.is_empty());
    assert!(
        session
            .query("anything".into(), 10)
            .expect("query")
            .is_empty()
    );
}

/// Resume is deliberately lazy: a dead nest does not fail AUTH — the failure
/// surfaces per query, where the Go caller degrades that one `SEARCH` to the
/// hint scan.
#[test]
fn a_dead_rail_fails_the_query_not_the_resume() {
    let session = capability(&MSEK)
        .resume_mail_index_session(snapshot(&[]), Arc::new(DeadRail))
        .expect("resume must not touch the rail");
    assert!(
        session
            .answer_body_search(vec!["x".into()], vec![hex::encode(NEST_ID_A)])
            .is_err(),
        "the outage surfaces on the query, recoverably"
    );
}

/// The session contract every method shares: after zeroize, everything fails.
#[test]
fn a_zeroized_session_refuses_every_method() {
    let store = Arc::new(Store::default());
    let session = capability(&MSEK)
        .resume_mail_index_session(snapshot(&[]), Arc::new(MdaRail(store)))
        .expect("resume");
    session.zeroize();
    session.zeroize(); // idempotent
    assert!(session.query("x".into(), 10).is_err());
    assert!(
        session
            .answer_body_search(vec!["x".into()], vec![])
            .is_err()
    );
}

/// Rule #7's blast radius, observed where it is observable: what the client
/// leg publishes is sealed, so the rail (and the nest holding it) learns
/// nothing — and the MDA session still answers from it, proving the sealing
/// is real rather than the bytes merely being elsewhere.
#[test]
fn the_rail_bytes_are_sealed_and_still_answer() {
    let store = Arc::new(Store::default());
    client_publishes(
        &store,
        &MSEK,
        &[("<b@x>", Some(&NEST_ID_B), "supersecrettoken")],
    );

    for (path, bytes) in store.latest() {
        assert!(
            !bytes
                .windows("supersecrettoken".len())
                .any(|w| w == b"supersecrettoken"),
            "plaintext body leaked into {path}"
        );
    }

    let session = capability(&MSEK)
        .resume_mail_index_session(snapshot(&[]), Arc::new(MdaRail(store)))
        .expect("resume");
    let answer = session
        .answer_body_search(
            vec!["supersecrettoken".into()],
            vec![hex::encode(NEST_ID_B)],
        )
        .expect("answer");
    assert_eq!(answer.matched, vec![hex::encode(NEST_ID_B)]);
}
