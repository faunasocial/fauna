//! Wrapped-blob crypto ecosystem (design tracked internally).

pub mod aead;
pub mod bounded_mail_mint;
pub mod dav_body;
pub mod envelope;
pub mod format;
pub mod generation_wraps;
pub mod group_generation_wraps;
pub mod kdf;
pub mod lan_cert;
pub mod mail_epoch_open;
pub mod mls_snapshot_plaintext;
pub mod service_user;
pub mod submission_token;
pub mod webdav_keys_plaintext;
pub mod xwing_envelope;

use crate::wrapped_blob::aead::{aead_open, aead_seal};
use crate::wrapped_blob::envelope::{hpke_open, hpke_seal};
use crate::wrapped_blob::kdf::{derive_key, unwrap_key};
use ed25519_dalek::VerifyingKey;
use fauna_pq_kem::XWingSecretKey;
// `pub` so the seal side (`fauna-ffi`'s `seal_to_recipient_xwing` export, the
// nest in-domain path) can parse the published 1216-B X-Wing pubkey into the
// `seal_to_recipient_xwing` param without a direct `fauna-pq-kem` dep —
// symmetric with the `MLKEM768_DECAPS_KEY_LEN` re-export below.
pub use fauna_pq_kem::XWingPublicKey;
use rand::RngCore;
use serde_bytes::ByteBuf;

pub use aead::{AEAD_NONCE_LEN, AEAD_TAG_LEN};
pub use bounded_mail_mint::{
    PriorMsekGeneration, bounded_mail_epoch_wraps, bounded_mail_epoch_wraps_for_range,
    bounded_mail_grant_scopes, build_bounded_mail_grant,
};
pub use envelope::derive_x25519_keypair_from_ikm;
pub use envelope::{
    AEAD_CHACHA20_POLY1305, HPKE_ENC_LEN, KDF_HKDF_SHA256, KEM_X25519_HKDF_SHA256,
    generate_x25519_keypair,
};
pub use format::{
    AadBinding, AtprotoIdentityBlob, AtprotoIdentityIndex, AtprotoIdentityKeyBundle,
    AtprotoIdentityPublishedKeys, AtprotoSessionSecretBlob, AtprotoSessionSecretBundle,
    AtprotoSessionSecretIndex, BLOB_FORMAT_VERSION, ExportSessionKeyBlob, ExportSessionKeyIndex,
    FAUNA_KEM_XWING, FormatVersion, GrantBlob, GrantIndex, GrantWindow, HpkeWire, KemSuite,
    MailRecordEnvelope, MlsSnapshotBlob, MlsSnapshotIndex, ScopeTuple, SealedRecordBytes,
    SeedEscrowBlob, SeedEscrowIndex, SerKdfParams, SpamModelCopyBlob, SpamModelCopyIndex,
    TlsCertBlob, TlsCertBundle, TlsCertIndex, UnwrapError, WebdavKeysBlob, WebdavKeysIndex,
    WrapError, WrappedMsekBlob, WrappedMsekIndex, WrappedScopeKey, WrappedSubmissionTokenBlob,
    custody_scope_set_from_tuples, grant_window_is_open,
};
pub use kdf::{
    ARGON2_VERSION_13, Argon2idParams, CredentialInput, HkdfSha256Params, KdfParams,
    credential_input, default_kdf_for,
};
pub use lan_cert::{
    LAN_TLS_CERT_ENTRY_ID, LanCertError, OpenedLanCert, open_lan_tls_cert_entry,
    seal_lan_tls_cert_entry,
};
pub use mail_epoch_open::{open_mail_epoch_chain, unseal_mail_record_with_derived_key};
pub use mls_snapshot_plaintext::{
    INDEX_SEGMENT_KEY_DERIVE_CONTEXT, LeafInitKeypair, MAIL_EPOCH_PUBLISH_HORIZON,
    MAIL_EPOCH_ROOT_DERIVE_CONTEXT, MAIL_EPOCH_SEALING_WRITE_DEFAULT, MAIL_SEALING_EPOCH_SECS,
    MlsSnapshotPlaintext, RECIPIENT_HPKE_DERIVE_CONTEXT, RECIPIENT_HPKE_EPOCH_DERIVE_CONTEXT,
    RECIPIENT_MLKEM_DERIVE_CONTEXT, RECIPIENT_MLKEM_EPOCH_DERIVE_CONTEXT,
    SNAPSHOT_PLAINTEXT_VERSION, StandingMailKeypair, build_mls_snapshot_plaintext,
    derive_index_segment_key, derive_mail_epoch_root, derive_recipient_epoch_hpke_keypair,
    derive_recipient_epoch_hpke_keypair_from_root, derive_recipient_epoch_xwing_keypair,
    derive_recipient_epoch_xwing_keypair_from_root, derive_recipient_hpke_keypair,
    derive_recipient_mail_capability_secret, derive_recipient_mail_epoch_capability_secret,
    derive_recipient_mail_epoch_capability_secret_from_root, derive_recipient_xwing_keypair,
    derive_standing_mail_keypairs, generation_trial_order, mail_epoch_range_for_window,
    mail_sealing_epoch_of, open_mail_record_standing,
};
pub use service_user::{KEYFILE_FORMAT_VERSION, ServiceUserKeyfile};
pub use submission_token::{SIGNATURE_LEN, SubmissionToken};
pub use webdav_keys_plaintext::{
    ServedSetKeys, WEBDAV_KEYS_PLAINTEXT_VERSION, WebdavKeysPlaintext,
};
pub use xwing_envelope::{XWING_ENC_LEN, xwing_open, xwing_seal};

/// Re-export so readers in `fauna-mail` / `fauna-ffi` can type the hybrid
/// decapsulation-key param without each adding a direct `fauna-pq-kem` dep.
pub use fauna_pq_kem::MLKEM768_DECAPS_KEY_LEN;
/// Length of a published ML-KEM-768 encapsulation key (1184 B), re-exported
/// symmetric with [`MLKEM768_DECAPS_KEY_LEN`] so seal-side callers
/// (`fauna-ffi`, the bridge-service-user derivation) can size the published `ek`
/// without a direct `fauna-pq-kem` dep.
pub use fauna_pq_kem::MLKEM768_ENCAPS_KEY_LEN;
/// Length of the published X-Wing public key (`mlkem_ek[1184] ∥ x25519[32]`),
/// re-exported alongside [`XWingPublicKey`] for the seal-side parsers.
pub use fauna_pq_kem::XWING_ENCAPS_KEY_LEN;

/// Seal a wrapped-MSEK blob.
///
/// Generates a random 16-byte salt and 12-byte nonce, derives the
/// AEAD key from `credential` via the chosen KDF, and AEAD-encrypts
/// `msek` with AAD bound to `(actor_id, credential_id)`.
///
/// # Errors
///
/// Returns `WrapError::KdfFailed` for parameter-out-of-range or
/// underlying derive errors, `WrapError::InvalidInput` for
/// credential/KDF mismatch, `WrapError::AeadFailed` for an AEAD
/// encryption error (practically unreachable).
pub fn seal_wrapped_msek(
    msek: &[u8; 32],
    actor_id: &[u8; 32],
    credential_id: &str,
    credential: &CredentialInput<'_>,
    kdf_params: KdfParams,
) -> Result<WrappedMsekBlob, WrapError> {
    let mut rng = rand::thread_rng();
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; AEAD_NONCE_LEN];
    rng.fill_bytes(&mut salt);
    rng.fill_bytes(&mut nonce);

    let key = derive_key(credential, &salt, actor_id, credential_id, kdf_params)?;
    let aad = AadBinding::for_wrapped_msek(actor_id, credential_id);
    let ciphertext = aead_seal(&key, &nonce, &aad, msek)?;

    Ok(WrappedMsekBlob {
        version: format::BLOB_FORMAT_VERSION,
        kind: "wrapped-msek".into(),
        index: WrappedMsekIndex(actor_id.to_vec(), credential_id.into()),
        kdf: SerKdfParams::from(kdf_params),
        salt: ByteBuf::from(salt.to_vec()),
        nonce: ByteBuf::from(nonce.to_vec()),
        ciphertext: ByteBuf::from(ciphertext),
    })
}

/// Unseal a wrapped-MSEK blob. Returns the 32-byte MSEK on success.
///
/// AEAD failure is mapped to `UnwrapError::AeadFailed` — at the bridge,
/// this is the IMAP/CalDAV authentication-failure signal.
///
/// # Errors
///
/// Returns `UnwrapError::AeadFailed` for AEAD verify failure (the
/// auth signal), `UnwrapError::KdfFailed` for KDF parameter or
/// derivation issues, `UnwrapError::InvalidFormat` for blob shape
/// problems (wrong kind, wrong field lengths, unknown argon2 version).
pub fn unseal_wrapped_msek(
    blob: &WrappedMsekBlob,
    credential: &CredentialInput<'_>,
) -> Result<zeroize::Zeroizing<[u8; 32]>, UnwrapError> {
    if blob.kind != "wrapped-msek" {
        return Err(UnwrapError::InvalidFormat(format!(
            "expected kind=wrapped-msek, got {}",
            blob.kind
        )));
    }
    if blob.index.0.len() != 32 {
        return Err(UnwrapError::InvalidFormat(
            "actor_id must be 32 bytes".into(),
        ));
    }
    if blob.salt.len() != 16 {
        return Err(UnwrapError::InvalidFormat(format!(
            "salt must be 16 bytes, got {}",
            blob.salt.len()
        )));
    }
    if blob.nonce.len() != AEAD_NONCE_LEN {
        return Err(UnwrapError::InvalidFormat(format!(
            "nonce must be {AEAD_NONCE_LEN} bytes, got {}",
            blob.nonce.len()
        )));
    }
    let mut actor = [0u8; 32];
    actor.copy_from_slice(&blob.index.0);

    let kdf_params = KdfParams::try_from(&blob.kdf)?;
    let key = unwrap_key(
        credential,
        blob.salt.as_ref(),
        &actor,
        &blob.index.1,
        kdf_params,
    )?;

    let aad = AadBinding::for_wrapped_msek(&actor, &blob.index.1);
    let mut nonce = [0u8; AEAD_NONCE_LEN];
    nonce.copy_from_slice(&blob.nonce);

    let plaintext =
        zeroize::Zeroizing::new(aead_open(&key, &nonce, &aad, blob.ciphertext.as_ref())?);
    if plaintext.len() != 32 {
        return Err(UnwrapError::InvalidFormat(format!(
            "MSEK plaintext must be 32 bytes, got {}",
            plaintext.len()
        )));
    }
    let mut out = zeroize::Zeroizing::new([0u8; 32]);
    out.copy_from_slice(&plaintext);
    Ok(out)
}

/// Seal an MLS-state snapshot under MSEK. `serialized_state` is the
/// caller-prepared read-only MLS provider snapshot (signing key
/// elided per spec § Snapshot plaintext contents).
///
/// # Errors
///
/// Returns `WrapError::AeadFailed` if AEAD encryption fails
/// (practically unreachable for fixed-shape inputs).
pub fn seal_mls_snapshot(
    serialized_state: &[u8],
    actor_id: &[u8; 32],
    msek: &[u8; 32],
) -> Result<MlsSnapshotBlob, WrapError> {
    let mut rng = rand::thread_rng();
    let mut nonce = [0u8; AEAD_NONCE_LEN];
    rng.fill_bytes(&mut nonce);

    let aad = AadBinding::for_mls_snapshot(actor_id);
    let ciphertext = aead_seal(msek, &nonce, &aad, serialized_state)?;

    Ok(MlsSnapshotBlob {
        version: format::BLOB_FORMAT_VERSION,
        kind: "mls-snapshot".into(),
        index: MlsSnapshotIndex(actor_id.to_vec()),
        nonce: ByteBuf::from(nonce.to_vec()),
        ciphertext: ByteBuf::from(ciphertext),
    })
}

/// Unseal an MLS-state snapshot. Returns the serialized state bytes
/// wrapped in `Zeroizing` so they zero on drop (the snapshot
/// plaintext contains MLS read-side secrets — HPKE init private
/// keys, group/epoch secrets, blob epoch keys — per spec
/// § Snapshot plaintext contents).
///
/// # Errors
///
/// Returns `UnwrapError::AeadFailed` for AEAD verify failure (e.g.
/// wrong MSEK, tampered ciphertext, or AAD substitution).
/// Returns `UnwrapError::InvalidFormat` for blob shape problems
/// (wrong kind, wrong field lengths).
pub fn unseal_mls_snapshot(
    blob: &MlsSnapshotBlob,
    msek: &[u8; 32],
) -> Result<zeroize::Zeroizing<Vec<u8>>, UnwrapError> {
    if blob.kind != "mls-snapshot" {
        return Err(UnwrapError::InvalidFormat(format!(
            "expected kind=mls-snapshot, got {}",
            blob.kind
        )));
    }
    if blob.index.0.len() != 32 {
        return Err(UnwrapError::InvalidFormat(
            "actor_id must be 32 bytes".into(),
        ));
    }
    if blob.nonce.len() != AEAD_NONCE_LEN {
        return Err(UnwrapError::InvalidFormat(format!(
            "nonce must be {AEAD_NONCE_LEN} bytes, got {}",
            blob.nonce.len()
        )));
    }
    let mut actor = [0u8; 32];
    actor.copy_from_slice(&blob.index.0);
    let aad = AadBinding::for_mls_snapshot(&actor);
    let mut nonce = [0u8; AEAD_NONCE_LEN];
    nonce.copy_from_slice(&blob.nonce);
    let plaintext = aead_open(msek, &nonce, &aad, blob.ciphertext.as_ref())?;
    Ok(zeroize::Zeroizing::new(plaintext))
}

/// Seal a WebDAV served-set key blob under MSEK. `plaintext` is the
/// canonically-encoded [`webdav_keys_plaintext::WebdavKeysPlaintext`] the
/// caller prepared (per-served-set content keys). The exact MSEK-sealed sibling
/// of [`seal_mls_snapshot`] — `webdav-server.md` § Key model,
/// `key-material-hierarchy.md` § Path B-sibling-3.
///
/// # Errors
///
/// Returns `WrapError::AeadFailed` if AEAD encryption fails
/// (practically unreachable for fixed-shape inputs).
pub fn seal_webdav_keys_blob(
    plaintext: &[u8],
    actor_id: &[u8; 32],
    msek: &[u8; 32],
) -> Result<WebdavKeysBlob, WrapError> {
    let mut rng = rand::thread_rng();
    let mut nonce = [0u8; AEAD_NONCE_LEN];
    rng.fill_bytes(&mut nonce);

    let aad = AadBinding::for_webdav_keys(actor_id);
    let ciphertext = aead_seal(msek, &nonce, &aad, plaintext)?;

    Ok(WebdavKeysBlob {
        version: format::BLOB_FORMAT_VERSION,
        kind: "webdav-keys".into(),
        index: WebdavKeysIndex(actor_id.to_vec()),
        nonce: ByteBuf::from(nonce.to_vec()),
        ciphertext: ByteBuf::from(ciphertext),
    })
}

/// Unseal a WebDAV served-set key blob. Returns the canonically-encoded
/// [`webdav_keys_plaintext::WebdavKeysPlaintext`] bytes wrapped in `Zeroizing`
/// (the plaintext contains folder content keys — decode then drop). The exact
/// sibling of [`unseal_mls_snapshot`].
///
/// # Errors
///
/// Returns `UnwrapError::AeadFailed` for AEAD verify failure (wrong MSEK,
/// tampered ciphertext, or AAD substitution). Returns `UnwrapError::InvalidFormat`
/// for blob shape problems (wrong kind, wrong field lengths).
pub fn unseal_webdav_keys_blob(
    blob: &WebdavKeysBlob,
    msek: &[u8; 32],
) -> Result<zeroize::Zeroizing<Vec<u8>>, UnwrapError> {
    if blob.kind != "webdav-keys" {
        return Err(UnwrapError::InvalidFormat(format!(
            "expected kind=webdav-keys, got {}",
            blob.kind
        )));
    }
    if blob.index.0.len() != 32 {
        return Err(UnwrapError::InvalidFormat(
            "actor_id must be 32 bytes".into(),
        ));
    }
    if blob.nonce.len() != AEAD_NONCE_LEN {
        return Err(UnwrapError::InvalidFormat(format!(
            "nonce must be {AEAD_NONCE_LEN} bytes, got {}",
            blob.nonce.len()
        )));
    }
    let mut actor = [0u8; 32];
    actor.copy_from_slice(&blob.index.0);
    let aad = AadBinding::for_webdav_keys(&actor);
    let mut nonce = [0u8; AEAD_NONCE_LEN];
    nonce.copy_from_slice(&blob.nonce);
    let plaintext = aead_open(msek, &nonce, &aad, blob.ciphertext.as_ref())?;
    Ok(zeroize::Zeroizing::new(plaintext))
}

/// Seal the 32-byte identity seed into a [`SeedEscrowBlob`] for `actor_id`,
/// HPKE-sealed to the RecoveryKey's derived X25519 public half
/// (`fauna_core::recovery::RecoveryKey::escrow_public`).
///
/// This is the client-side half of the escrow plane: the blob it returns is what
/// `fauna.recovery.escrow.put` stores opaque, and what the phrase alone reopens
/// after total device loss (`identity-succession.md` § Seed escrow).
///
/// **The recipient key must be the escrow half, never the recovery root's
/// signing half** — they are domain-separated derivations of the same root
/// (`RECOVERY_ESCROW_X25519_CONTEXT`), and sealing to the wrong one produces a
/// blob no kit can open. `fauna-mls` deliberately does not depend on
/// `fauna-core`'s `RecoveryKey`, so the caller passes the bytes;
/// `tests/recovery_escrow_interop.rs` pins that the two crates agree.
///
/// # Errors
///
/// Returns `WrapError::HpkeFailed` for HPKE seal failure (including an
/// unusable recipient public key).
pub fn seal_seed_escrow(
    identity_seed: &[u8; 32],
    actor_id: &[u8; 32],
    recovery_escrow_pubkey: &[u8; 32],
) -> Result<SeedEscrowBlob, WrapError> {
    seal_seed_escrow_with_predecessors(identity_seed, actor_id, recovery_escrow_pubkey, &[])
}

/// One predecessor identity's seed, carried in a successor's escrow blob
/// during the corpus re-seal window (`identity-succession.md` § Seed escrow).
/// Fixed arrays at the API (the wire shape is
/// [`format::PredecessorSeedEntry`]); no `Debug` derive — the seed is an
/// identity secret.
#[derive(Clone)]
pub struct PredecessorSeed {
    pub actor_id: [u8; 32],
    pub seed: [u8; 32],
}

/// [`seal_seed_escrow`] with the predecessor section (ratified 2026-08-03):
/// the successor's kit ceremony passes the predecessor seed(s) so a
/// total-device-loss restore inside the re-seal window recovers the account
/// *and* the material to open the not-yet-re-sealed corpus. An empty slice
/// seals exactly the classic blob — no `pred` key in the encoding at all.
pub fn seal_seed_escrow_with_predecessors(
    identity_seed: &[u8; 32],
    actor_id: &[u8; 32],
    recovery_escrow_pubkey: &[u8; 32],
    predecessors: &[PredecessorSeed],
) -> Result<SeedEscrowBlob, WrapError> {
    let info = AadBinding::for_seed_escrow(actor_id);
    let aad = AadBinding::for_seed_escrow(actor_id);
    let (enc, ciphertext) = hpke_seal(recovery_escrow_pubkey, &info, &aad, identity_seed)?;

    let pred = if predecessors.is_empty() {
        None
    } else {
        let list = format::PredecessorSeedList(
            predecessors
                .iter()
                .map(|p| format::PredecessorSeedEntry {
                    actor_id: ByteBuf::from(p.actor_id.to_vec()),
                    seed: ByteBuf::from(p.seed.to_vec()),
                })
                .collect(),
        );
        let mut plaintext = fauna_cbor::encode_canonical(&list)
            .map_err(|e| WrapError::InvalidInput(format!("predecessor list encode: {e}")))?;
        let p_info = AadBinding::for_seed_escrow_predecessors(actor_id);
        let p_aad = AadBinding::for_seed_escrow_predecessors(actor_id);
        let (p_enc, p_ciphertext) = hpke_seal(recovery_escrow_pubkey, &p_info, &p_aad, &plaintext)?;
        zeroize::Zeroize::zeroize(&mut plaintext);
        Some(HpkeWire {
            kem_suite: KemSuite::STANDARD,
            enc: ByteBuf::from(p_enc),
            ciphertext: ByteBuf::from(p_ciphertext),
        })
    };

    Ok(SeedEscrowBlob {
        version: format::BLOB_FORMAT_VERSION,
        kind: SeedEscrowBlob::KIND.into(),
        index: SeedEscrowIndex(ByteBuf::from(actor_id.to_vec())),
        hpke: HpkeWire {
            kem_suite: KemSuite::STANDARD,
            enc: ByteBuf::from(enc),
            ciphertext: ByteBuf::from(ciphertext),
        },
        pred,
    })
}

/// Unseal a [`SeedEscrowBlob`] with the RecoveryKey's derived X25519 secret,
/// returning the 32-byte identity seed zeroized on drop.
///
/// **The AAD comes from the blob's own index, and the recovered seed is checked
/// to be 32 bytes.** Both matter: the AAD binding is what makes a blob stolen
/// from one account's row unopenable as another's, and the length check keeps a
/// malformed plaintext from being handed on as an identity secret.
///
/// # Errors
///
/// - `UnwrapError::InvalidFormat` — wrong `kind`, malformed actor id, or a
///   plaintext that is not exactly 32 bytes.
/// - `UnwrapError::HpkeFailed` — wrong recovery root, tampered ciphertext, or an
///   AAD/info mismatch (the cross-account substitution attempt).
pub fn unseal_seed_escrow(
    blob: &SeedEscrowBlob,
    recovery_escrow_secret: &[u8; 32],
) -> Result<zeroize::Zeroizing<[u8; 32]>, UnwrapError> {
    if blob.kind != SeedEscrowBlob::KIND {
        return Err(UnwrapError::InvalidFormat(format!(
            "expected kind={}, got {}",
            SeedEscrowBlob::KIND,
            blob.kind
        )));
    }
    if blob.index.0.len() != 32 {
        return Err(UnwrapError::InvalidFormat(
            "actor_id must be 32 bytes".into(),
        ));
    }
    let mut actor = [0u8; 32];
    actor.copy_from_slice(&blob.index.0);
    let info = AadBinding::for_seed_escrow(&actor);
    let aad = AadBinding::for_seed_escrow(&actor);

    let plaintext = hpke_open(
        recovery_escrow_secret,
        &info,
        &aad,
        blob.hpke.enc.as_ref(),
        blob.hpke.ciphertext.as_ref(),
    )?;
    let seed: [u8; 32] = plaintext.as_slice().try_into().map_err(|_| {
        UnwrapError::InvalidFormat(format!(
            "escrow plaintext must be a 32-byte identity seed, got {}",
            plaintext.len()
        ))
    })?;
    Ok(zeroize::Zeroizing::new(seed))
}

/// A predecessor entry as recovered from the section, seed zeroized on drop.
pub struct PredecessorSeedOpened {
    pub actor_id: [u8; 32],
    pub seed: zeroize::Zeroizing<[u8; 32]>,
}

/// What the predecessor section yielded — deliberately three-valued, so a
/// surface can tell "no section" from "section destroyed" instead of
/// flattening both to an empty list.
pub enum PredecessorsOutcome {
    /// The blob carries no predecessor section (every pre-succession kit, and
    /// every blob re-put after the corpus re-seal completed).
    Absent,
    /// The section opened; entries in the order they were sealed.
    Opened(Vec<PredecessorSeedOpened>),
    /// A section is present but did not open or parse — tampering or
    /// corruption. The primary seed is unaffected (the auxiliary must never
    /// cost the account its restore), but surfaces must REPORT this rather
    /// than silently losing predecessor-corpus access.
    Unreadable(UnwrapError),
}

/// What [`unseal_seed_escrow_with_predecessors`] recovers.
pub struct OpenedSeedEscrow {
    /// The account's identity seed — the primary, exactly what
    /// [`unseal_seed_escrow`] returns.
    pub seed: zeroize::Zeroizing<[u8; 32]>,
    pub predecessors: PredecessorsOutcome,
}

/// [`unseal_seed_escrow`], plus the predecessor section where one rides
/// (`identity-succession.md` § Seed escrow). The primary seed's failure modes
/// are unchanged; a broken predecessor section degrades to
/// [`PredecessorsOutcome::Unreadable`] rather than failing the restore —
/// never let the auxiliary destroy the primary.
pub fn unseal_seed_escrow_with_predecessors(
    blob: &SeedEscrowBlob,
    recovery_escrow_secret: &[u8; 32],
) -> Result<OpenedSeedEscrow, UnwrapError> {
    let seed = unseal_seed_escrow(blob, recovery_escrow_secret)?;
    let Some(pred) = &blob.pred else {
        return Ok(OpenedSeedEscrow {
            seed,
            predecessors: PredecessorsOutcome::Absent,
        });
    };
    // The index length was validated by the primary open above.
    let mut actor = [0u8; 32];
    actor.copy_from_slice(&blob.index.0);
    let info = AadBinding::for_seed_escrow_predecessors(&actor);
    let aad = AadBinding::for_seed_escrow_predecessors(&actor);
    let predecessors = match hpke_open(
        recovery_escrow_secret,
        &info,
        &aad,
        pred.enc.as_ref(),
        pred.ciphertext.as_ref(),
    ) {
        Err(e) => PredecessorsOutcome::Unreadable(e),
        Ok(plaintext) => {
            let plaintext = zeroize::Zeroizing::new(plaintext);
            match fauna_cbor::decode_strict::<format::PredecessorSeedList>(&plaintext) {
                Err(e) => PredecessorsOutcome::Unreadable(UnwrapError::InvalidFormat(format!(
                    "predecessor list decode: {e}"
                ))),
                Ok(list) => {
                    let mut opened = Vec::with_capacity(list.0.len());
                    let mut bad = None;
                    for entry in &list.0 {
                        let (Ok(actor_id), Ok(seed_arr)) = (
                            <[u8; 32]>::try_from(entry.actor_id.as_slice()),
                            <[u8; 32]>::try_from(entry.seed.as_slice()),
                        ) else {
                            bad = Some(UnwrapError::InvalidFormat(
                                "predecessor entry fields must be 32 bytes".into(),
                            ));
                            break;
                        };
                        opened.push(PredecessorSeedOpened {
                            actor_id,
                            seed: zeroize::Zeroizing::new(seed_arr),
                        });
                    }
                    match bad {
                        Some(e) => PredecessorsOutcome::Unreadable(e),
                        None => PredecessorsOutcome::Opened(opened),
                    }
                }
            }
        }
    };
    Ok(OpenedSeedEscrow { seed, predecessors })
}

/// Seal a wrapped-submission-token blob.
///
/// `token` MUST already be signed by the user's primary client.
/// Sealing only adds the credential-derived AEAD layer.
///
/// # Errors
///
/// Returns `WrapError::InvalidInput` if `token.actor_id` or
/// `token.credential_id` don't match the wrap's parameters.
/// Returns `WrapError::KdfFailed` for KDF parameter or derivation
/// errors. Returns `WrapError::CborEncode` if the inner token
/// encode fails. Returns `WrapError::AeadFailed` for an AEAD
/// encryption error.
pub fn seal_submission_token(
    token: &SubmissionToken,
    actor_id: &[u8; 32],
    credential_id: &str,
    credential: &CredentialInput<'_>,
    kdf_params: KdfParams,
) -> Result<WrappedSubmissionTokenBlob, WrapError> {
    if token.actor_id != actor_id {
        return Err(WrapError::InvalidInput(
            "token.actor_id must match the wrap's actor_id".into(),
        ));
    }
    if token.credential_id != credential_id {
        return Err(WrapError::InvalidInput(
            "token.credential_id must match the wrap's credential_id".into(),
        ));
    }

    let mut rng = rand::thread_rng();
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; AEAD_NONCE_LEN];
    rng.fill_bytes(&mut salt);
    rng.fill_bytes(&mut nonce);

    let key = derive_key(credential, &salt, actor_id, credential_id, kdf_params)?;
    let aad = AadBinding::for_submission_token(actor_id, credential_id);
    let plaintext = token.to_canonical_bytes()?;
    let ciphertext = aead_seal(&key, &nonce, &aad, &plaintext)?;

    Ok(WrappedSubmissionTokenBlob {
        version: format::BLOB_FORMAT_VERSION,
        kind: "submission-token".into(),
        index: WrappedMsekIndex(actor_id.to_vec(), credential_id.into()),
        kdf: SerKdfParams::from(kdf_params),
        salt: ByteBuf::from(salt.to_vec()),
        nonce: ByteBuf::from(nonce.to_vec()),
        ciphertext: ByteBuf::from(ciphertext),
    })
}

/// Unseal a submission token. Verifies (a) AEAD, (b) the embedded
/// Ed25519 signature against `user_signing_pubkey`. Returns the
/// SubmissionToken plaintext on full success.
///
/// # Errors
///
/// - `UnwrapError::InvalidFormat` for blob shape problems (wrong
///   kind, wrong field lengths, bad inner CBOR).
/// - `UnwrapError::KdfFailed` for KDF derivation issues.
/// - `UnwrapError::AeadFailed` for AEAD verify failure (the auth signal).
/// - `UnwrapError::SignatureFailed` for Ed25519 signature verify
///   failure on the inner SubmissionToken.
pub fn unseal_submission_token(
    blob: &WrappedSubmissionTokenBlob,
    credential: &CredentialInput<'_>,
    user_signing_pubkey: &VerifyingKey,
) -> Result<SubmissionToken, UnwrapError> {
    if blob.kind != "submission-token" {
        return Err(UnwrapError::InvalidFormat(format!(
            "expected kind=submission-token, got {}",
            blob.kind
        )));
    }
    if blob.index.0.len() != 32 {
        return Err(UnwrapError::InvalidFormat(
            "actor_id must be 32 bytes".into(),
        ));
    }
    if blob.salt.len() != 16 {
        return Err(UnwrapError::InvalidFormat(format!(
            "salt must be 16 bytes, got {}",
            blob.salt.len()
        )));
    }
    if blob.nonce.len() != AEAD_NONCE_LEN {
        return Err(UnwrapError::InvalidFormat(format!(
            "nonce must be {AEAD_NONCE_LEN} bytes, got {}",
            blob.nonce.len()
        )));
    }
    let mut actor = [0u8; 32];
    actor.copy_from_slice(&blob.index.0);

    let kdf_params = KdfParams::try_from(&blob.kdf)?;
    let key = unwrap_key(
        credential,
        blob.salt.as_ref(),
        &actor,
        &blob.index.1,
        kdf_params,
    )?;

    let aad = AadBinding::for_submission_token(&actor, &blob.index.1);
    let mut nonce = [0u8; AEAD_NONCE_LEN];
    nonce.copy_from_slice(&blob.nonce);

    let plaintext = aead_open(&key, &nonce, &aad, blob.ciphertext.as_ref())?;
    let token = SubmissionToken::from_canonical_bytes(&plaintext)?;
    token.verify(user_signing_pubkey)?;
    Ok(token)
}

/// Open an [`HpkeWire`] blob, **dispatching on its self-describing KEM suite**
/// (`ks`) — the crypto-agility contract (goal
/// `architecture/security/post-quantum.md` § 7.1).
///
/// - **Classical** suite ([`KemSuite::is_standard`]) → today's X25519 HPKE path.
/// - **X-Wing** ([`FAUNA_KEM_XWING`] with the pinned HKDF-SHA-256 / ChaCha20-
///   Poly1305 kdf/aead) → the hybrid path, **iff** the recipient's
///   ML-KEM decapsulation key is supplied (`recipient_mlkem_dk = Some`). When
///   `None` (a classical-only opener met a hybrid blob) it is a *typed*
///   [`UnwrapError::InvalidFormat`] naming the hybrid opener, never a silent
///   AEAD mis-decrypt.
/// - **Unknown** suite → typed `InvalidFormat` naming the ids.
///
/// This is the single agility dispatch point for every HPKE-wrapped blob
/// (`MailRecordEnvelope`, `TlsCertBlob`); the X-Wing arm lives
/// **here and nowhere else**. The TLS-cert opener passes `None` until that
/// surface gains hybrid (S6); the mail reader passes `Some` via
/// [`unseal_mail_record_hybrid`].
fn hpke_open_dispatch(
    hpke: &HpkeWire,
    recipient_secret: &[u8; 32],
    recipient_mlkem_dk: Option<&[u8; MLKEM768_DECAPS_KEY_LEN]>,
    info: &AadBinding,
    aad: &AadBinding,
) -> Result<Vec<u8>, UnwrapError> {
    let ks = hpke.kem_suite;
    if ks.is_standard() {
        hpke_open(
            recipient_secret,
            info,
            aad,
            hpke.enc.as_ref(),
            hpke.ciphertext.as_ref(),
        )
    } else if ks.kem == FAUNA_KEM_XWING
        && ks.kdf == KDF_HKDF_SHA256
        && ks.aead == AEAD_CHACHA20_POLY1305
    {
        // The X-Wing envelope pins kdf=HKDF-SHA-256 / aead=ChaCha20Poly1305
        // (see `xwing_envelope.rs`), so require all three suite fields here —
        // mirroring the classical `is_standard()` gate — rather than routing on
        // `kem` alone. A kem-matched but kdf/aead-mismatched descriptor falls
        // through to the typed unknown-suite error below, never the X-Wing
        // open. (PQ-1)
        let Some(mlkem_dk) = recipient_mlkem_dk else {
            return Err(UnwrapError::InvalidFormat(
                "X-Wing hybrid blob requires the recipient's ML-KEM decapsulation \
                 key; open it with unseal_mail_record_hybrid"
                    .into(),
            ));
        };
        // The X25519 half is the recipient's existing recipient-mail key
        // (reused, not minted); the ML-KEM half is the MSEK-derived dk.
        let sk = XWingSecretKey::from_parts(*mlkem_dk, *recipient_secret);
        xwing_open(&sk, info, aad, hpke.enc.as_ref(), hpke.ciphertext.as_ref())
    } else {
        Err(UnwrapError::InvalidFormat(format!(
            "unknown HPKE KEM suite: kem=0x{:04x} kdf=0x{:04x} aead=0x{:04x}",
            ks.kem, ks.kdf, ks.aead
        )))
    }
}

/// Seal a per-user ATProto identity-key bundle to the `atproto.pds` bridge's
/// attested X25519 pubkey (the bridge-custodied half of the key-custody split —
/// `atproto-pds-bridge.md` § State & data shape; the user-custodied senior
/// rotation key never passes through here).
///
/// # Errors
///
/// Returns `WrapError::InvalidInput` if `bundle.actor_id` doesn't match
/// `actor_id` (the AAD index) or isn't 32 bytes.
/// Returns `WrapError::HpkeFailed` for HPKE seal failure.
/// Returns `WrapError::CborEncode` if the bundle CBOR encode fails.
pub fn seal_atproto_identity(
    bundle: &AtprotoIdentityKeyBundle,
    actor_id: &[u8; 32],
    recipient_x25519_pubkey: &[u8; 32],
) -> Result<AtprotoIdentityBlob, WrapError> {
    if bundle.actor_id.as_slice() != actor_id.as_slice() {
        return Err(WrapError::InvalidInput(
            "bundle actor_id must match the wrap's actor_id".into(),
        ));
    }
    let info = AadBinding::for_atproto_identity(actor_id);
    let aad = AadBinding::for_atproto_identity(actor_id);
    let plaintext = bundle.to_canonical_bytes()?;
    let (enc, ciphertext) = hpke_seal(recipient_x25519_pubkey, &info, &aad, &plaintext)?;

    Ok(AtprotoIdentityBlob {
        version: format::BLOB_FORMAT_VERSION,
        kind: AtprotoIdentityBlob::KIND.into(),
        index: AtprotoIdentityIndex(ByteBuf::from(actor_id.to_vec())),
        hpke: HpkeWire {
            kem_suite: KemSuite::STANDARD,
            enc: ByteBuf::from(enc),
            ciphertext: ByteBuf::from(ciphertext),
        },
    })
}

/// Unseal an ATProto identity-key blob with the bridge's X25519 secret, and
/// refuse it unless the keys inside are the identity's PUBLISHED keys.
///
/// Two bindings, and they detect different things:
///
/// - **The index binding** (the AAD is built from the blob's OWN plaintext
///   index) detects an *edited* index and nothing more. A whole blob moved
///   under another identity carries a self-consistent index and opens.
/// - **The published-key binding** (`expected`) is what refuses that whole-blob
///   substitution: the bundle's two `did:key` strings must equal the ones the
///   identity row records. It is deliberately a binding to the *keys* and not
///   to an actor id — see [`AtprotoIdentityPublishedKeys`] for why an actor
///   comparison would strand every succeeded account.
///
/// What this closes is a misrouted or row-level-substituted blob (a routing
/// bug, a swapped `atproto_identity_key_blobs` row). It is NOT a defence
/// against a hostile nest: `expected` is nest-sourced at every consumer, and
/// the nest minted these keys in the first place.
///
/// # Errors
///
/// - `UnwrapError::InvalidFormat` for blob shape problems (wrong kind, wrong
///   index length).
/// - `UnwrapError::HpkeFailed` for HPKE open failure (wrong recipient secret,
///   edited index, tampered ciphertext).
/// - `UnwrapError::PublishedKeyMismatch` when the blob opens but its keys are
///   not `expected` — including an `expected` with an empty half, which fails
///   closed rather than reading as "no expectation".
pub fn unseal_atproto_identity(
    blob: &AtprotoIdentityBlob,
    recipient_x25519_secret: &[u8; 32],
    expected: &AtprotoIdentityPublishedKeys<'_>,
) -> Result<AtprotoIdentityKeyBundle, UnwrapError> {
    if blob.kind != AtprotoIdentityBlob::KIND {
        return Err(UnwrapError::InvalidFormat(format!(
            "expected kind={}, got {}",
            AtprotoIdentityBlob::KIND,
            blob.kind
        )));
    }
    let actor_id: [u8; 32] = blob.index.0.as_slice().try_into().map_err(|_| {
        UnwrapError::InvalidFormat(format!(
            "actor_id index must be 32 bytes, got {}",
            blob.index.0.len()
        ))
    })?;
    let info = AadBinding::for_atproto_identity(&actor_id);
    let aad = AadBinding::for_atproto_identity(&actor_id);
    // Classical-only like the TLS cert blob; PQ hybridization rides the same
    // future surface-A uniformity pass.
    let plaintext = hpke_open_dispatch(&blob.hpke, recipient_x25519_secret, None, &info, &aad)?;
    let bundle = AtprotoIdentityKeyBundle::from_canonical_bytes(&plaintext)?;
    // The pubkeys are public, so the comparison needs no constant time; the
    // bundle (and its scalars) zeroes on drop on every refusal below.
    if expected.signing_pub_did_key.is_empty() || expected.rotation_pub_did_key.is_empty() {
        return Err(UnwrapError::PublishedKeyMismatch(
            "the identity row records no published keys to bind the blob to".into(),
        ));
    }
    if bundle.signing_pub_did_key != expected.signing_pub_did_key {
        return Err(UnwrapError::PublishedKeyMismatch(format!(
            "signing key: blob holds {}, identity publishes {}",
            bundle.signing_pub_did_key, expected.signing_pub_did_key
        )));
    }
    if bundle.rotation_pub_did_key != expected.rotation_pub_did_key {
        return Err(UnwrapError::PublishedKeyMismatch(format!(
            "bridge rotation key: blob holds {}, identity publishes {}",
            bundle.rotation_pub_did_key, expected.rotation_pub_did_key
        )));
    }
    Ok(bundle)
}

/// Seal the bridge-wide ATProto session-token secret to the `atproto.pds`
/// bridge's attested X25519 pubkey (`atproto-pds-full.md` § Key material
/// inventory — bridge-wide sealed material, the TLS-cert discipline; nest
/// discards the plaintext after sealing).
///
/// # Errors
///
/// Returns `WrapError::InvalidInput` if `bundle.secret` isn't 32 bytes.
/// Returns `WrapError::HpkeFailed` for HPKE seal failure.
/// Returns `WrapError::CborEncode` if the bundle CBOR encode fails.
pub fn seal_atproto_session_secret(
    bundle: &AtprotoSessionSecretBundle,
    bridge_role: &str,
    bridge_id: &str,
    recipient_x25519_pubkey: &[u8; 32],
) -> Result<AtprotoSessionSecretBlob, WrapError> {
    if bundle.secret.len() != 32 {
        return Err(WrapError::InvalidInput(format!(
            "session secret must be 32 bytes, got {}",
            bundle.secret.len()
        )));
    }
    let info = AadBinding::for_atproto_session_secret(bridge_role, bridge_id);
    let aad = AadBinding::for_atproto_session_secret(bridge_role, bridge_id);
    let plaintext = bundle.to_canonical_bytes()?;
    let (enc, ciphertext) = hpke_seal(recipient_x25519_pubkey, &info, &aad, &plaintext)?;

    Ok(AtprotoSessionSecretBlob {
        version: format::BLOB_FORMAT_VERSION,
        kind: AtprotoSessionSecretBlob::KIND.into(),
        index: AtprotoSessionSecretIndex(bridge_role.into(), bridge_id.into()),
        hpke: HpkeWire {
            kem_suite: KemSuite::STANDARD,
            enc: ByteBuf::from(enc),
            ciphertext: ByteBuf::from(ciphertext),
        },
    })
}

/// Unseal an ATProto session-secret blob with the bridge's X25519 secret.
///
/// # Errors
///
/// - `UnwrapError::InvalidFormat` for blob shape problems (wrong kind), or a
///   bundle whose secret isn't 32 bytes.
/// - `UnwrapError::HpkeFailed` for HPKE open failure (wrong recipient secret,
///   bridge-substitution attempt, tampered ciphertext).
pub fn unseal_atproto_session_secret(
    blob: &AtprotoSessionSecretBlob,
    recipient_x25519_secret: &[u8; 32],
) -> Result<AtprotoSessionSecretBundle, UnwrapError> {
    if blob.kind != AtprotoSessionSecretBlob::KIND {
        return Err(UnwrapError::InvalidFormat(format!(
            "expected kind={}, got {}",
            AtprotoSessionSecretBlob::KIND,
            blob.kind
        )));
    }
    let info = AadBinding::for_atproto_session_secret(&blob.index.0, &blob.index.1);
    let aad = AadBinding::for_atproto_session_secret(&blob.index.0, &blob.index.1);
    // Classical-only like the TLS-cert/identity blobs; PQ hybridization rides
    // the same future surface-A uniformity pass.
    let plaintext = hpke_open_dispatch(&blob.hpke, recipient_x25519_secret, None, &info, &aad)?;
    let bundle = AtprotoSessionSecretBundle::from_canonical_bytes(&plaintext)?;
    if bundle.secret.len() != 32 {
        return Err(UnwrapError::InvalidFormat(format!(
            "session secret must be 32 bytes, got {}",
            bundle.secret.len()
        )));
    }
    Ok(bundle)
}

/// Seal a TLS cert bundle to a specific bridge's X25519 pubkey.
///
/// # Errors
///
/// Returns `WrapError::HpkeFailed` for HPKE seal failure.
/// Returns `WrapError::CborEncode` if the bundle CBOR encode fails.
pub fn seal_tls_cert(
    bundle: &TlsCertBundle,
    bridge_role: &str,
    bridge_id: &str,
    domain: &str,
    recipient_x25519_pubkey: &[u8; 32],
) -> Result<TlsCertBlob, WrapError> {
    let info = AadBinding::for_tls_cert(bridge_role, bridge_id, domain);
    let aad = AadBinding::for_tls_cert(bridge_role, bridge_id, domain);
    let plaintext = bundle.to_canonical_bytes()?;
    let (enc, ciphertext) = hpke_seal(recipient_x25519_pubkey, &info, &aad, &plaintext)?;

    Ok(TlsCertBlob {
        version: format::BLOB_FORMAT_VERSION,
        kind: "tls-cert".into(),
        index: TlsCertIndex(bridge_role.into(), bridge_id.into(), domain.into()),
        hpke: HpkeWire {
            kem_suite: KemSuite::STANDARD,
            enc: ByteBuf::from(enc),
            ciphertext: ByteBuf::from(ciphertext),
        },
    })
}

/// Unseal a TLS cert blob with the bridge's X25519 secret.
///
/// # Errors
///
/// - `UnwrapError::InvalidFormat` for blob shape problems (wrong
///   kind, wrong enc length).
/// - `UnwrapError::HpkeFailed` for HPKE open failure (wrong
///   recipient secret, info mismatch from substitution attempt,
///   tampered ciphertext).
pub fn unseal_tls_cert(
    blob: &TlsCertBlob,
    recipient_x25519_secret: &[u8; 32],
) -> Result<TlsCertBundle, UnwrapError> {
    if blob.kind != "tls-cert" {
        return Err(UnwrapError::InvalidFormat(format!(
            "expected kind=tls-cert, got {}",
            blob.kind
        )));
    }
    let info = AadBinding::for_tls_cert(&blob.index.0, &blob.index.1, &blob.index.2);
    let aad = AadBinding::for_tls_cert(&blob.index.0, &blob.index.1, &blob.index.2);
    // TLS-cert blobs are classical-only until surface-A uniformity (S6); no ML-KEM key.
    let plaintext = hpke_open_dispatch(&blob.hpke, recipient_x25519_secret, None, &info, &aad)?;
    TlsCertBundle::from_canonical_bytes(&plaintext)
}

/// HKDF domain separator for a **capability-grant holder's** ML-KEM-768 keypair
/// (post-quantum overlay, PQ-CAP-2). A grant's holder is an enrolled bridge
/// service-user — **not** an actor — so, exactly like the TLS-cert wrap, it
/// cannot reuse a recipient's MSEK-derived ek: it derives its **own** ML-KEM
/// keypair from the bridge's Ed25519 identity seed via
/// [`fauna_pq_kem::derive_mlkem768_keypair_from_ikm`]`(ed25519_seed, this)` and
/// publishes the encapsulation key at service-user enrollment
/// (`bridge_service_users.mlkem_ek` + the `register_service_user` field). The
/// context is **distinct** from mail's [`RECIPIENT_MLKEM_DERIVE_CONTEXT`]
/// (`fauna.mail.recipient-mlkem.v1`) and subscriptions'
/// `SUBSCRIBER_MLKEM_DERIVE_CONTEXT` (`fauna.subscription.subscriber-mlkem.v1`)
/// so the three derivations never collide even off the same seed material.
/// Authority: `docs/goal/architecture/security/post-quantum.md` § Post-quantum
/// key publication and derivation (Capability-grant holders). The actual
/// derivation lives in `fauna-ffi` (the Go bridge is the sole deriver, via FFI);
/// this const is the single shared source of the domain separator.
pub const BRIDGE_SERVICE_USER_MLKEM_DERIVE_CONTEXT: &str = "fauna.bridge.service-user-mlkem.v1";

/// Derive a capability-grant **holder's** ML-KEM-768 keypair from the bridge
/// service-user's Ed25519 identity seed, domain-separated by
/// [`BRIDGE_SERVICE_USER_MLKEM_DERIVE_CONTEXT`]. Returns `(decaps_key,
/// encaps_key)` — the holder keeps the 2400-byte `dk` to open hybrid grant wraps
/// ([`unseal_capability_hybrid`]) and publishes the 1184-byte `ek` at enrollment
/// (`bridge_service_users.mlkem_ek`).
///
/// Deterministic in `ed25519_seed`: the same bridge re-derives the identical
/// keypair across restarts, so a grant sealed today to a published `ek` still
/// opens after a bridge bounce (no stored ML-KEM key needed — the keyfile's
/// existing Ed25519 seed is the sole input). The `ek` half is the ML-KEM part of
/// the holder's X-Wing public key
/// (`XWingPublicKey::from_parts(ek, holder_x25519_pubkey)`); the X25519 half is
/// the bridge's independent keyfile X25519 key, unchanged.
///
/// This is the **shared** bridge-service-user ML-KEM derivation the paused S6
/// TLS-cert hybrid also needs — built once here, in `fauna-mls`, so both the
/// capability-grant overlay (PQ-CAP-2) and a future S6 reuse it. The Go bridge is
/// the sole runtime caller, via the `fauna-ffi` thin wrapper.
pub fn derive_bridge_service_user_mlkem768(
    ed25519_seed: &[u8],
) -> ([u8; MLKEM768_DECAPS_KEY_LEN], [u8; MLKEM768_ENCAPS_KEY_LEN]) {
    fauna_pq_kem::derive_mlkem768_keypair_from_ikm(
        ed25519_seed,
        BRIDGE_SERVICE_USER_MLKEM_DERIVE_CONTEXT,
    )
}

/// Seal a capability grant's minimal derived content key to the grant's
/// **holder** — an enrolled bridge service-user X25519 pubkey (`holder_pubkey`,
/// from `bridge_service_users.x25519_pubkey`), the same wrap target
/// [`seal_tls_cert`] uses. The wrapped `payload` is the exact
/// opening key for one content kind (e.g. the 32-byte recipient-mail HPKE
/// secret, a tier `period_key`, or an index-segment key) — **never** MSEK, the
/// index master, `BackupKey`, or the identity seed (`key-material-hierarchy.md`
/// rule #7).
///
/// The seal binds `owner_actor_id + scope (class/kind/tier) + epoch` into the
/// HPKE AAD via [`AadBinding::for_capability`], so scope is
/// **cryptographically self-enforcing**: an out-of-scope, cross-tier,
/// cross-epoch, or cross-owner wrapped key fails AEAD verify at open rather
/// than opening the wrong content. **No net-new crypto** — this is the exact
/// `seal_tls_cert` HPKE-to-service-user substrate. Design spec § Phase 2 Step 2
/// § 2.1–2.2.
///
/// `epoch` is `None` for a master-key grant (every content kind today) or
/// `Some(e)` once the target kind adopts a content-sealing epoch (Phase 3 for
/// mail).
///
/// # Errors
///
/// Returns `WrapError::HpkeFailed` for HPKE seal failure (e.g. a malformed
/// `holder_pubkey`).
pub fn seal_capability(
    payload: &[u8],
    owner_actor_id: &[u8; 32],
    scope: &ScopeTuple,
    epoch: Option<u64>,
    holder_pubkey: &[u8; 32],
) -> Result<WrappedScopeKey, WrapError> {
    let info = AadBinding::for_capability(
        owner_actor_id,
        &scope.class,
        scope.kind.as_deref(),
        scope.tier.as_deref(),
        epoch,
        scope.set.as_deref(),
        scope.factor.as_deref(),
    );
    let aad = AadBinding::for_capability(
        owner_actor_id,
        &scope.class,
        scope.kind.as_deref(),
        scope.tier.as_deref(),
        epoch,
        scope.set.as_deref(),
        scope.factor.as_deref(),
    );
    let (enc, ciphertext) = hpke_seal(holder_pubkey, &info, &aad, payload)?;

    Ok(WrappedScopeKey {
        scope: scope.clone(),
        epoch,
        hpke: HpkeWire {
            kem_suite: KemSuite::STANDARD,
            enc: ByteBuf::from(enc),
            ciphertext: ByteBuf::from(ciphertext),
        },
    })
}

/// Open a [`WrappedScopeKey`] with the **holder's** X25519 secret — the
/// enrolled bridge service-user secret (the MDA / scorer / FTS-indexer key),
/// **not** the actor identity. Returns the minimal derived content key inside
/// the seal.
///
/// `owner_actor_id` is required because the seal's AAD binds the owner (§ 2.2);
/// in the real flow the holder reads it from the enclosing `GrantBlob.ix`, so
/// this is a deliberate addition to the design's shorthand
/// `unseal_capability(&WrappedScopeKey, x25519_secret)`. The `scope` and
/// `epoch` are taken from the [`WrappedScopeKey`] itself, so a tampered
/// on-the-wire scope/epoch recomputes a different AAD and fails to open.
///
/// # Errors
///
/// Returns `UnwrapError::HpkeFailed` for any HPKE open failure — a wrong
/// holder secret, an owner/scope/epoch mismatch (substitution attempt), or a
/// tampered ciphertext.
pub fn unseal_capability(
    wrapped: &WrappedScopeKey,
    owner_actor_id: &[u8; 32],
    holder_x25519_secret: &[u8; 32],
) -> Result<Vec<u8>, UnwrapError> {
    let info = AadBinding::for_capability(
        owner_actor_id,
        &wrapped.scope.class,
        wrapped.scope.kind.as_deref(),
        wrapped.scope.tier.as_deref(),
        wrapped.epoch,
        wrapped.scope.set.as_deref(),
        wrapped.scope.factor.as_deref(),
    );
    let aad = AadBinding::for_capability(
        owner_actor_id,
        &wrapped.scope.class,
        wrapped.scope.kind.as_deref(),
        wrapped.scope.tier.as_deref(),
        wrapped.epoch,
        wrapped.scope.set.as_deref(),
        wrapped.scope.factor.as_deref(),
    );
    // The classical opener passes `None`; a hybrid (X-Wing) wrapped key is
    // opened by `unseal_capability_hybrid` (the post-quantum overlay). A hybrid
    // blob reaching this classical path returns a typed `InvalidFormat` via
    // `hpke_open_dispatch` (naming the hybrid opener), never a silent mis-decrypt.
    hpke_open_dispatch(&wrapped.hpke, holder_x25519_secret, None, &info, &aad)
}

/// Seal a mailbox-export session key to the exporting user's **own**
/// MSEK-derived X-Wing public key — the client-side wrap of
/// `mail-export.md` § Key material, minted at `start_export_session` time and
/// stored by the nest in `export_sessions.blob_decryption_key_wrapped_for_actor`
/// so any of the user's clients can later open the download.
///
/// `recipient_xwing_pubkey` is `derive_recipient_xwing_keypair(msek).public`
/// for the user's *current* MSEK generation; the opener
/// ([`unseal_export_session_key`]) trials the whole standing set, so a session
/// started before a rotation still opens from a rotated client inside the
/// grace window.
///
/// **Hybrid unconditionally, no classical degrade.** Both halves derive from
/// the one MSEK the sealing client already holds, so there is no published-key
/// policy gate to fall through the way [`seal_spam_model_copy`] and
/// `seal_capability_selecting` have — and the plaintext here is the key to a
/// user's entire mail history, the worst thing in this module to leave under a
/// harvest-now-decrypt-later posture.
///
/// # Errors
///
/// Returns [`WrapError::HpkeFailed`] if the X-Wing seal fails (a malformed
/// recipient key or an AEAD error).
pub fn seal_export_session_key(
    session_key: &[u8; 32],
    actor_id: &[u8; 32],
    recipient_xwing_pubkey: &XWingPublicKey,
) -> Result<ExportSessionKeyBlob, WrapError> {
    let info = AadBinding::for_export_session_key(actor_id);
    let aad = AadBinding::for_export_session_key(actor_id);
    let (enc, ciphertext) = xwing_seal(recipient_xwing_pubkey, &info, &aad, session_key)?;
    Ok(ExportSessionKeyBlob {
        version: format::BLOB_FORMAT_VERSION,
        kind: ExportSessionKeyBlob::KIND.into(),
        index: ExportSessionKeyIndex(ByteBuf::from(actor_id.to_vec())),
        hpke: HpkeWire {
            kem_suite: KemSuite {
                kem: FAUNA_KEM_XWING,
                ..KemSuite::STANDARD
            },
            enc: ByteBuf::from(enc),
            ciphertext: ByteBuf::from(ciphertext),
        },
    })
}

/// Open an [`ExportSessionKeyBlob`] under the user's **complete standing mail
/// key set** — the current MSEK's keypair first, then each grace generation
/// ([`derive_standing_mail_keypairs`]) — the same set and the same trial order
/// the receive path opens mail with (`owner-key-material.md`
/// § Path B-sibling-2).
///
/// The standing set rather than a single keypair is what makes a download
/// survive a rotation: the wizard may have started the session under MSEK
/// *n-1* and the user may open the archive from a client that has since
/// rotated to *n*. A single-generation opener would hand that client a session
/// row it can see, a blob it can download, and no way to read it.
///
/// The AAD is reconstructed from the blob's own actor index, so a blob whose
/// index was tampered with fails AEAD-open; callers additionally compare
/// [`ExportSessionKeyIndex`] against the actor they believe they are.
///
/// # Errors
///
/// - [`UnwrapError::InvalidFormat`] for a wrong `kind` or a non-32-byte index.
/// - [`UnwrapError::HpkeFailed`] when every keypair in the set misses — the
///   blob is not this user's, the rotation is past the grace window, or the
///   ciphertext was tampered with. Indistinguishable by design, exactly as in
///   [`open_mail_record_standing`].
pub fn unseal_export_session_key(
    blob: &ExportSessionKeyBlob,
    keypairs: &[StandingMailKeypair],
) -> Result<[u8; 32], UnwrapError> {
    if blob.kind != ExportSessionKeyBlob::KIND {
        return Err(UnwrapError::InvalidFormat(format!(
            "expected kind={}, got {}",
            ExportSessionKeyBlob::KIND,
            blob.kind
        )));
    }
    let actor: [u8; 32] = blob.index.0.as_slice().try_into().map_err(|_| {
        UnwrapError::InvalidFormat(format!(
            "exporting actor_id index must be 32 bytes, got {}",
            blob.index.0.len()
        ))
    })?;
    let info = AadBinding::for_export_session_key(&actor);
    let aad = AadBinding::for_export_session_key(&actor);
    let plaintext = keypairs
        .iter()
        .find_map(|kp| {
            hpke_open_dispatch(
                &blob.hpke,
                &kp.x25519_secret,
                kp.mlkem_dk.as_deref(),
                &info,
                &aad,
            )
            .ok()
        })
        .ok_or(UnwrapError::HpkeFailed)?;
    plaintext.as_slice().try_into().map_err(|_| {
        UnwrapError::InvalidFormat(format!(
            "export session key must be 32 bytes, got {}",
            plaintext.len()
        ))
    })
}

/// Seal a spam-model deployment-baseline **holder copy** to the aggregation
/// holder's pubkey — the contributor-side write of the keyless
/// `content.read{spam-model}` shape (`mail-spam.md` § Encrypted-mode
/// interaction, ratified 2026-07-13; `key-material-hierarchy.md` § Audience:
/// deployment infrastructure → Spam-baseline holder copy).
///
/// `model_bytes` is the plaintext `SpamModel` serde_json (the contributor's
/// client/agent holds it unwrapped mid-write anyway); the seal binds the
/// contributing owner into the AAD ([`AadBinding::for_spam_model_copy`]) so a
/// copy cannot be re-attributed to another contributor. Selects the
/// post-quantum **X-Wing** suite iff the holder has published a valid-length
/// ML-KEM encapsulation key (`holder_mlkem_ek` — the same ek-presence policy
/// gate as the capability-grant wrap selector), degrading to classical X25519
/// otherwise or on an X-Wing seal error (PQ-4b, mirroring
/// `seal_capability_selecting`): a harvested holder copy is the contributor's
/// model, so it deserves the same harvest-now-decrypt-later posture as a
/// grant wrap.
///
/// # Errors
///
/// Returns [`WrapError::HpkeFailed`] only if the selected/fallback classical
/// seal fails (e.g. a malformed `holder_x25519_pubkey`).
pub fn seal_spam_model_copy(
    model_bytes: &[u8],
    owner_actor_id: &[u8; 32],
    holder_x25519_pubkey: &[u8; 32],
    holder_mlkem_ek: Option<&[u8]>,
) -> Result<SpamModelCopyBlob, WrapError> {
    let info = AadBinding::for_spam_model_copy(owner_actor_id);
    let aad = AadBinding::for_spam_model_copy(owner_actor_id);
    if let Some(ek) = holder_mlkem_ek.filter(|ek| ek.len() == MLKEM768_ENCAPS_KEY_LEN) {
        let mut ek_arr = [0u8; MLKEM768_ENCAPS_KEY_LEN];
        ek_arr.copy_from_slice(ek);
        let xwing_pk = XWingPublicKey::from_parts(ek_arr, *holder_x25519_pubkey);
        match xwing_seal(&xwing_pk, &info, &aad, model_bytes) {
            Ok((enc, ciphertext)) => {
                return Ok(SpamModelCopyBlob {
                    version: format::BLOB_FORMAT_VERSION,
                    kind: SpamModelCopyBlob::KIND.into(),
                    index: SpamModelCopyIndex(ByteBuf::from(owner_actor_id.to_vec())),
                    hpke: HpkeWire {
                        kem_suite: KemSuite {
                            kem: FAUNA_KEM_XWING,
                            ..KemSuite::STANDARD
                        },
                        enc: ByteBuf::from(enc),
                        ciphertext: ByteBuf::from(ciphertext),
                    },
                });
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "X-Wing spam-model-copy wrap failed; degrading to classical X25519 (PQ-4b)"
                );
            }
        }
    }
    let (enc, ciphertext) = hpke_seal(holder_x25519_pubkey, &info, &aad, model_bytes)?;
    Ok(SpamModelCopyBlob {
        version: format::BLOB_FORMAT_VERSION,
        kind: SpamModelCopyBlob::KIND.into(),
        index: SpamModelCopyIndex(ByteBuf::from(owner_actor_id.to_vec())),
        hpke: HpkeWire {
            kem_suite: KemSuite::STANDARD,
            enc: ByteBuf::from(enc),
            ciphertext: ByteBuf::from(ciphertext),
        },
    })
}

/// Open a [`SpamModelCopyBlob`] of **either** suite with the aggregation
/// holder's own service-user key halves — the holder-side read during a
/// `publish_spam_baseline` drain run. The AAD is reconstructed from the
/// blob's self-described owner index, so a copy whose owner index was
/// tampered with (re-attribution) fails AEAD-open; callers additionally
/// compare [`SpamModelCopyIndex`] against the worklist's claimed owner. A
/// classical blob ignores `holder_mlkem_dk`; a hybrid blob requires it.
///
/// # Errors
///
/// - [`UnwrapError::InvalidFormat`] for an unknown suite, or a hybrid blob
///   opened without `holder_mlkem_dk`.
/// - [`UnwrapError::HpkeFailed`] for any KEM/AEAD open failure (wrong holder
///   secret, owner mismatch, tampered ciphertext).
pub fn unseal_spam_model_copy(
    blob: &SpamModelCopyBlob,
    holder_x25519_secret: &[u8; 32],
    holder_mlkem_dk: Option<&[u8; MLKEM768_DECAPS_KEY_LEN]>,
) -> Result<Vec<u8>, UnwrapError> {
    if blob.kind != SpamModelCopyBlob::KIND {
        return Err(UnwrapError::InvalidFormat(format!(
            "expected kind={}, got {}",
            SpamModelCopyBlob::KIND,
            blob.kind
        )));
    }
    let owner: [u8; 32] = blob.index.0.as_slice().try_into().map_err(|_| {
        UnwrapError::InvalidFormat(format!(
            "owner actor_id index must be 32 bytes, got {}",
            blob.index.0.len()
        ))
    })?;
    let info = AadBinding::for_spam_model_copy(&owner);
    let aad = AadBinding::for_spam_model_copy(&owner);
    hpke_open_dispatch(
        &blob.hpke,
        holder_x25519_secret,
        holder_mlkem_dk,
        &info,
        &aad,
    )
}

/// Seal a capability grant's minimal derived content key to the grant's
/// **holder** under the post-quantum **X-Wing** (ML-KEM-768 ∥ X25519) hybrid
/// suite — the quantum-resistant counterpart of [`seal_capability`]
/// (`architecture/security/post-quantum.md` § surface A, the capability-grant
/// row).
///
/// A grant wraps a *standing* content-opening key (for `content.read{mail}` the
/// MSEK-derived recipient-mail secret — the `32 + 2400`-byte
/// `x25519_secret ∥ mlkem_decaps_key` payload from [`build_grant_blob`]'s byte
/// contract). Sealed classically, that key would let a harvest-now-decrypt-later
/// adversary who records the nest's `capability_grants` table today recover it
/// with a future CRQC and thence open the X-Wing-sealed content it reaches — a
/// bypass of the closed surface-A mail seal. So the wrap itself rides X-Wing
/// whenever the holder has published an ML-KEM encapsulation key; the seal-side
/// suite choice is the caller's (capability-gated policy), exactly like
/// [`seal_to_recipient_xwing`] for mail.
///
/// `holder_xwing_pubkey` is [`XWingPublicKey::from_parts`]`(holder_mlkem_ek,
/// holder_x25519_pubkey)` — the enrolled bridge service-user's X25519 keyfile
/// key plus its seed-derived ML-KEM ek. The seal binds `owner + scope + epoch`
/// into the HPKE AAD identically to [`seal_capability`], so scope stays
/// cryptographically self-enforcing; only the KEM changes. The result
/// self-describes its suite (`hpke.kem_suite.kem = FAUNA_KEM_XWING`, 1120-byte
/// `enc`) and is opened by [`unseal_capability_hybrid`].
///
/// # Errors
///
/// Returns [`WrapError::HpkeFailed`] if the holder's ML-KEM encapsulation key
/// fails FIPS 203 validation, or on AEAD seal failure.
pub fn seal_capability_xwing(
    payload: &[u8],
    owner_actor_id: &[u8; 32],
    scope: &ScopeTuple,
    epoch: Option<u64>,
    holder_xwing_pubkey: &XWingPublicKey,
) -> Result<WrappedScopeKey, WrapError> {
    let info = AadBinding::for_capability(
        owner_actor_id,
        &scope.class,
        scope.kind.as_deref(),
        scope.tier.as_deref(),
        epoch,
        scope.set.as_deref(),
        scope.factor.as_deref(),
    );
    let aad = AadBinding::for_capability(
        owner_actor_id,
        &scope.class,
        scope.kind.as_deref(),
        scope.tier.as_deref(),
        epoch,
        scope.set.as_deref(),
        scope.factor.as_deref(),
    );
    let (enc, ciphertext) = xwing_seal(holder_xwing_pubkey, &info, &aad, payload)?;

    Ok(WrappedScopeKey {
        scope: scope.clone(),
        epoch,
        hpke: HpkeWire {
            kem_suite: KemSuite {
                kem: FAUNA_KEM_XWING,
                ..KemSuite::STANDARD
            },
            enc: ByteBuf::from(enc),
            ciphertext: ByteBuf::from(ciphertext),
        },
    })
}

/// Open a [`WrappedScopeKey`] of **either** suite — classical X25519 or hybrid
/// X-Wing — given the holder's X25519 secret **and** its ML-KEM decapsulation
/// key. The post-quantum-capable counterpart of [`unseal_capability`]; the
/// enrolled bridge service-user holder derives both halves (its keyfile X25519
/// secret + its seed-derived ML-KEM dk) and calls this so it can open both
/// classical (ek-absent holders) and hybrid grants with one opener.
///
/// A classical wrapped key ignores `holder_mlkem_dk` (it opens with the X25519
/// secret); a hybrid wrapped key uses both halves. `owner_actor_id`, and the
/// `scope`/`epoch` taken from the [`WrappedScopeKey`], reconstruct the AAD
/// exactly as [`unseal_capability`], so a tampered on-the-wire scope/epoch
/// recomputes a different AAD and fails to open.
///
/// # Errors
///
/// - [`UnwrapError::InvalidFormat`] for an unknown suite.
/// - [`UnwrapError::HpkeFailed`] for any KEM/AEAD open failure (wrong holder
///   secret, an owner/scope/epoch mismatch, or a tampered ciphertext).
pub fn unseal_capability_hybrid(
    wrapped: &WrappedScopeKey,
    owner_actor_id: &[u8; 32],
    holder_x25519_secret: &[u8; 32],
    holder_mlkem_dk: &[u8; MLKEM768_DECAPS_KEY_LEN],
) -> Result<Vec<u8>, UnwrapError> {
    let info = AadBinding::for_capability(
        owner_actor_id,
        &wrapped.scope.class,
        wrapped.scope.kind.as_deref(),
        wrapped.scope.tier.as_deref(),
        wrapped.epoch,
        wrapped.scope.set.as_deref(),
        wrapped.scope.factor.as_deref(),
    );
    let aad = AadBinding::for_capability(
        owner_actor_id,
        &wrapped.scope.class,
        wrapped.scope.kind.as_deref(),
        wrapped.scope.tier.as_deref(),
        wrapped.epoch,
        wrapped.scope.set.as_deref(),
        wrapped.scope.factor.as_deref(),
    );
    hpke_open_dispatch(
        &wrapped.hpke,
        holder_x25519_secret,
        Some(holder_mlkem_dk),
        &info,
        &aad,
    )
}

/// Seal one capability payload to `holder_x25519_pubkey`, selecting the
/// post-quantum **X-Wing** suite iff the holder has published a valid-length
/// (1184-B) ML-KEM encapsulation key (`holder_mlkem_ek`), else the classical
/// X25519 suite. The client-mint counterpart of the nest's mail selector
/// [`seal_recipient_blob`](../../../../bins/fauna-nest/src/bridge_routing_handlers.rs)
/// (`post-quantum.md` § Capability-grant holders).
///
/// **The ek's presence IS the policy gate.** A bridge service-user derives +
/// publishes its ek unconditionally at enrollment (no capability token), which
/// is exactly why the goal doc gates the mint on ek-presence ("wraps … when the
/// ek is present, degrading to classical otherwise") rather than on a separately
/// plumbed capability check. Degrades to the classical seal on an X-Wing seal
/// *error* (PQ-4b): the ek is length-gated here but FIPS-203-validated only at
/// encaps, so a right-length-but-invalid ek would otherwise fail the whole mint
/// instead of degrading — mail's `seal_recipient_blob` degrades identically.
///
/// # Errors
///
/// Returns [`WrapError::HpkeFailed`] only if the selected/fallback classical
/// seal fails (e.g. a malformed `holder_x25519_pubkey`).
fn seal_capability_selecting(
    payload: &[u8],
    owner_actor_id: &[u8; 32],
    scope: &ScopeTuple,
    epoch: Option<u64>,
    holder_x25519_pubkey: &[u8; 32],
    holder_mlkem_ek: Option<&[u8]>,
) -> Result<WrappedScopeKey, WrapError> {
    if let Some(ek) = holder_mlkem_ek.filter(|ek| ek.len() == MLKEM768_ENCAPS_KEY_LEN) {
        let mut ek_arr = [0u8; MLKEM768_ENCAPS_KEY_LEN];
        ek_arr.copy_from_slice(ek);
        let xwing_pk = XWingPublicKey::from_parts(ek_arr, *holder_x25519_pubkey);
        match seal_capability_xwing(payload, owner_actor_id, scope, epoch, &xwing_pk) {
            Ok(wrapped) => return Ok(wrapped),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "X-Wing capability wrap failed; degrading to classical X25519 (PQ-4b)"
                );
            }
        }
    }
    seal_capability(payload, owner_actor_id, scope, epoch, holder_x25519_pubkey)
}

/// Assemble a user-minted [`GrantBlob`] from its declared scope and the minimal
/// derived key-subsets — the "build `GrantBlob`" half of the client-side mint
/// (design § Phase 2 Step 2 § 2.6: "the client-side mint — derive payloads +
/// build `GrantBlob`").
///
/// The caller — the owner's client, holding the content root off-box — passes
/// each declared scope tuple paired with its already-derived minimal payload:
/// the recipient-mail HPKE secret / a tier `period_key` / an index-segment key
/// (the § 2.1 table). A keyless `content.label-write` tuple pairs with `None` —
/// it appears in the grant's `scope` but carries no [`WrappedScopeKey`]. Each
/// key-bearing tuple is HPKE-sealed to `holder` via [`seal_capability_selecting`],
/// so its scope + owner are AAD-bound and un-substitutable at open time.
///
/// **Post-quantum wrap selection (surface A, derived).** `holder_mlkem_ek` is the
/// holder's published ML-KEM encapsulation key (from
/// `fauna.bridges.fetch_bridge_pubkey`), or `None` for a holder that hasn't
/// published one. When present + valid-length every key-bearing tuple is wrapped
/// under the **X-Wing** (ML-KEM-768 ∥ X25519) hybrid suite, so a harvested
/// `capability_grants` row is not a CRQC-openable bypass of the closed
/// mail-at-rest seal; absent, the classical X25519 wrap (the non-erroring
/// degrade). Read-side opens either suite via [`unseal_capability_hybrid`].
/// `post-quantum.md` § Capability-grant holders.
///
/// **The `content.read{mail}` payload byte contract** (what the drain's opener,
/// `fauna-ffi::open_mail_record_with_key`, dispatches on): **32 bytes** — the
/// recipient-mail X25519 HPKE secret (`derive_recipient_hpke_keypair`) — opens
/// classical records only; **32 + 2400 bytes** — `x25519_secret ∥
/// mlkem_decaps_key`, the [`derive_recipient_xwing_keypair`] halves — opens
/// both classical and hybrid (X-Wing) records. Mint the concatenated shape once
/// the owner has published a post-quantum key, else hybrid-sealed mail is
/// undrainable under the grant.
///
/// Every content kind today is standing-keyed, so this builds the **master-key**
/// regime: exactly one [`WrappedScopeKey`] per key-bearing tuple, `epoch: None`,
/// and `window` is the advisory honest bound (design § Phase 2 Step 1). The
/// epoch-sealed regime (one key per epoch, Phase 3 for mail) is a later
/// generalization and is deliberately not built here (design Q-expiry: ship
/// honest master-key first).
///
/// The result is ready for `to_canonical_bytes()` → `fauna.capabilities.mint`
/// (`MintGrantRequest { grant_blob }`). It touches only the minimal derived
/// subsets the caller supplies — never MSEK, the index master, or the identity
/// seed — so it stays wasm-clean and identity-free (`key-material-hierarchy.md`
/// rule #7).
///
/// # Errors
///
/// Returns `WrapError::HpkeFailed` if any key-bearing tuple's seal fails (e.g. a
/// malformed `holder_pubkey`).
pub fn build_grant_blob(
    owner_actor_id: &[u8; 32],
    grant_id: &[u8; 16],
    holder_pubkey: &[u8; 32],
    holder_mlkem_ek: Option<&[u8]>,
    window: GrantWindow,
    scopes: &[(ScopeTuple, Option<Vec<u8>>)],
) -> Result<GrantBlob, WrapError> {
    let epoched: Vec<ScopeWraps> = scopes
        .iter()
        .map(|(tuple, payload)| {
            // Master-key today: one WrappedScopeKey per key-bearing tuple, epoch None.
            (
                tuple.clone(),
                payload.iter().map(|p| (None, p.clone())).collect(),
            )
        })
        .collect();
    build_grant_blob_with_epochs(
        owner_actor_id,
        grant_id,
        holder_pubkey,
        holder_mlkem_ek,
        window,
        &epoched,
    )
}

/// One declared scope tuple paired with its `(epoch, payload)` wraps — the
/// input element of [`build_grant_blob_with_epochs`].
pub type ScopeWraps = (ScopeTuple, Vec<(Option<u64>, Vec<u8>)>);

/// The epoch-aware generalization of [`build_grant_blob`]: each tuple pairs
/// with **zero or more** `(epoch, payload)` wraps — zero for a keyless tuple,
/// one `(None, key)` for a master-key kind (what [`build_grant_blob`]
/// delegates here), and one wrap **per content-key generation** with
/// `epoch = Some(generation.version)` for the folder scope
/// (`mls-group-key-material.md` § M2 third distribution channel). Every wrap
/// is AAD-bound to its `(owner, class, kind, tier, epoch, set)` index via
/// [`seal_capability_selecting`], so a stale generation's key cannot be
/// presented for a newer version and set A's key cannot open set B.
///
/// # Errors
///
/// Returns `WrapError::HpkeFailed` if any wrap's seal fails (e.g. a malformed
/// `holder_pubkey`).
pub fn build_grant_blob_with_epochs(
    owner_actor_id: &[u8; 32],
    grant_id: &[u8; 16],
    holder_pubkey: &[u8; 32],
    holder_mlkem_ek: Option<&[u8]>,
    window: GrantWindow,
    scopes: &[ScopeWraps],
) -> Result<GrantBlob, WrapError> {
    let mut scope = Vec::with_capacity(scopes.len());
    let mut wrapped_keys = Vec::new();
    for (tuple, wraps) in scopes {
        check_scope_wraps_policy(tuple, wraps)?;
        scope.push(tuple.clone());
        for (epoch, payload) in wraps {
            // X-Wing iff the holder published a valid ML-KEM ek, else classical.
            wrapped_keys.push(seal_capability_selecting(
                payload,
                owner_actor_id,
                tuple,
                *epoch,
                holder_pubkey,
                holder_mlkem_ek,
            )?);
        }
    }
    Ok(GrantBlob {
        version: BLOB_FORMAT_VERSION,
        kind: GrantBlob::KIND.to_string(),
        index: GrantIndex(owner_actor_id.to_vec(), grant_id.to_vec()),
        holder: ByteBuf::from(holder_pubkey.to_vec()),
        window,
        scope,
        wrapped_keys,
    })
}

/// Whether `tuple`'s content kind uses `epoch` as a **wall-clock sealing
/// epoch** (mail, calendar) rather than a content-key generation version
/// (folder) — the axis the bounded-XOR-master-key mint policy applies to.
fn is_wall_clock_epoch_kind(tuple: &ScopeTuple) -> bool {
    matches!(
        tuple.kind.as_deref(),
        Some(ScopeTuple::KIND_MAIL) | Some(ScopeTuple::KIND_CALENDAR)
    )
}

/// The mint-side scope policy every wrap path enforces (both the blob builder
/// and the renew-append path — the two ways a `WrappedScopeKey` reaches a
/// holder):
///
/// - **`content.read{spam-model}` is keyless BY RATIFIED DESIGN** — its only
///   opening key is the recipient-mail secret, so any wrap here would be
///   `content.read{mail}` under another name. Fail loudly rather than seal an
///   over-granting key (`key-material-hierarchy.md` rule #7 + § Don't do
///   these, resolved 2026-07-13).
/// - **A wall-clock-epoch kind (`mail`, `calendar`) must not mix regimes in
///   one grant** — a standing (`epoch: None`) wrap alongside per-epoch wraps
///   would let the holder open every epoch forever while the grant's window
///   claims a bound (content-sealing-epochs design 2026-07-18 § 2 mint
///   policy: bounded XOR master-key). The folder kind is exempt: its
///   `epoch` is a content-key generation, where multiple `Some` values (and
///   no `None`) are the normal shape.
fn check_scope_wraps_policy(
    tuple: &ScopeTuple,
    wraps: &[(Option<u64>, Vec<u8>)],
) -> Result<(), WrapError> {
    if tuple.kind.as_deref() == Some(ScopeTuple::KIND_SPAM_MODEL) && !wraps.is_empty() {
        return Err(WrapError::InvalidInput(
            "content.read{spam-model} is a keyless scope; it must never wrap a key \
             (the model travels as a SpamModelCopyBlob sealed to the holder)"
                .into(),
        ));
    }
    if is_wall_clock_epoch_kind(tuple)
        && wraps.iter().any(|(e, _)| e.is_none())
        && wraps.iter().any(|(e, _)| e.is_some())
    {
        return Err(WrapError::InvalidInput(
            "a mail/calendar grant is bounded (per-epoch wraps only) XOR master-key \
             (one standing wrap) — mixing regimes would let the holder outlive its \
             window (content-sealing-epochs design § 2 mint policy)"
                .into(),
        ));
    }
    if is_wall_clock_epoch_kind(tuple) {
        // One wrap per (scope, epoch), mint-side too — the renew plane's slot
        // invariant made universal (amendment 2026-07-19). Cross-generation
        // boundary coverage rides INSIDE the one wrap's payload, never as a
        // second wrap the renew replace would nondeterministically drop.
        let mut epochs: Vec<u64> = wraps.iter().filter_map(|(e, _)| *e).collect();
        epochs.sort_unstable();
        if epochs.windows(2).any(|w| w[0] == w[1]) {
            return Err(WrapError::InvalidInput(
                "a mail/calendar grant carries at most one wrap per epoch — a \
                 rotation-boundary epoch concatenates its generations' secrets \
                 inside the single wrap's payload (content-sealing-epochs \
                 amendment 2026-07-19)"
                    .into(),
            ));
        }
    }
    Ok(())
}

/// Whether `wrapped_keys` mixes a standing (`epoch: None`) wrap with
/// per-epoch (`epoch: Some`) wraps for the same wall-clock-epoch scope
/// (mail/calendar) — the bounded-XOR-master-key mint policy (§ 2), checked
/// across the WHOLE set rather than one mint/renew call's own wraps.
/// [`check_scope_wraps_policy`]/[`build_renewal_wraps`]'s guard is per-call
/// and cannot see this: a renew that appends a pure `(mail, epoch: None)`
/// key passes the per-call guard in isolation yet would silently mix
/// regimes against a grant's EXISTING per-epoch wraps — the nest's
/// `(scope, epoch)` dedup can't catch it either (`None` never matches
/// `Some`). Content-sealing-epochs design 2026-07-18 §§ 2/10 (INFO-C/INFO-E).
/// The nest's `fauna.capabilities.renew` handler calls this after merging
/// appended keys into the stored grant, before writing it back.
#[must_use]
pub fn wall_clock_epoch_regime_conflict(wrapped_keys: &[WrappedScopeKey]) -> bool {
    // A grant carries only a handful of distinct scopes, so a small linear
    // scan (keyed by `ScopeTuple`'s existing `PartialEq`) is simpler than
    // adding `Hash` to a wire-format type just for this.
    let mut regimes: Vec<(&ScopeTuple, bool, bool)> = Vec::new();
    for k in wrapped_keys {
        if !is_wall_clock_epoch_kind(&k.scope) {
            continue;
        }
        match regimes.iter_mut().find(|(scope, _, _)| *scope == &k.scope) {
            Some((_, has_none, has_some)) => {
                if k.epoch.is_none() {
                    *has_none = true;
                } else {
                    *has_some = true;
                }
            }
            None => regimes.push((&k.scope, k.epoch.is_none(), k.epoch.is_some())),
        }
    }
    regimes
        .iter()
        .any(|&(_, has_none, has_some)| has_none && has_some)
}

/// A bounded grant's window is never wider than its wraps: the first
/// `(scope, epoch)` a `fauna.capabilities.renew` moving `window.1` from
/// `old_end` to `new_end` would leave without a key. For every
/// wall-clock-epoch scope (mail/calendar) in the per-epoch regime (`epoch:
/// Some` wraps only — a master-key scope is a pure window bump), every
/// sealing epoch in `epoch_of(old_end) + 1 ..= epoch_of(new_end)` must have
/// a wrap once the appends are merged, or the holder would fetch a window
/// whose tail no key opens — a keyless bump leaving the extension epochs
/// silently dark. `None` when the extension is covered; an equal-end renew
/// (the rotation heal's shape) extends nothing. The nest's renew handler
/// checks it over the merged set, after [`wall_clock_epoch_regime_conflict`].
///
/// Walks the wraps, never the extension range — a far-future `new_end` is a
/// range of ~10¹³ epochs and is refused at the first uncovered one.
#[must_use]
pub fn uncovered_bounded_extension(
    wrapped_keys: &[WrappedScopeKey],
    old_end: u64,
    new_end: u64,
) -> Option<(ScopeTuple, u64)> {
    if new_end <= old_end {
        return None;
    }
    let first = mail_sealing_epoch_of(old_end) + 1;
    let last = mail_sealing_epoch_of(new_end);
    if first > last {
        return None;
    }
    let mut scopes: Vec<&ScopeTuple> = Vec::new();
    for k in wrapped_keys {
        if is_wall_clock_epoch_kind(&k.scope) && k.epoch.is_some() && !scopes.contains(&&k.scope) {
            scopes.push(&k.scope);
        }
    }
    for scope in scopes {
        let mut held: Vec<u64> = wrapped_keys
            .iter()
            .filter(|k| &k.scope == scope)
            .filter_map(|k| k.epoch)
            .filter(|e| (first..=last).contains(e))
            .collect();
        held.sort_unstable();
        held.dedup();
        if held.len() as u64 == last - first + 1 {
            continue;
        }
        let mut cursor = first;
        for e in held {
            if e != cursor {
                break;
            }
            cursor += 1;
        }
        return Some((scope.clone(), cursor));
    }
    None
}

/// A bounded grant's window slides, it never grows: drop every per-epoch
/// wall-clock wrap (mail/calendar, `epoch: Some`) whose sealing epoch lies
/// below `epoch_of(window_start)`, returning how many went. The retention
/// ruling (`encryption-at-rest.md` § Capability tiering → *Content-sealing
/// epochs*): a renewal re-centres the window on the renewal instant, and the
/// wraps for epochs the window no longer covers are pruned nest-side in the
/// same write — without it every renewal appends one wrap per newly covered
/// week for ever and the grant reaches the nest's size cap within months.
/// Standing wraps (`epoch: None`) and generation-indexed folder wraps are
/// untouched: their `epoch` slot is not a wall-clock index. Idempotent, so the
/// nest's renew handler runs it on every renew over the merged set, whether
/// or not the request moved the start.
pub fn prune_wraps_below_window_start(
    wrapped_keys: &mut Vec<WrappedScopeKey>,
    window_start: u64,
) -> usize {
    let floor = mail_sealing_epoch_of(window_start);
    let before = wrapped_keys.len();
    wrapped_keys
        .retain(|k| !(is_wall_clock_epoch_kind(&k.scope) && k.epoch.is_some_and(|e| e < floor)));
    before - wrapped_keys.len()
}

/// Build the loose [`WrappedScopeKey`]s a `fauna.capabilities.renew` appends
/// (`appended_keys`) — e.g. the next window's per-epoch mail keys for a
/// bounded grant. Applies the same [`check_scope_wraps_policy`] guards and
/// the same X-Wing-iff-ek wrap selection as [`build_grant_blob_with_epochs`],
/// so the renew path cannot mint a wrap the original mint would have refused.
/// The nest dedups appended keys by `(scope, epoch)`, so overlap with keys
/// the grant already carries is harmless.
///
/// # Errors
///
/// Returns [`WrapError::InvalidInput`] on a policy violation and
/// [`WrapError::HpkeFailed`] if a wrap's seal fails.
pub fn build_renewal_wraps(
    owner_actor_id: &[u8; 32],
    holder_pubkey: &[u8; 32],
    holder_mlkem_ek: Option<&[u8]>,
    scopes: &[ScopeWraps],
) -> Result<Vec<WrappedScopeKey>, WrapError> {
    let mut wrapped_keys = Vec::new();
    for (tuple, wraps) in scopes {
        check_scope_wraps_policy(tuple, wraps)?;
        for (epoch, payload) in wraps {
            wrapped_keys.push(seal_capability_selecting(
                payload,
                owner_actor_id,
                tuple,
                *epoch,
                holder_pubkey,
                holder_mlkem_ek,
            )?);
        }
    }
    Ok(wrapped_keys)
}

/// HPKE-Seal `plaintext` (raw RFC 5322 message bytes, or canonical
/// index-hint token bytes) to a recipient's X25519 pubkey, returning
/// the on-the-wire `MailRecordEnvelope`.
///
/// The MTA bridge calls this twice per inbound message: once with the
/// raw bytes targeted at the recipient's MLS pubkey (→ `encrypted_body`),
/// and once with `CanonicalTokenSet::canonical_bytes` targeted at the
/// recipient's index pubkey (→ `encrypted_index_hint`); both feed into
/// `fauna.bridges.ingest_inbound_mail`, per the mail-bridge rearchitecture's
/// inbound mail flow (tracked internally).
///
/// HPKE info + AAD are the constant `AadBinding::for_mail_record()`
/// binding; per-recipient targeting is encoded by the KEM step (the
/// encapsulated key is bound to `recipient_x25519_pubkey`).
///
/// # Errors
///
/// Returns `WrapError::HpkeFailed` on HPKE seal failure (e.g.
/// malformed recipient pubkey).
pub fn seal_to_recipient(
    plaintext: &[u8],
    recipient_x25519_pubkey: &[u8; 32],
) -> Result<MailRecordEnvelope, WrapError> {
    let info = AadBinding::for_mail_record();
    let aad = AadBinding::for_mail_record();
    let (enc, ciphertext) = hpke_seal(recipient_x25519_pubkey, &info, &aad, plaintext)?;
    Ok(MailRecordEnvelope {
        version: format::BLOB_FORMAT_VERSION,
        kind: "mail-record".into(),
        hpke: HpkeWire {
            kem_suite: KemSuite::STANDARD,
            enc: ByteBuf::from(enc),
            ciphertext: ByteBuf::from(ciphertext),
        },
    })
}

/// Seal `plaintext` to a recipient's **X-Wing** (ML-KEM-768 ∥ X25519) public key,
/// the post-quantum hybrid counterpart of [`seal_to_recipient`] (surface A step 2,
/// goal `architecture/security/post-quantum.md`).
///
/// The seal-side suite choice is the caller's (data-dependent policy): the MTA /
/// nest calls this iff the recipient published a hybrid key, else
/// [`seal_to_recipient`] (the non-erroring degrade).
/// The resulting `MailRecordEnvelope` self-describes its suite via
/// `hpke.kem_suite.kem = FAUNA_KEM_XWING` and carries the 1120-byte X-Wing
/// ciphertext in `hpke.enc`; the AEAD ciphertext + AAD binding are byte-identical
/// to the classical path. A reader opens it with [`unseal_mail_record_hybrid`].
///
/// # Errors
///
/// Returns [`WrapError::HpkeFailed`] if the recipient ML-KEM encapsulation key
/// fails FIPS 203 validation, or on AEAD seal failure.
pub fn seal_to_recipient_xwing(
    plaintext: &[u8],
    recipient_xwing_pubkey: &XWingPublicKey,
) -> Result<MailRecordEnvelope, WrapError> {
    let info = AadBinding::for_mail_record();
    let aad = AadBinding::for_mail_record();
    let (enc, ciphertext) = xwing_seal(recipient_xwing_pubkey, &info, &aad, plaintext)?;
    Ok(MailRecordEnvelope {
        version: format::BLOB_FORMAT_VERSION,
        kind: "mail-record".into(),
        hpke: HpkeWire {
            kem_suite: KemSuite {
                kem: FAUNA_KEM_XWING,
                ..KemSuite::STANDARD
            },
            enc: ByteBuf::from(enc),
            ciphertext: ByteBuf::from(ciphertext),
        },
    })
}

/// Unseal a `MailRecordEnvelope` produced by `seal_to_recipient`.
/// The recipient client / MDA calls this with its X25519 secret to
/// recover the plaintext (raw RFC 5322 bytes for `encrypted_body`;
/// canonical token bytes for `encrypted_index_hint`).
///
/// Not called from the MTA itself — it's the at-rest reader's
/// counterpart. Kept in the same module as the seal so the round-trip
/// stays in one file and tests cover both directions.
///
/// # Errors
///
/// - `UnwrapError::InvalidFormat` for blob shape problems (wrong kind).
/// - `UnwrapError::HpkeFailed` for HPKE open failure (wrong recipient
///   secret, tampered ciphertext).
pub fn unseal_mail_record(
    envelope: &MailRecordEnvelope,
    recipient_x25519_secret: &[u8; 32],
) -> Result<Vec<u8>, UnwrapError> {
    if envelope.kind != "mail-record" {
        return Err(UnwrapError::InvalidFormat(format!(
            "expected kind=mail-record, got {}",
            envelope.kind
        )));
    }
    let info = AadBinding::for_mail_record();
    let aad = AadBinding::for_mail_record();
    // Classical-only opener: an X-Wing blob returns a typed error pointing at
    // `unseal_mail_record_hybrid` rather than mis-decrypting.
    hpke_open_dispatch(&envelope.hpke, recipient_x25519_secret, None, &info, &aad)
}

/// Open a `MailRecordEnvelope` of **either** suite — classical X25519 or hybrid
/// X-Wing — given the recipient's X25519 secret **and** MSEK-derived ML-KEM
/// decapsulation key. This is the post-quantum-capable reader counterpart of
/// [`unseal_mail_record`]; the MDA / client derives both halves from MSEK via
/// [`derive_recipient_xwing_keypair`] and calls this so it can read both
/// classical (ek-absent or degraded) and hybrid mail.
///
/// A classical blob ignores `recipient_mlkem_dk` (it opens with the X25519
/// secret); a hybrid blob uses both halves.
///
/// # Errors
///
/// - [`UnwrapError::InvalidFormat`] for blob-shape problems (wrong kind, wrong
///   `enc` length) or an unknown suite.
/// - [`UnwrapError::HpkeFailed`] for any KEM/AEAD open failure (wrong recipient
///   key, tampered ciphertext).
pub fn unseal_mail_record_hybrid(
    envelope: &MailRecordEnvelope,
    recipient_x25519_secret: &[u8; 32],
    recipient_mlkem_dk: &[u8; MLKEM768_DECAPS_KEY_LEN],
) -> Result<Vec<u8>, UnwrapError> {
    if envelope.kind != "mail-record" {
        return Err(UnwrapError::InvalidFormat(format!(
            "expected kind=mail-record, got {}",
            envelope.kind
        )));
    }
    let info = AadBinding::for_mail_record();
    let aad = AadBinding::for_mail_record();
    hpke_open_dispatch(
        &envelope.hpke,
        recipient_x25519_secret,
        Some(recipient_mlkem_dk),
        &info,
        &aad,
    )
}

/// Report whether `bytes` are a sealed [`MailRecordEnvelope`] (canonical
/// DAG-CBOR, `kind == "mail-record"`, supported version, suite-consistent
/// `enc` length) — as opposed to raw plaintext (an RFC 5322 mail body,
/// an iCalendar event, canonical index-hint token bytes).
///
/// Every mail-plane record rests sealed, so no read path branches on this;
/// it is the predicate tests use to prove a stored payload rests sealed.
/// The write side's proof is [`SealedRecordBytes::verify`], the same strict
/// decode. Reliable because the strict decode requires the exact
/// `{v, kind, hpke}` map — text formats (RFC 5322, iCalendar, vCard) and
/// the `<u32 BE len><bytes>` token-set serialization cannot parse as it.
pub fn is_sealed_mail_record(bytes: &[u8]) -> bool {
    MailRecordEnvelope::from_canonical_bytes(bytes).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The nest's coverage guard for a bounded renew: an extension across a
    /// sealing-epoch boundary is covered only when every newly covered epoch
    /// has a per-epoch wrap for the scope; a master-key scope and a
    /// generation-indexed folder scope are never bounded by it.
    #[test]
    fn uncovered_bounded_extension_names_the_first_epoch_without_a_wrap() {
        let owner = [1u8; 32];
        let (_secret, holder) = generate_x25519_keypair();
        let mail = ScopeTuple {
            class: "content.read".into(),
            kind: Some("mail".into()),
            tier: None,
            set: None,
            factor: None,
        };
        let wrap = |scope: &ScopeTuple, epoch: Option<u64>| {
            seal_capability(&[0xAAu8; 32], &owner, scope, epoch, &holder).expect("seal")
        };
        let week = MAIL_SEALING_EPOCH_SECS;
        // A grant covering epochs 8 and 9, ending late in epoch 9.
        let old_end = 10 * week - 1;
        let held = vec![wrap(&mail, Some(8)), wrap(&mail, Some(9))];

        // An equal-end renew, and one that stays inside epoch 9, extend nothing.
        assert_eq!(uncovered_bounded_extension(&held, old_end, old_end), None);
        assert_eq!(
            uncovered_bounded_extension(&held, 10 * week - 100, old_end),
            None
        );
        // Into epochs 10 and 11 with only 10 appended: 11 is the gap.
        let with_10 = [held.clone(), vec![wrap(&mail, Some(10))]].concat();
        assert_eq!(
            uncovered_bounded_extension(&with_10, old_end, 12 * week - 1),
            Some((mail.clone(), 11))
        );
        // Both appended: covered.
        let with_11 = [with_10, vec![wrap(&mail, Some(11))]].concat();
        assert_eq!(
            uncovered_bounded_extension(&with_11, old_end, 12 * week - 1),
            None
        );
        // A keyless bump to a far-future end is refused at the first epoch
        // past the old end, without walking the range.
        assert_eq!(
            uncovered_bounded_extension(&held, old_end, u64::MAX),
            Some((mail.clone(), 10))
        );
        // A master-key mail scope is a pure window bump.
        let standing = vec![wrap(&mail, None)];
        assert_eq!(
            uncovered_bounded_extension(&standing, old_end, u64::MAX),
            None
        );
        // A folder scope's `epoch` is a generation version, not a wall-clock
        // epoch — never bounded by the calendar.
        let folder = ScopeTuple {
            class: "content.read".into(),
            kind: Some("folder".into()),
            tier: None,
            set: Some("paywalled".into()),
            factor: None,
        };
        let generation = vec![wrap(&folder, Some(1))];
        assert_eq!(
            uncovered_bounded_extension(&generation, old_end, u64::MAX),
            None
        );
    }

    /// The retention ruling's prune: a per-epoch mail wrap below the window
    /// start goes, everything else stays, and a second pass is a no-op.
    #[test]
    fn prune_wraps_below_window_start_drops_only_stale_wall_clock_epochs() {
        let owner = [1u8; 32];
        let (_secret, holder) = generate_x25519_keypair();
        let mail = ScopeTuple {
            class: "content.read".into(),
            kind: Some("mail".into()),
            tier: None,
            set: None,
            factor: None,
        };
        let folder = ScopeTuple {
            class: "content.read".into(),
            kind: Some("folder".into()),
            tier: None,
            set: Some("paywalled".into()),
            factor: None,
        };
        let wrap = |scope: &ScopeTuple, epoch: Option<u64>| {
            seal_capability(&[0xAAu8; 32], &owner, scope, epoch, &holder).expect("seal")
        };
        let week = MAIL_SEALING_EPOCH_SECS;
        let mut keys = vec![
            wrap(&mail, Some(8)),
            wrap(&mail, Some(9)),
            wrap(&mail, Some(10)),
            wrap(&mail, Some(11)),
            // A folder generation "1" is not a calendar epoch — never pruned.
            wrap(&folder, Some(1)),
            // A standing wrap has no epoch — never pruned.
            wrap(&mail, None),
        ];
        // A start inside epoch 10 keeps 10 and 11 (the epoch containing the
        // start is still covered) and drops 8 and 9.
        assert_eq!(prune_wraps_below_window_start(&mut keys, 10 * week + 5), 2);
        let mail_epochs: Vec<u64> = keys
            .iter()
            .filter(|k| k.scope == mail)
            .filter_map(|k| k.epoch)
            .collect();
        assert_eq!(mail_epochs, vec![10, 11]);
        assert!(keys.iter().any(|k| k.scope == folder && k.epoch == Some(1)));
        assert!(keys.iter().any(|k| k.scope == mail && k.epoch.is_none()));
        // Idempotent.
        assert_eq!(prune_wraps_below_window_start(&mut keys, 10 * week + 5), 0);
        // A start of 0 (a grant that never slid) prunes nothing.
        assert_eq!(prune_wraps_below_window_start(&mut keys, 0), 0);
    }

    #[test]
    fn sealed_record_bytes_verify_accepts_both_suites_and_preserves_bytes() {
        use crate::wrapped_blob::mls_snapshot_plaintext::{
            derive_recipient_hpke_keypair, derive_recipient_xwing_keypair,
        };
        let msek = [0x42u8; 32];

        // Classical X25519 seal — the client `seal_event_body` / Go
        // `EncryptToRecipient` shape.
        let (_sec, pubkey) = derive_recipient_hpke_keypair(&msek);
        let classical = seal_to_recipient(b"BEGIN:VCALENDAR...", &pubkey)
            .expect("seal")
            .to_canonical_bytes()
            .expect("canonical");
        let typed = SealedRecordBytes::verify(classical.clone()).expect("classical verifies");
        assert_eq!(typed.as_slice(), classical.as_slice(), "bytes preserved");
        assert_eq!(typed.len(), classical.len());
        assert!(!typed.is_empty());

        // X-Wing hybrid seal — the `EncryptToRecipientHybrid` /
        // `seal_event_body_xwing` shape.
        let xwing_pub = derive_recipient_xwing_keypair(&msek).public;
        let hybrid = seal_to_recipient_xwing(b"raw body", &xwing_pub)
            .expect("xwing seal")
            .to_canonical_bytes()
            .expect("canonical");
        let typed = SealedRecordBytes::verify(hybrid.clone()).expect("hybrid verifies");
        assert_eq!(typed.into_inner(), hybrid);
    }

    #[test]
    fn sealed_record_bytes_verify_rejects_everything_unsealed() {
        // The exact payload shapes a forgetful ingest site would pass raw:
        // RFC 5322 mail, iCalendar, vCard, the `<u32 BE len><bytes>`
        // canonical-token-set hint serialization, and the empty slice.
        let raw_candidates: &[&[u8]] = &[
            b"From: a@x\r\nSubject: hi\r\n\r\nbody",
            b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:u\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            b"BEGIN:VCARD\r\nVERSION:4.0\r\nFN:X\r\nEND:VCARD\r\n",
            &[0, 0, 0, 5, b'h', b'e', b'l', b'l', b'o'],
            b"",
        ];
        for raw in raw_candidates {
            let err = SealedRecordBytes::verify(raw.to_vec())
                .expect_err("raw bytes must not mint SealedRecordBytes");
            assert!(matches!(err, UnwrapError::InvalidFormat(_)));
        }
    }

    #[test]
    fn errors_are_distinct_variants() {
        let aead_err: UnwrapError = UnwrapError::AeadFailed;
        let format_err: UnwrapError = UnwrapError::InvalidFormat("test".into());
        assert!(matches!(aead_err, UnwrapError::AeadFailed));
        assert!(matches!(format_err, UnwrapError::InvalidFormat(_)));
    }

    #[test]
    fn aad_binding_canonical_encoding_is_deterministic() {
        let actor = [0x42u8; 32];

        // wrapped-MSEK
        let aad1 = AadBinding::for_wrapped_msek(&actor, "cred-1").canonical_bytes();
        let aad2 = AadBinding::for_wrapped_msek(&actor, "cred-1").canonical_bytes();
        assert_eq!(aad1, aad2);
        // Different actor → different AAD.
        let actor2 = [0x43u8; 32];
        let aad3 = AadBinding::for_wrapped_msek(&actor2, "cred-1").canonical_bytes();
        assert_ne!(aad1, aad3);

        // mls-snapshot
        assert_eq!(
            AadBinding::for_mls_snapshot(&actor).canonical_bytes(),
            AadBinding::for_mls_snapshot(&actor).canonical_bytes()
        );

        // submission-token
        assert_eq!(
            AadBinding::for_submission_token(&actor, "c").canonical_bytes(),
            AadBinding::for_submission_token(&actor, "c").canonical_bytes()
        );

        // tls-cert
        assert_eq!(
            AadBinding::for_tls_cert("mta", "b1", "ex.com").canonical_bytes(),
            AadBinding::for_tls_cert("mta", "b1", "ex.com").canonical_bytes()
        );
    }
}

#[cfg(test)]
mod seal_tests {
    use super::*;

    fn small_argon2id() -> KdfParams {
        // Small for fast tests.
        KdfParams::Argon2id(Argon2idParams {
            m: 4096,
            t: 1,
            p: 1,
        })
    }

    #[test]
    fn seal_then_unseal_with_plain_credential() {
        let msek = [0xABu8; 32];
        let actor = [0x01u8; 32];
        let cred = CredentialInput::Plain(b"correct horse");
        let blob = seal_wrapped_msek(&msek, &actor, "cred-1", &cred, small_argon2id()).unwrap();
        let unwrapped = unseal_wrapped_msek(&blob, &cred).unwrap();
        assert_eq!(*unwrapped, msek);
    }

    #[test]
    fn seal_then_unseal_with_oauth_credential() {
        let msek = [0xCDu8; 32];
        let actor = [0x02u8; 32];
        let cred = CredentialInput::OauthBearer(b"high-entropy-token");
        let blob = seal_wrapped_msek(
            &msek,
            &actor,
            "cred-1",
            &cred,
            KdfParams::HkdfSha256(HkdfSha256Params),
        )
        .unwrap();
        let unwrapped = unseal_wrapped_msek(&blob, &cred).unwrap();
        assert_eq!(*unwrapped, msek);
    }

    #[test]
    fn wrong_credential_fails_aead_verify() {
        let msek = [0u8; 32];
        let actor = [0u8; 32];
        let cred = CredentialInput::Plain(b"correct");
        let wrong = CredentialInput::Plain(b"wrong");
        let blob = seal_wrapped_msek(&msek, &actor, "cred-1", &cred, small_argon2id()).unwrap();
        let err = unseal_wrapped_msek(&blob, &wrong).unwrap_err();
        assert!(matches!(err, UnwrapError::AeadFailed));
    }

    #[test]
    fn cross_credential_id_substitution_fails() {
        let msek = [0u8; 32];
        let actor = [0u8; 32];
        let cred = CredentialInput::Plain(b"correct");
        let mut blob = seal_wrapped_msek(&msek, &actor, "cred-1", &cred, small_argon2id()).unwrap();
        // Tamper the credential_id so the AAD differs from the
        // sealing-time AAD; AEAD must fail.
        blob.index.1 = "cred-2".into();
        let err = unseal_wrapped_msek(&blob, &cred).unwrap_err();
        assert!(matches!(err, UnwrapError::AeadFailed));
    }

    #[test]
    fn cross_actor_substitution_fails() {
        let msek = [0u8; 32];
        let actor = [0xAAu8; 32];
        let cred = CredentialInput::Plain(b"correct");
        let mut blob = seal_wrapped_msek(&msek, &actor, "cred-1", &cred, small_argon2id()).unwrap();
        blob.index.0 = vec![0xBBu8; 32];
        let err = unseal_wrapped_msek(&blob, &cred).unwrap_err();
        assert!(matches!(err, UnwrapError::AeadFailed));
    }

    #[test]
    fn round_trip_through_canonical_bytes() {
        let msek = [0xEEu8; 32];
        let actor = [0x77u8; 32];
        let cred = CredentialInput::Plain(b"correct");
        let blob = seal_wrapped_msek(&msek, &actor, "cred-1", &cred, small_argon2id()).unwrap();
        let bytes = blob.to_canonical_bytes().unwrap();
        let decoded = WrappedMsekBlob::from_canonical_bytes(&bytes).unwrap();
        let unwrapped = unseal_wrapped_msek(&decoded, &cred).unwrap();
        assert_eq!(*unwrapped, msek);
    }

    #[test]
    fn two_seals_with_same_inputs_produce_distinct_blobs() {
        // Per-provision uniqueness: each call to seal_wrapped_msek
        // MUST generate a fresh random salt and nonce, otherwise
        // identical-credential reseals would collide and AEAD nonce
        // reuse would weaken security. A future regression that
        // accidentally caches randomness would pass every other
        // test; this guards against that.
        let msek = [0u8; 32];
        let actor = [0u8; 32];
        let cred = CredentialInput::Plain(b"x");
        let a = seal_wrapped_msek(&msek, &actor, "c", &cred, small_argon2id()).unwrap();
        let b = seal_wrapped_msek(&msek, &actor, "c", &cred, small_argon2id()).unwrap();
        assert_ne!(a.salt.as_ref(), b.salt.as_ref(), "salt reused!");
        assert_ne!(a.nonce.as_ref(), b.nonce.as_ref(), "nonce reused!");
        assert_ne!(
            a.ciphertext.as_ref(),
            b.ciphertext.as_ref(),
            "ciphertext reused!"
        );
    }

    #[test]
    fn snapshot_roundtrip_succeeds() {
        let msek = [0x55u8; 32];
        let actor = [0x77u8; 32];
        let state = b"serialized read-only MLS state bytes";
        let blob = seal_mls_snapshot(state, &actor, &msek).unwrap();
        let opened = unseal_mls_snapshot(&blob, &msek).unwrap();
        assert_eq!(&*opened, state);
    }

    #[test]
    fn snapshot_with_wrong_msek_fails() {
        let msek = [0x55u8; 32];
        let wrong = [0x66u8; 32];
        let actor = [0x77u8; 32];
        let blob = seal_mls_snapshot(b"x", &actor, &msek).unwrap();
        let err = unseal_mls_snapshot(&blob, &wrong).unwrap_err();
        assert!(matches!(err, UnwrapError::AeadFailed));
    }

    #[test]
    fn snapshot_cross_actor_substitution_fails() {
        let msek = [0x55u8; 32];
        let actor = [0xAAu8; 32];
        let mut blob = seal_mls_snapshot(b"x", &actor, &msek).unwrap();
        blob.index.0 = vec![0xBBu8; 32];
        let err = unseal_mls_snapshot(&blob, &msek).unwrap_err();
        assert!(matches!(err, UnwrapError::AeadFailed));
    }

    #[test]
    fn snapshot_canonical_bytes_roundtrip() {
        let msek = [0x55u8; 32];
        let actor = [0x77u8; 32];
        let blob = seal_mls_snapshot(b"abc", &actor, &msek).unwrap();
        let bytes = blob.to_canonical_bytes().unwrap();
        let decoded = MlsSnapshotBlob::from_canonical_bytes(&bytes).unwrap();
        let opened = unseal_mls_snapshot(&decoded, &msek).unwrap();
        assert_eq!(&*opened, b"abc");
    }

    #[test]
    fn snapshot_two_seals_produce_distinct_blobs() {
        // Per-state-update uniqueness: re-encryption MUST use a fresh
        // random nonce so MSEK isn't subject to nonce reuse.
        let msek = [0x55u8; 32];
        let actor = [0x77u8; 32];
        let a = seal_mls_snapshot(b"state", &actor, &msek).unwrap();
        let b = seal_mls_snapshot(b"state", &actor, &msek).unwrap();
        assert_ne!(a.nonce.as_ref(), b.nonce.as_ref(), "nonce reused!");
        assert_ne!(
            a.ciphertext.as_ref(),
            b.ciphertext.as_ref(),
            "ciphertext reused!"
        );
    }

    // ── WebDAV served-set key blob (webdav-server.md § Key model) ──

    #[test]
    fn webdav_keys_roundtrip_succeeds() {
        let msek = [0x55u8; 32];
        let actor = [0x77u8; 32];
        let pt = WebdavKeysPlaintext::new(vec![ServedSetKeys {
            set_name: "docs".into(),
            read_only: false,
            keys: fauna_core::folder_keys::FolderContentKeys::genesis([0xAB; 32], 1_700_000_000),
        }]);
        let plaintext = pt.to_canonical_bytes().unwrap();
        let blob = seal_webdav_keys_blob(&plaintext, &actor, &msek).unwrap();
        let opened = unseal_webdav_keys_blob(&blob, &msek).unwrap();
        let decoded = WebdavKeysPlaintext::from_canonical_bytes(&opened).unwrap();
        assert_eq!(decoded, pt);
        assert_eq!(decoded.served_sets[0].keys.current_key(), &[0xAB; 32]);
    }

    #[test]
    fn webdav_keys_with_wrong_msek_fails() {
        let msek = [0x55u8; 32];
        let wrong = [0x66u8; 32];
        let actor = [0x77u8; 32];
        let blob = seal_webdav_keys_blob(b"x", &actor, &msek).unwrap();
        let err = unseal_webdav_keys_blob(&blob, &wrong).unwrap_err();
        assert!(matches!(err, UnwrapError::AeadFailed));
    }

    #[test]
    fn webdav_keys_cross_actor_substitution_fails() {
        let msek = [0x55u8; 32];
        let actor = [0xAAu8; 32];
        let mut blob = seal_webdav_keys_blob(b"x", &actor, &msek).unwrap();
        blob.index.0 = vec![0xBBu8; 32];
        let err = unseal_webdav_keys_blob(&blob, &msek).unwrap_err();
        assert!(matches!(err, UnwrapError::AeadFailed));
    }

    #[test]
    fn webdav_keys_canonical_bytes_roundtrip() {
        let msek = [0x55u8; 32];
        let actor = [0x77u8; 32];
        let blob = seal_webdav_keys_blob(b"abc", &actor, &msek).unwrap();
        let bytes = blob.to_canonical_bytes().unwrap();
        let decoded = WebdavKeysBlob::from_canonical_bytes(&bytes).unwrap();
        let opened = unseal_webdav_keys_blob(&decoded, &msek).unwrap();
        assert_eq!(&*opened, b"abc");
    }

    #[test]
    fn webdav_keys_two_seals_produce_distinct_blobs() {
        // Fresh random nonce per (re)provision — no MSEK nonce reuse.
        let msek = [0x55u8; 32];
        let actor = [0x77u8; 32];
        let a = seal_webdav_keys_blob(b"state", &actor, &msek).unwrap();
        let b = seal_webdav_keys_blob(b"state", &actor, &msek).unwrap();
        assert_ne!(a.nonce.as_ref(), b.nonce.as_ref(), "nonce reused!");
        assert_ne!(
            a.ciphertext.as_ref(),
            b.ciphertext.as_ref(),
            "ciphertext reused!"
        );
    }

    #[test]
    fn webdav_keys_aad_domain_separated_from_mls_snapshot() {
        // A webdav-keys ciphertext must not open as an mls-snapshot (and vice
        // versa) even at the same (actor, msek) — the distinct AAD kind tag is
        // the only thing standing between the two same-MSEK blob families.
        let actor = [0x33u8; 32];
        assert_ne!(
            AadBinding::for_webdav_keys(&actor).canonical_bytes(),
            AadBinding::for_mls_snapshot(&actor).canonical_bytes(),
        );
        let msek = [0x99u8; 32];
        // Seal as webdav-keys, then hand the raw ciphertext to the snapshot
        // opener via a same-shape blob: the AAD mismatch fails the AEAD verify.
        let wk = seal_webdav_keys_blob(b"payload", &actor, &msek).unwrap();
        let as_snapshot = MlsSnapshotBlob {
            version: wk.version,
            kind: "mls-snapshot".into(),
            index: MlsSnapshotIndex(wk.index.0.clone()),
            nonce: wk.nonce.clone(),
            ciphertext: wk.ciphertext.clone(),
        };
        assert!(matches!(
            unseal_mls_snapshot(&as_snapshot, &msek).unwrap_err(),
            UnwrapError::AeadFailed
        ));
    }

    #[test]
    fn webdav_keys_rejects_wrong_kind() {
        let msek = [0x55u8; 32];
        let actor = [0x77u8; 32];
        let mut blob = seal_webdav_keys_blob(b"x", &actor, &msek).unwrap();
        blob.kind = "mls-snapshot".into();
        assert!(matches!(
            unseal_webdav_keys_blob(&blob, &msek).unwrap_err(),
            UnwrapError::InvalidFormat(_)
        ));
    }

    use super::submission_token::fresh_signed_token;
    use ed25519_dalek::SigningKey;

    fn fresh_signing_key() -> SigningKey {
        use rand::RngCore;
        let mut secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret);
        SigningKey::from_bytes(&secret)
    }

    #[test]
    fn submission_token_round_trip() {
        let sk = fresh_signing_key();
        let vk = sk.verifying_key();
        let actor = [0x33u8; 32];
        let cred = CredentialInput::Plain(b"correct");
        let token = fresh_signed_token(&sk, &actor, "cred-1");
        let blob =
            seal_submission_token(&token, &actor, "cred-1", &cred, small_argon2id()).unwrap();
        let unwrapped = unseal_submission_token(&blob, &cred, &vk).unwrap();
        assert_eq!(unwrapped.actor_id, token.actor_id);
        assert_eq!(unwrapped.credential_id, token.credential_id);
    }

    #[test]
    fn submission_token_wrong_credential_fails_aead() {
        let sk = fresh_signing_key();
        let actor = [0x33u8; 32];
        let cred = CredentialInput::Plain(b"correct");
        let wrong = CredentialInput::Plain(b"wrong");
        let token = fresh_signed_token(&sk, &actor, "cred-1");
        let blob =
            seal_submission_token(&token, &actor, "cred-1", &cred, small_argon2id()).unwrap();
        let err = unseal_submission_token(&blob, &wrong, &sk.verifying_key()).unwrap_err();
        assert!(matches!(err, UnwrapError::AeadFailed));
    }

    #[test]
    fn submission_token_wrong_signing_pubkey_fails() {
        let signer = fresh_signing_key();
        let other = fresh_signing_key();
        let actor = [0x33u8; 32];
        let cred = CredentialInput::Plain(b"correct");
        let token = fresh_signed_token(&signer, &actor, "cred-1");
        let blob =
            seal_submission_token(&token, &actor, "cred-1", &cred, small_argon2id()).unwrap();
        let err = unseal_submission_token(&blob, &cred, &other.verifying_key()).unwrap_err();
        assert!(matches!(err, UnwrapError::SignatureFailed));
    }

    #[test]
    fn submission_token_seal_rejects_actor_mismatch() {
        let sk = fresh_signing_key();
        let actor_a = [0x33u8; 32];
        let actor_b = [0x44u8; 32];
        let cred = CredentialInput::Plain(b"correct");
        // Token signed for actor_a but seal called with actor_b.
        let token = fresh_signed_token(&sk, &actor_a, "cred-1");
        let err =
            seal_submission_token(&token, &actor_b, "cred-1", &cred, small_argon2id()).unwrap_err();
        assert!(matches!(err, WrapError::InvalidInput(_)));
    }

    use super::envelope::generate_x25519_keypair;

    fn fresh_atproto_bundle(actor_id: &[u8; 32]) -> AtprotoIdentityKeyBundle {
        AtprotoIdentityKeyBundle {
            actor_id: actor_id.to_vec(),
            signing_priv: vec![0xA1u8; 32],
            signing_curve: "k256".into(),
            signing_pub_did_key: "did:key:zQ3shokFPEXAMPLEsigning".into(),
            rotation_priv: vec![0xB2u8; 32],
            rotation_curve: "k256".into(),
            rotation_pub_did_key: "did:key:zQ3shokFPEXAMPLErotation".into(),
            issued_at: 1_700_000_000,
        }
    }

    /// The published halves `fresh_atproto_bundle` carries — what an honest
    /// nest's identity row would record for it.
    fn published_keys_of(bundle: &AtprotoIdentityKeyBundle) -> AtprotoIdentityPublishedKeys<'_> {
        AtprotoIdentityPublishedKeys {
            signing_pub_did_key: &bundle.signing_pub_did_key,
            rotation_pub_did_key: &bundle.rotation_pub_did_key,
        }
    }

    #[test]
    fn atproto_identity_round_trip() {
        let (sk, pk) = generate_x25519_keypair();
        let actor = [0x33u8; 32];
        let bundle = fresh_atproto_bundle(&actor);
        let blob = seal_atproto_identity(&bundle, &actor, &pk).unwrap();
        assert_eq!(blob.kind, AtprotoIdentityBlob::KIND);
        assert_eq!(blob.index.0.as_slice(), actor.as_slice());
        let bytes = blob.to_canonical_bytes().unwrap();
        let decoded = AtprotoIdentityBlob::from_canonical_bytes(&bytes).unwrap();
        let opened = unseal_atproto_identity(&decoded, &sk, &published_keys_of(&bundle)).unwrap();
        assert_eq!(opened.signing_priv, bundle.signing_priv);
        assert_eq!(opened.rotation_priv, bundle.rotation_priv);
        assert_eq!(opened.signing_pub_did_key, bundle.signing_pub_did_key);
        assert_eq!(opened.rotation_pub_did_key, bundle.rotation_pub_did_key);
        assert_eq!(opened.actor_id, actor.to_vec());
    }

    #[test]
    fn atproto_identity_wrong_recipient_fails() {
        let (_, pk) = generate_x25519_keypair();
        let (other_sk, _) = generate_x25519_keypair();
        let actor = [0x33u8; 32];
        let bundle = fresh_atproto_bundle(&actor);
        let blob = seal_atproto_identity(&bundle, &actor, &pk).unwrap();
        let err =
            unseal_atproto_identity(&blob, &other_sk, &published_keys_of(&bundle)).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn atproto_identity_seal_rejects_actor_mismatch() {
        let (_, pk) = generate_x25519_keypair();
        let bundle = fresh_atproto_bundle(&[0x33u8; 32]);
        let err = seal_atproto_identity(&bundle, &[0x44u8; 32], &pk).unwrap_err();
        assert!(matches!(err, WrapError::InvalidInput(_)));
    }

    #[test]
    fn atproto_identity_edited_index_fails() {
        let (sk, pk) = generate_x25519_keypair();
        let actor = [0x33u8; 32];
        let bundle = fresh_atproto_bundle(&actor);
        let mut blob = seal_atproto_identity(&bundle, &actor, &pk).unwrap();
        // Swap the on-the-wire actor index — the AAD/info recomputed at unseal
        // time must no longer match. This is ALL the index binding detects: an
        // edited index. A whole blob served for the wrong identity carries a
        // self-consistent index and is the next test's business.
        blob.index = AtprotoIdentityIndex(ByteBuf::from(vec![0x44u8; 32]));
        let err = unseal_atproto_identity(&blob, &sk, &published_keys_of(&bundle)).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    /// The whole-blob substitution: identity X's sealed blob, untouched and so
    /// self-consistent, served where identity Y's was asked for. The HPKE open
    /// succeeds — the blob is genuinely sealed to this bridge — and the refusal
    /// has to come from the published-key binding.
    #[test]
    fn atproto_identity_whole_blob_substitution_is_refused() {
        let (sk, pk) = generate_x25519_keypair();
        let actor_x = [0x33u8; 32];
        let x = fresh_atproto_bundle(&actor_x);
        let x_blob = seal_atproto_identity(&x, &actor_x, &pk).unwrap();

        let y_published = AtprotoIdentityPublishedKeys {
            signing_pub_did_key: "did:key:zQ3shokFPEXAMPLEsigningOfY",
            rotation_pub_did_key: "did:key:zQ3shokFPEXAMPLErotationOfY",
        };
        let err = unseal_atproto_identity(&x_blob, &sk, &y_published).unwrap_err();
        assert!(
            matches!(err, UnwrapError::PublishedKeyMismatch(ref m) if m.contains("signing key")),
            "X's whole blob must not open as Y's: {err:?}"
        );

        // Either half alone is a mismatch too — a blob is one identity's PAIR.
        for half in [
            AtprotoIdentityPublishedKeys {
                signing_pub_did_key: &x.signing_pub_did_key,
                ..y_published
            },
            AtprotoIdentityPublishedKeys {
                rotation_pub_did_key: &x.rotation_pub_did_key,
                ..y_published
            },
        ] {
            assert!(unseal_atproto_identity(&x_blob, &sk, &half).is_err());
        }
    }

    /// An identity row that records no published keys gives the caller nothing
    /// to bind to. That must fail closed rather than read as "no expectation":
    /// a bundle carrying empty pubkey strings would otherwise match it.
    #[test]
    fn atproto_identity_empty_expectation_fails_closed() {
        let (sk, pk) = generate_x25519_keypair();
        let actor = [0x33u8; 32];
        let mut bundle = fresh_atproto_bundle(&actor);
        bundle.signing_pub_did_key = String::new();
        bundle.rotation_pub_did_key = String::new();
        let blob = seal_atproto_identity(&bundle, &actor, &pk).unwrap();
        let err = unseal_atproto_identity(&blob, &sk, &published_keys_of(&bundle)).unwrap_err();
        assert!(matches!(err, UnwrapError::PublishedKeyMismatch(_)));
    }

    /// The succession shape: the row moved to a successor, so the caller asks on
    /// behalf of an actor the blob has never heard of — and both ids inside still
    /// name the predecessor. It opens, because the binding is the published keys
    /// that moved with the row, not the actor.
    #[test]
    fn atproto_identity_opens_for_a_successor_the_blob_never_names() {
        let (sk, pk) = generate_x25519_keypair();
        let predecessor = [0x33u8; 32];
        let bundle = fresh_atproto_bundle(&predecessor);
        let blob = seal_atproto_identity(&bundle, &predecessor, &pk).unwrap();
        let opened = unseal_atproto_identity(&blob, &sk, &published_keys_of(&bundle)).unwrap();
        assert_eq!(
            opened.actor_id,
            predecessor.to_vec(),
            "provenance, never compared"
        );
    }

    #[test]
    fn atproto_identity_cannot_open_as_tls_cert() {
        // Cross-shape domain separation: an atproto-identity ciphertext
        // re-labeled as a TLS cert blob must fail AEAD-open, not decode.
        let (sk, pk) = generate_x25519_keypair();
        let actor = [0x33u8; 32];
        let blob = seal_atproto_identity(&fresh_atproto_bundle(&actor), &actor, &pk).unwrap();
        let forged = TlsCertBlob {
            version: blob.version,
            kind: "tls-cert".into(),
            index: TlsCertIndex("mta".into(), "bridge-1".into(), "example.com".into()),
            hpke: blob.hpke.clone(),
        };
        let err = unseal_tls_cert(&forged, &sk).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn atproto_session_secret_round_trip() {
        let (sk, pk) = generate_x25519_keypair();
        let bundle = AtprotoSessionSecretBundle {
            secret: vec![0xC4u8; 32],
            issued_at: 1_700_000_000,
        };
        let blob = seal_atproto_session_secret(&bundle, "atproto.pds", "pds-1", &pk).unwrap();
        assert_eq!(blob.kind, AtprotoSessionSecretBlob::KIND);
        assert_eq!(blob.index.0, "atproto.pds");
        assert_eq!(blob.index.1, "pds-1");
        assert!(blob.hpke.kem_suite.is_standard());
        let bytes = blob.to_canonical_bytes().unwrap();
        let decoded = AtprotoSessionSecretBlob::from_canonical_bytes(&bytes).unwrap();
        let opened = unseal_atproto_session_secret(&decoded, &sk).unwrap();
        assert_eq!(opened.secret, bundle.secret);
        assert_eq!(opened.issued_at, bundle.issued_at);
    }

    #[test]
    fn atproto_session_secret_seal_rejects_bad_length() {
        let (_, pk) = generate_x25519_keypair();
        let bundle = AtprotoSessionSecretBundle {
            secret: vec![0xC4u8; 16],
            issued_at: 0,
        };
        let err = seal_atproto_session_secret(&bundle, "atproto.pds", "pds-1", &pk).unwrap_err();
        assert!(matches!(err, WrapError::InvalidInput(_)));
    }

    #[test]
    fn atproto_session_secret_wrong_recipient_fails() {
        let (_, pk) = generate_x25519_keypair();
        let (other_sk, _) = generate_x25519_keypair();
        let bundle = AtprotoSessionSecretBundle {
            secret: vec![0xC4u8; 32],
            issued_at: 0,
        };
        let blob = seal_atproto_session_secret(&bundle, "atproto.pds", "pds-1", &pk).unwrap();
        let err = unseal_atproto_session_secret(&blob, &other_sk).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn atproto_session_secret_bridge_substitution_fails() {
        // Swap the on-the-wire bridge index — the AAD/info recomputed at
        // unseal time must no longer match (deployment A's PDS secret can't
        // open under deployment B's bridge identity).
        let (sk, pk) = generate_x25519_keypair();
        let bundle = AtprotoSessionSecretBundle {
            secret: vec![0xC4u8; 32],
            issued_at: 0,
        };
        let mut blob = seal_atproto_session_secret(&bundle, "atproto.pds", "pds-1", &pk).unwrap();
        blob.index = AtprotoSessionSecretIndex("atproto.pds".into(), "pds-2".into());
        let err = unseal_atproto_session_secret(&blob, &sk).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn atproto_session_secret_cannot_open_as_mail_record() {
        // Cross-shape domain separation: the session-secret ciphertext must
        // never open on the generic mail-record path (the reason this blob
        // has its own AAD kind instead of riding `seal_to_recipient`).
        let (sk, pk) = generate_x25519_keypair();
        let bundle = AtprotoSessionSecretBundle {
            secret: vec![0xC4u8; 32],
            issued_at: 0,
        };
        let blob = seal_atproto_session_secret(&bundle, "atproto.pds", "pds-1", &pk).unwrap();
        let forged = MailRecordEnvelope {
            version: blob.version,
            kind: "mail-record".into(),
            hpke: blob.hpke.clone(),
        };
        let err = unseal_mail_record(&forged, &sk).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    /// The happy path `start_export_session` runs: seal the session key to the
    /// user's own current MSEK generation, open it back from the standing set.
    #[test]
    fn export_session_key_round_trips_under_the_standing_set() {
        let msek = [0x3au8; 32];
        let actor = [0x91u8; 32];
        let session_key = [0x5bu8; 32];
        let xwing = derive_recipient_xwing_keypair(&msek);
        let blob = seal_export_session_key(&session_key, &actor, &xwing.public).unwrap();
        assert_eq!(blob.kind, ExportSessionKeyBlob::KIND);
        assert_eq!(blob.index.0.as_slice(), actor.as_slice());
        assert_eq!(blob.hpke.kem_suite.kem, FAUNA_KEM_XWING);

        // Through the wire encoding the nest stores verbatim.
        let bytes = blob.to_canonical_bytes().unwrap();
        let decoded = ExportSessionKeyBlob::from_canonical_bytes(&bytes).unwrap();
        let standing = derive_standing_mail_keypairs(&[msek]);
        assert_eq!(
            unseal_export_session_key(&decoded, &standing).unwrap(),
            session_key
        );
    }

    /// The rotation case the standing set exists for: the wizard ran under the
    /// old MSEK, the user opens the archive from a client that has rotated. A
    /// single-generation opener would leave that download unreadable.
    #[test]
    fn export_session_key_opens_from_a_rotated_client_within_grace() {
        let msek_old = [0x11u8; 32];
        let msek_new = [0x22u8; 32];
        let actor = [0x44u8; 32];
        let session_key = [0x77u8; 32];
        let blob = seal_export_session_key(
            &session_key,
            &actor,
            &derive_recipient_xwing_keypair(&msek_old).public,
        )
        .unwrap();

        // Current generation only — the rotation has blinded this client.
        let current_only = derive_standing_mail_keypairs(&[msek_new]);
        assert!(matches!(
            unseal_export_session_key(&blob, &current_only).unwrap_err(),
            UnwrapError::HpkeFailed
        ));

        // Current + the grace generation the rotation retained: opens.
        let with_grace = derive_standing_mail_keypairs(&[msek_new, msek_old]);
        assert_eq!(
            unseal_export_session_key(&blob, &with_grace).unwrap(),
            session_key
        );
    }

    /// Cross-shape domain separation, in the direction that actually has a
    /// shared recipient: an export session key and an at-rest mail record are
    /// both sealed to the user's own standing pair, so only the AAD kind tag
    /// keeps one from opening as the other.
    #[test]
    fn export_session_key_cannot_open_as_a_mail_record_or_vice_versa() {
        let msek = [0x6eu8; 32];
        let actor = [0x6fu8; 32];
        let standing = derive_standing_mail_keypairs(&[msek]);
        let xwing = derive_recipient_xwing_keypair(&msek);

        let key_blob = seal_export_session_key(&[0x01u8; 32], &actor, &xwing.public).unwrap();
        let forged_record = MailRecordEnvelope {
            version: key_blob.version,
            kind: "mail-record".into(),
            hpke: key_blob.hpke.clone(),
        };
        assert!(
            open_mail_record_standing(&forged_record, &standing, &[], None).is_none(),
            "a session-key ciphertext must not open on the mail-record path"
        );

        let record = seal_to_recipient_xwing(b"body bytes", &xwing.public).unwrap();
        let forged_key = ExportSessionKeyBlob {
            version: record.version,
            kind: ExportSessionKeyBlob::KIND.into(),
            index: ExportSessionKeyIndex(ByteBuf::from(actor.to_vec())),
            hpke: record.hpke.clone(),
        };
        assert!(matches!(
            unseal_export_session_key(&forged_key, &standing).unwrap_err(),
            UnwrapError::HpkeFailed
        ));
    }

    /// A nest that re-indexed the blob to another actor must fail AEAD-open,
    /// not hand back a key the frames would then be judged against.
    #[test]
    fn export_session_key_refuses_a_retargeted_actor_index() {
        let msek = [0x8cu8; 32];
        let xwing = derive_recipient_xwing_keypair(&msek);
        let mut blob =
            seal_export_session_key(&[0x02u8; 32], &[0xaau8; 32], &xwing.public).unwrap();
        blob.index = ExportSessionKeyIndex(ByteBuf::from(vec![0xbbu8; 32]));
        let standing = derive_standing_mail_keypairs(&[msek]);
        assert!(matches!(
            unseal_export_session_key(&blob, &standing).unwrap_err(),
            UnwrapError::HpkeFailed
        ));

        // And a short index is refused at decode, before any AEAD work.
        let mut short =
            seal_export_session_key(&[0x03u8; 32], &[0xccu8; 32], &xwing.public).unwrap();
        short.index = ExportSessionKeyIndex(ByteBuf::from(vec![0u8; 16]));
        let bytes = short.to_canonical_bytes().unwrap();
        assert!(matches!(
            ExportSessionKeyBlob::from_canonical_bytes(&bytes).unwrap_err(),
            UnwrapError::InvalidFormat(_)
        ));
    }

    #[test]
    fn spam_model_copy_round_trip_classical() {
        let (sk, pk) = generate_x25519_keypair();
        let owner = [0x22u8; 32];
        let model = br#"{"v":1,"ngrams":{}}"#;
        let blob = seal_spam_model_copy(model, &owner, &pk, None).unwrap();
        assert_eq!(blob.kind, SpamModelCopyBlob::KIND);
        assert_eq!(blob.index.0.as_slice(), owner.as_slice());
        assert!(blob.hpke.kem_suite.is_standard());
        let bytes = blob.to_canonical_bytes().unwrap();
        let decoded = SpamModelCopyBlob::from_canonical_bytes(&bytes).unwrap();
        let opened = unseal_spam_model_copy(&decoded, &sk, None).unwrap();
        assert_eq!(opened, model);
    }

    #[test]
    fn spam_model_copy_round_trip_xwing() {
        let (sk, pk) = generate_x25519_keypair();
        let (mlkem_dk, mlkem_ek) = fauna_pq_kem::derive_mlkem768_keypair_from_ikm(
            b"spam-copy-holder-seed",
            "fauna.test.capability-holder.v1",
        );
        let owner = [0x22u8; 32];
        let model = br#"{"v":1,"ngrams":{"a b":1}}"#;
        let blob = seal_spam_model_copy(model, &owner, &pk, Some(&mlkem_ek)).unwrap();
        assert_eq!(blob.hpke.kem_suite.kem, FAUNA_KEM_XWING);
        let opened = unseal_spam_model_copy(&blob, &sk, Some(&mlkem_dk)).unwrap();
        assert_eq!(opened, model);
        // A hybrid copy opened without the ML-KEM half is a typed refusal,
        // never a silent mis-decrypt.
        let err = unseal_spam_model_copy(&blob, &sk, None).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }

    #[test]
    fn spam_model_copy_reattribution_fails() {
        // Tampering the on-the-wire owner index (presenting actor X's copy as
        // actor Y's — the k-anon double-count path) recomputes a different
        // AAD at open time and fails.
        let (sk, pk) = generate_x25519_keypair();
        let owner = [0x22u8; 32];
        let mut blob = seal_spam_model_copy(b"model", &owner, &pk, None).unwrap();
        blob.index = SpamModelCopyIndex(ByteBuf::from(vec![0x33u8; 32]));
        let err = unseal_spam_model_copy(&blob, &sk, None).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn spam_model_copy_wrong_holder_fails() {
        let (_, pk) = generate_x25519_keypair();
        let (other_sk, _) = generate_x25519_keypair();
        let owner = [0x22u8; 32];
        let blob = seal_spam_model_copy(b"model", &owner, &pk, None).unwrap();
        let err = unseal_spam_model_copy(&blob, &other_sk, None).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn grant_blob_rejects_key_bearing_spam_model_tuple() {
        // The definition-of-success negative at the mint layer: no
        // WrappedScopeKey may ever carry the spam-model kind — a key-bearing
        // spam-model tuple is `content.read{mail}` under another name
        // (`key-material-hierarchy.md` § Don't do these, resolved 2026-07-13).
        let owner = [0x11u8; 32];
        let grant_id = [0x22u8; 16];
        let (_, holder_pk) = generate_x25519_keypair();
        let tuple = ScopeTuple {
            class: ScopeTuple::CLASS_CONTENT_READ.into(),
            kind: Some(ScopeTuple::KIND_SPAM_MODEL.into()),
            tier: None,
            set: None,
            factor: None,
        };
        let err = build_grant_blob(
            &owner,
            &grant_id,
            &holder_pk,
            None,
            GrantWindow(0, 1),
            &[(tuple.clone(), Some(vec![0xABu8; 32]))],
        )
        .unwrap_err();
        assert!(matches!(err, WrapError::InvalidInput(_)));
        // The keyless shape mints cleanly: the tuple appears in scope with no
        // wrapped key, so the grant conveys no key material at all.
        let blob = build_grant_blob(
            &owner,
            &grant_id,
            &holder_pk,
            None,
            GrantWindow(0, 1),
            &[(tuple.clone(), None)],
        )
        .unwrap();
        assert_eq!(blob.scope, vec![tuple]);
        assert!(
            blob.wrapped_keys.is_empty(),
            "a spam-model-only grant carries zero keys — nothing in it can open mail"
        );
    }

    #[test]
    fn grant_blob_rejects_mixed_regime_mail_tuple() {
        // The content-sealing-epochs § 2 mint policy: a mail (or calendar)
        // grant is bounded (per-epoch wraps only) XOR master-key (one
        // standing wrap). A standing wrap alongside epoch wraps would let the
        // holder open every epoch forever while the window claims a bound.
        let owner = [0x11u8; 32];
        let grant_id = [0x22u8; 16];
        let (_, holder_pk) = generate_x25519_keypair();
        let mail = ScopeTuple::mail();
        let mixed = vec![(None, vec![0xAAu8; 32]), (Some(2958), vec![0xBBu8; 32])];
        let err = build_grant_blob_with_epochs(
            &owner,
            &grant_id,
            &holder_pk,
            None,
            GrantWindow(0, 1),
            &[(mail.clone(), mixed)],
        )
        .unwrap_err();
        assert!(matches!(err, WrapError::InvalidInput(_)));

        // Both pure regimes mint cleanly: bounded (all Some) …
        let bounded = build_grant_blob_with_epochs(
            &owner,
            &grant_id,
            &holder_pk,
            None,
            GrantWindow(0, 1),
            &[(
                mail.clone(),
                vec![
                    (Some(2958), vec![0xAAu8; 32]),
                    (Some(2959), vec![0xBBu8; 32]),
                ],
            )],
        )
        .unwrap();
        assert_eq!(bounded.wrapped_keys.len(), 2);
        assert!(bounded.wrapped_keys.iter().all(|w| w.epoch.is_some()));
        // … and master-key (one None).
        let master = build_grant_blob_with_epochs(
            &owner,
            &grant_id,
            &holder_pk,
            None,
            GrantWindow(0, 1),
            &[(mail, vec![(None, vec![0xAAu8; 32])])],
        )
        .unwrap();
        assert_eq!(master.wrapped_keys.len(), 1);
        assert!(master.wrapped_keys[0].epoch.is_none());

        // Calendar is a wall-clock-epoch kind too.
        let calendar = ScopeTuple {
            class: ScopeTuple::CLASS_CONTENT_READ.into(),
            kind: Some(ScopeTuple::KIND_CALENDAR.into()),
            tier: None,
            set: None,
            factor: None,
        };
        let err = build_grant_blob_with_epochs(
            &owner,
            &grant_id,
            &holder_pk,
            None,
            GrantWindow(0, 1),
            &[(
                calendar,
                vec![(None, vec![0xAAu8; 32]), (Some(1), vec![0xBBu8; 32])],
            )],
        )
        .unwrap_err();
        assert!(matches!(err, WrapError::InvalidInput(_)));
    }

    #[test]
    fn duplicate_epoch_wraps_are_refused_for_wall_clock_kinds_but_not_folder() {
        // Amendment 2026-07-19: one wrap per (scope, epoch) is universal for
        // mail/calendar — a boundary epoch's cross-generation coverage rides
        // INSIDE the single wrap's payload, never as a second wrap the renew
        // replace would nondeterministically drop. The folder kind keeps its
        // CRDT-merge tolerance (two keys can legitimately share a version).
        let owner = [0x44u8; 32];
        let grant_id = [0x05u8; 16];
        let (_sk, holder_pk) = generate_x25519_keypair();
        let mail = ScopeTuple::mail();
        let err = build_grant_blob_with_epochs(
            &owner,
            &grant_id,
            &holder_pk,
            None,
            GrantWindow(0, 1),
            &[(
                mail,
                vec![
                    (Some(2960), vec![0xAAu8; 32]),
                    (Some(2960), vec![0xBBu8; 32]),
                ],
            )],
        )
        .unwrap_err();
        assert!(matches!(err, WrapError::InvalidInput(_)));

        let folder = ScopeTuple {
            class: ScopeTuple::CLASS_CONTENT_READ.into(),
            kind: Some(ScopeTuple::KIND_FOLDER.into()),
            tier: None,
            set: Some("photos".into()),
            factor: None,
        };
        let blob = build_grant_blob_with_epochs(
            &owner,
            &grant_id,
            &holder_pk,
            None,
            GrantWindow(0, 1),
            &[(
                folder,
                vec![(Some(3), vec![0xAAu8; 32]), (Some(3), vec![0xBBu8; 32])],
            )],
        )
        .expect("folder keeps its concurrent-rotation duplicate-version tolerance");
        assert_eq!(blob.wrapped_keys.len(), 2);
    }

    #[test]
    fn renewal_wraps_apply_the_mint_policy_and_open() {
        // build_renewal_wraps is the renew-append path's builder: same policy
        // guards, same wrap selection, loose keys the nest dedups.
        let owner = [0x33u8; 32];
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let mail = ScopeTuple::mail();
        let wraps = build_renewal_wraps(
            &owner,
            &holder_pk,
            None,
            &[(
                mail.clone(),
                vec![
                    (Some(3000), vec![0x01u8; 32]),
                    (Some(3001), vec![0x02u8; 32]),
                ],
            )],
        )
        .unwrap();
        assert_eq!(wraps.len(), 2);
        assert_eq!(wraps[0].epoch, Some(3000));
        assert_eq!(
            unseal_capability(&wraps[0], &owner, &holder_sk).unwrap(),
            vec![0x01u8; 32]
        );
        assert_eq!(
            unseal_capability(&wraps[1], &owner, &holder_sk).unwrap(),
            vec![0x02u8; 32]
        );

        // The same guards bite here: mixed mail regimes …
        let err = build_renewal_wraps(
            &owner,
            &holder_pk,
            None,
            &[(
                mail,
                vec![(None, vec![0x01u8; 32]), (Some(3000), vec![0x02u8; 32])],
            )],
        )
        .unwrap_err();
        assert!(matches!(err, WrapError::InvalidInput(_)));
        // … and a key-bearing spam-model tuple.
        let spam = ScopeTuple {
            class: ScopeTuple::CLASS_CONTENT_READ.into(),
            kind: Some(ScopeTuple::KIND_SPAM_MODEL.into()),
            tier: None,
            set: None,
            factor: None,
        };
        let err = build_renewal_wraps(
            &owner,
            &holder_pk,
            None,
            &[(spam, vec![(None, vec![0x01u8; 32])])],
        )
        .unwrap_err();
        assert!(matches!(err, WrapError::InvalidInput(_)));
    }

    fn fresh_tls_bundle() -> TlsCertBundle {
        TlsCertBundle {
            cert_chain: b"-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----".to_vec(),
            priv_key: vec![0u8; 64],
            expires_at: 1_700_000_000 + 90 * 86_400,
            issued_at: 1_700_000_000,
        }
    }

    #[test]
    fn tls_round_trip() {
        let (sk, pk) = generate_x25519_keypair();
        let bundle = fresh_tls_bundle();
        let blob = seal_tls_cert(&bundle, "mta", "bridge-1", "example.com", &pk).unwrap();
        let opened = unseal_tls_cert(&blob, &sk).unwrap();
        assert_eq!(opened.cert_chain, bundle.cert_chain);
        assert_eq!(opened.priv_key, bundle.priv_key);
        assert_eq!(opened.issued_at, bundle.issued_at);
    }

    #[test]
    fn tls_wrong_recipient_fails() {
        let (_, pk) = generate_x25519_keypair();
        let (other_sk, _) = generate_x25519_keypair();
        let bundle = fresh_tls_bundle();
        let blob = seal_tls_cert(&bundle, "mta", "bridge-1", "example.com", &pk).unwrap();
        let err = unseal_tls_cert(&blob, &other_sk).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn tls_cross_role_fails() {
        // A blob sealed for ("mta", "bridge-1", "example.com") cannot
        // be opened as ("mda", "bridge-1", "example.com").
        let (sk, pk) = generate_x25519_keypair();
        let bundle = fresh_tls_bundle();
        let mut blob = seal_tls_cert(&bundle, "mta", "bridge-1", "example.com", &pk).unwrap();
        blob.index = TlsCertIndex("mda".into(), "bridge-1".into(), "example.com".into());
        let err = unseal_tls_cert(&blob, &sk).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    // ── seal_capability / unseal_capability (capability-mediated content
    //    processing, design § Phase 2 Step 2 § 2.1–2.2) ──

    fn mail_read_scope() -> ScopeTuple {
        ScopeTuple::mail()
    }

    fn post_read_scope(tier: &str) -> ScopeTuple {
        ScopeTuple {
            class: "content.read".into(),
            kind: Some("post".into()),
            tier: Some(tier.into()),
            set: None,
            factor: None,
        }
    }

    fn label_write_scope() -> ScopeTuple {
        // The one keyless class — writing a label needs no seal (§ 2.1 table).
        ScopeTuple {
            class: "content.label-write".into(),
            kind: None,
            tier: None,
            set: None,
            factor: None,
        }
    }

    #[test]
    fn build_grant_blob_round_trips_and_omits_keyless() {
        // The client-side mint's "build GrantBlob" half: assemble a grant from
        // its declared scope + minimal payloads, seal each key-bearing tuple to
        // the holder, canonical-round-trip, and prove each wrapped key opens back
        // to its exact payload with the holder secret (design § 2.6).
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let owner = [0x77u8; 32];
        let grant_id = [0x01u8; 16];
        let mail_payload = vec![0xAAu8; 32]; // e.g. the recipient-mail HPKE secret
        let post_payload = vec![0xBBu8; 32]; // e.g. a tier period_key
        let scopes = vec![
            (mail_read_scope(), Some(mail_payload.clone())),
            (post_read_scope("tier-1"), Some(post_payload.clone())),
            (label_write_scope(), None), // keyless: in scope, not in wrapped_keys
        ];

        let blob = build_grant_blob(
            &owner,
            &grant_id,
            &holder_pk,
            None, // classical wrap: holder published no ML-KEM ek
            GrantWindow(100, 200),
            &scopes,
        )
        .unwrap();

        // All three tuples are declared; only the two key-bearing ones carry keys.
        assert_eq!(blob.version, BLOB_FORMAT_VERSION);
        assert_eq!(blob.kind, GrantBlob::KIND);
        assert_eq!(blob.scope.len(), 3);
        assert_eq!(blob.wrapped_keys.len(), 2);
        assert_eq!(blob.holder.as_ref(), holder_pk.as_slice());
        assert_eq!(blob.index.0, owner.to_vec());
        assert_eq!(blob.index.1, grant_id.to_vec());
        assert_eq!(blob.window, GrantWindow(100, 200));

        // Canonical wire round-trip (what fauna.capabilities.mint carries).
        let bytes = blob.to_canonical_bytes().unwrap();
        let decoded = GrantBlob::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.scope, blob.scope);
        assert_eq!(decoded.wrapped_keys.len(), 2);

        // Each wrapped key opens to its exact payload (order preserved), and
        // every wrapped key is master-key (epoch None) today.
        assert_eq!(decoded.wrapped_keys[0].scope, mail_read_scope());
        assert_eq!(decoded.wrapped_keys[0].epoch, None);
        assert_eq!(
            unseal_capability(&decoded.wrapped_keys[0], &owner, &holder_sk).unwrap(),
            mail_payload
        );
        assert_eq!(decoded.wrapped_keys[1].scope, post_read_scope("tier-1"));
        assert_eq!(decoded.wrapped_keys[1].epoch, None);
        assert_eq!(
            unseal_capability(&decoded.wrapped_keys[1], &owner, &holder_sk).unwrap(),
            post_payload
        );
        // `None` ek ⇒ classical X25519 wrap on every key-bearing tuple.
        assert_eq!(decoded.wrapped_keys[0].hpke.kem_suite, KemSuite::STANDARD);
        assert_eq!(decoded.wrapped_keys[1].hpke.kem_suite, KemSuite::STANDARD);
    }

    #[test]
    fn build_grant_blob_selects_xwing_when_holder_published_ek() {
        // The PQ-CAP-3 mint selector: a holder that published a valid ML-KEM ek
        // gets every key-bearing tuple wrapped X-Wing (the closed-mail-seal
        // bypass fix), and the holder opens each back with its X25519 secret +
        // derived ML-KEM dk. Mirrors the mail seal_recipient_blob selector.
        let (holder_xwing_pk, holder_x25519, holder_mlkem_dk) = xwing_holder(b"grant-holder-a");
        let holder_ek = *holder_xwing_pk.mlkem_encaps_key();
        let x25519_pk = *holder_xwing_pk.x25519_public();
        let owner = [0x55u8; 32];
        let grant_id = [0x02u8; 16];
        let mail_payload = vec![0xA1u8; 32];
        let scopes = vec![
            (mail_read_scope(), Some(mail_payload.clone())),
            (label_write_scope(), None),
        ];

        let blob = build_grant_blob(
            &owner,
            &grant_id,
            &x25519_pk,
            Some(holder_ek.as_slice()),
            GrantWindow(10, 20),
            &scopes,
        )
        .unwrap();

        assert_eq!(blob.wrapped_keys.len(), 1);
        assert_eq!(blob.wrapped_keys[0].hpke.kem_suite.kem, FAUNA_KEM_XWING);
        assert_eq!(blob.wrapped_keys[0].hpke.enc.len(), 1120);
        // Survives the wire, still opens via the hybrid opener.
        let bytes = blob.to_canonical_bytes().unwrap();
        let decoded = GrantBlob::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(
            unseal_capability_hybrid(
                &decoded.wrapped_keys[0],
                &owner,
                &holder_x25519,
                &holder_mlkem_dk
            )
            .unwrap(),
            mail_payload
        );
    }

    #[test]
    fn build_grant_blob_degrades_to_classical_on_malformed_ek() {
        // PQ-4b: a right-length-but-invalid ek must degrade to the classical
        // wrap (openable) rather than fail the whole mint. A FIPS-203-invalid
        // ML-KEM key of the correct length only fails at encaps.
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let owner = [0x66u8; 32];
        let bad_ek = vec![0xFFu8; MLKEM768_ENCAPS_KEY_LEN]; // all-ones: not a valid ML-KEM ek
        let payload = vec![0xB2u8; 32];
        let scopes = vec![(mail_read_scope(), Some(payload.clone()))];

        let blob = build_grant_blob(
            &owner,
            &[0x03u8; 16],
            &holder_pk,
            Some(bad_ek.as_slice()),
            GrantWindow(0, 1),
            &scopes,
        )
        .unwrap();

        // Degraded to the classical suite, and still opens classically.
        assert_eq!(blob.wrapped_keys[0].hpke.kem_suite, KemSuite::STANDARD);
        assert_eq!(
            unseal_capability(&blob.wrapped_keys[0], &owner, &holder_sk).unwrap(),
            payload
        );
    }

    #[test]
    fn build_grant_blob_wrong_length_ek_is_classical() {
        // An ek of the wrong length is ignored (not X-Wing) — the length filter
        // in the selector, not a seal attempt.
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let owner = [0x67u8; 32];
        let payload = vec![0xC3u8; 32];
        let scopes = vec![(mail_read_scope(), Some(payload.clone()))];
        let blob = build_grant_blob(
            &owner,
            &[0x04u8; 16],
            &holder_pk,
            Some([0u8; 10].as_slice()), // too short to be an ML-KEM ek
            GrantWindow(0, 1),
            &scopes,
        )
        .unwrap();
        assert_eq!(blob.wrapped_keys[0].hpke.kem_suite, KemSuite::STANDARD);
        assert_eq!(
            unseal_capability(&blob.wrapped_keys[0], &owner, &holder_sk).unwrap(),
            payload
        );
    }

    #[test]
    fn seal_unseal_capability_round_trips() {
        // holder = an enrolled bridge service-user X25519 keypair (NOT the
        // actor identity); owner = the content-owning user.
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let owner = [0x11u8; 32];
        // the wrapped payload is a minimal derived content key (here a
        // 32-byte dummy recipient-mail HPKE secret) — never MSEK/identity.
        let key = [0xABu8; 32];
        let wrapped = seal_capability(&key, &owner, &mail_read_scope(), None, &holder_pk).unwrap();
        assert_eq!(wrapped.scope, mail_read_scope());
        assert_eq!(wrapped.epoch, None);
        assert_eq!(wrapped.hpke.kem_suite, KemSuite::STANDARD);
        let opened = unseal_capability(&wrapped, &owner, &holder_sk).unwrap();
        assert_eq!(opened, key.to_vec());
    }

    #[test]
    fn wrapped_scope_key_canonical_round_trips() {
        // `to_canonical_bytes` → `from_canonical_bytes` preserves a sealed key —
        // the `appended_keys` wire form a `fauna.capabilities.renew` carries and
        // the element form of a `GrantBlob`'s `wrapped_keys`. It also still
        // opens after the round-trip (the AAD-bound ciphertext survives verbatim).
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let owner = [0x77u8; 32];
        let wrapped =
            seal_capability(&[0xCDu8; 32], &owner, &mail_read_scope(), None, &holder_pk).unwrap();
        let bytes = wrapped.to_canonical_bytes().unwrap();
        let decoded = WrappedScopeKey::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.scope, wrapped.scope);
        assert_eq!(decoded.epoch, wrapped.epoch);
        assert_eq!(decoded.hpke.enc, wrapped.hpke.enc);
        assert_eq!(decoded.hpke.ciphertext, wrapped.hpke.ciphertext);
        assert_eq!(
            unseal_capability(&decoded, &owner, &holder_sk).unwrap(),
            vec![0xCDu8; 32]
        );
    }

    #[test]
    fn unseal_capability_wrong_holder_fails() {
        let (_, holder_pk) = generate_x25519_keypair();
        let (other_sk, _) = generate_x25519_keypair();
        let owner = [0x22u8; 32];
        let wrapped =
            seal_capability(&[0u8; 32], &owner, &mail_read_scope(), None, &holder_pk).unwrap();
        let err = unseal_capability(&wrapped, &owner, &other_sk).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn unseal_capability_wrong_owner_fails() {
        // The AAD binds owner: owner X's blob can't be opened as owner Y's.
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let owner_x = [0x33u8; 32];
        let owner_y = [0x44u8; 32];
        let wrapped =
            seal_capability(&[0u8; 32], &owner_x, &mail_read_scope(), None, &holder_pk).unwrap();
        let err = unseal_capability(&wrapped, &owner_y, &holder_sk).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn unseal_capability_wrong_scope_fails() {
        // A key sealed for content.read{post:tier-1} can't be opened as
        // content.read{post:tier-3} (cross-tier substitution closed).
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let owner = [0x55u8; 32];
        let mut wrapped = seal_capability(
            &[0u8; 32],
            &owner,
            &post_read_scope("tier-1"),
            None,
            &holder_pk,
        )
        .unwrap();
        // Tamper the on-the-wire scope so the unseal-time AAD differs.
        wrapped.scope = post_read_scope("tier-3");
        let err = unseal_capability(&wrapped, &owner, &holder_sk).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn unseal_capability_wrong_epoch_fails() {
        // A key sealed for epoch e can't be opened as epoch e+1 — the crux
        // of epoch-sealed expiry actually biting (design § 2.2).
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let owner = [0x66u8; 32];
        let mut wrapped =
            seal_capability(&[0u8; 32], &owner, &mail_read_scope(), Some(5), &holder_pk).unwrap();
        wrapped.epoch = Some(6);
        let err = unseal_capability(&wrapped, &owner, &holder_sk).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    // ── seal_capability_xwing / unseal_capability_hybrid (post-quantum overlay
    //    for capability grants, `post-quantum.md` § surface A — the capability
    //    grant row). A grant wraps a *standing* content-opening key to its
    //    holder; a classical wrap harvested from the nest today is CRQC-openable
    //    later to recover that key and thence the X-Wing-sealed content it
    //    reaches, so the wrap itself must ride X-Wing. Mirrors the mail S3
    //    seal_to_recipient_xwing / unseal_mail_record_hybrid tests. ──

    /// Build an X-Wing *holder* the way a real enrolled bridge service-user
    /// will: an X25519 keyfile keypair plus an ML-KEM-768 keypair derived from
    /// the holder's own secret seed (context-separated) — the same
    /// seed-derived shape as the MSEK-derived recipient key, but rooted in the
    /// *holder's* seed (the PQ-CAP / S6 bridge-service-user derivation).
    /// Returns the holder's X-Wing public key (the mint's wrap target) plus the
    /// two secret halves the hybrid opener threads: the X25519 secret and the
    /// ML-KEM decapsulation key.
    fn xwing_holder(seed: &[u8]) -> (XWingPublicKey, [u8; 32], [u8; MLKEM768_DECAPS_KEY_LEN]) {
        let (x25519_secret, x25519_pk) = generate_x25519_keypair();
        let (mlkem_dk, mlkem_ek) =
            fauna_pq_kem::derive_mlkem768_keypair_from_ikm(seed, "fauna.test.capability-holder.v1");
        let xwing_pk = XWingPublicKey::from_parts(mlkem_ek, x25519_pk);
        (xwing_pk, x25519_secret, mlkem_dk)
    }

    #[test]
    fn capability_xwing_round_trip() {
        // A grant's minimal content key wrapped X-Wing to the holder round-trips
        // when the holder opens with its X25519 secret + derived ML-KEM dk.
        let (holder_xwing_pk, holder_x25519, holder_mlkem_dk) = xwing_holder(b"holder-seed-1");
        let owner = [0x11u8; 32];
        let key = [0xABu8; 32]; // a minimal derived content key (e.g. the recipient-mail secret)
        let wrapped =
            seal_capability_xwing(&key, &owner, &mail_read_scope(), None, &holder_xwing_pk)
                .unwrap();
        assert_eq!(wrapped.scope, mail_read_scope());
        assert_eq!(wrapped.epoch, None);
        assert_eq!(wrapped.hpke.kem_suite.kem, FAUNA_KEM_XWING);
        assert_eq!(
            wrapped.hpke.enc.len(),
            1120,
            "X-Wing enc is the 1120-byte ct"
        );
        let opened =
            unseal_capability_hybrid(&wrapped, &owner, &holder_x25519, &holder_mlkem_dk).unwrap();
        assert_eq!(opened, key.to_vec());
    }

    #[test]
    fn capability_xwing_survives_canonical_roundtrip() {
        // The X-Wing suite + 1120-byte enc survive the WrappedScopeKey wire
        // encode/decode (what fauna.capabilities.mint / renew carry) and still open.
        let (holder_xwing_pk, holder_x25519, holder_mlkem_dk) = xwing_holder(b"holder-seed-2");
        let owner = [0x77u8; 32];
        let key = [0xCDu8; 32];
        let wrapped =
            seal_capability_xwing(&key, &owner, &mail_read_scope(), None, &holder_xwing_pk)
                .unwrap();
        let bytes = wrapped.to_canonical_bytes().unwrap();
        let decoded = WrappedScopeKey::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.hpke.kem_suite.kem, FAUNA_KEM_XWING);
        let opened =
            unseal_capability_hybrid(&decoded, &owner, &holder_x25519, &holder_mlkem_dk).unwrap();
        assert_eq!(opened, key.to_vec());
    }

    #[test]
    fn capability_xwing_blob_needs_the_hybrid_opener() {
        // A hybrid wrapped key reaching the classical `unseal_capability` (no
        // ML-KEM key) is a typed InvalidFormat, not a silent mis-decrypt — the
        // "loud, not silent" contract (mirrors mail's needs-hybrid-opener).
        let (holder_xwing_pk, holder_x25519, _dk) = xwing_holder(b"holder-seed-3");
        let owner = [0x22u8; 32];
        let wrapped = seal_capability_xwing(
            &[0u8; 32],
            &owner,
            &mail_read_scope(),
            None,
            &holder_xwing_pk,
        )
        .unwrap();
        let err = unseal_capability(&wrapped, &owner, &holder_x25519).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }

    #[test]
    fn capability_classical_opens_via_hybrid_opener() {
        // The hybrid opener is a superset: a classical grant key opens through
        // it, ignoring the ML-KEM dk — so a holder can call one opener per suite.
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let owner = [0x33u8; 32];
        let key = [0xEEu8; 32];
        let wrapped = seal_capability(&key, &owner, &mail_read_scope(), None, &holder_pk).unwrap();
        let (mlkem_dk, _ek) =
            fauna_pq_kem::derive_mlkem768_keypair_from_ikm(b"unrelated", "fauna.test.v1");
        let opened = unseal_capability_hybrid(&wrapped, &owner, &holder_sk, &mlkem_dk).unwrap();
        assert_eq!(opened, key.to_vec());
    }

    #[test]
    fn capability_xwing_wrong_holder_fails() {
        // Wrong holder (both halves) ⇒ X-Wing yields a different shared secret
        // (ML-KEM implicit rejection) ⇒ the AEAD rejects: typed HpkeFailed.
        let (holder_a_pk, _, _) = xwing_holder(b"holder-a");
        let (_, holder_b_x25519, holder_b_dk) = xwing_holder(b"holder-b");
        let owner = [0x44u8; 32];
        let wrapped =
            seal_capability_xwing(&[0u8; 32], &owner, &mail_read_scope(), None, &holder_a_pk)
                .unwrap();
        let err =
            unseal_capability_hybrid(&wrapped, &owner, &holder_b_x25519, &holder_b_dk).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn capability_xwing_wrong_scope_fails() {
        // The AAD binds scope on the hybrid path too: a key sealed for
        // content.read{post:tier-1} can't be opened as {post:tier-3}.
        let (holder_xwing_pk, holder_x25519, holder_mlkem_dk) = xwing_holder(b"holder-seed-scope");
        let owner = [0x55u8; 32];
        let mut wrapped = seal_capability_xwing(
            &[0u8; 32],
            &owner,
            &post_read_scope("tier-1"),
            None,
            &holder_xwing_pk,
        )
        .unwrap();
        wrapped.scope = post_read_scope("tier-3");
        let err = unseal_capability_hybrid(&wrapped, &owner, &holder_x25519, &holder_mlkem_dk)
            .unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn aad_for_capability_distinguishes_components() {
        let o1 = [1u8; 32];
        let o2 = [2u8; 32];
        let base = AadBinding::for_capability(
            &o1,
            "content.read",
            Some("post"),
            Some("tier-1"),
            Some(5),
            None,
            None,
        )
        .canonical_bytes();
        // owner differs
        let owner = AadBinding::for_capability(
            &o2,
            "content.read",
            Some("post"),
            Some("tier-1"),
            Some(5),
            None,
            None,
        )
        .canonical_bytes();
        // class differs
        let class = AadBinding::for_capability(
            &o1,
            "index.read",
            Some("post"),
            Some("tier-1"),
            Some(5),
            None,
            None,
        )
        .canonical_bytes();
        // kind differs
        let kind = AadBinding::for_capability(
            &o1,
            "content.read",
            Some("mail"),
            Some("tier-1"),
            Some(5),
            None,
            None,
        )
        .canonical_bytes();
        // tier differs
        let tier = AadBinding::for_capability(
            &o1,
            "content.read",
            Some("post"),
            Some("tier-2"),
            Some(5),
            None,
            None,
        )
        .canonical_bytes();
        // epoch differs
        let epoch = AadBinding::for_capability(
            &o1,
            "content.read",
            Some("post"),
            Some("tier-1"),
            Some(6),
            None,
            None,
        )
        .canonical_bytes();
        // None vs Some for the optional fields differs
        let none_tier = AadBinding::for_capability(
            &o1,
            "content.read",
            Some("post"),
            None,
            Some(5),
            None,
            None,
        )
        .canonical_bytes();
        let none_epoch = AadBinding::for_capability(
            &o1,
            "content.read",
            Some("post"),
            Some("tier-1"),
            None,
            None,
            None,
        )
        .canonical_bytes();
        // the set qualifier differs (folder scope), and Some vs None differs
        let set_a = AadBinding::for_capability(
            &o1,
            "content.read",
            Some("folder"),
            None,
            Some(1),
            Some("set-a"),
            None,
        )
        .canonical_bytes();
        let set_b = AadBinding::for_capability(
            &o1,
            "content.read",
            Some("folder"),
            None,
            Some(1),
            Some("set-b"),
            None,
        )
        .canonical_bytes();
        let set_none = AadBinding::for_capability(
            &o1,
            "content.read",
            Some("folder"),
            None,
            Some(1),
            None,
            None,
        )
        .canonical_bytes();
        assert_ne!(set_a, set_b);
        assert_ne!(set_a, set_none);
        for other in [owner, class, kind, tier, epoch, none_tier, none_epoch] {
            assert_ne!(base, other);
        }
        // the factor qualifier (the per-labeler license) differs: labeler A's
        // wrap is not labeler B's, neither is the factor-less (built-in
        // scanner) wrap, and a factor-bearing binding never collides with a
        // set-bearing one of the same length.
        let mail_base = AadBinding::for_capability(
            &o1,
            "content.read",
            Some("mail"),
            None,
            Some(1),
            None,
            None,
        )
        .canonical_bytes();
        let factor_a = AadBinding::for_capability(
            &o1,
            "content.read",
            Some("mail"),
            None,
            Some(1),
            None,
            Some("labeler:aa"),
        )
        .canonical_bytes();
        let factor_b = AadBinding::for_capability(
            &o1,
            "content.read",
            Some("mail"),
            None,
            Some(1),
            None,
            Some("labeler:bb"),
        )
        .canonical_bytes();
        let set_as_factor = AadBinding::for_capability(
            &o1,
            "content.read",
            Some("mail"),
            None,
            Some(1),
            Some("labeler:aa"),
            None,
        )
        .canonical_bytes();
        assert_ne!(factor_a, factor_b);
        assert_ne!(factor_a, mail_base);
        assert_ne!(factor_a, set_as_factor);
    }

    fn folder_read_scope(set: &str) -> ScopeTuple {
        ScopeTuple {
            class: "content.read".into(),
            kind: Some("folder".into()),
            tier: None,
            set: Some(set.into()),
            factor: None,
        }
    }

    #[test]
    fn folder_grant_wraps_one_key_per_generation_and_round_trips() {
        // The web-paywall folder regime (`mls-group-key-material.md` § M2
        // third distribution channel): one WrappedScopeKey per content-key
        // generation, epoch = the generation version.
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let owner = [0x33u8; 32];
        let gen1 = [0xA1u8; 32];
        let gen2 = [0xA2u8; 32];
        let blob = build_grant_blob_with_epochs(
            &owner,
            &[0x44u8; 16],
            &holder_pk,
            None,
            GrantWindow(100, 200),
            &[(
                folder_read_scope("site-members"),
                vec![(Some(1), gen1.to_vec()), (Some(2), gen2.to_vec())],
            )],
        )
        .unwrap();
        assert_eq!(blob.scope.len(), 1);
        assert_eq!(blob.wrapped_keys.len(), 2);
        for (wrapped, (want_epoch, want_key)) in
            blob.wrapped_keys.iter().zip([(1u64, gen1), (2u64, gen2)])
        {
            assert_eq!(wrapped.epoch, Some(want_epoch));
            assert_eq!(wrapped.scope.set.as_deref(), Some("site-members"));
            let opened = unseal_capability(wrapped, &owner, &holder_sk).unwrap();
            assert_eq!(opened, want_key);
        }
    }

    #[test]
    fn folder_wrap_refuses_cross_set_and_cross_generation_substitution() {
        // Scope is crypto-self-enforcing for the two net-new index
        // components: set A's key presented as set B, and generation v's key
        // presented as v+1, both fail AEAD verify at open.
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let owner = [0x55u8; 32];
        let wrapped = seal_capability(
            &[0xB7u8; 32],
            &owner,
            &folder_read_scope("set-a"),
            Some(3),
            &holder_pk,
        )
        .unwrap();
        // Sanity: the untampered wrap opens.
        assert!(unseal_capability(&wrapped, &owner, &holder_sk).is_ok());
        // Cross-set: rewrite the on-the-wire set name.
        let mut cross_set = wrapped.clone();
        cross_set.scope.set = Some("set-b".into());
        assert!(matches!(
            unseal_capability(&cross_set, &owner, &holder_sk).unwrap_err(),
            UnwrapError::HpkeFailed
        ));
        // Cross-generation: present the version-3 key as version 4.
        let mut cross_gen = wrapped.clone();
        cross_gen.epoch = Some(4);
        assert!(matches!(
            unseal_capability(&cross_gen, &owner, &holder_sk).unwrap_err(),
            UnwrapError::HpkeFailed
        ));
        // Dropped qualifier: a set-less presentation of a set-bound wrap.
        let mut dropped = wrapped;
        dropped.scope.set = None;
        assert!(matches!(
            unseal_capability(&dropped, &owner, &holder_sk).unwrap_err(),
            UnwrapError::HpkeFailed
        ));
    }

    #[test]
    fn scope_tuple_set_field_is_wire_additive() {
        // Old 3-key tuple bytes decode with `set: None`, and a set-less tuple
        // encodes WITHOUT a "set" key (byte-stable vs the pre-field shape) —
        // the within-major additive-everywhere contract.
        let old = mail_read_scope();
        let bytes = fauna_cbor::encode_canonical(&old).unwrap();
        assert!(
            !bytes.windows(4).any(|w| w == b"cset"),
            "set-less tuple must omit the field (0x63 'set' key present)"
        );
        let decoded: ScopeTuple = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(decoded.set, None);
        // And a set-bearing tuple round-trips.
        let new = folder_read_scope("s");
        let bytes = fauna_cbor::encode_canonical(&new).unwrap();
        let decoded: ScopeTuple = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(decoded.set.as_deref(), Some("s"));
    }

    #[test]
    fn aad_capability_differs_from_other_kinds() {
        // The "capability-grant" domain tag separates it from all 5 existing
        // wrapped-blob AAD kinds (cross-shape domain separation).
        let owner = [0u8; 32];
        let cap = AadBinding::for_capability(
            &owner,
            "content.read",
            Some("mail"),
            None,
            None,
            None,
            None,
        )
        .canonical_bytes();
        assert_ne!(
            cap,
            AadBinding::for_wrapped_msek(&owner, "x").canonical_bytes()
        );
        assert_ne!(cap, AadBinding::for_mls_snapshot(&owner).canonical_bytes());
        assert_ne!(
            cap,
            AadBinding::for_submission_token(&owner, "x").canonical_bytes()
        );
        assert_ne!(
            cap,
            AadBinding::for_tls_cert("mta", "b1", "ex.com").canonical_bytes()
        );
        assert_ne!(cap, AadBinding::for_mail_record().canonical_bytes());
    }

    /// Golden byte-stability lock (as `aad_wrapped_msek_golden_bytes`): the
    /// canonical-dag-cbor AAD bytes are bound into every capability seal, so
    /// they MUST stay byte-identical for cross-language (Go / Swift) parity.
    /// Map keys in canonical (length-first-then-bytewise) order: v, ix, kind.
    #[test]
    fn aad_for_capability_golden_bytes() {
        let owner = [0u8; 32];
        // `set: None` MUST keep these bytes identical to the pre-`set` shape —
        // every at-rest grant minted before the folder scope existed binds
        // exactly this 5-element AAD (within-major additive compat).
        let got = AadBinding::for_capability(
            &owner,
            "content.read",
            Some("post"),
            Some("tier-2"),
            Some(7),
            None,
            None,
        )
        .canonical_bytes();
        let mut want = vec![0xA3, 0x61, 0x76, 0x01, 0x62, 0x69, 0x78, 0x85];
        // ix[0]: owner_actor_id bstr(32)
        want.extend_from_slice(&[0x58, 0x20]);
        want.extend_from_slice(&[0u8; 32]);
        // ix[1]: class "content.read" (text(12))
        want.push(0x6c);
        want.extend_from_slice(b"content.read");
        // ix[2]: kind "post" (text(4))
        want.push(0x64);
        want.extend_from_slice(b"post");
        // ix[3]: tier "tier-2" (text(6))
        want.push(0x66);
        want.extend_from_slice(b"tier-2");
        // ix[4]: epoch 7 (uint)
        want.push(0x07);
        // "kind" => "capability-grant"
        want.extend_from_slice(&[0x64, 0x6b, 0x69, 0x6e, 0x64]); // "kind"
        want.push(0x70); // text(16)
        want.extend_from_slice(b"capability-grant");
        assert_eq!(got, want, "capability AAD canonical bytes drifted");
    }

    #[test]
    fn grant_blob_round_trips() {
        // A grant with one key-bearing tuple (mail read) plus one keyless
        // tuple (label-write) that appears in `scope` but NOT in `wrapped_keys`.
        let (_, holder_pk) = generate_x25519_keypair();
        let owner = [0x11u8; 32];
        let wrapped =
            seal_capability(&[0xABu8; 32], &owner, &mail_read_scope(), None, &holder_pk).unwrap();
        let blob = GrantBlob {
            version: BLOB_FORMAT_VERSION,
            kind: GrantBlob::KIND.into(),
            index: GrantIndex(owner.to_vec(), vec![0x22u8; 16]),
            holder: ByteBuf::from(holder_pk.to_vec()),
            window: GrantWindow(100, 200),
            scope: vec![
                mail_read_scope(),
                ScopeTuple {
                    class: "content.label-write".into(),
                    kind: None,
                    tier: None,
                    set: None,
                    factor: None,
                },
            ],
            wrapped_keys: vec![wrapped],
        };
        let bytes = blob.to_canonical_bytes().unwrap();
        let decoded = GrantBlob::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.kind, GrantBlob::KIND);
        assert_eq!(decoded.index.0, owner.to_vec());
        assert_eq!(decoded.index.1, vec![0x22u8; 16]);
        assert_eq!(decoded.holder.as_ref(), &holder_pk);
        assert_eq!(decoded.window, GrantWindow(100, 200));
        assert_eq!(decoded.scope.len(), 2);
        assert_eq!(decoded.wrapped_keys.len(), 1);
        // canonical re-encode is byte-stable
        assert_eq!(decoded.to_canonical_bytes().unwrap(), bytes);
    }

    #[test]
    fn grant_blob_rejects_wrong_kind() {
        let mut blob = GrantBlob {
            version: BLOB_FORMAT_VERSION,
            kind: "not-a-grant".into(),
            index: GrantIndex(vec![0u8; 32], vec![0u8; 16]),
            holder: ByteBuf::from(vec![0u8; 32]),
            window: GrantWindow(0, 0),
            scope: vec![],
            wrapped_keys: vec![],
        };
        let bytes = blob.to_canonical_bytes().unwrap();
        let err = GrantBlob::from_canonical_bytes(&bytes).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
        // sanity: the same blob with the right kind decodes
        blob.kind = GrantBlob::KIND.into();
        let ok = blob.to_canonical_bytes().unwrap();
        assert!(GrantBlob::from_canonical_bytes(&ok).is_ok());
    }

    #[test]
    fn grant_blob_end_to_end_unseal() {
        // Encode a grant, decode it as a holder would, then unseal the mail
        // key from the decoded wrapped key using the owner read from ix.
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let owner = [0x33u8; 32];
        let key = [0xCDu8; 32];
        let wrapped = seal_capability(&key, &owner, &mail_read_scope(), None, &holder_pk).unwrap();
        let blob = GrantBlob {
            version: BLOB_FORMAT_VERSION,
            kind: GrantBlob::KIND.into(),
            index: GrantIndex(owner.to_vec(), vec![0x01u8; 16]),
            holder: ByteBuf::from(holder_pk.to_vec()),
            window: GrantWindow(0, u64::MAX),
            scope: vec![mail_read_scope()],
            wrapped_keys: vec![wrapped],
        };
        let bytes = blob.to_canonical_bytes().unwrap();
        let decoded = GrantBlob::from_canonical_bytes(&bytes).unwrap();
        let owner_from_ix: [u8; 32] = decoded.index.0.as_slice().try_into().unwrap();
        let opened =
            unseal_capability(&decoded.wrapped_keys[0], &owner_from_ix, &holder_sk).unwrap();
        assert_eq!(opened, key.to_vec());
    }

    // ── seal_to_recipient / unseal_mail_record (Phase C.9) ──

    #[test]
    fn mail_record_round_trip() {
        let (sk, pk) = generate_x25519_keypair();
        let plaintext = b"raw rfc 5322 bytes the MTA buffers in Session.Data";
        let envelope = seal_to_recipient(plaintext, &pk).unwrap();
        assert_eq!(envelope.version, BLOB_FORMAT_VERSION);
        assert_eq!(envelope.kind, "mail-record");
        let opened = unseal_mail_record(&envelope, &sk).unwrap();
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn mail_record_wrong_recipient_fails() {
        let (_, pk) = generate_x25519_keypair();
        let (other_sk, _) = generate_x25519_keypair();
        let envelope = seal_to_recipient(b"x", &pk).unwrap();
        let err = unseal_mail_record(&envelope, &other_sk).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn mail_record_canonical_bytes_roundtrip() {
        // The bridge sends `to_canonical_bytes()` to nest as
        // `encrypted_body`; nest stores them opaque. Verify the wire
        // shape decodes back to a valid envelope and opens cleanly.
        let (sk, pk) = generate_x25519_keypair();
        let plaintext = b"index-hint canonical bytes";
        let envelope = seal_to_recipient(plaintext, &pk).unwrap();
        let bytes = envelope.to_canonical_bytes().unwrap();
        let decoded = MailRecordEnvelope::from_canonical_bytes(&bytes).unwrap();
        let opened = unseal_mail_record(&decoded, &sk).unwrap();
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn mail_record_two_seals_with_same_inputs_produce_distinct_envelopes() {
        // Per-call uniqueness: each seal MUST generate a fresh HPKE
        // encapsulation (a fresh ephemeral X25519 keypair), so identical
        // plaintext + identical recipient produce distinct ciphertexts.
        // A regression that accidentally cached the ephemeral key would
        // pass every other test in this module.
        let (_, pk) = generate_x25519_keypair();
        let a = seal_to_recipient(b"x", &pk).unwrap();
        let b = seal_to_recipient(b"x", &pk).unwrap();
        assert_ne!(a.hpke.enc.as_ref(), b.hpke.enc.as_ref(), "enc reused!");
        assert_ne!(
            a.hpke.ciphertext.as_ref(),
            b.hpke.ciphertext.as_ref(),
            "ciphertext reused!"
        );
    }

    #[test]
    fn mail_record_kind_substitution_fails() {
        // Tamper the `kind` field on the wire → from_canonical_bytes
        // rejects the substituted envelope before the HPKE open even
        // runs. This catches a malicious nest swapping a stored
        // mail-record into a future at-rest reader that expects (say)
        // an mls-snapshot.
        let (sk, pk) = generate_x25519_keypair();
        let envelope = seal_to_recipient(b"x", &pk).unwrap();
        let mut bytes = envelope.to_canonical_bytes().unwrap();
        // Find "mail-record" in the encoded bytes (CBOR text-string
        // header byte + the literal) and overwrite the first letter
        // so the kind mismatches.
        let needle = b"mail-record";
        let pos = bytes
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("kind string must appear in encoded envelope");
        bytes[pos] = b'X'; // "Xail-record"
        let err = MailRecordEnvelope::from_canonical_bytes(&bytes).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
        // Bonus: the wrong-kind envelope can't even be constructed and
        // passed to unseal_mail_record — but verify the unseal-level
        // guard too by hand-building a wrong-kind envelope.
        let mut tampered = seal_to_recipient(b"x", &pk).unwrap();
        tampered.kind = "tls-cert".into();
        let err = unseal_mail_record(&tampered, &sk).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }

    #[test]
    fn is_sealed_mail_record_discriminates_sealed_from_raw() {
        // Sealed records (both suites) → true.
        let (_, pk) = generate_x25519_keypair();
        let sealed = seal_to_recipient(b"body", &pk)
            .unwrap()
            .to_canonical_bytes()
            .unwrap();
        assert!(is_sealed_mail_record(&sealed));
        let xwing_pk = derive_recipient_xwing_keypair(&[7u8; 32]).public;
        let sealed_pq = seal_to_recipient_xwing(b"body", &xwing_pk)
            .unwrap()
            .to_canonical_bytes()
            .unwrap();
        assert!(is_sealed_mail_record(&sealed_pq));

        // Raw plaintext payloads → false.
        assert!(!is_sealed_mail_record(
            b"From: a@example.com\r\nSubject: hi\r\n\r\nplain mail body\r\n"
        ));
        assert!(!is_sealed_mail_record(
            b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        ));
        // Canonical index-hint token bytes: <u32 BE len><token> repeated.
        let mut hint = Vec::new();
        for tok in [b"hello".as_slice(), b"world".as_slice()] {
            hint.extend_from_slice(&(tok.len() as u32).to_be_bytes());
            hint.extend_from_slice(tok);
        }
        assert!(!is_sealed_mail_record(&hint));
        // Degenerate shapes.
        assert!(!is_sealed_mail_record(b""));
        assert!(!is_sealed_mail_record(&sealed[..sealed.len() / 2]));
    }

    #[test]
    fn unseal_dispatches_on_kem_suite() {
        // Crypto-agility (goal § 7.1): a non-classical `ks` must route away from
        // today's X25519 path. The *classical* opener (`unseal_mail_record`, no
        // ML-KEM key) returns a TYPED format error for a hybrid/unknown suite —
        // never a silent AEAD/HPKE failure that looks like tampering. Classical
        // blobs are unaffected (covered by `mail_record_round_trip`); the hybrid
        // opener is covered by `mail_record_xwing_round_trip`.
        let (sk, pk) = generate_x25519_keypair();

        // X-Wing suite via the classical opener → typed InvalidFormat (no dk).
        let mut xwing = seal_to_recipient(b"x", &pk).unwrap();
        xwing.hpke.kem_suite = KemSuite {
            kem: FAUNA_KEM_XWING,
            kdf: 0x0001,
            aead: 0x0003,
        };
        let err = unseal_mail_record(&xwing, &sk).unwrap_err();
        assert!(
            matches!(err, UnwrapError::InvalidFormat(_)),
            "X-Wing suite must yield a typed InvalidFormat, got {err:?}"
        );

        // Arbitrary unknown suite → typed InvalidFormat too.
        let mut unknown = seal_to_recipient(b"x", &pk).unwrap();
        unknown.hpke.kem_suite = KemSuite {
            kem: 0x0099,
            kdf: 0x0001,
            aead: 0x0003,
        };
        let err = unseal_mail_record(&unknown, &sk).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));

        // The same dispatch guards the TLS cert HPKE blob.
        let (tsk, tpk) = generate_x25519_keypair();
        let bundle = fresh_tls_bundle();
        let mut tls = seal_tls_cert(&bundle, "mta", "bridge-1", "example.com", &tpk).unwrap();
        tls.hpke.kem_suite = KemSuite {
            kem: FAUNA_KEM_XWING,
            kdf: 0x0001,
            aead: 0x0003,
        };
        let err = unseal_tls_cert(&tls, &tsk).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }

    // ── seal_to_recipient_xwing / unseal_mail_record_hybrid (post-quantum S3) ──

    /// Derive the recipient's X-Wing public key plus the two secret halves the
    /// hybrid opener takes separately (the MSEK-derived ML-KEM dk + the reused
    /// recipient-mail X25519 secret) — exactly what the real reader threads.
    fn xwing_recipient(
        msek: &[u8; 32],
    ) -> (
        fauna_pq_kem::XWingPublicKey,
        [u8; 32],
        [u8; fauna_pq_kem::MLKEM768_DECAPS_KEY_LEN],
    ) {
        let pubkey = derive_recipient_xwing_keypair(msek).public;
        let (x25519_secret, _) = derive_recipient_hpke_keypair(msek);
        let (mlkem_dk, _ek) =
            fauna_pq_kem::derive_mlkem768_keypair_from_ikm(msek, RECIPIENT_MLKEM_DERIVE_CONTEXT);
        (pubkey, x25519_secret, mlkem_dk)
    }

    #[test]
    fn mail_record_xwing_round_trip() {
        let msek = [42u8; 32];
        let (pubkey, x25519_secret, mlkem_dk) = xwing_recipient(&msek);
        let plaintext = b"raw rfc 5322 bytes sealed to a hybrid recipient";
        let envelope = seal_to_recipient_xwing(plaintext, &pubkey).unwrap();
        assert_eq!(envelope.kind, "mail-record");
        assert_eq!(envelope.hpke.kem_suite.kem, FAUNA_KEM_XWING);
        assert_eq!(
            envelope.hpke.enc.len(),
            1120,
            "X-Wing enc is the 1120-byte ct"
        );
        // The reader reconstructs the X-Wing secret from the two halves it derives.
        let opened = unseal_mail_record_hybrid(&envelope, &x25519_secret, &mlkem_dk).unwrap();
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn mail_record_xwing_survives_canonical_bytes_roundtrip() {
        // The bridge ships `to_canonical_bytes()` to nest; verify the 1120-byte
        // enc + X-Wing suite survive the wire encode/decode and still open.
        let msek = [3u8; 32];
        let (pubkey, x25519_secret, mlkem_dk) = xwing_recipient(&msek);
        let envelope = seal_to_recipient_xwing(b"hybrid body", &pubkey).unwrap();
        let bytes = envelope.to_canonical_bytes().unwrap();
        let decoded = MailRecordEnvelope::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.hpke.kem_suite.kem, FAUNA_KEM_XWING);
        let opened = unseal_mail_record_hybrid(&decoded, &x25519_secret, &mlkem_dk).unwrap();
        assert_eq!(opened, b"hybrid body");
    }

    #[test]
    fn mail_record_xwing_blob_needs_the_hybrid_opener() {
        // A hybrid blob reaching the classical opener (no ML-KEM key) is a typed
        // InvalidFormat, not a silent mis-decrypt — the "loud, not silent" contract.
        let msek = [7u8; 32];
        let (pubkey, x25519_secret, _mlkem_dk) = xwing_recipient(&msek);
        let envelope = seal_to_recipient_xwing(b"hybrid only", &pubkey).unwrap();
        let err = unseal_mail_record(&envelope, &x25519_secret).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }

    #[test]
    fn xwing_kem_with_wrong_kdf_or_aead_is_unknown_suite() {
        // PQ-1: the X-Wing arm requires the full pinned suite (kem ∥ kdf ∥ aead),
        // not `kem` alone. A blob carrying FAUNA_KEM_XWING but a non-pinned
        // kdf/aead must route to the typed *unknown-suite* error (naming the
        // ids), NOT attempt the X-Wing open — even when the real ML-KEM dk is
        // supplied.
        let msek = [9u8; 32];
        let (pubkey, x25519_secret, mlkem_dk) = xwing_recipient(&msek);

        for (kdf, aead) in [(0x0002u16, 0x0003u16), (0x0001, 0x0002)] {
            let mut env = seal_to_recipient_xwing(b"hybrid body", &pubkey).unwrap();
            env.hpke.kem_suite = KemSuite {
                kem: FAUNA_KEM_XWING,
                kdf,
                aead,
            };
            let err = unseal_mail_record_hybrid(&env, &x25519_secret, &mlkem_dk).unwrap_err();
            match err {
                UnwrapError::InvalidFormat(msg) => assert!(
                    msg.contains("unknown HPKE KEM suite"),
                    "expected the unknown-suite error, got: {msg}"
                ),
                other => panic!("expected typed InvalidFormat, got {other:?}"),
            }
        }
    }

    #[test]
    fn mail_record_classical_blob_opens_via_hybrid_opener() {
        // The hybrid opener is a superset: a classical blob opens through it,
        // ignoring the ML-KEM key (so a reader can call one opener for any suite).
        let (sk, pk) = generate_x25519_keypair();
        let envelope = seal_to_recipient(b"classical body", &pk).unwrap();
        let (mlkem_dk, _ek) =
            fauna_pq_kem::derive_mlkem768_keypair_from_ikm(b"unrelated", "fauna.test.v1");
        let opened = unseal_mail_record_hybrid(&envelope, &sk, &mlkem_dk).unwrap();
        assert_eq!(opened, b"classical body");
    }

    #[test]
    fn mail_record_xwing_wrong_recipient_fails() {
        // Wrong recipient (both halves) ⇒ X-Wing yields a different shared secret
        // (ML-KEM implicit rejection) ⇒ the AEAD rejects: typed HpkeFailed.
        let (pubkey_a, _, _) = xwing_recipient(&[1u8; 32]);
        let (_, x25519_b, mlkem_dk_b) = xwing_recipient(&[2u8; 32]);
        let envelope = seal_to_recipient_xwing(b"secret", &pubkey_a).unwrap();
        let err = unseal_mail_record_hybrid(&envelope, &x25519_b, &mlkem_dk_b).unwrap_err();
        assert!(matches!(err, UnwrapError::HpkeFailed));
    }

    #[test]
    fn mail_record_xwing_two_seals_produce_distinct_envelopes() {
        // Each X-Wing seal draws fresh ML-KEM + X25519 ephemeral randomness.
        let (pubkey, _, _) = xwing_recipient(&[9u8; 32]);
        let a = seal_to_recipient_xwing(b"x", &pubkey).unwrap();
        let b = seal_to_recipient_xwing(b"x", &pubkey).unwrap();
        assert_ne!(a.hpke.enc.as_ref(), b.hpke.enc.as_ref(), "enc reused!");
        assert_ne!(
            a.hpke.ciphertext.as_ref(),
            b.hpke.ciphertext.as_ref(),
            "ciphertext reused!"
        );
    }

    #[test]
    fn aad_for_mail_record_differs_from_other_kinds() {
        // Cross-shape domain separation: a stored mail-record AAD
        // must not equal any other wrapped-blob AAD, so a future
        // wrong-shape decrypt site cannot succeed even with matching
        // recipient secret.
        let actor = [0u8; 32];
        let mail = AadBinding::for_mail_record().canonical_bytes();
        assert_ne!(
            mail,
            AadBinding::for_tls_cert("mta", "b1", "example.com").canonical_bytes()
        );
        assert_ne!(mail, AadBinding::for_mls_snapshot(&actor).canonical_bytes());
        assert_ne!(
            mail,
            AadBinding::for_wrapped_msek(&actor, "cred-1").canonical_bytes()
        );
    }
}
