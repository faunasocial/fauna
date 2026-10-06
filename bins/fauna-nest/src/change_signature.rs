//! **Ingest verification of writer-signed change records** — the nest half of
//! `docs/goal/architecture/mls-group-key-material.md` § M2 → *Multi-writer* →
//! *Writer-signed change records* (rulings (1)–(3)).
//!
//! One rule: a record that does not verify is not a record. Every record kind
//! that lands a `sync_changes` row a client wrote — `fauna.sync.changes.record`,
//! its federated relay, and the two conflict kinds that mint a winner head row
//! — rebuilds the row's `SignedChange` statement from **what the nest will
//! store**, under the set nonce **the nest has stored** (equality with the
//! client's binding is the only nonce check the nest makes — the value is the
//! client's, never the nest's), and verifies the carried signature through the
//! shared chain ([`fauna_protocol::sync_writer_sig::verify_statement`]).
//!
//! Where the cert comes from is the one plane difference ([`CertCarriage`]):
//! same-nest, **by reference** over every `sync_devices` grant row of the
//! actor carrying the signer key, with the `revoked_device_grants` tombstone
//! consulted (revocation is device-row deletion, enforced here at ingest);
//! across a trust boundary, **inline** on the relay request, since a foreign
//! writer has no device row on this nest.
//!
//! The typed refusals are `signature_invalid`, `signature_required` and
//! `author_mismatch`, each in the calling kind's error namespace.
//!
//! **An unsigned record is refused `signature_required`** (ruling (4): birth-
//! shape, no legacy arm) — [`IngestRefusal::Required`], the same policy every
//! client reader holds (`fauna_protocol::sync_row_verify` refuses an unsigned
//! row). The ruling's class exemptions never reach these callers: a record
//! request carries no `item_class` (state-entry rows arrive on their own kind),
//! retention rows are nest-minted beside a verified winner, and WebDAV
//! pseudo-device rows are recorded by the bridge kind, not here.

use fauna_core::encoding::EmbedAsBytes;
use fauna_protocol::RpcError;
use fauna_protocol::sync_writer_sig::{
    ChangeVerifyError, SET_NONCE_LEN, SignedChange, SignerCertCache, verify_statement,
};

use crate::db::CacheDb;

/// The signature fields a record request carried.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CarriedSignature<'a> {
    pub signature: Option<&'a [u8]>,
    pub signer_key: Option<&'a [u8]>,
}

impl<'a> CarriedSignature<'a> {
    pub(crate) fn new(
        signature: Option<&'a serde_bytes::ByteBuf>,
        signer_key: Option<&'a serde_bytes::ByteBuf>,
    ) -> Self {
        Self {
            signature: signature.map(|b| &b[..]),
            signer_key: signer_key.map(|b| &b[..]),
        }
    }

    /// Neither field present — the unsigned record.
    pub(crate) fn is_unsigned(&self) -> bool {
        self.signature.is_none() && self.signer_key.is_none()
    }
}

/// Where a delegated signer's `DeviceAuthorization` comes from.
#[derive(Debug, Clone, Copy)]
pub(crate) enum CertCarriage<'a> {
    /// Same-nest: by reference over the actor's registered grants. A cert the
    /// request carries inline is ignored here — honouring it would let a
    /// deleted device's old cert outlive the tombstone.
    ByReference,
    /// Across a trust boundary (the federated relay): the writer's cert rides
    /// the request, relayed by the writer's home nest, which resolved it by
    /// reference against ITS grants and tombstones.
    Inline(Option<&'a EmbedAsBytes>),
}

/// A signature that verified — what the row stores, plus the cert to remember
/// for the list replies' `signer_certs` side table.
#[derive(Debug, Clone)]
pub(crate) struct VerifiedSignature {
    pub signature: Vec<u8>,
    pub signer_key: Vec<u8>,
    /// `(actor, device key, canonical cert wire)` for a delegated signer;
    /// `None` for a direct one (the actor id is its own key). The actor is the
    /// statement's, which is the one the cert names on both arms of
    /// [`verify_carried`] — the chain verified to it by reference, an inline
    /// cert naming another refused `author_mismatch` — so it is also the
    /// stored row's `cert_actor_id`.
    pub cert: Option<([u8; 32], [u8; 32], Vec<u8>)>,
}

impl VerifiedSignature {
    pub(crate) fn as_row(&self) -> crate::db::RowSignature<'_> {
        crate::db::RowSignature {
            signature: &self.signature,
            signer_key: &self.signer_key,
        }
    }

    /// Remember the delegated cert (idempotent upsert; the latest verified
    /// cert for a key and the identity it names wins). Called once the row
    /// itself is accepted. Ingest stores a cert only under the recorder it
    /// names, so the row lands at `(actor, key, actor)`; a succession later
    /// moves its `actor_id` alone (`writer-signed-change-records.md` ruling
    /// (8)(i)).
    pub(crate) async fn remember_cert(&self, db: &CacheDb) -> anyhow::Result<()> {
        if let Some((actor, key, wire)) = &self.cert {
            db.upsert_sync_signer_cert(actor, key, actor, wire).await?;
        }
        Ok(())
    }
}

/// Why ingest refused a record. The caller namespaces it
/// ([`IngestRefusal::into_rpc`]).
#[derive(Debug)]
pub(crate) enum IngestRefusal {
    /// `signature_invalid` — the signature, the chain, or the set binding.
    Invalid(String),
    /// `author_mismatch` — the chain names another actor than the recorder.
    AuthorMismatch(String),
    /// `signature_required` — an unsigned record outside the class exemptions.
    Required,
    /// A storage error while resolving the signer.
    Internal(anyhow::Error),
}

impl IngestRefusal {
    pub(crate) fn into_rpc(self, ns: &str) -> RpcError {
        match self {
            Self::Invalid(detail) => crate::rpc_errors::coded_ns(ns, "signature_invalid", detail),
            Self::AuthorMismatch(detail) => {
                crate::rpc_errors::coded_ns(ns, "author_mismatch", detail)
            }
            Self::Required => crate::rpc_errors::coded_ns(
                ns,
                "signature_required",
                "an unsigned change record is not a record: sign it with a SyncWrite \
                 writer key under the set's nonce",
            ),
            Self::Internal(e) => crate::rpc_errors::internal_ns(ns, format!("{e:#}")),
        }
    }
}

fn invalid(e: ChangeVerifyError) -> IngestRefusal {
    IngestRefusal::Invalid(e.to_string())
}

/// The set's stored nonce, as the 32-byte binding a statement is built under.
/// A set with none (no client pushed it yet) cannot verify a signed record —
/// the owner's custody reconcile pushes it via `fauna.folders.update`.
pub(crate) fn stored_set_nonce(
    fs: &crate::db::FolderRow,
) -> Result<[u8; SET_NONCE_LEN], IngestRefusal> {
    fs.set_nonce
        .as_deref()
        .and_then(|n| n.try_into().ok())
        .ok_or_else(|| {
            IngestRefusal::Invalid(
                "the set has no stored set nonce to bind a signed record to — the \
                 owner pushes it with fauna.folders.update"
                    .into(),
            )
        })
}

/// Whether a STORED row's signature still verifies over the row's own
/// statement under `set_nonce` — the signature alone, by the row's stored
/// `signer_key`; the chain was verified when the row was recorded and is not
/// what this asks. A row signed under a nonce the owner's custody reconcile
/// has since retired answers `false` (it verifies nowhere as current,
/// `writer-signed-change-records.md` custody (e)), as does an unsigned or
/// malformed row. The record door's echo-delete guard asks it of the path's
/// head tombstone (`CacheDb::record_sync_change_in_conn`).
pub(crate) fn stored_row_signed_under(
    row: &crate::db::SyncChangeRow,
    set_nonce: &[u8; SET_NONCE_LEN],
) -> bool {
    fn b32(v: &[u8]) -> Option<[u8; 32]> {
        v.try_into().ok()
    }
    let (Some(signature), Some(signer_key)) = (&row.signature, &row.signer_key) else {
        return false;
    };
    let statement = (|| {
        Some(SignedChange {
            set_nonce: *set_nonce,
            actor_id: b32(&row.actor_id)?,
            device_id: b32(row.device_id.as_deref()?)?,
            path_hash: b32(&row.path_hash)?,
            manifest_hash: match row.manifest_hash.as_deref() {
                Some(h) => Some(b32(h)?),
                None => None,
            },
            change_type: row.change_type.clone(),
            size_bytes: row.size_bytes,
            content_key_version: match row.content_key_version {
                Some(v) => Some(u64::try_from(v).ok()?),
                None => None,
            },
            path_sealed: row.path_sealed.clone(),
            thumbnail_hash: match row.thumbnail_hash.as_deref() {
                Some(t) => Some(fauna_core::hex32::decode(t).ok()?),
                None => None,
            },
            derived_through: row.derived_through,
            is_resolution: row.is_resolution.unwrap_or(false),
            is_retention: row.is_retention.unwrap_or(false),
        })
    })();
    let (Some(statement), Some(key)) = (statement, b32(signer_key)) else {
        return false;
    };
    fauna_core::identity::verify_detached(&key, &statement.signed_message(), signature)
}

/// Verify a carried signature over `statement` (already built under the set's
/// stored nonce, naming the recorder as its actor). An unsigned record is
/// refused [`IngestRefusal::Required`].
pub(crate) async fn verify_carried(
    db: &CacheDb,
    statement: &SignedChange,
    carried: CarriedSignature<'_>,
    carriage: CertCarriage<'_>,
) -> Result<VerifiedSignature, IngestRefusal> {
    let (signature, signer_key) = match (carried.signature, carried.signer_key) {
        (None, None) => return Err(IngestRefusal::Required),
        (Some(sig), Some(key)) => (sig, key),
        _ => {
            return Err(IngestRefusal::Invalid(
                "signature and signer_key travel together".into(),
            ));
        }
    };
    let key: [u8; 32] = signer_key
        .try_into()
        .map_err(|_| IngestRefusal::Invalid("signer_key is not 32 bytes".into()))?;
    let actor = statement.actor_id;
    let now = fauna_core::data::Timestamp::now();

    let verified = |cert: Option<Vec<u8>>| VerifiedSignature {
        signature: signature.to_vec(),
        signer_key: signer_key.to_vec(),
        cert: cert.map(|wire| (actor, key, wire)),
    };

    // Direct: the identity key signed; no cert, no grant.
    if key == actor {
        verify_statement(
            statement,
            signature,
            signer_key,
            &SignerCertCache::new(),
            now,
        )
        .map_err(invalid)?;
        return Ok(verified(None));
    }

    match carriage {
        CertCarriage::Inline(cert) => {
            let cert = cert.ok_or_else(|| invalid(ChangeVerifyError::CertMissing))?;
            // The chain must end at the recorder: an inline cert that
            // certifies this key for ANOTHER actor is a record claiming an
            // author the relay did not authenticate.
            if let Ok(decoded) = fauna_core::encoding::decode_signed_bytes::<
                fauna_core::data::DeviceAuthorization,
            >(&cert.bytes)
                && decoded.actor_id.0 != actor
            {
                return Err(IngestRefusal::AuthorMismatch(
                    "the carried cert certifies the signer for another actor".into(),
                ));
            }
            let mut certs = SignerCertCache::new();
            certs.ingest(cert);
            verify_statement(statement, signature, signer_key, &certs, now).map_err(invalid)?;
            let wire = fauna_cbor::encode_canonical(cert)
                .map_err(|e| IngestRefusal::Internal(anyhow::anyhow!("re-encode cert: {e}")))?;
            Ok(verified(Some(wire)))
        }
        CertCarriage::ByReference => {
            let grants = db
                .live_sync_device_grants(&actor, &key)
                .await
                .map_err(IngestRefusal::Internal)?;
            if grants.is_empty() {
                return Err(IngestRefusal::Invalid(
                    "no live grant of the recorder carries this signer key \
                     (never registered, or revoked by a device deletion)"
                        .into(),
                ));
            }
            // Every row carrying the key is a candidate (a re-ceremony may
            // leave rows at different capability sets); the first that
            // verifies wins. The LAST error is the one reported.
            let mut last = ChangeVerifyError::CertMissing;
            for wire in grants {
                let Ok(cert) = fauna_cbor::decode_strict::<EmbedAsBytes>(&wire) else {
                    continue;
                };
                let mut certs = SignerCertCache::new();
                certs.ingest(&cert);
                match verify_statement(statement, signature, signer_key, &certs, now) {
                    Ok(_) => return Ok(verified(Some(wire))),
                    Err(e) => last = e,
                }
            }
            Err(invalid(last))
        }
    }
}

#[cfg(test)]
mod tests {
    //! The federated relay's INLINE arm (the same-nest by-reference arm is
    //! pinned end to end in `tests/conformance_writer_signed_changes.rs`).
    use super::*;
    use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
    use fauna_core::identity::ActorKeypair;

    fn statement(actor: [u8; 32]) -> SignedChange {
        SignedChange {
            set_nonce: [7; 32],
            actor_id: actor,
            device_id: [2; 32],
            path_hash: fauna_core::sync::path_hash("docs/a.txt"),
            manifest_hash: Some([3; 32]),
            change_type: "create".into(),
            size_bytes: 42,
            content_key_version: Some(1),
            path_sealed: Some(vec![9, 9]),
            thumbnail_hash: None,
            derived_through: Some(5),
            is_resolution: false,
            is_retention: false,
        }
    }

    fn cert(root: &ActorKeypair, device_key: [u8; 32]) -> EmbedAsBytes {
        let auth = DeviceAuthorization {
            actor_id: root.actor_id(),
            device_key,
            capabilities: vec![Capability::RenewBearer, Capability::SyncWrite],
            created_at: Timestamp(0),
            expires_at: None,
        };
        let (bytes, env) = fauna_core::encoding::sign_envelope(root, &auth).unwrap();
        EmbedAsBytes::from_signed(bytes, env)
    }

    #[tokio::test]
    async fn the_inline_arm_verifies_the_carried_cert_and_names_the_recorder() {
        let db = CacheDb::open_in_memory().unwrap();
        let writer_root = ActorKeypair::generate();
        let actor = writer_root.actor_id().0;
        let device = ActorKeypair::generate();
        let key = device.actor_id().0;
        let s = statement(actor);
        let sig = s.sign(device.signing_key());
        let carried = CarriedSignature {
            signature: Some(&sig),
            signer_key: Some(&key),
        };

        // The writer's own cert, inline: verified, and remembered.
        let good = cert(&writer_root, key);
        let verified = verify_carried(&db, &s, carried, CertCarriage::Inline(Some(&good)))
            .await
            .map_err(|e| format!("{e:?}"))
            .expect("the inline cert verifies");
        let (a, k, _) = verified
            .cert
            .expect("a delegated signer's cert is remembered");
        assert_eq!((a, k), (actor, key));

        // No cert on the relay: a delegated signer cannot verify.
        assert!(matches!(
            verify_carried(&db, &s, carried, CertCarriage::Inline(None)).await,
            Err(IngestRefusal::Invalid(_))
        ));

        // A cert certifying this key for ANOTHER actor: the chain would end at
        // an author the relay did not authenticate.
        let other = ActorKeypair::generate();
        let foreign = cert(&other, key);
        assert!(matches!(
            verify_carried(&db, &s, carried, CertCarriage::Inline(Some(&foreign))).await,
            Err(IngestRefusal::AuthorMismatch(_))
        ));

        // Unsigned is refused `signature_required`; half a pair is malformed.
        assert!(matches!(
            verify_carried(
                &db,
                &s,
                CarriedSignature::default(),
                CertCarriage::ByReference
            )
            .await,
            Err(IngestRefusal::Required)
        ));
        let half = CarriedSignature {
            signature: Some(&sig),
            signer_key: None,
        };
        assert!(matches!(
            verify_carried(&db, &s, half, CertCarriage::ByReference).await,
            Err(IngestRefusal::Invalid(_))
        ));
    }
}
