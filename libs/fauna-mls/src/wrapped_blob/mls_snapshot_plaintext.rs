//! Plaintext shape of an `MlsSnapshotBlob` after MSEK AEAD-unwrap.
//!
//! Per the wrapped-blob crypto design (tracked internally),
//! § Snapshot plaintext contents, the actor's read-side MLS state
//! includes "HPKE init private keys (current + last 2 rotations for
//! grace decrypt of in-flight mail)". The MDA bridge needs those
//! init secrets to HPKE-Open the `MailRecordEnvelope`s the MTA sealed
//! to the actor's leaf init pubkey (IMAP body fetch + CalDAV metadata
//! unseal both depend on it).
//!
//! This struct is the wire shape the user's primary client emits via
//! `provision_mls_snapshot_blob` and the MDA decodes after AEAD-
//! unwrap. The leaf init keypairs are listed explicitly so the MDA
//! doesn't have to reconstruct OpenMLS provider storage to find them.
//! Future fields (full provider storage bytes, epoch secrets, blob
//! epoch keys) can grow this struct additively — the wire format
//! versions via the `v` field and new fields are additive: canonical
//! dag-cbor (`fauna_cbor`) keeps map keys length-first sorted, and serde
//! ignores unknown fields, so older decoders still parse newer blobs.

use crate::wrapped_blob::format::{
    GrantWindow, MailRecordEnvelope, RecordKind, UnwrapError, WrapError,
};
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// Current snapshot-plaintext format version.
pub const SNAPSHOT_PLAINTEXT_VERSION: u8 = 1;

/// One leaf-node HPKE init keypair. `x25519_pubkey` is the
/// recipient-binding pubkey the MTA seals to (fetched from nest via
/// `fauna.bridges.fetch_recipient_mls_pubkey`); `x25519_secret` is the
/// matching 32-byte X25519 scalar the MDA uses to HPKE-Open.
///
/// Both fields are exactly 32 bytes. The struct does not zeroize on
/// drop — the surrounding `MlsSnapshotPlaintext` is decoded from a
/// short-lived buffer the MDA discards after the open completes; the
/// open path's `unseal_mail_record` already zeroes its internal HPKE
/// state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeafInitKeypair {
    #[serde(rename = "pk")]
    pub x25519_pubkey: ByteBuf,
    #[serde(rename = "sk")]
    pub x25519_secret: ByteBuf,
    /// The ML-KEM-768 **decapsulation** key (2400 bytes) of the hybrid X-Wing
    /// recipient-mail keypair derived from the *same* MSEK as the X25519 half
    /// ([`derive_recipient_xwing_keypair`]). The MDA threads it into
    /// [`super::unseal_mail_record_hybrid`] alongside `x25519_secret` so it can
    /// open both classical and post-quantum (X-Wing) mail records sealed to this
    /// keypair's published encapsulation key (post-quantum slice S3d,
    /// `architecture/security/post-quantum.md` § "Post-quantum key publication
    /// and derivation").
    ///
    /// **Additive + omitted when `None`** (`skip_serializing_if`): a pre-hybrid
    /// snapshot has no `mdk` key and stays byte-identical to its pre-S3d form,
    /// and an older decoder ignores the unknown key (the struct carries no
    /// `deny_unknown_fields`). Absent ⇒ the MDA opens only classical records for
    /// this keypair — correct, since hybrid mail is only sealed once the
    /// recipient has *published* the matching ek (S3d step 2 / S3e), which the
    /// same client transaction that (re)writes this snapshot performs.
    #[serde(rename = "mdk", default, skip_serializing_if = "Option::is_none")]
    pub mlkem_dk: Option<ByteBuf>,
}

impl LeafInitKeypair {
    /// Build a **classical** entry (no ML-KEM half) from raw 32-byte arrays.
    /// The byte order is whatever `generate_x25519_keypair` (and OpenMLS's HPKE
    /// init key generation) produces — the wire shape is opaque bytes.
    #[must_use]
    pub fn new(x25519_pubkey: [u8; 32], x25519_secret: [u8; 32]) -> Self {
        Self {
            x25519_pubkey: ByteBuf::from(x25519_pubkey.to_vec()),
            x25519_secret: ByteBuf::from(x25519_secret.to_vec()),
            mlkem_dk: None,
        }
    }

    /// Build a **hybrid** entry carrying the 2400-byte ML-KEM decapsulation-key
    /// half, so the MDA can open X-Wing mail records sealed to this keypair.
    #[must_use]
    pub fn new_hybrid(
        x25519_pubkey: [u8; 32],
        x25519_secret: [u8; 32],
        mlkem_dk: &[u8; fauna_pq_kem::MLKEM768_DECAPS_KEY_LEN],
    ) -> Self {
        Self {
            x25519_pubkey: ByteBuf::from(x25519_pubkey.to_vec()),
            x25519_secret: ByteBuf::from(x25519_secret.to_vec()),
            mlkem_dk: Some(ByteBuf::from(mlkem_dk.to_vec())),
        }
    }
}

/// AEAD-unwrapped plaintext of an `MlsSnapshotBlob`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlsSnapshotPlaintext {
    #[serde(rename = "v")]
    pub v: u8,
    #[serde(rename = "leaf_init_keypairs")]
    pub leaf_init_keypairs: Vec<LeafInitKeypair>,
    /// One 32-byte mail-epoch root ([`derive_mail_epoch_root`]) per **prior**
    /// grace MSEK generation, newest first — aligned with
    /// `leaf_init_keypairs[1..]` (the current generation's root re-derives
    /// from the session MSEK on demand and is never carried). This is the
    /// MSEK-rotation grace material for epoch-sealed mail (design § 5): a
    /// record epoch-sealed under a rotated-away root opens by repeating the
    /// § 4 epoch chain per root here. A derived root, never the prior MSEK —
    /// complete for that generation's mail-epoch reads, powerless beyond
    /// them ([`MAIL_EPOCH_ROOT_DERIVE_CONTEXT`]).
    ///
    /// **Additive + omitted when empty** (`skip_serializing_if`): a
    /// pre-epochs snapshot stays byte-identical, and an older decoder
    /// ignores the unknown key (same shape as `LeafInitKeypair::mlkem_dk`).
    #[serde(
        rename = "mail_epoch_grace_roots",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub mail_epoch_grace_roots: Vec<ByteBuf>,
    /// One 32-byte **mail/calendar index-segment key**
    /// ([`derive_index_segment_key`]) per **prior** grace MSEK generation,
    /// newest first — aligned with `leaf_init_keypairs[1..]` exactly like
    /// [`Self::mail_epoch_grace_roots`]. The current generation's key is
    /// **never carried**: it re-derives from the session MSEK on demand, so a
    /// snapshot leak never hands over the key the live segments are sealed
    /// under. This is the MSEK-rotation grace material for the content
    /// index's mail/calendar slices (`key-material-hierarchy.md`
    /// § Path B-sibling-4 → *Snapshot carriage*): a segment still wrapped
    /// under a rotated-away key stays openable from this list until the
    /// rotating client's rewrap pass converges.
    ///
    /// A derived key, never the prior MSEK — complete for that generation's
    /// mail/calendar *index* reads, powerless beyond them (it opens index
    /// tokens, not mail bodies; those need [`derive_mail_epoch_root`]).
    ///
    /// **Additive + omitted when empty** (`skip_serializing_if`): a pre-S5
    /// snapshot stays byte-identical and an older decoder ignores the
    /// unknown key (the [`LeafInitKeypair::mlkem_dk`] shape).
    #[serde(
        rename = "index_seg_grace_keys",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub index_seg_grace_keys: Vec<ByteBuf>,
    /// The unix-seconds instant each **prior** MSEK generation was retired,
    /// newest first — aligned with `leaf_init_keypairs[1..]` exactly like
    /// [`Self::mail_epoch_grace_roots`] (the current generation has none: it
    /// is not retired). What lets the MDA's opener trial the generation
    /// current at a record's seal basis first, then outward, instead of
    /// walking the whole ring per record ([`generation_trial_order`];
    /// `owner-key-material.md` § Path B-sibling-2 → *Pre-rotation mail at
    /// rest*). Public timing metadata, no key material.
    ///
    /// **Additive + omitted when empty** (`skip_serializing_if`): a
    /// no-priors snapshot stays byte-identical and an older decoder ignores
    /// the unknown key (the [`LeafInitKeypair::mlkem_dk`] shape); a reader
    /// finding it empty or short walks the whole ring.
    #[serde(
        rename = "generation_retired_at_unix",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub generation_retired_at_unix: Vec<u64>,
    /// One 32-byte **calendar root** ([`derive_calendar_root`]) per **prior**
    /// MSEK generation, newest first — aligned with `leaf_init_keypairs[1..]`
    /// exactly like [`Self::mail_epoch_grace_roots`], whose shape this is
    /// verbatim. The current generation's root re-derives from the session MSEK
    /// and is never carried. Rotation grace for every calendar body at rest
    /// (`owner-key-material.md` § Path B-sibling-5 → *Snapshot carriage*): the
    /// MDA's calendar ring is `[derive(session MSEK)] ++ these`
    /// ([`StandingDavRing::from_msek_and_grace_roots`]). **Additive + omitted
    /// when empty.**
    #[serde(
        rename = "calendar_grace_roots",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub calendar_grace_roots: Vec<ByteBuf>,
    /// [`Self::calendar_grace_roots`]' twin for the address book: one 32-byte
    /// **card root** ([`derive_card_root`]) per prior generation
    /// (§ Path B-sibling-6). **Additive + omitted when empty.**
    #[serde(
        rename = "card_grace_roots",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub card_grace_roots: Vec<ByteBuf>,
}

impl MlsSnapshotPlaintext {
    /// The carried grace roots of DAV `kind`, borrowed as 32-byte arrays (the
    /// decoder enforced the length) — what
    /// [`StandingDavRing::from_msek_and_grace_roots`] takes.
    #[must_use]
    pub fn dav_grace_roots(&self, kind: DavKind) -> Vec<&[u8; 32]> {
        let roots = match kind {
            DavKind::Calendar => &self.calendar_grace_roots,
            DavKind::Card => &self.card_grace_roots,
        };
        roots
            .iter()
            .filter_map(|r| r.as_slice().try_into().ok())
            .collect()
    }
}

impl Default for MlsSnapshotPlaintext {
    /// An empty snapshot at the current version — the fixture shape. Production
    /// snapshots are built by [`build_mls_snapshot_plaintext`] from the actor's
    /// MSEK history; this exists so fixtures can spread `..Default::default()`
    /// and stop colliding every time this wire type grows a field.
    fn default() -> Self {
        Self {
            v: SNAPSHOT_PLAINTEXT_VERSION,
            leaf_init_keypairs: Vec::new(),
            mail_epoch_grace_roots: Vec::new(),
            index_seg_grace_keys: Vec::new(),
            generation_retired_at_unix: Vec::new(),
            calendar_grace_roots: Vec::new(),
            card_grace_roots: Vec::new(),
        }
    }
}

impl MlsSnapshotPlaintext {
    /// Encode to canonical DAG-CBOR bytes (the same codec used for
    /// every other wrapped-blob wire shape; see
    /// `format::WrappedMsekBlob::to_canonical_bytes`).
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on encoding failure
    /// (practically unreachable for the fixed wire shape).
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode from canonical CBOR.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` for bad CBOR, unsupported
    /// version, or wrong keypair-field lengths.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        let snap: Self = fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("snapshot-plaintext cbor: {e}")))?;
        if snap.v != SNAPSHOT_PLAINTEXT_VERSION {
            return Err(UnwrapError::InvalidFormat(format!(
                "unsupported snapshot-plaintext version: {}",
                snap.v
            )));
        }
        for (i, kp) in snap.leaf_init_keypairs.iter().enumerate() {
            if kp.x25519_pubkey.len() != 32 {
                return Err(UnwrapError::InvalidFormat(format!(
                    "leaf_init_keypairs[{i}].pk must be 32 bytes, got {}",
                    kp.x25519_pubkey.len()
                )));
            }
            if kp.x25519_secret.len() != 32 {
                return Err(UnwrapError::InvalidFormat(format!(
                    "leaf_init_keypairs[{i}].sk must be 32 bytes, got {}",
                    kp.x25519_secret.len()
                )));
            }
            if let Some(mdk) = &kp.mlkem_dk
                && mdk.len() != fauna_pq_kem::MLKEM768_DECAPS_KEY_LEN
            {
                return Err(UnwrapError::InvalidFormat(format!(
                    "leaf_init_keypairs[{i}].mdk must be {} bytes, got {}",
                    fauna_pq_kem::MLKEM768_DECAPS_KEY_LEN,
                    mdk.len()
                )));
            }
        }
        for (i, root) in snap.mail_epoch_grace_roots.iter().enumerate() {
            if root.len() != 32 {
                return Err(UnwrapError::InvalidFormat(format!(
                    "mail_epoch_grace_roots[{i}] must be 32 bytes, got {}",
                    root.len()
                )));
            }
        }
        for (field, list) in [
            ("index_seg_grace_keys", &snap.index_seg_grace_keys),
            ("calendar_grace_roots", &snap.calendar_grace_roots),
            ("card_grace_roots", &snap.card_grace_roots),
        ] {
            for (i, key) in list.iter().enumerate() {
                if key.len() != 32 {
                    return Err(UnwrapError::InvalidFormat(format!(
                        "{field}[{i}] must be 32 bytes, got {}",
                        key.len()
                    )));
                }
            }
        }
        let priors = snap.leaf_init_keypairs.len().saturating_sub(1);
        if snap.generation_retired_at_unix.len() > priors {
            return Err(UnwrapError::InvalidFormat(format!(
                "generation_retired_at_unix carries {} instants for {priors} prior generations",
                snap.generation_retired_at_unix.len()
            )));
        }
        Ok(snap)
    }
}

/// Domain-separation context for deriving the actor's standing
/// recipient-mail HPKE keypair from MSEK. Versioned per
/// `docs/goal/architecture/key-material-hierarchy.md` rule #3 (every
/// derivation off a shared root uses a versioned, domain-separated
/// context string).
pub const RECIPIENT_HPKE_DERIVE_CONTEXT: &str = "fauna.mail.recipient-hpke.v1 2026-05-23";

/// Derive the actor's standing recipient-mail HPKE keypair from MSEK.
///
/// `BLAKE3::derive_key` (domain-separated) → IKM → RFC 9180
/// `DeriveKeyPair` for the `X25519HkdfSha256` KEM. Deterministic in
/// `msek`, so every device holding the same `fauna.state.mail` MSEK
/// derives the identical keypair — the registered recipient pubkey is
/// fleet-consistent. Returns `(secret, public)`; the public half is
/// what `provision_recipient_mls_pubkey` registers and the MTA seals
/// inbound mail to, the secret half is what the MDA / client opens with.
/// See `key-material-hierarchy.md` § Path B-sibling-2.
#[must_use]
pub fn derive_recipient_hpke_keypair(msek: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let ikm = blake3::derive_key(RECIPIENT_HPKE_DERIVE_CONTEXT, msek);
    crate::wrapped_blob::envelope::derive_x25519_keypair_from_ikm(&ikm)
}

/// Domain-separation context for deriving the **ML-KEM-768 half** of the actor's
/// standing recipient-mail **X-Wing** keypair from MSEK (the post-quantum
/// hybrid surface, goal `architecture/security/post-quantum.md` § "Post-quantum
/// key publication and derivation"). Fed to `fauna_pq_kem`'s
/// `expand(MSEK, context, 64) → ML-KEM-768.KeyGen`.
pub const RECIPIENT_MLKEM_DERIVE_CONTEXT: &str = "fauna.mail.recipient-mlkem.v1";

/// Derive the actor's standing recipient-mail **X-Wing** (ML-KEM-768 ∥ X25519)
/// keypair from MSEK.
///
/// The **X25519 half is the *existing* recipient-mail key**, reused verbatim from
/// [`derive_recipient_hpke_keypair`] (not minted) — so a classical-only sender
/// and a hybrid sender target the *same* X25519 public key, and the X-Wing key is
/// a pure superset. Only the **ML-KEM-768 half is newly derived**, from MSEK via
/// [`RECIPIENT_MLKEM_DERIVE_CONTEXT`] (`expand(MSEK, context, 64) → KeyGen`).
/// Like the X25519 key it is deterministic in `msek` → **fleet-consistent**
/// (every device re-derives the identical keypair) and rides MSEK rotation. The
/// public half's encapsulation key is what `provision_recipient_mls_pubkey`
/// publishes (additive sibling field, S3c); the secret half is what the
/// MDA / client opens hybrid mail with. See `key-material-hierarchy.md`
/// § Path B-sibling-2 (the X25519 half it reuses) and
/// `post-quantum.md` § "Post-quantum key publication and derivation".
#[must_use]
pub fn derive_recipient_xwing_keypair(msek: &[u8; 32]) -> fauna_pq_kem::XWingKeyPair {
    let (x25519_secret, _x25519_public) = derive_recipient_hpke_keypair(msek);
    fauna_pq_kem::derive_keypair(msek, RECIPIENT_MLKEM_DERIVE_CONTEXT, &x25519_secret)
}

/// The `content.read{mail}` capability-grant payload for an owner's standing
/// mail secret (the calendar grant wraps its own,
/// [`derive_recipient_calendar_capability_secret`]): the recipient's X-Wing
/// mail secret serialized as
/// `x25519_secret(32) ∥ mlkem_decaps_key(2400)` (the `derive_recipient_xwing_keypair`
/// secret halves).
///
/// **Single source of truth for the `32 + 2400` byte contract**
/// (`architecture/security/post-quantum.md` § surface A → the derived-High
/// capability-grant note): both the client-side mint
/// (`fauna_client_capabilities::derive_scope_payload`) and the `fauna-ffi`
/// recipient-material export derive it here, so the shape can never drift between
/// them. The first 32 bytes are byte-identical to [`derive_recipient_hpke_keypair`]'s
/// secret (the X-Wing keypair reuses that X25519 key), so the payload is a pure
/// superset: a holder's `open_mail_record_with_key` (32-vs-2432 length dispatch)
/// opens **both** classical and hybrid (X-Wing-sealed) mail records with it, which is
/// what makes hybrid-sealed mail drainable under the grant. Deterministic in `msek`.
#[must_use]
pub fn derive_recipient_mail_capability_secret(msek: &[u8; 32]) -> Vec<u8> {
    let kp = derive_recipient_xwing_keypair(msek);
    let mut payload = Vec::with_capacity(32 + fauna_pq_kem::MLKEM768_DECAPS_KEY_LEN);
    payload.extend_from_slice(kp.secret.x25519_secret());
    payload.extend_from_slice(kp.secret.mlkem_decaps_key());
    payload
}

/// Length of one MSEK generation's chunk in a **master-key** `content.read{mail}`
/// payload: the generation's standing recipient secret
/// ([`derive_recipient_mail_capability_secret`], `x25519_secret ∥ ml-kem-dk`,
/// `32 + 2400` bytes) followed by its 32-byte mail-epoch root
/// ([`derive_mail_epoch_root`]) — `capability-grants.md` § Capability tiering →
/// *Content-sealing epochs* → *What a master-key mail wrap conveys*.
pub const MASTER_MAIL_CHUNK_LEN: usize = 32 + fauna_pq_kem::MLKEM768_DECAPS_KEY_LEN + 32;

/// The most MSEK generations a master-key mail payload carries (newest first;
/// older ones are left out and stay dark to the grant). The cap is also what
/// keeps the master form apart from the bounded `k × 2432` form in the
/// opener's length dispatch: the first length both forms share is
/// `76 × 2464 = 77 × 2432`, past this cap.
pub const MAX_MASTER_MAIL_GENERATIONS: usize = 64;

/// The **master-key** `content.read{mail}` capability payload: one
/// [`MASTER_MAIL_CHUNK_LEN`]-byte chunk per retained MSEK generation, newest
/// first (`msek`, then `prior_mseks_newest_first` — `MailConfig.prior_mseks`'
/// own order), truncated to the newest [`MAX_MASTER_MAIL_GENERATIONS`]. Each
/// chunk is the generation's standing recipient secret then its mail-epoch
/// root, so the holder opens that generation's standing-sealed AND
/// epoch-sealed mail (`open_mail_record_with_grant_key`). Ruled in
/// `capability-grants.md` § Capability tiering → *Content-sealing epochs* →
/// *What a master-key mail wrap conveys*; the bounded per-epoch form is
/// [`derive_recipient_mail_epoch_capability_secret_from_root`].
#[must_use]
pub fn derive_master_mail_capability_payload(
    msek: &[u8; 32],
    prior_mseks_newest_first: &[[u8; 32]],
) -> Zeroizing<Vec<u8>> {
    let generations = std::iter::once(msek)
        .chain(prior_mseks_newest_first.iter())
        .take(MAX_MASTER_MAIL_GENERATIONS);
    let mut payload = Zeroizing::new(Vec::with_capacity(
        MASTER_MAIL_CHUNK_LEN
            * (1 + prior_mseks_newest_first.len()).min(MAX_MASTER_MAIL_GENERATIONS),
    ));
    for g in generations {
        let standing = Zeroizing::new(derive_recipient_mail_capability_secret(g));
        payload.extend_from_slice(&standing);
        payload.extend_from_slice(&*derive_mail_epoch_root(g));
    }
    payload
}

// ── Calendar and card recipient keypairs (`owner-key-material.md`
// § Path B-sibling-5 / § Path B-sibling-6, designed 2026-10-23) ──
//
// Each DAV kind gets its own standing keypair, two steps off MSEK: a
// per-kind ROOT (`derive_key(<kind>.root context, MSEK)`), then the X25519
// half (`derive_key(<kind>.recipient-hpke context, root)` → RFC 9180
// `DeriveKeyPair`) and the ML-KEM-768 half (`expand(root, <kind>.recipient-
// mlkem context)`) of an X-Wing keypair. The root is what rotation grace
// carries (one per prior generation, `MlsSnapshotPlaintext::
// calendar_grace_roots` / `card_grace_roots`), and the root a later calendar
// content-sealing epoch would derive its per-epoch keys from — so a kind
// adopting epochs changes no carriage. Golden-byte-pinned below.

/// Domain-separation context for the **calendar root**
/// (`calendar_root = BLAKE3::derive_key(context, MSEK)`). Versioned + dated per
/// `key-material-hierarchy.md` rule #3.
pub const CALENDAR_ROOT_DERIVE_CONTEXT: &str = "fauna.calendar.root.v1 2026-10-23";
/// Context for the X25519 half of the recipient-calendar keypair, keyed by the
/// calendar root.
pub const RECIPIENT_CALENDAR_HPKE_DERIVE_CONTEXT: &str =
    "fauna.calendar.recipient-hpke.v1 2026-10-23";
/// Context for the ML-KEM-768 half of the recipient-calendar X-Wing keypair,
/// keyed by the calendar root (the mail ML-KEM context's undated shape).
pub const RECIPIENT_CALENDAR_MLKEM_DERIVE_CONTEXT: &str = "fauna.calendar.recipient-mlkem.v1";
/// Domain-separation context for the **card root** — the calendar root's twin.
pub const CARD_ROOT_DERIVE_CONTEXT: &str = "fauna.card.root.v1 2026-10-23";
/// Context for the X25519 half of the recipient-card keypair.
pub const RECIPIENT_CARD_HPKE_DERIVE_CONTEXT: &str = "fauna.card.recipient-hpke.v1 2026-10-23";
/// Context for the ML-KEM-768 half of the recipient-card X-Wing keypair.
pub const RECIPIENT_CARD_MLKEM_DERIVE_CONTEXT: &str = "fauna.card.recipient-mlkem.v1";

/// Which DAV kind a body belongs to — the selector of root, keypair, envelope
/// kind and AAD for everything CalDAV ([`Self::Calendar`]) and CardDAV
/// ([`Self::Card`]) seal at rest (`owner-key-material.md` § Path B-sibling-5,
/// § Path B-sibling-6). Mail is not a DAV kind: its standing keypair derives
/// straight from MSEK ([`derive_recipient_hpke_keypair`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DavKind {
    Calendar,
    Card,
}

impl DavKind {
    /// The envelope kind a body of this DAV kind is sealed as.
    #[must_use]
    pub const fn record_kind(self) -> RecordKind {
        match self {
            Self::Calendar => RecordKind::Calendar,
            Self::Card => RecordKind::Card,
        }
    }

    const fn root_context(self) -> &'static str {
        match self {
            Self::Calendar => CALENDAR_ROOT_DERIVE_CONTEXT,
            Self::Card => CARD_ROOT_DERIVE_CONTEXT,
        }
    }

    const fn hpke_context(self) -> &'static str {
        match self {
            Self::Calendar => RECIPIENT_CALENDAR_HPKE_DERIVE_CONTEXT,
            Self::Card => RECIPIENT_CARD_HPKE_DERIVE_CONTEXT,
        }
    }

    const fn mlkem_context(self) -> &'static str {
        match self {
            Self::Calendar => RECIPIENT_CALENDAR_MLKEM_DERIVE_CONTEXT,
            Self::Card => RECIPIENT_CARD_MLKEM_DERIVE_CONTEXT,
        }
    }
}

/// Derive a generation's 32-byte root for DAV `kind` from its MSEK — the one
/// value rotation grace carries for that kind. [`Zeroizing`], the mail-epoch
/// root's custody rule ([`derive_mail_epoch_root`] → *Custody of the returned
/// bytes*): the root is complete for every read of its kind and generation.
#[must_use]
pub fn derive_dav_root(kind: DavKind, msek: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    Zeroizing::new(blake3::derive_key(kind.root_context(), msek))
}

/// [`derive_dav_root`] for the calendar (`owner-key-material.md`
/// § Path B-sibling-5 → *Root*).
#[must_use]
pub fn derive_calendar_root(msek: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    derive_dav_root(DavKind::Calendar, msek)
}

/// [`derive_dav_root`] for the address book (§ Path B-sibling-6).
#[must_use]
pub fn derive_card_root(msek: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    derive_dav_root(DavKind::Card, msek)
}

/// Compile-time pins, the mail-epoch root's twins: revert either return type
/// to `[u8; 32]` and the build fails here.
const _CALENDAR_ROOT_IS_NOT_COPY: fn(&[u8; 32]) -> Zeroizing<[u8; 32]> = derive_calendar_root;
const _CARD_ROOT_IS_NOT_COPY: fn(&[u8; 32]) -> Zeroizing<[u8; 32]> = derive_card_root;

/// Derive the standing recipient X25519 keypair of DAV `kind` from that kind's
/// `root`. Returns `(secret, public)`; deterministic in the root, so every
/// device holding the account's mail custody derives the identical pair.
#[must_use]
pub fn derive_recipient_dav_hpke_keypair(kind: DavKind, root: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let ikm = blake3::derive_key(kind.hpke_context(), root);
    crate::wrapped_blob::envelope::derive_x25519_keypair_from_ikm(&ikm)
}

/// Derive the standing recipient **X-Wing** keypair of DAV `kind` from its
/// `root` — the mail X-Wing shape: the X25519 half IS
/// [`derive_recipient_dav_hpke_keypair`]'s, the ML-KEM-768 half derives from
/// the same root under the kind's ML-KEM context.
#[must_use]
pub fn derive_recipient_dav_xwing_keypair(
    kind: DavKind,
    root: &[u8; 32],
) -> fauna_pq_kem::XWingKeyPair {
    let (x25519_secret, _x25519_public) = derive_recipient_dav_hpke_keypair(kind, root);
    fauna_pq_kem::derive_keypair(root, kind.mlkem_context(), &x25519_secret)
}

/// The public halves of an owner's calendar and card recipient X-Wing
/// keypairs — what an app registers with the nest and what every sealer
/// seals to. Public material only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DavSealPubkeys {
    /// 32-byte X25519 public half of the recipient-calendar keypair.
    pub calendar_pubkey: [u8; 32],
    /// 1184-byte ML-KEM-768 encapsulation key of the recipient-calendar X-Wing
    /// keypair.
    pub calendar_mlkem_ek: Vec<u8>,
    /// 32-byte X25519 public half of the recipient-card keypair.
    pub card_pubkey: [u8; 32],
    /// 1184-byte ML-KEM-768 encapsulation key of the recipient-card X-Wing
    /// keypair.
    pub card_mlkem_ek: Vec<u8>,
}

impl DavSealPubkeys {
    /// Both DAV kinds' public halves from one MSEK — through
    /// [`derive_recipient_dav_xwing_keypair`], so what an app registers is by
    /// construction what the MDA and every app open with. The one derivation:
    /// the mail-settings machine and the FFI both consume it.
    #[must_use]
    pub fn derive(msek: &[u8; 32]) -> Self {
        let public = |kind: DavKind| {
            derive_recipient_dav_xwing_keypair(kind, &derive_dav_root(kind, msek)).public
        };
        let calendar = public(DavKind::Calendar);
        let card = public(DavKind::Card);
        Self {
            calendar_pubkey: *calendar.x25519_public(),
            calendar_mlkem_ek: calendar.mlkem_encaps_key().to_vec(),
            card_pubkey: *card.x25519_public(),
            card_mlkem_ek: card.mlkem_encaps_key().to_vec(),
        }
    }
}

/// The recipient-calendar X25519 keypair from the calendar root.
#[must_use]
pub fn derive_recipient_calendar_hpke_keypair(root: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    derive_recipient_dav_hpke_keypair(DavKind::Calendar, root)
}

/// The recipient-calendar X-Wing keypair from the calendar root.
#[must_use]
pub fn derive_recipient_calendar_xwing_keypair(root: &[u8; 32]) -> fauna_pq_kem::XWingKeyPair {
    derive_recipient_dav_xwing_keypair(DavKind::Calendar, root)
}

/// The recipient-card X25519 keypair from the card root.
#[must_use]
pub fn derive_recipient_card_hpke_keypair(root: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    derive_recipient_dav_hpke_keypair(DavKind::Card, root)
}

/// The recipient-card X-Wing keypair from the card root.
#[must_use]
pub fn derive_recipient_card_xwing_keypair(root: &[u8; 32]) -> fauna_pq_kem::XWingKeyPair {
    derive_recipient_dav_xwing_keypair(DavKind::Card, root)
}

/// `x25519_secret(32) ∥ mlkem_decaps_key(2400)` of an X-Wing keypair — the
/// one byte layout every standing and per-epoch capability payload shares.
fn xwing_capability_payload(kp: &fauna_pq_kem::XWingKeyPair) -> Vec<u8> {
    let mut payload = Vec::with_capacity(32 + fauna_pq_kem::MLKEM768_DECAPS_KEY_LEN);
    payload.extend_from_slice(kp.secret.x25519_secret());
    payload.extend_from_slice(kp.secret.mlkem_decaps_key());
    payload
}

/// The standing capability payload of DAV `kind` for an MSEK — in
/// [`derive_recipient_mail_capability_secret`]'s exact byte contract, so a
/// holder's 32-vs-2432 length-dispatch opener is unchanged. Never the root:
/// a root would convey every future epoch of the kind.
#[must_use]
pub fn derive_recipient_dav_capability_secret(kind: DavKind, msek: &[u8; 32]) -> Vec<u8> {
    xwing_capability_payload(&derive_recipient_dav_xwing_keypair(
        kind,
        &derive_dav_root(kind, msek),
    ))
}

/// The `content.read{calendar}` grant payload: the standing recipient-calendar
/// secret (`owner-key-material.md` § Path B-sibling-5 → *Capability payload*).
/// Opens `calendar-record` envelopes only.
#[must_use]
pub fn derive_recipient_calendar_capability_secret(msek: &[u8; 32]) -> Vec<u8> {
    derive_recipient_dav_capability_secret(DavKind::Calendar, msek)
}

/// The standing recipient-card secret in the same byte contract. No
/// `content.read{card}` scope is minted today (§ Path B-sibling-6); this is the
/// payload one would carry.
#[must_use]
pub fn derive_recipient_card_capability_secret(msek: &[u8; 32]) -> Vec<u8> {
    derive_recipient_dav_capability_secret(DavKind::Card, msek)
}

/// One DAV kind's **standing ring** — one keypair per MSEK generation, newest
/// first — the per-kind sibling of the standing mail set
/// ([`derive_standing_mail_keypairs`]). Every holder derives the same ring:
/// the owner's apps from `msek` + `prior_mseks` ([`Self::from_mseks`]), the MDA
/// from the session MSEK + the snapshot's grace roots of the kind
/// ([`Self::from_msek_and_grace_roots`]) — so whatever one can open, the other
/// can (§ Path B-sibling-5 → *Snapshot carriage*). It opens records of its own
/// kind only: [`Self::open`] refuses any other envelope kind before a trial.
pub struct StandingDavRing {
    kind: DavKind,
    keypairs: Vec<StandingMailKeypair>,
}

impl std::fmt::Debug for StandingDavRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StandingDavRing")
            .field("kind", &self.kind)
            .field("generations", &self.keypairs.len())
            .finish_non_exhaustive()
    }
}

impl StandingDavRing {
    /// The ring over per-generation `roots` of `kind`, newest first.
    #[must_use]
    pub fn from_roots(kind: DavKind, roots: &[&[u8; 32]]) -> Self {
        let keypairs = roots
            .iter()
            .map(|root| {
                let xwing = derive_recipient_dav_xwing_keypair(kind, root);
                StandingMailKeypair::new_hybrid(
                    xwing.secret.x25519_secret(),
                    xwing.secret.mlkem_decaps_key(),
                )
            })
            .collect();
        Self { kind, keypairs }
    }

    /// The ring over an MSEK history — `msek` the current generation, then
    /// `prior_mseks` newest first (the custody's order) — each generation's
    /// root derived here and dropped.
    #[must_use]
    pub fn from_mseks(kind: DavKind, msek: &[u8; 32], prior_mseks: &[[u8; 32]]) -> Self {
        let roots: Vec<Zeroizing<[u8; 32]>> = std::iter::once(msek)
            .chain(prior_mseks)
            .map(|m| derive_dav_root(kind, m))
            .collect();
        let refs: Vec<&[u8; 32]> = roots.iter().map(|r| &**r).collect();
        Self::from_roots(kind, &refs)
    }

    /// The MDA's ring: the current generation's root re-derived from the
    /// session `msek` (never carried), then the snapshot's grace roots of the
    /// kind as carried (newest first).
    #[must_use]
    pub fn from_msek_and_grace_roots(
        kind: DavKind,
        msek: &[u8; 32],
        grace_roots: &[&[u8; 32]],
    ) -> Self {
        let current = derive_dav_root(kind, msek);
        let refs: Vec<&[u8; 32]> = std::iter::once(&*current)
            .chain(grace_roots.iter().copied())
            .collect();
        Self::from_roots(kind, &refs)
    }

    /// The DAV kind this ring opens.
    #[must_use]
    pub fn kind(&self) -> DavKind {
        self.kind
    }

    /// How many generations the ring holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.keypairs.len()
    }

    /// Whether the ring holds no generation.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keypairs.is_empty()
    }

    /// Open a sealed record of this ring's kind, newest generation first.
    ///
    /// # Errors
    ///
    /// `UnwrapError::InvalidFormat` for an envelope of any other kind (refused
    /// before any trial), `UnwrapError::HpkeFailed` when no generation opens it.
    pub fn open(&self, envelope: &MailRecordEnvelope) -> Result<Vec<u8>, UnwrapError> {
        let kind = self.kind.record_kind();
        if envelope.kind != kind.as_str() {
            return Err(UnwrapError::InvalidFormat(format!(
                "expected kind={}, got {}",
                kind.as_str(),
                envelope.kind
            )));
        }
        open_record_standing(kind, envelope, &self.keypairs, &[], None)
            .ok_or(UnwrapError::HpkeFailed)
    }
}

// ── Mail content-sealing epochs (design spec 2026-07-18; goal
// `encryption-at-rest.md` § Capability tiering → Content-sealing epochs) ──

/// Length of one mail content-sealing epoch, in seconds (weekly). A hard-coded
/// per-kind constant — neither users nor admins choose an epoch length, so
/// this is bucket (1) of the configuration invariant: a Rust constant, no
/// config surface. Weekly balances grant-blob size (~13 per-epoch wraps per
/// 90-day window), forensic narrowing (a 7-day suspect window), and the
/// pubkey-schedule horizon (26 epochs ≈ 6 months at ~32 KB per actor).
pub const MAIL_SEALING_EPOCH_SECS: u64 = 7 * 24 * 60 * 60;

/// How many future epochs' public keys the owner's client pre-publishes to
/// the nest (`actor_epoch_seal_keys`), so the MTA can epoch-seal inbound mail
/// with no client online. 26 weekly epochs ≈ 6 months of client-offline
/// tolerance before the never-bounce degradation (seal under the newest
/// published epoch key) kicks in.
pub const MAIL_EPOCH_PUBLISH_HORIZON: u64 = 26;

/// **The mail ingest epoch-sealing write gate — FLIPPED `true` 2026-07-19**
/// (one deliberate user-gated commit, the `MAIL_ENVELOPE_WRITE_FORMAT`
/// precedent; user-confirmed with the flip checklist green — see
/// `docs/goal/architecture/encryption-at-rest.md` § Capability tiering →
/// Content-sealing epochs for the checklist state at flip). While it was
/// false, the nest's D2 seal-key resolver ignored any published epoch
/// schedule and sealed under the standing key. Now new mail ingest for a
/// recipient with a published epoch schedule seals under the current epoch's
/// key (never-bounce degradation: stale schedule → newest published epoch →
/// standing key; a recipient with no schedule is unchanged). Every reader
/// that must open post-flip mail shipped first — all clients' epoch opener
/// chain, the MDA/drain legs, and the pinned previous build advanced past
/// them (within-major bidirectional compatibility). **Never flip this back:**
/// content sealed under epoch keys exists from 2026-07-19 on, and the epoch
/// derivation goldens are eternal. Content-sealing-epochs design
/// 2026-07-18 § 6.
pub const MAIL_EPOCH_SEALING_WRITE_DEFAULT: bool = true;

/// Domain-separation context for the **per-generation mail-epoch root** —
/// the single 32-byte intermediate every per-epoch derivation hangs off:
/// `epoch_root = BLAKE3::derive_key(context, MSEK)`, then each epoch key
/// derives from `epoch_root ∥ LE64(e)`.
///
/// **Why the two-step chain exists (MSEK-rotation grace, design § 5):** a
/// record epoch-sealed before an MSEK hard-revoke needs the OLD generation's
/// epoch keys to open, and the old-root-sealed epoch span is unbounded (the
/// § 3 stale-schedule degradation plus the § 5 rotation window can seal
/// under any epoch of the old root's lifetime) — so grace material must be a
/// *root*, not an enumeration. Carrying the raw prior MSEK would resurrect
/// every old-generation power; this derived root is complete for mail-epoch
/// reads and nothing else (`key-material-hierarchy.md` rule #3's
/// least-privilege shape, the same reason the standing grace carries derived
/// leaf keypairs rather than prior MSEKs). The snapshot carries one such
/// root per grace generation (`MlsSnapshotPlaintext::mail_epoch_grace_roots`).
pub const MAIL_EPOCH_ROOT_DERIVE_CONTEXT: &str = "fauna.mail.epoch-root.v1";

/// Domain-separation context for the **X25519 half** of a per-epoch
/// recipient-mail keypair. Versioned per `key-material-hierarchy.md` rule #3.
/// The epoch index is folded into the *key material*
/// (`epoch_root ∥ LE64(e)`, root per [`MAIL_EPOCH_ROOT_DERIVE_CONTEXT`]),
/// not the context, so every epoch is an independent PRF output of the
/// secret root — deliberately NOT a chain: holding any set of epoch secrets
/// yields nothing about any other epoch (the capability-expiry
/// forward-bounding requirement (iii)).
pub const RECIPIENT_HPKE_EPOCH_DERIVE_CONTEXT: &str = "fauna.mail.recipient-hpke-epoch.v1";

/// Domain-separation context for the **ML-KEM-768 half** of a per-epoch
/// recipient-mail X-Wing keypair (the epoch sibling of
/// [`RECIPIENT_MLKEM_DERIVE_CONTEXT`]).
pub const RECIPIENT_MLKEM_EPOCH_DERIVE_CONTEXT: &str = "fauna.mail.recipient-mlkem-epoch.v1";

/// The mail content-sealing epoch containing `unix_secs`:
/// `floor(unix_secs / MAIL_SEALING_EPOCH_SECS)`, an **absolute** index (no
/// per-user genesis) so every device computes the same schedule with no
/// negotiation. Pure — the caller supplies the clock (this crate has none;
/// wasm-clean).
#[must_use]
pub fn mail_sealing_epoch_of(unix_secs: u64) -> u64 {
    unix_secs / MAIL_SEALING_EPOCH_SECS
}

/// The inclusive set of epoch indices whose time-range intersects a grant's
/// `[start, end]` unix-seconds window — the per-epoch wraps a bounded mail
/// grant carries (`WrappedScopeKey.epoch = Some(e)` for each `e` here). The
/// window itself stays unix-seconds on the wire, ALWAYS; epoch indices never
/// go in `GrantWindow` (design § Revision history 2026-07-06).
#[must_use]
pub fn mail_epoch_range_for_window(window: &GrantWindow) -> std::ops::RangeInclusive<u64> {
    mail_sealing_epoch_of(window.0)..=mail_sealing_epoch_of(window.1)
}

/// Derive a generation's 32-byte mail-epoch root from its MSEK — step one of
/// the two-step epoch derivation ([`MAIL_EPOCH_ROOT_DERIVE_CONTEXT`] has the
/// full rationale: this root is what MSEK-rotation grace carries, complete
/// for every epoch of its generation yet powerless beyond mail-epoch reads).
///
/// # Custody of the returned bytes
///
/// [`Zeroizing`] rather than a bare `[u8; 32]`, the same rule its two
/// MSEK-derived siblings carry ([`derive_index_segment_key`] here and
/// `fauna_core::crypto::derive_index_master_key`, which documents the
/// reasoning in full): a bare array is `Copy` and never zeroized, so it leaks
/// implicit duplicates the type system says nothing about.
///
/// This member is the **strongest** of the three, which is why it must not be
/// the one left bare: rule #7 bounds the index-segment key to index-token read
/// material over mail/calendar, never mail bodies, whereas this root is
/// complete for *every* per-epoch mail content key of its MSEK generation
/// (`key-material-hierarchy.md` § Path B-sibling-2 → *Per-epoch extension*).
/// The need was already recognised as a hand-wrapping **convention** at three
/// call sites before it was a signature; making it the signature is what stops
/// a fourth call site from forgetting. Ratified in `key-material-hierarchy.md`
/// § Path B-sibling-2 → *Custody of the derived bytes*.
#[must_use]
pub fn derive_mail_epoch_root(msek: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    Zeroizing::new(blake3::derive_key(MAIL_EPOCH_ROOT_DERIVE_CONTEXT, msek))
}

/// Compile-time pin, the index keys' twin — **named mutation**:
/// revert this return type to `[u8; 32]` and the build must fail here with a
/// type mismatch, ahead of any call site. A function-pointer coercion and
/// **not** the conflicting-impl trick: coherence rejects an `impl<T: Copy>`
/// blanket against a foreign type like [`Zeroizing`] whether or not that type
/// is `Copy`, so that shape pins nothing (verified by running it).
const _MAIL_EPOCH_ROOT_IS_NOT_COPY: fn(&[u8; 32]) -> Zeroizing<[u8; 32]> = derive_mail_epoch_root;

/// Domain-separation context for the **mail/calendar index-segment key** —
/// the per-kind wrap key of the content-index per-kind key split
/// (`key-material-hierarchy.md` § Path B-sibling-4, ratified 2026-08-02;
/// `content-index.md` § Encryption posture). Versioned + dated per rule #3.
pub const INDEX_SEGMENT_KEY_DERIVE_CONTEXT: &str = "fauna.mail.index-seg.v1 2026-08-02";

/// Derive a generation's 32-byte **mail/calendar index-segment key** from its
/// MSEK. Fleet-consistent like every sibling here: a client derives it from its
/// own `fauna.state.mail` MSEK (the client-builder leg needs no snapshot); the
/// MDA derives it from the session MSEK at MUA-AUTH. The **current**
/// generation's key is never carried in the snapshot — only prior grace
/// generations ride it (the `mail_epoch_grace_roots` pattern; that additive
/// field ships with the MDA leg, rollout S5). Consumed opaquely by
/// `fauna_index::IndexSegmentKey`.
///
/// # Custody of the returned bytes
///
/// [`Zeroizing`] rather than a bare `[u8; 32]`, for the reason its master-key
/// sibling documents in full (`fauna_core::crypto::derive_index_master_key`
/// → *Custody of the returned bytes*): a bare array is `Copy` and never
/// zeroized, so it leaks implicit duplicates the type system says nothing
/// about. The two index-key derivations deliberately keep **one** shape —
/// fixing only the master key would have split the family — and the custody
/// type proper is `fauna_index::IndexSegmentKey`, which this crate cannot
/// reach any more than `fauna-core` can. Ratified in
/// `key-material-hierarchy.md` § Path B-sibling-4 → *Custody of the derived
/// bytes*.
#[must_use]
pub fn derive_index_segment_key(msek: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    Zeroizing::new(blake3::derive_key(INDEX_SEGMENT_KEY_DERIVE_CONTEXT, msek))
}

/// Compile-time pin, the master key's twin — **named mutation**:
/// revert this return type to `[u8; 32]` and the build must fail here with a
/// type mismatch. See `fauna_core::crypto::derive_index_master_key`'s pin for
/// why a runtime test cannot do this job.
const _INDEX_SEGMENT_KEY_IS_NOT_COPY: fn(&[u8; 32]) -> Zeroizing<[u8; 32]> =
    derive_index_segment_key;

/// `epoch_root ∥ LE64(e)` — the per-epoch key material both epoch
/// derivations consume. Folding `e` into the material (under an
/// epoch-specific context) makes each epoch an independent derivation from
/// the generation's root.
fn epoch_key_material(epoch_root: &[u8; 32], e: u64) -> [u8; 40] {
    let mut km = [0u8; 40];
    km[..32].copy_from_slice(epoch_root);
    km[32..].copy_from_slice(&e.to_le_bytes());
    km
}

/// Derive the actor's per-epoch recipient-mail X25519 keypair for sealing
/// epoch `e` — the epoch sibling of [`derive_recipient_hpke_keypair`].
/// Deterministic in `(msek, e)` → fleet-consistent, and re-derivable on
/// demand by any MSEK holder (the owner's clients; the MDA after AUTH), which
/// is what keeps the MDA serve path master-key with no wrapped epoch keys.
/// Returns `(secret, public)`. Internally two-step via
/// [`derive_mail_epoch_root`]; a grace holder starts from the root with
/// [`derive_recipient_epoch_hpke_keypair_from_root`].
#[must_use]
pub fn derive_recipient_epoch_hpke_keypair(msek: &[u8; 32], e: u64) -> ([u8; 32], [u8; 32]) {
    derive_recipient_epoch_hpke_keypair_from_root(&derive_mail_epoch_root(msek), e)
}

/// [`derive_recipient_epoch_hpke_keypair`] from a generation's mail-epoch
/// root (the MSEK-rotation grace path: the snapshot carries prior
/// generations' roots, never their MSEKs).
#[must_use]
pub fn derive_recipient_epoch_hpke_keypair_from_root(
    epoch_root: &[u8; 32],
    e: u64,
) -> ([u8; 32], [u8; 32]) {
    let ikm = blake3::derive_key(
        RECIPIENT_HPKE_EPOCH_DERIVE_CONTEXT,
        &epoch_key_material(epoch_root, e),
    );
    crate::wrapped_blob::envelope::derive_x25519_keypair_from_ikm(&ikm)
}

/// Derive the actor's per-epoch recipient-mail **X-Wing** keypair for sealing
/// epoch `e` — the epoch sibling of [`derive_recipient_xwing_keypair`], with
/// the same half-reuse shape: the X25519 half IS
/// [`derive_recipient_epoch_hpke_keypair`]`(msek, e)`, so a classical-only
/// sender and a hybrid sender target the same per-epoch X25519 public key.
#[must_use]
pub fn derive_recipient_epoch_xwing_keypair(msek: &[u8; 32], e: u64) -> fauna_pq_kem::XWingKeyPair {
    derive_recipient_epoch_xwing_keypair_from_root(&derive_mail_epoch_root(msek), e)
}

/// [`derive_recipient_epoch_xwing_keypair`] from a generation's mail-epoch
/// root (the grace path).
#[must_use]
pub fn derive_recipient_epoch_xwing_keypair_from_root(
    epoch_root: &[u8; 32],
    e: u64,
) -> fauna_pq_kem::XWingKeyPair {
    let (x25519_secret, _x25519_public) =
        derive_recipient_epoch_hpke_keypair_from_root(epoch_root, e);
    fauna_pq_kem::derive_keypair(
        &epoch_key_material(epoch_root, e),
        RECIPIENT_MLKEM_EPOCH_DERIVE_CONTEXT,
        &x25519_secret,
    )
}

/// The `content.read{mail}` capability payload for sealing epoch `e`:
/// `x25519_secret(32) ∥ mlkem_decaps_key(2400)` — byte-layout-identical to
/// [`derive_recipient_mail_capability_secret`] (the standing master-key
/// payload), so the holder's `open_mail_record_with_key` 32-vs-2432 length
/// dispatch works unchanged per epoch. A **bounded** mail grant wraps one of
/// these per epoch in its window (`WrappedScopeKey.epoch = Some(e)`) and
/// never the standing secret — mixing the two in one grant would let the
/// holder outlive its window and is a mint-side error.
#[must_use]
pub fn derive_recipient_mail_epoch_capability_secret(msek: &[u8; 32], e: u64) -> Vec<u8> {
    derive_recipient_mail_epoch_capability_secret_from_root(&derive_mail_epoch_root(msek), e)
}

/// [`derive_recipient_mail_epoch_capability_secret`] from a generation's
/// mail-epoch root (the grace path: the § 4 opener chain repeats per grace
/// root carried in the snapshot).
#[must_use]
pub fn derive_recipient_mail_epoch_capability_secret_from_root(
    epoch_root: &[u8; 32],
    e: u64,
) -> Vec<u8> {
    let kp = derive_recipient_epoch_xwing_keypair_from_root(epoch_root, e);
    let mut payload = Vec::with_capacity(32 + fauna_pq_kem::MLKEM768_DECAPS_KEY_LEN);
    payload.extend_from_slice(kp.secret.x25519_secret());
    payload.extend_from_slice(kp.secret.mlkem_decaps_key());
    payload
}

/// One generation's **standing** recipient-mail secret material — the X25519
/// secret and the ML-KEM-768 decapsulation half of the X-Wing keypair derived
/// from the same MSEK — held zeroizing. The unit of the standing key set every
/// MSEK holder opens standing-sealed mail with: the MDA reads the set out of
/// the snapshot's `leaf_init_keypairs`, and the owner's clients derive the
/// identical set from their MSEK history via
/// [`derive_standing_mail_keypairs`]; both trial it through
/// [`open_mail_record_standing`] (`owner-key-material.md` § Path B-sibling-2:
/// current + the last 2 rotations for grace-decrypt). Secrets only: the
/// registered public half is routing metadata the opener never reads.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct StandingMailKeypair {
    pub x25519_secret: Zeroizing<[u8; 32]>,
    /// `None` only for a keypair parsed from a pre-hybrid snapshot entry (no
    /// `mdk`); a derived keypair always carries it.
    pub mlkem_dk: Option<Zeroizing<[u8; fauna_pq_kem::MLKEM768_DECAPS_KEY_LEN]>>,
}

impl StandingMailKeypair {
    /// A hybrid entry: both halves, copied into zeroizing storage.
    #[must_use]
    pub fn new_hybrid(
        x25519_secret: &[u8; 32],
        mlkem_dk: &[u8; fauna_pq_kem::MLKEM768_DECAPS_KEY_LEN],
    ) -> Self {
        Self {
            x25519_secret: Zeroizing::new(*x25519_secret),
            mlkem_dk: Some(Zeroizing::new(*mlkem_dk)),
        }
    }

    /// A classical-only entry (a pre-hybrid snapshot's keypair): opens X25519
    /// records only.
    #[must_use]
    pub fn new_classical(x25519_secret: &[u8; 32]) -> Self {
        Self {
            x25519_secret: Zeroizing::new(*x25519_secret),
            mlkem_dk: None,
        }
    }
}

impl std::fmt::Debug for StandingMailKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StandingMailKeypair")
            .field("hybrid", &self.mlkem_dk.is_some())
            .finish_non_exhaustive()
    }
}

/// Derive the standing recipient-mail key set for an MSEK history —
/// `mseks[0]` the current generation, then EVERY prior generation, uncapped
/// (`owner-key-material.md` § Path B-sibling-2 → *Pre-rotation mail at
/// rest*) — one [`StandingMailKeypair`] per generation, newest first. This
/// is the ONE derivation of that set: the
/// snapshot builder ([`build_mls_snapshot_plaintext`]) serializes its output
/// for the MDA, and a client holds it directly for its own receive path, so a
/// record the MDA can open is by construction one the owner's client can open
/// too (and vice versa).
#[must_use]
pub fn derive_standing_mail_keypairs(mseks: &[[u8; 32]]) -> Vec<StandingMailKeypair> {
    mseks
        .iter()
        .map(|msek| {
            let (sk, _pk) = derive_recipient_hpke_keypair(msek);
            let xwing = derive_recipient_xwing_keypair(msek);
            StandingMailKeypair::new_hybrid(&sk, xwing.secret.mlkem_decaps_key())
        })
        .collect()
}

/// **The seal-time trial order over a key set of `generations` generations**
/// — index `0` the current generation, index `i ≥ 1` the prior one retired at
/// `retired_at_unix[i - 1]` (newest first, the order every key set and the
/// snapshot's aligned lists keep). The generation current at
/// `seal_basis_unix` comes first — one step older per retirement after that
/// instant — then its neighbours outward, the OLDER one first at each step
/// (mail sealed to a just-rotated-away pubkey lands after the rotation), then
/// the rest; every index exactly once, so a miss still tries the whole ring.
/// An unknown basis (`None`), or instants missing for some generations, walk
/// from the newest. The epoch opener's select-by-timestamp shape lifted to
/// generations (`owner-key-material.md` § Path B-sibling-2 → *Pre-rotation
/// mail at rest*): with every generation carried, a per-record walk of the
/// whole ring would cost one AEAD failure per rotation ever made.
#[must_use]
pub fn generation_trial_order(
    generations: usize,
    retired_at_unix: &[u64],
    seal_basis_unix: Option<u64>,
) -> Vec<usize> {
    let Some(t) = seal_basis_unix.filter(|_| generations > 0) else {
        return (0..generations).collect();
    };
    let current_at_t = retired_at_unix
        .iter()
        .take(generations - 1)
        .filter(|&&r| r > t)
        .count();
    let mut order = Vec::with_capacity(generations);
    order.push(current_at_t);
    for d in 1..generations {
        if current_at_t + d < generations {
            order.push(current_at_t + d);
        }
        if d <= current_at_t {
            order.push(current_at_t - d);
        }
    }
    order
}

/// The standing trial: open an inner sealed mail record under the keypairs of
/// a standing key set (newest first, `retired_at_unix` aligned with
/// `keypairs[1..]`) in [`generation_trial_order`] for its `seal_basis_unix`
/// — the generation current at that instant first, then outward; the whole
/// set newest first when the basis is unknown — opening either suite per
/// keypair (hybrid when the ML-KEM half is present, classical otherwise).
/// `None` when every keypair misses — wrong recipient or a tampered envelope
/// (no rotation ever retires a generation out of the set). The standing arm
/// of [`super::open_mail_epoch_chain`] on both the MDA and the client.
#[must_use]
pub fn open_mail_record_standing(
    envelope: &MailRecordEnvelope,
    keypairs: &[StandingMailKeypair],
    retired_at_unix: &[u64],
    seal_basis_unix: Option<u64>,
) -> Option<Vec<u8>> {
    open_record_standing(
        RecordKind::Mail,
        envelope,
        keypairs,
        retired_at_unix,
        seal_basis_unix,
    )
}

/// [`open_mail_record_standing`] for a record of any `kind` — the trial shared
/// by the mail set and every [`StandingDavRing`]; a record of another kind
/// opens under no keypair (each open checks the kind first).
#[must_use]
pub fn open_record_standing(
    kind: RecordKind,
    envelope: &MailRecordEnvelope,
    keypairs: &[StandingMailKeypair],
    retired_at_unix: &[u64],
    seal_basis_unix: Option<u64>,
) -> Option<Vec<u8>> {
    generation_trial_order(keypairs.len(), retired_at_unix, seal_basis_unix)
        .into_iter()
        .find_map(|i| {
            let kp = &keypairs[i];
            match kp.mlkem_dk.as_deref() {
                Some(dk) => {
                    crate::wrapped_blob::unseal_record_hybrid(kind, envelope, &kp.x25519_secret, dk)
                }
                None => crate::wrapped_blob::unseal_record(kind, envelope, &kp.x25519_secret),
            }
            .ok()
        })
}

/// Build the v1 read-side snapshot plaintext from the actor's MSEK
/// history. `mseks[0]` is the current MSEK (its derived pubkey is the
/// one registered with nest); every further entry is a prior MSEK,
/// retained so mail sealed to any rotated-away pubkey still opens —
/// UNCAPPED (`owner-key-material.md` § Path B-sibling-2 → *Pre-rotation
/// mail at rest*). `retired_at_unix` is each prior generation's
/// retirement instant, aligned with `mseks[1..]` (extra entries are
/// ignored; a short list leaves the later generations without one, and
/// their openers walk the ring).
///
/// The v1 snapshot carries no OpenMLS provider state — it is fully
/// derivable from MSEK, which is why the per-app `MlsSnapshotProvider`
/// seam is unnecessary. When the snapshot later grows to carry genuine
/// group/epoch state, that becomes a shared export over `MlsEngine`.
#[must_use]
pub fn build_mls_snapshot_plaintext(
    mseks: &[[u8; 32]],
    retired_at_unix: &[u64],
) -> MlsSnapshotPlaintext {
    // The SAME set the owner's own clients open with
    // (`derive_standing_mail_keypairs`): the MDA reads it out of this snapshot,
    // a client derives it from its `fauna.state.mail` MSEK history, and both
    // trial it through `open_mail_record_standing` — one key set, one chain
    // (`owner-key-material.md` § Path B-sibling-2).
    let leaf_init_keypairs = mseks
        .iter()
        .zip(derive_standing_mail_keypairs(mseks))
        .map(|(msek, kp)| {
            // The registered public half rides beside the secret set (the
            // snapshot entry carries it; the secret type deliberately does not).
            let (_sk, pk) = derive_recipient_hpke_keypair(msek);
            // Carry the MSEK-derived ML-KEM decaps half too (X25519 half of the
            // X-Wing keypair == the classical pair, reused) so the MDA can open
            // both classical and hybrid mail for every grace MSEK. Additive: a
            // reader that predates S3d ignores the `mdk` field.
            let dk = kp
                .mlkem_dk
                .as_deref()
                .expect("derive_standing_mail_keypairs always carries the ML-KEM half");
            LeafInitKeypair::new_hybrid(pk, *kp.x25519_secret, dk)
        })
        .collect();
    // Mail-epoch grace roots for the PRIOR generations only (the current
    // generation's root re-derives from the session MSEK on demand) — the
    // § 5 rotation-grace material for epoch-sealed mail, same order as the
    // leaf list.
    let mail_epoch_grace_roots = mseks
        .iter()
        .skip(1)
        .map(|msek| ByteBuf::from(derive_mail_epoch_root(msek).to_vec()))
        .collect();
    // Mail/calendar index-segment keys for the PRIOR generations only — same
    // order, same skip-the-current rule as the mail-epoch roots
    // above, so index i of both lists describes `leaf_init_keypairs[i + 1]`.
    // Rotation-grace material for the content index's mail/calendar slices
    // (`key-material-hierarchy.md` § Path B-sibling-4 → *Snapshot carriage*):
    // a segment the rotating client has not rewrapped yet still opens here.
    let index_seg_grace_keys = mseks
        .iter()
        .skip(1)
        .map(|msek| ByteBuf::from(derive_index_segment_key(msek).to_vec()))
        .collect();
    // The prior generations' retirement instants, aligned the same way — the
    // MDA opener's seal-time selection (`generation_trial_order`).
    let generation_retired_at_unix = retired_at_unix
        .iter()
        .copied()
        .take(mseks.len().saturating_sub(1))
        .collect();
    // The calendar and card roots of the PRIOR generations — the same
    // skip-the-current slice, so index i of every prior list describes
    // `leaf_init_keypairs[i + 1]` (`owner-key-material.md` § Path
    // B-sibling-5 → *Snapshot carriage*).
    let dav_grace_roots = |kind: DavKind| -> Vec<ByteBuf> {
        mseks
            .iter()
            .skip(1)
            .map(|msek| ByteBuf::from(derive_dav_root(kind, msek).to_vec()))
            .collect()
    };
    MlsSnapshotPlaintext {
        leaf_init_keypairs,
        mail_epoch_grace_roots,
        index_seg_grace_keys,
        generation_retired_at_unix,
        calendar_grace_roots: dav_grace_roots(DavKind::Calendar),
        card_grace_roots: dav_grace_roots(DavKind::Card),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wrapped_blob::envelope::generate_x25519_keypair;
    use crate::wrapped_blob::{seal_to_recipient, unseal_mail_record};

    #[test]
    fn cbor_roundtrip_preserves_keypairs() {
        let (sk1, pk1) = generate_x25519_keypair();
        let (sk2, pk2) = generate_x25519_keypair();
        let snap = MlsSnapshotPlaintext {
            leaf_init_keypairs: vec![
                LeafInitKeypair::new(pk1, sk1),
                LeafInitKeypair::new(pk2, sk2),
            ],
            ..Default::default()
        };
        let bytes = snap.to_canonical_bytes().unwrap();
        let decoded = MlsSnapshotPlaintext::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.v, SNAPSHOT_PLAINTEXT_VERSION);
        assert_eq!(decoded.leaf_init_keypairs.len(), 2);
        assert_eq!(decoded.leaf_init_keypairs[0].x25519_pubkey.as_slice(), &pk1);
        assert_eq!(decoded.leaf_init_keypairs[0].x25519_secret.as_slice(), &sk1);
        assert_eq!(decoded.leaf_init_keypairs[1].x25519_pubkey.as_slice(), &pk2);
        assert_eq!(decoded.leaf_init_keypairs[1].x25519_secret.as_slice(), &sk2);
    }

    #[test]
    fn rejects_wrong_pubkey_length() {
        // Hand-roll the CBOR rather than going through to_canonical_bytes
        // (which always produces 32-byte buffers).
        let bad = MlsSnapshotPlaintext {
            leaf_init_keypairs: vec![LeafInitKeypair {
                x25519_pubkey: ByteBuf::from(vec![0u8; 31]),
                x25519_secret: ByteBuf::from(vec![0u8; 32]),
                mlkem_dk: None,
            }],
            ..Default::default()
        };
        let bytes = bad.to_canonical_bytes().unwrap();
        let err = MlsSnapshotPlaintext::from_canonical_bytes(&bytes).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }

    #[test]
    fn rejects_unknown_version() {
        let bad = MlsSnapshotPlaintext {
            v: 99,
            ..Default::default()
        };
        let bytes = bad.to_canonical_bytes().unwrap();
        let err = MlsSnapshotPlaintext::from_canonical_bytes(&bytes).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }

    // ── recipient-mail keypair derivation (Path B-sibling-2) ──

    #[test]
    fn derive_recipient_keypair_is_deterministic() {
        // Fleet-consistency: the same MSEK on any device derives the
        // identical keypair, so any device can register the pubkey and
        // any can open mail sealed to it.
        let msek = [7u8; 32];
        let (sk1, pk1) = derive_recipient_hpke_keypair(&msek);
        let (sk2, pk2) = derive_recipient_hpke_keypair(&msek);
        assert_eq!(sk1, sk2);
        assert_eq!(pk1, pk2);
        // Both halves are real 32-byte X25519 material, not zeroed.
        assert_ne!(sk1, [0u8; 32]);
        assert_ne!(pk1, [0u8; 32]);
    }

    #[test]
    fn distinct_mseks_give_distinct_keypairs() {
        let (_, pk_a) = derive_recipient_hpke_keypair(&[1u8; 32]);
        let (_, pk_b) = derive_recipient_hpke_keypair(&[2u8; 32]);
        assert_ne!(pk_a, pk_b);
    }

    // ── recipient-mail X-Wing keypair derivation (post-quantum surface A) ──

    #[test]
    fn derive_recipient_xwing_keypair_is_fleet_consistent() {
        // Same MSEK on any device ⇒ identical X-Wing keypair (the published ek
        // must match across devices, exactly as the classical X25519 pubkey does).
        let msek = [9u8; 32];
        let kp1 = derive_recipient_xwing_keypair(&msek);
        let kp2 = derive_recipient_xwing_keypair(&msek);
        assert_eq!(kp1.public, kp2.public);
        assert_eq!(kp1.public.to_bytes().len(), 1216);
    }

    #[test]
    fn xwing_keypair_reuses_the_existing_x25519_recipient_key() {
        // The load-bearing reuse property: the X25519 half of the X-Wing key is
        // the *existing* recipient-mail X25519 key, not a fresh one — so a
        // classical sender and a hybrid sender target the same X25519 public key.
        let msek = [11u8; 32];
        let (_x25519_sk, x25519_pk) = derive_recipient_hpke_keypair(&msek);
        let kp = derive_recipient_xwing_keypair(&msek);
        assert_eq!(kp.public.x25519_public(), &x25519_pk);
    }

    #[test]
    fn distinct_mseks_give_distinct_xwing_keypairs() {
        let kp_a = derive_recipient_xwing_keypair(&[3u8; 32]);
        let kp_b = derive_recipient_xwing_keypair(&[4u8; 32]);
        assert_ne!(kp_a.public, kp_b.public);
        // The ML-KEM halves differ too (not just the X25519 halves).
        assert_ne!(
            kp_a.public.mlkem_encaps_key(),
            kp_b.public.mlkem_encaps_key()
        );
    }

    #[test]
    fn derived_xwing_keypair_round_trips_through_xwing_seal() {
        // The hybrid analogue of `derived_keypair_round_trips_through_mail_seal`:
        // seal to the derived X-Wing public key, open with the derived secret.
        use crate::wrapped_blob::{xwing_open, xwing_seal};
        let msek = [42u8; 32];
        let kp = derive_recipient_xwing_keypair(&msek);
        let info = crate::wrapped_blob::AadBinding::for_mail_record();
        let (enc, ct) = xwing_seal(&kp.public, &info, &info, b"inbound rfc5322 bytes").unwrap();
        assert_eq!(enc.len(), 1120);
        let opened = xwing_open(&kp.secret, &info, &info, &enc, &ct).unwrap();
        assert_eq!(opened, b"inbound rfc5322 bytes");
    }

    #[test]
    fn derived_keypair_round_trips_through_mail_seal() {
        // The whole point: the MTA seals to the derived pubkey, the MDA
        // opens with the derived secret.
        let msek = [42u8; 32];
        let (sk, pk) = derive_recipient_hpke_keypair(&msek);
        let envelope = seal_to_recipient(b"inbound rfc5322 bytes", &pk).unwrap();
        let opened = unseal_mail_record(&envelope, &sk).unwrap();
        assert_eq!(opened, b"inbound rfc5322 bytes");
    }

    #[test]
    fn wrong_msek_secret_fails_to_open() {
        let (_, pk) = derive_recipient_hpke_keypair(&[1u8; 32]);
        let (other_sk, _) = derive_recipient_hpke_keypair(&[2u8; 32]);
        let envelope = seal_to_recipient(b"x", &pk).unwrap();
        assert!(unseal_mail_record(&envelope, &other_sk).is_err());
    }

    // ── snapshot builder ──

    #[test]
    fn snapshot_pubkeys_match_per_msek_derivation() {
        let mseks = [[10u8; 32], [11u8; 32]];
        let snap = build_mls_snapshot_plaintext(&mseks, &[]);
        assert_eq!(snap.v, SNAPSHOT_PLAINTEXT_VERSION);
        assert_eq!(snap.leaf_init_keypairs.len(), 2);
        for (i, kp) in snap.leaf_init_keypairs.iter().enumerate() {
            let (sk, pk) = derive_recipient_hpke_keypair(&mseks[i]);
            assert_eq!(kp.x25519_pubkey.as_slice(), &pk);
            assert_eq!(kp.x25519_secret.as_slice(), &sk);
        }
        // Round-trips through the canonical codec unchanged.
        let bytes = snap.to_canonical_bytes().unwrap();
        let decoded = MlsSnapshotPlaintext::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.leaf_init_keypairs.len(), 2);
    }

    #[test]
    fn snapshot_carries_mlkem_dk_matching_derivation() {
        // S3d: the builder populates the ML-KEM decaps half from the same MSEK
        // and it survives the canonical round-trip at the expected length.
        let mseks = [[10u8; 32], [11u8; 32]];
        let snap = build_mls_snapshot_plaintext(&mseks, &[]);
        let bytes = snap.to_canonical_bytes().unwrap();
        let decoded = MlsSnapshotPlaintext::from_canonical_bytes(&bytes).unwrap();
        for (i, kp) in decoded.leaf_init_keypairs.iter().enumerate() {
            let mdk = kp.mlkem_dk.as_ref().expect("hybrid builder sets mdk");
            assert_eq!(mdk.len(), fauna_pq_kem::MLKEM768_DECAPS_KEY_LEN);
            let expected = derive_recipient_xwing_keypair(&mseks[i]);
            assert_eq!(
                mdk.as_slice(),
                expected.secret.mlkem_decaps_key().as_slice()
            );
        }
    }

    #[test]
    fn snapshot_keypair_opens_hybrid_mail_record() {
        // The MDA path: seal a hybrid (X-Wing) record to the recipient's
        // published ek, then open it with the snapshot's x25519 secret + mdk.
        use crate::wrapped_blob::{seal_to_recipient_xwing, unseal_mail_record_hybrid};
        let msek = [7u8; 32];
        let xwing = derive_recipient_xwing_keypair(&msek);
        let envelope = seal_to_recipient_xwing(b"hybrid inbound bytes", &xwing.public).unwrap();

        let snap = build_mls_snapshot_plaintext(&[msek], &[]);
        let kp = &snap.leaf_init_keypairs[0];
        let x25519_secret: [u8; 32] = kp.x25519_secret.as_slice().try_into().unwrap();
        let mdk: [u8; fauna_pq_kem::MLKEM768_DECAPS_KEY_LEN] =
            kp.mlkem_dk.as_ref().unwrap().as_slice().try_into().unwrap();
        let opened = unseal_mail_record_hybrid(&envelope, &x25519_secret, &mdk).unwrap();
        assert_eq!(opened, b"hybrid inbound bytes");

        // A classical record still opens via the same hybrid path.
        let classical = seal_to_recipient(b"classical too", &x25519_secret_pub(&msek)).unwrap();
        let opened_classical = unseal_mail_record_hybrid(&classical, &x25519_secret, &mdk).unwrap();
        assert_eq!(opened_classical, b"classical too");
    }

    /// Helper: the X25519 public half a classical sender targets (the snapshot's
    /// stored `pk`), so the classical-still-opens assertion seals to the right key.
    fn x25519_secret_pub(msek: &[u8; 32]) -> [u8; 32] {
        derive_recipient_hpke_keypair(msek).1
    }

    #[test]
    fn classical_new_omits_mdk_and_round_trips() {
        // A pre-hybrid entry (`new`) carries no mdk: the field is None and the
        // canonical bytes do not include the "mdk" key (byte-stability with
        // pre-S3d snapshots).
        let (sk, pk) = derive_recipient_hpke_keypair(&[1u8; 32]);
        let snap = MlsSnapshotPlaintext {
            leaf_init_keypairs: vec![LeafInitKeypair::new(pk, sk)],
            ..Default::default()
        };
        assert!(snap.leaf_init_keypairs[0].mlkem_dk.is_none());
        let bytes = snap.to_canonical_bytes().unwrap();
        // "mdk" must not appear as a CBOR text-key (0x63 = text(3) then "mdk").
        assert!(
            !bytes.windows(4).any(|w| w == [0x63, b'm', b'd', b'k']),
            "classical snapshot must omit the mdk map key"
        );
        let decoded = MlsSnapshotPlaintext::from_canonical_bytes(&bytes).unwrap();
        assert!(decoded.leaf_init_keypairs[0].mlkem_dk.is_none());
    }

    #[test]
    fn rejects_wrong_mdk_length() {
        let (sk, pk) = derive_recipient_hpke_keypair(&[1u8; 32]);
        let bad = MlsSnapshotPlaintext {
            leaf_init_keypairs: vec![LeafInitKeypair {
                x25519_pubkey: ByteBuf::from(pk.to_vec()),
                x25519_secret: ByteBuf::from(sk.to_vec()),
                mlkem_dk: Some(ByteBuf::from(vec![0u8; 2399])),
            }],
            ..Default::default()
        };
        let bytes = bad.to_canonical_bytes().unwrap();
        let err = MlsSnapshotPlaintext::from_canonical_bytes(&bytes).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }

    /// **Every generation is carried, uncapped** (`owner-key-material.md`
    /// § Path B-sibling-2 → *Pre-rotation mail at rest*) — the inversion of
    /// the retired cap-3 pin: five MSEKs in history → five leaf keypairs,
    /// and one epoch root, one index-segment key and one retirement instant
    /// per PRIOR generation, all aligned newest first.
    #[test]
    fn snapshot_carries_every_generation_uncapped() {
        let mseks = [[1u8; 32], [2u8; 32], [3u8; 32], [4u8; 32], [5u8; 32]];
        let snap = build_mls_snapshot_plaintext(&mseks, &[400, 300, 200, 100]);
        assert_eq!(snap.leaf_init_keypairs.len(), 5);
        assert_eq!(snap.mail_epoch_grace_roots.len(), 4);
        assert_eq!(snap.index_seg_grace_keys.len(), 4);
        assert_eq!(snap.generation_retired_at_unix, [400, 300, 200, 100]);
        assert_eq!(
            snap.mail_epoch_grace_roots[3].as_slice(),
            derive_mail_epoch_root(&[5u8; 32]).as_slice(),
            "the oldest generation's root rides too"
        );
        // And the oldest generation's standing keypair opens mail sealed to it.
        let (_, oldest_pk) = derive_recipient_hpke_keypair(&[5u8; 32]);
        let sealed = seal_to_recipient(b"sealed four rotations ago", &oldest_pk).unwrap();
        let ring = derive_standing_mail_keypairs(&mseks);
        assert_eq!(
            open_mail_record_standing(&sealed, &ring, &[400, 300, 200, 100], Some(50)).unwrap(),
            b"sealed four rotations ago"
        );
        // Extra instants are ignored; a decoder refuses more instants than priors.
        let extra = build_mls_snapshot_plaintext(&mseks[..2], &[9, 8, 7]);
        assert_eq!(extra.generation_retired_at_unix, [9]);
        let mut bad = extra.clone();
        bad.generation_retired_at_unix = vec![9, 8];
        let bytes = bad.to_canonical_bytes().unwrap();
        assert!(matches!(
            MlsSnapshotPlaintext::from_canonical_bytes(&bytes),
            Err(UnwrapError::InvalidFormat(_))
        ));
    }

    /// **The 5-generation golden pin**: the canonical bytes of a snapshot
    /// carrying five generations with their retirement instants, hashed —
    /// the wire shape the MDA reads (field names, order, the aligned prior
    /// lists, the additive instants). Any change to the derivations or the
    /// layout is a hard failure here.
    #[test]
    fn five_generation_snapshot_golden_bytes() {
        let mseks = [[1u8; 32], [2u8; 32], [3u8; 32], [4u8; 32], [5u8; 32]];
        let snap = build_mls_snapshot_plaintext(
            &mseks,
            &[1_800_000_400, 1_800_000_300, 1_800_000_200, 1_800_000_100],
        );
        let bytes = snap.to_canonical_bytes().unwrap();
        // The full 32-byte hash, as bytes (a hex string literal reads as key
        // material to the publish scan). RE-PINNED 2026-10-23: the snapshot
        // gained the additive `calendar_grace_roots` / `card_grace_roots`
        // (`owner-key-material.md` § Path B-sibling-5 → *Snapshot carriage*),
        // which a five-generation snapshot carries.
        assert_eq!(
            blake3::hash(&bytes).as_bytes(),
            &[
                0x68, 0xfa, 0xa4, 0x87, 0xae, 0xec, 0xbf, 0xf1, 0x44, 0xc0, 0xe5, 0x5e, 0x86, 0x19,
                0x5e, 0x07, 0xca, 0xff, 0xf8, 0x9a, 0xe4, 0x25, 0xa8, 0x7e, 0xd3, 0xca, 0xcf, 0x93,
                0x72, 0x75, 0x83, 0x9b,
            ],
        );
        for needle in [&b"calendar_grace_roots"[..], &b"card_grace_roots"[..]] {
            assert!(bytes.windows(needle.len()).any(|w| w == needle));
        }
        let needle = b"generation_retired_at_unix";
        assert!(bytes.windows(needle.len()).any(|w| w == needle));
        let decoded = MlsSnapshotPlaintext::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.generation_retired_at_unix.len(), 4);
    }

    /// The instants list is additive: omitted when empty, so a no-priors
    /// snapshot — and every snapshot built without instants — keeps the
    /// pre-carriage bytes.
    #[test]
    fn generation_instants_field_is_additive_on_the_wire() {
        let needle = b"generation_retired_at_unix";
        for snap in [
            build_mls_snapshot_plaintext(&[[1u8; 32]], &[7]),
            build_mls_snapshot_plaintext(&[[1u8; 32], [2u8; 32]], &[]),
        ] {
            let bytes = snap.to_canonical_bytes().unwrap();
            assert!(
                !bytes.windows(needle.len()).any(|w| w == needle),
                "an empty instants list must omit the map key"
            );
        }
    }

    /// **The seal-time trial order**: the generation current at the basis
    /// first (one step older per retirement after it), then outward, the
    /// older neighbour first; every generation exactly once; an unknown basis
    /// walks newest first.
    #[test]
    fn generation_trial_order_selects_by_seal_time() {
        // Five generations: current, then priors retired at 400, 300, 200, 100.
        let r = [400, 300, 200, 100];
        assert_eq!(generation_trial_order(5, &r, None), [0, 1, 2, 3, 4]);
        assert_eq!(generation_trial_order(5, &r, Some(500)), [0, 1, 2, 3, 4]);
        assert_eq!(generation_trial_order(5, &r, Some(400)), [0, 1, 2, 3, 4]);
        assert_eq!(generation_trial_order(5, &r, Some(399)), [1, 2, 0, 3, 4]);
        assert_eq!(generation_trial_order(5, &r, Some(250)), [2, 3, 1, 4, 0]);
        assert_eq!(generation_trial_order(5, &r, Some(50)), [4, 3, 2, 1, 0]);
        // Missing instants: count only what is known, still a permutation.
        assert_eq!(generation_trial_order(5, &[400], Some(50)), [1, 2, 0, 3, 4]);
        assert_eq!(generation_trial_order(1, &[], Some(50)), [0]);
        assert!(generation_trial_order(0, &r, Some(50)).is_empty());
    }

    #[test]
    fn grace_keypair_opens_mail_sealed_to_prior_pubkey() {
        // Mail sealed to the OLD pubkey before a hard-revoke must still
        // open via the grace (prior-MSEK) keypair carried in the new
        // snapshot.
        let old_msek = [9u8; 32];
        let new_msek = [8u8; 32];
        let (_, old_pk) = derive_recipient_hpke_keypair(&old_msek);
        let in_flight = seal_to_recipient(b"sealed before rotation", &old_pk).unwrap();

        // Snapshot after hard-revoke: [new, old].
        let snap = build_mls_snapshot_plaintext(&[new_msek, old_msek], &[]);
        let grace_sk: [u8; 32] = snap.leaf_init_keypairs[1]
            .x25519_secret
            .as_slice()
            .try_into()
            .unwrap();
        let opened = unseal_mail_record(&in_flight, &grace_sk).unwrap();
        assert_eq!(opened, b"sealed before rotation");

        // And the new current key cannot open the old-sealed mail.
        let current_sk: [u8; 32] = snap.leaf_init_keypairs[0]
            .x25519_secret
            .as_slice()
            .try_into()
            .unwrap();
        assert!(unseal_mail_record(&in_flight, &current_sk).is_err());
    }

    #[test]
    fn empty_history_yields_empty_snapshot() {
        let snap = build_mls_snapshot_plaintext(&[], &[]);
        assert!(snap.leaf_init_keypairs.is_empty());
        assert!(snap.mail_epoch_grace_roots.is_empty());
        // generate_x25519_keypair is still exercised elsewhere; keep the
        // import meaningful by asserting it differs from a derived one.
        let (_, fresh_pk) = generate_x25519_keypair();
        let (_, derived_pk) = derive_recipient_hpke_keypair(&[5u8; 32]);
        assert_ne!(fresh_pk, derived_pk);
    }

    #[test]
    fn snapshot_carries_epoch_grace_roots_for_priors_only() {
        // The current generation's root re-derives from the session MSEK on
        // demand and is never carried; each PRIOR grace generation carries
        // exactly its derived mail-epoch root (never the MSEK itself), in
        // history order, every prior generation like the leaf list.
        let mseks = [[1u8; 32], [2u8; 32], [3u8; 32], [4u8; 32]];
        let snap = build_mls_snapshot_plaintext(&mseks, &[]);
        assert_eq!(snap.mail_epoch_grace_roots.len(), 3);
        assert_eq!(
            snap.mail_epoch_grace_roots[0].as_slice(),
            derive_mail_epoch_root(&[2u8; 32]).as_slice()
        );
        assert_eq!(
            snap.mail_epoch_grace_roots[1].as_slice(),
            derive_mail_epoch_root(&[3u8; 32]).as_slice()
        );
        // Single-generation history (enable-mail, no rotations yet) → none.
        let single = build_mls_snapshot_plaintext(&[[1u8; 32]], &[]);
        assert!(single.mail_epoch_grace_roots.is_empty());

        // The grace root really opens the old generation's epoch-sealed
        // content: seal to an OLD-root epoch pubkey, open with the secret
        // derived from the carried root — and the NEW generation's key for
        // the same epoch fails (independent roots).
        let e = 2958u64;
        let old_root: [u8; 32] = snap.mail_epoch_grace_roots[0]
            .as_slice()
            .try_into()
            .unwrap();
        let (_, old_epoch_pk) = derive_recipient_epoch_hpke_keypair(&[2u8; 32], e);
        let sealed = seal_to_recipient(b"epoch-sealed before rotation", &old_epoch_pk).unwrap();
        let (grace_sk, _) = derive_recipient_epoch_hpke_keypair_from_root(&old_root, e);
        assert_eq!(
            unseal_mail_record(&sealed, &grace_sk).unwrap(),
            b"epoch-sealed before rotation"
        );
        let (new_sk, _) = derive_recipient_epoch_hpke_keypair(&[1u8; 32], e);
        assert!(unseal_mail_record(&sealed, &new_sk).is_err());
    }

    #[test]
    fn epoch_grace_roots_field_is_additive_on_the_wire() {
        // Omitted-when-empty: a no-priors snapshot stays byte-identical to
        // the pre-epochs shape (no "mail_epoch_grace_roots" CBOR key), and
        // an old-shape encoding decodes with an empty vec.
        let single = build_mls_snapshot_plaintext(&[[1u8; 32]], &[]);
        let bytes = single.to_canonical_bytes().unwrap();
        let needle = b"mail_epoch_grace_roots";
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "empty grace list must omit the map key"
        );
        let decoded = MlsSnapshotPlaintext::from_canonical_bytes(&bytes).unwrap();
        assert!(decoded.mail_epoch_grace_roots.is_empty());

        // Populated → round-trips.
        let multi = build_mls_snapshot_plaintext(&[[1u8; 32], [2u8; 32]], &[]);
        let bytes = multi.to_canonical_bytes().unwrap();
        let decoded = MlsSnapshotPlaintext::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.mail_epoch_grace_roots.len(), 1);
        assert_eq!(
            decoded.mail_epoch_grace_roots[0],
            multi.mail_epoch_grace_roots[0]
        );
    }

    #[test]
    fn rejects_wrong_epoch_grace_root_length() {
        let mut snap = build_mls_snapshot_plaintext(&[[1u8; 32], [2u8; 32]], &[]);
        snap.mail_epoch_grace_roots[0] = ByteBuf::from(vec![0u8; 31]);
        let bytes = snap.to_canonical_bytes().unwrap();
        let err = MlsSnapshotPlaintext::from_canonical_bytes(&bytes).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }

    // ── mail/calendar index-segment key rotation grace (rollout S5) ──
    // `key-material-hierarchy.md` § Path B-sibling-4 → *Snapshot carriage*:
    // the CURRENT generation's key is never carried (it re-derives from the
    // session MSEK); grace rides as one 32-byte derived key per prior grace
    // generation, newest first, aligned with `leaf_init_keypairs[1..]`,
    // omitted when empty — "the `mail_epoch_grace_roots` shape verbatim".

    #[test]
    fn snapshot_carries_index_seg_grace_keys_for_priors_only() {
        let mseks = [[1u8; 32], [2u8; 32], [3u8; 32], [4u8; 32]];
        let snap = build_mls_snapshot_plaintext(&mseks, &[]);
        // Same order as the sibling prior lists, so index i of this list and
        // of `mail_epoch_grace_roots` describe the same generation as
        // `leaf_init_keypairs[i + 1]`.
        assert_eq!(snap.index_seg_grace_keys.len(), 3);
        assert_eq!(
            snap.index_seg_grace_keys.len(),
            snap.mail_epoch_grace_roots.len()
        );
        assert_eq!(
            snap.index_seg_grace_keys[0].as_slice(),
            &*derive_index_segment_key(&[2u8; 32])
        );
        assert_eq!(
            snap.index_seg_grace_keys[1].as_slice(),
            &*derive_index_segment_key(&[3u8; 32])
        );

        // The CURRENT generation's key is never carried — a snapshot leak
        // must not hand over the key the live segments are sealed under.
        let current = derive_index_segment_key(&[1u8; 32]);
        assert!(
            !snap
                .index_seg_grace_keys
                .iter()
                .any(|k| k.as_slice() == *current),
            "the current generation's index-segment key must never ride the snapshot"
        );

        // Single-generation history (enable-mail, no rotations yet) → none.
        let single = build_mls_snapshot_plaintext(&[[1u8; 32]], &[]);
        assert!(single.index_seg_grace_keys.is_empty());
    }

    #[test]
    fn index_seg_grace_keys_field_is_additive_on_the_wire() {
        // Omitted-when-empty: a no-priors snapshot stays byte-identical to
        // the pre-S5 shape (no "index_seg_grace_keys" CBOR key), which is
        // what keeps an older decoder — and every snapshot already at rest —
        // reading unchanged.
        let single = build_mls_snapshot_plaintext(&[[1u8; 32]], &[]);
        let bytes = single.to_canonical_bytes().unwrap();
        let needle = b"index_seg_grace_keys";
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "empty grace list must omit the map key"
        );
        let decoded = MlsSnapshotPlaintext::from_canonical_bytes(&bytes).unwrap();
        assert!(decoded.index_seg_grace_keys.is_empty());

        // Populated → round-trips.
        let multi = build_mls_snapshot_plaintext(&[[1u8; 32], [2u8; 32]], &[]);
        let bytes = multi.to_canonical_bytes().unwrap();
        let decoded = MlsSnapshotPlaintext::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.index_seg_grace_keys.len(), 1);
        assert_eq!(
            decoded.index_seg_grace_keys[0],
            multi.index_seg_grace_keys[0]
        );
    }

    #[test]
    fn rejects_wrong_index_seg_grace_key_length() {
        let mut snap = build_mls_snapshot_plaintext(&[[1u8; 32], [2u8; 32]], &[]);
        snap.index_seg_grace_keys[0] = ByteBuf::from(vec![0u8; 31]);
        let bytes = snap.to_canonical_bytes().unwrap();
        let err = MlsSnapshotPlaintext::from_canonical_bytes(&bytes).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }

    // ── mail content-sealing epochs (B1; design spec 2026-07-18) ──

    #[test]
    fn mail_sealing_epoch_index_math() {
        // Absolute schedule: floor(unix_secs / EPOCH_LEN), boundary-exact.
        assert_eq!(mail_sealing_epoch_of(0), 0);
        assert_eq!(mail_sealing_epoch_of(MAIL_SEALING_EPOCH_SECS - 1), 0);
        assert_eq!(mail_sealing_epoch_of(MAIL_SEALING_EPOCH_SECS), 1);
        // 2026-07-18 ≈ unix 1_789_000_000 lands around weekly epoch 2958.
        assert_eq!(mail_sealing_epoch_of(1_789_000_000), 2958);
    }

    #[test]
    fn epoch_range_is_the_window_time_intersection() {
        // A window strictly inside one epoch → exactly that epoch.
        let e = 3000u64;
        let inside = GrantWindow(
            e * MAIL_SEALING_EPOCH_SECS + 10,
            e * MAIL_SEALING_EPOCH_SECS + 20,
        );
        assert_eq!(mail_epoch_range_for_window(&inside), e..=e);
        // A ~90-day window at weekly epochs spans 13 or 14 indices inclusive.
        let start = e * MAIL_SEALING_EPOCH_SECS + 12_345;
        let window = GrantWindow(start, start + 90 * 24 * 60 * 60);
        let range = mail_epoch_range_for_window(&window);
        let count = range.end() - range.start() + 1;
        assert!((13..=14).contains(&count), "got {count} epochs");
        assert_eq!(*range.start(), e);
    }

    #[test]
    fn epoch_keypair_is_deterministic_and_epoch_separated() {
        // Fleet-consistency per (msek, e); distinct epochs and the standing
        // keypair are all pairwise distinct.
        let msek = [7u8; 32];
        let (sk_a1, pk_a1) = derive_recipient_epoch_hpke_keypair(&msek, 2958);
        let (sk_a2, pk_a2) = derive_recipient_epoch_hpke_keypair(&msek, 2958);
        assert_eq!(sk_a1, sk_a2);
        assert_eq!(pk_a1, pk_a2);
        let (_, pk_next) = derive_recipient_epoch_hpke_keypair(&msek, 2959);
        assert_ne!(pk_a1, pk_next);
        let (_, standing_pk) = derive_recipient_hpke_keypair(&msek);
        assert_ne!(pk_a1, standing_pk);
        // And the root matters: another MSEK, same epoch → different key.
        let (_, other_root_pk) = derive_recipient_epoch_hpke_keypair(&[8u8; 32], 2958);
        assert_ne!(pk_a1, other_root_pk);
    }

    #[test]
    fn epoch_secrets_are_pairwise_independent_not_a_chain() {
        // The forward-bounding requirement (iii): each epoch secret is an
        // independent PRF output of the client-side root — the API offers no
        // epoch→epoch derivation, and no secret equals any function of a
        // neighbor exposed here. Structurally: deriving epoch e requires the
        // MSEK parameter; a holder of wrapped secrets for [a, b] holds no
        // MSEK and can only ever use exactly the epochs it was wrapped.
        // This test pins the independence observable: a contiguous run of
        // epoch secrets contains no repeats and does not contain the next
        // epoch's secret anywhere in its own material.
        let msek = [21u8; 32];
        let secrets: Vec<[u8; 32]> = (100..113)
            .map(|e| derive_recipient_epoch_hpke_keypair(&msek, e).0)
            .collect();
        for (i, a) in secrets.iter().enumerate() {
            for b in secrets.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
        let (next_secret, _) = derive_recipient_epoch_hpke_keypair(&msek, 113);
        assert!(!secrets.contains(&next_secret));
    }

    #[test]
    fn index_segment_key_golden_bytes_pin_the_derive_context() {
        // Golden bytes for the S1 index-segment key derivation
        // (`INDEX_SEGMENT_KEY_DERIVE_CONTEXT`): sealed mail/calendar index
        // slices depend on this being eternal once rollout S3 writes the
        // first real segment. Pinned BEFORE any writer exists, like the
        // format-version scheme itself.
        let msek = [7u8; 32];
        assert_eq!(
            hex::encode(derive_index_segment_key(&msek)),
            "71c95792d54aa81187209c7d0a3432b98956cd1d283f1a0cca149150283ad912",
        );
        // Fleet-consistency + distinctness from the sibling derivations off
        // the same MSEK (rule #3 domain separation is real, not nominal).
        assert_eq!(
            derive_index_segment_key(&msek),
            derive_index_segment_key(&msek)
        );
        assert_ne!(
            *derive_index_segment_key(&msek),
            *derive_mail_epoch_root(&msek)
        );
        assert_ne!(
            derive_index_segment_key(&[8u8; 32]),
            derive_index_segment_key(&msek)
        );
    }

    #[test]
    fn epoch_golden_bytes_pin_both_derive_contexts() {
        // Golden bytes: any change to the epoch contexts, the two-step
        // root ∥ LE64(e) key-material layout, or the underlying KDF chain
        // shows up here as a hard failure. Sealed at-rest mail depends on
        // these being eternal.
        //
        // RE-PINNED ONCE (2026-07-18, Track 1c): the derivation gained the
        // per-generation mail-epoch-root intermediate
        // (`derive_mail_epoch_root`) so MSEK-rotation grace can carry a
        // bounded, least-privilege root per prior generation — structurally
        // impossible under the original one-step `MSEK ∥ LE64(e)` shape.
        // Safe exactly because it happened BEFORE the
        // `MAIL_EPOCH_SEALING_WRITE_DEFAULT` flip: no production content is
        // epoch-sealed, and published schedules self-heal on every
        // reconnect (`refresh_epoch_schedule` republish-upsert). Post-flip,
        // these bytes really are eternal.
        let msek = [7u8; 32];
        assert_eq!(
            hex::encode(derive_mail_epoch_root(&msek)),
            "ac681df776267234d037b53cc9308af8acef9a36a12a44a1c78db35279b04be8",
        );
        let (sk, pk) = derive_recipient_epoch_hpke_keypair(&msek, 2958);
        assert_eq!(
            hex::encode(sk),
            "aaccd63e92ce2e748c5fbaff3268eea2ec7d462f6689e1d2b4cdc5b167a9c3a1",
        );
        assert_eq!(
            hex::encode(pk),
            "77724cf072b6607db8133ecb3d93d9563782f56f71eaa35508aa32369347bd27",
        );
        let xwing = derive_recipient_epoch_xwing_keypair(&msek, 2958);
        let ek = xwing.public.mlkem_encaps_key();
        assert_eq!(
            hex::encode(blake3::hash(ek).as_bytes()),
            "50ece3673859c08a715eec1c05cfeee0b292b094a2c74654106b004acd7a0daf",
        );
        // The two-step split is exact: deriving from the root equals
        // deriving from the MSEK.
        let root = derive_mail_epoch_root(&msek);
        assert_eq!(
            derive_recipient_epoch_hpke_keypair_from_root(&root, 2958),
            (sk, pk)
        );
        assert_eq!(
            derive_recipient_mail_epoch_capability_secret_from_root(&root, 2958),
            derive_recipient_mail_epoch_capability_secret(&msek, 2958)
        );
    }

    #[test]
    fn epoch_xwing_reuses_the_epoch_x25519_half() {
        // The same half-reuse shape as the standing X-Wing keypair, per epoch:
        // classical and hybrid senders target the same per-epoch X25519 key.
        let msek = [11u8; 32];
        let (_, x25519_pk) = derive_recipient_epoch_hpke_keypair(&msek, 500);
        let kp = derive_recipient_epoch_xwing_keypair(&msek, 500);
        assert_eq!(kp.public.x25519_public(), &x25519_pk);
        // ML-KEM halves are epoch-separated too.
        let kp_next = derive_recipient_epoch_xwing_keypair(&msek, 501);
        assert_ne!(
            kp.public.mlkem_encaps_key(),
            kp_next.public.mlkem_encaps_key()
        );
        // And distinct from the standing ML-KEM half.
        let standing = derive_recipient_xwing_keypair(&msek);
        assert_ne!(
            kp.public.mlkem_encaps_key(),
            standing.public.mlkem_encaps_key()
        );
    }

    #[test]
    fn epoch_capability_secret_keeps_the_length_dispatch_contract() {
        // Byte-layout parity with the standing 32+2400 payload, so the
        // holder's open_mail_record_with_key length dispatch is unchanged.
        let msek = [13u8; 32];
        let payload = derive_recipient_mail_epoch_capability_secret(&msek, 2958);
        assert_eq!(payload.len(), 32 + fauna_pq_kem::MLKEM768_DECAPS_KEY_LEN);
        let standing = derive_recipient_mail_capability_secret(&msek);
        assert_eq!(payload.len(), standing.len());
        // The X25519 half is exactly the epoch keypair's secret.
        let (sk, _) = derive_recipient_epoch_hpke_keypair(&msek, 2958);
        assert_eq!(&payload[..32], &sk);
        assert_ne!(&payload[..32], &standing[..32]);
    }

    #[test]
    fn epoch_sealed_record_opens_only_with_its_epoch_key() {
        // The record-level crux of crypto expiry: a record sealed to epoch
        // e's pubkey opens with K_e and FAILS with K_{e±1}, the standing key,
        // and another root's K_e. (Cross-epoch substitution at the *wrap*
        // layer is separately killed by the for_capability AAD.)
        let msek = [17u8; 32];
        let e = 2958u64;
        let (_, pk_e) = derive_recipient_epoch_hpke_keypair(&msek, e);
        let envelope = seal_to_recipient(b"epoch-sealed inbound mail", &pk_e).unwrap();

        let (sk_e, _) = derive_recipient_epoch_hpke_keypair(&msek, e);
        assert_eq!(
            unseal_mail_record(&envelope, &sk_e).unwrap(),
            b"epoch-sealed inbound mail"
        );

        let (sk_prev, _) = derive_recipient_epoch_hpke_keypair(&msek, e - 1);
        let (sk_next, _) = derive_recipient_epoch_hpke_keypair(&msek, e + 1);
        let (sk_standing, _) = derive_recipient_hpke_keypair(&msek);
        let (sk_other_root, _) = derive_recipient_epoch_hpke_keypair(&[18u8; 32], e);
        assert!(unseal_mail_record(&envelope, &sk_prev).is_err());
        assert!(unseal_mail_record(&envelope, &sk_next).is_err());
        assert!(unseal_mail_record(&envelope, &sk_standing).is_err());
        assert!(unseal_mail_record(&envelope, &sk_other_root).is_err());
    }

    #[test]
    fn epoch_hybrid_record_round_trips_and_epoch_separates() {
        // The hybrid analogue: X-Wing-seal to the per-epoch ek, open with the
        // per-epoch capability payload's halves; the neighbor epoch fails.
        use crate::wrapped_blob::{seal_to_recipient_xwing, unseal_mail_record_hybrid};
        let msek = [19u8; 32];
        let e = 3000u64;
        let kp = derive_recipient_epoch_xwing_keypair(&msek, e);
        let envelope = seal_to_recipient_xwing(b"hybrid epoch mail", &kp.public).unwrap();

        let payload = derive_recipient_mail_epoch_capability_secret(&msek, e);
        let x25519_secret: [u8; 32] = payload[..32].try_into().unwrap();
        let mdk: [u8; fauna_pq_kem::MLKEM768_DECAPS_KEY_LEN] = payload[32..].try_into().unwrap();
        assert_eq!(
            unseal_mail_record_hybrid(&envelope, &x25519_secret, &mdk).unwrap(),
            b"hybrid epoch mail"
        );

        let wrong = derive_recipient_mail_epoch_capability_secret(&msek, e + 1);
        let wrong_sk: [u8; 32] = wrong[..32].try_into().unwrap();
        let wrong_mdk: [u8; fauna_pq_kem::MLKEM768_DECAPS_KEY_LEN] =
            wrong[32..].try_into().unwrap();
        assert!(unseal_mail_record_hybrid(&envelope, &wrong_sk, &wrong_mdk).is_err());
    }

    // ── calendar and card keypairs (§ Path B-sibling-5 / -6, 2026-10-23) ──

    /// Golden bytes for every new derivation: the two roots, both keypairs'
    /// X25519 halves, a hash of both ML-KEM encapsulation keys and of both
    /// capability payloads. Calendar and card bodies at rest depend on these
    /// being eternal once the first one is sealed.
    #[test]
    fn calendar_and_card_golden_bytes_pin_every_derive_context() {
        // Expected values as byte arrays: a hex string literal reads as key
        // material to the publish scan.
        let msek = [7u8; 32];
        let cal_root = derive_calendar_root(&msek);
        let card_root = derive_card_root(&msek);
        assert_eq!(
            hex::encode(*cal_root),
            hex::encode([
                0x29, 0x20, 0x32, 0xf2, 0x09, 0x6c, 0xa6, 0x40, 0xe1, 0x46, 0xa0, 0x95, 0x6d, 0xd5,
                0x5f, 0x34, 0x34, 0x03, 0xeb, 0x0b, 0x1e, 0x51, 0x22, 0x1d, 0x62, 0x2b, 0xc4, 0x70,
                0xc9, 0x3c, 0x20, 0xc7,
            ])
        );
        assert_eq!(
            hex::encode(*card_root),
            hex::encode([
                0x19, 0x6c, 0x4f, 0x62, 0x37, 0x1a, 0x52, 0xd7, 0xc9, 0xdd, 0x22, 0xa9, 0xae, 0xc8,
                0xff, 0x3b, 0x87, 0x94, 0x9a, 0x69, 0xce, 0x14, 0x56, 0x7e, 0x7e, 0x6e, 0xcb, 0xea,
                0x60, 0xec, 0xce, 0xda,
            ])
        );
        let (cal_sk, cal_pk) = derive_recipient_calendar_hpke_keypair(&cal_root);
        let (card_sk, card_pk) = derive_recipient_card_hpke_keypair(&card_root);
        assert_eq!(
            hex::encode(cal_sk),
            hex::encode([
                0x27, 0x39, 0x36, 0x71, 0xe7, 0x27, 0x45, 0x6f, 0xe4, 0x93, 0x8b, 0x80, 0x59, 0x54,
                0x38, 0xab, 0xc2, 0xb2, 0xfe, 0xd9, 0x76, 0x41, 0xc9, 0x44, 0x91, 0xd7, 0xcb, 0x60,
                0x34, 0xfb, 0xf6, 0xb4,
            ])
        );
        assert_eq!(
            hex::encode(cal_pk),
            hex::encode([
                0xbf, 0x36, 0x00, 0xd9, 0xd0, 0x22, 0x79, 0x69, 0xb8, 0xa1, 0x5d, 0x02, 0x89, 0x39,
                0xc1, 0x83, 0x0f, 0xce, 0x02, 0xa5, 0xc0, 0x5c, 0xa0, 0x5f, 0x51, 0x74, 0x5f, 0x3e,
                0x73, 0x8f, 0xe6, 0x67,
            ])
        );
        assert_eq!(
            hex::encode(card_sk),
            hex::encode([
                0x9d, 0x30, 0x0f, 0x45, 0xb5, 0x22, 0x5b, 0x8a, 0x96, 0x01, 0x26, 0x52, 0xa5, 0xa4,
                0x05, 0xd4, 0xc3, 0x0d, 0x1a, 0x77, 0xdb, 0xa5, 0x1b, 0x8c, 0x46, 0x03, 0xc1, 0x81,
                0x6a, 0xa1, 0x3c, 0x45,
            ])
        );
        assert_eq!(
            hex::encode(card_pk),
            hex::encode([
                0x05, 0xd9, 0x53, 0x21, 0x8d, 0xc0, 0x52, 0x83, 0x3f, 0xa6, 0xbe, 0x0f, 0x81, 0xf0,
                0x4a, 0x61, 0x39, 0xae, 0x1a, 0x44, 0x38, 0xb5, 0xc8, 0xca, 0xaf, 0xe6, 0x87, 0x50,
                0xae, 0xee, 0x74, 0x46,
            ])
        );
        let cal_ek = derive_recipient_calendar_xwing_keypair(&cal_root);
        let card_ek = derive_recipient_card_xwing_keypair(&card_root);
        assert_eq!(
            hex::encode(blake3::hash(cal_ek.public.mlkem_encaps_key()).as_bytes()),
            hex::encode([
                0x48, 0x48, 0x7b, 0x40, 0x46, 0x30, 0x9f, 0xb2, 0x67, 0x05, 0x5d, 0x73, 0xc0, 0xb3,
                0x73, 0x12, 0x5a, 0x2f, 0x46, 0x5c, 0xa5, 0x1e, 0x62, 0x62, 0xfb, 0x28, 0xda, 0x72,
                0x3e, 0xa9, 0x87, 0x8a,
            ])
        );
        assert_eq!(
            hex::encode(blake3::hash(card_ek.public.mlkem_encaps_key()).as_bytes()),
            hex::encode([
                0x30, 0x21, 0xe2, 0x0b, 0x5c, 0x6f, 0x8a, 0xcb, 0x6e, 0x01, 0xc9, 0x69, 0x27, 0x5f,
                0x13, 0xb7, 0xcb, 0xae, 0xe4, 0x83, 0xc9, 0xd0, 0xf8, 0xf8, 0xd5, 0x19, 0xa4, 0x4d,
                0x1e, 0x8c, 0x60, 0x87,
            ])
        );
        assert_eq!(
            hex::encode(
                blake3::hash(&derive_recipient_calendar_capability_secret(&msek)).as_bytes()
            ),
            hex::encode([
                0x00, 0x66, 0xa7, 0xa3, 0x98, 0x00, 0x86, 0xb3, 0x39, 0x71, 0x25, 0x70, 0xe7, 0x3f,
                0x1a, 0xdc, 0x64, 0xe2, 0xd3, 0x7c, 0x41, 0x75, 0x1a, 0x31, 0xd6, 0x86, 0x08, 0xc4,
                0x04, 0xfb, 0x2d, 0x65,
            ])
        );
        assert_eq!(
            hex::encode(blake3::hash(&derive_recipient_card_capability_secret(&msek)).as_bytes()),
            hex::encode([
                0x37, 0x95, 0x4d, 0x5d, 0xd6, 0x45, 0x2d, 0xae, 0x44, 0x6d, 0xc2, 0x88, 0x94, 0xe5,
                0x0c, 0x36, 0x62, 0x83, 0x58, 0x02, 0x1f, 0x8d, 0xef, 0x40, 0xb4, 0x91, 0xd9, 0x2b,
                0xe2, 0xb2, 0xaa, 0xf6,
            ])
        );
    }

    /// Rule #3's domain separation is real: the calendar, card and mail keys of
    /// one MSEK are pairwise distinct, each X-Wing keypair reuses its own X25519
    /// half, and both payloads keep the mail payload's 2432-byte contract.
    #[test]
    fn dav_keypairs_are_domain_separated_and_keep_the_payload_contract() {
        let msek = [11u8; 32];
        let (_, mail_pk) = derive_recipient_hpke_keypair(&msek);
        let (_, cal_pk) = derive_recipient_calendar_hpke_keypair(&derive_calendar_root(&msek));
        let (_, card_pk) = derive_recipient_card_hpke_keypair(&derive_card_root(&msek));
        assert_ne!(mail_pk, cal_pk);
        assert_ne!(mail_pk, card_pk);
        assert_ne!(cal_pk, card_pk);
        assert_ne!(*derive_calendar_root(&msek), *derive_card_root(&msek));
        assert_ne!(*derive_calendar_root(&msek), *derive_mail_epoch_root(&msek));
        let cal_xwing = derive_recipient_calendar_xwing_keypair(&derive_calendar_root(&msek));
        assert_eq!(cal_xwing.public.x25519_public(), &cal_pk);
        assert_ne!(
            cal_xwing.public.mlkem_encaps_key(),
            derive_recipient_xwing_keypair(&msek)
                .public
                .mlkem_encaps_key()
        );
        let mail = derive_recipient_mail_capability_secret(&msek);
        for payload in [
            derive_recipient_calendar_capability_secret(&msek),
            derive_recipient_card_capability_secret(&msek),
        ] {
            assert_eq!(payload.len(), mail.len());
            assert_ne!(payload[..32], mail[..32]);
        }
        // Fleet-consistent: another derivation of the same MSEK agrees.
        assert_eq!(
            derive_recipient_calendar_capability_secret(&msek),
            derive_recipient_calendar_capability_secret(&msek)
        );
    }

    /// Open a record with a 2432-byte capability payload under `kind`.
    fn open_with_payload(
        kind: RecordKind,
        envelope: &MailRecordEnvelope,
        payload: &[u8],
    ) -> Result<Vec<u8>, UnwrapError> {
        let sk: [u8; 32] = payload[..32].try_into().unwrap();
        let dk: [u8; fauna_pq_kem::MLKEM768_DECAPS_KEY_LEN] = payload[32..].try_into().unwrap();
        crate::wrapped_blob::unseal_record_hybrid(kind, envelope, &sk, &dk)
    }

    /// **The test that pins the design** (`owner-key-material.md` § Path
    /// B-sibling-5 → *Capability payload*): a `content.read{calendar}` payload
    /// opens a `calendar-record` and NOT a `mail-record` or a `card-record`; a
    /// `content.read{mail}` payload opens no `calendar-record`; a card ring
    /// opens no calendar record. Each refusal is tried both ways — under the
    /// record's own kind (the key misses) and under the payload's kind (the
    /// envelope kind refuses) — so neither layer alone carries the guarantee.
    #[test]
    fn a_calendar_payload_opens_calendar_records_and_nothing_else() {
        use crate::wrapped_blob::{seal_record_to_recipient_xwing, seal_to_recipient_xwing};
        let msek = [23u8; 32];
        let cal_pub = derive_recipient_calendar_xwing_keypair(&derive_calendar_root(&msek)).public;
        let card_pub = derive_recipient_card_xwing_keypair(&derive_card_root(&msek)).public;
        let mail_pub = derive_recipient_xwing_keypair(&msek).public;
        let calendar_record =
            seal_record_to_recipient_xwing(RecordKind::Calendar, b"VEVENT", &cal_pub).unwrap();
        let card_record =
            seal_record_to_recipient_xwing(RecordKind::Card, b"VCARD", &card_pub).unwrap();
        let mail_record = seal_to_recipient_xwing(b"RFC 5322", &mail_pub).unwrap();

        let calendar_payload = derive_recipient_calendar_capability_secret(&msek);
        let mail_payload = derive_recipient_mail_capability_secret(&msek);

        assert_eq!(
            open_with_payload(RecordKind::Calendar, &calendar_record, &calendar_payload).unwrap(),
            b"VEVENT"
        );
        for kind in [RecordKind::Mail, RecordKind::Calendar] {
            assert!(open_with_payload(kind, &mail_record, &calendar_payload).is_err());
        }
        for kind in [RecordKind::Card, RecordKind::Calendar] {
            assert!(open_with_payload(kind, &card_record, &calendar_payload).is_err());
        }
        for kind in [RecordKind::Calendar, RecordKind::Mail] {
            assert!(open_with_payload(kind, &calendar_record, &mail_payload).is_err());
        }
        assert_eq!(
            open_with_payload(RecordKind::Mail, &mail_record, &mail_payload).unwrap(),
            b"RFC 5322"
        );
        // The mail opener family refuses the calendar record by kind.
        let mail_ring = derive_standing_mail_keypairs(&[msek]);
        assert!(open_mail_record_standing(&calendar_record, &mail_ring, &[], None).is_none());
        // A card ring opens no calendar record; a calendar ring opens its own.
        let card_ring = StandingDavRing::from_mseks(DavKind::Card, &msek, &[]);
        assert!(matches!(
            card_ring.open(&calendar_record),
            Err(UnwrapError::InvalidFormat(_))
        ));
        assert_eq!(card_ring.open(&card_record).unwrap(), b"VCARD");
        let cal_ring = StandingDavRing::from_mseks(DavKind::Calendar, &msek, &[]);
        assert_eq!(cal_ring.open(&calendar_record).unwrap(), b"VEVENT");
        assert!(cal_ring.open(&mail_record).is_err());
    }

    /// The snapshot carries one calendar and one card root per PRIOR
    /// generation (never the current one), aligned with the leaf list; the
    /// MDA's ring built from the session MSEK + those roots equals the apps'
    /// ring from the MSEK history — a calendar body sealed two rotations ago
    /// opens through both.
    #[test]
    fn snapshot_carries_dav_grace_roots_and_both_rings_agree() {
        let mseks = [[1u8; 32], [2u8; 32], [3u8; 32]];
        let snap = build_mls_snapshot_plaintext(&mseks, &[300, 200]);
        assert_eq!(snap.calendar_grace_roots.len(), 2);
        assert_eq!(snap.card_grace_roots.len(), 2);
        assert_eq!(
            snap.calendar_grace_roots[1].as_slice(),
            derive_calendar_root(&[3u8; 32]).as_slice()
        );
        assert_eq!(
            snap.card_grace_roots[0].as_slice(),
            derive_card_root(&[2u8; 32]).as_slice()
        );
        let current_cal = derive_calendar_root(&[1u8; 32]);
        assert!(
            !snap
                .calendar_grace_roots
                .iter()
                .any(|r| r.as_slice() == current_cal.as_slice()),
            "the current generation's calendar root must never ride the snapshot"
        );

        let bytes = snap.to_canonical_bytes().unwrap();
        let decoded = MlsSnapshotPlaintext::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.calendar_grace_roots, snap.calendar_grace_roots);
        assert_eq!(decoded.card_grace_roots, snap.card_grace_roots);

        let old_pub =
            derive_recipient_calendar_xwing_keypair(&derive_calendar_root(&[3u8; 32])).public;
        let sealed = crate::wrapped_blob::seal_record_to_recipient_xwing(
            RecordKind::Calendar,
            b"sealed two rotations ago",
            &old_pub,
        )
        .unwrap();
        let mda = StandingDavRing::from_msek_and_grace_roots(
            DavKind::Calendar,
            &mseks[0],
            &decoded.dav_grace_roots(DavKind::Calendar),
        );
        let apps = StandingDavRing::from_mseks(DavKind::Calendar, &mseks[0], &mseks[1..]);
        assert_eq!(mda.len(), 3);
        for ring in [&mda, &apps] {
            assert_eq!(ring.open(&sealed).unwrap(), b"sealed two rotations ago");
        }
        // The card grace list is the card ring's, not the calendar's.
        let card_mda = StandingDavRing::from_msek_and_grace_roots(
            DavKind::Card,
            &mseks[0],
            &decoded.dav_grace_roots(DavKind::Card),
        );
        assert!(card_mda.open(&sealed).is_err());
    }

    /// Both DAV grace lists are additive: omitted when empty, so a
    /// single-generation snapshot keeps its pre-split bytes; a wrong-length
    /// root is refused.
    #[test]
    fn dav_grace_root_fields_are_additive_and_length_checked() {
        let single = build_mls_snapshot_plaintext(&[[1u8; 32]], &[]);
        let bytes = single.to_canonical_bytes().unwrap();
        for needle in [&b"calendar_grace_roots"[..], &b"card_grace_roots"[..]] {
            assert!(!bytes.windows(needle.len()).any(|w| w == needle));
        }
        for card in [false, true] {
            let mut snap = build_mls_snapshot_plaintext(&[[1u8; 32], [2u8; 32]], &[]);
            let list = if card {
                &mut snap.card_grace_roots
            } else {
                &mut snap.calendar_grace_roots
            };
            list[0] = ByteBuf::from(vec![0u8; 31]);
            let bytes = snap.to_canonical_bytes().unwrap();
            assert!(matches!(
                MlsSnapshotPlaintext::from_canonical_bytes(&bytes),
                Err(UnwrapError::InvalidFormat(_))
            ));
        }
    }

    /// The master-key mail payload's byte layout (`capability-grants.md`
    /// § Capability tiering → *What a master-key mail wrap conveys*): per
    /// generation, newest first, the standing secret then the mail-epoch root
    /// — golden-pinned so neither the layout nor a derive context drifts.
    #[test]
    fn master_mail_payload_golden_bytes_for_two_generations() {
        let new = [0x5Au8; 32];
        let old = [0xA5u8; 32];
        let payload = derive_master_mail_capability_payload(&new, &[old]);
        assert_eq!(MASTER_MAIL_CHUNK_LEN, 2464);
        assert_eq!(payload.len(), 2 * MASTER_MAIL_CHUNK_LEN);
        let mut expected = Vec::new();
        for g in [&new, &old] {
            expected.extend_from_slice(&derive_recipient_mail_capability_secret(g));
            expected.extend_from_slice(&*derive_mail_epoch_root(g));
        }
        assert_eq!(payload.as_slice(), expected.as_slice());
        assert_eq!(
            hex::encode(blake3::hash(&payload).as_bytes()),
            "fab4771eb38737092c0d9506a64367ddf08dd2030098bd80cd1e1b8ca6be7c11"
        );
    }
}
