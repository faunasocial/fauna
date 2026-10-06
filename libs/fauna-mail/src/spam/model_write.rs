//! Client-side per-user spam-model **WRITE** orchestrator — the tier-1
//! model-write half of the content-moderation-and-ranking frame's *first
//! sealing slice* (`docs/goal/architecture/content-moderation-and-ranking.md`
//! § Sealing tier-1; design tracked internally).
//!
//! When the per-user spam model is **sealed at rest** (nest-opaque), the nest can
//! no longer read-mutate-write it in-process, so every mutation must run at a
//! **capability-holder** that holds the user's key — the Fauna app (or the
//! AUTH'd MDA session). This module is the shared-Rust composition all six
//! apps call (priority #2) so none re-implements the delicate
//! unwrap → mutate → re-seal loop: it is the WRITE twin of the READ path
//! [`crate::open_sealed_inner_record`] and `fauna-wasm`'s `enable_spam_scoring`.
//!
//! **No net-new crypto.** [`apply_model_write_op`] is pure `SpamModel` mutation
//! (train / undo / model-sync), and [`apply_and_reseal`] wraps it with the
//! already-shipped, wasm-safe primitives [`crate::open_sealed_inner_record`]
//! (unwrap) + [`fauna_mls::wrapped_blob::seal_to_recipient`] (re-seal to the
//! actor's own recipient key). The re-sealed model is written back opaque via the
//! `fauna.bridges.put_spam_model` RPC (nest leg 3; the client caller is leg 1b).
//!
//! The **hybrid (X-Wing) re-seal** counterpart [`apply_and_reseal_hybrid`] is the
//! symmetric twin of the classical [`apply_and_reseal`]: it unwraps with
//! [`crate::open_sealed_inner_record_hybrid`] (which reads *either* suite, so a
//! classically-sealed model still opens) and re-seals with
//! [`fauna_mls::wrapped_blob::seal_to_recipient_xwing`] — so a post-quantum-hybrid
//! actor's model is never downgraded to the classical suite on write-back (and a
//! classically-sealed model — the nest seals classical when the X-Wing seal
//! fails — gets its at-rest blob upgraded in place, no
//! plaintext exposure, no data loss).

use std::collections::BTreeSet;

use fauna_mls::wrapped_blob::{
    MLKEM768_DECAPS_KEY_LEN, XWingPublicKey, seal_to_recipient, seal_to_recipient_xwing,
};

use crate::spam::classifier::{MODEL_MAX_BYTES_DEFAULT, SpamLabel, SpamModel};
use crate::{open_sealed_inner_record, open_sealed_inner_record_hybrid};

/// One client-driven model-write operation over the user's own (unwrapped) model.
///
/// The model rests sealed, so every mutation runs at a capability holder (the
/// client, or the AUTH'd mail agent): a train, the per-event undo, and the
/// adopt-if-larger merge — none has a server-side twin (`mail-spam.md` reader
/// table).
#[derive(Debug, Clone, PartialEq)]
pub enum ModelWriteOp {
    /// Apply one training event on `text` with `label` — the client already holds
    /// the plaintext (a post-decrypt local detection, or a body it fetched +
    /// decrypted by `content_id`; see the co-design § Train-body source). Byte-for-
    /// byte equivalent to the nest's `train_spam`/`train_ham`.
    Train { text: String, label: SpamLabel },
    /// Invert one prior training event from its **stored** forward-delta set (the
    /// `spam_training_history.model_delta_applied` the client unwraps — leg 1c).
    /// Exact inverse (`SpamModel::apply_inverse_delta_ngrams`).
    Undo {
        delta: BTreeSet<String>,
        label: SpamLabel,
    },
    /// Adopt a locally- or peer-trained `other` model **iff** it carries strictly
    /// more training data than the current one (an adopt-if-larger compare that
    /// runs client-side because the nest can't decode a sealed model to compare
    /// counts).
    ModelSync { other: SpamModel },
}

/// The result of one [`apply_model_write_op`]: the mutated model and the
/// **forward delta** the caller persists as a new `spam_training_history` row
/// (non-empty only for [`ModelWriteOp::Train`]).
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedModel {
    pub model: SpamModel,
    /// The distinct n-gram set a `Train` event touched (empty for `Undo`/`ModelSync`).
    pub delta: BTreeSet<String>,
}

/// Apply one write op to the decoded model — **pure**, no crypto, no I/O.
///
/// The mutation is size-capped ([`SpamModel::cap_to_bytes`],
/// `MODEL_MAX_BYTES_DEFAULT`) so the re-sealed blob honours the at-rest bound
/// before it is sealed (the seal is opaque to the nest, so the client is the only
/// place the cap can be enforced — `mail-spam.md` § Bounded size). Fully testable
/// with no keys; [`apply_and_reseal`] composes this with unwrap + re-seal.
pub fn apply_model_write_op(mut model: SpamModel, op: &ModelWriteOp) -> AppliedModel {
    let delta = match op {
        ModelWriteOp::Train { text, label } => {
            // Precompute the delta so the caller can persist it for a later
            // exact Undo, and apply it (byte-equivalent to `train_spam`/`train_ham`).
            let delta = SpamModel::delta_ngrams(text);
            model.apply_forward_delta_ngrams(&delta, *label);
            delta
        }
        ModelWriteOp::Undo { delta, label } => {
            model.apply_inverse_delta_ngrams(delta, *label);
            BTreeSet::new()
        }
        ModelWriteOp::ModelSync { other } => {
            if other.sample_count() > model.sample_count() {
                model = other.clone();
            }
            BTreeSet::new()
        }
    };
    model.cap_to_bytes(MODEL_MAX_BYTES_DEFAULT);
    AppliedModel { model, delta }
}

/// The re-sealed model plus the metadata the `fauna.bridges.put_spam_model` write
/// (leg 1b) and a new `spam_training_history` row (leg 1c) carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReSealedModel {
    /// The whole re-sealed model — `seal_to_recipient(model.to_bytes(), pubkey)`
    /// canonical bytes, a bare inner `wrapped_blob` (the exact shape
    /// `fetch_spam_model` returns), to write back opaque via `put_spam_model`.
    pub sealed_model: Vec<u8>,
    /// The post-mutation training-document count — **advisory** metadata for the
    /// settings display only; the nest never trusts it (it can't verify it against
    /// the opaque blob).
    pub sample_count: u32,
    /// The training-document count BEFORE the mutation — what is still on record
    /// when the nest rejects the write under the one-lesson rule
    /// (`PutSpamModelOutcome::DuplicateSignal`, `mail-spam.md` § 3), so the
    /// caller can report the unchanged count instead of the count of a write
    /// that did not happen.
    pub sample_count_before: u32,
    /// The forward delta for a new history row (empty for `Undo`/`ModelSync`).
    pub delta: BTreeSet<String>,
    /// The **post-mutation plaintext** `SpamModel` canonical bytes (`model.to_bytes()`),
    /// the exact bytes that were re-sealed into [`Self::sealed_model`]. Exposed so the
    /// deployment-baseline **holder copy** (piece (b), `mail-spam.md` § Encrypted-mode
    /// interaction) can be sealed to the aggregation holder from the same bytes —
    /// without a redundant unseal of `sealed_model` — when the writing actor is opted
    /// in. Never crosses the wire (the write ships only the sealed/holder-sealed
    /// forms); it is scratch plaintext for the write orchestrator.
    pub model_bytes: Vec<u8>,
}

/// Errors from [`apply_and_reseal`]. The mutation itself is infallible; only the
/// crypto edges (unwrap the fetched blob, re-seal to the recipient key) can fail.
#[derive(Debug)]
pub enum ModelWriteError {
    /// The sealed blob failed to decode or unseal under `x25519_secret` (a wrong
    /// key, a corrupt blob, or a decode error).
    Unseal(String),
    /// The re-seal to `recipient_pubkey` failed (e.g. a malformed recipient key).
    Reseal(String),
    /// The unwrapped bytes were not a decodable `SpamModel`.
    Decode,
}

impl std::fmt::Display for ModelWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelWriteError::Unseal(e) => write!(f, "unseal spam model: {e}"),
            ModelWriteError::Reseal(e) => write!(f, "re-seal spam model: {e}"),
            ModelWriteError::Decode => write!(f, "unwrapped bytes are not a SpamModel"),
        }
    }
}

impl std::error::Error for ModelWriteError {}

/// Unwrap a fetched sealed model (classical X25519 suite), apply `op`, and
/// re-seal the whole model to the actor's **own** recipient key — the full
/// client model-write loop for the common (non-post-quantum) suite.
///
/// `sealed_model` is `None` (or empty) when the actor is untrained
/// (`fetch_spam_model` returned nothing) → the op applies to a fresh
/// [`SpamModel::new`]. `x25519_secret` unwraps (the MSEK-derived recipient secret
/// the caller derives for `open_sealed_inner_record`);
/// `recipient_pubkey` is the actor's own recipient X25519 public key to re-seal
/// under (derivable from the same MSEK).
pub fn apply_and_reseal(
    sealed_model: Option<&[u8]>,
    x25519_secret: &[u8; 32],
    recipient_pubkey: &[u8; 32],
    op: &ModelWriteOp,
) -> Result<ReSealedModel, ModelWriteError> {
    let model = match sealed_model {
        Some(bytes) if !bytes.is_empty() => {
            let plaintext = open_sealed_inner_record(bytes, x25519_secret)
                .map_err(|e| ModelWriteError::Unseal(e.to_string()))?;
            SpamModel::from_bytes(&plaintext).ok_or(ModelWriteError::Decode)?
        }
        _ => SpamModel::new(),
    };

    let sample_count_before = model.sample_count();
    let AppliedModel { model, delta } = apply_model_write_op(model, op);
    let sample_count = model.sample_count();
    let model_bytes = model.to_bytes();

    let sealed_model = seal_to_recipient(&model_bytes, recipient_pubkey)
        .map_err(|e| ModelWriteError::Reseal(e.to_string()))?
        .to_canonical_bytes()
        .map_err(|e| ModelWriteError::Reseal(e.to_string()))?;

    Ok(ReSealedModel {
        sealed_model,
        sample_count,
        sample_count_before,
        delta,
        model_bytes,
    })
}

/// Post-quantum-hybrid twin of [`apply_and_reseal`]: unwrap a fetched sealed model
/// of **either** suite, apply `op`, and re-seal the whole model to the actor's own
/// **X-Wing** (ML-KEM-768 ∥ X25519) recipient key — the model-write loop for a
/// post-quantum-capable actor.
///
/// The unwrap side ([`open_sealed_inner_record_hybrid`]) opens both a classical
/// (X25519) blob and a hybrid (X-Wing) one, so this handles the mixed-suite case too:
/// an actor whose model was sealed classically has it **upgraded to
/// X-Wing in place** on the next write (no plaintext exposure, no data loss). The
/// re-seal always targets the hybrid suite, so a hybrid actor's model is never
/// silently downgraded to classical on write-back — the reason this twin exists.
///
/// `sealed_model` is `None`/empty when the actor is untrained → a fresh
/// [`SpamModel::new`]. `x25519_secret` + `mlkem_dk` are both MSEK-derived (via
/// `derive_recipient_hpke_keypair` and `derive_recipient_xwing_keypair(&msek)
/// .secret.mlkem_decaps_key()`, as for `open_sealed_inner_record_hybrid`);
///`recipient_xwing_pubkey` is the actor's own X-Wing public key to
/// re-seal under (the same MSEK's `derive_recipient_xwing_keypair(&msek).public`).
pub fn apply_and_reseal_hybrid(
    sealed_model: Option<&[u8]>,
    x25519_secret: &[u8; 32],
    mlkem_dk: &[u8; MLKEM768_DECAPS_KEY_LEN],
    recipient_xwing_pubkey: &XWingPublicKey,
    op: &ModelWriteOp,
) -> Result<ReSealedModel, ModelWriteError> {
    let model = match sealed_model {
        Some(bytes) if !bytes.is_empty() => {
            let plaintext = open_sealed_inner_record_hybrid(bytes, x25519_secret, mlkem_dk)
                .map_err(|e| ModelWriteError::Unseal(e.to_string()))?;
            SpamModel::from_bytes(&plaintext).ok_or(ModelWriteError::Decode)?
        }
        _ => SpamModel::new(),
    };

    let sample_count_before = model.sample_count();
    let AppliedModel { model, delta } = apply_model_write_op(model, op);
    let sample_count = model.sample_count();
    let model_bytes = model.to_bytes();

    let sealed_model = seal_to_recipient_xwing(&model_bytes, recipient_xwing_pubkey)
        .map_err(|e| ModelWriteError::Reseal(e.to_string()))?
        .to_canonical_bytes()
        .map_err(|e| ModelWriteError::Reseal(e.to_string()))?;

    Ok(ReSealedModel {
        sealed_model,
        sample_count,
        sample_count_before,
        delta,
        model_bytes,
    })
}

/// Which write path a client model-write surface (
/// slice 1d) takes, from the two nest signals it negotiates on — the resolved 1d
/// dispatch design (tracked internally, § Revision history 2026-07-06,
/// C/D + G).
///
/// This is **feature-presence capability negotiation** (`version-compatibility.md`
/// I2 § 3), *not* a mode branch: a v-newer client that unconditionally re-seals
/// and writes back against a v-older nest that still seals-on-read would
/// double-seal the model into unreadability → cold-start reset = **user-data
/// loss** (I1/I2 break). So the surface picks [`ServerPath`](Self::ServerPath) —
/// the degrade-down — whenever the nest hasn't advertised that it does the
/// sealed-at-rest handling ([`fauna_protocol::discovery::capability::SPAM_MODEL_SEALED_AT_REST`]),
/// and only re-seals when it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealedWriteDispatch {
    /// Re-seal the mutated model client-side with [`apply_and_reseal_hybrid`]
    /// (X-Wing) and write it back opaque via `fauna.bridges.put_spam_model`. The
    /// suite is always hybrid: a client always publishes its ML-KEM ek, so the
    /// nest's `seal_recipient_blob` gate (recipient ek published) seals the same
    /// suite on read; no capability token
    /// gates it since the 2026-09-24 ruling (`post-quantum.md` § Capability
    /// negotiation).
    Sealed,
    /// No sealed write is possible — the model half is skipped, with no
    /// server-side write to fall back to (the model rests sealed): either the
    /// nest hasn't advertised the sealed-at-rest handling (cannot happen on a
    /// current nest), **or** the actor has no MSEK (mail not enabled) so there is
    /// no recipient key to seal a model to — such an actor has no per-user model
    /// at all.
    ServerPath,
}

/// Decide the client model-write path from the nest signal + the actor's mail
/// state — the pure core of slice 1d's dispatch (co-design § Revision history
/// 2026-07-06 C/D, the feature-presence gate).
///
/// - `sealed_write_supported` — the nest advertises
///   [`capability::SPAM_MODEL_SEALED_AT_REST`](fauna_protocol::discovery::capability::SPAM_MODEL_SEALED_AT_REST)
///   (it returns client-sealed models verbatim on fetch). `false` ⇒
///   [`ServerPath`](SealedWriteDispatch::ServerPath), never a blind re-seal.
/// - `mail_enabled` — the actor has an MSEK (mail provisioned). `false` ⇒ no key
///   to seal under ⇒ [`ServerPath`](SealedWriteDispatch::ServerPath): the actor
///   has no per-user model, and the model half is skipped.
pub fn dispatch_for(sealed_write_supported: bool, mail_enabled: bool) -> SealedWriteDispatch {
    if sealed_write_supported && mail_enabled {
        SealedWriteDispatch::Sealed
    } else {
        SealedWriteDispatch::ServerPath
    }
}

/// The content-detected form of a training-history row's stored
/// `model_delta_applied` (`SpamTrainingHistoryRow.model_delta_applied`,
/// returned verbatim by `list_spam_training_history`
/// ). The undo surface
/// (leg 1c) undoes only a sealed row (unwrap under the actor's own recipient
/// key → [`ModelWriteOp::Undo`]); every row a current nest stores is sealed, so
/// the other two shapes fail the undo closed (no server-side undo exists).
#[derive(Debug, Clone, PartialEq)]
pub enum HistoryDelta {
    /// No stored delta — not undoable.
    Absent,
    /// A **plaintext** `serde_json` n-gram array — a shape no current writer
    /// stores (the nest refuses it); not undoable.
    Plaintext(BTreeSet<String>),
    /// An **opaque sealed** `wrapped_blob` — a client-written row (Slice B's
    /// `put_spam_model` `history_op`; the client sealed the delta to its own
    /// recipient key, so only the client can invert it). Carries the verbatim
    /// sealed bytes for the unwrap.
    Sealed(Vec<u8>),
}

/// Content-detect a stored `model_delta_applied` — the client-side twin of the
/// nest's `is_sealed_model_blob` shape detection (a plaintext value always
/// `serde_json`-decodes; a `wrapped_blob` never does, its canonical encoding is
/// dag-cbor). No version flag needed: the byte shape itself is the signal, so
/// the detection stays correct across any mix of row generations.
pub fn decode_history_delta(bytes: &[u8]) -> HistoryDelta {
    if bytes.is_empty() {
        return HistoryDelta::Absent;
    }
    match serde_json::from_slice::<BTreeSet<String>>(bytes) {
        Ok(delta) => HistoryDelta::Plaintext(delta),
        Err(_) => HistoryDelta::Sealed(bytes.to_vec()),
    }
}

/// Serialize a training event's forward n-gram delta to its **inner plaintext**
/// byte shape (a `serde_json` array of the distinct n-grams — the exact form
/// [`decode_history_delta`] reads back as [`HistoryDelta::Plaintext`]). Every
/// stored row seals *these same bytes* to
/// the actor's own recipient key ([`seal_history_blob`] / [`seal_history_blob_hybrid`]),
/// so the seal is transparent: a sealed delta's inner plaintext is byte-identical
/// to a plaintext row's stored bytes, and [`unwrap_sealed_history_delta`]'s
/// unwrap → decode is symmetric with the plaintext path. Infallible for a
/// `BTreeSet<String>` (always a JSON string array).
pub fn encode_history_delta(delta: &BTreeSet<String>) -> Vec<u8> {
    serde_json::to_vec(delta).expect("a BTreeSet<String> always serializes to a JSON array")
}

/// Seal a history-row blob — a subject's UTF-8 bytes or an [`encode_history_delta`]
/// payload — to the actor's **own** classical (X25519) recipient key, producing the
/// `wrapped_blob` bytes the nest stores **opaque/verbatim** into
/// `spam_training_history.{sealed_subject,model_delta_applied}` for a client-written
/// row (`SpamHistoryOp::Insert`, co-design § 3). Same suite + canonical-bytes
/// encoding as [`apply_and_reseal`]'s model re-seal, so a sealed row's subject and
/// delta ride the same key custody as the model itself. `recipient_pubkey` is the
/// actor's own MSEK-derived recipient X25519 public key. The output never
/// `serde_json`-decodes (its canonical encoding is dag-cbor), so
/// [`decode_history_delta`] content-detects it as [`HistoryDelta::Sealed`].
pub fn seal_history_blob(
    plaintext: &[u8],
    recipient_pubkey: &[u8; 32],
) -> Result<Vec<u8>, ModelWriteError> {
    seal_to_recipient(plaintext, recipient_pubkey)
        .map_err(|e| ModelWriteError::Reseal(e.to_string()))?
        .to_canonical_bytes()
        .map_err(|e| ModelWriteError::Reseal(e.to_string()))
}

/// Post-quantum-hybrid (X-Wing) twin of [`seal_history_blob`]: seals to the actor's
/// own X-Wing recipient key so a PQ-hybrid actor's history row rides the same suite
/// as its hybrid-sealed model ([`apply_and_reseal_hybrid`]), never downgraded to
/// classical. `recipient_xwing_pubkey` is the actor's own
/// `derive_recipient_xwing_keypair(&msek).public`.
pub fn seal_history_blob_hybrid(
    plaintext: &[u8],
    recipient_xwing_pubkey: &XWingPublicKey,
) -> Result<Vec<u8>, ModelWriteError> {
    seal_to_recipient_xwing(plaintext, recipient_xwing_pubkey)
        .map_err(|e| ModelWriteError::Reseal(e.to_string()))?
        .to_canonical_bytes()
        .map_err(|e| ModelWriteError::Reseal(e.to_string()))
}

/// Open a client-written history row's sealed blob ([`HistoryDelta::Sealed`], or a
/// sealed `sealed_subject`) back to its inner plaintext bytes — the read half a
/// client runs before rendering a sealed subject or inverting a sealed delta.
/// Opens under the actor's MSEK-derived recipient secret; when `mlkem_dk` is `Some`
/// the hybrid opener ([`open_sealed_inner_record_hybrid`]) is used, which reads
/// **either** suite (so a classically-sealed row still opens for a
/// hybrid actor), else the classical opener.
pub fn unwrap_sealed_history_bytes(
    sealed: &[u8],
    x25519_secret: &[u8; 32],
    mlkem_dk: Option<&[u8; MLKEM768_DECAPS_KEY_LEN]>,
) -> Result<Vec<u8>, ModelWriteError> {
    match mlkem_dk {
        Some(dk) => open_sealed_inner_record_hybrid(sealed, x25519_secret, dk),
        None => open_sealed_inner_record(sealed, x25519_secret),
    }
    .map_err(|e| ModelWriteError::Unseal(e.to_string()))
}

/// Unwrap a client-written row's sealed `model_delta_applied` back to its forward
/// n-gram set — the read half a client-side undo runs before [`ModelWriteOp::Undo`]
/// (`libs/fauna-client-mail-settings` routes a sealed row's undo through here). The
/// inner plaintext is the same `serde_json` array [`encode_history_delta`] produced,
/// so the decode is byte-symmetric with the plaintext-row path.
pub fn unwrap_sealed_history_delta(
    sealed: &[u8],
    x25519_secret: &[u8; 32],
    mlkem_dk: Option<&[u8; MLKEM768_DECAPS_KEY_LEN]>,
) -> Result<BTreeSet<String>, ModelWriteError> {
    let plaintext = unwrap_sealed_history_bytes(sealed, x25519_secret, mlkem_dk)?;
    serde_json::from_slice::<BTreeSet<String>>(&plaintext).map_err(|_| ModelWriteError::Decode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_mls::wrapped_blob::{derive_recipient_hpke_keypair, derive_recipient_xwing_keypair};

    // ── Dispatch decision (the C/D feature-presence gate) ──

    #[test]
    fn dispatch_degrades_to_server_path_when_seal_not_supported() {
        // The load-bearing I1/I2 gate: a nest that has NOT advertised the
        // sealed-at-rest handling still seals-on-read, so a re-seal would
        // double-seal → user-data loss. Must degrade down regardless of mail.
        for mail in [false, true] {
            assert_eq!(
                dispatch_for(false, mail),
                SealedWriteDispatch::ServerPath,
                "seal unsupported ⇒ ServerPath (mail={mail})"
            );
        }
    }

    #[test]
    fn dispatch_degrades_to_server_path_when_mail_not_enabled() {
        // No MSEK ⇒ no recipient key to seal under (the social-only edge);
        // keep the server path so the train isn't lost.
        assert_eq!(dispatch_for(true, false), SealedWriteDispatch::ServerPath);
    }

    #[test]
    fn dispatch_seals_when_supported_and_mail_enabled() {
        assert_eq!(dispatch_for(true, true), SealedWriteDispatch::Sealed);
    }

    /// A deterministic 32-byte MSEK stand-in for keypair derivation in tests.
    fn test_recipient_keypair() -> ([u8; 32], [u8; 32]) {
        let msek = [7u8; 32];
        // `derive_recipient_hpke_keypair` returns `(secret, public)` (receive.rs:152).
        derive_recipient_hpke_keypair(&msek)
    }

    fn seal_model(model: &SpamModel, pubkey: &[u8; 32]) -> Vec<u8> {
        seal_to_recipient(&model.to_bytes(), pubkey)
            .expect("seal")
            .to_canonical_bytes()
            .expect("encode")
    }

    // ── Pure mutation core ──────────────────────────────────────────────────

    #[test]
    fn train_applies_forward_delta_and_returns_it() {
        let base = SpamModel::new();
        let applied = apply_model_write_op(
            base,
            &ModelWriteOp::Train {
                text: "buy cheap pills now".into(),
                label: SpamLabel::Spam,
            },
        );
        assert_eq!(applied.model.spam_messages, 1);
        assert_eq!(applied.model.ham_messages, 0);
        // The returned delta is exactly the n-gram set the event touched, so a
        // later Undo can invert it byte-for-byte.
        assert_eq!(
            applied.delta,
            SpamModel::delta_ngrams("buy cheap pills now")
        );
        assert!(!applied.delta.is_empty());
    }

    #[test]
    fn undo_is_exact_inverse_of_train() {
        // Train, capture the delta, then Undo with the stored delta → the model
        // returns byte-for-byte to its prior (empty) state.
        let trained = apply_model_write_op(
            SpamModel::new(),
            &ModelWriteOp::Train {
                text: "limited offer act now".into(),
                label: SpamLabel::Spam,
            },
        );
        let undone = apply_model_write_op(
            trained.model.clone(),
            &ModelWriteOp::Undo {
                delta: trained.delta.clone(),
                label: SpamLabel::Spam,
            },
        );
        assert_eq!(undone.model, SpamModel::new());
        assert!(undone.delta.is_empty());
    }

    #[test]
    fn model_sync_adopts_only_a_strictly_larger_model() {
        let mut small = SpamModel::new();
        small.train_ham("hello team");
        let mut large = SpamModel::new();
        large.train_ham("hello team");
        large.train_spam("buy cheap pills");
        large.train_spam("act now offer");

        // Current smaller than `other` ⇒ adopt.
        let adopted = apply_model_write_op(
            small.clone(),
            &ModelWriteOp::ModelSync {
                other: large.clone(),
            },
        );
        assert_eq!(adopted.model, large);

        // Current larger-or-equal ⇒ keep.
        let kept = apply_model_write_op(
            large.clone(),
            &ModelWriteOp::ModelSync {
                other: small.clone(),
            },
        );
        assert_eq!(kept.model, large);
    }

    // ── Full crypto round-trip (classical) ──────────────────────────────────

    #[test]
    fn apply_and_reseal_round_trips_a_train_through_the_seal() {
        let (secret, public) = test_recipient_keypair();

        // Start from a sealed model with one ham event.
        let mut start = SpamModel::new();
        start.train_ham("weekly team sync notes");
        let sealed = seal_model(&start, &public);

        let out = apply_and_reseal(
            Some(&sealed),
            &secret,
            &public,
            &ModelWriteOp::Train {
                text: "buy cheap pills now".into(),
                label: SpamLabel::Spam,
            },
        )
        .expect("apply+reseal");

        assert_eq!(out.sample_count, 2); // 1 ham + 1 spam
        assert!(!out.delta.is_empty());

        // Unwrap the re-sealed blob and confirm the mutation persisted through the
        // seal (the nest never sees this plaintext — it stores the opaque blob).
        let plaintext = open_sealed_inner_record(&out.sealed_model, &secret).expect("unseal");
        let round = SpamModel::from_bytes(&plaintext).expect("decode");
        assert_eq!(round.spam_messages, 1);
        assert_eq!(round.ham_messages, 1);
    }

    #[test]
    fn apply_and_reseal_starts_from_fresh_when_untrained() {
        let (secret, public) = test_recipient_keypair();
        // `None` (untrained: fetch_spam_model returned nothing) ⇒ fresh model.
        let out = apply_and_reseal(
            None,
            &secret,
            &public,
            &ModelWriteOp::Train {
                text: "act now limited offer".into(),
                label: SpamLabel::Spam,
            },
        )
        .expect("apply+reseal from fresh");
        assert_eq!(out.sample_count, 1);

        let plaintext = open_sealed_inner_record(&out.sealed_model, &secret).expect("unseal");
        let round = SpamModel::from_bytes(&plaintext).expect("decode");
        assert_eq!(round.spam_messages, 1);
    }

    #[test]
    fn apply_and_reseal_undo_through_the_seal_restores_prior_model() {
        let (secret, public) = test_recipient_keypair();

        // Seal an empty model; train through the seal, capturing the delta.
        let empty_sealed = seal_model(&SpamModel::new(), &public);
        let trained = apply_and_reseal(
            Some(&empty_sealed),
            &secret,
            &public,
            &ModelWriteOp::Train {
                text: "buy cheap pills now".into(),
                label: SpamLabel::Spam,
            },
        )
        .expect("train");

        // Undo that event through the seal with the captured delta.
        let undone = apply_and_reseal(
            Some(&trained.sealed_model),
            &secret,
            &public,
            &ModelWriteOp::Undo {
                delta: trained.delta.clone(),
                label: SpamLabel::Spam,
            },
        )
        .expect("undo");
        assert_eq!(undone.sample_count, 0);

        let plaintext = open_sealed_inner_record(&undone.sealed_model, &secret).expect("unseal");
        let round = SpamModel::from_bytes(&plaintext).expect("decode");
        assert_eq!(round, SpamModel::new());
    }

    #[test]
    fn apply_and_reseal_rejects_a_wrong_key() {
        let (_secret, public) = test_recipient_keypair();
        let sealed = seal_model(&SpamModel::new(), &public);
        // A different secret cannot unwrap the blob.
        let (wrong, _) = derive_recipient_hpke_keypair(&[9u8; 32]);
        let err = apply_and_reseal(
            Some(&sealed),
            &wrong,
            &public,
            &ModelWriteOp::Train {
                text: "x".into(),
                label: SpamLabel::Ham,
            },
        )
        .unwrap_err();
        assert!(matches!(err, ModelWriteError::Unseal(_)));
    }

    // ── Full crypto round-trip (hybrid / X-Wing) ────────────────────────────
    //
    // The X-Wing keypair is derived inline in each test (as `segments::receive`'s
    // hybrid tests do) rather than via a helper: `fauna-pq-kem` is not a direct
    // dep of `fauna-mail`, so `XWingKeyPair` can't be named in a helper signature,
    // and `mlkem_decaps_key()` borrows from the keypair so it must stay in scope.

    #[test]
    fn apply_and_reseal_hybrid_round_trips_a_train_through_the_xwing_seal() {
        let (secret, _) = derive_recipient_hpke_keypair(&[7u8; 32]);
        let xwing = derive_recipient_xwing_keypair(&[7u8; 32]);
        let mdk = xwing.secret.mlkem_decaps_key();

        // Seal an initial model (one ham event) to the actor's X-Wing key.
        let mut start = SpamModel::new();
        start.train_ham("weekly team sync notes");
        let sealed = seal_to_recipient_xwing(&start.to_bytes(), &xwing.public)
            .expect("seal xwing")
            .to_canonical_bytes()
            .expect("encode");

        let out = apply_and_reseal_hybrid(
            Some(&sealed),
            &secret,
            mdk,
            &xwing.public,
            &ModelWriteOp::Train {
                text: "buy cheap pills now".into(),
                label: SpamLabel::Spam,
            },
        )
        .expect("apply+reseal hybrid");

        assert_eq!(out.sample_count, 2); // 1 ham + 1 spam
        assert!(!out.delta.is_empty());

        // The mutation persisted through the X-Wing seal.
        let plaintext =
            open_sealed_inner_record_hybrid(&out.sealed_model, &secret, mdk).expect("unseal");
        let round = SpamModel::from_bytes(&plaintext).expect("decode");
        assert_eq!(round.spam_messages, 1);
        assert_eq!(round.ham_messages, 1);

        // The re-sealed blob is genuinely X-Wing, NOT downgraded to classical —
        // the classical opener must refuse it. This is the whole point of the twin.
        assert!(
            open_sealed_inner_record(&out.sealed_model, &secret).is_err(),
            "re-sealed model must be X-Wing, not classical"
        );
    }

    #[test]
    fn apply_and_reseal_hybrid_upgrades_a_classically_sealed_model() {
        // A post-quantum-hybrid actor whose model was sealed classically: the hybrid write path opens the classical blob and re-seals
        // it to X-Wing, upgrading the at-rest suite in place (no plaintext
        // exposure, no data loss).
        let msek = [7u8; 32];
        let (secret, x25519_public) = derive_recipient_hpke_keypair(&msek);
        let xwing = derive_recipient_xwing_keypair(&msek);
        let mdk = xwing.secret.mlkem_decaps_key();

        // Classically-sealed at-rest shape: a *classical* seal.
        let mut start = SpamModel::new();
        start.train_spam("act now limited offer");
        let classical_sealed = seal_to_recipient(&start.to_bytes(), &x25519_public)
            .expect("seal classical")
            .to_canonical_bytes()
            .expect("encode");

        let out = apply_and_reseal_hybrid(
            Some(&classical_sealed),
            &secret,
            mdk,
            &xwing.public,
            &ModelWriteOp::Train {
                text: "weekly team sync notes".into(),
                label: SpamLabel::Ham,
            },
        )
        .expect("upgrade classical → hybrid");
        assert_eq!(out.sample_count, 2);

        // Output is now X-Wing: classical opener refuses it, hybrid opener reads it.
        assert!(
            open_sealed_inner_record(&out.sealed_model, &secret).is_err(),
            "upgraded model must be X-Wing, not classical"
        );
        let plaintext =
            open_sealed_inner_record_hybrid(&out.sealed_model, &secret, mdk).expect("unseal");
        let round = SpamModel::from_bytes(&plaintext).expect("decode");
        assert_eq!(round.spam_messages, 1);
        assert_eq!(round.ham_messages, 1);
    }

    #[test]
    fn apply_and_reseal_hybrid_starts_from_fresh_when_untrained() {
        let (secret, _) = derive_recipient_hpke_keypair(&[7u8; 32]);
        let xwing = derive_recipient_xwing_keypair(&[7u8; 32]);
        let mdk = xwing.secret.mlkem_decaps_key();
        let out = apply_and_reseal_hybrid(
            None,
            &secret,
            mdk,
            &xwing.public,
            &ModelWriteOp::Train {
                text: "act now limited offer".into(),
                label: SpamLabel::Spam,
            },
        )
        .expect("apply+reseal hybrid from fresh");
        assert_eq!(out.sample_count, 1);

        let plaintext =
            open_sealed_inner_record_hybrid(&out.sealed_model, &secret, mdk).expect("unseal");
        assert_eq!(
            SpamModel::from_bytes(&plaintext)
                .expect("decode")
                .spam_messages,
            1
        );
    }

    #[test]
    fn apply_and_reseal_hybrid_undo_through_the_seal_restores_prior_model() {
        let (secret, _) = derive_recipient_hpke_keypair(&[7u8; 32]);
        let xwing = derive_recipient_xwing_keypair(&[7u8; 32]);
        let mdk = xwing.secret.mlkem_decaps_key();

        let empty_sealed = seal_to_recipient_xwing(&SpamModel::new().to_bytes(), &xwing.public)
            .expect("seal xwing")
            .to_canonical_bytes()
            .expect("encode");
        let trained = apply_and_reseal_hybrid(
            Some(&empty_sealed),
            &secret,
            mdk,
            &xwing.public,
            &ModelWriteOp::Train {
                text: "buy cheap pills now".into(),
                label: SpamLabel::Spam,
            },
        )
        .expect("train");

        let undone = apply_and_reseal_hybrid(
            Some(&trained.sealed_model),
            &secret,
            mdk,
            &xwing.public,
            &ModelWriteOp::Undo {
                delta: trained.delta.clone(),
                label: SpamLabel::Spam,
            },
        )
        .expect("undo");
        assert_eq!(undone.sample_count, 0);

        let plaintext =
            open_sealed_inner_record_hybrid(&undone.sealed_model, &secret, mdk).expect("unseal");
        assert_eq!(
            SpamModel::from_bytes(&plaintext).expect("decode"),
            SpamModel::new()
        );
    }

    #[test]
    fn apply_and_reseal_hybrid_rejects_a_wrong_key() {
        let xwing = derive_recipient_xwing_keypair(&[7u8; 32]);
        let sealed = seal_to_recipient_xwing(&SpamModel::new().to_bytes(), &xwing.public)
            .expect("seal xwing")
            .to_canonical_bytes()
            .expect("encode");
        // A different MSEK's secrets cannot unwrap the blob.
        let (wrong_secret, _) = derive_recipient_hpke_keypair(&[9u8; 32]);
        let wrong_xwing = derive_recipient_xwing_keypair(&[9u8; 32]);
        let err = apply_and_reseal_hybrid(
            Some(&sealed),
            &wrong_secret,
            wrong_xwing.secret.mlkem_decaps_key(),
            &xwing.public,
            &ModelWriteOp::Train {
                text: "x".into(),
                label: SpamLabel::Ham,
            },
        )
        .unwrap_err();
        assert!(matches!(err, ModelWriteError::Unseal(_)));
    }

    // ── decode_history_delta (leg 1c content-detection) ─────────────────

    #[test]
    fn history_delta_detects_plaintext_json_array() {
        let stored = br#"["win","prize"]"#;
        assert_eq!(
            decode_history_delta(stored),
            HistoryDelta::Plaintext(BTreeSet::from(["win".to_string(), "prize".to_string()]))
        );
    }

    #[test]
    fn history_delta_detects_sealed_wrapped_blob() {
        // A real sealed delta: a wrapped_blob's canonical bytes are dag-cbor,
        // which never parse as a JSON string array.
        let (_, public) = derive_recipient_hpke_keypair(&[7u8; 32]);
        let sealed = seal_to_recipient(br#"["win","prize"]"#, &public)
            .expect("seal")
            .to_canonical_bytes()
            .expect("encode");
        assert_eq!(
            decode_history_delta(&sealed),
            HistoryDelta::Sealed(sealed.clone())
        );
    }

    #[test]
    fn history_delta_empty_is_absent() {
        assert_eq!(decode_history_delta(&[]), HistoryDelta::Absent);
    }

    // ── History-row seal / unwrap (build-item 3 write side) ─────────────────

    fn sample_delta() -> BTreeSet<String> {
        SpamModel::delta_ngrams("buy cheap pills now")
    }

    /// `encode_history_delta` produces the *exact* plaintext-row byte shape, so a
    /// sealed delta's inner plaintext is byte-symmetric with a server-written row
    /// (the seal is transparent).
    #[test]
    fn encode_history_delta_is_the_plaintext_row_shape() {
        let delta = sample_delta();
        assert_eq!(
            decode_history_delta(&encode_history_delta(&delta)),
            HistoryDelta::Plaintext(delta)
        );
    }

    /// Classical round-trip: encode → seal to the actor's own X25519 key → the
    /// stored blob content-detects as `Sealed` → unwrap (no ML-KEM) restores the
    /// exact delta.
    #[test]
    fn history_delta_seal_unwrap_round_trips_classical() {
        let (secret, public) = test_recipient_keypair();
        let delta = sample_delta();
        let sealed = seal_history_blob(&encode_history_delta(&delta), &public).expect("seal");
        // Opaque at rest — the undo router content-detects it as sealed.
        assert!(matches!(
            decode_history_delta(&sealed),
            HistoryDelta::Sealed(_)
        ));
        assert_eq!(
            unwrap_sealed_history_delta(&sealed, &secret, None).expect("unwrap"),
            delta
        );
    }

    /// Hybrid round-trip: seal to the actor's own X-Wing key → unwrap with the
    /// ML-KEM decaps key restores the delta.
    #[test]
    fn history_delta_seal_unwrap_round_trips_hybrid() {
        let msek = [9u8; 32];
        let (secret, _) = derive_recipient_hpke_keypair(&msek);
        let xwing = derive_recipient_xwing_keypair(&msek);
        let mdk = xwing.secret.mlkem_decaps_key();
        let delta = sample_delta();
        let sealed =
            seal_history_blob_hybrid(&encode_history_delta(&delta), &xwing.public).expect("seal");
        assert_eq!(
            unwrap_sealed_history_delta(&sealed, &secret, Some(mdk)).expect("unwrap"),
            delta
        );
    }

    /// Mixed-suite case: the hybrid opener reads a **classically**-sealed row too, so
    /// a PQ-hybrid actor can still undo a row sealed classically.
    #[test]
    fn hybrid_opener_reads_a_classically_sealed_delta() {
        let msek = [11u8; 32];
        let (secret, public) = derive_recipient_hpke_keypair(&msek);
        let xwing = derive_recipient_xwing_keypair(&msek);
        let mdk = xwing.secret.mlkem_decaps_key();
        let delta = sample_delta();
        let sealed = seal_history_blob(&encode_history_delta(&delta), &public).expect("seal");
        assert_eq!(
            unwrap_sealed_history_delta(&sealed, &secret, Some(mdk)).expect("unwrap"),
            delta
        );
    }

    /// A wrong recipient secret cannot open the sealed delta (fails closed).
    #[test]
    fn unwrap_sealed_history_delta_wrong_key_errors() {
        let (_, public) = derive_recipient_hpke_keypair(&[1u8; 32]);
        let (other_secret, _) = derive_recipient_hpke_keypair(&[2u8; 32]);
        let sealed =
            seal_history_blob(&encode_history_delta(&sample_delta()), &public).expect("seal");
        assert!(matches!(
            unwrap_sealed_history_delta(&sealed, &other_secret, None),
            Err(ModelWriteError::Unseal(_))
        ));
    }

    /// A subject (arbitrary UTF-8) rides `seal_history_blob` / `unwrap_sealed_history_bytes`
    /// with no JSON decode — the display path renders `{subject} · {mailbox}`.
    #[test]
    fn subject_seal_unwrap_round_trips_via_bytes() {
        let (secret, public) = test_recipient_keypair();
        let subject = "Re: cheap meds — 90% off!";
        let sealed = seal_history_blob(subject.as_bytes(), &public).expect("seal");
        let opened = unwrap_sealed_history_bytes(&sealed, &secret, None).expect("unwrap");
        assert_eq!(String::from_utf8(opened).unwrap(), subject);
    }
}
