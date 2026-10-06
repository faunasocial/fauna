//! Content-key generations for cross-user **shared** folders — the **M2**
//! mechanism (`docs/goal/architecture/key-material-hierarchy.md` § Audience: an
//! MLS group at a specific epoch).
//!
//! ## Why a separate content key, not the raw epoch secret
//!
//! A shared folder is *persistent collaborative storage*: a member joining at
//! MLS epoch *N* must still read the existing files (sealed before they joined —
//! **history-on-join**), and a *removed* member must lose access to **future**
//! content (**rotate-on-removal** forward secrecy). MLS's exported epoch secret
//! gives the second but not the first: it rotates on **every** commit (add *and*
//! remove), so a key derived directly from it can't open a chunk sealed at an
//! earlier epoch. M2 therefore distributes a separate **per-set content key**
//! over the MLS group — the group is a key-*distribution* channel, and the
//! content key is the `chunk_crypto` root. The content key changes only on a
//! **removal** (a "generation"), not on every add, which is the M2 simplicity
//! win over the epoch-direct M1 alternative (one key per epoch-*range*, not one
//! per epoch).
//!
//! This module is the **pure custody core**: the generation history + the
//! transitions (genesis at bind, rotate-on-removal, generation lookup on read).
//! It holds no I/O, no clock, and no network — the caller supplies `now` and
//! the fresh key bytes. It is the direct analog of the subscriptions period-key
//! custody (`SubscriptionsConfig` / `TierPeriodKeys` / `TierPeriod` in
//! [`crate::data`]; `key-material-hierarchy.md` § Audience: an opaque set of
//! subscriber pubkeys), which solves the isomorphic problem (a 32-byte content
//! key, rotated on membership change, with a full retained history so new
//! members can be granted the back-catalogue). The shapes are deliberately
//! mirrored (priority #3 — same concepts everywhere); only the distribution
//! channel differs (MLS group here vs. per-subscriber X25519 `KeyBlob` wraps
//! there).
//!
//! ## Custody and distribution (the surrounding mechanism this core serves)
//!
//! - **Owner** (the set's binder, the sole writer in Slices 2–3) generates each
//!   generation and holds the authoritative [`FolderContentKeys`] as owner-only
//!   state, `BackupKey`-sealed on the account plane (`fauna.state.folder-keys`) and synced across their device
//!   fleet (the `SubscriptionsConfig` audience + seal).
//! - **Members** receive the generation bundle ([`FolderContentKeys::generations`])
//!   via the MLS group — the owner seals it under the group's *current* epoch
//!   secret into a per-group **content-key envelope**, re-published on every
//!   membership change. A new joiner (Welcome → current epoch secret) decrypts
//!   the envelope and obtains **all** generations (history-on-join). A removed
//!   member, after the Remove commit advances the epoch, can no longer read the
//!   re-sealed envelope (MLS forward secrecy) and so never receives the
//!   post-removal generation — the crypto-layer basis of rotate-on-removal that
//!   `key-material-hierarchy.md` § OBS-1 says the guarantee rests on (never the
//!   nest roster).
//! - **Reads** select the generation a snapshot was sealed under by its stamped
//!   [`ContentKeyGeneration::version`]; [`FolderContentKeys::key_for`] resolves
//!   it, and a caller who lacks that generation **fails closed** (it does not
//!   silently fall through to a plaintext/owner-key read — the FS-BIND-5
//!   posture).
//!
//! The envelope seal/unseal (in `fauna-mls`, alongside `seal_conversation_blob`),
//! the account-plane at-rest wiring, the snapshot version-stamp + the
//! generation-aware `chunk_root`, and the crash-staged rotation orchestration are
//! the remaining Slice-3 pieces built on this core.

use crate::secret::SecretArray32;
use serde::{Deserialize, Serialize};

/// One content-key generation for a shared folder.
///
/// Mirrors [`crate::data::TierPeriod`]: a monotonic `version`, the raw 32-byte
/// key, and a strictly-increasing `rotated_at`. The `key` is the opaque
/// `chunk_crypto` root — fed verbatim to
/// [`crate::chunk_crypto::encrypt_chunk`] / `decrypt_chunk`, which derive the
/// per-chunk key+nonce from it and the chunk's content hash. Held as
/// [`SecretArray32`] (zeroize-on-drop + redacted `Debug`; wire-identical to a
/// bare `[u8; 32]`, so the plane custody and the sealed group content-key
/// envelope are unchanged at rest) — like `TierPeriod::key` and `DeploymentSeedEntry::seed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentKeyGeneration {
    /// Generation number: `1` at bind, `+1` on each rotate-on-removal. Stamped
    /// into every snapshot sealed under this generation so a reader can select
    /// the right key.
    pub version: u64,
    /// The 32-byte content key — the `chunk_crypto` root for chunks sealed under
    /// this generation.
    pub key: SecretArray32,
    /// Microseconds since the Unix epoch. Strictly increases across generations
    /// (enforced by [`FolderContentKeys::rotate`]) so generation order is
    /// total even under clock skew — the same monotonicity `TierPeriod::rotated_at`
    /// carries for the nest's `stale_rotation` check.
    pub rotated_at: u64,
}

/// The content-key history for one shared folder, held by each member (owner or
/// reader) who can decrypt the set.
///
/// `current` is the generation new uploads seal under; `prior` retains **every**
/// rotated-out generation, most-recent first, **uncapped** — history-on-join
/// (FS-NUANCE) distributes *all* generations to a new joiner, so a member must
/// keep the full back-catalogue to read pre-rotation files. (Same uncapped
/// rationale as `TierPeriodKeys::prior`, and unlike `MailConfig::prior_mseks`'s
/// cap-2.) Invariant: `current.version` is the maximum. Versions are unique in
/// the single-writer flow, but a concurrent-rotation [`Self::merge`] (two owner
/// devices rotating to the same version with different keys; also
/// `migrate_set_identity`'s target-exists branch on a serve+share race) can
/// retain two distinct-keyed generations at ONE version — and chunks may exist
/// stamped with that version under EITHER key, so open paths must consult
/// [`Self::keys_for`] (all candidates; the AEAD tag disambiguates), never a
/// single version→key mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderContentKeys {
    /// The active generation — what new uploads seal under and what the envelope
    /// advertises as current.
    pub current: ContentKeyGeneration,
    /// Rotated-out generations, most-recent first. Empty until the first removal.
    #[serde(default)]
    pub prior: Vec<ContentKeyGeneration>,
}

impl ContentKeyGeneration {
    /// Deterministic ordering key for picking the surviving `current` on a
    /// concurrent-rotation conflict (two owner devices rotating at once): newer
    /// version first, then later `rotated_at`, then larger key bytes as a final
    /// total-order tiebreaker. Mirrors [`crate::data::TierPeriod::order`].
    fn order(&self) -> (u64, u64, [u8; 32]) {
        // The transient key copy exists only for the comparison (`Ord` needs an
        // owned tuple) — same exposure class as feeding the key to a crypto op.
        (self.version, self.rotated_at, self.key.to_array())
    }
}

impl FolderContentKeys {
    /// Create generation 1 for a set at bind time (`fauna.folders.share`).
    pub fn genesis(key: [u8; 32], now: u64) -> Self {
        Self {
            current: ContentKeyGeneration {
                version: 1,
                key: key.into(),
                rotated_at: now,
            },
            prior: Vec::new(),
        }
    }

    /// Rotate to a fresh generation on member **removal**: the previous `current`
    /// moves to the front of `prior` (kept readable for pre-rotation content),
    /// and a new `current` is installed with `version + 1`.
    ///
    /// `rotated_at` is forced strictly greater than the previous generation's
    /// (`now.max(prev + 1)`) so the total order across generations holds even if
    /// `now` regressed (clock skew / a stale device). Returns the new current
    /// version.
    pub fn rotate(&mut self, new_key: [u8; 32], now: u64) -> u64 {
        let prev = self.current.clone();
        let next_version = prev.version + 1;
        let rotated_at = now.max(prev.rotated_at.saturating_add(1));
        self.prior.insert(0, prev);
        self.current = ContentKeyGeneration {
            version: next_version,
            key: new_key.into(),
            rotated_at,
        };
        next_version
    }

    /// The **first** candidate key for a generation `version` (current-first), or
    /// `None` if this holder does not have it (e.g. a removed member asked for a
    /// post-removal version, or a generation older than this member was ever
    /// granted). Callers MUST fail closed on `None` — never fall back to another
    /// key.
    ///
    /// ⚠ Open paths must use [`Self::keys_for`] instead: after a concurrent-
    /// rotation [`Self::merge`] two distinct keys can share one version, and this
    /// single-candidate lookup silently shadows the
    /// loser. This remains for single-candidate
    /// contexts (tests, "does this holder have the generation at all" checks).
    pub fn key_for(&self, version: u64) -> Option<&[u8; 32]> {
        self.keys_for(version).next()
    }

    /// **All** candidate keys for a generation `version`, current-first then
    /// `prior` in most-recent-first order.
    ///
    /// Normally a single candidate — but a concurrent-rotation [`Self::merge`]
    /// (two owner devices rotating to the same version with different keys, or
    /// `migrate_set_identity`'s target-exists branch on a serve+share race)
    /// retains BOTH same-version generations, and chunks can exist stamped with
    /// that version under EITHER key. No single version→key mapping can serve
    /// both, so an open path must try each candidate — the AEAD tag
    /// disambiguates, and both keys are honest generation members (retained by
    /// the no-generation-lost merge). An empty iterator means this holder lacks
    /// the generation entirely — fail closed, never fall back to another
    /// generation.
    pub fn keys_for(&self, version: u64) -> impl Iterator<Item = &[u8; 32]> {
        std::iter::once(&self.current)
            .chain(self.prior.iter())
            .filter(move |g| g.version == version)
            .map(|g| &*g.key)
    }

    /// The current generation's version — stamp this into a freshly sealed
    /// snapshot.
    pub fn current_version(&self) -> u64 {
        self.current.version
    }

    /// The current generation's content key — the root new uploads seal under.
    pub fn current_key(&self) -> &[u8; 32] {
        &self.current.key
    }

    /// All generations this holder has, most-recent first (`current` then
    /// `prior`). This is the bundle the owner seals into the group content-key
    /// envelope; a new joiner reconstructs a [`FolderContentKeys`] from it via
    /// [`Self::from_generations`].
    pub fn generations(&self) -> impl Iterator<Item = &ContentKeyGeneration> {
        std::iter::once(&self.current).chain(self.prior.iter())
    }

    /// Merge two holders' content-key custody for the **same** set without
    /// dropping any generation — the CRDT merge two of the owner's devices
    /// converge under (a removal's fresh key is *irrecoverable*, no-user-data-loss).
    /// The deterministically-higher `current` (by `version`, then `rotated_at`,
    /// then key bytes) wins; every distinct generation from both sides (the losing
    /// `current` plus both `prior` lists) is retained in `prior`, most-recent
    /// first, deduplicated. Deterministic + commutative → both devices converge
    /// regardless of merge order. The direct analog of [`crate::data::TierPeriodKeys::merge`].
    pub fn merge(&self, other: &Self) -> Self {
        let (current, loser) = if self.current.order() >= other.current.order() {
            (self.current.clone(), other.current.clone())
        } else {
            (other.current.clone(), self.current.clone())
        };
        let mut prior: Vec<ContentKeyGeneration> = Vec::new();
        for g in self
            .prior
            .iter()
            .chain(other.prior.iter())
            .chain(std::iter::once(&loser))
        {
            if *g != current && !prior.contains(g) {
                prior.push(g.clone());
            }
        }
        // Most-recent first (version desc, then rotated_at desc) — same
        // convention `rotate` maintains.
        prior.sort_by_key(|x| std::cmp::Reverse(x.order()));
        Self { current, prior }
    }

    /// Reconstruct from an unordered bundle of generations (the member side: the
    /// generations decrypted out of the group content-key envelope). Orders the
    /// bundle exactly as [`Self::merge`] does — `current` is the highest by
    /// `(version, rotated_at, key)`, the rest are `prior`, most-recent first —
    /// so the member lands on the owner's own history whatever the wire order.
    ///
    /// Two generations of ONE version are accepted: that is what the owner's
    /// custody holds after two of its devices rotated at once, and the
    /// envelope carries it verbatim (`mls-group-key-material.md` § M2 →
    /// *Generations*, same-version candidates). The pick among them is the
    /// merge's total order, never arbitrary; a repeated generation folds away.
    /// Errors only on an empty bundle.
    pub fn from_generations(mut generations: Vec<ContentKeyGeneration>) -> anyhow::Result<Self> {
        if generations.is_empty() {
            anyhow::bail!("folder content-key bundle is empty");
        }
        // Descending by the merge's order: index 0 is the max → current.
        generations.sort_by_key(|g| std::cmp::Reverse(g.order()));
        generations.dedup();
        let current = generations.remove(0);
        Ok(Self {
            current,
            prior: generations,
        })
    }

    /// The value of the WebDAV bearer door's keys header
    /// ([`FOLDER_KEYS_HEADER`]): this bundle's canonical CBOR, base64url
    /// without padding (`webdav-server.md` § Key model → *A principal's read*,
    /// (3)). What a principal sends beside `Authorization: DPoP` for the one
    /// set its request names.
    pub fn to_header_value(&self) -> anyhow::Result<String> {
        use base64::Engine as _;
        let bytes = zeroize::Zeroizing::new(crate::encoding::canonical_encode(self)?);
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes.as_slice()))
    }

    /// Parse the keys header [`Self::to_header_value`] writes — the MDA's side,
    /// so the Go bridge never decodes CBOR itself. Strict canonical decode,
    /// bounded by [`MAX_FOLDER_KEYS_HEADER_LEN`], and normalised through
    /// [`Self::from_generations`]: a sender's `current`/`prior` order is never
    /// trusted, the merge's total order picks `current`. Refuses anything
    /// else; the error never echoes the value.
    pub fn from_header_value(value: &str) -> anyhow::Result<Self> {
        use base64::Engine as _;
        if value.len() > MAX_FOLDER_KEYS_HEADER_LEN {
            anyhow::bail!(
                "folder keys header is {} bytes, over the {MAX_FOLDER_KEYS_HEADER_LEN}-byte cap",
                value.len()
            );
        }
        let bytes = zeroize::Zeroizing::new(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(value.trim())
                .map_err(|_| anyhow::anyhow!("folder keys header is not base64url"))?,
        );
        let keys: Self = crate::encoding::canonical_decode(&bytes)
            .map_err(|_| anyhow::anyhow!("folder keys header is not a canonical key bundle"))?;
        Self::from_generations(keys.generations().cloned().collect())
    }
}

/// The request header a principal's WebDAV bearer request carries its set's
/// content keys in (`webdav-server.md` § Key model → *A principal's read*, (3)).
pub const FOLDER_KEYS_HEADER: &str = "Fauna-Folder-Keys";

/// The longest keys header [`FolderContentKeys::from_header_value`] parses —
/// 64 KiB of base64url, several hundred generations, far past any set's real
/// rotation history; the cap only bounds the decode.
pub const MAX_FOLDER_KEYS_HEADER_LEN: usize = 64 * 1024;

/// One retired nonce of a set's **lineage**, with the identity that minted it
/// (`writer-signed-change-records.md` ruling (11)(b)) — what a reader tries a
/// row under once the live nonce failed, and what tells history from a plant.
/// `minted_by: None` where none was recorded (it reads as the earliest identity
/// in the owner's chain).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetiredSetNonce {
    #[serde(with = "serde_bytes")]
    pub nonce: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minted_by: Option<crate::identity::ActorId>,
}

/// A set's binding as a reader holds it: its nonces (ruling (11)(b)) — the
/// live nonce with its minter, and the lineage of retired ones, newest first
/// — and its **serve window** (ruling (7)(b)(ii) rules (2) + (4)), the
/// owner's two stamps off the custody entry the nonce was read from. What the
/// owner's custody answers ([`FolderKeyResolver::set_lineage`]) and the
/// envelope carries to a member.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetNonceLineage {
    pub live: Option<[u8; 32]>,
    pub live_minted_by: Option<crate::identity::ActorId>,
    pub retired: Vec<RetiredSetNonce>,
    /// The owner's serve-on stamp for the set, `None` when custody holds none
    /// or the set is not content-keyed.
    pub served_at: Option<u64>,
    /// The owner's serve-off stamp, beside [`Self::served_at`].
    pub unserved_at: Option<u64>,
}

impl SetNonceLineage {
    /// Whether custody calls the set WebDAV-served ([`serve_window_open`]) —
    /// the reader's exemption for the owner's pseudo-device rows.
    #[must_use]
    pub fn webdav_served(&self) -> bool {
        serve_window_open(self.served_at, self.unserved_at)
    }
}

/// Whether a serve window is open: the serve-on stamp is present and stands
/// strictly above the serve-off (`writer-signed-change-records.md` ruling
/// (7)(b)(ii) rule (1)) — a tie reads NOT served. The stamp half of
/// [`crate::data::FolderKeyCustody::is_served`], shared with the copies of the
/// pair that travel ([`FolderEngineKeys`]).
#[must_use]
pub fn serve_window_open(served_at: Option<u64>, unserved_at: Option<u64>) -> bool {
    match (served_at, unserved_at) {
        (Some(_), None) => true,
        (Some(on), Some(off)) => on > off,
        (None, _) => false,
    }
}

/// The plaintext the owner seals into a shared set's content-key envelope
/// (`MlsEngine::seal_content_key_envelope`) — the generation history plus the
/// set's nonce, the binding every writer-signed change record covers
/// (`mls-group-key-material.md` § M2 → *Custody shape of the set nonce*, (h)):
/// sealed under the group epoch, so the nest can neither read nor forge it,
/// and a member's ingest takes the nonce over its own copy — forward only, and
/// only under the current owner's signature over the sealed blob
/// (`writer-signed-change-records.md` ruling (11)(b)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentKeyEnvelopePayload {
    /// The full generation history (history-on-join).
    pub keys: FolderContentKeys,
    /// The set's live nonce, `None` only from an owner whose custody entry
    /// has none (a nonce-less entry the reconcile has not repaired yet).
    pub set_nonce: Option<[u8; 32]>,
    /// The identity whose device minted [`Self::set_nonce`] (ruling (11)(b)).
    pub minted_by: Option<crate::identity::ActorId>,
    /// The set's lineage with its siblings, newest first (ruling (11)(b)) —
    /// what a member verifies rows written before a cut under, so a member's
    /// retired list is no longer empty by construction.
    pub retired_set_nonces: Vec<RetiredSetNonce>,
    /// The owner's serve-on stamp for the set (ruling (7)(b)(ii) rule (4)) —
    /// a member's ingest joins it into its own entry, to the later, so a
    /// replayed envelope cannot move the window.
    pub served_at: Option<u64>,
    /// The owner's serve-off stamp, joined as [`Self::served_at`] is.
    pub unserved_at: Option<u64>,
}

/// The payload's wire shape: a map, so later fields stay additive.
#[derive(Serialize, Deserialize)]
struct ContentKeyEnvelopeWire {
    generations: Vec<ContentKeyGeneration>,
    #[serde(default)]
    #[serde(with = "serde_bytes")]
    set_nonce: Option<[u8; 32]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    minted_by: Option<crate::identity::ActorId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retired_set_nonces: Vec<RetiredSetNonce>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    served_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unserved_at: Option<u64>,
}

impl ContentKeyEnvelopePayload {
    /// Canonical encoding of the sealed plaintext.
    pub fn encode(&self) -> anyhow::Result<Vec<u8>> {
        Ok(crate::encoding::canonical_encode(
            &ContentKeyEnvelopeWire {
                generations: self.keys.generations().cloned().collect(),
                set_nonce: self.set_nonce,
                minted_by: self.minted_by,
                retired_set_nonces: self.retired_set_nonces.clone(),
                served_at: self.served_at,
                unserved_at: self.unserved_at,
            },
        )?)
    }

    /// Decode a sealed plaintext: the map form, or the bare generation array
    /// an envelope published before the nonce carries (read as `set_nonce:
    /// None` — the owner's next re-publish carries it).
    pub fn decode(bytes: &[u8]) -> anyhow::Result<Self> {
        let wire = match crate::encoding::canonical_decode::<ContentKeyEnvelopeWire>(bytes) {
            Ok(wire) => wire,
            Err(_) => ContentKeyEnvelopeWire {
                generations: crate::encoding::canonical_decode::<Vec<ContentKeyGeneration>>(bytes)?,
                set_nonce: None,
                minted_by: None,
                retired_set_nonces: Vec::new(),
                served_at: None,
                unserved_at: None,
            },
        };
        Ok(Self {
            keys: FolderContentKeys::from_generations(wire.generations)?,
            set_nonce: wire.set_nonce,
            minted_by: wire.minted_by,
            retired_set_nonces: wire.retired_set_nonces,
            served_at: wire.served_at,
            unserved_at: wire.unserved_at,
        })
    }
}

/// The custody/sentinel key for a **WebDAV-served set with no MLS group** — the
/// channel-optional extension of `FolderKeyCustody` keying ratified in
/// `docs/goal/behavior/webdav-server.md` § Key model (custody note).
///
/// A shared set is keyed in the `fauna.state.folder-keys` custody by its derived
/// `ChannelId` (`blake3::derive_key("fauna.channel.v1", mls_group_id)` in
/// `fauna-mls`). A *served-but-unshared* set has content keys but no MLS group,
/// so it is keyed by this **pseudo-channel** instead: the same `[u8; 32]` shape
/// (no at-rest schema change — every reader decodes and union-merges the entry
/// unchanged), derived from the set's
/// **name** (its stable nest address — `(name, owner)`; no rename kind exists)
/// under a distinct BLAKE3 context, so it can never collide with a real derived
/// `ChannelId`.
///
/// When the owner later *shares* a served set, `bind_set` migrates the custody
/// entry from this pseudo-channel to the real
/// derived `ChannelId`, preserving every generation so already-stamped
/// `content_key_version`s stay resolvable and the group envelope carries the
/// served-era back-catalogue (history-on-join).
///
/// The context string is **cryptographically frozen**: the derived ids key
/// at-rest `fauna.state.folder-keys` custody entries, so renaming the tag would
/// silently re-derive every served set's pseudo-channel and orphan its custody
/// entry. Never rename it (`version-compatibility.md` § Dimension 1).
#[must_use]
pub fn serve_custody_channel_id(set_name: &str) -> [u8; 32] {
    blake3::derive_key("fauna.folders.serve.v1", set_name.as_bytes())
}

/// The channel id an MLS group id derives — the key a **shared** set's content
/// keys rest under in `fauna.state.folder-keys` custody, and a cross-nest set's
/// [`FolderRef::Foreign`] identity. `blake3::derive_key("fauna.channel.v1",
/// group_id)`: the ONE derivation, which `fauna-mls`'s `ChannelId::from_group_id`
/// delegates to, lifted here so a process that reads custody without linking MLS
/// (the desktop sync agent resolving its own content keys) derives the identical
/// id. The context string is cryptographically frozen — every channel id and
/// every custody entry keyed by one depends on it.
#[must_use]
pub fn channel_id_for_group(group_id: &[u8]) -> [u8; 32] {
    blake3::derive_key("fauna.channel.v1", group_id)
}

/// A folder's **stable, holder-local identity** — what the sync-agent seam
/// addresses a set by, in place of its *name*.
///
/// ## Why the name is not enough
///
/// The nest enforces only `UNIQUE(name, actor_id)`, so names are unique **per
/// owner**. A caller who owns "docs" *and* is a member of someone else's "docs"
/// has two sets with one name: the pushed [`FolderEngineKeys`] batch carries two
/// same-named entries, a name-keyed `.find` takes whichever comes first, and the
/// engine for one set is keyed off the *other's* content keys — a silent
/// wrong-key path (uploads seal under a key the set's members cannot open).
///
/// ## Why these two arms
///
/// The identity has to satisfy three properties at once, and only this split
/// does:
///
/// 1. **It spans every set a holder can bind** — owner-only, owner-shared,
///    WebDAV-served, same-nest *member*, and cross-nest. The nest row id alone
///    fails the last one: a cross-nest set has **no row on this holder's nest at
///    all** (it exists only in their `fauna.state.folder-keys` `ForeignFolder` row).
/// 2. **It is stable across the set's lifecycle.** A set that gains an MLS group
///    (becomes shared) or has its WebDAV serve flag flipped keeps its identity,
///    so an existing folder binding survives. Anything derived from the *name* or
///    from the custody channel of an owner-only set would change under it.
/// 3. **It is available at the moment of binding**, from what the binding UI
///    already holds — a `FolderSummary` for a same-nest set, a `ForeignFolder`
///    for a cross-nest one.
///
/// The two arms are **structurally disjoint**: [`Local`](Self::Local) is a row on
/// this holder's own nest, [`Foreign`](Self::Foreign) is a set with no such row.
/// One cannot masquerade as the other, so no tie-break rule is needed — unlike
/// the name, which needs one and has no correct one to give.
///
/// ## Wire form
///
/// [`to_wire`](Self::to_wire) / [`parse`](Self::parse) render one opaque
/// `String`, because this value crosses five encodings that share no richer
/// vocabulary: the dag-cbor engine-key blob, the agent's TOML `config.toml`, the
/// JSON `location-map.json`, the app↔agent IPC, and a **filename** component
/// ([`db_component`](Self::db_component)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FolderRef {
    /// A set with a row on **this holder's own** nest — one they own (bound,
    /// unbound or served) *or* a same-nest set shared with them. The value is the
    /// nest's `folders.id` primary key, which both the owner-scoped and the
    /// member-visible `fauna.folders.list` projections carry verbatim from the
    /// same table, so the two can never collide.
    Local(i64),
    /// A **cross-nest** set, which has no row on this holder's nest. The value is
    /// the derived `ChannelId` — per `ForeignFolder::channel_id`, "the stable
    /// identity custody + the federated read kinds are keyed by".
    Foreign([u8; 32]),
}

impl FolderRef {
    /// Render the opaque wire string: `local:<id>` or `foreign:<64-hex>`.
    #[must_use]
    pub fn to_wire(&self) -> String {
        match self {
            FolderRef::Local(id) => format!("local:{id}"),
            FolderRef::Foreign(channel) => format!("foreign:{}", hex::encode(channel)),
        }
    }

    /// Parse a wire string produced by [`to_wire`](Self::to_wire).
    ///
    /// `None` for anything else — an unknown scheme from a *newer* peer, a
    /// truncated value, a bare set name. Every consumer treats `None` as "this
    /// row names no set" and **refuses** it — there is no name-matching
    /// fallback: an unparseable ref must never be *invented* into a wrong one,
    /// nor degraded onto a name that two sets can share.
    #[must_use]
    pub fn parse(wire: &str) -> Option<Self> {
        if let Some(id) = wire.strip_prefix("local:") {
            return id.parse::<i64>().ok().map(FolderRef::Local);
        }
        let channel = wire.strip_prefix("foreign:")?;
        let raw = hex::decode(channel).ok()?;
        <[u8; 32]>::try_from(raw.as_slice()).ok().map(Self::Foreign)
    }

    /// [`Foreign`](Self::Foreign) from a hex channel id — the form every
    /// cross-nest surface already carries (`ForeignFolder.channel_id` rendered
    /// for the wire, `FolderEngineKeys::channel_id_hex`).
    ///
    /// `None` on malformed hex, so a corrupt value reads as "no identity" — which
    /// every binding seam refuses — rather than as a wrong one.
    #[must_use]
    pub fn foreign_from_hex(channel_hex: &str) -> Option<Self> {
        let raw = hex::decode(channel_hex).ok()?;
        <[u8; 32]>::try_from(raw.as_slice()).ok().map(Self::Foreign)
    }

    /// The filesystem-safe component identifying this set's per-set state DB.
    ///
    /// Deliberately **not** the `fs-` prefix the retired name-keyed namer
    /// used (it sanitized every non-alphanumeric to `_`, so a set literally
    /// *named* `local_42` would have landed on the same `fs-local_42.db` as
    /// `Local(42)`): a distinct `fsid-` namespace keeps an identity-keyed DB
    /// from ever meeting a name-keyed file a device may still hold.
    #[must_use]
    pub fn db_component(&self) -> String {
        match self {
            FolderRef::Local(id) => format!("fsid-local-{id}"),
            FolderRef::Foreign(channel) => format!("fsid-foreign-{}", hex::encode(channel)),
        }
    }

    /// This set's per-set state DB under `state_dir`: `<state_dir>/fsid-<ref>.db`
    /// ([`db_component`](Self::db_component) + `.db`).
    ///
    /// The **one** join every host of a folder binding uses — the
    /// `fauna-sync-agent`'s resident engines, the in-process
    /// `FfiSyncEngineHost` (ingest, the badge and backlog reads), the apple File
    /// Provider host and the app's steward reads into the extension's root — so
    /// a set's writer and its cross-process readers can never disagree on which
    /// file is that set's (`on-demand-files.md` § Hosting multiple on-demand
    /// folders — *Per-set state DBs live in their own `fsid-<ref>.db` filename
    /// namespace*). A second spelling of the join is how a reader ends up
    /// looking at a file no writer ever touches.
    #[must_use]
    pub fn state_db_path(&self, state_dir: &std::path::Path) -> std::path::PathBuf {
        state_dir.join(format!("{}.db", self.db_component()))
    }

    /// This set's identity on one device for one account —
    /// [`ActorScopedFolderRef`].
    #[must_use]
    pub fn scoped_to(self, actor_id: [u8; 32]) -> ActorScopedFolderRef {
        ActorScopedFolderRef {
            actor_id,
            folder: self,
        }
    }

    /// The ref as ONE filesystem- and registry-safe token: the wire string
    /// percent-encoded as a single path component — `local:1` → `local%3A1`,
    /// `foreign:<hex>` → `foreign%3A<hex>` (ASCII alphanumerics and `-_. `
    /// pass, every other byte is `%XX`, uppercase). The **one encoder** behind
    /// every per-device registry that keys a directory or an OS identifier by
    /// a set: the ref half of [`ActorScopedFolderRef::to_wire`], apple's
    /// staging root (`FileProvider/roots/<actor-id-hex>/<component>`) and
    /// android's two owned-tree roots (`on-demand/<actor-id-hex>/<component>`)
    /// — so the identifier's ref half and the directory it stages into are one
    /// spelling (`on-demand-files.md` § Apple File Provider binding, *the
    /// actor-scoped device identity*). Not a wire form: [`parse`](Self::parse)
    /// refuses it.
    #[must_use]
    pub fn path_component(&self) -> String {
        percent_encode_component(&self.to_wire())
    }
}

/// Percent-encode `value` as one path component: ASCII alphanumerics and
/// `-_. ` pass, every other byte is `%XX` (uppercase hex). The rule
/// [`FolderRef::path_component`] applies; kept generic only so its alphabet is
/// pinned on its own.
fn percent_encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b' ') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Undo [`percent_encode_component`]: every `%XX` back to its byte. `None` on
/// a truncated or non-hex escape, or a result that is not UTF-8. Deliberately
/// lenient about the escape's hex case and about escapes the encoder never
/// writes — the CALLER re-encodes and compares, which is what makes the
/// scoped grammar accept exactly what it emits.
fn percent_decode_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let byte = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A [`FolderRef`] scoped to the actor whose account holds it on a device —
/// the key for anything a DEVICE keeps per set on behalf of one account, where
/// the bare ref is the wrong key: a `FolderRef` is unique per **nest**, not
/// per device (`local:<id>` is that nest's `folders.id`), so two accounts on
/// two nests routinely both own `local:1`, and a device-wide registry keyed by
/// the bare ref hands one account's presence to the other. The apple File
/// Provider domain identifier, its staging root, its on-demand-toggle
/// preference and its domain-owner record are the four such registries, and
/// android's SAF document id is the same key with a path appended
/// (`on-demand-files.md` § Apple File Provider binding, *the actor-scoped
/// device identity*); the per-set state DB is not one — it already lives
/// under the account's scoped state dir.
///
/// ## Wire form
///
/// `<ref-component>@<actor-id-hex>` — [`to_wire`](Self::to_wire) /
/// [`parse`](Self::parse), one opaque `String` like [`FolderRef`]'s, because
/// the OS domain registry takes exactly a string. The ref half is
/// [`FolderRef::path_component`] — the wire percent-encoded as one component,
/// `local%3A1` — never the bare wire: iOS refuses a File Provider domain
/// identifier carrying `/` or `:` (measured 2026-09-27, `NSPOSIXErrorDomain`
/// 22 on `local:1@…`), and an OS registry may key a directory by the
/// identifier, so the ref half is spelled exactly as the directory it stages
/// into. The `@` is the one separator: neither half carries it (the component's
/// alphabet is alphanumerics and `%`, the actor is 64 hex chars), and it is
/// path-safe everywhere — `/` was rejected as the separator because it would
/// be a path boundary. The ref leads so a log line shows the set before the
/// 64-char account. A bare ref is **not** a scoped ref, and neither is the
/// pre-2026-09-29 spelling with the bare `:` in the ref half:
/// [`parse`](Self::parse) refuses both, so an identifier from before either
/// change can never be mistaken for a current one (and a scoped one never
/// parses as a bare [`FolderRef`] — `local%3A1@…` has no `local:` scheme).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActorScopedFolderRef {
    /// The account holding this set on the device (`ActorId` bytes).
    pub actor_id: [u8; 32],
    /// The set.
    pub folder: FolderRef,
}

impl ActorScopedFolderRef {
    /// The separator between the ref component and the actor in the wire form.
    pub const SEPARATOR: char = '@';

    /// Render the opaque wire string: `<ref-component>@<actor-id-hex>`
    /// (lowercase hex) — carries neither `/` nor `:` for any [`FolderRef`].
    #[must_use]
    pub fn to_wire(&self) -> String {
        format!(
            "{}{}{}",
            self.folder.path_component(),
            Self::SEPARATOR,
            hex::encode(self.actor_id)
        )
    }

    /// Parse a wire string produced by [`to_wire`](Self::to_wire) — and
    /// **only** that: the ref half is decoded, parsed as a [`FolderRef`] and
    /// re-encoded, and must come back byte-identical, so the bare wire
    /// (`local:1@…`, the pre-2026-09-29 spelling) and any escape the encoder
    /// would not have written are refused along with everything else.
    ///
    /// `None` for anything else — a bare [`FolderRef`] (an identifier from
    /// before actor scoping), a set name, a malformed or wrong-length actor.
    /// Every consumer treats `None` as "this identifier scopes no set to any
    /// account" and refuses to serve or key anything by it; the one thing a
    /// consumer may still do with such an identifier is remove it.
    #[must_use]
    pub fn parse(wire: &str) -> Option<Self> {
        let (component, actor) = wire.split_once(Self::SEPARATOR)?;
        let folder = FolderRef::parse(&percent_decode_component(component)?)?;
        if folder.path_component() != component {
            return None;
        }
        if actor.len() != 64 {
            return None;
        }
        let raw = hex::decode(actor).ok()?;
        let actor_id = <[u8; 32]>::try_from(raw.as_slice()).ok()?;
        Some(Self { actor_id, folder })
    }
}

/// One folder's engine key material — resolved by the owner's identity-holding
/// client and **pushed** to a **bearer-only** sync service that cannot unseal
/// account-plane custody itself (Slice-3 piece **5d(c)**, windows;
/// `docs/goal/architecture/key-material-hierarchy.md` § Implementation status:
/// "the windows sync service is bearer-only … its content keys must be pushed from
/// the owner's primary device via the app-provisioned `SyncCapability`").
///
/// The `(mls_group_id, content_keys)` pair is exactly `fauna-client-folders`'s
/// `EngineKeyBinding::engine_args()` tagged with the set name, so the service
/// threads it straight into `SyncEngine::new` (`mls_group_id` / `content_keys`
/// positions) and **inherits the fail-closed posture**: a bound-but-keyless set
/// carries `Some(mls_group_id)` + `None` keys — never `(None, None)`, which would
/// seal shared content in plaintext. Resolved in-process by the host that builds
/// the engine (`fauna_client_folders::engine_keys`) and never persisted — the
/// app-pushed `SyncCapability.content_key_bindings` blob that once carried it
/// across a process boundary is retired.
///
/// **Cross-nest sets ride the same blob**: a set homed on
/// *another* nest additionally carries [`home_nest_url`](Self::home_nest_url) +
/// [`channel_id_hex`](Self::channel_id_hex), which tell the service to point the
/// byte plane at the home nest under a `WriteTokenBearer` and to route the
/// control plane's change-log read + record through the member's own nest as a
/// relay. Both are `#[serde(default)]`, so an entry written by an older app
/// decodes as a same-nest set — the pre-Phase-4 behavior, and the fail-safe one.
///
/// `Default` is derived deliberately: fixtures build these with
/// `..Default::default()` so two branches independently growing this struct
/// merge cleanly instead of colliding on every hand-listed literal — the
/// house convention for any wire type that grows on an additive cadence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderEngineKeys {
    /// The nest folder name (`FolderSummary.name`) — the key the sync service
    /// matches against the set it hydrates.
    ///
    /// ⚠ Names are unique only **per owner**, so this key is ambiguous when a
    /// caller owns a set and is a member of a same-named one — which is why
    /// [`folder_id`](Self::folder_id) is the key every consumer matches on.
    /// This field is a **label only**: what the engine is told its set is
    /// called, and what logs print. Nothing resolves an entry by it.
    ///
    /// Spelled `folder` since phase 1b's rename from `file_set`. The read
    /// alias that kept a blob persisted under the old spelling decodable was
    /// retired 2026-09-24 (the compat-remnant sweep at its universal scope,
    /// `docs/goal/architecture/version-compatibility.md` § Dimension 2 — no
    /// pre-rename blob rests anywhere), so the field is simply required and a
    /// pre-rename blob is refused whole — pinned by
    /// `pre_rename_content_key_bindings_are_refused`.
    pub folder: String,
    /// The set's [`FolderRef`] in wire form — the **unambiguous** key the agent
    /// seam matches on, because [`folder`](Self::folder) cannot tell an owned
    /// set from a same-named one shared by someone else.
    ///
    /// **Required.** The name-keyed fallback a pre-identity app's entry used to
    /// take was retired 2026-09-24 (the compat-remnant sweep at its universal
    /// scope, `docs/goal/architecture/version-compatibility.md` § Dimension 2):
    /// every current app resolves the ref at bind time
    /// (`fauna_client_folders::engine_binding::folder_ref_for_row`), so an
    /// entry without one is refused whole at decode — pinned by
    /// `entry_without_folder_id_is_refused`.
    pub folder_id: String,
    /// The raw MLS group id (the engine's bound-marker), or `None` for an
    /// owner-only unbound set. `Some` ⇒ the engine treats the set as shared and
    /// **fails closed** if `content_keys` is `None`.
    #[serde(default, with = "serde_bytes")]
    pub mls_group_id: Option<Vec<u8>>,
    /// The full content-key generation history to seal current uploads under and
    /// open any prior-generation chunk with. `None` when the set is unbound, or
    /// bound-but-keyless (a removed member / unsynced generation → fail closed).
    pub content_keys: Option<FolderContentKeys>,
    /// **Cross-nest only** — the base URL of the nest this set is *homed* on,
    /// when that is not the caller's own nest. `Some` ⇔ this is a foreign set
    /// (always paired with [`channel_id_hex`](Self::channel_id_hex)); the source
    /// is the member's own `fauna.state.folder-keys` `ForeignFolder` row, which is the only
    /// place a foreign set exists client-side — no nest projection can hold it.
    ///
    /// The service reads it as: point the byte plane (chunk/manifest transfers)
    /// at *this* URL under a `WriteTokenBearer`, and hand the pair to
    /// `SyncEngine::set_foreign_routing` so the control-plane change-log read +
    /// record relay through the caller's own nest to here.
    #[serde(default)]
    pub home_nest_url: Option<String>,
    /// **Cross-nest only** — the set's derived `ChannelId` as lowercase hex, the
    /// identity both federated relay kinds address the set by (the own nest has
    /// no row for a foreign set, so its *name* addresses nothing there). Always
    /// `Some` exactly when [`home_nest_url`](Self::home_nest_url) is.
    #[serde(default)]
    pub channel_id_hex: Option<String>,
    /// **Cross-nest only** — the home nest's deployment `nest_actor_id`
    /// (hex-encoded 32-byte Ed25519 pubkey), carried from the member's own
    /// `fauna.state.folder-keys` `ForeignFolder` row. The member holds no account on the home nest,
    /// so its direct byte-plane HTTPS dial graduates an SPKI pin against
    /// `IdentityRoot::PreResolved` of this actor id (the pre-identity
    /// `fauna.auth.nest_handshake`) before any transfer — `RequireWebPki` alone
    /// refuses a self-signed home. `None` (a non-conforming home nest that relayed no actor id) → the
    /// byte plane keeps today's `RequireWebPki`, never weaker.
    /// (`docs/goal/architecture/security.md` § Transport trust, the
    /// federation-granted Axis-2 row.)
    #[serde(default)]
    pub home_nest_actor_id: Option<String>,
    /// The **`public`-audience** write-arm flag (folders re-model phase 4):
    /// `true` ⇒ the owner declassified this folder, so the engine the consumer
    /// builds uploads **unsealed** (`SyncEngine::with_public_audience`) —
    /// plaintext chunks and labels, the ratified world-readable shape. Additive
    /// with a `false` default, which is the fail-safe direction: a consumer
    /// (or a pre-phase-4 blob) that never heard of audiences keeps sealing —
    /// over-sealing a public folder is an availability lag, under-sealing a
    /// private one is an unrecoverable disclosure. Orthogonal to the key
    /// material above: a declassified *bound* folder still carries its
    /// `mls_group_id`/`content_keys` (they open the sealed pre-declassify
    /// back-catalogue); only the write side is plaintext.
    #[serde(default)]
    pub public_audience: bool,
    /// Retired M2 generations of a set this entry's *live* binding no longer
    /// claims — the pushed-blob twin of
    /// [`fauna_sync_engine::SyncEngine::retired_content_keys`]
    /// (`webdav-server.md` § Key model, Revocation): a WebDAV serve toggle
    /// rotates a group-less set's content key rather than forgetting it, so
    /// the owner's own custody still holds every generation a served window
    /// sealed chunks under — even once this entry's own `mls_group_id` /
    /// `content_keys` above degrade to owner-only. The producer
    /// (`resolve_engine_key_bindings`) resolves it via
    /// `fauna_client_folders::retired_serve_custody`; the bearer-only
    /// consumer threads it onto the engine it builds via
    /// `SyncEngine::set_retired_content_keys`, a **read** candidate only —
    /// it can never re-trip the content-keyed gate that already resolved
    /// this entry to the owner path.
    ///
    /// `#[serde(default)]`: additive, no major bump
    /// (`version-compatibility.md`) — `None` means no retired serve custody.
    #[serde(default)]
    pub retired_content_keys: Option<FolderContentKeys>,
    /// The set's live nonce from the pushing app's custody — the binding every
    /// writer-signed change record the engine signs or verifies covers
    /// (`mls-group-key-material.md` § M2 → *Custody shape of the set nonce*,
    /// (g)). Carried on every arm, an unbound set's included. `None` when
    /// custody holds none yet (a nonce-less entry the owner reconcile repairs).
    /// `#[serde(default)]`: additive.
    #[serde(default)]
    #[serde(with = "serde_bytes")]
    pub set_nonce: Option<[u8; 32]>,
    /// The nonces of the owner's retired entries for this set's name — never a
    /// current binding, only the input the engine's re-record leg reads to
    /// re-sign this device's rows onto [`Self::set_nonce`] (ruling (g)).
    /// Empty for a member. `#[serde(default)]`: additive.
    #[serde(default)]
    #[serde(with = "crate::byte_array::vec")]
    pub retired_set_nonces: Vec<[u8; 32]>,
    /// The identity whose device minted [`Self::set_nonce`]
    /// (`writer-signed-change-records.md` ruling (11)(a)) — the reader's arm
    /// (1) asks it of every row signed as a predecessor of the owner.
    /// `#[serde(default)]`: additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_nonce_minted_by: Option<crate::identity::ActorId>,
    /// The set's lineage with each nonce's minter and its siblings, newest
    /// first (ruling (11)(b)) — what the reader tries a row under once the
    /// live nonce failed: the owner's lineage off custody, a member's off the
    /// owner's envelope. [`Self::retired_set_nonces`] stays the owner's
    /// re-record leg's input. `#[serde(default)]`: additive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired_lineage: Vec<RetiredSetNonce>,
    /// The device-local **adoption marker** (ruling (11)(d)): the nonce this
    /// device's re-mint replaced, written by the owner's custody reconcile
    /// before its custody write — what licenses this device's engine to adopt
    /// the nest's history heads once. `None` on every other device and set.
    /// `#[serde(default)]`: additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "serde_bytes")]
    pub adoption_marker: Option<[u8; 32]>,
    /// The custody entry's serve-on stamp (ruling (7)(b)(ii) rule (4)) — the
    /// owner's live pick, or a member's entry at the real channel. Carried on
    /// every arm, so the agent's custody re-read rebuilds the binding edge
    /// when the stamps move and the roster row does not.
    /// `#[serde(default)]`: additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served_at: Option<u64>,
    /// The custody entry's serve-off stamp, beside [`Self::served_at`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unserved_at: Option<u64>,
}

impl FolderEngineKeys {
    /// Whether the custody entry this binding was resolved from calls the set
    /// WebDAV-served — the producer resolves a live entry only, so the stamps
    /// alone answer ([`serve_window_open`]).
    #[must_use]
    pub fn webdav_served(&self) -> bool {
        serve_window_open(self.served_at, self.unserved_at)
    }

    /// The `(home_nest_url, channel_id_hex)` cross-nest routing pair, when this
    /// entry is a foreign set. Both fields are set together by the producer, so
    /// a half-populated entry is a corrupt blob: it yields `None` here — the
    /// same-nest answer, which is the fail-safe one (a foreign set treated as
    /// same-nest fails **loud** at the own nest with `not_found`, whereas a
    /// same-nest set treated as foreign would silently relay writes at a nest
    /// that never claimed the set).
    pub fn foreign_routing(&self) -> Option<(String, String)> {
        match (&self.home_nest_url, &self.channel_id_hex) {
            (Some(url), Some(chan)) => Some((url.clone(), chan.clone())),
            _ => None,
        }
    }
}

// ── The custody-resolution seam ────────────────────────────────────────────────
//
// Lives here, in the crate that owns the custody types themselves, because it
// has **three** consumers with no other crate in common: the Media machine's
// byte download, the snapshot browse/diff reads, and the conflict list. It
// started life in `fauna-media-machine` when Media was the only consumer; a
// devices machine or a snapshots client reaching for it there would have had to
// depend on the Media page machine to render a file name, which is exactly the
// forked-second-resolver shape the sealing ruling's read half forbids.

/// A **content-keyed** folder's key material, as resolved from the caller's
/// custody: a set bound to a cross-user group, or a WebDAV-served group-less
/// one (`webdav-server.md` § Key model, the custody note — its keys rest at
/// the serve pseudo-channel). Either way the set's chunks and labels are sealed
/// under M2 content keys, never the owner root.
#[derive(Clone)]
pub struct ResolvedFolderKeys {
    /// The raw MLS group id binding the set when it is **shared**; `None` for a
    /// served-but-unshared set, which is content-keyed with no group at all
    /// (the read-side twin of the engine's `EngineKeyBinding::ServedUnshared`).
    /// The *set being content-keyed* is carried by [`ResolvedCustody`]'s arm,
    /// not by this field: [`crate::file_download::FileDownloadKeys`] suppresses
    /// the owner-key path on the **chunk** side for both shapes (a content-keyed
    /// set's chunks are never owner-keyed), and a served set must never fake a
    /// group id to get there. The *label* side deliberately keeps the owner
    /// key — see [`crate::file_download::FileDownloadKeys::label_open_roots`].
    pub mls_group_id: Option<Vec<u8>>,
    /// The caller's content-key generation history for the set (owner or member
    /// custody — the same shape). A read at a stamped generation fails closed if
    /// this holder lacks it (removed member / unsynced generation).
    ///
    /// `None` = **content-keyed but unresolvable**: the set IS content-keyed
    /// (bound, or served) and this holder's custody cannot produce its content
    /// keys right now (removed member, or the window before custody ingest
    /// completes — for a served set, the serve-enable custody write racing
    /// this device). Carrying the content-keyed-ness instead of collapsing to
    /// a resolver miss is what makes
    /// [`crate::file_download::FileDownloadKeys::label_seal_root`]'s
    /// fail-closed bail reachable for every seal site — the
    /// old shape forced `resolve` to answer `None`, which callers read as
    /// "unbound" and sealed under the owner root no roster member could open.
    pub content_keys: Option<FolderContentKeys>,
    /// The set's **home-nest base URL** when it is a FOREIGN (cross-nest) set —
    /// its manifest/chunk bytes live on that nest, so a byte download must fetch
    /// there (Phase 2 client read-side; the byte routes are public + CORS-open
    /// and integrity is by content address, so a plain unauthenticated GET is
    /// the ratified posture). `None` for every same-nest set (owner or member).
    /// Label rendering never needs it — a sealed label travels in the row.
    pub home_nest_url: Option<String>,
    /// The home nest's identity (`nest_actor_id`, 64-hex) as the grant
    /// delivered it — the byte-plane dial's trust root for a self-signed home
    /// (`security.md` § Transport trust, the federation-granted row): the
    /// member holds no account there, so the dial verifies the nest against
    /// THIS identity instead of a bearer handshake. `None` when the home never
    /// stamped one (a relay-unaware home) — the dial then keeps the WebPKI
    /// floor — and always `None` for a same-nest set.
    pub home_nest_actor_id: Option<String>,
}

/// Where a FOREIGN set's bytes live, as [`crate::label_custody::LabelCustody::keys_for`]
/// hands it to a byte consumer: the home nest's base URL plus the identity the
/// dial is verified against. One value rather than two loose `Option`s so the
/// URL and its trust root can never be paired from different records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignHome {
    /// The home nest's base URL (no trailing slash).
    pub nest_url: String,
    /// [`ResolvedFolderKeys::home_nest_actor_id`] — the dial's trust root, if
    /// the grant delivered one.
    pub nest_actor_id: Option<String>,
}

/// A [`FolderKeyResolver`]'s answer for one set — which key custody the set's
/// bytes and labels rest under.
///
/// Two arms, never a bare `Option`: the owner-only arm has a payload of its
/// own (a since-unflagged serve window's retired generations), and an
/// `Option<ResolvedFolderKeys>` could carry it only by overloading "no
/// content-keyed custody" with "some", which is precisely the collapse the
/// seal sites must never see.
#[derive(Clone)]
pub enum ResolvedCustody {
    /// **Positively owner-only**: an owned/member roster row exists and is
    /// neither bound to a group nor WebDAV-served, or no row and no foreign
    /// record exists. The owner-key path is *correct* here, for seals and
    /// opens alike.
    OwnerOnly {
        /// M2 generations a **since-unflagged** serve window sealed this set's
        /// files and names under — the owner's custody at the serve
        /// pseudo-channel still holds them (`webdav-server.md` § Key model,
        /// Revocation: unflagging rotates, never forgets). **Read-only
        /// candidates**, threaded onto
        /// [`crate::file_download::FileDownloadKeys::retired_content_keys`];
        /// never a seal root, and never the reason the set counts as
        /// content-keyed. `None` for a set with no serve history.
        retired_content_keys: Option<FolderContentKeys>,
    },
    /// **Content-keyed**: bound to a cross-user group, or served group-less.
    /// `content_keys: None` inside means content-keyed-but-unresolvable: seals
    /// must fail closed, opens fail closed on their own.
    ContentKeyed(ResolvedFolderKeys),
}

impl ResolvedCustody {
    /// The plain owner-only answer — no group, no serve history.
    pub fn owner_only() -> Self {
        Self::OwnerOnly {
            retired_content_keys: None,
        }
    }

    /// Whether the set's bytes and labels rest under M2 content keys (either
    /// [`ContentKeyed`](Self::ContentKeyed) shape), as opposed to the owner
    /// root.
    pub fn is_content_keyed(&self) -> bool {
        matches!(self, Self::ContentKeyed(_))
    }

    /// The content-keyed payload, when there is one.
    pub fn content_keyed(&self) -> Option<&ResolvedFolderKeys> {
        match self {
            Self::ContentKeyed(keys) => Some(keys),
            Self::OwnerOnly { .. } => None,
        }
    }
}

/// Resolves a folder's key custody. Given a set's **`name_hash`**
/// ([`crate::path_crypto::set_name_hash`] — the address every roster row
/// carries even once its plaintext `name` rests sealed), returns whether the
/// set is content-keyed and, if so, its identity + content keys. A caller
/// holding only the name hashes it first
/// ([`crate::label_custody::LabelCustody::keys_for`] does); a sealed-label
/// render, which needs the keys *before* it can open the name, passes the
/// row's own hash ([`crate::label_custody::LabelCustody::keys_for_hash`]).
///
/// The three-valued answer is load-bearing — the fixes
/// live in its shape, because this seam serves **both** directions and their
/// failure asymmetry is opposite: an *open* with too-few keys degrades safely
/// (`Omit` / a chunk that will not decrypt), while a *seal* that mistakes
/// "don't know" for "unbound" mints an owner-root seal no roster member can
/// open — which the S9 scrub then treats as proof the plaintext is
/// recoverable, and destroys it.
///
/// - `Ok(ResolvedCustody::ContentKeyed(_))` — the set is **content-keyed**
///   (bound to a group, or WebDAV-served with no group). `content_keys: None`
///   inside means content-keyed-but-unresolvable: seals must fail closed,
///   opens fail closed on their own.
/// - `Ok(ResolvedCustody::OwnerOnly { .. })` — **positively owner-only**: an
///   owned/member roster row exists and is neither bound nor served, or no row
///   and no foreign record exists. The owner-key path is *correct* here, for
///   seals and opens alike; the arm additionally carries any retired serve
///   generations as read candidates.
/// - `Err(_)` — **could not determine** (roster/config transport failure,
///   corrupt bound-row state). Callers must not substitute the owner path:
///   [`crate::label_custody::LabelCustody::keys_for`] yields no keys at all,
///   so a seal records plaintext-only (the ratified degrade — a later backfill
///   converges it) and a render omits for one pass.
///
/// Async because the impl consults the member-visible roster (`name_hash` →
/// `mls_group_id` / `webdav_enabled`) alongside the local custody
/// read. The production impl is `fauna_client_folders::NestFolderKeyResolver`;
/// a surface built without a resolver treats every set as owner-only.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait FolderKeyResolver: crate::MaybeSendSync {
    /// The set's custody answer — see the trait docs for the three-valued
    /// contract. Never fold a lookup *failure* into an owner-only answer:
    /// "positively owner-only" licenses the owner root at every seal site.
    async fn resolve(&self, name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody>;

    /// The set's nonce — the binding a writer-signed change record for
    /// `folder` covers (`mls-group-key-material.md` § M2 → *Writer-signed
    /// change records* (2)) — from the same roster + custody read [`Self::resolve`]
    /// makes. `Ok(None)`: custody holds no nonce for the set (the record goes
    /// out unsigned, loudly). The default answers `None` — a resolver that
    /// cannot see custody signs nothing.
    async fn set_nonce(&self, _folder: &str) -> anyhow::Result<Option<[u8; 32]>> {
        Ok(None)
    }

    /// The set's nonce **lineage** (`writer-signed-change-records.md` ruling
    /// (11)(b)) from the same read — the live nonce with its minter and the
    /// retired nonces with theirs, which a projection reader verifies history
    /// under. The default carries [`Self::set_nonce`] alone.
    ///
    /// The set is addressed by `name_hash`, as [`Self::resolve`] is: a
    /// projection names a sealed set by its hash alone, and `folder` is then
    /// empty (`path-sealing.md` § the set-name plane). `folder` serves only a
    /// resolver that keys by name — the default.
    async fn set_lineage(
        &self,
        folder: &str,
        _name_hash: &[u8; 32],
    ) -> anyhow::Result<SetNonceLineage> {
        Ok(SetNonceLineage {
            live: self.set_nonce(folder).await?,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    /// A `SyncCapability.content_key_bindings` blob in its **pre-folders-rename**
    /// shape: one bound, keyed set, with the entry's fields spelled `file_set` /
    /// `file_set_id`.
    ///
    /// The agent **persists** its provisioned capability (`credentials.rs` — hex
    /// of the canonical dag-cbor), which is why the old spelling used to be
    /// read through `#[serde(alias)]`es; since the 2026-09-24 compat-remnant
    /// sweep no such blob exists and the aliases are gone, so this literal now
    /// pins the *refusal*. A literal, for the same reason [`crate::data`]'s
    /// goldens are: a fixture built from today's struct stops modelling the
    /// historical shape the moment a rename sweep walks over it.
    const PRE_RENAME_BINDINGS_HEX: &str = "81a46866696c655f73657464646f63736b66696c655f7365745f69646a6f776e65723a646f63736c636f6e74656e745f6b657973a2657072696f72806763757272656e74a3636b6579582007070707070707070707070707070707070707070707070707070707070707076776657273696f6e016a726f74617465645f61741903e86c6d6c735f67726f75705f69648818ab18ab18ab18ab18ab18ab18ab18ab";

    /// **Named mutation M-13-D** — re-add either retired read alias
    /// (`file_set` / `file_set_id`) on [`FolderEngineKeys`] and this fails.
    ///
    /// A capability persisted before the folders rename carries the old
    /// spellings. Until 2026-09-24 the aliases read it, because a decode
    /// failure withholds every engine: `engine_driver::engine_keys_for`
    /// answers a malformed blob with `None` for *every* hosted set — fail-closed,
    /// never the all-default `(None, None)` owner-only reading this type's own
    /// doc calls out as sealing shared content in plaintext — so the agent syncs
    /// nothing until the app pushes a well-formed blob. The compat-remnant
    /// sweep (universal scope) retired the aliases on the ruling that no
    /// pre-rename blob rests anywhere, so the pin is now the **refusal**:
    /// `folder` is required, the whole `Vec` fails to decode, and an alias
    /// quietly re-added would be a remnant serving nothing.
    #[test]
    fn pre_rename_content_key_bindings_are_refused() {
        let bytes = hex::decode(PRE_RENAME_BINDINGS_HEX).unwrap();
        let decoded = crate::encoding::canonical_decode::<Vec<FolderEngineKeys>>(&bytes);
        assert!(
            decoded.is_err(),
            "a pre-rename blob must be refused: its read aliases were retired 2026-09-24"
        );
    }

    #[test]
    fn genesis_is_version_1_with_no_prior() {
        let g = FolderContentKeys::genesis(k(7), 1_000);
        assert_eq!(g.current_version(), 1);
        assert_eq!(g.current_key(), &k(7));
        assert!(g.prior.is_empty());
        assert_eq!(g.key_for(1), Some(&k(7)));
        assert_eq!(g.key_for(2), None);
    }

    #[test]
    fn rotate_bumps_version_and_keeps_prior_readable() {
        let mut g = FolderContentKeys::genesis(k(1), 1_000);
        let v = g.rotate(k(2), 2_000);
        assert_eq!(v, 2);
        assert_eq!(g.current_version(), 2);
        assert_eq!(g.current_key(), &k(2));
        // Pre-rotation content (sealed under gen 1) stays readable — history.
        assert_eq!(g.key_for(1), Some(&k(1)));
        assert_eq!(g.key_for(2), Some(&k(2)));

        let v = g.rotate(k(3), 3_000);
        assert_eq!(v, 3);
        assert_eq!(g.prior.len(), 2);
        // prior is most-recent first.
        assert_eq!(g.prior[0].version, 2);
        assert_eq!(g.prior[1].version, 1);
        assert_eq!(g.key_for(1), Some(&k(1)));
        assert_eq!(g.key_for(2), Some(&k(2)));
        assert_eq!(g.key_for(3), Some(&k(3)));
    }

    #[test]
    fn rotate_forces_strictly_increasing_rotated_at_under_clock_skew() {
        let mut g = FolderContentKeys::genesis(k(1), 5_000);
        // `now` regressed below the prior generation's stamp (stale device).
        g.rotate(k(2), 100);
        assert!(
            g.current.rotated_at > g.prior[0].rotated_at,
            "rotated_at must strictly increase even when now regresses"
        );
        assert_eq!(g.current.rotated_at, 5_001);
        // Equal `now` also bumps strictly.
        g.rotate(k(3), 5_001);
        assert_eq!(g.current.rotated_at, 5_002);
    }

    #[test]
    fn key_for_unknown_version_is_none_fail_closed() {
        let g = FolderContentKeys::genesis(k(1), 1_000);
        // A removed member reading a post-removal version it never received.
        assert_eq!(g.key_for(99), None);
    }

    #[test]
    fn generations_yields_current_then_prior() {
        let mut g = FolderContentKeys::genesis(k(1), 1_000);
        g.rotate(k(2), 2_000);
        g.rotate(k(3), 3_000);
        let versions: Vec<u64> = g.generations().map(|entry| entry.version).collect();
        assert_eq!(versions, vec![3, 2, 1]);
    }

    #[test]
    fn from_generations_roundtrips_via_the_bundle() {
        let mut owner = FolderContentKeys::genesis(k(1), 1_000);
        owner.rotate(k(2), 2_000);
        owner.rotate(k(3), 3_000);

        // The bundle the owner seals into the envelope (any order on the wire).
        let mut bundle: Vec<ContentKeyGeneration> = owner.generations().cloned().collect();
        bundle.reverse(); // simulate a different wire order
        let member = FolderContentKeys::from_generations(bundle).unwrap();

        // The member reconstructs the identical history (history-on-join).
        assert_eq!(member, owner);
        assert_eq!(member.current_version(), 3);
        assert_eq!(member.key_for(1), Some(&k(1)));
        assert_eq!(member.key_for(2), Some(&k(2)));
        assert_eq!(member.key_for(3), Some(&k(3)));
    }

    #[test]
    fn from_generations_rejects_an_empty_bundle() {
        assert!(FolderContentKeys::from_generations(vec![]).is_err());
    }

    /// Two owner devices rotated to one version before either saw the other:
    /// the owner's merged custody holds both generations, its envelope carries
    /// both, and the member reconstructs the owner's exact history — in any
    /// wire order — and opens a chunk sealed under either key.
    #[test]
    fn a_member_ingests_a_bundle_with_two_generations_of_one_version() {
        let mut a = FolderContentKeys::genesis(k(1), 1_000);
        a.rotate(k(9), 2_000);
        let mut b = FolderContentKeys::genesis(k(1), 1_000);
        b.rotate(k(5), 2_001);
        let owner = a.merge(&b);

        let hash = crate::data::ContentHash::of_raw(b"one chunk");
        let sealed_by_a = crate::chunk_crypto::encrypt_chunk(&k(9), &hash, b"one chunk").unwrap();
        let sealed_by_b = crate::chunk_crypto::encrypt_chunk(&k(5), &hash, b"one chunk").unwrap();

        let payload = ContentKeyEnvelopePayload {
            keys: owner.clone(),
            set_nonce: Some([7; 32]),
            minted_by: None,
            retired_set_nonces: Vec::new(),
            served_at: None,
            unserved_at: None,
        };
        let member = ContentKeyEnvelopePayload::decode(&payload.encode().unwrap())
            .expect("a same-version pair is a legitimate owner state")
            .keys;
        assert_eq!(member, owner, "the member holds the owner's history");

        let mut bundle: Vec<ContentKeyGeneration> = owner.generations().cloned().collect();
        bundle.reverse();
        bundle.push(owner.current.clone()); // a repeated generation folds away
        assert_eq!(FolderContentKeys::from_generations(bundle).unwrap(), owner);

        for sealed in [&sealed_by_a, &sealed_by_b] {
            assert!(
                member
                    .keys_for(2)
                    .any(|key| crate::chunk_crypto::decrypt_chunk(key, &hash, sealed).is_ok()),
                "a version-2 chunk opens under one of the version-2 candidates"
            );
        }
    }

    #[test]
    fn merge_keeps_higher_current_and_retains_all_generations() {
        // Device A rotated to gen 2; device B is still at gen 1. Merge keeps
        // gen 2 as current and retains gen 1 in prior (no generation lost).
        let mut a = FolderContentKeys::genesis(k(1), 1_000);
        a.rotate(k(2), 2_000);
        let b = FolderContentKeys::genesis(k(1), 1_000);

        let merged = a.merge(&b);
        assert_eq!(merged.current_version(), 2);
        assert_eq!(merged.key_for(1), Some(&k(1)));
        assert_eq!(merged.key_for(2), Some(&k(2)));
        // Commutative.
        assert_eq!(b.merge(&a), merged);
    }

    #[test]
    fn merge_concurrent_rotation_retains_loser_key() {
        // Both devices rotated gen 1 → gen 2 independently with DIFFERENT keys.
        // The deterministically-higher current wins; the loser's key is retained
        // in prior (irrecoverable → never dropped).
        let mut a = FolderContentKeys::genesis(k(1), 1_000);
        a.rotate(k(9), 2_000);
        let mut b = FolderContentKeys::genesis(k(1), 1_000);
        b.rotate(k(5), 2_000);

        let merged = a.merge(&b);
        assert_eq!(merged.current_version(), 2);
        // Both gen-2 keys are reachable (one as current, one in prior).
        let all: Vec<[u8; 32]> = merged.generations().map(|g| g.key.to_array()).collect();
        assert!(
            all.contains(&k(9)) && all.contains(&k(5)),
            "both keys retained"
        );
        assert!(all.contains(&k(1)), "gen 1 retained");
        // Commutative + deterministic.
        assert_eq!(b.merge(&a), merged);
    }

    #[test]
    fn keys_for_reaches_every_same_version_candidate_after_concurrent_merge() {
        // Retained is not enough — the
        // LOOKUP must reach both same-version keys, current-first, or the
        // loser's chunks AEAD-fail forever despite the key sitting in `prior`.
        let mut a = FolderContentKeys::genesis(k(1), 1_000);
        a.rotate(k(9), 2_000);
        let mut b = FolderContentKeys::genesis(k(1), 1_000);
        b.rotate(k(5), 2_000);
        let merged = a.merge(&b);

        let v2: Vec<[u8; 32]> = merged.keys_for(2).copied().collect();
        assert_eq!(v2, vec![k(9), k(5)], "both v2 candidates, current first");
        let v1: Vec<[u8; 32]> = merged.keys_for(1).copied().collect();
        assert_eq!(v1, vec![k(1)], "unique version stays a single candidate");
        assert_eq!(
            merged.keys_for(3).count(),
            0,
            "absent generation yields no candidates (fail closed)"
        );
        // key_for stays the first candidate.
        assert_eq!(merged.key_for(2), Some(&k(9)));
    }

    #[test]
    fn content_key_generation_debug_is_redacted() {
        // A stray `{:?}` on any custody struct
        // must never print raw content-key bytes into a log.
        let g = ContentKeyGeneration {
            version: 1,
            key: [0xABu8; 32].into(),
            rotated_at: 5,
        };
        let rendered = format!("{g:?}");
        // The bare-array Debug would render the bytes (`[171, 171, …]`).
        assert!(!rendered.contains("171"), "Debug leaked the content key");
        assert!(
            rendered.contains("version: 1"),
            "diagnostics survive: {rendered}"
        );
    }

    #[test]
    fn content_key_generation_wire_is_bare_array_compatible() {
        // At-rest compat pin (account-plane custody + the sealed content-key
        // envelope, alpha no-data-loss): the generation must encode
        // byte-for-byte as if `key` were a bare `[u8; 32]`.
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Mirror {
            version: u64,
            #[serde(with = "serde_bytes")]
            key: [u8; 32],
            rotated_at: u64,
        }
        let g = ContentKeyGeneration {
            version: 3,
            key: [0x5Au8; 32].into(),
            rotated_at: 9,
        };
        let bytes = crate::encoding::canonical_encode(&g).unwrap();
        let mirror: Mirror = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(mirror.version, 3);
        assert_eq!(mirror.key, [0x5Au8; 32]);
        assert_eq!(mirror.rotated_at, 9);
        let mirror_bytes = crate::encoding::canonical_encode(&mirror).unwrap();
        assert_eq!(
            bytes, mirror_bytes,
            "wire identical to the bare-array shape"
        );
        let back: ContentKeyGeneration = crate::encoding::canonical_decode(&mirror_bytes).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn serde_roundtrip_is_canonical_stable() {
        let mut g = FolderContentKeys::genesis(k(1), 1_000);
        g.rotate(k(2), 2_000);
        let encoded = crate::encoding::canonical_encode(&g).unwrap();
        let decoded: FolderContentKeys = crate::encoding::canonical_decode(&encoded).unwrap();
        assert_eq!(decoded, g);
        // Re-encoding the decoded value is byte-identical (canonical/dedup-stable).
        let re_encoded = crate::encoding::canonical_encode(&decoded).unwrap();
        assert_eq!(encoded, re_encoded);
    }

    #[test]
    fn engine_keys_bundle_canonical_roundtrips() {
        // The three engine-key arms the resolver produces, as they ride the
        // opaque `SyncCapability.content_key_bindings` blob: unbound, bound-with-
        // keys, and bound-but-keyless (fail-closed).
        let mut keys = FolderContentKeys::genesis(k(1), 1_000);
        keys.rotate(k(2), 2_000);
        let bindings = vec![
            FolderEngineKeys {
                folder: "owner-only".into(),
                ..Default::default()
            },
            FolderEngineKeys {
                folder: "shared-docs".into(),
                mls_group_id: Some(b"raw-openmls-group-id".to_vec()),
                content_keys: Some(keys.clone()),
                ..Default::default()
            },
            FolderEngineKeys {
                folder: "removed-member-set".into(),
                mls_group_id: Some(b"another-raw-group-id".to_vec()),
                ..Default::default()
            },
            // The cross-nest arm (D5): a foreign set carries the same fail-closed
            // `(gid, keys)` pair PLUS the routing pair that points the byte plane
            // at its home nest and relays the control plane there.
            FolderEngineKeys {
                folder: "xnest-docs".into(),
                mls_group_id: Some(b"foreign-raw-group-id".to_vec()),
                content_keys: Some(keys),
                home_nest_url: Some("https://home.example".into()),
                channel_id_hex: Some("ab".repeat(32)),
                home_nest_actor_id: Some("cd".repeat(32)),
                folder_id: FolderRef::Foreign([0xab; 32]).to_wire(),
                ..Default::default()
            },
        ];
        let encoded = crate::encoding::canonical_encode(&bindings).unwrap();
        let decoded: Vec<FolderEngineKeys> = crate::encoding::canonical_decode(&encoded).unwrap();
        assert_eq!(decoded, bindings);
        // Canonical/dedup-stable: re-encoding the decoded value is byte-identical.
        let re_encoded = crate::encoding::canonical_encode(&decoded).unwrap();
        assert_eq!(encoded, re_encoded);
    }

    /// An entry written without the cross-nest fields must decode
    /// as a **same-nest** set, not fail and not half-route. The blob is app↔agent
    /// lockstep, but an agent can hold a persisted capability across an app
    /// upgrade, so this decode really happens.
    #[test]
    fn legacy_entry_without_cross_nest_fields_decodes_as_same_nest() {
        /// `FolderEngineKeys` without the D5 fields (the identity key it has
        /// always needed since the 2026-09-24 sweep made it required).
        #[derive(Serialize)]
        struct LegacyEngineKeys {
            folder: String,
            folder_id: String,
            #[serde(default, with = "serde_bytes")]
            mls_group_id: Option<Vec<u8>>,
            content_keys: Option<FolderContentKeys>,
        }
        let legacy = vec![LegacyEngineKeys {
            folder: "docs".into(),
            folder_id: FolderRef::Local(1).to_wire(),
            mls_group_id: Some(b"gid".to_vec()),
            content_keys: Some(FolderContentKeys::genesis(k(9), 1_000)),
        }];
        let encoded = crate::encoding::canonical_encode(&legacy).unwrap();
        let decoded: Vec<FolderEngineKeys> = crate::encoding::canonical_decode(&encoded).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].folder, "docs");
        assert!(
            decoded[0].foreign_routing().is_none(),
            "an absent pair must read as same-nest — the pre-Phase-4 behavior"
        );
    }

    #[test]
    fn folder_ref_round_trips_both_arms() {
        for r in [
            FolderRef::Local(42),
            FolderRef::Local(0),
            FolderRef::Local(-1),
            FolderRef::Local(i64::MAX),
            FolderRef::Foreign([0u8; 32]),
            FolderRef::Foreign(k(0xab)),
        ] {
            assert_eq!(FolderRef::parse(&r.to_wire()), Some(r), "{r:?}");
        }
        assert_eq!(FolderRef::Local(42).to_wire(), "local:42");
        assert_eq!(
            FolderRef::Foreign([0xab; 32]).to_wire(),
            format!("foreign:{}", "ab".repeat(32))
        );
    }

    /// An unparseable ref must yield `None` (⇒ the caller refuses the binding)
    /// and must never be *coerced* into some other ref — a wrong identity is exactly the silent wrong-key path this type
    /// exists to close.
    #[test]
    fn folder_ref_rejects_everything_it_did_not_write() {
        for bad in [
            "",
            "docs",                                  // a bare set name
            "local:",                                // truncated
            "local:abc",                             // not an integer
            "local:1.5",                             //
            "foreign:",                              // truncated
            "foreign:zz",                            // not hex
            &"ab".repeat(32),                        // hex with no scheme
            "foreign:ab",                            // hex, but not 32 bytes
            &format!("foreign:{}", "ab".repeat(33)), // 33 bytes
            "remote:42",                             // an unknown scheme from a newer peer
            "LOCAL:42",                              // schemes are exact, not case-folded
        ] {
            assert_eq!(FolderRef::parse(bad), None, "{bad:?} must not parse");
        }
    }

    /// The two arms are structurally disjoint, and their **DB components** must be
    /// too — including against the name-keyed `fs-<sanitized>.db` space the
    /// standalone single-set engines still use.
    #[test]
    fn folder_ref_db_components_cannot_collide() {
        assert_eq!(FolderRef::Local(42).db_component(), "fsid-local-42");
        assert!(
            !FolderRef::Local(42).db_component().starts_with("fs-"),
            "must not land in the name-keyed `fs-` namespace"
        );
        assert_ne!(
            FolderRef::Local(42).db_component(),
            FolderRef::Foreign([0u8; 32]).db_component()
        );
        // A set literally NAMED "local-42" sanitizes to `fs-local-42.db` under the
        // name-keyed namer — which is why the id namespace is `fsid-`, not `fs-`.
        assert_ne!(
            FolderRef::Local(42).db_component(),
            "fs-local-42",
            "the two namespaces must not meet"
        );
    }

    /// The state-DB path is the component plus `.db`, directly under the state
    /// dir — the one join every writer and cross-process reader shares.
    #[test]
    fn state_db_path_is_the_component_under_the_state_dir() {
        let state_dir = std::path::Path::new("/tmp/sync-state");
        assert_eq!(
            FolderRef::Local(42).state_db_path(state_dir),
            state_dir.join("fsid-local-42.db")
        );
        assert_eq!(
            FolderRef::Foreign([0xab; 32]).state_db_path(state_dir),
            state_dir.join(format!("fsid-foreign-{}.db", "ab".repeat(32)))
        );
    }

    /// The one encoder behind every per-device registry keyed by a set: the
    /// wire string as a single percent-encoded component, the same rule apple's
    /// staging root and android's owned-tree roots key their directories by
    /// (alphanumerics and `-_. ` pass, every other byte is `%XX`, uppercase).
    #[test]
    fn folder_ref_path_component_percent_encodes_the_wire_as_one_component() {
        assert_eq!(FolderRef::Local(1).path_component(), "local%3A1");
        assert_eq!(
            FolderRef::Foreign([0xcd; 32]).path_component(),
            format!("foreign%3A{}", "cd".repeat(32))
        );
        assert_eq!(percent_encode_component("a/b"), "a%2Fb");
        assert_eq!(percent_encode_component("keep-_. ok"), "keep-_. ok");
        assert_eq!(
            FolderRef::parse(&FolderRef::Local(1).path_component()),
            None,
            "the component is not a wire form"
        );
    }

    /// The scoped wire form round-trips for both arms, and two accounts' `local:1`
    /// render distinct identifiers — the collision the scope exists to remove.
    #[test]
    fn actor_scoped_folder_ref_round_trips_and_is_distinct_per_actor() {
        let a = [0xaa; 32];
        let b = [0xbb; 32];
        let local_a = FolderRef::Local(1).scoped_to(a);
        assert_eq!(local_a.to_wire(), format!("local%3A1@{}", "aa".repeat(32)));
        assert_eq!(
            ActorScopedFolderRef::parse(&local_a.to_wire()),
            Some(local_a)
        );

        let foreign_b = FolderRef::Foreign([0xcd; 32]).scoped_to(b);
        assert_eq!(
            foreign_b.to_wire(),
            format!("foreign%3A{}@{}", "cd".repeat(32), "bb".repeat(32))
        );
        assert_eq!(
            ActorScopedFolderRef::parse(&foreign_b.to_wire()),
            Some(foreign_b)
        );

        assert_ne!(
            local_a.to_wire(),
            FolderRef::Local(1).scoped_to(b).to_wire(),
            "the same nest row under two accounts is two device identities"
        );
        for scoped in [local_a, foreign_b] {
            let wire = scoped.to_wire();
            assert_eq!(
                wire,
                format!(
                    "{}@{}",
                    scoped.folder.path_component(),
                    hex::encode(scoped.actor_id)
                ),
                "the ref half IS the staging root's component — one spelling"
            );
            assert!(
                !wire.contains('/') && !wire.contains(':'),
                "registry-safe: an OS registry may key a directory by the identifier, \
                 and iOS refuses a File Provider domain identifier carrying '/' or ':'"
            );
            assert!(
                wire.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '%' || c == '@'),
                "the identifier alphabet is alphanumerics, '%' and the one '@'"
            );
        }
    }

    /// A bare ref, a name, a malformed or wrong-length actor and a scoped ref
    /// read as a bare ref all refuse — the two grammars never cross — and so
    /// does the pre-2026-09-29 spelling whose ref half carried the bare `:`,
    /// and any escape `to_wire` would not have written: a device registry
    /// still holding such an identifier keeps it only to remove it.
    #[test]
    fn actor_scoped_folder_ref_refuses_unscoped_and_malformed_identifiers() {
        let hex64 = "aa".repeat(32);
        assert_eq!(ActorScopedFolderRef::parse("local:1"), None, "bare ref");
        assert_eq!(
            ActorScopedFolderRef::parse("local%3A1"),
            None,
            "bare component"
        );
        assert_eq!(ActorScopedFolderRef::parse("docs"), None, "set name");
        assert_eq!(ActorScopedFolderRef::parse(&format!("docs@{hex64}")), None);
        assert_eq!(
            ActorScopedFolderRef::parse(&format!("local:1@{hex64}")),
            None,
            "the pre-2026-09-29 spelling: the ref half carries ':'"
        );
        assert_eq!(
            ActorScopedFolderRef::parse(&format!("local%3a1@{hex64}")),
            None,
            "a lowercase escape is not what to_wire emits"
        );
        assert_eq!(
            ActorScopedFolderRef::parse(&format!("%6Cocal%3A1@{hex64}")),
            None,
            "an escape of a byte the encoder passes through is not either"
        );
        assert_eq!(
            ActorScopedFolderRef::parse(&format!("local%3@{hex64}")),
            None,
            "a truncated escape"
        );
        assert_eq!(
            ActorScopedFolderRef::parse("local%3A1@abcd"),
            None,
            "short actor"
        );
        assert_eq!(
            ActorScopedFolderRef::parse(&format!("local%3A1@{}", "zz".repeat(32))),
            None,
            "non-hex actor"
        );
        assert_eq!(
            ActorScopedFolderRef::parse(&format!("local%3A1@{hex64}@{hex64}")),
            None,
            "a second separator lands in the actor half"
        );
        assert_eq!(
            FolderRef::parse(&format!("local%3A1@{hex64}")),
            None,
            "a scoped identifier is not a bare ref either"
        );
    }

    /// An entry without the identity field is **refused**, never decoded into a
    /// name-keyed binding: the name is a label two sets can share, and the
    /// fallback that resolved by it for a pre-identity app is retired (the
    /// 2026-09-24 compat-remnant sweep).
    #[test]
    fn entry_without_folder_id_is_refused() {
        #[derive(Serialize)]
        struct PreIdentityEngineKeys {
            folder: String,
            #[serde(default, with = "serde_bytes")]
            mls_group_id: Option<Vec<u8>>,
            content_keys: Option<FolderContentKeys>,
        }
        let encoded = crate::encoding::canonical_encode(&vec![PreIdentityEngineKeys {
            folder: "docs".into(),
            mls_group_id: Some(b"gid".to_vec()),
            content_keys: None,
        }])
        .unwrap();
        assert!(
            crate::encoding::canonical_decode::<Vec<FolderEngineKeys>>(&encoded).is_err(),
            "an entry naming its set only by name must not decode"
        );
    }

    /// `foreign_routing` is the ONLY way the routing pair is read, so it is where
    /// the both-or-neither invariant is enforced: a half-populated entry (corrupt
    /// blob) resolves to the same-nest answer, which fails loud at the own nest
    /// rather than silently relaying a write to a nest that never claimed the set.
    #[test]
    fn half_populated_cross_nest_routing_reads_as_same_nest() {
        let url_only = FolderEngineKeys {
            folder: "docs".into(),
            home_nest_url: Some("https://home.example".into()),
            ..Default::default()
        };
        assert_eq!(url_only.foreign_routing(), None);

        let chan_only = FolderEngineKeys {
            folder: "docs".into(),
            channel_id_hex: Some("ab".repeat(32)),
            ..Default::default()
        };
        assert_eq!(chan_only.foreign_routing(), None);

        let both = FolderEngineKeys {
            folder: "docs".into(),
            home_nest_url: Some("https://home.example".into()),
            channel_id_hex: Some("cd".repeat(32)),
            ..Default::default()
        };
        assert_eq!(
            both.foreign_routing(),
            Some(("https://home.example".into(), "cd".repeat(32)))
        );
    }

    #[test]
    fn envelope_payload_round_trips_the_nonce_beside_the_generations() {
        let mut keys = FolderContentKeys::genesis([1u8; 32], 1_000);
        keys.rotate([2u8; 32], 2_000);
        let payload = ContentKeyEnvelopePayload {
            keys: keys.clone(),
            set_nonce: Some([7u8; 32]),
            minted_by: None,
            retired_set_nonces: Vec::new(),
            served_at: None,
            unserved_at: None,
        };
        let back = ContentKeyEnvelopePayload::decode(&payload.encode().unwrap()).unwrap();
        assert_eq!(back, payload);
    }

    /// Ruling (7)(b)(ii) rule (4): the payload carries the owner's serve
    /// stamps — additive, so a never-served set's payload encodes to the bytes
    /// it always did, and a payload sealed before the stamps decodes without.
    #[test]
    fn envelope_payload_round_trips_the_serve_stamps() {
        let keys = FolderContentKeys::genesis([1u8; 32], 1_000);
        let bare = ContentKeyEnvelopePayload {
            keys: keys.clone(),
            set_nonce: Some([7u8; 32]),
            minted_by: None,
            retired_set_nonces: Vec::new(),
            served_at: None,
            unserved_at: None,
        };
        let stamped = ContentKeyEnvelopePayload {
            served_at: Some(300),
            unserved_at: Some(200),
            ..bare.clone()
        };
        let back = ContentKeyEnvelopePayload::decode(&stamped.encode().unwrap()).unwrap();
        assert_eq!(back, stamped);
        #[derive(serde::Serialize)]
        struct Before {
            generations: Vec<ContentKeyGeneration>,
            #[serde(with = "serde_bytes")]
            set_nonce: Option<[u8; 32]>,
        }
        let before = crate::encoding::canonical_encode(&Before {
            generations: keys.generations().cloned().collect(),
            set_nonce: Some([7u8; 32]),
        })
        .unwrap();
        assert_eq!(bare.encode().unwrap(), before);
        assert_eq!(ContentKeyEnvelopePayload::decode(&before).unwrap(), bare);
        assert!(serve_window_open(Some(300), Some(200)));
        assert!(!serve_window_open(Some(200), Some(200)), "a tie is shut");
        assert!(!serve_window_open(None, None));
    }

    /// Ruling (11)(b): the payload carries the live nonce's minter and the
    /// lineage with each nonce's — additive, so a payload without them encodes
    /// to the bytes it always did.
    #[test]
    fn envelope_payload_round_trips_the_minter_and_the_lineage() {
        let keys = FolderContentKeys::genesis([1u8; 32], 1_000);
        let bare = ContentKeyEnvelopePayload {
            keys: keys.clone(),
            set_nonce: Some([7u8; 32]),
            minted_by: None,
            retired_set_nonces: Vec::new(),
            served_at: None,
            unserved_at: None,
        };
        let full = ContentKeyEnvelopePayload {
            minted_by: Some(crate::identity::ActorId([2; 32])),
            retired_set_nonces: vec![
                RetiredSetNonce {
                    nonce: [6u8; 32],
                    minted_by: Some(crate::identity::ActorId([1; 32])),
                },
                RetiredSetNonce {
                    nonce: [5u8; 32],
                    minted_by: None,
                },
            ],
            ..bare.clone()
        };
        let back = ContentKeyEnvelopePayload::decode(&full.encode().unwrap()).unwrap();
        assert_eq!(back, full);
        #[derive(serde::Serialize)]
        struct Before {
            generations: Vec<ContentKeyGeneration>,
            #[serde(with = "serde_bytes")]
            set_nonce: Option<[u8; 32]>,
        }
        let before = crate::encoding::canonical_encode(&Before {
            generations: keys.generations().cloned().collect(),
            set_nonce: Some([7u8; 32]),
        })
        .unwrap();
        assert_eq!(bare.encode().unwrap(), before);
    }

    #[test]
    fn envelope_payload_decodes_the_pre_nonce_bare_generation_array() {
        let keys = FolderContentKeys::genesis([3u8; 32], 1_000);
        let bare: Vec<ContentKeyGeneration> = keys.generations().cloned().collect();
        let bytes = crate::encoding::canonical_encode(&bare).unwrap();
        let back = ContentKeyEnvelopePayload::decode(&bytes).unwrap();
        assert_eq!(back.keys, keys);
        assert_eq!(back.set_nonce, None);
    }

    #[test]
    fn serve_custody_channel_id_is_deterministic_and_per_name() {
        let a = serve_custody_channel_id("docs");
        assert_eq!(a, serve_custody_channel_id("docs"), "deterministic");
        assert_ne!(
            a,
            serve_custody_channel_id("photos"),
            "distinct per set name"
        );
    }

    #[test]
    fn serve_custody_channel_id_is_domain_separated_from_real_channels() {
        // A real ChannelId is blake3::derive_key("fauna.channel.v1", group_id)
        // (fauna-mls). The serve pseudo-channel uses a distinct context, so even
        // the same input bytes can never produce a colliding custody key.
        let input = b"same-bytes-as-a-raw-group-id";
        let pseudo = serve_custody_channel_id(core::str::from_utf8(input).unwrap());
        let real_style = blake3::derive_key("fauna.channel.v1", input);
        assert_ne!(pseudo, real_style);
    }

    /// The bearer door's keys header round-trips a rotated bundle, and the
    /// parse re-derives `current` by the merge's order — a sender's
    /// mis-ordered `current`/`prior` lands on the same bundle.
    #[test]
    fn folder_keys_header_round_trips_and_normalises_order() {
        let mut keys = FolderContentKeys::genesis(k(1), 100);
        keys.rotate(k(2), 200);
        let value = keys.to_header_value().unwrap();
        assert!(!value.contains('=') && !value.contains('+') && !value.contains('/'));
        assert_eq!(FolderContentKeys::from_header_value(&value).unwrap(), keys);

        let swapped = FolderContentKeys {
            current: keys.prior[0].clone(),
            prior: vec![keys.current.clone()],
        };
        let parsed =
            FolderContentKeys::from_header_value(&swapped.to_header_value().unwrap()).unwrap();
        assert_eq!(parsed, keys);
    }

    /// Anything but a canonical bundle is refused, and the error never echoes
    /// the presented value (it may carry key bytes).
    #[test]
    fn folder_keys_header_refuses_garbage_without_echoing_it() {
        let good = FolderContentKeys::genesis(k(9), 1)
            .to_header_value()
            .unwrap();
        for bad in [
            "not base64!".to_string(),
            format!("{good}=="),
            "oA".to_string(), // an empty CBOR map
            "A".repeat(MAX_FOLDER_KEYS_HEADER_LEN + 1),
        ] {
            let err = FolderContentKeys::from_header_value(&bad)
                .unwrap_err()
                .to_string();
            assert!(!err.contains(&bad), "{err}");
        }
    }
}
