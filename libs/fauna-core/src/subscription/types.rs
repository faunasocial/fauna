use serde::{Deserialize, Serialize};

use crate::data::{ContentHash, Timestamp};
use crate::identity::ActorId;

/// Identifies an MLS group. Opaque variable-length identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MlsGroupId(#[serde(with = "serde_bytes")] pub Vec<u8>);

/// Gated content metadata attached to a Post.
///
/// When present, the Post's `body` is the public preview and the full content
/// is in an encrypted blob at `encrypted_ref`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GatedInfo {
    /// BLAKE3 hash of the encrypted blob containing the full PostBody.
    pub encrypted_ref: ContentHash,
    /// How subscribers obtain the decryption key.
    pub key_access: KeyAccess,
    /// Minimum tier name required to access this content.
    pub tier: String,
    /// Tier rank at post creation time, for stable resolution.
    pub tier_rank: u32,
    /// The creator-chosen random per-post **seal id** — the `post_id` input to
    /// `derive_post_key(base_key, seal_id)` for this post's body blob (and its
    /// attached media). It must be a value knowable *before* the post record
    /// exists: the record's own CID cannot be the derive input (the record
    /// contains `encrypted_ref`, whose ciphertext depends on the derived key —
    /// circular at creation), and a plaintext-body hash would be a
    /// content-confirmation oracle; so the creator mints 32 fresh random bytes
    /// and carries them here for every reader (subscriber clients, the
    /// web-paywall serve holder). Required (ratified 2026-07-12 with the
    /// web-paywall render path — `ui/feed.md` § Encryption at rest): a gated
    /// post without its seal id could never be opened by anyone.
    pub seal_id: ContentHash,
    /// The content addresses of the blobs sealed alongside this post's body
    /// under the same per-post key — its attached media items and their
    /// thumbnails — carried **in plaintext** so a nest can pin them.
    ///
    /// **Why this is on the floor (ratified 2026-09-08 —
    /// `encryption-at-rest.md` § Plaintext floor → Posts row; `ui/feed.md`
    /// § Encryption at rest).** A nest's blob GC deletes every blob no live
    /// record references, and a gated post's attachment hashes live only
    /// inside the sealed body the nest by construction cannot read — so
    /// without this list a nest swept a gated post's photos ~30 minutes
    /// after upload while the post kept rendering. What the list reveals,
    /// and to whom, is argued in that owning row (audience corrected
    /// 2026-09-09): it rides the signed envelope, so its audience is EVERY
    /// reader of the record, not the storing nest alone — the same accepted
    /// class as `encrypted_ref`'s address beside it (sealed bytes fetchable
    /// by whoever holds the address; confidentiality rests on the per-post
    /// key, never on the address being unknown), extended from a text-sized
    /// body to media-sized blobs. The bytes stay sealed. Same class as a
    /// file-sync manifest's plaintext chunk hashes (`media.md` § Encryption
    /// at rest already places the attachment-relationship floor on the
    /// referencing post).
    ///
    /// Filled by the gated-post builder from the sealed body's own media
    /// list (`fauna_client_core::post::build_gated_post_at`, via
    /// [`crate::data::PostBody::blob_refs`]). Additive: empty on a gated post
    /// with no attachments and skipped when empty so those records stay
    /// byte-identical; a reader that ignores the field leaves the gated
    /// attachments unpinned.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_refs: Vec<ContentHash>,
}

/// How a reader obtains the base key that opens gated content.
///
/// **Externally tagged on the wire, with hand-written serde.** The derive this
/// replaced encoded `{ "<Arm>": { ..fields } }`, and every shipped app decodes
/// exactly that inside `Post.gated`, so the known arms keep those bytes to the
/// bit (pinned by the `ShippedKeyAccess` tests below). What the hand-written
/// impls add is the rule-3 fallthrough ([`KeyAccess::Unknown`];
/// `transport.md` § Schema and forward-compat discipline), which a derive
/// cannot express for an externally tagged enum.
///
/// ⚠ **Never add an arm to reach shipped apps.** `Unknown` protects only apps
/// built after it existed; one built before it fails the whole `Post` on an
/// unknown tag. A new access mode reaching those apps grows an existing arm
/// additively instead — which is how the room arm got `generation`
/// (`ui/feed.md` § Encryption at rest → *Room-restricted — the ruling*,
/// ruling 2).
#[derive(Debug, Clone, PartialEq)]
pub enum KeyAccess {
    /// A **room-restricted** post: readers are the floor members of the room
    /// whose channel id is `group_id` (`ui/feed.md` § Encryption at rest →
    /// *Room-restricted*). The base key is the room class's own:
    ///
    /// - `generation: None` — an **end-to-end** room: the MLS epoch secret at
    ///   `epoch`, the arm as first designed;
    /// - `generation: Some(id)` — a **community** room: the room generation
    ///   key `id` names, domain-separated for posts
    ///   (`group_content_key(gen_key, ROOM_POST_CONTENT_KIND)`,
    ///   [`crate::group_content`]); `epoch` is `0` and unread.
    ///
    /// Either way the per-post key is `derive_post_key(base, seal_id)`.
    ///
    /// The wire tag is **`Mls`, frozen** — the name the arm shipped under; the
    /// Rust name says what it addresses now. The arm's never-authored
    /// subscriber-tier reading (an MLS group backing a tier) decodes here too
    /// and is left as it was: the room reading governs new authoring only.
    Room {
        group_id: MlsGroupId,
        epoch: u64,
        /// A community room's generation id. Additive (2026-09-10): absent on
        /// the wire when `None`, so an end-to-end arm is byte-identical to the
        /// pre-field one, and an older app decodes a community arm as the
        /// gated card it already could not open.
        generation: Option<[u8; 32]>,
    },
    /// Key found in a broadcast key blob.
    Broadcast { key_blob_ref: ContentHash },
    /// An arm this build does not know — the rule-3 fallthrough, preserving
    /// the tag and its payload so a re-encode reproduces the bytes a
    /// signature covers. Nothing can open it; a reader renders the locked
    /// card, and a nest stores it like any other gated post.
    Unknown {
        kind: String,
        payload: fauna_cbor::Value,
    },
}

/// The frozen wire tag of [`KeyAccess::Room`].
const KEY_ACCESS_ROOM_TAG: &str = "Mls";
/// The wire tag of [`KeyAccess::Broadcast`].
const KEY_ACCESS_BROADCAST_TAG: &str = "Broadcast";

/// [`KeyAccess::Room`]'s fields as the wire carries them. Unknown fields are
/// tolerated (serde's default), which is what lets a future additive field on
/// this arm reach this build the way `generation` reaches the shipped apps.
#[derive(Deserialize)]
struct RoomArmWire {
    group_id: MlsGroupId,
    epoch: u64,
    #[serde(default)]
    #[serde(with = "serde_bytes")]
    generation: Option<[u8; 32]>,
}

/// [`KeyAccess::Broadcast`]'s fields as the wire carries them.
#[derive(Deserialize)]
struct BroadcastArmWire {
    key_blob_ref: ContentHash,
}

impl Serialize for KeyAccess {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeStructVariant};
        // The known arms go through `serialize_struct_variant`, exactly the
        // call the derive made, so their encoding cannot drift from it.
        match self {
            KeyAccess::Room {
                group_id,
                epoch,
                generation,
            } => {
                let len = 2 + usize::from(generation.is_some());
                let mut arm =
                    s.serialize_struct_variant("KeyAccess", 0, KEY_ACCESS_ROOM_TAG, len)?;
                arm.serialize_field("group_id", group_id)?;
                arm.serialize_field("epoch", epoch)?;
                match generation {
                    Some(g) => arm.serialize_field("generation", serde_bytes::Bytes::new(g))?,
                    None => arm.skip_field("generation")?,
                }
                arm.end()
            }
            KeyAccess::Broadcast { key_blob_ref } => {
                let mut arm =
                    s.serialize_struct_variant("KeyAccess", 1, KEY_ACCESS_BROADCAST_TAG, 1)?;
                arm.serialize_field("key_blob_ref", key_blob_ref)?;
                arm.end()
            }
            KeyAccess::Unknown { kind, payload } => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry(kind, payload)?;
                m.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for KeyAccess {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct ArmVisitor;

        impl<'de> serde::de::Visitor<'de> for ArmVisitor {
            type Value = KeyAccess;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an externally tagged KeyAccess — a map with exactly one arm")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<KeyAccess, A::Error> {
                use serde::de::Error;
                let kind: String = map
                    .next_key()?
                    .ok_or_else(|| A::Error::custom("KeyAccess carries no arm"))?;
                let access = match kind.as_str() {
                    KEY_ACCESS_ROOM_TAG => {
                        let arm: RoomArmWire = map.next_value()?;
                        KeyAccess::Room {
                            group_id: arm.group_id,
                            epoch: arm.epoch,
                            generation: arm.generation,
                        }
                    }
                    KEY_ACCESS_BROADCAST_TAG => {
                        let arm: BroadcastArmWire = map.next_value()?;
                        KeyAccess::Broadcast {
                            key_blob_ref: arm.key_blob_ref,
                        }
                    }
                    _ => KeyAccess::Unknown {
                        payload: map.next_value()?,
                        kind,
                    },
                };
                // Exactly one arm. A second key would make the choice a guess,
                // and the fallthrough must never turn a malformed record into a
                // silently chosen arm.
                if map.next_key::<serde::de::IgnoredAny>()?.is_some() {
                    return Err(A::Error::custom("KeyAccess carries more than one arm"));
                }
                Ok(access)
            }
        }

        d.deserialize_map(ArmVisitor)
    }
}

/// Broadcast key blob: period key encrypted to each subscriber's public key.
///
/// Signed via `fauna_cbor::SignedEnvelope` (sign-over-CID); the envelope
/// ships alongside the canonical bytes in the embed-as-bytes wire shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyBlob {
    pub author: ActorId,
    pub tier: String,
    pub rotated_at: Timestamp,
    /// Map of subscriber ActorId -> encrypted period key.
    pub entries: Vec<KeyBlobEntry>,
    /// Public key of the device that signed this blob (nest's key).
    #[serde(default)]
    #[serde(with = "serde_bytes")]
    pub signer: [u8; 32],
    /// **The key witness** — `BLAKE3::derive_key("fauna.keyblob.commit.v1",
    /// wrapped_key)` over the period key (or archival epoch secret) the entries
    /// wrap, computed by [`super::crypto::period_key_commitment`] and set by
    /// every minter that holds the key.
    ///
    /// It exists because nothing else in a stored blob can answer *"which key
    /// does this wrap?"* to the author: every entry is wrapped to a subscriber
    /// under a discarded ephemeral key, the nest admits no entry beyond the
    /// roster, and `rotated_at` witnesses freshness only — an ordinary approve
    /// advances it without minting, so an approve that read custody before a
    /// rotation but stamped a later `rotated_at` lands the pre-rotation key
    /// over the rotated blob and reads as settled. The post-succession rotation leg compares this against its custody
    /// instead.
    ///
    /// Required: a blob without its witness is refused at decode, so no reader
    /// ever falls back to the weaker freshness rule. A public function of the
    /// key, deliberately: the blob is served only to the author and current
    /// subscribers, every one of whom can unwrap the key it commits to.
    #[serde(with = "serde_bytes")]
    pub key_commitment: [u8; 32],
}

/// Self-describing KEM-suite identifier for a [`KeyBlobEntry`] — the
/// crypto-agility discriminator (goal `architecture/security/post-quantum.md`
/// § 7.1, surface B).
///
/// [`Self::Classical`] is the default **and is omitted on the wire** (via the
/// `skip_serializing_if` on `KeyBlobEntry::suite`): an entry wrapped the classical
/// way is byte-identical to a pre-agility entry, so the conformance pin holds and
/// the parent `KeyBlob`'s sign-over-CID signature is unchanged. A hybrid entry
/// (slice S4) carries [`Self::Xwing`] explicitly inside the signed bytes; rosters
/// may mix suites per-entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum KemSuiteId {
    /// Ephemeral-X25519 ECDH + BLAKE3 `derive_key` + ChaCha20-Poly1305 — today's
    /// wrap (`subscription/crypto.rs`). The default; omitted on the wire.
    #[default]
    Classical,
    /// X-Wing hybrid (ML-KEM-768 ∥ X25519). The subscriber publishes an ML-KEM
    /// encapsulation key and the author wraps to it. Implemented in slice S4.
    Xwing,
    /// A suite a newer build wraps with and this one does not name
    /// (`transport.md` § Schema and forward-compat discipline → *Rule 3 in
    /// full*; `post-quantum.md` § 7.1): that one entry is unopenable here,
    /// never the blob. Never written — a path that would serialize it fails.
    #[serde(other)]
    #[serde(skip_serializing)]
    Unknown,
}

impl KemSuiteId {
    /// Whether this is the [`Self::Classical`] default — the `skip_serializing_if`
    /// predicate that keeps a classical entry byte-identical to a pre-agility one.
    pub fn is_classical(&self) -> bool {
        matches!(self, Self::Classical)
    }
}

/// A single entry in a broadcast key blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyBlobEntry {
    pub subscriber: ActorId,
    /// The period key encrypted to this subscriber's X25519 public key.
    /// Format: 32-byte ephemeral public key + 12-byte nonce + ciphertext + 16-byte tag.
    #[serde(with = "serde_bytes")]
    pub encrypted_key: Vec<u8>,
    /// KEM suite that produced `encrypted_key` (crypto-agility, goal § 7.1).
    /// `#[serde(default)]` + `skip_serializing_if` ⇒ a classical entry omits the
    /// field on the wire, so it decodes byte-identically to a pre-agility entry
    /// (old signers simply never wrote it) and the parent blob's sign-over-CID
    /// signature is unaffected. X-Wing entries (slice S4) carry it explicitly.
    #[serde(default, skip_serializing_if = "KemSuiteId::is_classical")]
    pub suite: KemSuiteId,
}

/// A subscription tier definition, published in the author's profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionTier {
    pub author: ActorId,
    /// Immutable tier name.
    pub name: String,
    pub description: Option<String>,
    /// Hierarchical rank: `1` = lowest *paid* tier, higher = more access.
    /// Rank `0` ([`super::FOLLOWERS_TIER_RANK`]) is reserved for the free
    /// [`super::FOLLOWERS_TIER`] tier, below every paid tier.
    pub rank: u32,
    /// Display-only price hint (e.g., "$5/month"). Not enforced.
    pub price_hint: Option<String>,
    /// Link to external payment page. Not enforced.
    pub payment_url: Option<String>,
    pub created_at: Timestamp,
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

/// A subscriber's request to access a creator's gated content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeRequest {
    pub subscriber: ActorId,
    pub author: ActorId,
    pub tier: String,
    pub created_at: Timestamp,
    /// Signed by the subscriber's key.
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Crypto-agility additive contract (goal § 7.1, surface B): a pre-agility
    /// `KeyBlobEntry` (no `suite` field) must decode to `Classical`, and a
    /// classical entry must re-encode **byte-identically** to the pre-agility
    /// shape — otherwise the conformance pin and the parent blob's sign-over-CID
    /// signature would change. Mirrors the `OldNestInfoReply` skew test in
    /// `fauna_protocol::discovery`.
    #[test]
    fn key_blob_entry_classical_suite_is_wire_invisible() {
        /// The `KeyBlobEntry` shape as a pre-agility signer serialized it — no
        /// `suite` field.
        #[derive(Serialize)]
        struct OldKeyBlobEntry {
            subscriber: ActorId,
            #[serde(with = "serde_bytes")]
            encrypted_key: Vec<u8>,
        }
        let subscriber = ActorId([7u8; 32]);
        let payload = vec![1u8, 2, 3, 4, 5];

        let old = OldKeyBlobEntry {
            subscriber,
            encrypted_key: payload.clone(),
        };
        let old_bytes = fauna_cbor::encode_canonical(&old).unwrap();

        // A classical entry omits `suite` ⇒ byte-identical to the old shape.
        let classical = KeyBlobEntry {
            subscriber,
            encrypted_key: payload.clone(),
            suite: KemSuiteId::Classical,
        };
        let classical_bytes = fauna_cbor::encode_canonical(&classical).unwrap();
        assert_eq!(
            old_bytes, classical_bytes,
            "a classical KeyBlobEntry must not add `suite` to the wire"
        );

        // Decoding the field-less old bytes yields the Classical default.
        let decoded: KeyBlobEntry = fauna_cbor::decode_strict(&old_bytes).unwrap();
        assert_eq!(decoded.suite, KemSuiteId::Classical);
        assert_eq!(decoded.encrypted_key, payload);

        // An X-Wing entry DOES carry `suite` (inside the signed bytes), so it is
        // distinguishable and round-trips.
        let hybrid = KeyBlobEntry {
            subscriber,
            encrypted_key: payload.clone(),
            suite: KemSuiteId::Xwing,
        };
        let hybrid_bytes = fauna_cbor::encode_canonical(&hybrid).unwrap();
        assert_ne!(hybrid_bytes, old_bytes);
        let decoded: KeyBlobEntry = fauna_cbor::decode_strict(&hybrid_bytes).unwrap();
        assert_eq!(decoded.suite, KemSuiteId::Xwing);
    }

    #[test]
    fn kem_suite_id_default_is_classical() {
        assert_eq!(KemSuiteId::default(), KemSuiteId::Classical);
        assert!(KemSuiteId::Classical.is_classical());
        assert!(!KemSuiteId::Xwing.is_classical());
    }

    /// `KeyAccess` exactly as every shipped app decodes it — the derived,
    /// externally tagged two-arm enum from before the room arm grew
    /// `generation` and before the rule-3 fallthrough existed. The envelope
    /// reaches every follower's app whatever its version, so this shape is
    /// the one the new bytes must keep satisfying (`ui/feed.md` § Encryption
    /// at rest → *Room-restricted — the ruling*, ruling 2).
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    enum ShippedKeyAccess {
        Mls { group_id: MlsGroupId, epoch: u64 },
        Broadcast { key_blob_ref: ContentHash },
    }

    fn community_arm() -> KeyAccess {
        KeyAccess::Room {
            group_id: MlsGroupId(vec![0xC7; 32]),
            epoch: 0,
            generation: Some([0x9A; 32]),
        }
    }

    #[test]
    fn a_community_room_arm_decodes_through_the_shipped_shape() {
        // The whole reason the arm grew a field instead of a variant: an older
        // app meets a room post in its feed and must decode it — as the
        // gated card it already cannot open — rather than fail the whole Post.
        let bytes = fauna_cbor::encode_canonical(&community_arm()).unwrap();
        let old: ShippedKeyAccess = fauna_cbor::decode_strict(&bytes)
            .expect("a shipped app must decode the room arm it cannot open");
        assert_eq!(
            old,
            ShippedKeyAccess::Mls {
                group_id: MlsGroupId(vec![0xC7; 32]),
                epoch: 0,
            }
        );
    }

    #[test]
    fn an_arm_without_a_generation_is_byte_identical_to_the_shipped_mls_arm() {
        // End-to-end rooms carry no generation: the field must then be
        // invisible, so every such record keeps its
        // bytes — and every signature over them.
        let group_id = MlsGroupId(vec![1, 2, 3]);
        let old_bytes = fauna_cbor::encode_canonical(&ShippedKeyAccess::Mls {
            group_id: group_id.clone(),
            epoch: 7,
        })
        .unwrap();
        let new = KeyAccess::Room {
            group_id: group_id.clone(),
            epoch: 7,
            generation: None,
        };
        assert_eq!(fauna_cbor::encode_canonical(&new).unwrap(), old_bytes);
        let decoded: KeyAccess = fauna_cbor::decode_strict(&old_bytes).unwrap();
        assert_eq!(decoded, new);
    }

    #[test]
    fn a_broadcast_arm_is_byte_identical_to_the_shipped_one() {
        let key_blob_ref = ContentHash::from_digest_raw([5u8; 32]);
        let old_bytes =
            fauna_cbor::encode_canonical(&ShippedKeyAccess::Broadcast { key_blob_ref }).unwrap();
        let new = KeyAccess::Broadcast { key_blob_ref };
        assert_eq!(fauna_cbor::encode_canonical(&new).unwrap(), old_bytes);
        let decoded: KeyAccess = fauna_cbor::decode_strict(&old_bytes).unwrap();
        assert_eq!(decoded, new);
    }

    #[test]
    fn the_community_arm_round_trips() {
        let bytes = fauna_cbor::encode_canonical(&community_arm()).unwrap();
        let back: KeyAccess = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(back, community_arm());
    }

    #[test]
    fn an_arm_this_build_does_not_know_falls_through_and_re_encodes_byte_identically() {
        // Rule 3 (`transport.md` § Schema and forward-compat discipline): a
        // future arm must not fail the Post it rides in, and anything that
        // re-encodes what it decoded must not change the bytes a signature
        // covers.
        let future = fauna_cbor::Value::Map(std::collections::BTreeMap::from([(
            "Sealed".to_string(),
            fauna_cbor::Value::Map(std::collections::BTreeMap::from([
                ("scheme".to_string(), fauna_cbor::Value::Integer(3)),
                ("salt".to_string(), fauna_cbor::Value::Bytes(vec![9; 4])),
            ])),
        )]));
        let bytes = fauna_cbor::encode_canonical(&future).unwrap();
        let decoded: KeyAccess = fauna_cbor::decode_strict(&bytes).unwrap();
        let KeyAccess::Unknown { kind, .. } = &decoded else {
            panic!("an unknown arm must fall through, got {decoded:?}");
        };
        assert_eq!(kind, "Sealed");
        assert_eq!(fauna_cbor::encode_canonical(&decoded).unwrap(), bytes);
    }

    #[test]
    fn a_tagged_union_with_two_tags_is_refused() {
        // Externally tagged means exactly one key: two would make "which arm"
        // a guess, and the fallthrough must not turn a malformed record into a
        // silently chosen one.
        let two = fauna_cbor::Value::Map(std::collections::BTreeMap::from([
            (
                "Broadcast".to_string(),
                fauna_cbor::Value::Map(Default::default()),
            ),
            (
                "Mls".to_string(),
                fauna_cbor::Value::Map(Default::default()),
            ),
        ]));
        let bytes = fauna_cbor::encode_canonical(&two).unwrap();
        assert!(fauna_cbor::decode_strict::<KeyAccess>(&bytes).is_err());
    }

    #[test]
    fn the_room_arm_round_trips_through_json_too() {
        // Not only dag-cbor: the FFI/wasm edges and fixtures move a Post
        // through serde_json, so the hand-written impls must stay format-blind.
        let json = serde_json::to_string(&community_arm()).unwrap();
        assert!(json.starts_with("{\"Mls\":"), "{json}");
        let back: KeyAccess = serde_json::from_str(&json).unwrap();
        assert_eq!(back, community_arm());
    }
}
