//! Re-exports `fauna-mail` for UniFFI binding generation.
//!
//! Each consumer language (Swift, Kotlin, C#, plus the future Go path) gets
//! the full public API of fauna-mail by virtue of fauna-ffi being the
//! aggregation point for `uniffi-bindgen generate`.
//!
//! We re-export by name (rather than `pub use fauna_mail::*`) to avoid
//! shadowing conflicts with fauna-ffi's own private `mod auth` and `mod spam`
//! modules. UniFFI only needs the types and functions to be present in the
//! crate namespace; it doesn't need the module paths themselves.
//!
//! **Maintenance note:** when `fauna-mail` adds a new public type or function,
//! add a corresponding `pub use fauna_mail::{...}` line here.

// auth
pub use fauna_mail::{
    ArcVerdict, AuthError, AuthVerdicts, DkimVerdict, DmarcPolicy, DmarcVerdict, SpfVerdict,
    verify_inbound,
};

// kind_registry
pub use fauna_mail::{KindMetadata, lookup_kind};

// parser
pub use fauna_mail::{ParseError, ParsedHeader, ParsedMessage, ParsedMimePart, parse_rfc5322};

// bodystructure (IMAP BODYSTRUCTURE derivation from raw RFC 5322 bytes)
pub use fauna_mail::{BodyStructure, MimeParam, derive_body_structure};

// bodysection (IMAP FETCH BODY[<section>] / BINARY[<section>] extraction from
// raw RFC 5322 bytes)
pub use fauna_mail::{
    BinarySectionSpec, BodySectionPartial, BodySectionSpec, fetch_binary_section,
    fetch_binary_size, fetch_body_section,
};

// envelope (IMAP ENVELOPE derivation from raw RFC 5322 bytes)
pub use fauna_mail::{Envelope, EnvelopeAddress, derive_envelope};

// icalendar (CalDAV PUT validation + RRULE expansion for the MDA)
pub use fauna_mail::{
    ExpandedOccurrence, ICalComponent, ICalDocument, ICalError, ICalParameter, ICalProperty,
    expand_recurrence, parse_icalendar,
};

// spam
pub use fauna_mail::spam::{
    BayesianKnobs, default_bayesian_knobs, weighted_bayesian_milli_for_model,
};
pub use fauna_mail::{
    SpamDisposition, SpamPolicy, combined_spam_score_milli, decide_spam_disposition,
};

// tokenizer
pub use fauna_mail::{CanonicalTokenSet, tokenize};

// ── Wrapped-blob asymmetric unwrap (HPKE-Open) ──
//
// Exposes `unseal_tls_cert` from
// `libs/fauna-mls/src/wrapped_blob/mod.rs` to Go (via UniFFI), so the
// Go fauna-mail-bridge can HPKE-Open the wrapped TLS-cert blob nest
// hands it after WS-RPC auth.
//
// Pattern: the FFI takes the *encoded* wrapped-blob bytes plus the
// 32-byte recipient X25519 secret, decodes + unwraps internally, and
// returns the plaintext bundle as a UniFFI Record. Pushing the
// CBOR-decode through the FFI boundary means the Go side doesn't need
// to mirror `TlsCertBlob` (the wire type) — that stays in Rust where
// the canonical-CBOR encode lives.
//
// Errors are flattened to `FfiError::General { msg }` per the
// project-wide UniFFI error pattern (see `src/lib.rs`); the
// inner `UnwrapError` discriminant is preserved in the message.

use crate::FfiError;
use crate::crypto::bytes32;
use ed25519_dalek::VerifyingKey;
use fauna_mls::wrapped_blob::{
    AEAD_NONCE_LEN, AadBinding, Argon2idParams, AtprotoIdentityBlob as InnerAtprotoIdentityBlob,
    AtprotoIdentityPublishedKeys as InnerAtprotoIdentityPublishedKeys,
    AtprotoSessionSecretBlob as InnerAtprotoSessionSecretBlob, CredentialInput,
    GrantBlob as InnerGrantBlob, GrantWindow, HkdfSha256Params, KdfParams,
    LeafInitKeypair as InnerLeafInitKeypair, MLKEM768_DECAPS_KEY_LEN, MailRecordEnvelope,
    MlsSnapshotBlob, MlsSnapshotPlaintext, ScopeTuple, ServedSetKeys,
    SpamModelCopyBlob as InnerSpamModelCopyBlob, StandingMailKeypair,
    SubmissionToken as InnerSubmissionToken, TlsCertBlob as InnerTlsCertBlob,
    TlsCertBundle as InnerTlsCertBundle, WebdavKeysBlob, WebdavKeysPlaintext, WrappedMsekBlob,
    WrappedSubmissionTokenBlob, XWING_ENCAPS_KEY_LEN, XWingPublicKey, aead::aead_open,
    build_grant_blob as inner_build_grant_blob,
    derive_bridge_service_user_mlkem768 as inner_derive_bridge_service_user_mlkem768,
    derive_recipient_hpke_keypair as inner_derive_recipient_hpke_keypair,
    derive_recipient_mail_capability_secret as inner_derive_recipient_mail_capability_secret,
    derive_recipient_xwing_keypair as inner_derive_recipient_xwing_keypair, format::SerKdfParams,
    generate_x25519_keypair as inner_generate_x25519_keypair, kdf::unwrap_key,
    open_mail_record_standing, seal_mls_snapshot as inner_seal_mls_snapshot,
    seal_spam_model_copy as inner_seal_spam_model_copy,
    seal_submission_token as inner_seal_submission_token, seal_tls_cert as inner_seal_tls_cert,
    seal_to_recipient as inner_seal_to_recipient,
    seal_to_recipient_xwing as inner_seal_to_recipient_xwing,
    seal_wrapped_msek as inner_seal_wrapped_msek,
    unseal_atproto_identity as inner_unseal_atproto_identity,
    unseal_atproto_session_secret as inner_unseal_atproto_session_secret,
    unseal_capability as inner_unseal_capability,
    unseal_capability_hybrid as inner_unseal_capability_hybrid, unseal_mls_snapshot,
    unseal_spam_model_copy as inner_unseal_spam_model_copy,
    unseal_submission_token as inner_unseal_submission_token,
    unseal_tls_cert as inner_unseal_tls_cert,
    unseal_webdav_keys_blob as inner_unseal_webdav_keys_blob, unseal_wrapped_msek,
};
use std::sync::Mutex;
use zeroize::Zeroizing;

/// TLS certificate + private-key bundle, plaintext side of an HPKE-Opened
/// `TlsCertBlob`. Mirrors `fauna_mls::wrapped_blob::TlsCertBundle`
/// field-for-field; `cert_chain` is one or more concatenated PEM-encoded
/// certificates (server cert first, then intermediates), `priv_key` is the
/// PEM-encoded private key.
///
/// The Rust source struct zeroes its `priv_key` on drop; the Go side
/// receives a copy of the bytes (UniFFI copies across the boundary),
/// so the bridge is responsible for keeping the lifetime of that copy
/// short.
#[derive(uniffi::Record)]
pub struct TlsCertBundle {
    pub cert_chain: Vec<u8>,
    pub priv_key: Vec<u8>,
    pub expires_at: u64,
    pub issued_at: u64,
}

/// HPKE-Open a wrapped TLS-cert blob.
///
/// `blob_bytes` is the canonical DAG-CBOR encoding of `TlsCertBlob` as
/// nest serves it via `fauna.bridges.fetch_tls_cert_blob`.
/// `recipient_x25519_secret` is the bridge's 32-byte X25519 private key
/// (from its on-disk service-user keyfile).
///
/// Errors:
///   - `InvalidFormat`: blob CBOR malformed, wrong kind/version, or
///     `recipient_x25519_secret` is not exactly 32 bytes.
///   - `HpkeFailed`: wrong recipient secret, blob tampered, or
///     `(role, bridge_id, domain)` substitution attempt.
#[uniffi::export]
pub fn unseal_tls_cert_blob(
    blob_bytes: Vec<u8>,
    recipient_x25519_secret: Vec<u8>,
) -> Result<TlsCertBundle, FfiError> {
    let secret = secret_32(&recipient_x25519_secret)?;
    let blob =
        InnerTlsCertBlob::from_canonical_bytes(&blob_bytes).map_err(|e| FfiError::General {
            msg: format!("decode tls-cert blob: {e}"),
        })?;
    let bundle = inner_unseal_tls_cert(&blob, &secret).map_err(|e| FfiError::General {
        msg: format!("unseal tls-cert: {e}"),
    })?;
    Ok(TlsCertBundle {
        cert_chain: bundle.cert_chain.clone(),
        priv_key: bundle.priv_key.clone(),
        expires_at: bundle.expires_at,
        issued_at: bundle.issued_at,
    })
}

/// ATProto identity-key bundle, plaintext side of an HPKE-Opened
/// `AtprotoIdentityBlob` (the bridge-custodied half of the did:plc
/// key-custody split — `atproto-pds-bridge.md` § State & data shape).
/// Mirrors `fauna_mls::wrapped_blob::AtprotoIdentityKeyBundle`
/// field-for-field; the `*_priv` fields are raw 32-byte K-256 scalars.
///
/// The Rust source struct zeroes its scalars on drop; the Go side receives a
/// copy (UniFFI copies across the boundary) — the atproto bridge keeps the
/// lifetime short.
#[derive(uniffi::Record)]
pub struct AtprotoIdentityKeyBundle {
    /// The 32-byte actor id the keys were MINTED for — provenance, never an
    /// identity check: the blob moves to a successor unchanged, so after a
    /// succession this names an ancestor of the account that was asked about.
    pub actor_id: Vec<u8>,
    /// Repo-commit signing key: raw scalar.
    pub signing_priv: Vec<u8>,
    /// Curve tag for `signing_priv` (currently always `"k256"`).
    pub signing_curve: String,
    /// The signing key's public half as a `did:key:z…` string.
    pub signing_pub_did_key: String,
    /// The bridge's junior PLC rotation key: raw scalar.
    pub rotation_priv: Vec<u8>,
    /// Curve tag for `rotation_priv` (currently always `"k256"`).
    pub rotation_curve: String,
    /// The bridge rotation key's public half as a `did:key:z…` string.
    pub rotation_pub_did_key: String,
    pub issued_at: u64,
}

/// HPKE-Open a wrapped ATProto identity-key blob.
///
/// `blob_bytes` is the canonical DAG-CBOR encoding of `AtprotoIdentityBlob` as
/// nest serves it via `fauna.bridges.atproto.fetch_identity_key_blob`.
/// `recipient_x25519_secret` is the atproto.pds bridge's 32-byte X25519
/// private key (from its on-disk service-user keyfile).
///
/// `expected_signing_pub_did_key` / `expected_rotation_pub_did_key` are the two
/// published keys the SAME fetch reply carries beside the blob (the identity
/// row's record). The blob is refused unless the keys inside are exactly those
/// — the one check that stops identity X's whole blob opening where identity
/// Y's was asked for. Bound to the keys, never to an actor id: after a
/// succession the returned `actor_id` names an ancestor of the account the
/// caller asked about, for the life of the DID, and is provenance only.
///
/// Errors: see `unseal_tls_cert_blob`. The AAD binds the blob's own actor
/// index, which detects an EDITED index (`HpkeFailed`) and nothing more; the
/// whole-blob substitution fails as a published-key mismatch, as does an empty
/// expectation.
#[uniffi::export]
pub fn unseal_atproto_identity_blob(
    blob_bytes: Vec<u8>,
    recipient_x25519_secret: Vec<u8>,
    expected_signing_pub_did_key: String,
    expected_rotation_pub_did_key: String,
) -> Result<AtprotoIdentityKeyBundle, FfiError> {
    let secret = secret_32(&recipient_x25519_secret)?;
    let blob = InnerAtprotoIdentityBlob::from_canonical_bytes(&blob_bytes).map_err(|e| {
        FfiError::General {
            msg: format!("decode atproto identity blob: {e}"),
        }
    })?;
    let expected = InnerAtprotoIdentityPublishedKeys {
        signing_pub_did_key: &expected_signing_pub_did_key,
        rotation_pub_did_key: &expected_rotation_pub_did_key,
    };
    let bundle = inner_unseal_atproto_identity(&blob, &secret, &expected).map_err(|e| {
        FfiError::General {
            msg: format!("unseal atproto identity: {e}"),
        }
    })?;
    Ok(AtprotoIdentityKeyBundle {
        actor_id: bundle.actor_id.clone(),
        signing_priv: bundle.signing_priv.clone(),
        signing_curve: bundle.signing_curve.clone(),
        signing_pub_did_key: bundle.signing_pub_did_key.clone(),
        rotation_priv: bundle.rotation_priv.clone(),
        rotation_curve: bundle.rotation_curve.clone(),
        rotation_pub_did_key: bundle.rotation_pub_did_key.clone(),
        issued_at: bundle.issued_at,
    })
}

/// The unsealed bridge-wide ATProto session-token secret (HS256 signing key).
///
/// The Rust source struct zeroes the secret on drop; the Go side receives a
/// copy (UniFFI copies across the boundary) — the atproto bridge holds it for
/// the process lifetime as its JWT signing key, exactly as it would the
/// interim in-memory secret this replaces.
#[derive(uniffi::Record)]
pub struct AtprotoSessionSecretBundle {
    /// The 32-byte HMAC-SHA-256 signing secret.
    pub secret: Vec<u8>,
    pub issued_at: u64,
}

/// HPKE-Open a wrapped ATProto session-secret blob.
///
/// `blob_bytes` is the canonical DAG-CBOR encoding of
/// `AtprotoSessionSecretBlob` as nest serves it via
/// `fauna.bridges.atproto.fetch_session_secret_blob`.
/// `recipient_x25519_secret` is the atproto.pds bridge's 32-byte X25519
/// private key (from its on-disk service-user keyfile).
///
/// Errors: see `unseal_tls_cert_blob` (a bridge-substitution attempt fails as
/// `HpkeFailed` — the blob's AAD binds `(bridge_role, bridge_id)`).
#[uniffi::export]
pub fn unseal_atproto_session_secret_blob(
    blob_bytes: Vec<u8>,
    recipient_x25519_secret: Vec<u8>,
) -> Result<AtprotoSessionSecretBundle, FfiError> {
    let secret = secret_32(&recipient_x25519_secret)?;
    let blob = InnerAtprotoSessionSecretBlob::from_canonical_bytes(&blob_bytes).map_err(|e| {
        FfiError::General {
            msg: format!("decode atproto session-secret blob: {e}"),
        }
    })?;
    let bundle =
        inner_unseal_atproto_session_secret(&blob, &secret).map_err(|e| FfiError::General {
            msg: format!("unseal atproto session secret: {e}"),
        })?;
    Ok(AtprotoSessionSecretBundle {
        secret: bundle.secret.clone(),
        issued_at: bundle.issued_at,
    })
}

/// One HPKE-Opened scope key of a capability grant — the plaintext side of a
/// [`fauna_mls::wrapped_blob::WrappedScopeKey`]. `key` is the **minimal derived
/// content key** for one kind (the recipient-mail HPKE secret, a tier
/// `period_key`, or an index-segment key) — never MSEK / identity / index-master
/// (`key-material-hierarchy.md` rule #7). `class`/`kind`/`tier`/`epoch` echo the
/// scope tuple the key serves, so the Go holder can index its cache by scope.
///
/// The `key` bytes are a secret: the Go side receives a copy (UniFFI copies
/// across the boundary), so the capability holder is responsible for keeping the
/// lifetime short and zeroizing on revoke.
#[derive(uniffi::Record)]
pub struct UnsealedScopeKey {
    pub class: String,
    pub kind: Option<String>,
    pub tier: Option<String>,
    pub epoch: Option<u64>,
    /// The bus factor this wrap's license is confined to
    /// (`ScopeTuple::factor`): `None` for the built-in perimeter factors,
    /// `Some("labeler:<hex>")` for a per-labeler grant. AAD-bound, so it is
    /// exactly what the mint declared — the holder selects keys by it.
    pub factor: Option<String>,
    pub key: Vec<u8>,
}

/// A capability grant with every wrapped scope key HPKE-Opened — the plaintext
/// side of a [`fauna_mls::wrapped_blob::GrantBlob`] the nest serves via
/// `fauna.capabilities.fetch`. `owner_actor_id` (32) + `grant_id` (16) come from
/// the blob's `ix`; `epoch_start`/`epoch_end` are its advisory window; `keys`
/// carries one [`UnsealedScopeKey`] per key-bearing scope tuple (a keyless
/// `content.label-write` tuple in the blob's declared `scope` produces no key).
///
/// All keys are sealed to the SAME holder (the grant's `holder` pubkey), so a
/// wrong holder secret fails every one — the FFI returns an `HpkeFailed` error
/// for the whole grant rather than a partial result (the Go holder then omits
/// that grant from its refreshed set).
#[derive(uniffi::Record)]
pub struct UnsealedCapabilityGrant {
    pub owner_actor_id: Vec<u8>,
    pub grant_id: Vec<u8>,
    pub epoch_start: u64,
    pub epoch_end: u64,
    pub keys: Vec<UnsealedScopeKey>,
}

/// HPKE-Open every wrapped scope key of a capability grant.
///
/// `grant_blob_bytes` is the canonical DAG-CBOR encoding of `GrantBlob` as the
/// nest serves it via `fauna.capabilities.fetch` (each element of that reply's
/// `grants` list). `holder_x25519_secret` is the bridge service-user's 32-byte
/// X25519 private key (the enrolled holder secret — **not** the actor identity),
/// from its on-disk keyfile. `holder_mlkem_dk` is the holder's 2400-byte
/// ML-KEM-768 decapsulation key (`Some` once the bridge has derived + published
/// its post-quantum key — PQ-CAP-2), or `None` for a classical-only holder. The
/// `owner_actor_id` the seal's AAD binds is read from the blob's `ix` (design
/// § Phase 2 Step 2 § 2.3), so the caller passes only the blob + its secrets.
///
/// **Suite dispatch (post-quantum overlay, PQ-CAP-1/2):** when `holder_mlkem_dk`
/// is `Some`, every wrapped key is opened via `unseal_capability_hybrid`, which
/// opens **both** a classical X25519 wrap (ignoring the dk) and a hybrid X-Wing
/// wrap (using both halves) — so a holder that has published an ML-KEM ek can
/// drain classical and hybrid grants alike with one call. When
/// `None`, keys open via the classical `unseal_capability`; a hybrid wrap
/// reaching that path fails with a typed `InvalidFormat` naming the hybrid
/// opener (never a silent mis-decrypt).
///
/// Returns the grant's index + window metadata plus every unsealed scope key.
/// The grant is unsealed **all-or-nothing**: since every key is sealed to the
/// one `holder`, a wrong secret / owner-scope-epoch substitution / tampered
/// ciphertext fails the first key and the whole call errors.
///
/// Errors:
///   - `InvalidFormat`: blob CBOR malformed, wrong kind/version, a wrong HPKE
///     `enc` length, `holder_x25519_secret` not 32 bytes, `holder_mlkem_dk`
///     present but not 2400 bytes, or `ix.owner` not 32.
///   - `HpkeFailed`: wrong holder secret, blob tampered, or an owner/scope/epoch
///     substitution attempt.
#[uniffi::export]
pub fn unseal_capability_grant(
    grant_blob_bytes: Vec<u8>,
    holder_x25519_secret: Vec<u8>,
    holder_mlkem_dk: Option<Vec<u8>>,
) -> Result<UnsealedCapabilityGrant, FfiError> {
    let secret = secret_32(&holder_x25519_secret)?;
    // Materialize the optional ML-KEM decaps key once (length-gated), zeroized on
    // drop. `Some` selects the hybrid opener for every key; `None` the classical.
    let mlkem_dk = match &holder_mlkem_dk {
        Some(dk_bytes) => {
            if dk_bytes.len() != MLKEM768_DECAPS_KEY_LEN {
                return Err(FfiError::General {
                    msg: format!(
                        "holder_mlkem_dk must be {} bytes (ML-KEM-768 decaps key), got {}",
                        MLKEM768_DECAPS_KEY_LEN,
                        dk_bytes.len()
                    ),
                });
            }
            let mut dk = Zeroizing::new([0u8; MLKEM768_DECAPS_KEY_LEN]);
            dk.copy_from_slice(dk_bytes);
            Some(dk)
        }
        None => None,
    };
    let blob =
        InnerGrantBlob::from_canonical_bytes(&grant_blob_bytes).map_err(|e| FfiError::General {
            msg: format!("decode capability grant blob: {e}"),
        })?;
    // owner_actor_id is `ix.0`; `from_canonical_bytes` does not enforce its
    // length, so pin it to 32 here before the AAD arm indexes it.
    let owner: [u8; 32] = blob
        .index
        .0
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::General {
            msg: format!(
                "grant owner_actor_id must be 32 bytes, got {}",
                blob.index.0.len()
            ),
        })?;
    let mut keys = Vec::with_capacity(blob.wrapped_keys.len());
    for wk in &blob.wrapped_keys {
        let key = match &mlkem_dk {
            Some(dk) => inner_unseal_capability_hybrid(wk, &owner, &secret, dk),
            None => inner_unseal_capability(wk, &owner, &secret),
        }
        .map_err(|e| FfiError::General {
            msg: format!("unseal capability scope key: {e}"),
        })?;
        keys.push(UnsealedScopeKey {
            class: wk.scope.class.clone(),
            kind: wk.scope.kind.clone(),
            tier: wk.scope.tier.clone(),
            epoch: wk.epoch,
            factor: wk.scope.factor.clone(),
            key,
        });
    }
    Ok(UnsealedCapabilityGrant {
        owner_actor_id: blob.index.0.clone(),
        grant_id: blob.index.1.clone(),
        epoch_start: blob.window.0,
        epoch_end: blob.window.1,
        keys,
    })
}

/// One sealed-to-holder spam-model copy paired with the owner the worklist
/// claims it belongs to — the holder-side input of
/// [`aggregate_spam_model_copies`]. `sealed_copy` is the canonical
/// `SpamModelCopyBlob` bytes the nest served from
/// `fauna.capabilities.spam_baseline_worklist`.
#[derive(uniffi::Record)]
pub struct SpamBaselineCopyInput {
    /// The contributing owner per the worklist row (32 bytes) — cross-checked
    /// against the blob's own AAD-bound owner index at open time, so a
    /// mis-attributed copy counts `unreadable`, never a wrong contributor.
    pub owner_actor_id: Vec<u8>,
    /// Canonical `SpamModelCopyBlob` bytes (nest-opaque; holder-openable).
    pub sealed_copy: Vec<u8>,
}

/// The holder-side merge result of [`aggregate_spam_model_copies`] — what the
/// aggregation holder submits back via `fauna.capabilities.submit_spam_baseline`.
#[derive(uniffi::Record)]
pub struct SpamBaselineAggregate {
    /// The additive n-gram merge (`SpamModel::merge`) of every copy that
    /// opened + decoded — plaintext `SpamModel` serde_json bytes, or empty
    /// when none did. The nest folds this with its own plaintext-row merge and
    /// applies the size cap after the union fold.
    pub merged_model: Vec<u8>,
    /// How many distinct contributors merged (the k-anonymity floor counts
    /// these alongside the nest's plaintext contributors).
    pub contributors: u32,
    /// How many copies failed to open / decode / matched the wrong owner —
    /// surfaced so the publish reply's skipped count stays honest
    /// (`mail-spam.md` § Encrypted-mode interaction, ratified 2026-07-13).
    pub unreadable: u32,
    /// The `owner_actor_id` of every copy that opened + decoded, in worklist
    /// order — what the holder names in `submit_spam_baseline`'s
    /// `merged_contributors` so the nest records exactly these as summed
    /// (`mail-spam.md` § Cold start Path 2 → *A contributor's departure
    /// withdraws the baseline*; additive 2026-09-27). Always
    /// `contributors` long.
    #[uniffi(default = [])]
    pub merged_contributors: Vec<Vec<u8>>,
}

/// Seal a plaintext `fauna_mail::spam::SpamModel` serde_json blob to the
/// aggregation holder's published X25519 pubkey — the **seal-side twin** of
/// [`aggregate_spam_model_copies`], producing the `SpamModelCopyBlob` a
/// contributor's client/agent attaches to `put_spam_model`'s `holder_copy`
/// (`mail-spam.md` § Encrypted-mode interaction — the keyless
/// `content.read{spam-model}` shape). The copy is **owner-bound**:
/// `owner_actor_id` is folded into the AEAD's AAD, so a holder that later opens
/// it (via [`aggregate_spam_model_copies`]) cross-checks the self-described
/// owner and a re-attributed copy fails AEAD-open.
///
/// `holder_mlkem_ek` selects the wrap suite: a valid 1184-byte ML-KEM-768
/// encapsulation key ⇒ the X-Wing (hybrid) seal (a harvested holder copy then
/// is not a CRQC-openable read of the contributor's model — the harvest-now-
/// decrypt-later posture the copy deserves, matching the grant wrap); an empty
/// `ek` ⇒ the classical X25519 seal. Returns canonical `SpamModelCopyBlob`
/// bytes, nest-opaque and holder-openable.
///
/// The production consumer of the underlying seal is (b)'s client-side
/// copy-seal (the shared `MailSettingsMachine` write path re-seals the holder
/// copy atomically with each opted-in `put_spam_model`; native/wasm glue
/// pending).
/// This FFI façade is the twin of the already-exported merge side; the tier_3
/// spam-baseline-drain harness (`seal-helper-testonly seal-spam-model-copy`)
/// drives it today so the seal→drain→merge path is proven end-to-end ahead of
/// that client wiring.
///
/// Errors on a malformed holder key (`holder_x25519_pubkey` / `owner_actor_id`
/// not 32 bytes) or a canonical-encode failure.
#[uniffi::export]
pub fn seal_spam_model_copy(
    model_bytes: Vec<u8>,
    owner_actor_id: Vec<u8>,
    holder_x25519_pubkey: Vec<u8>,
    holder_mlkem_ek: Option<Vec<u8>>,
) -> Result<Vec<u8>, FfiError> {
    let owner = bytes32(&owner_actor_id, "owner_actor_id")?;
    let holder = bytes32(&holder_x25519_pubkey, "holder_x25519_pubkey")?;
    let blob =
        inner_seal_spam_model_copy(&model_bytes, &owner, &holder, holder_mlkem_ek.as_deref())
            .map_err(|e| FfiError::General {
                msg: format!("seal_spam_model_copy: {e}"),
            })?;
    blob.to_canonical_bytes().map_err(|e| FfiError::General {
        msg: format!("seal_spam_model_copy encode: {e}"),
    })
}

/// Open + merge a `publish_spam_baseline` worklist of sealed-to-holder
/// spam-model copies with the holder's **own** service-user key halves — the
/// holder-side compute of the keyless `content.read{spam-model}` shape
/// (`mail-spam.md` § Encrypted-mode interaction; the whole
/// unseal→decode→merge loop stays in shared Rust so the Go drain worker is a
/// thin transport shim, priority #2). No key of any *user's* is involved:
/// each copy was sealed by its contributor to the holder's pubkey
/// (`seal_spam_model_copy`), and this opens with the holder's X25519 secret
/// (+ ML-KEM dk for hybrid copies, the same halves
/// [`unseal_capability_grant`] takes).
///
/// Per-copy failures (wrong suite half, tampered bytes, owner mismatch versus
/// the worklist row, undecodable plaintext) count `unreadable` and never fail
/// the call — one bad copy must not block the deployment's baseline.
///
/// Errors only on malformed holder key material (`holder_x25519_secret` not
/// 32 bytes; `holder_mlkem_dk` present but not 2400 bytes).
#[uniffi::export]
pub fn aggregate_spam_model_copies(
    copies: Vec<SpamBaselineCopyInput>,
    holder_x25519_secret: Vec<u8>,
    holder_mlkem_dk: Option<Vec<u8>>,
) -> Result<SpamBaselineAggregate, FfiError> {
    let secret = secret_32(&holder_x25519_secret)?;
    let mlkem_dk = match &holder_mlkem_dk {
        Some(dk_bytes) => {
            if dk_bytes.len() != MLKEM768_DECAPS_KEY_LEN {
                return Err(FfiError::General {
                    msg: format!(
                        "holder_mlkem_dk must be {} bytes (ML-KEM-768 decaps key), got {}",
                        MLKEM768_DECAPS_KEY_LEN,
                        dk_bytes.len()
                    ),
                });
            }
            let mut dk = Zeroizing::new([0u8; MLKEM768_DECAPS_KEY_LEN]);
            dk.copy_from_slice(dk_bytes);
            Some(dk)
        }
        None => None,
    };
    let mut merged = fauna_mail::spam::SpamModel::new();
    let mut contributors: u32 = 0;
    let mut unreadable: u32 = 0;
    let mut merged_contributors: Vec<Vec<u8>> = Vec::new();
    for copy in &copies {
        let opened = InnerSpamModelCopyBlob::from_canonical_bytes(&copy.sealed_copy)
            .ok()
            .filter(|blob| blob.index.0.as_slice() == copy.owner_actor_id.as_slice())
            .and_then(|blob| inner_unseal_spam_model_copy(&blob, &secret, mlkem_dk.as_deref()).ok())
            .and_then(|bytes| fauna_mail::spam::SpamModel::from_bytes(&bytes));
        match opened {
            Some(model) => {
                merged.merge(&model);
                contributors = contributors.saturating_add(1);
                merged_contributors.push(copy.owner_actor_id.clone());
            }
            None => unreadable = unreadable.saturating_add(1),
        }
    }
    let merged_model = if contributors == 0 {
        Vec::new()
    } else {
        merged.to_bytes()
    };
    Ok(SpamBaselineAggregate {
        merged_model,
        contributors,
        unreadable,
        merged_contributors,
    })
}

/// A declared capability scope tuple plus its minimal derived payload key — the
/// mint-side input to [`build_capability_grant_blob`]. Mirrors the design § 2.1
/// scope table: `class`/`kind`/`tier` name the tuple (`"content.read"`+`"mail"`,
/// `"content.read"`+`"post"`+a tier, `"content.label-write"`, …); `payload` is
/// the minimal derived per-kind key it grants — the recipient-mail HPKE secret /
/// a tier `period_key` / an index-segment key — or `None` for the keyless
/// `content.label-write` tuple (declared in the grant's `scope`, carrying no
/// wrapped key).
#[derive(uniffi::Record)]
pub struct CapabilityScopeInput {
    pub class: String,
    pub kind: Option<String>,
    pub tier: Option<String>,
    /// The per-labeler license qualifier (`ScopeTuple::factor`): `None` for
    /// the composed MDA role, `Some("labeler:<hex>")` to confine the tuple —
    /// and every wrap it carries — to one community labeler.
    pub factor: Option<String>,
    pub payload: Option<Vec<u8>>,
}

/// Build a user-minted capability grant blob — the "build `GrantBlob`" half of
/// the client-side mint (design § Phase 2 Step 2 § 2.6). The native apps
/// (UniFFI) and the tier_3 re-score-drain harness call this.
///
/// It derives no keys itself: the caller — the owner's client, holding the
/// content root off-box — passes each already-derived **minimal** payload (never
/// MSEK / identity / index-master, `key-material-hierarchy.md` rule #7). Each
/// key-bearing tuple is HPKE-sealed to `holder_pubkey`, AAD-bound to
/// owner+scope+epoch (so it is un-substitutable at open time); a keyless
/// `content.label-write` scope pairs with `payload: None` — declared in the
/// grant's `scope`, no `WrappedScopeKey`. The result is the canonical DAG-CBOR
/// `GrantBlob`, ready for `fauna.capabilities.mint` (`MintGrantRequest {
/// grant_blob }`).
///
/// **Post-quantum wrap suite:** `holder_mlkem_ek` is the holder's published
/// ML-KEM encapsulation key (`fauna.bridges.fetch_bridge_pubkey`), or `None` for
/// a holder that hasn't published one. When present + valid-length every
/// key-bearing tuple is wrapped **X-Wing** (ML-KEM-768 ∥ X25519), so a harvested
/// `capability_grants` row is not a CRQC-openable bypass of the closed
/// mail-at-rest seal; absent, the classical X25519 wrap (the non-erroring
/// degrade). The holder opens either suite via [`unseal_capability_grant`] with
/// its `holder_mlkem_dk`. `post-quantum.md` § Capability-grant holders.
///
/// Master-key regime only (every content kind is standing-keyed today): each
/// key-bearing tuple gets exactly one `WrappedScopeKey` with `epoch: None`;
/// `epoch_start`/`epoch_end` are the advisory honest window (design § Phase 2
/// Step 1). The epoch-sealed regime (one key per epoch, Phase 3 for mail) is a
/// later generalization, deliberately not built here.
///
/// # Errors
///
/// `FfiError::General` carrying:
///   - `"grant owner_actor_id must be 32 bytes ..."` / `"... grant_id must be 16
///     bytes ..."` / `"... holder_pubkey must be 32 bytes ..."` — a mis-sized
///     index or holder pubkey;
///   - `"build capability grant blob: ..."` — a key-bearing tuple's HPKE seal
///     failed (e.g. a malformed `holder_pubkey`);
///   - `"encode capability grant blob: ..."` — canonical CBOR encoding failed.
#[uniffi::export]
pub fn build_capability_grant_blob(
    owner_actor_id: Vec<u8>,
    grant_id: Vec<u8>,
    holder_pubkey: Vec<u8>,
    holder_mlkem_ek: Option<Vec<u8>>,
    epoch_start: u64,
    epoch_end: u64,
    scopes: Vec<CapabilityScopeInput>,
) -> Result<Vec<u8>, FfiError> {
    let owner: [u8; 32] = owner_actor_id
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::General {
            msg: format!(
                "grant owner_actor_id must be 32 bytes, got {}",
                owner_actor_id.len()
            ),
        })?;
    let gid: [u8; 16] = grant_id
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::General {
            msg: format!("grant grant_id must be 16 bytes, got {}", grant_id.len()),
        })?;
    let holder: [u8; 32] = holder_pubkey
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::General {
            msg: format!(
                "grant holder_pubkey must be 32 bytes, got {}",
                holder_pubkey.len()
            ),
        })?;
    let scope_pairs: Vec<(ScopeTuple, Option<Vec<u8>>)> = scopes
        .into_iter()
        .map(|s| {
            (
                ScopeTuple {
                    class: s.class,
                    kind: s.kind,
                    tier: s.tier,
                    set: None,
                    factor: s.factor,
                },
                s.payload,
            )
        })
        .collect();
    let blob = inner_build_grant_blob(
        &owner,
        &gid,
        &holder,
        holder_mlkem_ek.as_deref(),
        GrantWindow(epoch_start, epoch_end),
        &scope_pairs,
    )
    .map_err(|e| FfiError::General {
        msg: format!("build capability grant blob: {e}"),
    })?;
    blob.to_canonical_bytes().map_err(|e| FfiError::General {
        msg: format!("encode capability grant blob: {e}"),
    })
}

/// One prior (retired) MSEK generation for [`build_bounded_mail_grant_blob`]
/// — the FFI face of `fauna_mls::wrapped_blob::PriorMsekGeneration`
/// (content-sealing-epochs amendment 2026-07-19).
#[derive(uniffi::Record)]
pub struct BoundedMailPriorGeneration {
    /// The retired generation's 32-byte MSEK.
    pub msek: Vec<u8>,
    /// Unix-seconds instant the rotation retiring it committed.
    pub retired_at_unix: u64,
}

/// Build a **bounded** (epoch-wrapped) mail capability grant blob — the
/// content-sealing-epochs § 2 bounded regime: one `WrappedScopeKey { epoch:
/// Some(e) }` per epoch intersecting the `[window_start_secs,
/// window_end_secs]` unix-seconds window, and NEVER the standing secret (the
/// mint policy [`build_capability_grant_blob`]'s master-key shape carries).
/// Calls the SAME shared assembly core as the production
/// `fauna_client_capabilities::mint_bounded_mail_grant`
/// (`fauna_mls::wrapped_blob::build_bounded_mail_grant`) — one core,
/// so this mirror cannot drift — the seal-helper drives this so a
/// tier_3 harness gets a real bounded user-mint (and the Go side gets a real
/// epoch-carrying blob to unseal) without the client UI.
///
/// `prior_generations` (newest-first, the `MailConfig.prior_mseks` order)
/// carries retired MSEK generations so a post-rotation mint covers
/// pre-rotation in-window epochs per the 2026-07-19 amendment; pass empty
/// for the no-rotation case. `include_label_write` adds the keyless
/// `content.label-write` scope the production drain grant carries. `factor`
/// confines the grant to one bus factor (`None` = the composed MDA role;
/// `Some("labeler:<hex>")` = the per-labeler grant a subscription mints —
/// the production `fauna_client_capabilities::mint_bounded_mail_labeler_grant`
/// shape).
///
/// # Errors
///
/// `FfiError::General` on mis-sized inputs or a wrap/encode failure, same
/// shapes as [`build_capability_grant_blob`].
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn build_bounded_mail_grant_blob(
    owner_actor_id: Vec<u8>,
    grant_id: Vec<u8>,
    holder_pubkey: Vec<u8>,
    holder_mlkem_ek: Option<Vec<u8>>,
    window_start_secs: u64,
    window_end_secs: u64,
    msek: Vec<u8>,
    prior_generations: Vec<BoundedMailPriorGeneration>,
    include_label_write: bool,
    factor: Option<String>,
) -> Result<Vec<u8>, FfiError> {
    use fauna_mls::wrapped_blob::{PriorMsekGeneration, build_bounded_mail_grant};
    let owner: [u8; 32] = owner_actor_id
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::General {
            msg: format!(
                "grant owner_actor_id must be 32 bytes, got {}",
                owner_actor_id.len()
            ),
        })?;
    let gid: [u8; 16] = grant_id
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::General {
            msg: format!("grant grant_id must be 16 bytes, got {}", grant_id.len()),
        })?;
    let holder: [u8; 32] = holder_pubkey
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::General {
            msg: format!(
                "grant holder_pubkey must be 32 bytes, got {}",
                holder_pubkey.len()
            ),
        })?;
    let msek_arr: [u8; 32] = msek.as_slice().try_into().map_err(|_| FfiError::General {
        msg: format!("msek must be 32 bytes, got {}", msek.len()),
    })?;
    let priors: Vec<PriorMsekGeneration> = prior_generations
        .into_iter()
        .enumerate()
        .map(|(i, g)| {
            let msek: [u8; 32] = g
                .msek
                .as_slice()
                .try_into()
                .map_err(|_| FfiError::General {
                    msg: format!(
                        "prior_generations[{i}].msek must be 32 bytes, got {}",
                        g.msek.len()
                    ),
                })?;
            Ok(PriorMsekGeneration {
                msek,
                retired_at_unix: g.retired_at_unix,
            })
        })
        .collect::<Result<_, FfiError>>()?;
    let blob = build_bounded_mail_grant(
        &owner,
        &gid,
        &holder,
        holder_mlkem_ek.as_deref(),
        GrantWindow(window_start_secs, window_end_secs),
        &msek_arr,
        &priors,
        include_label_write,
        factor.as_deref(),
    )
    .map_err(|e| FfiError::General {
        msg: format!("build bounded mail grant blob: {e}"),
    })?;
    blob.to_canonical_bytes().map_err(|e| FfiError::General {
        msg: format!("encode bounded mail grant blob: {e}"),
    })
}

/// Open a `MailRecordEnvelope` with a RAW recipient content key — the
/// capability-holder counterpart of [`MLSCapability::open_mail_record`], for the
/// **background** case where no AUTH'd session (and hence no
/// `MlsSnapshotPlaintext`) exists: the re-score / re-index drain opens a sealed
/// record with the minimal derived key a user-minted `content.read{mail}` grant
/// carried ([`unseal_capability_grant`] → `UnsealedScopeKey.key`; design § 2.5
/// drain rendezvous).
///
/// `key` is the grant's unsealed payload and self-describes its reach by length
/// — the `content.read{mail}` payload byte contract:
///   - **32 bytes**: the recipient-mail X25519 HPKE secret
///     (`derive_recipient_hpke_keypair`) — opens **classical** records only; an
///     X-Wing record returns a typed error (never a mis-decrypt).
///   - **32 + 2400 bytes** (`x25519_secret ∥ mlkem_decaps_key`, the
///     `derive_recipient_xwing_keypair` halves): opens **both** classical and
///     hybrid (X-Wing) records, exactly as the AUTH'd MDA session does via the
///     snapshot's `mlkem_dk`.
///
/// Either shape is a minimal derived per-kind key — never MSEK / identity /
/// index-master (`key-material-hierarchy.md` rule #7). The caller (the Go drain)
/// zeroizes its `key` copy on grant revoke; this fn's internal copies zeroize on
/// drop.
///
/// # Errors
///
/// `FfiError::General` carrying:
///   - `"decode mail-record envelope: ..."` — malformed envelope bytes;
///   - `"content key must be 32 or 2432 bytes ..."` — a key of neither shape;
///   - the wrapped open failure (wrong key, tampered ciphertext, or a hybrid
///     record given only the 32-byte classical half).
#[uniffi::export]
pub fn open_mail_record_with_key(
    envelope_bytes: Vec<u8>,
    key: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let envelope = MailRecordEnvelope::from_canonical_bytes(&envelope_bytes).map_err(|e| {
        FfiError::General {
            msg: format!("decode mail-record envelope: {e}"),
        }
    })?;
    // The 32-vs-2432 length dispatch is the shared `content.read{mail}` open
    // (`fauna_mls::wrapped_blob::unseal_mail_record_with_derived_key`) — the
    // same primitive the epoch trial chain uses (design § 4), so a grant key
    // and an epoch-derived key open by identical rules.
    fauna_mls::wrapped_blob::unseal_mail_record_with_derived_key(&envelope, &key).map_err(|e| {
        FfiError::General {
            msg: format!("open mail-record with grant key: {e}"),
        }
    })
}

/// Report whether `bytes` are a sealed `MailRecordEnvelope` (strict
/// canonical DAG-CBOR, `kind == "mail-record"`, supported version) — as
/// opposed to raw plaintext (RFC 5322 / iCalendar / canonical index-hint
/// token bytes).
///
/// Every mail-plane record rests sealed, so no serve path branches on this;
/// bridge tests use it to prove a payload they handed the nest is sealed.
#[uniffi::export]
pub fn is_sealed_mail_record(bytes: Vec<u8>) -> bool {
    fauna_mls::wrapped_blob::is_sealed_mail_record(&bytes)
}

/// The wall-clock mail content-sealing epoch index for `unix_secs` —
/// `floor(unix_secs / MAIL_SEALING_EPOCH_SECS)` (content-sealing-epochs design
/// § 1, `mail_sealing_epoch_of`). A pure function of the timestamp, no state.
///
/// FFI-exported so the Go MDA/drain can classify which epoch a mail record
/// was sealed under from its stored ingest timestamp (`FetchedCiphertext.
/// InternalDate`) without duplicating `MAIL_SEALING_EPOCH_SECS` — the
/// candidate-chain opener selection (design § 4; `GrantSet.KeyForMailEpoch`)
/// needs this to turn a record timestamp into the epoch to try first.
#[uniffi::export]
pub fn mail_sealing_epoch_of(unix_secs: u64) -> u64 {
    fauna_mls::wrapped_blob::mail_sealing_epoch_of(unix_secs)
}

/// Per-connection mail-record opener: parses the AUTH'd session's
/// `MlsSnapshotPlaintext` ONCE at construction and holds the leaf
/// keypairs `Zeroizing` Rust-side, so each per-record [`open`] pays one
/// envelope decode + HPKE open — not the per-open Go→Rust snapshot
/// marshal + re-parse `MlsCapability::open_mail_record` pays (the F2
/// finding: ~18 ms/FETCH at 1k messages).
///
/// Construct at AUTH (where the session already unwraps + caches the
/// snapshot), call [`open`](Self::open) per record, and [`zeroize`]
/// (Self::zeroize) on session close alongside the capability. A UniFFI
/// `Object` (not `Record`) for the same reason `MlsCapability` is: the
/// secrets need Drop-time zeroization and must never cross the FFI
/// boundary.
#[derive(uniffi::Object)]
pub struct MailRecordOpener {
    /// The session's standing key set, parsed out of the snapshot's
    /// `leaf_init_keypairs` — the same [`StandingMailKeypair`] set the owner's
    /// clients derive from their MSEK history, trialed by the same shared
    /// `open_mail_record_standing`.
    keypairs: Mutex<Option<Vec<StandingMailKeypair>>>,
    /// The session's mail-epoch roots — the CURRENT generation's root
    /// (derived from the capability's MSEK at construction) first, then one
    /// grace root per prior MSEK generation from the snapshot's
    /// `mail_epoch_grace_roots` (design § 5 rotation grace). Present only
    /// for an opener built via
    /// [`MlsCapability::new_epoch_aware_mail_record_opener`] (the IMAP
    /// mail-new-ingest session) — `None` for the plain [`Self::new`]
    /// construction path (CalDAV/CardDAV metadata sessions, tests), which
    /// never serves epoch-sealed content (epoch
    /// semantics apply only to genuine mail records).
    /// Drives [`Self::open_mail`]'s epoch trials; [`Self::open`] never
    /// reads it. The MSEK itself is NOT retained — the scoped roots are all
    /// the epoch chain needs (least-privilege,
    /// `MAIL_EPOCH_ROOT_DERIVE_CONTEXT`). Never crosses the FFI boundary;
    /// zeroized alongside `keypairs`.
    epoch_roots: Mutex<Option<Vec<Zeroizing<[u8; 32]>>>>,
    /// Each prior generation's retirement instant from the snapshot's
    /// `generation_retired_at_unix` (newest first, aligned with
    /// `keypairs[1..]` and the grace roots) — what [`Self::open_mail`] selects
    /// a record's generation by, trialing the one current at its seal basis
    /// first (`owner-key-material.md` § Path B-sibling-2 → *Pre-rotation mail
    /// at rest*). Public timing metadata, not key material; empty (an older
    /// snapshot) walks the whole ring.
    generation_retired_at_unix: Vec<u64>,
}

impl std::fmt::Debug for MailRecordOpener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MailRecordOpener")
            .field(
                "zeroized",
                &self.keypairs.lock().map_or(true, |g| g.is_none()),
            )
            .finish()
    }
}

/// Parse `snapshot_plaintext_bytes` into the opener's per-record leaf
/// keypairs — the shared body of [`MailRecordOpener::new`] and
/// [`MailRecordOpener::new_with_msek`].
fn parse_leaf_keypairs(
    snapshot_plaintext_bytes: &[u8],
) -> Result<(Vec<StandingMailKeypair>, Vec<u64>), FfiError> {
    let snapshot =
        MlsSnapshotPlaintext::from_canonical_bytes(snapshot_plaintext_bytes).map_err(|e| {
            FfiError::General {
                msg: format!("decode mls-snapshot plaintext: {e}"),
            }
        })?;
    if snapshot.leaf_init_keypairs.is_empty() {
        return Err(FfiError::General {
            msg: "no leaf keypairs in snapshot".into(),
        });
    }
    let mut keypairs = Vec::with_capacity(snapshot.leaf_init_keypairs.len());
    for kp in &snapshot.leaf_init_keypairs {
        // from_canonical_bytes already enforced the 32-byte length,
        // but copy defensively rather than panic on a slice.
        if kp.x25519_secret.len() != 32 {
            continue;
        }
        let mut secret = Zeroizing::new([0u8; 32]);
        secret.copy_from_slice(&kp.x25519_secret);
        let entry = match &kp.mlkem_dk {
            Some(mdk) if mdk.len() == MLKEM768_DECAPS_KEY_LEN => {
                let mut dk = Zeroizing::new([0u8; MLKEM768_DECAPS_KEY_LEN]);
                dk.copy_from_slice(mdk);
                StandingMailKeypair::new_hybrid(&secret, &dk)
            }
            _ => StandingMailKeypair::new_classical(&secret),
        };
        keypairs.push(entry);
    }
    if keypairs.is_empty() {
        return Err(FfiError::General {
            msg: "no usable leaf keypairs in snapshot".into(),
        });
    }
    // A skipped (malformed) entry would shift the alignment: carry the
    // instants only when every entry parsed.
    let instants = if keypairs.len() == snapshot.leaf_init_keypairs.len() {
        snapshot.generation_retired_at_unix.clone()
    } else {
        Vec::new()
    };
    Ok((keypairs, instants))
}

impl MailRecordOpener {
    /// Build the opener with epoch-derivation material (content-sealing-
    /// epochs design § 4): the current generation's mail-epoch root is
    /// derived from `msek` here (the MSEK itself is then dropped — the
    /// opener never retains it), and the snapshot's `mail_epoch_grace_roots`
    /// supply prior generations' roots for MSEK-rotation grace (§ 5). Not
    /// UniFFI-exported directly — the only construction path is
    /// [`MlsCapability::new_epoch_aware_mail_record_opener`], which is the
    /// only place an actual mail-new-ingest opener (the IMAP session at
    /// AUTH) gets built; CalDAV/CardDAV sessions keep using [`Self::new`]
    /// (no epoch material, epoch trial never attempted).
    fn new_with_msek(
        snapshot_plaintext_bytes: &[u8],
        msek: Zeroizing<[u8; 32]>,
    ) -> Result<std::sync::Arc<Self>, FfiError> {
        let (keypairs, generation_retired_at_unix) = parse_leaf_keypairs(snapshot_plaintext_bytes)?;
        // Re-parse for the grace roots; parse_leaf_keypairs already proved
        // the bytes decode. Current generation's root first, then the
        // snapshot's prior-generation grace roots in their carried order
        // (newest first, aligned with leaf_init_keypairs[1..]).
        let snapshot = MlsSnapshotPlaintext::from_canonical_bytes(snapshot_plaintext_bytes)
            .map_err(|e| FfiError::General {
                msg: format!("decode mls-snapshot plaintext: {e}"),
            })?;
        let mut epoch_roots = Vec::with_capacity(1 + snapshot.mail_epoch_grace_roots.len());
        epoch_roots.push(fauna_mls::wrapped_blob::derive_mail_epoch_root(&msek));
        for root in &snapshot.mail_epoch_grace_roots {
            // from_canonical_bytes enforced the 32-byte length.
            if root.len() != 32 {
                continue;
            }
            let mut r = Zeroizing::new([0u8; 32]);
            r.copy_from_slice(root);
            epoch_roots.push(r);
        }
        Ok(std::sync::Arc::new(Self {
            keypairs: Mutex::new(Some(keypairs)),
            epoch_roots: Mutex::new(Some(epoch_roots)),
            generation_retired_at_unix,
        }))
    }
}

#[uniffi::export]
impl MailRecordOpener {
    /// Parse `snapshot_plaintext_bytes` (canonical DAG-CBOR
    /// `MlsSnapshotPlaintext`, already AEAD-unwrapped under MSEK by a
    /// prior `MlsCapability::decrypt` at AUTH) and hold its leaf init
    /// keypairs for per-record opens.
    ///
    /// # Errors
    ///
    /// `FfiError::General` if the bytes are not a valid
    /// `MlsSnapshotPlaintext`, or the snapshot carries no leaf keypairs.
    #[uniffi::constructor]
    pub fn new(snapshot_plaintext_bytes: Vec<u8>) -> Result<std::sync::Arc<Self>, FfiError> {
        let (keypairs, generation_retired_at_unix) =
            parse_leaf_keypairs(&snapshot_plaintext_bytes)?;
        Ok(std::sync::Arc::new(Self {
            keypairs: Mutex::new(Some(keypairs)),
            epoch_roots: Mutex::new(None),
            generation_retired_at_unix,
        }))
    }

    /// Open a sealed `MailRecordEnvelope` (either suite — classical
    /// X25519, or hybrid X-Wing when the snapshot entry carries the
    /// MSEK-derived ML-KEM decapsulation key). Each leaf keypair is
    /// tried newest first (current, then every prior generation — no seal
    /// basis is known here), exactly the `MlsCapability::open_mail_record`
    /// semantics.
    ///
    /// # Errors
    ///
    /// `FfiError::General` carrying:
    /// - `"mail-record opener already zeroized"` after [`zeroize`](Self::zeroize);
    /// - `"decode mail-record envelope: ..."` for a non-envelope input
    ///   (an unsealed payload — refused, never passed through);
    /// - `"HPKE open failed: no matching leaf keypair in snapshot"` when
    ///   every keypair fails (wrong recipient or tampered envelope).
    pub fn open(&self, envelope_bytes: Vec<u8>) -> Result<Vec<u8>, FfiError> {
        let guard = self.keypairs.lock().map_err(|e| FfiError::General {
            msg: format!("mail-record opener lock poisoned: {e}"),
        })?;
        let keypairs = guard.as_ref().ok_or_else(|| FfiError::General {
            msg: "mail-record opener already zeroized".into(),
        })?;
        let envelope = MailRecordEnvelope::from_canonical_bytes(&envelope_bytes).map_err(|e| {
            FfiError::General {
                msg: format!("decode mail-record envelope: {e}"),
            }
        })?;
        open_mail_record_standing(&envelope, keypairs, &[], None).ok_or_else(|| FfiError::General {
            msg: "HPKE open failed: no matching leaf keypair in snapshot".into(),
        })
    }

    /// Open a sealed `MailRecordEnvelope` known to be a genuine mail-new-
    /// ingest record — the MSEK-holder opener chain (content-sealing-epochs
    /// design § 4). `record_unix_secs` is the record's own seal instant
    /// (the Go MDA's `FetchedCiphertext.SealEpochBasisUnix()` — the nest's
    /// `stored_at`; `0` when unknown, a standing-sealed record),
    /// used to compute its candidate sealing epoch AND to select its MSEK
    /// generation: every per-generation step below runs the generation
    /// current at that instant first, then outward, by the snapshot's
    /// `generation_retired_at_unix` (`generation_trial_order`). Trial order: the
    /// record's target epoch key then the immediately-prior epoch key
    /// (boundary/clock-skew tolerance) — for the CURRENT generation's root
    /// first, then each MSEK-rotation grace root from the snapshot's
    /// `mail_epoch_grace_roots` (design § 5: a record epoch-sealed under a
    /// rotated-away root re-derives from that generation's carried root);
    /// then the standing/grace chain ([`Self::open`]'s exact trial —
    /// standing-key + degraded-schedule content); then — only if all
    /// of the above failed — a bounded back-scan of the earlier epochs a
    /// stale-published schedule could have sealed under
    /// (`MAIL_EPOCH_PUBLISH_HORIZON` weeks, design § 3 step 2), again per
    /// root. Each miss is one cheap AEAD failure. An opener with no epoch
    /// material (the [`Self::new`] construction path) skips the epoch
    /// trials entirely — byte-identical to [`Self::open`].
    ///
    /// # Errors
    ///
    /// Same shape as [`Self::open`] — every trial's AEAD failure folds into
    /// one "no matching key" outcome; corruption vs. genuinely out-of-
    /// window content are indistinguishable at this layer, exactly like the
    /// standing chain.
    pub fn open_mail(
        &self,
        envelope_bytes: Vec<u8>,
        record_unix_secs: u64,
    ) -> Result<Vec<u8>, FfiError> {
        let keypairs_guard = self.keypairs.lock().map_err(|e| FfiError::General {
            msg: format!("mail-record opener lock poisoned: {e}"),
        })?;
        let keypairs = keypairs_guard.as_ref().ok_or_else(|| FfiError::General {
            msg: "mail-record opener already zeroized".into(),
        })?;
        let roots_guard = self.epoch_roots.lock().map_err(|e| FfiError::General {
            msg: format!("mail-record opener lock poisoned: {e}"),
        })?;
        // Vec of borrows (no key copy — just pointers into the held Zeroizing
        // roots) so the shared chain can iterate them twice (near pair +
        // back-scan). Empty for a non-epoch opener (Self::new path) → the chain
        // runs only the standing closure.
        let epoch_roots: Vec<&[u8; 32]> = roots_guard
            .as_ref()
            .map_or_else(Vec::new, |roots| roots.iter().map(|z| &**z).collect());

        let envelope = MailRecordEnvelope::from_canonical_bytes(&envelope_bytes).map_err(|e| {
            FfiError::General {
                msg: format!("decode mail-record envelope: {e}"),
            }
        })?;

        // The § 4 trial order lives in the shared chain
        // (`fauna_mls::wrapped_blob::open_mail_epoch_chain`), which every
        // MSEK-holder open path uses (this opener + the client receive path).
        // The standing closure is this opener's own leaf-keypair trial —
        // standing-key + degraded-schedule content, exactly Self::open's chain,
        // and the client receive path's (`open_inbound_record_with_keys`).
        let retired = &self.generation_retired_at_unix;
        let basis = (record_unix_secs != 0).then_some(record_unix_secs);
        let standing =
            |env: &MailRecordEnvelope| open_mail_record_standing(env, keypairs, retired, basis);

        fauna_mls::wrapped_blob::open_mail_epoch_chain(
            &envelope,
            &epoch_roots,
            retired,
            record_unix_secs,
            standing,
        )
        .ok_or_else(|| FfiError::General {
            msg: "HPKE open failed: no matching leaf keypair or epoch key in snapshot".into(),
        })
    }

    /// Drop the held keypairs now, zeroizing the key memory. After
    /// `zeroize()`, `open`/`open_mail` return an error. Idempotent.
    pub fn zeroize(&self) {
        if let Ok(mut guard) = self.keypairs.lock() {
            guard.take(); // Drop runs on the Zeroizing wrappers → zeroes bytes
        }
        if let Ok(mut guard) = self.epoch_roots.lock() {
            guard.take();
        }
    }
}

/// Validate and copy a 32-byte X25519 secret. The returned array is
/// wrapped in `Zeroizing` so the secret zeroes on drop at the end of
/// the unwrap call.
fn secret_32(bytes: &[u8]) -> Result<zeroize::Zeroizing<[u8; 32]>, FfiError> {
    if bytes.len() != 32 {
        return Err(FfiError::General {
            msg: format!(
                "recipient_x25519_secret must be 32 bytes, got {}",
                bytes.len()
            ),
        });
    }
    let mut out = zeroize::Zeroizing::new([0u8; 32]);
    out.copy_from_slice(bytes);
    Ok(out)
}

// ── Wrapped-MSEK AEAD-unwrap (AEAD-unwrap-as-auth) ──
//
// Phase C.3 of the mail-bridge MDA arm. The bridge holds an mlock'd
// MSEK after a successful PLAIN / OAUTHBEARER AUTH; the wrapped-MSEK
// blob fetched from nest is canonical-CBOR `WrappedMsekBlob`. The FFI
// takes the encoded bytes plus the MUA-supplied credential and returns
// an `MlsCapability` (UniFFI `Object`, so it owns mlock'd memory and
// runs `Drop` to zeroize). See `docs/goal/behavior/imap-server.md`
// § Authentication and the wrapped-blob design spec.
//
// `MlsCapability` MUST be a UniFFI `Object` (not `Record`). Records
// are POD-style and don't run `Drop`; the MSEK needs Drop-time
// zeroization. The Drop chain is:
//   MlsCapability → Mutex<Option<MlsCapabilityInner>> →
//   MlsCapabilityInner → Zeroizing<[u8; 32]> → zeroize on drop.

/// KDF kind for the credential-derived AEAD key.
///
/// `Argon2id` is the spec-mandated KDF for PLAIN-style credentials
/// (low-entropy passwords); `Hkdf` is for OAUTHBEARER-style tokens
/// (high-entropy bearer values). The KDF parameters themselves are
/// carried inside the blob's `kdf` descriptor — this enum only
/// disambiguates which CredentialInput arm to use.
///
/// Design tracked internally.
#[derive(uniffi::Enum, Debug, Clone, Copy)]
pub enum KdfKind {
    Argon2id,
    Hkdf,
}

/// Mlock'd MLS-decryption capability obtained from a successful
/// wrapped-MSEK AEAD-unwrap. The inner 32-byte MSEK is `Zeroizing`,
/// so dropping the capability (Arc-counted by UniFFI) zeroes the
/// secret. Call `zeroize()` explicitly from the Go bridge's
/// `Session.Close` to make the intent clear in code review and to
/// reclaim the secret eagerly (without waiting for the finalizer).
///
/// Per `docs/goal/behavior/imap-server.md` § Authentication: on
/// LOGOUT / idle timeout / session disconnect, the MDA zeroizes the
/// unwrapped capability and reports `report_session_close`.
#[derive(uniffi::Object)]
pub struct MlsCapability {
    inner: Mutex<Option<MlsCapabilityInner>>,
}

impl std::fmt::Debug for MlsCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the inner MSEK — the bridge audit-log path
        // already redacts secrets, and tests' `expect_err` only
        // formats this when an UNEXPECTED Ok shows up.
        f.debug_struct("MlsCapability")
            .field("zeroized", &self.inner.lock().map_or(true, |g| g.is_none()))
            .finish()
    }
}

struct MlsCapabilityInner {
    msek: Zeroizing<[u8; 32]>,
    /// Bound to the AAD of every blob we decrypt under this
    /// capability — currently the `MlsSnapshotBlob` shape (which
    /// indexes by actor_id alone). Stored here so the bridge
    /// doesn't have to thread the actor_id through every decrypt
    /// call site.
    actor_id: [u8; 32],
}

#[uniffi::export]
impl MlsCapability {
    /// Decrypt an `MlsSnapshotBlob` sealed under this capability's
    /// MSEK. Returns the snapshot plaintext (the serialized
    /// read-side MLS state; design tracked internally, § Snapshot
    /// plaintext contents).
    ///
    /// Phase C.6 will wire the bridge's body-decryption path through
    /// this method (after extending the snapshot-state load); for
    /// now it's the one decrypt-shape MSEK directly seals.
    ///
    /// # Errors
    ///
    /// Returns `FfiError::General` if the capability has been
    /// zeroized, the input is not a valid `MlsSnapshotBlob`, the
    /// AAD does not match (e.g. actor_id substitution), or AEAD
    /// verify fails.
    pub fn decrypt(&self, ciphertext: Vec<u8>) -> Result<Vec<u8>, FfiError> {
        let guard = self.inner.lock().map_err(|e| FfiError::General {
            msg: format!("mls-capability lock poisoned: {e}"),
        })?;
        let inner = guard.as_ref().ok_or_else(|| FfiError::General {
            msg: "mls-capability already zeroized".into(),
        })?;
        let blob =
            MlsSnapshotBlob::from_canonical_bytes(&ciphertext).map_err(|e| FfiError::General {
                msg: format!("decode mls-snapshot: {e}"),
            })?;
        if blob.index.0.as_slice() != inner.actor_id.as_slice() {
            return Err(FfiError::General {
                msg: "mls-snapshot actor_id does not match capability".into(),
            });
        }
        let plaintext = unseal_mls_snapshot(&blob, &inner.msek).map_err(|e| FfiError::General {
            msg: format!("decrypt mls-snapshot: {e}"),
        })?;
        // `Zeroizing<Vec<u8>>` plaintext: copy the bytes out so the
        // Zeroizing wrapper zeros its copy on drop at function exit.
        // The caller's Vec<u8> is its own allocation.
        Ok((*plaintext).clone())
    }

    /// Unseal a `WebdavKeysBlob` under this capability's MSEK and return the
    /// decoded served-set content keys (`webdav-server.md` § Key model). The Go
    /// WebDAV MDA calls this once per AUTH'd session; the MSEK never crosses the
    /// FFI boundary (mirrors [`Self::decrypt`] — the capability holds it). The
    /// returned keys live in the MDA's zeroized session memory for the session.
    ///
    /// # Errors
    ///
    /// `FfiError::General` if the capability is zeroized, the input is not a
    /// valid `WebdavKeysBlob`, the AAD/MSEK mismatch, or the plaintext is not a
    /// supported `WebdavKeysPlaintext` version.
    pub fn unseal_webdav_keys_blob(
        &self,
        blob_bytes: Vec<u8>,
    ) -> Result<FfiWebdavKeysPlaintext, FfiError> {
        let guard = self.inner.lock().map_err(|e| FfiError::General {
            msg: format!("mls-capability lock poisoned: {e}"),
        })?;
        let inner = guard.as_ref().ok_or_else(|| FfiError::General {
            msg: "mls-capability already zeroized".into(),
        })?;
        let blob =
            WebdavKeysBlob::from_canonical_bytes(&blob_bytes).map_err(|e| FfiError::General {
                msg: format!("decode webdav-keys: {e}"),
            })?;
        let plaintext =
            inner_unseal_webdav_keys_blob(&blob, &inner.msek).map_err(|e| FfiError::General {
                msg: format!("unseal webdav-keys: {e}"),
            })?;
        let pt = WebdavKeysPlaintext::from_canonical_bytes(&plaintext).map_err(|e| {
            FfiError::General {
                msg: format!("decode webdav-keys plaintext: {e}"),
            }
        })?;
        Ok(FfiWebdavKeysPlaintext::from(&pt))
    }

    /// Construct the per-connection mail-record opener from this
    /// capability's unwrapped MSEK plus a decrypted `MlsSnapshotPlaintext`
    /// — the MSEK-holder opener chain (content-sealing-epochs design § 4).
    /// Unlike [`MailRecordOpener::new`] (the CalDAV/CardDAV construction
    /// path, whose content is never epoch-sealed),
    /// the returned opener's [`MailRecordOpener::open_mail`]
    /// additionally tries the record's candidate epoch keys — derived on
    /// demand from the current generation's mail-epoch root (computed here
    /// from this capability's MSEK) and from each MSEK-rotation grace root
    /// the snapshot carries (`mail_epoch_grace_roots`, design § 5) — before
    /// falling through to the standing/grace chain. The MSEK itself never
    /// crosses the FFI boundary, and the opener retains only the scoped
    /// epoch roots, not the MSEK.
    ///
    /// `snapshot_plaintext_bytes` must be the plaintext from a PRIOR
    /// `self.decrypt(snapshot_blob_bytes)` call on this same capability
    /// (not re-derived here — this method trusts the caller already
    /// AEAD-verified the snapshot against this capability's actor_id).
    ///
    /// # Errors
    ///
    /// `FfiError::General` if this capability is zeroized, or
    /// `snapshot_plaintext_bytes` is not a valid `MlsSnapshotPlaintext` with
    /// at least one usable leaf keypair (same failure shape as
    /// [`MailRecordOpener::new`]).
    pub fn new_epoch_aware_mail_record_opener(
        &self,
        snapshot_plaintext_bytes: Vec<u8>,
    ) -> Result<std::sync::Arc<MailRecordOpener>, FfiError> {
        let guard = self.inner.lock().map_err(|e| FfiError::General {
            msg: format!("mls-capability lock poisoned: {e}"),
        })?;
        let inner = guard.as_ref().ok_or_else(|| FfiError::General {
            msg: "mls-capability already zeroized".into(),
        })?;
        let mut msek = Zeroizing::new([0u8; 32]);
        msek.copy_from_slice(inner.msek.as_slice());
        MailRecordOpener::new_with_msek(&snapshot_plaintext_bytes, msek)
    }

    /// Drop the inner MSEK now, zeroizing the key memory. After
    /// `zeroize()`, `decrypt` returns an error. Idempotent — a
    /// second call is a no-op.
    pub fn zeroize(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.take(); // Drop runs on the Zeroizing wrapper → zeroes bytes
        }
    }
}

impl MlsCapability {
    /// A zeroizing copy of the unwrapped MSEK, for **in-crate** derivations
    /// only.
    ///
    /// `pub(crate)`, never exported: the MSEK does not cross the FFI boundary
    /// (that is the whole reason this type hands out opaque handles). Every
    /// exported method that needs it derives a scoped key from this and retains
    /// only the derived material — see `new_epoch_aware_mail_record_opener`
    /// and `resume_mail_index_session`.
    pub(crate) fn msek_copy(&self) -> Result<Zeroizing<[u8; 32]>, FfiError> {
        let guard = self.inner.lock().map_err(|e| FfiError::General {
            msg: format!("mls-capability lock poisoned: {e}"),
        })?;
        let inner = guard.as_ref().ok_or_else(|| FfiError::General {
            msg: "mls-capability already zeroized".into(),
        })?;
        let mut msek = Zeroizing::new([0u8; 32]);
        msek.copy_from_slice(inner.msek.as_slice());
        Ok(msek)
    }
}

/// AEAD-unwrap a per-credential wrapped-MSEK blob, returning the
/// MLS-decryption capability. This is the bridge's
/// AUTH-PLAIN / AUTH-OAUTHBEARER primitive: AEAD-success is the
/// authentication-success signal; AEAD-fail is the
/// authentication-failure signal.
///
/// - `blob_bytes`: canonical DAG-CBOR `WrappedMsekBlob` (as fetched
///   from `fauna.bridges.fetch_wrapped_mls_blob`).
/// - `credential`: the MUA-supplied secret (UTF-8 password for
///   `Argon2id`; high-entropy bearer token bytes for `Hkdf`).
/// - `actor_id`: the bridge-resolved 32-byte actor_id (from
///   `fauna.bridges.validate_recipient` → hex → bytes).
/// - `credential_id`: the credential identifier the blob was sealed
///   under (carried in the blob's `ix` field; supplied here so the
///   AAD reconstructs deterministically).
/// - `kind`: which KDF arm to dispatch (`Argon2id` for PLAIN,
///   `Hkdf` for OAUTHBEARER).
///
/// `AadBinding::for_wrapped_msek(actor_id, credential_id)` is
/// constructed internally so callers can't accidentally diverge it
/// from the seal-side AAD (which would fail AEAD verify).
///
/// # Errors
///
/// - AEAD-fail (wrong credential, tampered blob, actor_id/credential_id
///   substitution) → `FfiError::General` carrying "AEAD …" wording.
///   The bridge maps this to IMAP `NO Authentication failed` +
///   `report_auth_event(result=fail)`.
/// - Format-level errors (bad CBOR, unknown blob version, wrong
///   `kind` discriminator, wrong field length) → `FfiError::General`
///   carrying "decode …" / "invalid format …" wording. Bridge
///   treats these the same as AEAD-fail for the IMAP wire, but
///   nest's audit-log mapping can distinguish them by the message
///   string.
/// - KDF parameter out-of-range (adversary-controlled blob
///   delivering pathological Argon2 params) → `FfiError::General`
///   carrying "KDF …" wording.
#[uniffi::export]
pub fn unwrap_msek_blob(
    blob_bytes: Vec<u8>,
    credential: Vec<u8>,
    actor_id: Vec<u8>,
    credential_id: String,
    kind: KdfKind,
) -> Result<std::sync::Arc<MlsCapability>, FfiError> {
    let actor_arr: [u8; 32] = actor_id
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::General {
            msg: format!("actor_id must be 32 bytes, got {}", actor_id.len()),
        })?;

    let blob =
        WrappedMsekBlob::from_canonical_bytes(&blob_bytes).map_err(|e| FfiError::General {
            msg: format!("decode wrapped-msek blob: {e}"),
        })?;

    // Coarse cross-check: AUTH=PLAIN MUST resolve to an Argon2id-
    // sealed blob; AUTH=OAUTHBEARER MUST resolve to an HKDF-sealed
    // one. Mixing the two is a format-level error, not AEAD-fail —
    // PLAIN-creds + HKDF-blob (or vice versa) means the dispatch is
    // wrong (nest handed back the wrong shape, or the bridge picked
    // the wrong arm), not an attacker probing credentials.
    match (&kind, &blob.kdf) {
        (KdfKind::Argon2id, SerKdfParams::Argon2id { .. })
        | (KdfKind::Hkdf, SerKdfParams::HkdfSha256 { .. }) => {}
        _ => {
            return Err(FfiError::General {
                msg: "KDF kind does not match wrapped-MSEK blob".into(),
            });
        }
    }

    let cred_input = match kind {
        KdfKind::Argon2id => CredentialInput::Plain(&credential),
        KdfKind::Hkdf => CredentialInput::OauthBearer(&credential),
    };

    // Build the AAD from the CALLER's (actor_id, credential_id),
    // NOT from the blob's `ix` field. If an attacker swaps in a
    // different actor's blob (matching CBOR shape, valid AEAD),
    // the AAD reconstructed here from the bridge's resolved
    // actor_id will differ from the seal-time AAD and AEAD verify
    // will fail. This is "AEAD-unwrap-as-auth" with
    // substitution-resistance baked in at the bridge layer per
    // imap-server.md § Authentication.
    let kdf_params = KdfParams::try_from(&blob.kdf).map_err(|e| FfiError::General {
        msg: format!("invalid blob kdf params: {e}"),
    })?;
    let key = unwrap_key(
        &cred_input,
        blob.salt.as_ref(),
        &actor_arr,
        &credential_id,
        kdf_params,
    )
    .map_err(|e| FfiError::General {
        msg: format!("KDF derive failed: {e}"),
    })?;

    let aad = AadBinding::for_wrapped_msek(&actor_arr, &credential_id);

    if blob.nonce.len() != AEAD_NONCE_LEN {
        return Err(FfiError::General {
            msg: format!(
                "wrapped-msek nonce must be {AEAD_NONCE_LEN} bytes, got {}",
                blob.nonce.len()
            ),
        });
    }
    let mut nonce = [0u8; AEAD_NONCE_LEN];
    nonce.copy_from_slice(blob.nonce.as_ref());

    let plaintext = Zeroizing::new(
        aead_open(&key, &nonce, &aad, blob.ciphertext.as_ref()).map_err(|_| FfiError::General {
            msg: "AEAD verify failed".into(),
        })?,
    );
    if plaintext.len() != 32 {
        return Err(FfiError::General {
            msg: format!("MSEK plaintext must be 32 bytes, got {}", plaintext.len()),
        });
    }
    let mut msek = Zeroizing::new([0u8; 32]);
    msek.copy_from_slice(&plaintext);

    Ok(std::sync::Arc::new(MlsCapability {
        inner: Mutex::new(Some(MlsCapabilityInner {
            msek,
            actor_id: actor_arr,
        })),
    }))
}

// ── Wrapped-submission-token AEAD-unseal (Phase D.2 / MTA submission) ──
//
// Submission auth is AEAD-unwrap-as-auth on a *separate* blob shape
// from the IMAP MSEK blob: `WrappedSubmissionTokenBlob` (kind="submission-
// token") carries a signed `SubmissionToken` plaintext rather than raw
// key material. The MTA never decrypts mail — by design the submission
// blob carries no MLS-decryption capability (design tracked internally,
// § Outbound submission).
//
// Compared to `unwrap_msek_blob`, this surface adds an inner Ed25519
// signature verify against `actor_id`-as-VerifyingKey. The codebase
// invariant (`actor_id == 32-byte Ed25519 verifying key`, cf.
// `bins/fauna-nest/src/registration.rs:184` and
// `bins/fauna-nest/src/bridge_blob_handlers.rs:565`) lets the FFI
// reconstruct the user's primary signing pubkey from `actor_id`
// without a separate nest fetch. The verify defends against a
// coerced/compromised nest fabricating submission tokens.

/// Plaintext side of an AEAD-unwrapped `WrappedSubmissionTokenBlob`.
/// Mirrors `fauna_mls::wrapped_blob::SubmissionToken`'s public policy
/// fields; `user_sig` is verified inside the FFI and dropped from the
/// returned shape (the consumer authenticates the user via the FFI's
/// Ok/Err outcome, and uses the policy fields for D.3/D.7's quota +
/// expiry enforcement).
#[derive(uniffi::Record, Clone, Debug)]
pub struct SubmissionTokenFfi {
    pub actor_id: Vec<u8>,
    pub credential_id: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub max_recipients: u32,
    pub max_messages_per_day: u32,
}

/// AEAD-unwrap a per-credential `WrappedSubmissionTokenBlob`, returning
/// the plaintext policy fields on success.
///
/// This is the SMTP submission AUTH primitive: AEAD-success +
/// inner-signature-success is the authentication-success signal;
/// any returned error is the authentication-failure signal.
///
/// - `blob_bytes`: canonical DAG-CBOR `WrappedSubmissionTokenBlob`
///   (as fetched from `fauna.bridges.fetch_wrapped_submission_token`).
/// - `credential`: the MUA-supplied secret (UTF-8 password for
///   `Argon2id`; high-entropy bearer token bytes for `Hkdf`).
/// - `actor_id`: the bridge-resolved 32-byte actor_id (from
///   `fauna.bridges.validate_recipient`). MUST be the user's Ed25519
///   verifying-key bytes — used both as the AAD binding parameter and
///   as the public key for the inner SubmissionToken signature verify.
/// - `credential_id`: the credential identifier the blob was sealed
///   under. Cross-checked against `blob.index` and bound into the AAD.
/// - `kind`: which KDF arm to dispatch (`Argon2id` for PLAIN,
///   `Hkdf` for OAUTHBEARER).
///
/// Substitution defenses (parallel to `unwrap_msek_blob`):
///   1. `blob.index` ≠ `(actor_id, credential_id)` → pre-AEAD reject.
///   2. AAD reconstructed from `(actor_id, credential_id)` ≠ seal-time
///      AAD → AEAD verify fails.
///   3. Inner SubmissionToken signed by a key other than the actor's
///      primary Ed25519 key → signature verify fails.
///
/// # Errors
///
/// - AEAD-fail → "AEAD verify failed".
/// - Inner signature verify fail → "signature verify failed".
/// - Format-level errors (bad CBOR, wrong `kind` discriminator, wrong
///   field lengths, KDF parameters out of range, blob.index mismatch)
///   → wording carrying "decode", "invalid format", "kdf", "actor", or
///   "credential".
#[uniffi::export]
pub fn unseal_submission_token_blob(
    blob_bytes: Vec<u8>,
    credential: Vec<u8>,
    actor_id: Vec<u8>,
    credential_id: String,
    kind: KdfKind,
) -> Result<SubmissionTokenFfi, FfiError> {
    let actor_arr = bytes32(&actor_id, "actor_id")?;

    let blob = WrappedSubmissionTokenBlob::from_canonical_bytes(&blob_bytes).map_err(|e| {
        FfiError::General {
            msg: format!("decode submission-token blob: {e}"),
        }
    })?;

    // Pre-AEAD substitution defense: blob.index must equal the caller's
    // expected (actor_id, credential_id). Without this check, an
    // attacker who knows a victim's credential could substitute a
    // structurally-valid blob for a different actor and rely on AEAD
    // failing — but that wastes a costly KDF derivation; reject cheaply
    // up front, and let the AAD-bound AEAD verify still be the
    // load-bearing check if the index field is tampered.
    if blob.index.0.as_slice() != actor_arr.as_slice() {
        return Err(FfiError::General {
            msg: "submission-token blob actor_id does not match expected".into(),
        });
    }
    if blob.index.1 != credential_id {
        return Err(FfiError::General {
            msg: "submission-token blob credential_id does not match expected".into(),
        });
    }

    // KDF arm cross-check, mirroring `unwrap_msek_blob`. PLAIN-creds
    // + HKDF-blob (or vice versa) means the dispatch is wrong (nest
    // handed back the wrong shape, or the bridge picked the wrong
    // arm), not an attacker probing credentials — surface as a
    // format-level error, not AEAD-fail.
    match (&kind, &blob.kdf) {
        (KdfKind::Argon2id, SerKdfParams::Argon2id { .. })
        | (KdfKind::Hkdf, SerKdfParams::HkdfSha256 { .. }) => {}
        _ => {
            return Err(FfiError::General {
                msg: "KDF kind does not match submission-token blob".into(),
            });
        }
    }

    let cred_input = match kind {
        KdfKind::Argon2id => CredentialInput::Plain(&credential),
        KdfKind::Hkdf => CredentialInput::OauthBearer(&credential),
    };

    // The codebase invariant `actor_id == Ed25519 verifying key` lets
    // us reconstruct the user's signing pubkey directly from actor_id.
    // `VerifyingKey::from_bytes` validates the point is on-curve; any
    // failure here means actor_id was never a real user identity in
    // the first place, surface as format error.
    let verifying_key = VerifyingKey::from_bytes(&actor_arr).map_err(|_| FfiError::General {
        msg: "actor_id is not a valid Ed25519 verifying key".into(),
    })?;

    // Inner unseal does:
    //   1. AAD reconstruction from blob.index (== caller's expected IDs
    //      after the pre-checks above), KDF derive, AEAD-open.
    //   2. Inner SubmissionToken::from_canonical_bytes decode.
    //   3. Ed25519 verify against the supplied verifying key.
    let token = inner_unseal_submission_token(&blob, &cred_input, &verifying_key).map_err(|e| {
        // Map fauna-mls error variants to FFI wording the Go bridge
        // can pattern-match on. The mailfauna layer never logs
        // credential bytes; this string is the audit-log reason.
        let raw = format!("{e}");
        let lc = raw.to_lowercase();
        let msg = if lc.contains("aead") {
            "AEAD verify failed".to_string()
        } else if lc.contains("signature") {
            "signature verify failed".to_string()
        } else if lc.contains("kdf") {
            format!("KDF failed: {raw}")
        } else {
            format!("invalid format: {raw}")
        };
        FfiError::General { msg }
    })?;

    // Defense-in-depth: the inner token's actor_id and credential_id
    // MUST match the caller's expected values (the AAD-bound AEAD plus
    // the pre-check above already enforce this, but a future refactor
    // of those guards shouldn't silently weaken the FFI contract).
    if token.actor_id.as_slice() != actor_arr.as_slice() {
        return Err(FfiError::General {
            msg: "inner submission-token actor_id does not match expected".into(),
        });
    }
    if token.credential_id != credential_id {
        return Err(FfiError::General {
            msg: "inner submission-token credential_id does not match expected".into(),
        });
    }

    Ok(SubmissionTokenFfi {
        actor_id: token.actor_id,
        credential_id: token.credential_id,
        issued_at: token.issued_at,
        expires_at: token.expires_at,
        max_recipients: token.max_recipients,
        max_messages_per_day: token.max_messages_per_day,
    })
}

// ── Wrapped-blob symmetric seal/unseal (I3 Phase A.2) ──
//
// The user-side counterpart to the asymmetric unseal exports above.
// The shared Rust state machine in `libs/fauna-client-mail-settings/`
// calls these to provision a fresh actor's wrapped-MSEK blob,
// MLS-state snapshot, and per-credential submission token through
// nest's `fauna.bridges.provision_*` RPCs. Per-app UI in Phases
// B–F binds to these via UniFFI (Swift / Kotlin / C# / Go) or
// wasm-bindgen (Web), mirroring the unseal pattern: this layer takes
// language-neutral bytes/strings, runs the seal in Rust, and returns
// the canonical DAG-CBOR bytes nest stores opaque.
//
// `KdfParamsFfi` is a flat record because UniFFI does not love
// associated-data enums in every binding. `alg` selects the variant;
// `argon2_*` fields populate only when `alg == "argon2id"`. The
// library default per credential kind is named in
// `docs/goal/behavior/mail-credentials.md` § KDF choice (Argon2id
// Interactive `m=65_536, t=2, p=1` for PLAIN; HKDF-SHA-256 for
// OAUTHBEARER); pass `None` to select that default.

/// KDF parameters for credential-derived AEAD keys.
#[derive(uniffi::Record, Clone, Debug)]
pub struct KdfParamsFfi {
    /// `"argon2id"` or `"hkdf-sha256"`.
    pub alg: String,
    /// Argon2id memory cost in KiB. Required when `alg == "argon2id"`.
    pub argon2_m_kib: Option<u32>,
    /// Argon2id iteration count. Required when `alg == "argon2id"`.
    pub argon2_t: Option<u32>,
    /// Argon2id parallelism. Required when `alg == "argon2id"`.
    pub argon2_p: Option<u32>,
}

/// Seal a `WrappedMsekBlob` for nest's `provision_wrapped_mls_blob`.
///
/// Returns the canonical DAG-CBOR bytes ready to upload. `msek` and
/// `actor_id` must be exactly 32 bytes. `credential_kind` selects
/// the KDF family: `"plain"` → Argon2id, `"oauthbearer"` → HKDF-SHA-256.
/// `kdf_params` overrides the library default (named in
/// `docs/goal/behavior/mail-credentials.md` § KDF choice).
///
/// # Errors
///
/// `FfiError::General` carries:
///   - input-length errors (`msek must be 32 bytes` etc.),
///   - unknown `credential_kind` or `kdf_params.alg`,
///   - missing required Argon2id fields,
///   - underlying `WrapError` from `fauna_mls::wrapped_blob::seal_wrapped_msek`.
#[uniffi::export]
pub fn seal_wrapped_msek_blob(
    msek: Vec<u8>,
    actor_id: Vec<u8>,
    credential_id: String,
    credential_kind: String,
    credential_bytes: Vec<u8>,
    kdf_params: Option<KdfParamsFfi>,
) -> Result<Vec<u8>, FfiError> {
    let msek = bytes32(&msek, "msek")?;
    let actor_id = bytes32(&actor_id, "actor_id")?;
    let credential = credential_input(&credential_kind, &credential_bytes)?;
    let kdf = resolve_kdf_params(kdf_params, &credential_kind)?;
    let blob = inner_seal_wrapped_msek(&msek, &actor_id, &credential_id, &credential, kdf)
        .map_err(|e| FfiError::General {
            msg: format!("seal wrapped-msek: {e}"),
        })?;
    blob.to_canonical_bytes().map_err(|e| FfiError::General {
        msg: format!("encode wrapped-msek: {e}"),
    })
}

/// Unseal a `WrappedMsekBlob`. Returns the recovered 32-byte MSEK.
///
/// Counterpart to `seal_wrapped_msek_blob`. The MDA bridge calls this
/// at MUA-AUTH per `docs/goal/behavior/imap-server.md` § Authentication.
///
/// # Errors
///
/// `FfiError::General` carries the `UnwrapError` discriminant in the
/// message; AEAD failure is the auth-failure signal.
#[uniffi::export]
pub fn unseal_wrapped_msek_blob(
    blob_bytes: Vec<u8>,
    credential_kind: String,
    credential_bytes: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let blob =
        WrappedMsekBlob::from_canonical_bytes(&blob_bytes).map_err(|e| FfiError::General {
            msg: format!("decode wrapped-msek: {e}"),
        })?;
    let credential = credential_input(&credential_kind, &credential_bytes)?;
    let msek = unseal_wrapped_msek(&blob, &credential).map_err(|e| FfiError::General {
        msg: format!("unseal wrapped-msek: {e}"),
    })?;
    Ok(msek.to_vec())
}

/// Seal an `MlsSnapshotBlob` for nest's `provision_mls_snapshot_blob`.
///
/// `serialized_state` is the caller-prepared read-only MLS provider
/// snapshot (signing key elided; design tracked internally, § Snapshot
/// plaintext contents).
///
/// ⚠ TEST-ONLY IN PRACTICE — MUST NOT GAIN A PRODUCTION CALLER (2026-07-15
/// dark-rail audit). Takes the raw MSEK as a bare `Vec<u8>` — the exact
/// shape the webdav-blob removal forbids (see the NOTE below).
/// Production seals Rust-side (`fauna_client_mail_settings::wrap` →
/// `wrapped_blob::seal_mls_snapshot`, MSEK never crossing FFI) and the Go
/// MDA unseals via the capability method (`MlsCapability::decrypt`).
/// RATIFIED as the accepted test-minting surface:
/// the Go MDA test fixtures and
/// `seal-helper-testonly` mint blobs through it, and the
/// `dark-rail-audit-check` merge gate is the backstop that reds on any
/// future production caller. Its dead unseal twin was DELETED by the same
/// ruling — do not re-add it; tests open via the inner
/// `fauna_mls::wrapped_blob::unseal_mls_snapshot`, and the MDA opens via
/// the capability.
#[uniffi::export]
pub fn seal_mls_snapshot_blob(
    serialized_state: Vec<u8>,
    actor_id: Vec<u8>,
    msek: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let actor_id = bytes32(&actor_id, "actor_id")?;
    let msek = bytes32(&msek, "msek")?;
    let blob = inner_seal_mls_snapshot(&serialized_state, &actor_id, &msek).map_err(|e| {
        FfiError::General {
            msg: format!("seal mls-snapshot: {e}"),
        }
    })?;
    blob.to_canonical_bytes().map_err(|e| FfiError::General {
        msg: format!("encode mls-snapshot: {e}"),
    })
}

// NOTE: there is deliberately NO free `unseal_mls_snapshot_blob` FFI export
// (dead on the
// Go/client face; its only caller was one Rust FFI test, now on the inner
// `fauna_mls::wrapped_blob::unseal_mls_snapshot`). Same rationale as the
// webdav NOTE below: a bare-MSEK unseal export existing only to be tested
// is the anti-shape a review established.

// NOTE: there is deliberately NO free `seal_webdav_keys_blob` / `unseal_webdav_keys_blob`
// FFI export. The MSEK must never cross the FFI as a bare `Vec<u8>`:
// the Go MDA unseals via the
// capability method `MlsCapability::unseal_webdav_keys_blob` (the MSEK stays inside the
// capability), and the client seals Rust-side through `fauna_mls::wrapped_blob::
// seal_webdav_keys_blob` in `reconcile_webdav_keys_blob`. Both free fns were dead
// (no Go/Rust caller) and were removed to keep the "MSEK never crosses FFI" invariant tight.

/// One `FolderContentKeys` generation (`mls-group-key-material.md` § M2),
/// exposed to the Go WebDAV MDA. `key` is the 32-byte `chunk_crypto` root new
/// uploads at `version` sealed under.
#[derive(uniffi::Record, Clone)]
pub struct FfiContentKeyGeneration {
    pub version: u64,
    pub key: Vec<u8>,
    pub rotated_at: u64,
}

/// A served set's full content-key generation history, so the MDA can
/// `webdav_content_key_for(version)` any historical file it lists.
#[derive(uniffi::Record, Clone)]
pub struct FfiFolderContentKeys {
    pub current: FfiContentKeyGeneration,
    pub prior: Vec<FfiContentKeyGeneration>,
}

/// One WebDAV-served set's keys (the unsealed `ServedSetKeys`). `read_only` is
/// advisory metadata (chunk_crypto is symmetric); the MDA enforces it at the
/// PUT boundary (Guard 2).
#[derive(uniffi::Record, Clone)]
pub struct FfiServedSetKeys {
    pub set_name: String,
    pub read_only: bool,
    pub keys: FfiFolderContentKeys,
}

/// The unsealed `WebdavKeysPlaintext` — the actor's served-set content keys.
#[derive(uniffi::Record, Clone)]
pub struct FfiWebdavKeysPlaintext {
    pub served_sets: Vec<FfiServedSetKeys>,
}

impl From<&fauna_core::folder_keys::ContentKeyGeneration> for FfiContentKeyGeneration {
    fn from(g: &fauna_core::folder_keys::ContentKeyGeneration) -> Self {
        FfiContentKeyGeneration {
            version: g.version,
            key: g.key.to_vec(),
            rotated_at: g.rotated_at,
        }
    }
}

impl From<&fauna_core::folder_keys::FolderContentKeys> for FfiFolderContentKeys {
    fn from(k: &fauna_core::folder_keys::FolderContentKeys) -> Self {
        FfiFolderContentKeys {
            current: FfiContentKeyGeneration::from(&k.current),
            prior: k.prior.iter().map(FfiContentKeyGeneration::from).collect(),
        }
    }
}

impl From<&ServedSetKeys> for FfiServedSetKeys {
    fn from(s: &ServedSetKeys) -> Self {
        FfiServedSetKeys {
            set_name: s.set_name.clone(),
            read_only: s.read_only,
            keys: FfiFolderContentKeys::from(&s.keys),
        }
    }
}

impl From<&WebdavKeysPlaintext> for FfiWebdavKeysPlaintext {
    fn from(pt: &WebdavKeysPlaintext) -> Self {
        FfiWebdavKeysPlaintext {
            served_sets: pt.served_sets.iter().map(FfiServedSetKeys::from).collect(),
        }
    }
}

// NOTE: there is deliberately NO `webdav_content_keys_for` export any more
// (removed 2026-09-03 with the per-chunk `decrypt_chunk` door it fed). The
// generation selection it mirrored — every same-version candidate,
// current-first, fail closed on a generation this holder lacks — is
// `FolderContentKeys::keys_for`, which the shared open walk
// (`webdav_open_file` → `fauna_core::file_download`) already applies; a
// Go-side copy of the loop was one more place for the read policy to drift.

// ---------------------------------------------------------------------------
// Sealed names & paths — the MDA leg (path-sealing S4)
//
// `docs/goal/behavior/webdav-server.md` § Key model: *"the MDA unseals listing
// rows' `path_sealed` per AUTH'd session under these same already-held content
// keys (via the shared-Rust FFI, never a Go reimplementation) and seals the
// path on every PUT record"*. These two exports are that FFI. They add **no new
// key material** — both take the `FfiFolderContentKeys` the MDA already holds
// from its `WebdavKeysBlob`.
//
// Both delegate to `fauna_core::label_custody`, the one seam every other read
// surface uses (media list, snapshot browse, snapshot diff, conflicts). There is
// deliberately no second resolver and no second salt rule here — a wrong root
// and a wrong salt both degrade silently to "omit", so a divergent copy would
// fail invisibly.
// ---------------------------------------------------------------------------

/// One listing row for [`webdav_render_paths`] — the sealed label, its salt, and
/// the plaintext arm, exactly as `webdav_list_files` returns them.
#[derive(uniffi::Record, Clone)]
pub struct FfiWebdavPathRow {
    /// The plaintext `path` column. Empty once the flip scrubs it; a
    /// sealed row renders without it.
    pub path: String,
    /// The row's opaque `SealedLabel` envelope, or `None` on a row with no seal
    /// (a keyless writer's, on a plaintext-resting plane).
    pub path_sealed: Option<Vec<u8>>,
    /// The row's `path_hash` — the convergent salt `path_sealed` opens under.
    pub path_hash: Option<Vec<u8>>,
}

/// The MDA's custody for one served set: **content keys only**. Never a
/// `BackupKey` — `key-material-hierarchy.md` rule #7 forbids the MDA from ever
/// holding one, which is exactly why a `gen: None` label (sealed under the
/// owner root) correctly renders as "omit" for this reader rather than being a
/// gap to plug. One custody object for both the label render
/// ([`webdav_render_paths`]) and the byte walk ([`webdav_open_file`]) — the
/// ruling's own logic: whoever can open the set's bytes renders its names.
fn webdav_custody(
    keys: FfiFolderContentKeys,
) -> Result<fauna_core::file_download::FileDownloadKeys, FfiError> {
    fn generation(
        g: FfiContentKeyGeneration,
    ) -> Result<fauna_core::folder_keys::ContentKeyGeneration, FfiError> {
        let key: [u8; 32] = g.key.as_slice().try_into().map_err(|_| FfiError::General {
            msg: format!(
                "webdav label custody: content key for generation {} is {} bytes, expected 32",
                g.version,
                g.key.len()
            ),
        })?;
        Ok(fauna_core::folder_keys::ContentKeyGeneration {
            version: g.version,
            key: fauna_core::secret::SecretArray32::from(key),
            rotated_at: g.rotated_at,
        })
    }

    Ok(fauna_core::file_download::FileDownloadKeys {
        backup_key: None,
        mls_group_id: None,
        content_keys: Some(fauna_core::folder_keys::FolderContentKeys {
            current: generation(keys.current)?,
            prior: keys
                .prior
                .into_iter()
                .map(generation)
                .collect::<Result<Vec<_>, _>>()?,
        }),
        ..Default::default()
    })
}

/// Render a served set's listing rows sealed-first, for the DAV PROPFIND.
///
/// Returns one entry **per input row, index-aligned** (the caller must check
/// the lengths match): `Some(name)` to list the entry, `None` for the ratified
/// degrade — **omit the entry from the listing, and let it re-enter on
/// re-record** (`file-sync.md` § Sealed names & paths → *Migration*). An
/// unopenable row is never an error and never an empty name: a DAV client shown
/// a blank href would corrupt its own view of the collection.
///
/// A row renders as `None` when this session's blob holds no key that opens it —
/// most importantly a label stamped `gen: None`, i.e. sealed under the owner's
/// `BackupKey` before the set was flipped to served. That is **correct, not a
/// hole**: the MDA structurally never holds a `BackupKey`, and the serve-enable
/// re-seal (`SyncEngine::reseal_pending_under_current`) re-records every live
/// head under the genesis content key, so a served set's steady-state rows are
/// stamped `gen: Some(v)` and do open here.
///
/// A seal that opens but does **not** hash to the row's own `path_hash` renders
/// `None` too (`path-sealing.md` § *An opened path is bound to its own row*):
/// the MDA *acts* on these names — GET serves, DELETE tombstones, COPY/MOVE
/// resolve by them — so an unbound label would have it act on another row. The
/// check lives in the shared `label_custody::render_path`, never here.
#[uniffi::export]
pub fn webdav_render_paths(
    keys: FfiFolderContentKeys,
    rows: Vec<FfiWebdavPathRow>,
) -> Result<Vec<Option<String>>, FfiError> {
    use fauna_core::path_crypto::{LabelField, SealedLabelRender};

    let custody = webdav_custody(keys)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            match fauna_core::label_custody::render_path(
                &custody,
                row.path_sealed.as_deref(),
                &row.path,
                row.path_hash.as_deref(),
                LabelField::SyncChangePath,
            ) {
                SealedLabelRender::Sealed(name) | SealedLabelRender::Plaintext(name) => Some(name),
                SealedLabelRender::Omit => None,
            }
        })
        .collect())
}

/// The served set's hash address — `fauna_core::path_crypto::set_name_hash`,
/// the equality-only digest the nest resolves a set by. The MDA stamps it as
/// `name_hash` on every set-scoped request it sends (the listing, the record and
/// the byte-token mint), so the set stays addressable once the nest's plaintext
/// name blanks, and the Go bridge never re-implements the derivation.
#[uniffi::export]
pub fn webdav_set_name_hash(set_name: String) -> Vec<u8> {
    fauna_core::path_crypto::set_name_hash(&set_name).to_vec()
}

/// Parse a principal's `Fauna-Folder-Keys` header — the WebDAV bearer door's
/// per-request credential beside `Authorization: DPoP` (`webdav-server.md`
/// § Key model → *A principal's read*, (3)) — into the same
/// [`FfiFolderContentKeys`] the MSEK-unsealed `WebdavKeysBlob` hands the MDA,
/// so the bearer arm opens rows and bytes through the very exports the Basic
/// arm does. The codec is `FolderContentKeys::from_header_value`; the Go bridge
/// never decodes CBOR. The error names no byte of the value.
#[uniffi::export]
pub fn webdav_decode_folder_keys_header(value: String) -> Result<FfiFolderContentKeys, FfiError> {
    fauna_core::folder_keys::FolderContentKeys::from_header_value(&value)
        .map(|keys| FfiFolderContentKeys::from(&keys))
        .map_err(|e| FfiError::General {
            msg: format!("webdav folder keys header: {e}"),
        })
}

/// Seal a plaintext relative path for a DAV PUT, under the served set's
/// **current** content-key generation.
///
/// The bridge-side half of `webdav-server.md` § Key model — the nest holds no
/// key that could seal this, which is why the DAV write leg was one of the named
/// keyless writer seams until now. Byte-for-byte the same derivation as
/// `SyncEngine::seal_recorded_path` (convergent nonce, salt = `path_hash`, field
/// tag `SyncChangePath`), so a DAV-written row is indistinguishable from one an
/// ordinary client recorded — and opens under the same roots on every reader.
///
/// ⚠ Seal under the **same** generation the PUT stamps into
/// `content_key_version`. Both come from `keys.current` by construction here;
/// keep it that way, or a row's label and its chunks disagree about which
/// generation opens them.
#[uniffi::export]
pub fn webdav_seal_path(keys: FfiFolderContentKeys, path: String) -> Result<Vec<u8>, FfiError> {
    use fauna_core::path_crypto::{LabelField, LabelRoot, seal_convergent};

    let version = keys.current.version;
    let root = current_content_root(&keys, "webdav seal path")?;
    let salt = fauna_core::sync::path_hash(&path);
    seal_convergent(
        &LabelRoot::content_key(root, version),
        &salt,
        LabelField::SyncChangePath,
        path.as_bytes(),
    )
    .and_then(|sealed| sealed.to_bytes())
    .map_err(|e| FfiError::General {
        msg: format!("webdav seal path: {e}"),
    })
}

// ---------------------------------------------------------------------------
// The MDA's byte plane — seal on PUT, open on GET, both the SHARED pipeline
//
// `docs/goal/behavior/webdav-server.md` § Key model: *"PUT = chunk + seal under
// `current` via the shared Rust chunker/sealer through the existing
// UniFFI→Go FFI (never a Go reimplementation)"*, and § Implementation status:
// the DAV-written artifacts are *"byte-identical to the sync engine's ... and
// vice-versa"*. Until 2026-09-03 the PUT leg sealed each chunk RAW through a
// bare `encrypt_chunk` export while every Rust writer sealed the FRAMED body
// — two plaintexts under one deterministic (key, nonce), the plaintext
// recoverable by XOR with no key (`fauna_core::chunk_seal`, module doc) — and
// the GET leg never unframed at all, so an app-written file read over DAV
// came back with its frame bytes. Both legs now ARE the shared pipeline:
// `webdav_seal_file` is the engine's `seal_blob`; `webdav_open_file` is the
// apps' `download_file_bytes_by_manifest` walk (generation selection, AEAD
// open, unframe-by-hash, whole-file verify) over blobs the MDA fetched.
// ---------------------------------------------------------------------------

/// One sealed chunk of an [`FfiSealedFile`]: `store_key` is the **ciphertext**
/// hash (32 bytes — the blob-store address, `manifest.stored_hashes[i]`),
/// `body` the bytes to upload under it.
#[derive(uniffi::Record)]
pub struct FfiSealedChunk {
    pub store_key: Vec<u8>,
    pub body: Vec<u8>,
}

/// A whole file sealed for the byte plane by [`webdav_seal_file`].
#[derive(uniffi::Record)]
pub struct FfiSealedFile {
    /// Canonical dag-cbor `ChunkManifest` with `stored_hashes` populated —
    /// byte-identical to what the sync engine uploads for the same bytes.
    pub manifest_bytes: Vec<u8>,
    /// `blake3(manifest_bytes)` (32 bytes) — the change record's manifest hash
    /// and the DAV ETag.
    pub manifest_hash: Vec<u8>,
    /// The content-key generation the chunks were sealed under
    /// (`keys.current.version`) — stamp exactly this into the change record.
    pub content_key_version: u64,
    /// `(store key, body)` per chunk, in manifest order.
    pub chunks: Vec<FfiSealedChunk>,
}

/// The served set's **current** 32-byte content key, or an error naming the
/// generation whose key is the wrong length.
fn current_content_root(keys: &FfiFolderContentKeys, what: &str) -> Result<[u8; 32], FfiError> {
    keys.current
        .key
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::General {
            msg: format!(
                "{what}: content key for generation {} is {} bytes, expected 32",
                keys.current.version,
                keys.current.key.len()
            ),
        })
}

/// Chunk + seal `data` for a DAV PUT under the served set's **current**
/// content-key generation — the sync engine's `seal_blob`, verbatim: FastCDC
/// chunking, the one per-chunk seal (`fauna_core::chunk_seal` — frame, AEAD
/// keyed by the plaintext hash, re-key by ciphertext hash), and the canonical
/// manifest carrying `stored_hashes`. The Go MDA uploads each `body` under its
/// `store_key`, the manifest, and records the change stamped
/// `content_key_version`.
///
/// Seals under `current` and returns that generation together, so the record
/// stamps the generation the bytes were actually sealed under by construction
/// (the same invariant [`webdav_seal_path`] states for the label).
#[uniffi::export]
pub fn webdav_seal_file(
    keys: FfiFolderContentKeys,
    data: Vec<u8>,
) -> Result<FfiSealedFile, FfiError> {
    let version = keys.current.version;
    let root = current_content_root(&keys, "webdav seal file")?;
    let sealed =
        fauna_sync_engine::seal::seal_blob(&data, Some((root, Some(version)))).map_err(|e| {
            FfiError::General {
                msg: format!("webdav seal file: {e:#}"),
            }
        })?;
    Ok(FfiSealedFile {
        manifest_bytes: sealed.manifest_bytes,
        manifest_hash: sealed.manifest_hash.digest().to_vec(),
        content_key_version: version,
        chunks: sealed
            .chunks
            .into_iter()
            .map(|(store_key, body)| FfiSealedChunk {
                store_key: store_key.digest().to_vec(),
                body,
            })
            .collect(),
    })
}

/// Blobs the MDA already fetched over the byte routes, offered to the shared
/// walk through its own `BlobFetcher` seam — so the walk stays the ONE
/// implementation and this crate adds no read policy of its own.
struct SuppliedBlobs {
    manifest_hash: fauna_core::data::ContentHash,
    manifest_bytes: Vec<u8>,
    chunks: std::collections::HashMap<fauna_core::data::ContentHash, Vec<u8>>,
}

#[async_trait::async_trait]
impl fauna_core::file_download::BlobFetcher for SuppliedBlobs {
    async fn fetch_manifest(
        &self,
        hash: &fauna_core::data::ContentHash,
    ) -> anyhow::Result<Vec<u8>> {
        if *hash != self.manifest_hash {
            anyhow::bail!(
                "webdav open file: the walk asked for manifest {} but the MDA supplied {}",
                hex::encode(hash.digest()),
                hex::encode(self.manifest_hash.digest())
            );
        }
        Ok(self.manifest_bytes.clone())
    }

    async fn fetch_chunks(
        &self,
        store_keys: &[fauna_core::data::ContentHash],
        relative_path: &str,
    ) -> anyhow::Result<Vec<Vec<u8>>> {
        store_keys
            .iter()
            .map(|k| {
                self.chunks.get(k).cloned().ok_or_else(|| {
                    anyhow::anyhow!(
                        "webdav open file: {relative_path}: no supplied body for store key {}",
                        hex::encode(k.digest())
                    )
                })
            })
            .collect()
    }
}

/// Open a DAV GET's file — the apps' shared walk
/// (`fauna_core::file_download::download_file_bytes_by_manifest`) over the
/// manifest and ciphertext chunk bodies the MDA fetched by store key, under
/// the served set's content keys: generation selection for
/// `content_key_version` (every same-version candidate, current-first; a
/// generation this holder lacks fails closed), AEAD open, unframe decided by
/// the manifest's plaintext hash (framed first, then the settled raw read for
/// files sealed unframed), and the whole-file
/// content-address verify. Returns the plaintext.
///
/// `chunk_bodies` are in manifest order — one per `stored_hashes[i]`, exactly
/// as `deserialize_manifest` lists them; a count mismatch is an error before
/// any crypto runs.
#[uniffi::export]
pub fn webdav_open_file(
    keys: FfiFolderContentKeys,
    content_key_version: u64,
    relative_path: String,
    manifest_bytes: Vec<u8>,
    chunk_bodies: Vec<Vec<u8>>,
) -> Result<Vec<u8>, FfiError> {
    use fauna_core::data::ContentHash;

    let custody = webdav_custody(keys)?;
    let manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(&manifest_bytes).map_err(|e| FfiError::General {
            msg: format!("webdav open file: decode manifest: {e:#}"),
        })?;
    let store_keys = manifest.store_keys();
    if store_keys.len() != chunk_bodies.len() {
        return Err(FfiError::General {
            msg: format!(
                "webdav open file: {relative_path}: manifest lists {} chunks but {} bodies were \
                 supplied",
                store_keys.len(),
                chunk_bodies.len()
            ),
        });
    }
    let manifest_hash = ContentHash::of_raw(&manifest_bytes);
    let fetcher = SuppliedBlobs {
        manifest_hash,
        manifest_bytes,
        chunks: store_keys.into_iter().zip(chunk_bodies).collect(),
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| FfiError::General {
            msg: format!("webdav open file: runtime: {e}"),
        })?;
    rt.block_on(fauna_core::file_download::download_file_bytes_by_manifest(
        &fetcher,
        &custody,
        manifest_hash,
        Some(content_key_version),
        &relative_path,
    ))
    .map_err(|e| FfiError::General {
        msg: format!("webdav open file: {relative_path}: {e:#}"),
    })
}

#[cfg(test)]
mod webdav_keys_tests {
    use super::*;

    fn keygen(version: u64, byte: u8) -> FfiContentKeyGeneration {
        FfiContentKeyGeneration {
            version,
            key: vec![byte; 32],
            rotated_at: version * 1000,
        }
    }

    // ── The bearer door's keys header ───────────────────────────────────

    /// The header a principal sends for `{current: v3, key 0x42…, rotated_at
    /// 1}` — what `FolderContentKeys::to_header_value` writes. The Go bearer
    /// door's tests (`bins/fauna-bridges/internal/mda/webdav/bearer_test.go`)
    /// present this very string, so a drift in the codec reds both sides.
    const BEARER_FIXTURE_KEYS_HEADER: &str = "omVwcmlvcoBnY3VycmVudKNja2V5WCBCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQmd2ZXJzaW9uA2pyb3RhdGVkX2F0AQ";
    /// `fauna_core::sync::path_hash("notes.txt")` — the bound salt of the Go
    /// bearer tests' listing row.
    const BEARER_FIXTURE_PATH_HASH_HEX: &str =
        "a859c1198d69e635a868b337216913df319113c1315cdd69ff8eb460ee5f62de";

    #[test]
    fn webdav_decode_folder_keys_header_matches_the_principals_encoding() {
        let keys = fauna_core::folder_keys::FolderContentKeys {
            current: fauna_core::folder_keys::ContentKeyGeneration {
                version: 3,
                key: [0x42; 32].into(),
                rotated_at: 1,
            },
            prior: Vec::new(),
        };
        assert_eq!(keys.to_header_value().unwrap(), BEARER_FIXTURE_KEYS_HEADER);
        assert_eq!(
            hex::encode(fauna_core::sync::path_hash("notes.txt")),
            BEARER_FIXTURE_PATH_HASH_HEX
        );
        let decoded = webdav_decode_folder_keys_header(BEARER_FIXTURE_KEYS_HEADER.into()).unwrap();
        assert_eq!(decoded.current.version, 3);
        assert_eq!(decoded.current.key, vec![0x42; 32]);
        assert!(decoded.prior.is_empty());

        let err = match webdav_decode_folder_keys_header("bm90IGtleXM".into()) {
            Err(FfiError::General { msg }) => msg,
            other => panic!("garbage decoded: {}", other.is_ok()),
        };
        assert!(!err.contains("bm90IGtleXM"), "{err}");
    }

    // ── The byte plane: one seal pipeline, one open walk ────────────────

    /// A body larger than the 4 KiB compression floor and highly
    /// compressible, so the zstd arm of the frame actually runs, plus a tail
    /// that keeps it from being one chunk of repeats.
    fn file_body() -> Vec<u8> {
        let mut v = b"the DAV-written document, line after line. "
            .iter()
            .cycle()
            .take(40_000)
            .copied()
            .collect::<Vec<u8>>();
        v.extend((0..3000u32).map(|i| (i * 7 + 13) as u8));
        v
    }

    /// ~9 MiB of noisy bytes — past the chunker's 8 MiB single-chunk
    /// threshold (`chunker::SINGLE_CHUNK_THRESHOLD`: anything smaller is ONE
    /// chunk by design) and noisy enough for FastCDC to cut near its 2 MiB
    /// average, so a fixture spans several content-defined chunks.
    fn multi_chunk_body() -> Vec<u8> {
        let mut x: u32 = 0x9E37_79B9;
        (0..(9 * 1024 * 1024 + 12_345))
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (x >> 24) as u8
            })
            .collect()
    }

    /// **The finding's pin.** The MDA's seal door and the sync
    /// engine's seal are the same bytes — manifest and every chunk — for the
    /// same file under the same generation key. Before 2026-09-03 the MDA
    /// sealed each chunk raw and this assertion failed on every chunk. The
    /// body is a compressible head (the zstd frame arm) plus a noisy
    /// multi-chunk tail (the raw frame arm, several chunks).
    #[test]
    fn webdav_seal_file_is_the_sync_engines_seal_byte_for_byte() {
        let keys = FfiFolderContentKeys {
            current: keygen(3, 0xC1),
            prior: vec![keygen(2, 0xB2)],
        };
        let mut data = file_body();
        data.extend(multi_chunk_body());
        let sealed = webdav_seal_file(keys.clone(), data.clone()).unwrap();

        let engine =
            fauna_sync_engine::seal::seal_blob(&data, Some(([0xC1; 32], Some(3)))).unwrap();
        assert_eq!(sealed.manifest_bytes, engine.manifest_bytes);
        assert_eq!(sealed.manifest_hash, engine.manifest_hash.digest().to_vec());
        assert_eq!(sealed.content_key_version, 3);
        assert_eq!(sealed.chunks.len(), engine.chunks.len());
        assert!(
            sealed.chunks.len() > 1,
            "the fixture must span several chunks"
        );
        for (ffi, (store_key, body)) in sealed.chunks.iter().zip(&engine.chunks) {
            assert_eq!(ffi.store_key, store_key.digest().to_vec());
            assert_eq!(&ffi.body, body);
        }

        // And chunk by chunk against the per-chunk door itself — the function
        // the streaming upload and the share leg call.
        let manifest = fauna_core::chunker::chunk_file(&data);
        for ((hash, plain), ffi) in fauna_core::chunker::extract_chunks(&data, &manifest)
            .iter()
            .zip(&sealed.chunks)
        {
            let (k, b) = fauna_core::chunk_seal::seal_chunk_body(hash, plain, &[0xC1; 32]).unwrap();
            assert_eq!(ffi.store_key, k.digest().to_vec());
            assert_eq!(ffi.body, b);
        }
    }

    fn bodies_of(sealed: &FfiSealedFile) -> Vec<Vec<u8>> {
        sealed.chunks.iter().map(|c| c.body.clone()).collect()
    }

    /// DAV PUT → DAV GET, and engine write → DAV GET: the open walk returns
    /// the plaintext for a file sealed by either writer (they are the same
    /// bytes, but the GET leg is exercised on both to pin the *reader*).
    #[test]
    fn webdav_open_file_opens_a_dav_put_and_an_engine_write_alike() {
        let keys = FfiFolderContentKeys {
            current: keygen(3, 0xC1),
            prior: vec![keygen(2, 0xB2)],
        };
        let data = file_body();

        let put = webdav_seal_file(keys.clone(), data.clone()).unwrap();
        let got = webdav_open_file(
            keys.clone(),
            put.content_key_version,
            "docs/report.txt".into(),
            put.manifest_bytes.clone(),
            bodies_of(&put),
        )
        .unwrap();
        assert_eq!(got, data);

        // An app's engine wrote this one under the PRIOR generation.
        let engine =
            fauna_sync_engine::seal::seal_blob(&data, Some(([0xB2; 32], Some(2)))).unwrap();
        let got = webdav_open_file(
            keys,
            2,
            "docs/report.txt".into(),
            engine.manifest_bytes.clone(),
            engine.chunks.iter().map(|(_, b)| b.clone()).collect(),
        )
        .unwrap();
        assert_eq!(got, data);
    }

    /// Generation selection is the shared walk's, not a Go loop's: a file
    /// stamped with a generation this holder lacks fails closed (never a
    /// silent fall back to `current`, FS-5DC), and after a concurrent-rotation
    /// custody merge every same-version candidate is tried.
    #[test]
    fn webdav_open_file_selects_by_generation_and_fails_closed() {
        let data = file_body();
        let engine =
            fauna_sync_engine::seal::seal_blob(&data, Some(([0xCC; 32], Some(2)))).unwrap();
        let bodies: Vec<Vec<u8>> = engine.chunks.iter().map(|(_, b)| b.clone()).collect();

        // Sealed under the SHADOWED generation-2 key: opens because both
        // generation-2 candidates are tried.
        let merged = FfiFolderContentKeys {
            current: keygen(2, 0xBB),
            prior: vec![keygen(2, 0xCC), keygen(1, 0xAA)],
        };
        let got = webdav_open_file(
            merged.clone(),
            2,
            "docs/a.txt".into(),
            engine.manifest_bytes.clone(),
            bodies.clone(),
        )
        .unwrap();
        assert_eq!(got, data);

        // A generation this holder lacks: error, not the current key.
        let err = webdav_open_file(
            merged.clone(),
            7,
            "docs/a.txt".into(),
            engine.manifest_bytes.clone(),
            bodies.clone(),
        )
        .unwrap_err();
        assert!(
            format!("{err:?}").contains("generation 7"),
            "must name the missing generation: {err:?}"
        );

        // The right generation but not the key it was sealed under: the AEAD
        // fails closed (no candidate opens it).
        let wrong_key = FfiFolderContentKeys {
            current: keygen(2, 0xDD),
            prior: vec![],
        };
        assert!(
            webdav_open_file(
                wrong_key,
                2,
                "docs/a.txt".into(),
                engine.manifest_bytes,
                bodies
            )
            .is_err()
        );
    }

    /// A file sealed UNFRAMED, with a chunk whose plaintext begins with a frame
    /// byte, opens over DAV: the walk lets the manifest's plaintext hash decide
    /// (framed first, the raw body as the fallback), and since the apps run the
    /// same walk, on every app.
    #[test]
    fn webdav_open_file_opens_an_unframed_file_by_its_hash() {
        use fauna_core::data::ContentHash;

        let root = [0xC1u8; 32];
        let data = [b"\x00".as_slice(), b"unframed raw file body".as_slice()].concat();
        let mut manifest = fauna_core::chunker::chunk_file(&data);
        let chunks = fauna_core::chunker::extract_chunks(&data, &manifest);
        let sealed: Vec<(ContentHash, Vec<u8>)> = chunks
            .into_iter()
            .map(|(hash, raw)| {
                let raw_chunk =
                    fauna_core::chunk_seal::FramedChunk::arbitrary_for_fixtures(hash, raw);
                fauna_core::chunk_seal::seal_framed_chunk(&raw_chunk, &root).unwrap()
            })
            .collect();
        manifest.stored_hashes = Some(sealed.iter().map(|(k, _)| *k).collect());
        let manifest_bytes =
            fauna_core::encoding::canonical_encode(&manifest.wire_form(Some(&root)).unwrap())
                .unwrap();

        let keys = FfiFolderContentKeys {
            current: keygen(3, 0xC1),
            prior: vec![],
        };
        let got = webdav_open_file(
            keys,
            3,
            "docs/raw.bin".into(),
            manifest_bytes,
            sealed.into_iter().map(|(_, b)| b).collect(),
        )
        .unwrap();
        assert_eq!(got, data);
    }

    /// The body count is checked before any crypto runs.
    #[test]
    fn webdav_open_file_refuses_a_body_count_mismatch() {
        let keys = FfiFolderContentKeys {
            current: keygen(3, 0xC1),
            prior: vec![],
        };
        let put = webdav_seal_file(keys.clone(), file_body()).unwrap();
        let mut bodies = bodies_of(&put);
        bodies.pop();
        let err =
            webdav_open_file(keys, 3, "docs/x.txt".into(), put.manifest_bytes, bodies).unwrap_err();
        assert!(
            format!("{err:?}").contains("bodies were supplied"),
            "{err:?}"
        );
    }

    // ── Sealed names & paths: the MDA leg (S4) ──────────────────────────

    const REL: &str = "2026/eviction_notice.pdf";

    fn keys(
        current: FfiContentKeyGeneration,
        prior: Vec<FfiContentKeyGeneration>,
    ) -> FfiFolderContentKeys {
        FfiFolderContentKeys { current, prior }
    }

    fn row(path: &str, sealed: Option<Vec<u8>>) -> FfiWebdavPathRow {
        FfiWebdavPathRow {
            path: path.to_string(),
            path_sealed: sealed,
            // The convergent salt always rides the wire beside the seal — a
            // sealed row without it is unrenderable once the plaintext scrubs.
            path_hash: Some(fauna_core::sync::path_hash(REL).to_vec()),
        }
    }

    /// The round trip the DAV leg is: PUT seals, PROPFIND renders. Non-vacuous
    /// because the row's plaintext is a **decoy** — a renderer that read the
    /// plaintext column would return the decoy and fail here.
    #[test]
    fn a_put_seal_renders_back_through_the_listing_not_the_plaintext() {
        let k = keys(keygen(3, 0xC1), vec![]);
        let sealed = webdav_seal_path(k.clone(), REL.to_string()).unwrap();

        let rendered =
            webdav_render_paths(k, vec![row("DECOY-not-the-real-name", Some(sealed))]).unwrap();
        assert_eq!(rendered, vec![Some(REL.to_string())]);
    }

    /// The seal must be openable under a *retained prior* generation too — a
    /// set that rotated after the write still lists its own back-catalogue.
    #[test]
    fn a_prior_generation_seal_still_renders_after_a_rotation() {
        let at_write = keys(keygen(1, 0xAA), vec![]);
        let sealed = webdav_seal_path(at_write, REL.to_string()).unwrap();

        // The set rotated: current is now 2, generation 1 retained as prior.
        let after_rotate = keys(keygen(2, 0xBB), vec![keygen(1, 0xAA)]);
        let rendered = webdav_render_paths(after_rotate, vec![row("DECOY", Some(sealed))]).unwrap();
        assert_eq!(rendered, vec![Some(REL.to_string())]);
    }

    /// The ratified degrade, in its **post-flip** shape: the plaintext column is
    /// scrubbed (empty), so a seal this session cannot open leaves nothing to
    /// render and the entry omits. **Never** an error — one unopenable row must
    /// not take the whole PROPFIND down.
    #[test]
    fn an_unopenable_seal_omits_once_the_plaintext_is_scrubbed() {
        let sealed = webdav_seal_path(keys(keygen(1, 0xAA), vec![]), REL.to_string()).unwrap();

        // Same generation number, different key — the AEAD tag is the gate.
        let wrong_key = webdav_render_paths(
            keys(keygen(1, 0xFF), vec![]),
            vec![row("", Some(sealed.clone()))],
        )
        .unwrap();
        assert_eq!(wrong_key, vec![None], "a wrong key must omit, not guess");

        // A generation this holder never had at all.
        let missing_gen =
            webdav_render_paths(keys(keygen(9, 0xAA), vec![]), vec![row("", Some(sealed))])
                .unwrap();
        assert_eq!(missing_gen, vec![None]);
    }

    /// The same row **during expand**, where the plaintext still rests: it lists
    /// from the legacy column rather than vanishing. Pinned deliberately so a
    /// later reader does not "harden" this into an omit — that would make every
    /// already-served set's listing go dark the moment sealing switched on,
    /// which is exactly what the additive expand phase exists to avoid.
    #[test]
    fn an_unopenable_seal_still_lists_from_plaintext_during_expand() {
        let sealed = webdav_seal_path(keys(keygen(1, 0xAA), vec![]), REL.to_string()).unwrap();
        let rendered = webdav_render_paths(
            keys(keygen(9, 0xAA), vec![]),
            vec![row("legacy/name.pdf", Some(sealed))],
        )
        .unwrap();
        assert_eq!(rendered, vec![Some("legacy/name.pdf".to_string())]);
    }

    /// A `gen: None` label — sealed under the owner's `BackupKey` before the set
    /// was served — is the arm the MDA structurally cannot open (rule #7 forbids
    /// it holding a `BackupKey`). Post-flip it omits, and that is correct rather
    /// than a gap: the serve-enable re-seal re-records every live head under the
    /// genesis content key, so a served set's steady-state rows are `gen:
    /// Some(v)`.
    #[test]
    fn an_owner_root_label_omits_because_the_mda_never_holds_a_backup_key() {
        use fauna_core::path_crypto::{LabelField, LabelRoot, seal_convergent};
        let owner_sealed = seal_convergent(
            &LabelRoot::owner([0x5e; 32]),
            &fauna_core::sync::path_hash(REL),
            LabelField::SyncChangePath,
            REL.as_bytes(),
        )
        .unwrap()
        .to_bytes()
        .unwrap();

        let rendered = webdav_render_paths(
            keys(keygen(1, 0xAA), vec![]),
            vec![row("", Some(owner_sealed))],
        )
        .unwrap();
        assert_eq!(rendered, vec![None]);
    }

    /// A row with no seal still lists from its plaintext — what keeps a
    /// plaintext-resting plane (a `public`-audience folder) listable.
    #[test]
    fn an_unsealed_row_lists_from_its_legacy_plaintext() {
        let rendered =
            webdav_render_paths(keys(keygen(1, 0xAA), vec![]), vec![row(REL, None)]).unwrap();
        assert_eq!(rendered, vec![Some(REL.to_string())]);
    }

    /// The MDA **acts** on a rendered name — GET serves it, DELETE
    /// tombstones it, COPY/MOVE resolve it — so a seal that opens is not enough.
    /// A writer holding the set's label key seals path P under Q's hash (the
    /// row the nest files as Q); the listing must omit it, never offer P, or a
    /// DELETE of P tombstones the owner's real P while Q survives.
    #[test]
    fn a_seal_opening_to_another_rows_path_omits() {
        use fauna_core::path_crypto::{LabelField, LabelRoot, seal_convergent};
        let k = keys(keygen(1, 0xAA), vec![]);
        let root = [0xAA; 32];
        let forged = seal_convergent(
            &LabelRoot::content_key(root, 1),
            &fauna_core::sync::path_hash(REL),
            LabelField::SyncChangePath,
            b"victim/real.pdf",
        )
        .unwrap()
        .to_bytes()
        .unwrap();

        let rendered = webdav_render_paths(k.clone(), vec![row("", Some(forged))]).unwrap();
        assert_eq!(rendered, vec![None], "an unbound seal must omit");

        // The honest row beside it still renders.
        let honest = webdav_seal_path(k.clone(), REL.to_string()).unwrap();
        assert_eq!(
            webdav_render_paths(k, vec![row("", Some(honest))]).unwrap(),
            vec![Some(REL.to_string())]
        );
    }

    /// Index alignment is the caller's contract — a mixed page must map
    /// position-for-position, or the Go side would rename rows.
    #[test]
    fn a_mixed_page_renders_index_aligned() {
        let k = keys(keygen(1, 0xAA), vec![]);
        let sealed = webdav_seal_path(k.clone(), REL.to_string()).unwrap();
        let unopenable = webdav_seal_path(keys(keygen(4, 0xEE), vec![]), REL.to_string()).unwrap();

        let rendered = webdav_render_paths(
            k,
            vec![
                row("plain.txt", None),
                row("DECOY", Some(sealed)),
                row("", Some(unopenable)),
            ],
        )
        .unwrap();
        assert_eq!(
            rendered,
            vec![Some("plain.txt".to_string()), Some(REL.to_string()), None]
        );
    }

    /// A malformed key blob is a bug in our own provisioning, not a degrade —
    /// it must be loud rather than silently omitting every row.
    #[test]
    fn a_malformed_content_key_is_an_error_not_a_silent_omit() {
        let bad = FfiFolderContentKeys {
            current: FfiContentKeyGeneration {
                version: 1,
                key: vec![0xAA; 31],
                rotated_at: 1,
            },
            prior: vec![],
        };
        assert!(webdav_render_paths(bad.clone(), vec![row(REL, None)]).is_err());
        assert!(webdav_seal_path(bad, REL.to_string()).is_err());
    }
}

/// Seal a `WrappedSubmissionTokenBlob` for nest's
/// `provision_wrapped_submission_token`.
///
/// `token_canonical_bytes` is the canonical DAG-CBOR encoding of an
/// already-signed `SubmissionToken` (signed by the user's identity
/// key; design tracked internally, § Per-credential submission-token
/// blob). The seal step adds the
/// credential-derived AEAD layer; signing happens earlier in the
/// state machine.
///
/// `actor_id` and `credential_id` must match the embedded fields of
/// the inner token; mismatch returns `FfiError::General`.
#[uniffi::export]
pub fn seal_submission_token_blob(
    token_canonical_bytes: Vec<u8>,
    actor_id: Vec<u8>,
    credential_id: String,
    credential_kind: String,
    credential_bytes: Vec<u8>,
    kdf_params: Option<KdfParamsFfi>,
) -> Result<Vec<u8>, FfiError> {
    let actor_id = bytes32(&actor_id, "actor_id")?;
    let credential = credential_input(&credential_kind, &credential_bytes)?;
    let kdf = resolve_kdf_params(kdf_params, &credential_kind)?;
    let token =
        InnerSubmissionToken::from_canonical_bytes(&token_canonical_bytes).map_err(|e| {
            FfiError::General {
                msg: format!("decode submission-token: {e}"),
            }
        })?;
    let blob = inner_seal_submission_token(&token, &actor_id, &credential_id, &credential, kdf)
        .map_err(|e| FfiError::General {
            msg: format!("seal submission-token: {e}"),
        })?;
    blob.to_canonical_bytes().map_err(|e| FfiError::General {
        msg: format!("encode submission-token: {e}"),
    })
}

/// HPKE-Seal `plaintext` to a recipient's X25519 pubkey, returning the
/// canonical DAG-CBOR `MailRecordEnvelope` bytes ready to ship to nest
/// as `IngestInboundMailRequest::encrypted_body` (or
/// `encrypted_index_hint`, when called with the canonical-token-set
/// bytes targeted at the recipient's index pubkey).
///
/// Phase C.9 of the I4 mail-bridge MTA arm. The Go bridge calls this
/// twice per inbound message — once with `raw` (RFC 5322 bytes) +
/// `recipient_mls_pubkey`, once with `index_hint.CanonicalBytes` +
/// `recipient_index_pubkey` — and ships both ciphertexts to nest via
/// `fauna.bridges.ingest_inbound_mail` (design tracked internally,
/// § Inbound mail flow steps 8-9).
///
/// `recipient_x25519_pubkey` must be exactly 32 bytes; AAD/info
/// binding is the constant `AadBinding::for_mail_record()` (the kind
/// tag provides cross-shape domain separation against the four other
/// wrapped-blob kinds, and HPKE's KEM provides per-recipient binding).
///
/// # Errors
///
/// `FfiError::General` carries:
///   - `recipient_x25519_pubkey must be 32 bytes` (length mismatch);
///   - the wrapped `WrapError` message (HPKE-seal or CBOR-encode
///     failure — both practically unreachable for valid inputs).
#[uniffi::export]
pub fn seal_to_recipient(
    plaintext: Vec<u8>,
    recipient_x25519_pubkey: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let pubkey = bytes32(&recipient_x25519_pubkey, "recipient_x25519_pubkey")?;
    let envelope = inner_seal_to_recipient(&plaintext, &pubkey).map_err(|e| FfiError::General {
        msg: format!("seal mail-record: {e}"),
    })?;
    envelope
        .to_canonical_bytes()
        .map_err(|e| FfiError::General {
            msg: format!("encode mail-record: {e}"),
        })
}

/// Post-quantum sibling of [`seal_to_recipient`]: X-Wing-Seal `plaintext` to a
/// recipient's **1216-byte X-Wing public key** (`mlkem_ek[1184] ∥ x25519[32]`),
/// returning the canonical DAG-CBOR `MailRecordEnvelope` bytes (its `enc` is the
/// 1120-byte X-Wing ciphertext; AEAD + AAD are byte-identical to the classical
/// path). The Go MTA assembles the pubkey from the recipient's published
/// `mlkem_ek` + their X25519 `mls_pubkey` and calls this **only when the
/// recipient published a post-quantum key** (the capability gate — see
/// `architecture/security/post-quantum.md` § Capability negotiation); otherwise
/// it stays on [`seal_to_recipient`] (the non-erroring classical degrade).
///
/// The opposite (open) side is the existing `unseal_mail_record` /
/// `unseal_mail_record_hybrid` pair — a blob self-describes its suite, so the
/// reader dispatches at runtime; only the seal caller picks the suite.
///
/// # Errors
///
/// `FfiError::General` carries:
///   - `recipient_xwing_pubkey must be 1216 bytes` (length mismatch);
///   - the wrapped `WrapError` message (X-Wing-seal or CBOR-encode failure —
///     both practically unreachable for valid inputs).
#[uniffi::export]
pub fn seal_to_recipient_xwing(
    plaintext: Vec<u8>,
    recipient_xwing_pubkey: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let pubkey_bytes: &[u8; XWING_ENCAPS_KEY_LEN] = recipient_xwing_pubkey
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::General {
            msg: format!(
                "recipient_xwing_pubkey must be {XWING_ENCAPS_KEY_LEN} bytes, got {}",
                recipient_xwing_pubkey.len()
            ),
        })?;
    let pubkey = XWingPublicKey::from_bytes(pubkey_bytes);
    let envelope =
        inner_seal_to_recipient_xwing(&plaintext, &pubkey).map_err(|e| FfiError::General {
            msg: format!("seal mail-record (x-wing): {e}"),
        })?;
    envelope
        .to_canonical_bytes()
        .map_err(|e| FfiError::General {
            msg: format!("encode mail-record (x-wing): {e}"),
        })
}

/// 32-byte X25519 keypair, used as the leaf-node HPKE init key inside
/// an MLS group. The user's primary client generates one at MLS
/// state-advance time and stores `secret` in the `MlsSnapshotPlaintext`
/// before sealing the snapshot to nest; the public half also goes to
/// nest's `actor_mls_pubkeys` so the MTA seals inbound mail to it.
#[derive(uniffi::Record)]
pub struct X25519Keypair {
    pub pubkey: Vec<u8>,
    pub secret: Vec<u8>,
}

/// Generate a fresh 32-byte X25519 HPKE init keypair. Called by the
/// user's primary client during MLS leaf-key rotation (to mint the
/// new entry that goes into `MlsSnapshotPlaintext.leaf_init_keypairs`
/// + nest's `actor_mls_pubkeys`); also used in cross-area integration
/// tests that need a real keypair the open path can HPKE-Open against.
#[uniffi::export]
pub fn generate_x25519_keypair() -> X25519Keypair {
    let (sk, pk) = inner_generate_x25519_keypair();
    X25519Keypair {
        pubkey: pk.to_vec(),
        secret: sk.to_vec(),
    }
}

/// Derive the actor's standing recipient-mail HPKE keypair from its
/// 32-byte MSEK (`fauna_mls::wrapped_blob::derive_recipient_hpke_keypair`:
/// BLAKE3 domain-separated → RFC 9180 `DeriveKeyPair`). Deterministic in
/// `msek`, so every device holding the same MSEK in the `fauna.state.mail` row derives
/// the identical keypair.
///
/// The `pubkey` half is what the user's primary client registers via
/// `fauna.bridges.provision_recipient_mls_pubkey` (the MTA / APPEND seal
/// inbound mail to it); the `secret` half is carried in the
/// `MlsSnapshotPlaintext` (see [`encode_mls_snapshot_plaintext_v1`]) so the
/// MDA opens those bodies with it. The two MUST derive from the same MSEK or
/// the registered pubkey and the snapshot's leaf secret won't match. See
/// `docs/goal/architecture/key-material-hierarchy.md` § Path B-sibling-2 and
/// `docs/goal/behavior/mail-credentials.md`.
///
/// `msek` must be exactly 32 bytes; otherwise `FfiError::General`.
#[uniffi::export]
pub fn derive_recipient_hpke_keypair(msek: Vec<u8>) -> Result<X25519Keypair, FfiError> {
    let msek = bytes32(&msek, "msek")?;
    let (sk, pk) = inner_derive_recipient_hpke_keypair(&msek);
    Ok(X25519Keypair {
        pubkey: pk.to_vec(),
        secret: sk.to_vec(),
    })
}

/// Derive the actor's per-epoch recipient-mail HPKE keypair for epoch `e`
/// from its 32-byte MSEK — the epoch sibling of
/// [`derive_recipient_hpke_keypair`] (content-sealing-epochs design § 2,
/// `fauna_mls::wrapped_blob::derive_recipient_epoch_hpke_keypair`).
/// Deterministic in `(msek, e)`, so every device holding the same MSEK
/// derives the identical per-epoch keypair with no negotiation.
///
/// Test-only surface today: production nest/client code derives epoch
/// material inside shared Rust below the FFI (B3b's publish leg, B5's
/// holder/drain chain, `MailRecordOpener::open_mail`'s on-demand trial) —
/// no caller needs the raw keypair itself. This export exists so Go tests
/// can construct real epoch-sealed fixtures (mirrors
/// `derive_recipient_hpke_keypair`'s own test-fixture role, e.g.
/// `cmd/seal-helper-testonly`) without duplicating the derivation.
///
/// `msek` must be exactly 32 bytes; otherwise `FfiError::General`.
#[uniffi::export]
pub fn derive_recipient_epoch_hpke_keypair(
    msek: Vec<u8>,
    e: u64,
) -> Result<X25519Keypair, FfiError> {
    let msek = bytes32(&msek, "msek")?;
    let (sk, pk) = fauna_mls::wrapped_blob::derive_recipient_epoch_hpke_keypair(&msek, e);
    Ok(X25519Keypair {
        pubkey: pk.to_vec(),
        secret: sk.to_vec(),
    })
}

/// The post-quantum halves of an owner's standing recipient-mail X-Wing identity,
/// both derived from one 32-byte MSEK (`derive_recipient_mail_xwing_material`):
/// the public `mlkem_ek` the client publishes to enable hybrid inbound mail, and
/// the `capability_secret` a `content.read{mail|calendar}` grant wraps to a holder.
///
/// The `capability_secret` is secret material; UniFFI hands the caller a copy (not
/// zeroized across the boundary), so callers keep its lifetime short.
#[derive(uniffi::Record)]
pub struct RecipientMailXwingMaterial {
    /// 1184-byte ML-KEM-768 encapsulation key — the client publishes this alongside
    /// its X25519 recipient pubkey via `fauna.bridges.provision_recipient_mls_pubkey`
    /// (`mlkem_ek` field, S3c), so the MTA seals inbound mail to it under the X-Wing
    /// suite.
    pub mlkem_ek: Vec<u8>,
    /// The `32 + 2400`-byte `content.read{mail|calendar}` grant payload
    /// (`x25519_secret ∥ mlkem_decaps_key`) — byte-identical to what the client
    /// mint's `derive_scope_payload` wraps (both call the shared
    /// `fauna_mls::wrapped_blob::derive_recipient_mail_capability_secret`). A holder
    /// opens BOTH classical and X-Wing-sealed mail records with it (the 32-vs-2432
    /// length dispatch in [`open_mail_record_with_key`]).
    pub capability_secret: Vec<u8>,
}

/// Derive an owner's standing recipient-mail X-Wing material from its 32-byte MSEK:
/// the 1184-byte `mlkem_ek` to publish (hybrid inbound) and the `32 + 2400`-byte
/// `capability_secret` a `content.read{mail}` grant carries (hybrid-mail drain).
/// Deterministic in `msek` — every device re-derives identical bytes.
///
/// Test-only surface: production clients derive these inside the shared machine mint
/// (`derive_scope_payload`, below the FFI). The `cmd/seal-helper-testonly`
/// `mint-grant` / `derive-recipient-mlkem-ek` modes call this so the tier_3
/// hybrid-mail-drain harness can (a) publish the recipient ek → hybrid inbound and
/// (b) mint an X-Wing grant carrying the superset key, all without the client UI.
///
/// `msek` must be exactly 32 bytes; otherwise `FfiError::General`.
#[uniffi::export]
pub fn derive_recipient_mail_xwing_material(
    msek: Vec<u8>,
) -> Result<RecipientMailXwingMaterial, FfiError> {
    let msek = bytes32(&msek, "msek")?;
    let kp = inner_derive_recipient_xwing_keypair(&msek);
    Ok(RecipientMailXwingMaterial {
        mlkem_ek: kp.public.mlkem_encaps_key().to_vec(),
        capability_secret: inner_derive_recipient_mail_capability_secret(&msek),
    })
}

/// A capability-grant **holder's** derived ML-KEM-768 keypair — the post-quantum
/// half of the bridge service-user's X-Wing identity (PQ-CAP-2). Mirrors the
/// `(decaps_key, encaps_key)` split of
/// `fauna_mls::wrapped_blob::derive_bridge_service_user_mlkem768`.
///
/// The `mlkem_dk` is secret; UniFFI hands the Go side a copy (the bytes are not
/// zeroized across the boundary), so the bridge keeps its lifetime short and
/// re-derives on demand rather than persisting it.
#[derive(uniffi::Record)]
pub struct BridgeServiceUserMlkemKeypair {
    /// 2400-byte ML-KEM-768 decapsulation key — the holder keeps this and passes
    /// it to [`unseal_capability_grant`] to open hybrid (X-Wing) grant wraps.
    pub mlkem_dk: Vec<u8>,
    /// 1184-byte ML-KEM-768 encapsulation key — the holder publishes this at
    /// enrollment via `register_service_user` (`bridge_service_users.mlkem_ek`),
    /// so the client mint can seal grants X-Wing to it.
    pub mlkem_ek: Vec<u8>,
}

/// Derive the bridge service-user's ML-KEM-768 keypair from its 32-byte Ed25519
/// identity seed (the keyfile's `ed25519_seed`), domain-separated by
/// `fauna_mls::wrapped_blob::BRIDGE_SERVICE_USER_MLKEM_DERIVE_CONTEXT` so it
/// never collides with the mail-recipient (`fauna.mail.recipient-mlkem.v1`) or
/// subscription (`fauna.subscription.subscriber-mlkem.v1`) derivations.
///
/// A grant's holder is an enrolled bridge service-user, not an actor, so — like
/// the TLS-cert wrap — it cannot reuse a recipient's MSEK-derived ek and derives
/// its **own** post-quantum key here (design § Post-quantum key publication and
/// derivation → Capability-grant holders). Deterministic in the seed: the bridge
/// re-derives the identical keypair across restarts, so it publishes `mlkem_ek`
/// once at enrollment and re-derives `mlkem_dk` to drain grants forever after.
///
/// `ed25519_seed` must be exactly 32 bytes; otherwise `FfiError::General`.
#[uniffi::export]
pub fn derive_bridge_service_user_mlkem768(
    ed25519_seed: Vec<u8>,
) -> Result<BridgeServiceUserMlkemKeypair, FfiError> {
    if ed25519_seed.len() != 32 {
        return Err(FfiError::General {
            msg: format!("ed25519_seed must be 32 bytes, got {}", ed25519_seed.len()),
        });
    }
    let (dk, ek) = inner_derive_bridge_service_user_mlkem768(&ed25519_seed);
    Ok(BridgeServiceUserMlkemKeypair {
        mlkem_dk: dk.to_vec(),
        mlkem_ek: ek.to_vec(),
    })
}

/// Encode an `MlsSnapshotPlaintext` carrying `keypairs` as its
/// `leaf_init_keypairs` field to canonical DAG-CBOR bytes. The
/// user's primary client calls this on MLS state advance: take the
/// current + last 2 rotation keypairs, hand them to this function,
/// then AEAD-seal the result under the actor's MSEK via
/// `seal_mls_snapshot_blob` and upload via
/// `fauna.bridges.provision_mls_snapshot_blob`.
///
/// Every `keypair.pubkey` and `keypair.secret` must be exactly 32
/// bytes; mismatched length returns `FfiError::General`.
#[uniffi::export]
pub fn encode_mls_snapshot_plaintext_v1(keypairs: Vec<X25519Keypair>) -> Result<Vec<u8>, FfiError> {
    let mut leaf_init_keypairs = Vec::with_capacity(keypairs.len());
    for (i, kp) in keypairs.into_iter().enumerate() {
        if kp.pubkey.len() != 32 {
            return Err(FfiError::General {
                msg: format!(
                    "keypairs[{i}].pubkey must be 32 bytes, got {}",
                    kp.pubkey.len()
                ),
            });
        }
        if kp.secret.len() != 32 {
            return Err(FfiError::General {
                msg: format!(
                    "keypairs[{i}].secret must be 32 bytes, got {}",
                    kp.secret.len()
                ),
            });
        }
        let mut pk = [0u8; 32];
        let mut sk = [0u8; 32];
        pk.copy_from_slice(&kp.pubkey);
        sk.copy_from_slice(&kp.secret);
        leaf_init_keypairs.push(InnerLeafInitKeypair::new(pk, sk));
    }
    let snapshot = MlsSnapshotPlaintext {
        leaf_init_keypairs,
        ..Default::default()
    };
    snapshot
        .to_canonical_bytes()
        .map_err(|e| FfiError::General {
            msg: format!("encode mls-snapshot plaintext: {e}"),
        })
}

/// Encode an `MlsSnapshotPlaintext` from the actor's MSEK history —
/// `mseks[0]` is the current MSEK, further entries are prior generations
/// retained for grace — via the shared production builder
/// (`fauna_mls::wrapped_blob::build_mls_snapshot_plaintext`). Unlike
/// [`encode_mls_snapshot_plaintext_v1`] (explicit keypairs, no epoch
/// material), this includes the per-generation `mail_epoch_grace_roots`
/// the epoch-aware opener's MSEK-rotation grace consumes
/// (content-sealing-epochs design § 5).
///
/// Every entry must be exactly 32 bytes; mismatched length returns
/// `FfiError::General`.
#[uniffi::export]
pub fn encode_mls_snapshot_plaintext_from_mseks(mseks: Vec<Vec<u8>>) -> Result<Vec<u8>, FfiError> {
    let mut arrs = Vec::with_capacity(mseks.len());
    for (i, m) in mseks.iter().enumerate() {
        let a: [u8; 32] = m.as_slice().try_into().map_err(|_| FfiError::General {
            msg: format!("mseks[{i}] must be 32 bytes, got {}", m.len()),
        })?;
        arrs.push(a);
    }
    fauna_mls::wrapped_blob::build_mls_snapshot_plaintext(&arrs, &[])
        .to_canonical_bytes()
        .map_err(|e| FfiError::General {
            msg: format!("encode mls-snapshot plaintext: {e}"),
        })
}

/// Seal a `TlsCertBlob` for nest's `provision_tls_cert_blob`.
///
/// `tls_cert_bundle_canonical_bytes` is the canonical DAG-CBOR encoding
/// of a `TlsCertBundle` (per `libs/fauna-mls/src/wrapped_blob/mod.rs`;
/// design tracked internally, § Per-domain TLS cert blob).
/// `recipient_x25519_pubkey` is the
/// bridge's 32-byte X25519 public key, fetched via
/// `fauna.bridges.fetch_bridge_pubkey(bridge_role, bridge_id)`.
///
/// Argument order: bundle-bytes first, outer-wrap parameters next,
/// recipient last. Both TLS-provisioning paths — the
/// admin-uploaded path (admin client calls this) and the ACME-managed
/// path (nest's ACME pipeline does the equivalent seal in-process) —
/// bottom out at nest's `provision_tls_cert_blob` with the wire bytes
/// returned here. See `docs/goal/behavior/mail-bridge-lifecycle.md`
/// § TLS provisioning (two paths).
#[uniffi::export]
pub fn seal_tls_cert_blob(
    tls_cert_bundle_canonical_bytes: Vec<u8>,
    bridge_role: String,
    bridge_id: String,
    domain: String,
    recipient_x25519_pubkey: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let recipient = bytes32(&recipient_x25519_pubkey, "recipient_x25519_pubkey")?;
    let bundle = InnerTlsCertBundle::from_canonical_bytes(&tls_cert_bundle_canonical_bytes)
        .map_err(|e| FfiError::General {
            msg: format!("decode tls-cert-bundle: {e}"),
        })?;
    let blob = inner_seal_tls_cert(&bundle, &bridge_role, &bridge_id, &domain, &recipient)
        .map_err(|e| FfiError::General {
            msg: format!("seal tls-cert: {e}"),
        })?;
    blob.to_canonical_bytes().map_err(|e| FfiError::General {
        msg: format!("encode tls-cert: {e}"),
    })
}

/// Select the DKIM `d=` signing domain for an outbound message, given its From:
/// header domain and the deployment's active `local_domains` projection, per
/// RFC 6376 §3.6 + `docs/goal/behavior/mail-multidomain.md` § Signing-key
/// selection at outbound time. Returns the chosen local domain — an exact
/// match, else the closest-parent local domain when `from` is a subdomain — or
/// `None` when `from` is not local (and not a subdomain of any local domain),
/// in which case the Go MTA rejects submission with
/// `550 5.7.7 From: domain not local`.
///
/// One spelling of the selection rule across nest, clients, and the Go bridge
/// (priority #2): the Go MTA calls this to decide whether a message's From:
/// domain is one the deployment signs for, and the nest calls the underlying
/// function to pick that domain's signing key at the outbound hand-out. The
/// underlying `fauna_mail::outbound::dkim::select_signing_domain` borrows from
/// `local_domains`; this wrapper takes/returns owned `String`s for UniFFI.
#[uniffi::export]
pub fn select_signing_domain(from: String, local_domains: Vec<String>) -> Option<String> {
    let locals: Vec<&str> = local_domains.iter().map(String::as_str).collect();
    fauna_mail::outbound::dkim::select_signing_domain(&from, &locals)
        .ok()
        .map(str::to_string)
}

// ── Forward-loop detection (mail-forwarding N2 — Go MTA forward stage) ──
//
// The Go MTA's post-delivery forward stage (`mail-forwarding.md` § Trigger
// point) needs the two loop-suppression floors + the stamp builder from
// `fauna_mail::forward_loop` (the R2 (account-data-plane.md § The ratified decisions) shared core). Those pure functions take
// `usize` / `impl IntoIterator<Item = &str>` / `&str` shapes UniFFI cannot
// bind directly, so these thin wrappers adapt the signatures (u64 /
// Vec<String> / String) and bottom out at the shared impl — one spelling of
// the loop-detection rule across nest, clients, and the bridge (priority #2).
//
// SRS (R1, `fauna_mail::srs`) is deliberately NOT exposed here: the N3
// envelope rewrite runs nest-side at queue-out (`fetch_outbound_due`), so the
// per-deployment SRS secret never leaves the nest process. R4
// (`validate_forward_target`) is enforced nest-side at `set_forward_all_to`
// and has no Go-bridge caller. Add either here only when a Go consumer needs
// it.

/// True when an inbound message's `Received:` chain is too long to forward —
/// the forward is suppressed but **local delivery still completes**
/// (`mail-forwarding.md:134`). `received_header_count` is the number of
/// `Received:` header fields on the inbound message.
#[uniffi::export]
pub fn forward_received_chain_exceeded(received_header_count: u64) -> bool {
    fauna_mail::received_chain_exceeded(received_header_count as usize)
}

/// Build the `X-Fauna-Forwarded-By` header **value** (no field name):
/// `actor=<id>; t=<unix>; rule=<rule-id|forward-all>` (`mail-forwarding.md:138-141`).
/// The caller prepends the `X-Fauna-Forwarded-By: ` field name.
#[uniffi::export]
pub fn forward_stamp_value(actor_id: String, unix_time: i64, rule: String) -> String {
    fauna_mail::forwarded_by_value(&actor_id, unix_time, &rule)
}

/// True if any of the inbound message's `X-Fauna-Forwarded-By` header values
/// was stamped by *our own* forwarding actor — re-forwarding would tornado, so
/// suppress the forward (`mail-forwarding.md:146`). A peer's *different*
/// `actor=` does not suppress (we forward through, accreting the chain until
/// the Received: floor wins). `forwarded_by_values` are the values of every
/// `X-Fauna-Forwarded-By` header on the inbound message.
#[uniffi::export]
pub fn forward_self_already_forwarded(
    forwarded_by_values: Vec<String>,
    our_actor_id: String,
) -> bool {
    fauna_mail::self_already_forwarded(
        forwarded_by_values.iter().map(String::as_str),
        &our_actor_id,
    )
}

/// FFI error-type adapter over the shared
/// [`fauna_mls::wrapped_blob::credential_input`]; the credential-kind string
/// contract (`"plain"` / `"oauthbearer"`) lives there (priority #2).
fn credential_input<'a>(kind: &str, bytes: &'a [u8]) -> Result<CredentialInput<'a>, FfiError> {
    fauna_mls::wrapped_blob::credential_input(kind, bytes).map_err(|msg| FfiError::General { msg })
}

/// Resolve `KdfParamsFfi` to the inner `KdfParams`. `None` selects the
/// library default for `kind` (Argon2id Interactive for `"plain"`,
/// HKDF-SHA-256 for `"oauthbearer"`) per
/// `docs/goal/behavior/mail-credentials.md` § KDF choice.
fn resolve_kdf_params(
    params: Option<KdfParamsFfi>,
    credential_kind: &str,
) -> Result<KdfParams, FfiError> {
    match params {
        Some(p) => match p.alg.as_str() {
            "argon2id" => {
                let m = p.argon2_m_kib.ok_or_else(|| FfiError::General {
                    msg: "argon2_m_kib required when alg == \"argon2id\"".into(),
                })?;
                let t = p.argon2_t.ok_or_else(|| FfiError::General {
                    msg: "argon2_t required when alg == \"argon2id\"".into(),
                })?;
                let p_ = p.argon2_p.ok_or_else(|| FfiError::General {
                    msg: "argon2_p required when alg == \"argon2id\"".into(),
                })?;
                Ok(KdfParams::Argon2id(Argon2idParams { m, t, p: p_ }))
            }
            "hkdf-sha256" => Ok(KdfParams::HkdfSha256(HkdfSha256Params)),
            other => Err(FfiError::General {
                msg: format!("unknown kdf alg {other:?}; expected \"argon2id\" or \"hkdf-sha256\""),
            }),
        },
        None => default_kdf_for(credential_kind),
    }
}

/// FFI error-type adapter over the shared
/// [`fauna_mls::wrapped_blob::default_kdf_for`]; the default-KDF-per-credential
/// contract (PLAIN → Argon2id Interactive, OAUTHBEARER → HKDF-SHA-256, coupled
/// to [`Argon2idParams::interactive`]) lives there (priority #2).
fn default_kdf_for(credential_kind: &str) -> Result<KdfParams, FfiError> {
    fauna_mls::wrapped_blob::default_kdf_for(credential_kind)
        .map_err(|msg| FfiError::General { msg })
}

#[cfg(test)]
mod xwing_seal_tests {
    use fauna_mls::wrapped_blob::{
        MailRecordEnvelope, derive_recipient_xwing_keypair, unseal_mail_record,
        unseal_mail_record_hybrid,
    };

    use super::seal_to_recipient_xwing;

    #[test]
    fn seal_to_recipient_xwing_round_trips_via_hybrid_opener() {
        // A deterministic MSEK-derived recipient X-Wing keypair — seal + open use
        // the matched halves, exactly the production derivation.
        let kp = derive_recipient_xwing_keypair(&[0x42u8; 32]);
        let plaintext = b"hybrid mail body".to_vec();

        let bytes = seal_to_recipient_xwing(plaintext.clone(), kp.public.to_bytes().to_vec())
            .expect("seal x-wing");

        let envelope = MailRecordEnvelope::from_canonical_bytes(&bytes).expect("decode envelope");
        let opened = unseal_mail_record_hybrid(
            &envelope,
            kp.secret.x25519_secret(),
            kp.secret.mlkem_decaps_key(),
        )
        .expect("hybrid opener opens an x-wing record");
        assert_eq!(opened, plaintext);

        // The classical opener must REJECT an X-Wing blob (typed error, not a
        // silent mis-decrypt) — the suite is self-describing per blob.
        assert!(
            unseal_mail_record(&envelope, kp.secret.x25519_secret()).is_err(),
            "classical opener rejects an x-wing record"
        );
    }

    #[test]
    fn seal_to_recipient_xwing_rejects_wrong_length_pubkey() {
        let err = seal_to_recipient_xwing(b"x".to_vec(), vec![0u8; 1215])
            .expect_err("1215-byte pubkey must be rejected");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("1216"),
            "error names the required length: {msg}"
        );
    }
}

#[cfg(test)]
mod spam_baseline_aggregate_tests {
    use fauna_mls::wrapped_blob::{generate_x25519_keypair, seal_spam_model_copy};

    use super::{
        SpamBaselineCopyInput, aggregate_spam_model_copies,
        seal_spam_model_copy as ffi_seal_spam_model_copy,
    };

    fn model_bytes(spam_text: &str) -> Vec<u8> {
        // One spam-train over a fresh model — the same mutation primitive the
        // MDA drives (`apply_spam_training` on empty bytes ⇒ fresh model).
        fauna_mail::spam::apply_spam_training(b"", spam_text, true).new_model_bytes
    }

    #[test]
    fn aggregates_openable_copies_and_counts_unreadable() {
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let owner_a = [0xA1u8; 32];
        let owner_b = [0xB2u8; 32];

        let copy_a =
            seal_spam_model_copy(&model_bytes("buy cheap pills"), &owner_a, &holder_pk, None)
                .unwrap()
                .to_canonical_bytes()
                .unwrap();
        let copy_b =
            seal_spam_model_copy(&model_bytes("hot crypto deal"), &owner_b, &holder_pk, None)
                .unwrap()
                .to_canonical_bytes()
                .unwrap();

        let out = aggregate_spam_model_copies(
            vec![
                SpamBaselineCopyInput {
                    owner_actor_id: owner_a.to_vec(),
                    sealed_copy: copy_a,
                },
                SpamBaselineCopyInput {
                    owner_actor_id: owner_b.to_vec(),
                    // The worklist row claims owner B but the blob is owner
                    // A's — a mis-attribution must count unreadable, never a
                    // wrong contributor.
                    sealed_copy: seal_spam_model_copy(
                        &model_bytes("wrong owner"),
                        &owner_a,
                        &holder_pk,
                        None,
                    )
                    .unwrap()
                    .to_canonical_bytes()
                    .unwrap(),
                },
                SpamBaselineCopyInput {
                    owner_actor_id: owner_b.to_vec(),
                    sealed_copy: copy_b,
                },
                SpamBaselineCopyInput {
                    owner_actor_id: owner_b.to_vec(),
                    sealed_copy: vec![0xFF; 40], // garbage bytes
                },
            ],
            holder_sk.to_vec(),
            None,
        )
        .unwrap();

        assert_eq!(out.contributors, 2);
        assert_eq!(out.unreadable, 2);
        let merged = fauna_mail::spam::SpamModel::from_bytes(&out.merged_model)
            .expect("merged model decodes");
        assert_eq!(merged.spam_messages, 2, "both contributors' trains merged");
        // The names the holder submits: exactly the copies that opened, in
        // worklist order — never the mis-attributed or garbage ones.
        assert_eq!(
            out.merged_contributors,
            vec![owner_a.to_vec(), owner_b.to_vec()],
            "merged_contributors names the two opened copies' owners"
        );
    }

    #[test]
    fn empty_and_wrong_key_yield_empty_merge() {
        let (_, holder_pk) = generate_x25519_keypair();
        let (other_sk, _) = generate_x25519_keypair();
        let owner = [0xA1u8; 32];
        let copy = seal_spam_model_copy(&model_bytes("x y z"), &owner, &holder_pk, None)
            .unwrap()
            .to_canonical_bytes()
            .unwrap();

        // Wrong holder secret ⇒ the copy counts unreadable, merged is empty.
        let out = aggregate_spam_model_copies(
            vec![SpamBaselineCopyInput {
                owner_actor_id: owner.to_vec(),
                sealed_copy: copy,
            }],
            other_sk.to_vec(),
            None,
        )
        .unwrap();
        assert_eq!((out.contributors, out.unreadable), (0, 1));
        assert!(out.merged_model.is_empty());
        assert!(
            out.merged_contributors.is_empty(),
            "an unreadable copy is never named"
        );

        // No copies at all ⇒ clean zero result.
        let out = aggregate_spam_model_copies(vec![], other_sk.to_vec(), None).unwrap();
        assert_eq!((out.contributors, out.unreadable), (0, 0));
        assert!(out.merged_model.is_empty());
        assert!(out.merged_contributors.is_empty());
    }

    // The FFI seal-side twin (`seal_spam_model_copy`, the Vec<u8>-in/out UniFFI
    // façade the seal-helper + (b)'s client glue call) produces a copy the
    // merge side opens — the end-to-end seal→drain→merge path in one process,
    // ahead of the tier_3 harness that exercises the same two exports over the
    // wire. Owner-mismatch still counts unreadable through the façade.
    #[test]
    fn ffi_sealed_copy_round_trips_through_aggregate() {
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let owner = [0xC3u8; 32];
        let copy = ffi_seal_spam_model_copy(
            model_bytes("free money now"),
            owner.to_vec(),
            holder_pk.to_vec(),
            None,
        )
        .expect("ffi seal ok");

        let out = aggregate_spam_model_copies(
            vec![SpamBaselineCopyInput {
                owner_actor_id: owner.to_vec(),
                sealed_copy: copy.clone(),
            }],
            holder_sk.to_vec(),
            None,
        )
        .unwrap();
        assert_eq!((out.contributors, out.unreadable), (1, 0));
        assert!(!out.merged_model.is_empty());

        // A worklist row that mis-attributes the same bytes to another owner
        // fails the AAD-bound owner cross-check → unreadable, never a wrong
        // contributor.
        let out = aggregate_spam_model_copies(
            vec![SpamBaselineCopyInput {
                owner_actor_id: [0xD4u8; 32].to_vec(),
                sealed_copy: copy,
            }],
            holder_sk.to_vec(),
            None,
        )
        .unwrap();
        assert_eq!((out.contributors, out.unreadable), (0, 1));

        // A malformed holder pubkey is a typed FFI error, not a panic.
        assert!(
            ffi_seal_spam_model_copy(model_bytes("x"), owner.to_vec(), vec![0u8; 4], None).is_err()
        );
    }
}

#[cfg(test)]
mod bounded_mint_prior_generation_tests {
    use super::{
        BoundedMailPriorGeneration, build_bounded_mail_grant_blob, open_mail_record_with_key,
    };

    /// The FFI mirror threads prior generations into the shared core
    /// (amendment 2026-07-19): a rotation-boundary epoch's single wrap
    /// carries BOTH generations' secrets, and the unwrapped payload opens
    /// content sealed under either root through the same
    /// `open_mail_record_with_key` the Go drain uses.
    #[test]
    fn ffi_bounded_mint_boundary_epoch_payload_opens_both_generations() {
        use fauna_mls::wrapped_blob::{
            GrantBlob, MAIL_SEALING_EPOCH_SECS, derive_mail_epoch_root,
            derive_recipient_epoch_hpke_keypair_from_root, generate_x25519_keypair,
            seal_to_recipient, unseal_capability,
        };
        let instant_in = |e: u64| e * MAIL_SEALING_EPOCH_SECS + 3;
        let old_msek = [0xD1u8; 32];
        let new_msek = [0xD2u8; 32];
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let owner = [0x21u8; 32];

        let blob_bytes = build_bounded_mail_grant_blob(
            owner.to_vec(),
            vec![0x22; 16],
            holder_pk.to_vec(),
            None,
            instant_in(500),
            instant_in(502),
            new_msek.to_vec(),
            vec![BoundedMailPriorGeneration {
                msek: old_msek.to_vec(),
                retired_at_unix: instant_in(501),
            }],
            false,
            None,
        )
        .expect("ffi bounded mint with a prior generation");
        let blob = GrantBlob::from_canonical_bytes(&blob_bytes).expect("decode");

        let boundary = blob
            .wrapped_keys
            .iter()
            .find(|w| w.epoch == Some(501))
            .expect("boundary epoch wrap present");
        let payload = unseal_capability(boundary, &owner, &holder_sk).expect("holder unwraps");

        for (label, msek) in [("old", &old_msek), ("new", &new_msek)] {
            let root = derive_mail_epoch_root(msek);
            let (_sk, pk) = derive_recipient_epoch_hpke_keypair_from_root(&root, 501);
            let env = seal_to_recipient(b"boundary body", &pk).expect("seal");
            let opened = open_mail_record_with_key(
                env.to_canonical_bytes().expect("encode envelope"),
                payload.clone(),
            )
            .unwrap_or_else(|e| panic!("{label}-generation boundary content must open: {e:?}"));
            assert_eq!(opened, b"boundary body");
        }
    }
}

#[cfg(test)]
mod open_with_key_tests {
    use fauna_mls::wrapped_blob::derive_recipient_xwing_keypair;

    use super::{
        generate_x25519_keypair, open_mail_record_with_key, seal_to_recipient,
        seal_to_recipient_xwing,
    };

    #[test]
    fn open_with_key_classical_round_trip_and_wrong_key_fails() {
        let kp = generate_x25519_keypair();
        let plaintext = b"drain re-scores this body".to_vec();
        let envelope =
            seal_to_recipient(plaintext.clone(), kp.pubkey.clone()).expect("seal classical");

        let opened = open_mail_record_with_key(envelope.clone(), kp.secret.clone())
            .expect("32-byte grant key opens a classical record");
        assert_eq!(opened, plaintext);

        let wrong = generate_x25519_keypair();
        assert!(
            open_mail_record_with_key(envelope, wrong.secret).is_err(),
            "a wrong grant key must fail, never mis-decrypt"
        );
    }

    #[test]
    fn open_with_key_hybrid_concat_opens_both_suites() {
        // The 2432-byte x25519∥mlkem_dk payload shape (the content.read{mail}
        // grant contract) opens an X-Wing record AND a classical record —
        // parity with the AUTH'd session's snapshot-driven dispatch.
        let kp = derive_recipient_xwing_keypair(&[0x42u8; 32]);
        let mut concat = kp.secret.x25519_secret().to_vec();
        concat.extend_from_slice(kp.secret.mlkem_decaps_key());

        let plaintext = b"hybrid body".to_vec();
        let hybrid = seal_to_recipient_xwing(plaintext.clone(), kp.public.to_bytes().to_vec())
            .expect("seal x-wing");
        let opened =
            open_mail_record_with_key(hybrid.clone(), concat.clone()).expect("hybrid key opens");
        assert_eq!(opened, plaintext);

        // Classical-sealed record to the same x25519 half also opens.
        let classical = seal_to_recipient(plaintext.clone(), kp.public.x25519_public().to_vec())
            .expect("seal classical");
        let opened =
            open_mail_record_with_key(classical, concat).expect("hybrid key opens classical");
        assert_eq!(opened, plaintext);

        // The bare 32-byte half must NOT open the hybrid record (typed error).
        assert!(
            open_mail_record_with_key(hybrid, kp.secret.x25519_secret().to_vec()).is_err(),
            "classical-only key rejects a hybrid record"
        );
    }

    #[test]
    fn open_with_key_rejects_odd_key_lengths() {
        let kp = generate_x25519_keypair();
        let envelope = seal_to_recipient(b"x".to_vec(), kp.pubkey).expect("seal");
        let err = open_mail_record_with_key(envelope, vec![0u8; 64])
            .expect_err("a 64-byte key is neither shape");
        let msg = format!("{err:?}");
        assert!(msg.contains("2432"), "error names both shapes: {msg}");
    }
}

/// Zero-dependency micro-benchmark for the **Phase-3 perf gate**
/// (tracked internally, § Phasing → Phase 3). It isolates the *one*
/// per-message cost that migrating mail to
/// sealed-both-modes (Phase 3) would newly impose on a **plaintext-mode** box's
/// IMAP FETCH hot path.
///
/// Today (design (b)) a plaintext-mode nest stores mail as literal plaintext and
/// the MDA serves it **pass-through** — the FETCH does *no* crypto
/// (`internal/mda/imap/fetch.go:386`, `pt = ct.EncryptedBody`). Phase 3 would
/// seal both modes, so every FETCH would pay the encrypted-mode
/// `OpenMailRecord` HPKE-open (`fetch.go:388`). This bench measures that open
/// (via the byte-for-byte-equivalent grant-key opener `open_mail_record_with_key`
/// — same envelope decode + HPKE-open + AEAD as the snapshot-driven
/// `OpenMailRecord`) and the symmetric ingest-side `seal_to_recipient*`, for both
/// the classical X25519 and hybrid X-Wing (X25519∥ML-KEM-768) suites across
/// realistic mail sizes. The `clone <size>` baseline is the pure input-memcpy
/// each FFI call pays at the boundary — subtract it to get crypto-only cost.
///
/// `#[ignore]`d: a perf probe, not a CI gate. **Run in RELEASE** — debug-build
/// crypto is 10–100× slower (no SIMD AEAD, overflow checks) and would badly
/// mislead the gate (a debug run reads ~20 ms per 200 KiB open; release is
/// sub-millisecond). Run explicitly:
///   `cargo test -p fauna-ffi --release --lib mail_seal_open_microbench -- --ignored --nocapture`
#[cfg(test)]
mod perf_bench {
    use std::time::Instant;

    use fauna_mls::wrapped_blob::derive_recipient_xwing_keypair;

    use super::{
        generate_x25519_keypair, open_mail_record_with_key, seal_to_recipient,
        seal_to_recipient_xwing,
    };

    const SIZES: &[(usize, &str)] = &[
        (2 * 1024, "2 KiB"),
        (20 * 1024, "20 KiB"),
        (200 * 1024, "200 KiB"),
    ];
    const ITERS: usize = 400;
    const WARMUP: usize = 20;

    /// An RFC-5322-shaped filler body of `n` bytes (AEAD is size-driven, not
    /// content-driven, so the exact bytes don't matter — only the length).
    fn body(n: usize) -> Vec<u8> {
        let alpha = b"abcdefghijklmnopqrstuvwxyz0123456789 \r\n";
        (0..n).map(|i| alpha[i % alpha.len()]).collect()
    }

    /// (mean, p50, p95, p99) in microseconds.
    fn stats_us(mut v: Vec<f64>) -> (f64, f64, f64, f64) {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = v.len();
        let mean = v.iter().sum::<f64>() / n as f64;
        let pct = |p: f64| v[((p * (n as f64 - 1.0)).round() as usize).min(n - 1)];
        (mean, pct(0.50), pct(0.95), pct(0.99))
    }

    fn bench(label: &str, mut f: impl FnMut()) {
        for _ in 0..WARMUP {
            f();
        }
        let mut samples = Vec::with_capacity(ITERS);
        for _ in 0..ITERS {
            let t = Instant::now();
            f();
            samples.push(t.elapsed().as_secs_f64() * 1e6);
        }
        let (mean, p50, p95, p99) = stats_us(samples);
        println!(
            "  {label:<30} mean {mean:>9.2}  p50 {p50:>9.2}  p95 {p95:>9.2}  p99 {p99:>9.2}   (µs)"
        );
    }

    #[test]
    #[ignore = "perf probe; run explicitly with --ignored --nocapture"]
    fn mail_seal_open_microbench() {
        println!("\n=== Phase-3 perf gate: per-message mail seal/open microbench ===");
        println!("ITERS={ITERS} per row; times in microseconds. Plaintext-mode FETCH pays ZERO");
        println!("of the `open_*` cost (pass-through); Phase 3 would add it to every FETCH.\n");

        // Classical X25519 suite.
        let ckp = generate_x25519_keypair();
        for &(n, name) in SIZES {
            let pt = body(n);
            println!("[classical X25519]  body {name}");
            {
                let pt = pt.clone();
                bench("clone (input-memcpy baseline)", move || {
                    let _ = std::hint::black_box(pt.clone());
                });
            }
            {
                let pk = ckp.pubkey.clone();
                let pt = pt.clone();
                bench("seal_to_recipient", move || {
                    let _ = seal_to_recipient(pt.clone(), pk.clone()).unwrap();
                });
            }
            let env = seal_to_recipient(pt.clone(), ckp.pubkey.clone()).unwrap();
            {
                let sk = ckp.secret.clone();
                let env = env.clone();
                bench("open_mail_record_with_key", move || {
                    let _ = open_mail_record_with_key(env.clone(), sk.clone()).unwrap();
                });
            }
        }

        // Hybrid X-Wing suite (X25519 ∥ ML-KEM-768) — the PQ-forward production
        // path once a recipient publishes an ML-KEM key.
        let hkp = derive_recipient_xwing_keypair(&[0x42u8; 32]);
        let hpub = hkp.public.to_bytes().to_vec();
        let mut hconcat = hkp.secret.x25519_secret().to_vec();
        hconcat.extend_from_slice(hkp.secret.mlkem_decaps_key());
        for &(n, name) in SIZES {
            let pt = body(n);
            println!("[hybrid X-Wing]     body {name}");
            {
                let hpub = hpub.clone();
                let pt = pt.clone();
                bench("seal_to_recipient_xwing", move || {
                    let _ = seal_to_recipient_xwing(pt.clone(), hpub.clone()).unwrap();
                });
            }
            let env = seal_to_recipient_xwing(pt.clone(), hpub.clone()).unwrap();
            {
                let key = hconcat.clone();
                let env = env.clone();
                bench("open (hybrid concat key)", move || {
                    let _ = open_mail_record_with_key(env.clone(), key.clone()).unwrap();
                });
            }
        }
        println!();
    }
}

/// The atproto.pds bridge's unseal as the Go side calls it: bytes and strings
/// in, the published-key binding enforced behind the boundary.
#[cfg(test)]
mod atproto_identity_binding_tests {
    use fauna_mls::wrapped_blob::{
        AtprotoIdentityKeyBundle as InnerBundle, generate_x25519_keypair, seal_atproto_identity,
    };

    use super::unseal_atproto_identity_blob;

    fn sealed(actor: [u8; 32], tag: &str, bridge_pk: &[u8; 32]) -> (Vec<u8>, String, String) {
        let bundle = InnerBundle {
            actor_id: actor.to_vec(),
            signing_priv: vec![0xA1; 32],
            signing_curve: "k256".into(),
            signing_pub_did_key: format!("did:key:zQ3sSIGNING{tag}"),
            rotation_priv: vec![0xB2; 32],
            rotation_curve: "k256".into(),
            rotation_pub_did_key: format!("did:key:zQ3sROTATION{tag}"),
            issued_at: 1_700_000_000,
        };
        let blob = seal_atproto_identity(&bundle, &actor, bridge_pk)
            .unwrap()
            .to_canonical_bytes()
            .unwrap();
        (
            blob,
            bundle.signing_pub_did_key.clone(),
            bundle.rotation_pub_did_key.clone(),
        )
    }

    #[test]
    fn another_identitys_whole_blob_is_refused_across_the_ffi() {
        let (sk, pk) = generate_x25519_keypair();
        let (x_blob, x_signing, x_rotation) = sealed([0x33; 32], "X", &pk);
        let (_, y_signing, y_rotation) = sealed([0x44; 32], "Y", &pk);

        let Ok(opened) =
            unseal_atproto_identity_blob(x_blob.clone(), sk.to_vec(), x_signing, x_rotation)
        else {
            panic!("a blob must open against its own identity's published keys");
        };
        assert_eq!(opened.actor_id, vec![0x33; 32]);

        // X's blob, untouched, served where Y's was asked for.
        // `.err().expect` rather than `expect_err`: `AtprotoIdentityKeyBundle`
        // carries private key material and deliberately implements no `Debug`.
        #[allow(clippy::err_expect)]
        let err = unseal_atproto_identity_blob(x_blob.clone(), sk.to_vec(), y_signing, y_rotation)
            .err()
            .expect("X's whole blob must not open as Y's");
        assert!(format!("{err:?}").contains("published keys"), "{err:?}");

        // No published keys on the row is not "no expectation".
        assert!(
            unseal_atproto_identity_blob(x_blob, sk.to_vec(), String::new(), String::new())
                .is_err()
        );
    }
}
