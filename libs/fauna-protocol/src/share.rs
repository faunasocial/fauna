//! Share-link control-plane WS-RPC payloads — `fauna.share.{create,list,revoke}`
//! (Track E1 of the WS-RPC-everywhere migration; tracked internally). A **net-new** feature, not a
//! transport migration: there is no HTTP twin. `GET /share/{token}` (public,
//! browser-facing) stays HTTP residue and gains a revocation check.
//!
//! A ShareToken is client-minted, stateless and self-verifying
//! (`fauna_core::share::ShareToken`, base64url'd into a `/share/{token}` URL);
//! the nest never holds a user's signing key. `share.create` therefore does NOT
//! mint — it **registers** a minted token's metadata so the author can list and
//! revoke their live shares. The nest decodes+verifies the supplied token,
//! derives the metadata from it (never trusting client-supplied fields), and
//! requires the token's `author` to equal the connection actor.
//!
//! Wire convention (matching `account.rs` / `folders.rs`): the 32-byte
//! `token_id` (blake3 of the signed wire bytes) and `manifest_hash` ride as
//! **hex-encoded `String`**; the dag-cbor wire forbids floats (none here — every
//! numeric field is an `i64`/`bool`). Caller class is `User | Admin`.
//!
//! Kind registry: `kind.rs::register_share_kinds`. Handlers:
//! `bins/fauna-nest/src/share_handlers.rs`. Goal doc:
//! `docs/goal/architecture/api-layers.md` § Share.

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

use crate::Value;

/// One registered share-link — shared by `ShareCreateReply` (the just-registered
/// token) and the items of `ShareListReply`. All fields are server-authoritative,
/// derived from the verified token plus the registration row.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ShareRecord {
    /// 64-char hex of the 32-byte registry token-id (`blake3` of the signed
    /// wire bytes; `fauna_core::share::token_id_from_base64url`).
    pub token_id: String,
    /// 64-char hex of the shared content's 32-byte manifest hash.
    pub manifest_hash: String,

    /// Token expiry, Unix seconds (the ShareToken's `expires`).
    pub expires_at: i64,
    /// Whether the link is public (no `FaunaIdentity` header required to fetch).
    pub public: bool,
    /// Whether the author has revoked the link (`GET /share/{token}` → 410).
    pub revoked: bool,
    /// Server-side registration time, Unix seconds.
    pub created_at: i64,
    /// A fragment-keyed private link (the token declares `key_in_fragment`
    /// and registered a key envelope). Its URL cannot be re-derived from this
    /// row — the key lives only in the URL the author copied — so a list shows
    /// it without a copy control. Additive 2026-09-27: absent is `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub key_in_fragment: bool,
    /// The shared file's name, sealed for its author — a
    /// `fauna_core::path_crypto` `SealedLabel` under the author's owner root,
    /// salted by the raw `token_id` bytes, random nonce
    /// (`LabelField::ShareFilename`; `share-links.md` § The filename rests
    /// sealed). The nest holds the name in no other form; the list renders it
    /// through `label_custody::render_share_filename`.
    pub filename_sealed: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.share.create ──────────────────────────────────────────────────────

/// Register a client-minted token's metadata. `token` is the base64url form
/// exactly as it appears in the `/share/{token}` URL; the nest decodes+verifies
/// it, derives the metadata, and requires its `author` to equal the connection
/// actor (`fauna.share.permission_denied` otherwise). Idempotent on the derived
/// token-id.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ShareCreateRequest {
    pub token: String,
    /// The sealed `fauna_core::share::KeyEnvelope` of a fragment-keyed private
    /// link — required when the token declares `key_in_fragment`, refused on
    /// any other token (`share-links.md` § The private-file extension). The
    /// nest stores and serves it opaque; it never holds the link key.
    /// Additive 2026-09-27: absent for every public link (a nest that never
    /// registers an envelope keeps answering a sealed manifest `403` — the
    /// safe answer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_envelope: Option<ByteBuf>,
    /// The file's name sealed for the author's own list (the shape
    /// [`ShareRecord::filename_sealed`] names). The nest stores it opaque and
    /// rests the name in no other form; a registration carrying an empty one
    /// is refused.
    pub filename_sealed: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShareCreateReply {
    pub share: ShareRecord,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.share.list ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShareListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The calling actor's registered share links, newest first.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShareListReply {
    pub shares: Vec<ShareRecord>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.share.revoke ──────────────────────────────────────────────────────

/// Flag a registered token (owned by the calling actor) revoked. `token_id` is
/// the hex registry id (as returned by `create`/`list`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShareRevokeRequest {
    pub token_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShareRevokeReply {
    /// Always `true` on success (the token was found, owned by the caller, and
    /// is now revoked).
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── GET /share/{token}/manifest (HTTP, unauthenticated) ─────────────────────

/// The body of a fragment-keyed link's manifest fetch — the viewer's first
/// read (`share-links.md` § The private-file extension, *What the nest
/// serves*). Both fields are bytes the nest never opens: the link's
/// `ChunkManifest` in its canonical encoding (the token's `manifest_hash`
/// names it, so the viewer checks it against the signed token) and the key
/// envelope the author registered. The chunks follow one per request, by
/// index, at `GET /share/{token}/chunk/{i}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ShareFragmentManifest {
    pub manifest: ByteBuf,
    pub key_envelope: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::test_support::assert_round_trips;

    fn sample_record(revoked: bool) -> ShareRecord {
        ShareRecord {
            token_id: "ab".repeat(32),
            manifest_hash: "cd".repeat(32),
            filename_sealed: ByteBuf::from(vec![4, 2]),
            expires_at: 1_700_000_000,
            public: true,
            revoked,
            created_at: 1_699_000_000,
            ..Default::default()
        }
    }

    #[test]
    fn create_round_trips() {
        assert_round_trips(&ShareCreateRequest {
            token: "QWxhZGRpbjpvcGVuIHNlc2FtZQ".into(), // gitleaks:allow
            ..Default::default()
        });
        assert_round_trips(&ShareCreateRequest {
            token: "QWxhZGRpbjpvcGVuIHNlc2FtZQ".into(), // gitleaks:allow
            key_envelope: Some(ByteBuf::from(vec![1, 2, 3])),
            ..Default::default()
        });
        assert_round_trips(&ShareCreateRequest {
            token: "QWxhZGRpbjpvcGVuIHNlc2FtZQ".into(), // gitleaks:allow
            filename_sealed: ByteBuf::from(vec![7, 7]),
            ..Default::default()
        });
        assert_round_trips(&ShareCreateReply {
            share: sample_record(false),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&ShareCreateReply {
            share: sample_record(true),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&ShareCreateReply {
            share: ShareRecord {
                key_in_fragment: true,
                ..sample_record(false)
            },
            extra: BTreeMap::new(),
        });
        assert_round_trips(&ShareFragmentManifest {
            manifest: ByteBuf::from(vec![0xA1]),
            key_envelope: ByteBuf::from(vec![1, 9]),
            extra: BTreeMap::new(),
        });
    }

    /// The private-link additions are invisible when unused: an absent envelope
    /// and a `false` declaration are never written, so a public link's payloads
    /// carry neither (its `extra` stays empty).
    #[test]
    fn public_link_payloads_carry_neither_addition() {
        let req = crate::encode_canonical(&ShareCreateRequest {
            token: "t".into(),
            ..Default::default()
        })
        .unwrap();
        let back: ShareCreateRequest = crate::decode_strict(&req).unwrap();
        assert!(back.extra.is_empty() && back.key_envelope.is_none());
        let rec = crate::encode_canonical(&sample_record(false)).unwrap();
        let as_map: BTreeMap<String, Value> = crate::decode_strict(&rec).unwrap();
        assert!(!as_map.contains_key("key_in_fragment"));
        assert!(!as_map.contains_key("key_envelope"));
    }

    /// The sealed name is a required field: a registration or a record without
    /// one does not decode.
    #[test]
    fn a_payload_without_the_sealed_name_is_refused() {
        let mut req: BTreeMap<String, Value> = BTreeMap::new();
        req.insert("token".into(), Value::String("t".into()));
        let bytes = crate::encode_canonical(&req).unwrap();
        assert!(crate::decode_strict::<ShareCreateRequest>(&bytes).is_err());

        let rec = crate::encode_canonical(&sample_record(false)).unwrap();
        let mut as_map: BTreeMap<String, Value> = crate::decode_strict(&rec).unwrap();
        as_map.remove("filename_sealed");
        let bytes = crate::encode_canonical(&as_map).unwrap();
        assert!(crate::decode_strict::<ShareRecord>(&bytes).is_err());
    }

    #[test]
    fn list_round_trips_empty_and_populated() {
        assert_round_trips(&ShareListRequest {
            extra: BTreeMap::new(),
        });
        assert_round_trips(&ShareListReply {
            shares: vec![],
            extra: BTreeMap::new(),
        });
        assert_round_trips(&ShareListReply {
            shares: vec![sample_record(false), sample_record(true)],
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn revoke_round_trips() {
        assert_round_trips(&ShareRevokeRequest {
            token_id: "ab".repeat(32),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&ShareRevokeReply {
            ok: true,
            extra: BTreeMap::new(),
        });
    }
}
