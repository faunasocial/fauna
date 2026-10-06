//! **Writer-signed change records** — the signed statement every file-sync
//! change record carries, its signer, and the one verifier every reader and
//! the nest run (`docs/goal/architecture/writer-signed-change-records.md`
//! § Writer-signed change records, rulings (1)–(3) and (10)(d)).
//!
//! A change record is signed by the writer's **store device principal writer
//! key** (delegated: certified by a root-signed `DeviceAuthorization` carrying
//! [`Capability::SyncWrite`]) or directly by the identity key on a seed-holding
//! host with no principal. The signature is Ed25519 over
//! [`SYNC_CHANGE_WRITER_SIG_V1`] ‖ the canonical dag-cbor of a
//! [`SignedChange`]; the signer is named by the statement's `actor_id` plus the
//! row's `signer_key` (the device key, or the actor id itself for a direct
//! signature) — never a second author field.
//!
//! What this module does **not** decide (the caller's, by design): which set
//! nonce is "this set's" (the reader holds it through custody or the
//! content-key envelope), whether the signed actor may write the set (the
//! reader's roster read), and which row classes are exempt
//! ([`exempt_class`] names the row-intrinsic ones; the WebDAV pseudo-device
//! rule needs the set's serve flag and is the caller's). Item-class routing
//! precedes the signature check.

use std::collections::HashMap;

use fauna_core::data::{Capability, Timestamp};
use fauna_core::encoding::{AuthoringOrigin, EmbedAsBytes};
use serde::Serialize;

pub use crate::sig_domain::SYNC_CHANGE_WRITER_SIG_V1;
use crate::sync::{SyncChange, SyncChangeRecordRequest};

/// Length of a set nonce — the client-minted per-set binding (ruling (2)).
pub const SET_NONCE_LEN: usize = 32;

/// The statement a change record's signature covers — ruling (2)'s field list,
/// plus `is_retention` (ruling (10)(d)). Not covered, deliberately: `seq` and
/// `created_at` (nest-assigned), the plaintext `path` (a reader that places by
/// it recomputes [`fauna_core::sync::path_hash`] against the signed
/// `path_hash` — [`plaintext_path_matches`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedChange {
    /// The set binding: the client-minted 32-byte set nonce.
    pub set_nonce: [u8; SET_NONCE_LEN],
    /// The writer's actor id.
    pub actor_id: [u8; 32],
    /// The recording sync device id.
    pub device_id: [u8; 32],
    /// BLAKE3 of the normalized path.
    pub path_hash: [u8; 32],
    /// The manifest hash — `None` on a delete.
    pub manifest_hash: Option<[u8; 32]>,
    pub change_type: String,
    pub size_bytes: i64,
    pub content_key_version: Option<u64>,
    /// The sealed-path envelope bytes, verbatim.
    pub path_sealed: Option<Vec<u8>>,
    pub thumbnail_hash: Option<[u8; 32]>,
    pub derived_through: Option<i64>,
    /// `is_resolution`, normalized: an absent flag signs as `false` (the two
    /// are one meaning on every reader).
    pub is_resolution: bool,
    /// `is_retention`, normalized the same way — a conflict report's retained
    /// loser, signed by its reporter (ruling (10)(d)). Covered so a retained
    /// loser served with the flag stripped fails rather than passing as the
    /// reporter's fresh edit of the path; encoded only when `true`, so every
    /// ordinary statement is byte-for-byte what it was before the ruling.
    pub is_retention: bool,
}

/// The canonical dag-cbor wire of [`SignedChange`] — byte strings for every
/// hash, absent optionals omitted. Private: the encoding is the signed form and
/// has one construction point, [`SignedChange::signed_message`]. Encode-only:
/// nothing decodes a statement (a verifier rebuilds it from the row), so it
/// derives no `Deserialize` and carries no forward-compat catch-all — an
/// unknown key could never be part of what was signed.
#[derive(Serialize)]
struct StatementWire<'a> {
    #[serde(with = "serde_bytes")]
    set_nonce: &'a [u8],
    #[serde(with = "serde_bytes")]
    actor_id: &'a [u8],
    #[serde(with = "serde_bytes")]
    device_id: &'a [u8],
    #[serde(with = "serde_bytes")]
    path_hash: &'a [u8],
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "serde_bytes",
        borrow
    )]
    manifest_hash: Option<&'a [u8]>,
    change_type: &'a str,
    size_bytes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_key_version: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "serde_bytes",
        borrow
    )]
    path_sealed: Option<&'a [u8]>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "serde_bytes",
        borrow
    )]
    thumbnail_hash: Option<&'a [u8]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    derived_through: Option<i64>,
    is_resolution: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    is_retention: bool,
}

/// Why a change row did not verify — each arm a reader counts and warns on,
/// then treats the row as absent (ruling (3)).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChangeVerifyError {
    /// No `signature` / `signer_key` on a row outside the class exemptions.
    #[error("change record is unsigned")]
    Unsigned,
    /// A field the statement needs is absent or not well-formed hex/bytes.
    #[error("change record is malformed: {0}")]
    Malformed(String),
    /// The signature does not verify over the statement — a forged or altered
    /// row, or one bound to another set's nonce.
    #[error("change record signature does not verify")]
    SignatureInvalid,
    /// A delegated signer whose cert no side table and no cache supplied.
    #[error("no cert for delegated signer")]
    CertMissing,
    /// The delegation cert failed the shared chain (bad cert signature, another
    /// actor's cert, no `SyncWrite`, expired).
    #[error("signer chain refused: {0}")]
    Chain(String),
    /// The signed actor is neither the set's owner nor a writer on the reader's
    /// roster.
    #[error("signed actor is not a writer of this set")]
    NotAWriter,
    /// Signed under the set's LIVE nonce as a strict predecessor of the set's
    /// owner, where that nonce was minted by a strict successor of the signer
    /// (`writer-signed-change-records.md` ruling (11)(c), arm (1)): the
    /// signer had retired before the nonce existed, so the row is a plant by
    /// construction — refused, never current and never history.
    #[error("signed as a predecessor under a nonce minted after it retired")]
    HistoryEra,
    /// A row labelled with an item class (`state-entry`, `record-cid`) served
    /// to a file reader (`writer-signed-change-records.md` ruling (3)): the
    /// label routes it away from the file reader, not past its check, and the
    /// honest nest never serves one on a folder feed — a detection signal.
    #[error("item-class row is not a file row")]
    OtherPlane,
}

fn hex32(field: &str, s: &str) -> Result<[u8; 32], ChangeVerifyError> {
    fauna_core::hex32::decode(s)
        .map_err(|_| ChangeVerifyError::Malformed(format!("{field} is not 32-byte hex")))
}

fn opt_hex32(field: &str, s: Option<&str>) -> Result<Option<[u8; 32]>, ChangeVerifyError> {
    s.map(|s| hex32(field, s)).transpose()
}

fn bytes32(field: &str, b: &[u8]) -> Result<[u8; 32], ChangeVerifyError> {
    b.try_into()
        .map_err(|_| ChangeVerifyError::Malformed(format!("{field} is not 32 bytes")))
}

/// The routing key both rows a resolved report mints file under: the request's
/// `path_hash` when well-formed, else derived from `path` — as the nest does.
fn report_path_hash(req: &crate::folders::ConflictReportRequest) -> [u8; 32] {
    req.path_hash
        .as_ref()
        .and_then(|b| <[u8; 32]>::try_from(&b[..]).ok())
        .unwrap_or_else(|| fauna_core::sync::path_hash(&req.path))
}

impl SignedChange {
    /// The domain-separated message the signer signs and every verifier checks:
    /// [`SYNC_CHANGE_WRITER_SIG_V1`] ‖ canonical dag-cbor of the statement.
    pub fn signed_message(&self) -> Vec<u8> {
        let wire = StatementWire {
            set_nonce: &self.set_nonce,
            actor_id: &self.actor_id,
            device_id: &self.device_id,
            path_hash: &self.path_hash,
            manifest_hash: self.manifest_hash.as_ref().map(|h| &h[..]),
            change_type: &self.change_type,
            size_bytes: self.size_bytes,
            content_key_version: self.content_key_version,
            path_sealed: self.path_sealed.as_deref(),
            thumbnail_hash: self.thumbnail_hash.as_ref().map(|h| &h[..]),
            derived_through: self.derived_through,
            is_resolution: self.is_resolution,
            is_retention: self.is_retention,
        };
        let body =
            fauna_cbor::encode_canonical(&wire).expect("a SignedChange statement always encodes");
        crate::sig_domain::domain_separated(SYNC_CHANGE_WRITER_SIG_V1, &body)
    }

    /// Sign the statement with `key` (the principal writer key, or the identity
    /// key for a direct signature).
    pub fn sign(&self, key: &ed25519_dalek::SigningKey) -> [u8; 64] {
        use ed25519_dalek::Signer;
        key.sign(&self.signed_message()).to_bytes()
    }

    /// The statement a record request asserts, for the signing writer
    /// `actor_id` under `set_nonce` — the recorder's side. `path_hash` is
    /// derived from the request's plaintext path exactly as the nest derives it.
    pub fn for_record(
        req: &SyncChangeRecordRequest,
        actor_id: [u8; 32],
        set_nonce: [u8; SET_NONCE_LEN],
    ) -> Result<Self, ChangeVerifyError> {
        Ok(Self {
            set_nonce,
            actor_id,
            device_id: hex32("device_id", &req.device_id)?,
            path_hash: fauna_core::sync::path_hash(&req.path),
            manifest_hash: opt_hex32("manifest_hash", req.manifest_hash.as_deref())?,
            change_type: req.change_type.clone(),
            size_bytes: req.size_bytes,
            content_key_version: req.content_key_version,
            path_sealed: req.path_sealed.as_ref().map(|b| b.to_vec()),
            thumbnail_hash: opt_hex32("thumbnail_hash", req.thumbnail_hash.as_deref())?,
            derived_through: req.derived_through,
            is_resolution: req.is_resolution.unwrap_or(false),
            is_retention: false,
        })
    }

    /// The statement a re-seed ceremony's owner signs for one covered-folder
    /// row the materialize arm re-homes into the live set bound to `set_nonce`
    /// (`writer-signed-change-records.md` ruling (7)(a)(ii)) — the ONE builder
    /// the signing delivery leg and the verifying nest both call, so the two
    /// halves cannot drift. `device_id` is the owner's re-seed pseudo-device
    /// ([`fauna_core::label_custody::reseed_pseudo_device_id`]); the source's
    /// `path_sealed` verbatim; a `create` with no generation stamp, no
    /// thumbnail, no `derived_through`, not a resolution — exactly the row the
    /// arm mints.
    pub fn for_rehome(
        set_nonce: [u8; SET_NONCE_LEN],
        owner: [u8; 32],
        path_hash: [u8; 32],
        manifest_hash: [u8; 32],
        size_bytes: i64,
        path_sealed: &[u8],
    ) -> Self {
        Self {
            set_nonce,
            actor_id: owner,
            device_id: fauna_core::label_custody::reseed_pseudo_device_id(&owner),
            path_hash,
            manifest_hash: Some(manifest_hash),
            change_type: "create".to_string(),
            size_bytes,
            content_key_version: None,
            path_sealed: Some(path_sealed.to_vec()),
            thumbnail_hash: None,
            derived_through: None,
            is_resolution: false,
            is_retention: false,
        }
    }

    /// The statement a served row claims **as its nest-stamped author**,
    /// bound to `set_nonce`. What a signer builds over a row it is about to
    /// serve ([`ChangeSigner::sign_row`]) and a holder re-signing its own row;
    /// a READER does not trust the stamp — it recovers the signed actor
    /// ([`recover_signed_actor`]), of which this is only the first candidate.
    pub fn for_row(
        row: &SyncChange,
        set_nonce: [u8; SET_NONCE_LEN],
    ) -> Result<Self, ChangeVerifyError> {
        let actor = row
            .author_actor_id
            .as_deref()
            .ok_or_else(|| ChangeVerifyError::Malformed("author_actor_id absent".into()))?;
        Self::for_row_as(row, set_nonce, hex32("author_actor_id", actor)?)
    }

    /// The statement a served row claims with `actor_id` as its signed actor —
    /// every covered field from the row, the actor from the caller (a
    /// candidate of [`recover_signed_actor`], never read off the nest's stamp).
    pub fn for_row_as(
        row: &SyncChange,
        set_nonce: [u8; SET_NONCE_LEN],
        actor_id: [u8; 32],
    ) -> Result<Self, ChangeVerifyError> {
        let device = row
            .device_id
            .as_deref()
            .ok_or_else(|| ChangeVerifyError::Malformed("device_id absent".into()))?;
        Ok(Self {
            set_nonce,
            actor_id,
            device_id: hex32("device_id", device)?,
            path_hash: hex32("path_hash", &row.path_hash)?,
            manifest_hash: opt_hex32("manifest_hash", row.manifest_hash.as_deref())?,
            change_type: row.change_type.clone(),
            size_bytes: row.size_bytes,
            content_key_version: row.content_key_version,
            path_sealed: row.path_sealed.as_ref().map(|b| b.to_vec()),
            thumbnail_hash: opt_hex32("thumbnail_hash", row.thumbnail_hash.as_deref())?,
            derived_through: row.derived_through,
            is_resolution: row.is_resolution.unwrap_or(false),
            is_retention: row.is_retention.unwrap_or(false),
        })
    }

    /// The winner head row a resolved `fauna.sync.conflicts.report` makes the
    /// nest mint, as its reporter `actor_id` signs it (ruling (1)(ii), the
    /// resolved-report clause): `device_id` = the winning candidate's (the
    /// reporter's own for a merged, non-candidate winner), size and generation
    /// the candidate's (the request's `winning_*` for a non-candidate winner),
    /// the request's `path_hash` (derived from `path` when absent, as the nest
    /// does) and `path_sealed`, `modify`, no thumbnail, `derived_through` =
    /// `winning_derived_through` as sent, `is_resolution =
    /// !winning_carries_novelty`. `Ok(None)` for an unresolved report — it
    /// mints no winner row, so there is nothing to sign.
    pub fn for_resolved_report(
        req: &crate::folders::ConflictReportRequest,
        actor_id: [u8; 32],
        set_nonce: [u8; SET_NONCE_LEN],
    ) -> Result<Option<Self>, ChangeVerifyError> {
        let (Some(_), Some(winner_hex)) = (&req.resolution, &req.winning_manifest_hash) else {
            return Ok(None);
        };
        let winner = hex32("winning_manifest_hash", winner_hex)?;
        let candidate = req
            .candidates
            .iter()
            .find(|c| fauna_core::hex32::decode(&c.manifest_hash).ok() == Some(winner));
        let (device_id, size_bytes, content_key_version) = match candidate {
            Some(c) => (
                hex32("the winning candidate's device_id", &c.device_id)?,
                c.size_bytes,
                c.content_key_version,
            ),
            None => (
                hex32("device_id", &req.device_id)?,
                req.winning_size_bytes.ok_or_else(|| {
                    ChangeVerifyError::Malformed(
                        "a non-candidate winner requires winning_size_bytes".into(),
                    )
                })?,
                req.winning_content_key_version,
            ),
        };
        Ok(Some(Self {
            set_nonce,
            actor_id,
            device_id,
            path_hash: report_path_hash(req),
            manifest_hash: Some(winner),
            change_type: "modify".into(),
            size_bytes,
            content_key_version,
            path_sealed: req.path_sealed.as_ref().map(|b| b.to_vec()),
            thumbnail_hash: None,
            derived_through: req.winning_derived_through,
            is_resolution: !req.winning_carries_novelty.unwrap_or(false),
            is_retention: false,
        }))
    }

    /// The retention row a resolved `fauna.sync.conflicts.report` makes the
    /// nest mint for the reporter's own losing candidate, as its reporter
    /// `actor_id` signs it (ruling (10)(d)): the FIRST candidate whose
    /// `device_id` is the request's and whose manifest is not the winner's —
    /// exactly the nest's pick in `report_conflict_signed`, so two candidates
    /// carrying the reporter's device cannot make an honest report fail — with
    /// that candidate's device, manifest, size and generation, the request's
    /// `path_hash` (derived from `path` when absent) and `path_sealed`,
    /// `modify`, no thumbnail, `derived_through = losing_derived_through` as
    /// sent, not a resolution, `is_retention`. `Ok(None)` when the report
    /// retains nothing: unresolved, or no own candidate other than the winner.
    pub fn for_retained_loser(
        req: &crate::folders::ConflictReportRequest,
        actor_id: [u8; 32],
        set_nonce: [u8; SET_NONCE_LEN],
    ) -> Result<Option<Self>, ChangeVerifyError> {
        let (Some(_), Some(winner_hex)) = (&req.resolution, &req.winning_manifest_hash) else {
            return Ok(None);
        };
        let winner = hex32("winning_manifest_hash", winner_hex)?;
        let reporter = hex32("device_id", &req.device_id)?;
        let mut loser = None;
        for c in &req.candidates {
            let device = hex32("a candidate's device_id", &c.device_id)?;
            let manifest = hex32("a candidate's manifest_hash", &c.manifest_hash)?;
            if device == reporter && manifest != winner {
                loser = Some((c, manifest));
                break;
            }
        }
        let Some((c, manifest)) = loser else {
            return Ok(None);
        };
        Ok(Some(Self {
            set_nonce,
            actor_id,
            device_id: reporter,
            path_hash: report_path_hash(req),
            manifest_hash: Some(manifest),
            change_type: "modify".into(),
            size_bytes: c.size_bytes,
            content_key_version: c.content_key_version,
            path_sealed: req.path_sealed.as_ref().map(|b| b.to_vec()),
            thumbnail_hash: None,
            derived_through: req.losing_derived_through,
            is_resolution: false,
            is_retention: true,
        }))
    }

    /// The head row a choose-winner `fauna.sync.conflicts.resolve` of
    /// `conflict` keeping `winning_manifest_hash` makes the nest mint, as its
    /// chooser `actor_id` signs it (ruling (1)(ii)): the winning candidate's
    /// `device_id`, size and generation, the conflict's `path_hash` and
    /// `path_sealed`, `modify`, no thumbnail, no causal stamp, not a
    /// resolution. The chooser reads every field off the listed conflict
    /// (`conflicts.list`), the same rows the nest mints from.
    pub fn for_choose_winner(
        conflict: &crate::folders::SyncConflict,
        winning_manifest_hash: &str,
        actor_id: [u8; 32],
        set_nonce: [u8; SET_NONCE_LEN],
    ) -> Result<Self, ChangeVerifyError> {
        let winner = hex32("winning_manifest_hash", winning_manifest_hash)?;
        let candidate = conflict
            .candidates
            .iter()
            .find(|c| fauna_core::hex32::decode(&c.manifest_hash).ok() == Some(winner))
            .ok_or_else(|| {
                ChangeVerifyError::Malformed(
                    "winning_manifest_hash is not one of the conflict's candidates".into(),
                )
            })?;
        Ok(Self {
            set_nonce,
            actor_id,
            device_id: hex32("the winning candidate's device_id", &candidate.device_id)?,
            path_hash: bytes32("path_hash", &conflict.path_hash)?,
            manifest_hash: Some(winner),
            change_type: "modify".into(),
            size_bytes: candidate.size_bytes,
            content_key_version: candidate.content_key_version,
            path_sealed: conflict.path_sealed.as_ref().map(|b| b.to_vec()),
            thumbnail_hash: None,
            derived_through: None,
            is_resolution: false,
            is_retention: false,
        })
    }
}

/// A row class exempt from the signature check by what it is, before any
/// signature is consulted (ruling (3)). The WebDAV pseudo-device class needs
/// the set's serve flag and is the caller's to route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExemptClass {
    /// A conflict retention row (`is_retention`): the reporter's losing
    /// candidate a resolved report retains. Exempt for the FOLD only (ruling
    /// (10)(e)) — nothing of its content is consumed there, so it is accounted
    /// and skipped. A reader of versions takes no such exemption and judges it
    /// by its reporter's signature
    /// ([`crate::sync_row_verify::RowReader::judge_projection`]).
    Retention,
    /// An account-plane `state-entry` row (its own in-seal writer signature).
    StateEntry,
    /// A content-scope `record-cid` row (provenance is the record's envelope).
    RecordCid,
}

/// The row-intrinsic exemption, if any.
pub fn exempt_class(row: &SyncChange) -> Option<ExemptClass> {
    if row.is_retention == Some(true) {
        return Some(ExemptClass::Retention);
    }
    match row.item_class.as_deref() {
        Some(c) if c == crate::account_state::ItemClass::StateEntry.as_wire() => {
            Some(ExemptClass::StateEntry)
        }
        Some(c) if c == crate::account_state::ItemClass::RecordCid.as_wire() => {
            Some(ExemptClass::RecordCid)
        }
        _ => None,
    }
}

/// Whether a served set's WebDAV pseudo-device row is one the owner's adoption
/// signs (`writer-signed-change-records.md` ruling (7)(b)(i)(1)) — exactly the
/// honest DAV recorder's row shape, no wider: a STRICT delete (`delete` with no
/// manifest) or a non-delete carrying a `content_key_version`; a sealed label
/// whose generation is present; and no `thumbnail_hash`, `derived_through` or
/// `is_resolution` — every field the recorder never sets. The one home of the
/// test: the composition's sweep, the nest's adopt handler and the nest's flip
/// count all call it, so the count the flip refuses with and the rows the sweep
/// signs cannot disagree. Says nothing about WHOSE row it is — the caller has
/// already selected the set's pseudo-device rows.
pub fn served_row_adoptable(row: &SyncChange) -> bool {
    let content = if row.change_type == "delete" {
        row.manifest_hash.is_none()
    } else {
        row.content_key_version.is_some()
    };
    let label_generation = row
        .path_sealed
        .as_ref()
        .and_then(|b| fauna_core::path_crypto::SealedLabel::from_bytes(b).ok())
        .and_then(|l| l.generation)
        .is_some();
    content
        && label_generation
        && row.thumbnail_hash.is_none()
        && row.derived_through.is_none()
        && row.is_resolution != Some(true)
}

/// A reader's `DeviceAuthorization` cache, keyed `(actor, device_key)`
/// (ruling (2)): fed from every list reply's `signer_certs` side table and
/// every inline cert. **A different cert for a cached key REPLACES the cached
/// one** — a re-ceremony certifies the same key with more capabilities, so a
/// cache pinned to the first cert would refuse that writer for ever. Entries
/// are not verified on ingest; the chain runs on every use.
#[derive(Debug, Clone, Default)]
pub struct SignerCertCache {
    certs: HashMap<([u8; 32], [u8; 32]), EmbedAsBytes>,
}

impl SignerCertCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest one carried cert. A cert whose bytes do not decode is dropped
    /// (it could never verify); nothing else is judged here.
    pub fn ingest(&mut self, cert: &EmbedAsBytes) {
        let Ok(decoded) = fauna_core::encoding::decode_signed_bytes::<
            fauna_core::data::DeviceAuthorization,
        >(&cert.bytes) else {
            return;
        };
        self.certs
            .insert((decoded.actor_id.0, decoded.device_key), cert.clone());
    }

    /// Ingest a side table.
    pub fn ingest_all<'a>(&mut self, certs: impl IntoIterator<Item = &'a EmbedAsBytes>) {
        for c in certs {
            self.ingest(c);
        }
    }

    pub fn get(&self, actor: &[u8; 32], device_key: &[u8; 32]) -> Option<&EmbedAsBytes> {
        self.certs.get(&(*actor, *device_key))
    }

    /// The actors the cached certs certifying `device_key` name — the
    /// by-`device_key` read a reader recovers a delegated row's signed actor
    /// through (ruling (8)(a): after a succession the cert still names, and is
    /// cached under, the retired identity, while the row's stamp names its
    /// successor). Unverified, like every cache entry: the chain runs on use.
    pub fn actors_certifying(&self, device_key: &[u8; 32]) -> Vec<[u8; 32]> {
        self.certs
            .keys()
            .filter(|(_, key)| key == device_key)
            .map(|(actor, _)| *actor)
            .collect()
    }

    pub fn len(&self) -> usize {
        self.certs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.certs.is_empty()
    }
}

/// Verify `signature` over `statement` under `signer_key`, and the chain from
/// `signer_key` to the statement's `actor_id` with [`Capability::SyncWrite`]:
/// direct when `signer_key == actor_id`, else through the cached cert and the
/// shared [`fauna_core::encoding::verify_delegation_cert`] chain. `created_at`
/// is the instant the cert's expiry is judged at (the row's nest-stamped
/// `created_at` for a reader; now for the recording nest).
///
/// Answers *who wrote* only. The caller adds *this set* (the nonce it bound
/// the statement to) and *may they* ([`verify_writer`]).
pub fn verify_statement(
    statement: &SignedChange,
    signature: &[u8],
    signer_key: &[u8],
    certs: &SignerCertCache,
    created_at: Timestamp,
) -> Result<AuthoringOrigin, ChangeVerifyError> {
    let signer_key = bytes32("signer_key", signer_key)?;
    let sig: [u8; 64] = signature
        .try_into()
        .map_err(|_| ChangeVerifyError::Malformed("signature is not 64 bytes".into()))?;
    let origin = if signer_key == statement.actor_id {
        AuthoringOrigin::Direct
    } else {
        let cert = certs
            .get(&statement.actor_id, &signer_key)
            .ok_or(ChangeVerifyError::CertMissing)?;
        let decoded = fauna_core::encoding::verify_delegation_cert(
            cert,
            &statement.actor_id,
            &Capability::SyncWrite,
            created_at,
        )
        .map_err(|e| ChangeVerifyError::Chain(e.to_string()))?;
        if decoded.device_key != signer_key {
            return Err(ChangeVerifyError::Chain(
                "cert certifies a different device key".into(),
            ));
        }
        AuthoringOrigin::Delegated {
            device_key: signer_key,
        }
    };
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&signer_key)
        .map_err(|_| ChangeVerifyError::SignatureInvalid)?;
    vk.verify_strict(
        &statement.signed_message(),
        &ed25519_dalek::Signature::from_bytes(&sig),
    )
    .map_err(|_| ChangeVerifyError::SignatureInvalid)?;
    Ok(origin)
}

/// What a served row's signature itself establishes — who it was **signed
/// as**, and how the signer chains to that actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedRow {
    /// The statement's `actor_id` the signature verifies over. After a
    /// succession this is the retired identity, whatever the nest's stamp says.
    pub signed_as: [u8; 32],
    pub origin: AuthoringOrigin,
}

/// Recover the actor a served row was signed as — ruling (8)(a): **from the
/// signature, never from the nest's stamp.** A row names its signer by
/// `signer_key` alone and the signature covers the statement's `actor_id`, so
/// the signed actor is whichever candidate the row verifies under, tried in
/// cost order:
///
/// 1. the served `author_actor_id` — every row no succession has moved (one
///    verification, as before this rule);
/// 2. `signer_key` itself — a direct signature by a since-retired identity;
/// 3. the actor each cached cert certifying `signer_key` names — a delegated
///    one ([`SignerCertCache::actors_certifying`]).
///
/// *Verifies* is [`verify_statement`]'s whole check with that candidate as the
/// statement's actor. One signature does not verify (`verify_strict`) over two
/// distinct statements, so at most one candidate succeeds and the order is a
/// cost choice only. The nest chooses nothing: a lying `author_actor_id` or a
/// planted cert can make a row fail, never pass, and never changes which actor
/// it passes as.
///
/// Answers *who signed, and as whom* only. The caller adds *this set* (the
/// nonce it bound the statement to) and *may they* — whether that actor
/// resolves to a writer.
pub fn recover_signed_actor(
    row: &SyncChange,
    set_nonce: [u8; SET_NONCE_LEN],
    certs: &SignerCertCache,
) -> Result<SignedRow, ChangeVerifyError> {
    let (Some(signature), Some(signer_key)) = (&row.signature, &row.signer_key) else {
        return Err(ChangeVerifyError::Unsigned);
    };
    let signer = bytes32("signer_key", signer_key)?;
    // The row's `created_at` is nest-stamped epoch MILLIS; `Timestamp` is
    // micros. A negative stamp judges expiry at the epoch (fail-open only
    // for a cert that has no expiry, which is every ceremony grant).
    let at = Timestamp((row.created_at.max(0) as u64).saturating_mul(1000));
    // A stamp that is absent or not an actor id is simply no candidate.
    let served = row
        .author_actor_id
        .as_deref()
        .and_then(|a| fauna_core::hex32::decode(a).ok());
    let mut candidates: Vec<[u8; 32]> = Vec::new();
    for candidate in served
        .into_iter()
        .chain([signer])
        .chain(certs.actors_certifying(&signer))
    {
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    // Which failure is reported when none verifies: a cert that was found and
    // refused by the chain says most; else the served author's (the message
    // an unmoved row always gave); else the first.
    let mut chain_error = None;
    let mut served_error = None;
    let mut first_error = None;
    for candidate in candidates {
        let statement = SignedChange::for_row_as(row, set_nonce, candidate)?;
        match verify_statement(&statement, signature, signer_key, certs, at) {
            Ok(origin) => {
                return Ok(SignedRow {
                    signed_as: candidate,
                    origin,
                });
            }
            Err(e) => {
                if matches!(e, ChangeVerifyError::Chain(_)) && chain_error.is_none() {
                    chain_error = Some(e.clone());
                }
                if Some(candidate) == served {
                    served_error = Some(e.clone());
                }
                first_error.get_or_insert(e);
            }
        }
    }
    Err(chain_error
        .or(served_error)
        .or(first_error)
        .unwrap_or(ChangeVerifyError::SignatureInvalid))
}

/// The reader's whole check of one served row (ruling (3)), after class
/// routing: signed, the statement rebuilt under the reader's `set_nonce`
/// verifies with a `SyncWrite` chain as the actor [`recover_signed_actor`]
/// finds, and that **signed** actor is a writer (`is_writer` — the set's owner,
/// or a `writer`-access member per the reader's roster read; fail-closed when
/// no roster was ever read). A reader that must also place a retired identity
/// under its successor resolves the signed actor itself
/// (`sync_row_verify::RowReader`).
pub fn verify_row(
    row: &SyncChange,
    set_nonce: [u8; SET_NONCE_LEN],
    certs: &SignerCertCache,
    is_writer: impl FnOnce(&[u8; 32]) -> bool,
) -> Result<AuthoringOrigin, ChangeVerifyError> {
    let signed = recover_signed_actor(row, set_nonce, certs)?;
    if !is_writer(&signed.signed_as) {
        return Err(ChangeVerifyError::NotAWriter);
    }
    Ok(signed.origin)
}

/// Whether a plaintext `path` a reader would place by matches the signed
/// `path_hash` (ruling (2): the plaintext path is not covered; a reader that
/// places by it — the public plane, `path_sealed` absent — recomputes).
pub fn plaintext_path_matches(row: &SyncChange) -> bool {
    match (&row.path, fauna_core::hex32::decode(&row.path_hash)) {
        (Some(p), Ok(h)) => fauna_core::sync::path_hash(p) == h,
        _ => false,
    }
}

/// The writer side of ruling (1): the key a host signs its change records
/// with, and how a verifier reaches the account from it. One value, so the
/// key and the carriage that certifies it never travel apart.
///
/// - [`Self::delegated`] — the machine's store device principal writer key
///   plus its root-signed `DeviceAuthorization` (which must carry
///   [`Capability::SyncWrite`]); `signer_key` is the device key.
/// - [`Self::direct`] — the identity key itself, on a seed-holding host with no
///   principal; `signer_key` is the actor id and no cert travels.
pub struct ChangeSigner {
    actor_id: [u8; 32],
    key: ed25519_dalek::SigningKey,
    /// The root-signed cert, delegated signers only.
    cert: Option<EmbedAsBytes>,
}

impl std::fmt::Debug for ChangeSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangeSigner")
            .field("actor_id", &hex::encode(self.actor_id))
            .field("signer_key", &hex::encode(self.signer_key()))
            .field("delegated", &self.cert.is_some())
            .finish()
    }
}

impl ChangeSigner {
    /// The identity key signing directly (`signer_key == actor_id`).
    pub fn direct(identity: &fauna_core::identity::ActorKeypair) -> Self {
        Self {
            actor_id: identity.actor_id().0,
            key: ed25519_dalek::SigningKey::from_bytes(identity.secret_bytes()),
            cert: None,
        }
    }

    /// The machine principal's writer key, certified by `cert` for `actor_id`.
    pub fn delegated(
        actor_id: [u8; 32],
        writer_key: ed25519_dalek::SigningKey,
        cert: EmbedAsBytes,
    ) -> Self {
        Self {
            actor_id,
            key: writer_key,
            cert: Some(cert),
        }
    }

    /// Rebuild a delegated signer from the **carriage** a capability host is
    /// provisioned (ruling (1), *The capability host*): the principal's writer
    /// secret plus the canonical `EmbedAsBytes` encoding of its
    /// `DeviceAuthorization` — the exact bytes the principal slot keeps. Checked
    /// here, before the host signs anything, through the same chain every
    /// verifier runs: the cert is root-signed by `actor_id`, grants
    /// [`Capability::SyncWrite`], is unexpired now, and certifies this very key.
    /// A carriage that fails any of those would only mint records the nest
    /// refuses, so it is refused up front.
    pub fn from_delegated_carriage(
        actor_id: [u8; 32],
        writer_secret: &[u8; 32],
        device_authorization: &[u8],
    ) -> Result<Self, ChangeVerifyError> {
        let cert: EmbedAsBytes = fauna_core::encoding::canonical_decode(device_authorization)
            .map_err(|e| ChangeVerifyError::Malformed(format!("device authorization: {e}")))?;
        let key = ed25519_dalek::SigningKey::from_bytes(writer_secret);
        let decoded = fauna_core::encoding::verify_delegation_cert(
            &cert,
            &actor_id,
            &Capability::SyncWrite,
            Timestamp::now(),
        )
        .map_err(|e| ChangeVerifyError::Chain(e.to_string()))?;
        if decoded.device_key != key.verifying_key().to_bytes() {
            return Err(ChangeVerifyError::Chain(
                "cert certifies a different device key than the writer secret".into(),
            ));
        }
        Ok(Self::delegated(actor_id, key, cert))
    }

    /// The account the signatures speak for.
    pub fn actor_id(&self) -> [u8; 32] {
        self.actor_id
    }

    /// The key a verifier checks the signature under — the row's `signer_key`.
    pub fn signer_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    /// This signer's signature over `statement` — Ed25519 is deterministic, so
    /// a holder compares it against a served row's `signature` to recognise a
    /// row it signed itself (the engine's re-record leg, ruling (g)).
    pub fn sign_statement(&self, statement: &SignedChange) -> [u8; 64] {
        statement.sign(&self.key)
    }

    /// The root-signed cert a verifier chains [`Self::signer_key`] through —
    /// `None` for a direct signer. What a holder stores beside a row it signed
    /// so the row stays self-contained on a hop with no side table (the p2p
    /// share leg's inline `signer_cert`).
    pub fn carried_cert(&self) -> Option<&EmbedAsBytes> {
        self.cert.as_ref()
    }

    /// Sign a row as it will be SERVED under `set_nonce`: fills `signature` and
    /// `signer_key` over [`SignedChange::for_row`] — the reader's statement, so
    /// the row verifies exactly as it travels. `author_actor_id` must already
    /// name this signer's account (the statement's signed actor); a row naming
    /// anyone else is refused, never signed. For a holder serving its own
    /// retained rows off-nest (the p2p share leg), where no record request
    /// exists to sign.
    pub fn sign_row(
        &self,
        row: &mut SyncChange,
        set_nonce: [u8; SET_NONCE_LEN],
    ) -> Result<(), ChangeVerifyError> {
        let statement = SignedChange::for_row(row, set_nonce)?;
        if statement.actor_id != self.actor_id {
            return Err(ChangeVerifyError::Malformed(
                "author_actor_id is not this signer's account".into(),
            ));
        }
        row.signature = Some(serde_bytes::ByteBuf::from(
            statement.sign(&self.key).to_vec(),
        ));
        row.signer_key = Some(serde_bytes::ByteBuf::from(self.signer_key().to_vec()));
        Ok(())
    }

    /// Sign `req` under `set_nonce`: fills `signature` and `signer_key`, and —
    /// on a cross-nest relay (`nest_url` set), where the home nest holds no
    /// row of this writer's grants — the cert inline as `signer_cert`
    /// (devices.md § Carriage rules for a trust boundary). Sign LAST: every
    /// covered field must already hold its final value.
    pub fn sign_record(
        &self,
        req: &mut SyncChangeRecordRequest,
        set_nonce: [u8; SET_NONCE_LEN],
    ) -> Result<(), ChangeVerifyError> {
        let statement = SignedChange::for_record(req, self.actor_id, set_nonce)?;
        req.signature = Some(serde_bytes::ByteBuf::from(
            statement.sign(&self.key).to_vec(),
        ));
        req.signer_key = Some(serde_bytes::ByteBuf::from(self.signer_key().to_vec()));
        req.signer_cert = if req.nest_url.is_some() {
            self.cert.clone()
        } else {
            None
        };
        Ok(())
    }

    /// Sign a resolved report's winner head row
    /// ([`SignedChange::for_resolved_report`]) and the retention row of the
    /// loser it retains ([`SignedChange::for_retained_loser`], ruling (10)(d))
    /// under `set_nonce`: fills `winner_signature`, `winner_signer_key` and —
    /// when the report retains a loser — `loser_signature`, which verifies
    /// under the same key (one signer per report). An unresolved report mints
    /// no row and is left untouched. The nest resolves the signer by reference
    /// (the report is same-nest only), so no cert travels. Sign LAST, like
    /// [`Self::sign_record`].
    pub fn sign_report(
        &self,
        req: &mut crate::folders::ConflictReportRequest,
        set_nonce: [u8; SET_NONCE_LEN],
    ) -> Result<(), ChangeVerifyError> {
        let Some(winner) = SignedChange::for_resolved_report(req, self.actor_id, set_nonce)? else {
            return Ok(());
        };
        let loser = SignedChange::for_retained_loser(req, self.actor_id, set_nonce)?;
        req.winner_signature = Some(serde_bytes::ByteBuf::from(winner.sign(&self.key).to_vec()));
        req.winner_signer_key = Some(serde_bytes::ByteBuf::from(self.signer_key().to_vec()));
        req.loser_signature = loser.map(|l| serde_bytes::ByteBuf::from(l.sign(&self.key).to_vec()));
        Ok(())
    }

    /// Sign a choose-winner resolve of `conflict`
    /// ([`SignedChange::for_choose_winner`]) under `set_nonce`: fills
    /// `winner_signature` and `winner_signer_key`. A mark-only resolve (no
    /// winner) mints no row and is left untouched.
    pub fn sign_choose_winner(
        &self,
        req: &mut crate::folders::ConflictResolveRequest,
        conflict: &crate::folders::SyncConflict,
        set_nonce: [u8; SET_NONCE_LEN],
    ) -> Result<(), ChangeVerifyError> {
        let Some(winner) = req.winning_manifest_hash.as_deref() else {
            return Ok(());
        };
        let statement =
            SignedChange::for_choose_winner(conflict, winner, self.actor_id, set_nonce)?;
        req.winner_signature = Some(serde_bytes::ByteBuf::from(
            statement.sign(&self.key).to_vec(),
        ));
        req.winner_signer_key = Some(serde_bytes::ByteBuf::from(self.signer_key().to_vec()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::DeviceAuthorization;
    use fauna_core::identity::ActorKeypair;

    fn statement(actor: [u8; 32]) -> SignedChange {
        SignedChange {
            set_nonce: [7; 32],
            actor_id: actor,
            device_id: [2; 32],
            path_hash: fauna_core::sync::path_hash("docs/a.txt"),
            manifest_hash: Some([3; 32]),
            change_type: "modify".into(),
            size_bytes: 42,
            content_key_version: Some(1),
            path_sealed: Some(vec![9, 9]),
            thumbnail_hash: None,
            derived_through: Some(5),
            is_resolution: false,
            is_retention: false,
        }
    }

    fn cert(
        root: &ActorKeypair,
        device_key: [u8; 32],
        capabilities: Vec<Capability>,
    ) -> EmbedAsBytes {
        let auth = DeviceAuthorization {
            actor_id: root.actor_id(),
            device_key,
            capabilities,
            created_at: Timestamp(0),
            expires_at: None,
        };
        let (bytes, env) = fauna_core::encoding::sign_envelope(root, &auth).unwrap();
        EmbedAsBytes::from_signed(bytes, env)
    }

    fn row_for(s: &SignedChange, sig: [u8; 64], signer: [u8; 32]) -> SyncChange {
        SyncChange {
            seq: 11,
            path_hash: hex::encode(s.path_hash),
            manifest_hash: s.manifest_hash.map(hex::encode),
            size_bytes: s.size_bytes,
            change_type: s.change_type.clone(),
            created_at: 1,
            path: Some("docs/a.txt".into()),
            device_id: Some(hex::encode(s.device_id)),
            content_key_version: s.content_key_version,
            thumbnail_hash: s.thumbnail_hash.map(hex::encode),
            author_actor_id: Some(hex::encode(s.actor_id)),
            path_sealed: s.path_sealed.clone().map(serde_bytes::ByteBuf::from),
            derived_through: s.derived_through,
            is_resolution: Some(s.is_resolution),
            signature: Some(serde_bytes::ByteBuf::from(sig.to_vec())),
            signer_key: Some(serde_bytes::ByteBuf::from(signer.to_vec())),
            ..Default::default()
        }
    }

    /// KAT: the domain tag, and the message's exact prefix and length, pinned —
    /// a change to either is a wire break every signer and verifier must see.
    #[test]
    fn the_signed_message_is_the_tag_then_the_canonical_statement() {
        assert_eq!(
            SYNC_CHANGE_WRITER_SIG_V1,
            b"fauna.sync.change.writer-sig.v1\0"
        );
        let m = statement([1; 32]).signed_message();
        assert!(m.starts_with(SYNC_CHANGE_WRITER_SIG_V1));
        let body = &m[SYNC_CHANGE_WRITER_SIG_V1.len()..];
        // Deterministic: the canonical encoding is a function of the statement.
        assert_eq!(m, statement([1; 32]).signed_message());
        // Byte-pinned digest of the whole message (update only with a deliberate
        // wire change, never to make a red test green).
        assert_eq!(
            blake3::hash(&m).to_hex().as_str(),
            KAT_MESSAGE_BLAKE3,
            "the SignedChange signed message changed — a wire break"
        );
        assert!(!body.is_empty());
    }

    const KAT_MESSAGE_BLAKE3: &str =
        "a56569381848db0df60b96de7a97144e6a8479acec2a009a943ab6026dbfe142";

    #[test]
    fn every_covered_field_changes_the_message() {
        let base = statement([1; 32]);
        let m = base.signed_message();
        let mut variants: Vec<SignedChange> = Vec::new();
        let mut v = base.clone();
        v.set_nonce = [8; 32];
        variants.push(v);
        let mut v = base.clone();
        v.is_retention = true;
        variants.push(v);
        let mut v = base.clone();
        v.actor_id = [4; 32];
        variants.push(v);
        let mut v = base.clone();
        v.device_id = [5; 32];
        variants.push(v);
        let mut v = base.clone();
        v.path_hash = [6; 32];
        variants.push(v);
        let mut v = base.clone();
        v.manifest_hash = None;
        variants.push(v);
        let mut v = base.clone();
        v.change_type = "delete".into();
        variants.push(v);
        let mut v = base.clone();
        v.size_bytes = 43;
        variants.push(v);
        let mut v = base.clone();
        v.content_key_version = Some(2);
        variants.push(v);
        let mut v = base.clone();
        v.path_sealed = None;
        variants.push(v);
        let mut v = base.clone();
        v.thumbnail_hash = Some([1; 32]);
        variants.push(v);
        let mut v = base.clone();
        v.derived_through = Some(6);
        variants.push(v);
        let mut v = base.clone();
        v.is_resolution = true;
        variants.push(v);
        for (i, v) in variants.iter().enumerate() {
            assert_ne!(v.signed_message(), m, "field {i} is not covered");
        }
    }

    #[test]
    fn a_direct_signature_verifies_and_binds_the_set_nonce() {
        let root = ActorKeypair::generate();
        let actor = root.actor_id().0;
        let s = statement(actor);
        let sig = s.sign(root.signing_key());
        let row = row_for(&s, sig, actor);
        let certs = SignerCertCache::new();
        assert_eq!(
            verify_row(&row, s.set_nonce, &certs, |a| *a == actor),
            Ok(AuthoringOrigin::Direct)
        );
        // Another set's nonce: the same row does not verify (a copied record).
        assert_eq!(
            verify_row(&row, [0xEE; 32], &certs, |a| *a == actor),
            Err(ChangeVerifyError::SignatureInvalid)
        );
        // A signed actor off the roster.
        assert_eq!(
            verify_row(&row, s.set_nonce, &certs, |_| false),
            Err(ChangeVerifyError::NotAWriter)
        );
    }

    #[test]
    fn a_delegated_signature_needs_a_sync_write_cert_for_that_key() {
        let root = ActorKeypair::generate();
        let actor = root.actor_id().0;
        let writer = ActorKeypair::generate();
        let writer_pub = writer.actor_id().0;
        let s = statement(actor);
        let sig = s.sign(writer.signing_key());
        let row = row_for(&s, sig, writer_pub);

        let mut certs = SignerCertCache::new();
        assert_eq!(
            verify_row(&row, s.set_nonce, &certs, |_| true),
            Err(ChangeVerifyError::CertMissing)
        );
        // A `[RenewBearer]`-only grant authors nothing.
        certs.ingest(&cert(&root, writer_pub, vec![Capability::RenewBearer]));
        assert!(matches!(
            verify_row(&row, s.set_nonce, &certs, |_| true),
            Err(ChangeVerifyError::Chain(_))
        ));
        // The re-ceremony's cert for the SAME key replaces the cached one.
        certs.ingest(&cert(
            &root,
            writer_pub,
            vec![Capability::RenewBearer, Capability::SyncWrite],
        ));
        assert_eq!(certs.len(), 1);
        assert_eq!(
            verify_row(&row, s.set_nonce, &certs, |_| true),
            Ok(AuthoringOrigin::Delegated {
                device_key: writer_pub
            })
        );
        // A cert from another root for this key never chains to `actor`.
        let other = ActorKeypair::generate();
        let mut foreign = SignerCertCache::new();
        foreign.ingest(&cert(&other, writer_pub, vec![Capability::SyncWrite]));
        assert_eq!(
            verify_row(&row, s.set_nonce, &foreign, |_| true),
            Err(ChangeVerifyError::CertMissing)
        );
    }

    #[test]
    fn an_unsigned_or_altered_row_is_refused() {
        let root = ActorKeypair::generate();
        let actor = root.actor_id().0;
        let s = statement(actor);
        let sig = s.sign(root.signing_key());
        let mut unsigned = row_for(&s, sig, actor);
        unsigned.signature = None;
        let certs = SignerCertCache::new();
        assert_eq!(
            verify_row(&unsigned, s.set_nonce, &certs, |_| true),
            Err(ChangeVerifyError::Unsigned)
        );
        let mut altered = row_for(&s, sig, actor);
        altered.size_bytes += 1;
        assert_eq!(
            verify_row(&altered, s.set_nonce, &certs, |_| true),
            Err(ChangeVerifyError::SignatureInvalid)
        );
        // The nest re-stamping the author onto another actor changes nothing
        // (ruling (8)(a)): the signed actor is recovered from the signature,
        // so the writer check still asks about the real signer — never about
        // the stamp.
        let mut restamped = row_for(&s, sig, actor);
        restamped.author_actor_id = Some(hex::encode([0x44; 32]));
        let mut asked = None;
        assert_eq!(
            verify_row(&restamped, s.set_nonce, &certs, |a| {
                asked = Some(*a);
                true
            }),
            Ok(AuthoringOrigin::Direct)
        );
        assert_eq!(asked, Some(actor), "the signed actor, not the stamp");
        assert_eq!(
            verify_row(&restamped, s.set_nonce, &certs, |a| *a == [0x44; 32]),
            Err(ChangeVerifyError::NotAWriter),
            "a stamp naming a writer admits nobody else's signature"
        );
    }

    /// Ruling (8)(a): after a succession the nest serves the predecessor's row
    /// with the SUCCESSOR as author — a delegated row's cert still names (and is
    /// cached under) the predecessor, a direct row's `signer_key` is the
    /// predecessor itself. Both recover the predecessor as the signed actor;
    /// with no stamp at all they recover it too.
    #[test]
    fn a_moved_rows_signed_actor_is_recovered_from_the_signature() {
        let predecessor = ActorKeypair::generate();
        let successor = ActorKeypair::generate().actor_id().0;
        let p = predecessor.actor_id().0;
        let s = statement(p);

        // Direct.
        let mut direct = row_for(&s, s.sign(predecessor.signing_key()), p);
        direct.author_actor_id = Some(hex::encode(successor));
        let certs = SignerCertCache::new();
        assert_eq!(
            recover_signed_actor(&direct, s.set_nonce, &certs),
            Ok(SignedRow {
                signed_as: p,
                origin: AuthoringOrigin::Direct
            })
        );
        direct.author_actor_id = None;
        assert_eq!(
            recover_signed_actor(&direct, s.set_nonce, &certs).map(|r| r.signed_as),
            Ok(p)
        );

        // Delegated.
        let writer = ActorKeypair::generate();
        let device_key = writer.actor_id().0;
        let mut delegated = row_for(&s, s.sign(writer.signing_key()), device_key);
        delegated.author_actor_id = Some(hex::encode(successor));
        let mut certs = SignerCertCache::new();
        assert_eq!(
            recover_signed_actor(&delegated, s.set_nonce, &certs),
            Err(ChangeVerifyError::CertMissing),
            "no cert for the device key: the served author's answer stands"
        );
        certs.ingest(&cert(&predecessor, device_key, vec![Capability::SyncWrite]));
        assert_eq!(certs.actors_certifying(&device_key), vec![p]);
        assert_eq!(
            recover_signed_actor(&delegated, s.set_nonce, &certs),
            Ok(SignedRow {
                signed_as: p,
                origin: AuthoringOrigin::Delegated { device_key }
            })
        );
        // A planted cert from another root for the same key never makes the
        // row pass as that root: the signature covers the real actor.
        let planted = ActorKeypair::generate();
        let mut only_planted = SignerCertCache::new();
        only_planted.ingest(&cert(&planted, device_key, vec![Capability::SyncWrite]));
        assert!(recover_signed_actor(&delegated, s.set_nonce, &only_planted).is_err());
        certs.ingest(&cert(&planted, device_key, vec![Capability::SyncWrite]));
        assert_eq!(
            recover_signed_actor(&delegated, s.set_nonce, &certs).map(|r| r.signed_as),
            Ok(p)
        );
    }

    #[test]
    fn the_record_and_row_statements_agree() {
        let actor = [1u8; 32];
        let req = SyncChangeRecordRequest {
            folder: "f".into(),
            device_id: hex::encode([2; 32]),
            path: "docs/a.txt".into(),
            manifest_hash: Some(hex::encode([3; 32])),
            size_bytes: 42,
            change_type: "modify".into(),
            content_key_version: Some(1),
            path_sealed: Some(serde_bytes::ByteBuf::from(vec![9, 9])),
            derived_through: Some(5),
            is_resolution: None,
            ..Default::default()
        };
        let signed = SignedChange::for_record(&req, actor, [7; 32]).unwrap();
        assert_eq!(signed, statement(actor));
        let row = row_for(&signed, [0; 64], actor);
        assert_eq!(SignedChange::for_row(&row, [7; 32]).unwrap(), signed);
        assert!(plaintext_path_matches(&row));
        let mut moved = row;
        moved.path = Some("docs/b.txt".into());
        assert!(!plaintext_path_matches(&moved));
    }

    /// The re-home statement IS the row the materialize arm mints: an owner
    /// signature over [`SignedChange::for_rehome`] verifies on every reader as
    /// that row (the pseudo-device id, `create`, no plaintext path), and binds
    /// the target's nonce (`writer-signed-change-records.md` ruling (7)(a)(ii)).
    #[test]
    fn a_rehome_signature_verifies_as_the_minted_row() {
        let root = ActorKeypair::from_secret([0x31; 32]);
        let owner = root.actor_id().0;
        let s = SignedChange::for_rehome([7; 32], owner, [4; 32], [5; 32], 99, b"sealed");
        assert_eq!(
            s.device_id,
            fauna_core::label_custody::reseed_pseudo_device_id(&owner)
        );
        let sig = s.sign(root.signing_key());
        let mut row = row_for(&s, sig, owner);
        // The arm mints no plaintext path on a live folder set.
        row.path = None;
        assert_eq!(SignedChange::for_row(&row, [7; 32]).unwrap(), s);
        let certs = SignerCertCache::new();
        assert_eq!(
            verify_row(&row, [7; 32], &certs, |a| *a == owner),
            Ok(AuthoringOrigin::Direct)
        );
        assert_eq!(
            verify_row(&row, [8; 32], &certs, |a| *a == owner),
            Err(ChangeVerifyError::SignatureInvalid),
            "bound to the target's nonce"
        );
    }

    fn record_req() -> SyncChangeRecordRequest {
        SyncChangeRecordRequest {
            folder: "f".into(),
            device_id: hex::encode([2; 32]),
            path: "docs/a.txt".into(),
            manifest_hash: Some(hex::encode([3; 32])),
            size_bytes: 42,
            change_type: "modify".into(),
            ..Default::default()
        }
    }

    /// The nest's side of a record: rebuild the statement under the recording
    /// actor and the stored nonce, verify with the request's own carriage.
    fn verify_req(
        req: &SyncChangeRecordRequest,
        actor: [u8; 32],
        nonce: [u8; 32],
        certs: &SignerCertCache,
    ) -> Result<AuthoringOrigin, ChangeVerifyError> {
        let statement = SignedChange::for_record(req, actor, nonce)?;
        verify_statement(
            &statement,
            req.signature
                .as_deref()
                .ok_or(ChangeVerifyError::Unsigned)?,
            req.signer_key
                .as_deref()
                .ok_or(ChangeVerifyError::Unsigned)?,
            certs,
            Timestamp(1),
        )
    }

    #[test]
    fn a_direct_signer_signs_a_record_the_nest_verifies() {
        let root = ActorKeypair::from_secret([0x21; 32]);
        let signer = ChangeSigner::direct(&root);
        assert_eq!(signer.signer_key(), root.actor_id().0);
        let mut req = record_req();
        signer.sign_record(&mut req, [7; 32]).unwrap();
        assert!(req.signer_cert.is_none(), "a direct signer carries no cert");
        let certs = SignerCertCache::new();
        assert_eq!(
            verify_req(&req, root.actor_id().0, [7; 32], &certs).unwrap(),
            AuthoringOrigin::Direct
        );
        assert!(
            verify_req(&req, root.actor_id().0, [8; 32], &certs).is_err(),
            "bound to the set nonce it was signed under"
        );
    }

    #[test]
    fn a_delegated_signer_verifies_through_its_cert_and_carries_it_only_cross_nest() {
        let root = ActorKeypair::from_secret([0x22; 32]);
        let writer = ed25519_dalek::SigningKey::from_bytes(&[0x33; 32]);
        let device_key = writer.verifying_key().to_bytes();
        let cert = cert(
            &root,
            device_key,
            vec![Capability::RenewBearer, Capability::SyncWrite],
        );
        let signer = ChangeSigner::delegated(root.actor_id().0, writer, cert.clone());
        // The carriage a capability host is provisioned rebuilds the same
        // signer — and one it must never be handed is refused.
        let carriage = fauna_core::encoding::canonical_encode(&cert).unwrap();
        let rebuilt =
            ChangeSigner::from_delegated_carriage(root.actor_id().0, &[0x33; 32], &carriage)
                .expect("a SyncWrite carriage over this key rebuilds");
        assert_eq!(rebuilt.signer_key(), device_key);
        assert!(
            ChangeSigner::from_delegated_carriage(root.actor_id().0, &[0x44; 32], &carriage)
                .is_err(),
            "a cert over another key"
        );
        assert!(
            ChangeSigner::from_delegated_carriage([0x55; 32], &[0x33; 32], &carriage).is_err(),
            "a cert for another account"
        );
        let renew_only = fauna_core::encoding::canonical_encode(&self::cert(
            &root,
            device_key,
            vec![Capability::RenewBearer],
        ))
        .unwrap();
        assert!(
            ChangeSigner::from_delegated_carriage(root.actor_id().0, &[0x33; 32], &renew_only)
                .is_err(),
            "a RenewBearer-only grant signs nothing"
        );
        assert!(
            ChangeSigner::from_delegated_carriage(root.actor_id().0, &[0x33; 32], b"junk").is_err(),
            "an undecodable carriage"
        );

        let mut same_nest = record_req();
        signer.sign_record(&mut same_nest, [7; 32]).unwrap();
        assert!(same_nest.signer_cert.is_none(), "same-nest: by reference");
        let mut certs = SignerCertCache::new();
        assert_eq!(
            verify_req(&same_nest, root.actor_id().0, [7; 32], &certs),
            Err(ChangeVerifyError::CertMissing)
        );
        certs.ingest(&cert);
        assert_eq!(
            verify_req(&same_nest, root.actor_id().0, [7; 32], &certs).unwrap(),
            AuthoringOrigin::Delegated { device_key }
        );

        let mut relayed = record_req();
        relayed.nest_url = Some("https://home.example".into());
        signer.sign_record(&mut relayed, [7; 32]).unwrap();
        assert_eq!(
            relayed.signer_cert.as_ref(),
            Some(&cert),
            "inline across the boundary"
        );
    }

    fn resolved_report(winner: [u8; 32]) -> crate::folders::ConflictReportRequest {
        use crate::folders::{ConflictCandidate, ConflictReportRequest};
        ConflictReportRequest {
            folder: "docs".into(),
            device_id: hex::encode([0x3a; 32]),
            path: "notes.txt".into(),
            path_sealed: Some(serde_bytes::ByteBuf::from(vec![9, 9])),
            candidates: vec![
                ConflictCandidate {
                    manifest_hash: hex::encode([0x11; 32]),
                    device_id: hex::encode([0x3a; 32]),
                    size_bytes: 10,
                    ..Default::default()
                },
                ConflictCandidate {
                    manifest_hash: hex::encode([0x22; 32]),
                    device_id: hex::encode([0x4b; 32]),
                    size_bytes: 20,
                    content_key_version: Some(3),
                    ..Default::default()
                },
            ],
            resolution: Some("latest_wins".into()),
            winning_manifest_hash: Some(hex::encode(winner)),
            winning_size_bytes: Some(15),
            winning_content_key_version: Some(4),
            winning_derived_through: Some(7),
            ..Default::default()
        }
    }

    /// The resolved-report statement is the winner row exactly as the nest
    /// mints it (ruling (1)(ii)): a candidate winner takes the candidate's
    /// device, size and generation; a merged winner the reporter's device and
    /// the request's `winning_*`; the path hash is derived when absent; the
    /// class flips with `winning_carries_novelty`.
    #[test]
    fn the_resolved_report_statement_is_the_minted_winner_row() {
        let actor = [1; 32];
        let candidate =
            SignedChange::for_resolved_report(&resolved_report([0x22; 32]), actor, [7; 32])
                .unwrap()
                .expect("a resolved report signs its winner");
        assert_eq!(
            candidate.device_id, [0x4b; 32],
            "the winning candidate's device"
        );
        assert_eq!(candidate.size_bytes, 20);
        assert_eq!(candidate.content_key_version, Some(3));
        assert_eq!(
            candidate.path_hash,
            fauna_core::sync::path_hash("notes.txt")
        );
        assert_eq!(candidate.path_sealed, Some(vec![9, 9]));
        assert_eq!(candidate.change_type, "modify");
        assert_eq!(candidate.derived_through, Some(7), "the claim as sent");
        assert!(candidate.is_resolution);
        assert_eq!(candidate.thumbnail_hash, None);

        let mut merged = resolved_report([0x66; 32]);
        merged.winning_carries_novelty = Some(true);
        let merged = SignedChange::for_resolved_report(&merged, actor, [7; 32])
            .unwrap()
            .unwrap();
        assert_eq!(merged.device_id, [0x3a; 32], "the reporter's own device");
        assert_eq!(merged.size_bytes, 15);
        assert_eq!(merged.content_key_version, Some(4));
        assert!(!merged.is_resolution, "novelty mints edit-class");

        let mut unresolved = resolved_report([0x22; 32]);
        unresolved.resolution = None;
        assert_eq!(
            SignedChange::for_resolved_report(&unresolved, actor, [7; 32]).unwrap(),
            None
        );
    }

    #[test]
    fn sign_report_and_choose_winner_verify_as_the_minted_rows() {
        let account = ActorKeypair::generate();
        let actor = account.actor_id().0;
        let signer = ChangeSigner::direct(&account);

        let mut report = resolved_report([0x22; 32]);
        signer.sign_report(&mut report, [7; 32]).unwrap();
        let statement = SignedChange::for_resolved_report(&report, actor, [7; 32])
            .unwrap()
            .unwrap();
        let sig: [u8; 64] = report.winner_signature.as_ref().unwrap()[..]
            .try_into()
            .unwrap();
        assert_eq!(
            report.winner_signer_key.as_ref().map(|k| &k[..]),
            Some(&actor[..])
        );
        assert_eq!(statement.sign(account.signing_key()), sig);

        let conflict = crate::folders::SyncConflict {
            folder: "docs".into(),
            path_hash: serde_bytes::ByteBuf::from(
                fauna_core::sync::path_hash("notes.txt").to_vec(),
            ),
            path_sealed: Some(serde_bytes::ByteBuf::from(vec![9, 9])),
            candidates: report.candidates.clone(),
            ..Default::default()
        };
        let mut resolve = crate::folders::ConflictResolveRequest {
            id: 5,
            winning_manifest_hash: Some(hex::encode([0x11; 32])),
            ..Default::default()
        };
        signer
            .sign_choose_winner(&mut resolve, &conflict, [7; 32])
            .unwrap();
        let chosen =
            SignedChange::for_choose_winner(&conflict, &hex::encode([0x11; 32]), actor, [7; 32])
                .unwrap();
        assert_eq!(chosen.device_id, [0x3a; 32]);
        assert_eq!(chosen.size_bytes, 10);
        assert_eq!(
            (chosen.derived_through, chosen.is_resolution),
            (None, false)
        );
        let sig: [u8; 64] = resolve.winner_signature.as_ref().unwrap()[..]
            .try_into()
            .unwrap();
        assert_eq!(chosen.sign(account.signing_key()), sig);

        let mut mark_only = crate::folders::ConflictResolveRequest::default();
        signer
            .sign_choose_winner(&mut mark_only, &conflict, [7; 32])
            .unwrap();
        assert_eq!(
            mark_only.winner_signature, None,
            "a mark-only resolve mints nothing"
        );

        let mut stranger = crate::folders::ConflictResolveRequest {
            winning_manifest_hash: Some(hex::encode([0x99; 32])),
            ..Default::default()
        };
        assert!(matches!(
            signer.sign_choose_winner(&mut stranger, &conflict, [7; 32]),
            Err(ChangeVerifyError::Malformed(_))
        ));
    }

    #[test]
    fn row_intrinsic_exemptions() {
        let mut r = SyncChange::default();
        assert_eq!(exempt_class(&r), None);
        r.is_retention = Some(true);
        assert_eq!(exempt_class(&r), Some(ExemptClass::Retention));
    }

    /// Ruling (10)(d): `is_retention` is covered. A retained loser signed with
    /// the flag verifies as served with it and fails with it stripped (it
    /// would otherwise pass as the reporter's fresh edit of the path); an
    /// ordinary signed row fails once the flag is added. An ordinary
    /// statement is byte-identical to before the ruling — the KAT in
    /// `the_signed_message_is_the_tag_then_the_canonical_statement` pins it.
    #[test]
    fn the_retention_flag_is_covered_and_encoded_only_when_true() {
        let account = ActorKeypair::generate();
        let actor = account.actor_id().0;
        let certs = SignerCertCache::new();

        let mut retained = statement(actor);
        retained.is_retention = true;
        let sig = retained.sign(account.signing_key());
        let mut row = row_for(&retained, sig, actor);
        row.is_retention = Some(true);
        assert_eq!(
            recover_signed_actor(&row, [7; 32], &certs)
                .unwrap()
                .signed_as,
            actor,
            "a signed retention row verifies with its flag"
        );
        row.is_retention = None;
        assert_eq!(
            recover_signed_actor(&row, [7; 32], &certs),
            Err(ChangeVerifyError::SignatureInvalid),
            "the flag stripped fails the statement"
        );

        let ordinary = statement(actor);
        let mut row = row_for(&ordinary, ordinary.sign(account.signing_key()), actor);
        row.is_retention = Some(false);
        recover_signed_actor(&row, [7; 32], &certs).expect("an explicit false is the absent flag");
        row.is_retention = Some(true);
        assert_eq!(
            recover_signed_actor(&row, [7; 32], &certs),
            Err(ChangeVerifyError::SignatureInvalid),
            "the flag added to an ordinary row fails the statement"
        );
    }

    /// Ruling (10)(d): the retained loser is the row the nest mints — the
    /// FIRST candidate carrying the reporter's device id and a manifest other
    /// than the winner's (`report_conflict_signed`'s pick), with the request's
    /// path hash and seal, `modify`, no thumbnail, `losing_derived_through`,
    /// not a resolution, `is_retention`. Nothing when the report retains
    /// nothing.
    #[test]
    fn the_retained_loser_statement_is_the_row_the_nest_mints() {
        use crate::folders::ConflictCandidate;
        let actor = [1; 32];
        let reporter = hex::encode([0x3a; 32]);
        let mut req = resolved_report([0x22; 32]);
        req.losing_derived_through = Some(4);
        let loser = SignedChange::for_retained_loser(&req, actor, [7; 32])
            .unwrap()
            .expect("a latest-wins the reporter lost retains its candidate");
        assert_eq!(loser.device_id, [0x3a; 32]);
        assert_eq!(loser.manifest_hash, Some([0x11; 32]));
        assert_eq!(loser.size_bytes, 10);
        assert_eq!(loser.content_key_version, None);
        assert_eq!(loser.path_hash, fauna_core::sync::path_hash("notes.txt"));
        assert_eq!(loser.path_sealed, Some(vec![9, 9]));
        assert_eq!(loser.change_type, "modify");
        assert_eq!(loser.thumbnail_hash, None);
        assert_eq!(loser.derived_through, Some(4), "losing_derived_through");
        assert!(!loser.is_resolution);
        assert!(loser.is_retention);

        // Several candidates carry the reporter's device: the first that is
        // not the winner, exactly the nest's `find`.
        let mut twin = resolved_report([0x22; 32]);
        twin.candidates = [(0x22, 20, None), (0x33, 30, Some(2)), (0x44, 40, None)]
            .into_iter()
            .map(|(m, size_bytes, content_key_version)| ConflictCandidate {
                manifest_hash: hex::encode([m; 32]),
                device_id: reporter.clone(),
                size_bytes,
                content_key_version,
                ..Default::default()
            })
            .collect();
        let picked = SignedChange::for_retained_loser(&twin, actor, [7; 32])
            .unwrap()
            .unwrap();
        assert_eq!(picked.manifest_hash, Some([0x33; 32]));
        assert_eq!(
            (picked.size_bytes, picked.content_key_version),
            (30, Some(2))
        );

        // The reporter's candidate IS the winner: nothing is retained.
        assert_eq!(
            SignedChange::for_retained_loser(&resolved_report([0x11; 32]), actor, [7; 32]).unwrap(),
            None
        );
        // An unresolved report retains nothing.
        let mut open = resolved_report([0x22; 32]);
        open.resolution = None;
        assert_eq!(
            SignedChange::for_retained_loser(&open, actor, [7; 32]).unwrap(),
            None
        );
    }

    /// `sign_report` signs the retained loser beside the winner, under the one
    /// signer, and the signature verifies over the retention row as the nest
    /// mints and serves it.
    #[test]
    fn sign_report_signs_the_retained_loser_as_its_minted_row() {
        let account = ActorKeypair::generate();
        let actor = account.actor_id().0;
        let signer = ChangeSigner::direct(&account);
        let mut report = resolved_report([0x22; 32]);
        report.losing_derived_through = Some(4);
        signer.sign_report(&mut report, [7; 32]).unwrap();
        let minted = SyncChange {
            seq: 12,
            path_hash: hex::encode(fauna_core::sync::path_hash("notes.txt")),
            manifest_hash: Some(hex::encode([0x11; 32])),
            size_bytes: 10,
            change_type: "modify".into(),
            created_at: 1,
            device_id: Some(hex::encode([0x3a; 32])),
            author_actor_id: Some(hex::encode(actor)),
            path_sealed: Some(serde_bytes::ByteBuf::from(vec![9, 9])),
            derived_through: Some(4),
            is_retention: Some(true),
            signature: report.loser_signature.clone(),
            signer_key: report.winner_signer_key.clone(),
            ..Default::default()
        };
        assert!(minted.signature.is_some(), "the retained loser is signed");
        verify_row(&minted, [7; 32], &SignerCertCache::new(), |a| *a == actor)
            .expect("the retention row verifies as its reporter");

        let mut won = resolved_report([0x11; 32]);
        signer.sign_report(&mut won, [7; 32]).unwrap();
        assert!(won.winner_signature.is_some());
        assert_eq!(
            won.loser_signature, None,
            "nothing retained, nothing signed"
        );
    }

    fn label(generation: Option<u64>) -> serde_bytes::ByteBuf {
        let env = fauna_core::path_crypto::SealedLabel {
            v: fauna_core::path_crypto::SEALED_LABEL_V1,
            generation,
            nonce: None,
            ct: serde_bytes::ByteBuf::from(vec![5; 24]),
        };
        serde_bytes::ByteBuf::from(env.to_bytes().unwrap())
    }

    /// The honest DAV recorder's two row shapes: a stamped content row and a
    /// strict tombstone, each under a generation-bearing label.
    fn served(change_type: &str) -> SyncChange {
        let delete = change_type == "delete";
        SyncChange {
            seq: 4,
            path_hash: hex::encode([1; 32]),
            manifest_hash: (!delete).then(|| hex::encode([2; 32])),
            change_type: change_type.into(),
            content_key_version: (!delete).then_some(3),
            path_sealed: Some(label(Some(3))),
            ..Default::default()
        }
    }

    /// Ruling (7)(b)(i)(1): the adoption signs exactly the honest recorder's
    /// row shape — one row of each class.
    #[test]
    fn served_row_adoptable_accepts_only_the_honest_recorders_row_shape() {
        assert!(
            served_row_adoptable(&served("create")),
            "stamped content row"
        );
        assert!(
            served_row_adoptable(&served("modify")),
            "stamped content row"
        );
        assert!(served_row_adoptable(&served("delete")), "strict delete");

        let mut r = served("delete");
        r.manifest_hash = Some(hex::encode([2; 32]));
        assert!(!served_row_adoptable(&r), "a delete carrying a manifest");

        let mut r = served("create");
        r.content_key_version = None;
        assert!(!served_row_adoptable(&r), "an unstamped content row");

        let mut r = served("create");
        r.path_sealed = Some(label(None));
        assert!(!served_row_adoptable(&r), "a generation-less label");
        r.path_sealed = None;
        assert!(!served_row_adoptable(&r), "no label at all");
        r.path_sealed = Some(serde_bytes::ByteBuf::from(vec![0xff, 0x00]));
        assert!(!served_row_adoptable(&r), "a label that does not parse");

        let mut r = served("create");
        r.thumbnail_hash = Some(hex::encode([6; 32]));
        assert!(!served_row_adoptable(&r), "a thumbnail");

        let mut r = served("create");
        r.derived_through = Some(1);
        assert!(!served_row_adoptable(&r), "a causal stamp");

        let mut r = served("create");
        r.is_resolution = Some(true);
        assert!(!served_row_adoptable(&r), "a resolution");
        r.is_resolution = Some(false);
        assert!(served_row_adoptable(&r), "an explicit false is unset");
    }
}
