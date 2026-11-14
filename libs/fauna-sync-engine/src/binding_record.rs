//! The **binding record** — what a host built cold with the nest unreachable
//! builds a set's engine on (`on-demand-files-shared-sets.md` § Shared sets on
//! a capability host, decision 2″).
//!
//! Decision 2′ says every engine stands on the last floor it read. A host the
//! OS relaunches cold has read nothing yet, and its custody is a throwaway
//! replica that walks from the nest (decision 1′), so with the nest out of
//! reach it has neither a row nor a key. The record is the answer: at every
//! resolution that read the row from a nest in reach, the builder writes **the
//! row exactly as read** (a same-nest set's list row; a cross-nest set's custody
//! record, which is its row) and **this set's custody slice alone**
//! ([`fauna_client_folders::engine_binding::custody_slice_for_row`] /
//! [`custody_slice_for_foreign`](fauna_client_folders::engine_binding::custody_slice_for_foreign))
//! — into the set's own state DB, as one sealed `meta` value
//! ([`SyncDb::binding_record`]). It is replaced atomically with the DB's own
//! write and removed with the set.
//!
//! **Sealed as the group ring's mirror is** (`group-set.md` decision 12):
//! ChaCha20-Poly1305 over the canonical dag-cbor record under
//! [`BindingRecordKey`] (a `BackupKey` derivation, a sibling of
//! `GroupRingMirrorKey`), AAD-bound to a versioned context, the actor id and
//! the set's [`FolderRef`] wire form — so a record cannot move between
//! accounts or sets. A record that will not open is an **error**, never an
//! empty binding (decision 2″ (7)).

use anyhow::{Context as _, Result};
use fauna_core::aead_seal;
use fauna_core::crypto::BindingRecordKey;
use fauna_core::data::FoldersConfig;
use fauna_core::encoding::{canonical_decode, canonical_encode};
use fauna_core::folder_keys::FolderRef;
use fauna_protocol::folders::FolderSummary;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::db::SyncDb;

/// The one format version this build writes and reads: the record schema and
/// this byte move together, so an older build refuses a newer record loudly.
const VERSION: u8 = 1;
/// The AEAD's context, versioned and NUL-terminated per the repo's
/// domain-separation convention; the actor id and the set's ref follow it.
const AAD_CONTEXT: &[u8] = b"fauna.binding-record.v1\0";
const HEADER_LEN: usize = 1 + aead_seal::NONCE_LEN;

/// One set's binding record, opened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BindingRecord {
    /// A same-nest set's row as the last successful list read returned it —
    /// the owner's audience attestation included. `None` for a cross-nest
    /// set, whose row is its custody record in [`Self::custody`].
    pub row: Option<FolderSummary>,
    /// This set's custody entries alone — what the resolution read.
    pub custody: FoldersConfig,
    /// This device's adoption marker for the set, when the resolution found
    /// one (`writer-signed-change-records.md` ruling (11)(d)).
    #[serde(default, with = "serde_bytes")]
    pub adoption_marker: Option<[u8; 32]>,
}

/// Why no record could be opened — each a refusal of the cold build
/// (decision 2″ (7)).
#[derive(Debug)]
pub enum OpenError {
    /// No record rests for the set.
    Missing,
    /// The record is from a format version this build does not read.
    UnknownVersion(u8),
    /// The record did not open: another `BackupKey` (a succession), another
    /// account's or set's record, a torn or tampered value.
    WillNotOpen(anyhow::Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(f, "no binding record rests for the set"),
            Self::UnknownVersion(v) => {
                write!(
                    f,
                    "the binding record is format version {v}; this build reads {VERSION}"
                )
            }
            Self::WillNotOpen(e) => write!(f, "the binding record did not open: {e:#}"),
        }
    }
}

fn aad(actor_id_hex: &str, folder_ref: FolderRef) -> Vec<u8> {
    let mut aad = AAD_CONTEXT.to_vec();
    aad.extend_from_slice(actor_id_hex.as_bytes());
    aad.push(0);
    aad.extend_from_slice(folder_ref.to_wire().as_bytes());
    aad
}

/// Seal `record` for `folder_ref` of the account `actor_id_hex`:
/// `version || nonce || ciphertext+tag`.
///
/// # Errors
/// The record failing to encode.
pub fn seal(
    key: &BindingRecordKey,
    actor_id_hex: &str,
    folder_ref: FolderRef,
    record: &BindingRecord,
) -> Result<Vec<u8>> {
    let plain = Zeroizing::new(canonical_encode(record).context("encode the binding record")?);
    let nonce = aead_seal::fresh_nonce();
    let sealed = aead_seal::seal_with_nonce(
        key.as_bytes(),
        &nonce,
        &aad(actor_id_hex, folder_ref),
        &plain,
    );
    let mut out = Vec::with_capacity(HEADER_LEN + sealed.len());
    out.push(VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// Open a value [`seal`] produced for the same account and set.
///
/// # Errors
/// [`OpenError::UnknownVersion`] or [`OpenError::WillNotOpen`].
pub fn open(
    key: &BindingRecordKey,
    actor_id_hex: &str,
    folder_ref: FolderRef,
    sealed: &[u8],
) -> Result<BindingRecord, OpenError> {
    if sealed.len() < HEADER_LEN {
        return Err(OpenError::WillNotOpen(anyhow::anyhow!(
            "the binding record is {} bytes, shorter than its header",
            sealed.len()
        )));
    }
    if sealed[0] != VERSION {
        return Err(OpenError::UnknownVersion(sealed[0]));
    }
    let nonce: [u8; aead_seal::NONCE_LEN] = sealed[1..HEADER_LEN]
        .try_into()
        .expect("the header length covers the nonce");
    let plain = aead_seal::open_with_nonce(
        key.as_bytes(),
        &nonce,
        &aad(actor_id_hex, folder_ref),
        &sealed[HEADER_LEN..],
    )
    .context("a wrong key, another account's or set's record, or a tampered value")
    .map_err(OpenError::WillNotOpen)?;
    let plain = Zeroizing::new(plain);
    canonical_decode(&plain)
        .context("decode the binding record")
        .map_err(OpenError::WillNotOpen)
}

/// Write `record` as the set's binding record in `db`, replacing any.
///
/// # Errors
/// The record failing to encode, or the DB failing to write.
pub fn put(
    db: &SyncDb,
    key: &BindingRecordKey,
    actor_id_hex: &str,
    folder_ref: FolderRef,
    record: &BindingRecord,
) -> Result<()> {
    let sealed = seal(key, actor_id_hex, folder_ref, record)?;
    db.set_binding_record(&hex::encode(sealed))
}

/// Read and open the set's binding record from `db`.
///
/// # Errors
/// [`OpenError::Missing`] with no record; a DB read failure or a malformed
/// value is [`OpenError::WillNotOpen`].
pub fn get(
    db: &SyncDb,
    key: &BindingRecordKey,
    actor_id_hex: &str,
    folder_ref: FolderRef,
) -> Result<BindingRecord, OpenError> {
    let stored = db
        .binding_record()
        .context("read the binding record")
        .map_err(OpenError::WillNotOpen)?
        .ok_or(OpenError::Missing)?;
    let sealed = hex::decode(stored)
        .context("the binding record is not hex")
        .map_err(OpenError::WillNotOpen)?;
    open(key, actor_id_hex, folder_ref, &sealed)
}

/// Remove the set's binding record — a read found the row gone, or the row
/// itself refused the build (decision 2″ (3)).
///
/// # Errors
/// The DB failing to write.
pub fn remove(db: &SyncDb) -> Result<()> {
    db.clear_binding_record()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::crypto::BackupKey;

    fn record() -> BindingRecord {
        BindingRecord {
            row: Some(FolderSummary {
                id: 7,
                name: "docs".into(),
                content_key_floor: Some(3),
                ..FolderSummary::default()
            }),
            custody: FoldersConfig {
                sets: vec![fauna_core::data::FolderKeyCustody {
                    name: Some("docs".into()),
                    set_nonce: Some([9; 32]),
                    ..Default::default()
                }],
                ..FoldersConfig::default()
            },
            adoption_marker: Some([4; 32]),
        }
    }

    fn key(seed: u8) -> BindingRecordKey {
        BindingRecordKey::derive(&BackupKey::derive(&[seed; 32]))
    }

    #[test]
    fn a_record_round_trips_through_the_state_db() {
        let dir = tempfile::tempdir().unwrap();
        let db = SyncDb::open(dir.path().join("fsid.db")).unwrap();
        let r = FolderRef::Local(7);
        assert!(matches!(
            get(&db, &key(1), "aa", r),
            Err(OpenError::Missing)
        ));
        put(&db, &key(1), "aa", r, &record()).unwrap();
        assert_eq!(get(&db, &key(1), "aa", r).unwrap(), record());
        remove(&db).unwrap();
        assert!(matches!(
            get(&db, &key(1), "aa", r),
            Err(OpenError::Missing)
        ));
    }

    /// Decision 2″ (7): another key, another account or another set is an
    /// error, never an empty binding.
    #[test]
    fn a_record_opens_only_for_its_key_account_and_set() {
        let sealed = seal(&key(1), "aa", FolderRef::Local(7), &record()).unwrap();
        for (k, actor, r) in [
            (key(2), "aa", FolderRef::Local(7)),
            (key(1), "bb", FolderRef::Local(7)),
            (key(1), "aa", FolderRef::Local(8)),
            (key(1), "aa", FolderRef::Foreign([7; 32])),
        ] {
            assert!(
                matches!(open(&k, actor, r, &sealed), Err(OpenError::WillNotOpen(_))),
                "{actor} {r:?}"
            );
        }
        let mut torn = sealed.clone();
        *torn.last_mut().unwrap() ^= 1;
        assert!(matches!(
            open(&key(1), "aa", FolderRef::Local(7), &torn),
            Err(OpenError::WillNotOpen(_))
        ));
    }

    #[test]
    fn a_record_of_an_unknown_version_is_refused_by_name() {
        let mut sealed = seal(&key(1), "aa", FolderRef::Local(7), &record()).unwrap();
        sealed[0] = VERSION + 1;
        assert!(matches!(
            open(&key(1), "aa", FolderRef::Local(7), &sealed),
            Err(OpenError::UnknownVersion(v)) if v == VERSION + 1
        ));
    }
}
