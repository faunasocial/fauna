//! WS-RPC payload types for the wrapped-blob ecosystem. The blob
//! field carries `Vec<u8>` (canonical-encoded form); blob structure
//! is interpreted by `fauna-mls::wrapped_blob`.
//!
//! Kind registry entries live in `kind.rs`.

use crate::Value;
use crate::bridge_routing::SpamHistoryOp;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

// ---- Fetch ----

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchWrappedMlsBlobRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    pub credential_id: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchWrappedMlsBlobReply {
    /// Canonical CBOR of `WrappedMsekBlob`, or `None` if not found.
    pub blob: Option<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchMlsSnapshotBlobRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchMlsSnapshotBlobReply {
    pub blob: Option<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchWebdavKeysBlobRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchWebdavKeysBlobReply {
    /// Canonical CBOR of `fauna_mls::wrapped_blob::WebdavKeysBlob`, or `None`
    /// if the actor has no provisioned served-set key blob.
    pub blob: Option<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Access level a bulk-byte token conveys (`webdav-server.md` § Bulk-byte plane).
/// Transport authz only — it never carries decryption capability. `Read` permits
/// the (open) download routes; `Write` additionally permits the upload/check
/// routes. A `Read` token presented to a write route is rejected — defense in
/// depth for the user's read-only-over-WebDAV preference, which the MDA enforces
/// authoritatively at the PUT boundary.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BulkByteAccess {
    /// The least-privilege default, so a fixture built by struct update never
    /// asks for write by accident.
    #[default]
    Read,
    Write,
}

/// What a bulk-byte token is being minted *for*. The mint gate — the only thing
/// that meaningfully differs between them — branches on this.
///
/// Every purpose yields a token of **identical power at the byte layer**: the
/// chunk store is a single global content-addressed store and the token is
/// explicitly "not a chunk-hash ACL" (`webdav-server.md` § Bulk-byte plane), so
/// what the purpose selects is *who may ask and what nest checks before saying
/// yes*, not what the bearer can then reach. That is exactly why this is one
/// discriminator on one mint rather than several mint kinds: a second RPC would
/// read to a future session as if it conferred something different, and it does
/// not.
///
/// One route reads the purpose after the mint, and it selects a roster, not a
/// reach: see [`Self::ForeignFolderRead`].
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BulkByteMintPurpose {
    /// WebDAV folder bytes. **BridgeMda only.** Gate: the named set exists, is
    /// owned by `actor_id`, is not reserved, and is served. The
    /// `#[serde(default)]` — so a request naming no purpose means a folder
    /// mint.
    #[default]
    Folder,
    /// A sealed mail body over the inline budget (`smtp-server.md` § Message size
    /// limits). **BridgeMta or BridgeMda.** Gate: the target actor is a mail
    /// recipient on this nest (it has a recipient seal key). No `folder` — mail
    /// belongs to none, which is why widening the folder gate to admit the MTA
    /// would have been meaningless: every such call would fail `set_not_served`.
    MailBody,
    /// A sealed **content-index segment** the MDA publishes on the `__index` rail
    /// during a MUA session (`content-index.md` § Where the index is built —
    /// rollout S5, the bridge builder leg). **BridgeMda only.** Gate: the target
    /// actor is a mail recipient on this nest — deliberately the same fact
    /// [`Self::MailBody`] gates on, because the MDA's *index* reach is exactly its
    /// *mail* reach (`key-material-hierarchy.md` rule #7: it holds only the
    /// MSEK-derived mail/calendar index-segment key). No `folder` — the
    /// `__index` set is reserved and per-actor, minted lazily by the rail itself.
    ///
    /// **Why a purpose of its own rather than reusing `MailBody`.** The two gates
    /// are the same predicate, so this buys no enforcement — it buys honesty in
    /// the two places the purpose is actually read: the mint is narrowed to the
    /// one bridge class that is a ratified index-builder position (an MTA is
    /// not), and the scope a later audit reads says what the bytes were for. The
    /// byte routes still do not branch on it; see the type docs.
    IndexSegment,
    /// A cross-nest **writer's** short-lived byte-plane token, minted **only** by
    /// the `fauna.federation.folder.write_token.mint` federation handler after
    /// its structural foreign-member + `access == 'writer'` gate — never by a
    /// bridge (`federation.md` § Cross-nest…, contract point (ii)).
    /// Carried on the scope for audit/attribution ("who nest minted this for");
    /// no write route branches on it (the one route that reads it is the read
    /// door named on [`Self::ForeignFolderRead`]). Present in this wire enum so the
    /// bridge mint handler's match is forced to refuse it explicitly (a bridge is
    /// never a foreign-set writer) rather than silently accept a caller-set value.
    ForeignFolderWrite,
    /// A **source nest's** short-lived byte-plane token for writing an owner's
    /// segment-backup custody to this (destination) nest, minted **only** by the
    /// `fauna.federation.backup.write_token.mint` federation handler after its
    /// nest-writer-grant gate (`federation.md` § Nest-writer backup plane) —
    /// never by a bridge. Same audit/attribution-only role as
    /// [`Self::ForeignFolderWrite`]: the byte routes don't branch on it, so the
    /// mint gate is where it is spent. Present here so the bridge mint handler's
    /// match is forced to refuse it explicitly rather than silently accept a
    /// caller-set value.
    NestBackupWrite,
    /// A cross-nest **conversation member's** short-lived byte-plane token for
    /// uploading a sealed attachment DIRECT to the room's home nest, minted
    /// **only** by the `fauna.federation.conversation.write_token.mint`
    /// federation handler after its structural foreign-member gate
    /// (`require_foreign_member` — the `channel.fetch` gate verbatim; no
    /// `access == 'writer'` arm, because a conversation has no claimant and
    /// every member posts). Ratified 2026-09-09: a room's attachment bytes rest
    /// on its home nest beside the record that pins them
    /// (`conversation-rooms.md` § The home nest → *Attachment bytes*). Same
    /// audit/attribution-only role as [`Self::ForeignFolderWrite`]; present here
    /// so the bridge mint handler's match is forced to refuse it explicitly.
    ForeignConversationWrite,
    /// A cross-nest **member's** short-lived byte-plane token for reading a
    /// shared folder's bytes off its home nest, minted **only** by the
    /// `fauna.federation.folder.read_token.mint` federation handler after its
    /// structural foreign-member gate alone — a reader holds no write grant to
    /// mint under (`federation.md` § Cross-nest shared folders + channel append
    /// → *Relay serving across nests*). Access `Read`, so every write route
    /// refuses it. Present here so the bridge mint handler's match is forced to
    /// refuse it explicitly.
    ///
    /// **This and [`Self::ForeignFolderWrite`] are the two purposes a byte
    /// route DOES read**, at one door: the store-miss relay arm of the chunk
    /// route takes a bulk token only when its purpose is one of this pair, and
    /// then resolves the hinted folder through the cross-nest roster alone
    /// (`chunk_routes::relay_chunk_for_folder`). That is not a narrower power
    /// at the byte layer — the hit arm stays an open read by hash — it is the
    /// purpose saying which roster names the caller.
    ForeignFolderRead,
}

impl BulkByteMintPurpose {
    /// Whether the token was minted for a cross-nest member of a shared folder
    /// — the pair the chunk route's relay arm admits a bulk token under.
    pub fn is_foreign_folder(&self) -> bool {
        matches!(self, Self::ForeignFolderWrite | Self::ForeignFolderRead)
    }

    /// Serde skip predicate: a folder mint encodes exactly as it did before this
    /// field existed.
    pub fn is_folder(&self) -> bool {
        matches!(self, Self::Folder)
    }
}

/// `fauna.bridges.mint_bulk_byte_token` — a bridge, on its authed service-user
/// connection, mints a short-TTL token the chunk/manifest HTTP byte routes accept
/// in place of a client bearer it does not hold (`webdav-server.md` § Bulk-byte
/// plane). Bulk bytes never ride WS-RPC (2 MiB cap — `transport.md` § Max frame).
///
/// [`BulkByteMintPurpose`] selects the gate: `Folder` (BridgeMda, served-set
/// check — the enforceable "cross-set" boundary) or `MailBody` (BridgeMta or
/// BridgeMda, recipient-exists check). An MTA may **not** mint a `Folder` token:
/// admitting the MTA to the byte plane for mail deliberately did not open the
/// folder path to it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MintBulkByteTokenRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// The served set the MDA is moving bytes for. Gates the mint (served +
    /// non-reserved + owned) and is the QUOTA/audit attribution key (slice 4). It is
    /// NOT a chunk-hash ACL: the chunk store is a single global content-addressed
    /// store, so per-set byte partitioning is impossible without a forbidden
    /// mirror index — confidentiality between sets is the M2 content key.
    ///
    /// Empty (and ignored) when `purpose` is `MailBody` — mail has no folder —
    /// and empty from the WebDAV MDA, which addresses the set by `name_hash`
    /// alone (`path-sealing.md` § the set-name plane); omitted on the wire
    /// when empty. The attribution key is the resolved row id, never this.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    pub access: BulkByteAccess,
    /// `#[serde(default)]` → `Folder`, so a request naming no purpose is a
    /// folder mint; `skip_serializing_if` keeps a folder mint's encoding free
    /// of the key, so the WebDAV path carries no purpose on the wire.
    #[serde(default, skip_serializing_if = "BulkByteMintPurpose::is_folder")]
    pub purpose: BulkByteMintPurpose,
    /// Hash-first addressing (S5b) — see `crate::folders::FolderUpdateRequest::name_hash`.
    /// The MDA stamps it from the served set's name (`set_name_hash`, through the
    /// shared-Rust FFI export) so the set stays addressable once the nest's
    /// plaintext name blanks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MintBulkByteTokenReply {
    /// Opaque short-TTL bearer the byte routes accept (a `BulkByteTokenStore`
    /// key, disjoint from the full-session `TokenStore` so it can never be
    /// replayed as a session bearer on WS-RPC / other routes).
    pub token: String,
    /// Absolute expiry, Unix seconds.
    pub expires_at: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── WebDAV data-plane kinds (`webdav-server.md` § MDA↔nest WS-RPC contract) ──
//
// The three `BridgeMda` kinds the Go MDA WebDAV terminator (slice 4) drives to
// serve the folder substrate over RFC 4918: enumerate the actor's served
// sets, list a served set's latest-per-path files, and record a WebDAV write as
// an ordinary folder change. Co-located here with the other WebDAV kinds
// (`fetch_webdav_keys_blob` / `mint_bulk_byte_token`) rather than the CardDAV
// data-plane siblings in `bridge_routing.rs`, so the whole WebDAV kind family
// reads from one file (the goal doc's ":105 bridge_routing.rs" was superseded by
// slices 1b/3 landing the family here).

/// `fauna.bridges.webdav_list_folders` — the actor's WebDAV-served folders
/// (non-reserved + per-set `webdav_enabled`). The nest-authoritative served-set
/// enumeration that drives the MDA's root-collection listing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebdavListFoldersRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One served set in a [`WebdavListFoldersReply`]. **Name only** — `read_only`
/// is NOT sourced here: nest has no per-set read-only column (`webdav-server.md`
/// § Bulk-byte plane / § Key model); the MDA reads each set's `read_only` from
/// the MSEK-sealed `WebdavKeysBlob` it fetches via `fetch_webdav_keys_blob`, and
/// enforces it authoritatively at the PUT boundary (Guard 2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebdavServedSet {
    pub name: String,
    /// The set's `set_name_hash` (`path-sealing.md` § the set-name plane): the
    /// MDA matches a request's set by it and takes the display name from its
    /// `WebdavKeysBlob`, so a sealed set whose row holds no plaintext name is
    /// still served. `None` for a reserved `__` set, which has no hash. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebdavListFoldersReply {
    pub folders: Vec<WebdavServedSet>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.webdav_quota` — the actor's file-storage usage and ceiling,
/// for the MDA's RFC 4331 `quota-used-bytes` / `quota-available-bytes`
/// (`webdav-server.md` § Protocol surface (v1) and deliberate deferrals). The
/// same meter and tier ceiling `webdav_record_change` enforces, so the space a
/// file manager shows is the space a PUT is refused against.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebdavQuotaRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebdavQuotaReply {
    /// Bytes charged to the actor's storage meter (`users.storage_bytes_used`).
    pub storage_bytes_used: u64,
    /// The actor's tier storage ceiling; absent when the actor has no tier
    /// ceiling (the record path then meters without a cap).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_bytes_limit: Option<u64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.webdav_admit_principal` — the WebDAV bearer door's admission
/// relay (`webdav-server.md` § Key model → *A principal's read*, (6)). The MDA
/// hands over what a principal's DAV request presented — RFC 9449's
/// `Authorization: DPoP <token>` and its `DPoP` proof(s) — with the request's
/// method and URL, and the nest runs its one principal admission over them (the
/// issuer verifying its own token; a revoked family refused). BridgeMda only.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WebdavAdmitPrincipalRequest {
    /// The access token, verbatim from `Authorization: DPoP <token>`.
    pub token: String,
    /// Every `DPoP` proof header the request carried, verbatim — the
    /// admission's proof gate refuses anything but exactly one.
    pub dpop_proofs: Vec<String>,
    /// The request's method — the proof's `htm`.
    pub htm: String,
    /// The request's absolute `https` URL — the proof's `htu`. Must name a
    /// path under `/webdav/`; anything else is refused before the admission
    /// runs, so the relay cannot vouch a proof minted for another door.
    pub htu: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The answer to [`WebdavAdmitPrincipalRequest`]: `admitted` on success; on
/// refusal `None` with the HTTP challenge the MDA answers the DAV client with,
/// verbatim — status, `WWW-Authenticate` and the nest's fresh `DPoP-Nonce` —
/// so a client that sent no nonce learns one, as at every nest door.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WebdavAdmitPrincipalReply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admitted: Option<WebdavAdmittedPrincipal>,
    /// The HTTP status of a refusal (`401`, `403`, `429`, `503`); `0` when
    /// admitted.
    #[serde(default)]
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub www_authenticate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpop_nonce: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// An admitted principal, as the bearer door serves it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WebdavAdmittedPrincipal {
    /// The account the token names — the `{user}` the door serves.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// The X25519 holder key the principal attested at consent — the key its
    /// read twin is wrapped to. Empty when it attested none (then no
    /// `folders` entry is live: there is nothing a grant could be wrapped to).
    #[serde(with = "serde_bytes", default)]
    pub holder_x25519: Vec<u8>,
    /// The principal's granted scopes ∩ the token's — every string, so the
    /// MDA reads its `folder:read` arms through the one grammar.
    pub scopes: Vec<String>,
    /// The access token's `exp`, epoch seconds — the admission's cache ceiling.
    pub exp: i64,
    /// One entry per `fauna:folder:read:<id>` scope in [`Self::scopes`].
    pub folders: Vec<WebdavAdmittedFolder>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One folder a `folder:read` scope names, re-resolved at the admission.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WebdavAdmittedFolder {
    pub folder_id: i64,
    /// The set's `set_name_hash` — what the MDA matches the keys header's set
    /// and the folder substrate's rows by. Empty for an id that is not a
    /// hashed folder of the account (then `grant_live` is `false`).
    #[serde(with = "serde_bytes", default)]
    pub name_hash: Vec<u8>,
    /// Whether a LIVE `content.read{folder, set}` grant over this folder,
    /// owned by the account and held by the principal's holder key, exists
    /// now — the deposit door's re-resolution, kept where the grant rows are.
    pub grant_live: bool,
    /// Whether the folder is served over WebDAV now (the nest's served gate,
    /// the one the Basic door's data plane reads) — the door's backstop
    /// behind the unserve's revoke. The MDA serves a folder only when both
    /// this and `grant_live` hold.
    pub served: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.webdav_list_files` — one served set's latest-per-path files.
/// Gated: the named set must exist, be owned by `actor_id`, be non-reserved, and be
/// served (else `set_not_served`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WebdavListFilesRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// The served set by name — empty from the MDA, which sends `name_hash`
    /// alone; omitted on the wire when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// Hash-first addressing (S5b) — see `crate::folders::FolderUpdateRequest::name_hash`.
    /// The MDA stamps it from the served set's name (`set_name_hash`, through the
    /// shared-Rust FFI export) so the set stays addressable once the nest's
    /// plaintext name blanks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One file in a [`WebdavListFilesReply`] — the WebDAV twin of `SyncFile`, plus
/// the content-key generation the MDA needs to GET-decrypt. (`SyncFile` omits
/// `content_key_version`; it rides the `fauna.sync.changes` feed instead, which
/// the MDA does not consume — the MDA sees only this listing.)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebdavFile {
    /// Normalized forward-slash relative path within the set.
    pub path: String,
    /// Hex BLAKE3 manifest hash — the WebDAV `getetag`.
    pub manifest_hash: String,
    /// WebDAV `getcontentlength`.
    pub size_bytes: i64,
    /// WebDAV `getlastmodified` (the change `created_at`, epoch milliseconds).
    pub updated_at: i64,
    /// The `FolderContentKeys` generation these chunks were sealed under; the
    /// MDA tries every `keys_for(version)` candidate to decrypt (fail-closed if it lacks that
    /// generation). `None` for owner-only and pre-bind rows (a served set is always
    /// content-key-sealed, so its live rows carry it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_key_version: Option<u64>,
    /// The client's opaque `SealedLabel` envelope over `path`
    /// (`file-sync.md` § Sealed names & paths). The MDA renders this
    /// sealed-first under the `WebdavKeysBlob` content keys it already holds,
    /// since the plaintext `path` above rests only on a plaintext-resting plane
    /// (S9). `None` on rows a keyless writer recorded there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_sealed: Option<ByteBuf>,
    /// The row's `path_hash` — **the salt `path_sealed` opens under**, not a
    /// convenience. A `path` seal is convergent (its nonce derives from the
    /// salt), so a reader holding the seal but not this hash cannot render the
    /// name the moment the plaintext is scrubbed. Carried for exactly the same
    /// reason `fauna.media.list` carries it (S2b).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebdavListFilesReply {
    pub files: Vec<WebdavFile>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.webdav_record_change` — record a WebDAV write (PUT / DELETE
/// tombstone / MOVE-COPY manifest re-record) as an ordinary folder change,
/// attributed to the actor's stable `"WebDAV"` pseudo-device (registered
/// write-capable, idempotent) so forwarding, self-echo skip, the Devices roster,
/// and the conflict machinery behave normally (`webdav-server.md`
/// § Protocol surface — Device identity). Gated identically to
/// `webdav_list_files`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WebdavRecordChangeRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// The served set by name — empty from the MDA, which sends `name_hash`
    /// alone; omitted on the wire when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// Plaintext relative path; the nest hashes it to `path_hash`.
    pub path: String,
    /// Hex manifest hash; `None` for a DELETE tombstone.
    #[serde(default)]
    pub manifest_hash: Option<String>,
    pub size_bytes: i64,
    /// `"create"` | `"modify"` | `"delete"` — the SAME lowercase tokens the sync
    /// feed's own writes use (`fauna-sync-engine::engine`), so a WebDAV write
    /// applies through the identical `apply_changes` arm on every synced device.
    /// (NOT `"Created"`/`"Modified"`/`"Deleted"` — the sync apply match is
    /// case-sensitive; the wrong case silently drops the change as "unknown".)
    pub change_type: String,
    /// The content-key generation the MDA sealed these chunks under (its
    /// `WebdavKeysBlob` `current` version at PUT). `None` for a delete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_key_version: Option<u64>,
    /// WebDAV `If-Match`: require the path's current manifest-hash ETag to equal
    /// this hex value (`"*"` = require the path present). Enforced at the nest
    /// (the write serialization point) so a lost race is authoritatively a
    /// `conflict` → `412` (`webdav-server.md` § Protocol surface).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_match: Option<String>,
    /// WebDAV `If-None-Match`: `"*"` = require the path absent (create-only PUT).
    /// If the path is present → `conflict` → `412`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_none_match: Option<String>,
    /// The MDA's `SealedLabel` envelope over `path`, sealed bridge-side under
    /// the served set's `current` content-key generation before this call
    /// (`webdav-server.md` § Key model — the nest holds no key that could seal
    /// it). This is what makes the DAV write leg an ordinary *keyed* writer
    /// rather than one of the named keyless seams. `None` when the served set has no content keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_sealed: Option<ByteBuf>,
    /// Hash-first addressing (S5b) — see `crate::folders::FolderUpdateRequest::name_hash`.
    /// The MDA stamps it from the served set's name (`set_name_hash`, through the
    /// shared-Rust FFI export) so the set stays addressable once the nest's
    /// plaintext name blanks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebdavRecordChangeReply {
    /// The monotonic sequence number assigned to this change.
    pub seq: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchWrappedSubmissionTokenRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    pub credential_id: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchWrappedSubmissionTokenReply {
    pub blob: Option<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchTlsCertBlobRequest {
    pub bridge_role: String,
    pub bridge_id: String,
    pub domain: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchTlsCertBlobReply {
    pub blob: Option<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchBridgePubkeyRequest {
    pub bridge_role: String,
    pub bridge_id: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchBridgePubkeyReply {
    #[serde(with = "serde_bytes")]
    pub ed25519_pubkey: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub x25519_pubkey: Vec<u8>,
    /// Post-quantum sibling (PQ-CAP-3): the holder's published 1184-byte
    /// ML-KEM-768 encapsulation key (`bridge_service_users.mlkem_ek`, written by
    /// PQ-CAP-2's `register_service_user`), or `None` for a holder that hasn't
    /// published one. The client mint reads it to seal capability grants X-Wing
    /// to `from_parts(mlkem_ek, x25519_pubkey)`; absent ⇒ the classical wrap (the
    /// non-erroring degrade). `skip_serializing_if` keeps a classical-holder reply
    /// byte-identical to the pre-PQ-CAP wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mlkem_ek: Option<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.bridges.fetch_spam_model` — fetch the per-user
/// Bayesian spam model sealed to `actor_id`, so an authenticated agent
/// scores inbound mail on-device at the search-equivalent position
/// (`mail-spam.md` § Scoring placement, § Encrypted-mode interaction).
///
/// **Caller-binding:** a `User`/`Admin` caller may fetch only their **own**
/// model — the handler enforces `target == caller`,
/// even for an admin (no one reads another user's model — `mail-spam.md`
/// § Cross-actor isolation). `BridgeMda` keeps the *trusted-naming* model:
/// it serves the local-mail actor whose AUTH'd session it didn't itself
/// authenticate as (the same trust model as `put_spam_model`), bounded by
/// `require_local_mail_serving` + the bridge approval gate.
///
/// The User/Fauna-app leg (the on-device post-decrypt scorer, e.g. the
/// android per-user posts scorer) is **enabled**: the content-reconstruction
/// oracle that previously kept this MDA-only (a User-facing readback + the
/// no-authz `fauna.moderation.train(arbitrary content_id)`) is closed —
/// `train` now gates the body read on the caller being able to read the
/// post, and this fetch is caller-scoped (reviews tracked
/// internally).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchSpamModelRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.bridges.fetch_spam_model`. `blob` is the actor's
/// per-user model **sealed to their MSEK-derived key** — the same
/// wrapped-blob shape (`fauna_mls::wrapped_blob`) as the mail body and
/// the per-message index hint. The authenticated agent unwraps it under
/// its session MLS capability (exactly as it opens a sealed body for
/// SEARCH), then runs the shared scorer
/// (`weighted_bayesian_milli_for_model`). `None` ⇒ the actor has no
/// trained model and no baseline is published ⇒ cold start (the scorer's
/// weight is 0 below `bayesian_min_samples`). nest holds only the public half,
/// so it can seal *to* the actor but never reads back what it sealed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FetchSpamModelReply {
    pub blob: Option<ByteBuf>,
    /// `true` iff `blob` is the actor's **stored** model (always sealed at
    /// rest, returned verbatim). `false` ⇒ the actor has no stored model and
    /// `blob` — when present — is the **cold-start seed**: the published
    /// deployment baseline folded onto a fresh model and **sealed on read**
    /// (nothing is persisted). The wire shape is a sealed envelope either
    /// way, so the reply alone can't otherwise tell them apart. Every train
    /// position dispatches on it: `true` → open the blob → mutate → re-seal →
    /// `put_spam_model`; `false` → train from an **empty** model (never from
    /// the seed — the fold is read-time only, `mail-spam.md` § Cold start
    /// Path 2) → seal → `put_spam_model`. `#[serde(default)]` ⇒ an absent key
    /// decodes `false`.
    #[serde(default)]
    pub stored_sealed: bool,
    /// The published deployment baseline (the plaintext aggregate
    /// `SpamModel` serde_json), present **exactly when the nest did not
    /// fold it** — i.e. only alongside a stored model (`stored_sealed:
    /// true`), which the nest cannot merge into.
    /// The agent applies the faded fold locally
    /// (`SpamModel::merge_scaled` against its own `sample_count` — the
    /// no-double-fold rule, `mail-spam.md` § Encrypted-mode interaction).
    /// Absent ⇒ no non-empty baseline is published, or the returned model
    /// already carries the server-side fold. The nest never withholds it
    /// on the untrusted advisory `sample_count` — the agent's fade
    /// contributes zero at full confidence anyway. Additive: `None` (no
    /// baseline published) means no cold-start seeding for a sealed model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<ByteBuf>,
    /// `true` iff this actor has opted into the deployment baseline
    /// (`spam_preferences.contribute_baseline`) — the opt-in-contributor **write
    /// signal** (additive, piece (b), ratified 2026-07-14, `mail-spam.md` § Wire
    /// shapes). Paired with a [`Self::holder_seal_target`], every sealed-model
    /// `put_spam_model` the writing agent (client or MDA `\Junk`-train) emits
    /// **also attaches a fresh `holder_copy`** sealed to that target (the
    /// re-seal-on-every-write rule, § Encrypted-mode interaction). Scoring
    /// ignores it. `#[serde(default)]` ⇒ an omitted key reads as
    /// `false` (attach no copy).
    #[serde(default)]
    pub contribute_baseline: bool,
    /// The box's **aggregation-holder** seal target — the X25519 pubkey (+
    /// optional ML-KEM ek) the opted-in contributor seals its `holder_copy` to
    /// and mints the keyless `content.read{spam-model}` grant against. The nest
    /// volunteers its **own** content-processor holder (the same identity
    /// `holder_granted_scopes` gates the publish worklist on; its pubkey is
    /// already `User`-fetchable via `fetch_bridge_pubkey`, so volunteering it
    /// here leaks no service-user roster), so a **non-admin contributor mints +
    /// seals without the Admin-gated `list_service_users`** (§ Wire shapes; §
    /// Encrypted-mode interaction — this makes `nest-trust-holder-discovery` a
    /// non-blocker for the baseline feature). `None` ⇒ no holder enrolled ⇒ the write attaches no copy and the toggle sets the bit
    /// only. Additive, piece (b), 2026-07-14.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder_seal_target: Option<HolderSealTarget>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The aggregation-holder **seal target** the nest volunteers on a caller's own
/// [`FetchSpamModelReply`] (`holder_seal_target`) so an opted-in contributor can
/// seal its holder copy and mint the keyless `content.read{spam-model}` grant
/// **without** enumerating the service-user roster (the Admin-gated
/// `list_service_users`). The nest resolves its own content-processor holder —
/// the same identity the publish worklist gates on (`holder_granted_scopes`) —
/// whose X25519 pubkey is already `User`-fetchable via `fetch_bridge_pubkey`, so
/// volunteering it here leaks no roster (`mail-spam.md` § Wire shapes; §
/// Encrypted-mode interaction, ratified 2026-07-14). The same target keys both
/// `seal_spam_model_copy` (the copy seal) and the grant's `holder_pubkey`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HolderSealTarget {
    /// The aggregation holder's X25519 pubkey (32 bytes) — the seal target for
    /// both the `holder_copy` (`seal_spam_model_copy`) and the keyless grant's
    /// `holder_pubkey` (they pair on this identity).
    #[serde(with = "serde_bytes")]
    pub x25519_pubkey: Vec<u8>,
    /// The holder's ML-KEM-768 encapsulation key, present iff the holder has
    /// published one — selects the post-quantum **X-Wing** seal suite (the same
    /// ek-presence gate as the capability-grant wrap selector, PQ-4b); absent ⇒
    /// classical X25519 seal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mlkem_ek: Option<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.bridges.put_spam_model` — the **opaque sealed write-back**
/// twin of [`FetchSpamModelRequest`] (the opaque read). When the per-user spam
/// model is sealed at rest (nest-opaque), the nest can no longer read-mutate-write
/// it, so the client (which holds the user's key) runs the whole
/// unwrap → mutate → **re-seal** loop and writes the resulting blob back via this
/// kind. The nest stores `sealed_model` **verbatim** into `spam_models.model_json`
/// (opaque bytes — no decode, no merge, no count-from-blob); it never sees the
/// plaintext model (`mail-spam.md:355` — "re-seals the whole model … nest stores
/// the sealed blob opaque, never merging"). It is the ONE model writer: the nest
/// itself never trains, undoes or merges a per-user model, and refuses a
/// `sealed_model` that is not sealed (a plaintext `SpamModel` decodes) and a
/// history insert whose subject or delta is not sealed (`invalid_params`).
///
/// **Caller-binding — two paths (leg 2 widened this to `BridgeMda`):**
/// - **`User`/`Admin` (the client's own-key write) — caller-scoped.**
///   [`Self::actor_id`] must be **empty** (the pre-leg-2 client shape; the
///   connection's authenticated actor *is* the subject) or name the caller
///   itself; naming anyone else is **rejected, never silently redirected to
///   self** — the mirror on the write twin, so a buggy
///   privileged client gets an error instead of an `ok` that overwrote its
///   OWN row (even an admin writes only their own — `mail-spam.md`
///   § Cross-actor isolation).
/// - **`BridgeMda` (the MDA agent-side `\Junk`-train re-seal, leg 2) — trusted
///   naming.** The MDA serves a local-mail actor whose AUTH'd session it did not
///   authenticate as, so it names the target in [`Self::actor_id`] — the same
///   trusted-naming model as `fetch_spam_model`, bounded by
///   `require_local_mail_serving`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PutSpamModelRequest {
    /// The whole re-sealed model — `seal_to_recipient(model.to_bytes(),
    /// own_recipient_pubkey)` canonical bytes, a bare inner `wrapped_blob` (the
    /// exact shape [`FetchSpamModelReply::blob`] returns). Bounded client-side by
    /// `SpamModel::cap_to_bytes` before sealing (the seal is opaque, so the client
    /// is the only place the at-rest cap can be enforced) and by the inbound WS
    /// frame cap on the wire.
    #[serde(with = "serde_bytes")]
    pub sealed_model: Vec<u8>,
    /// The post-mutation training-document count — **advisory** metadata for the
    /// settings display only. The nest **NEVER** trusts it for anything (it cannot
    /// verify it against the opaque blob); it lands in an advisory column and is
    /// never used for a security or merge decision.
    #[serde(default)]
    pub sample_count: u32,
    /// Optional training-history mutation applied **atomically** with the model
    /// re-seal (option (a) of the co-design, `mail-spam.md` § Training-sample
    /// retention — build-item 3 write side). A client-path **train** rides an
    /// [`SpamHistoryOp::Insert`] (its client-sealed subject + delta); a client-path
    /// **undo** rides a [`SpamHistoryOp::Delete`] (the client already applied the
    /// inverse locally, so this only removes the consumed audit row). `None` ⇒ a
    /// model-only write. Carried on the same kind (not a separate
    /// `put_spam_training_history` RPC) so `{model-write + history-INSERT}` /
    /// `{model-write + history-DELETE}` are one atomic transaction — no
    /// crash-between-two-RPCs half-state (a model updated with no undo row, or an
    /// orphan row for a model that didn't change). `#[serde(default)]` ⇒ absent
    /// from a pre-build-item-3-write client decodes as `None`.
    #[serde(default)]
    pub history_op: Option<SpamHistoryOp>,
    /// The actor whose sealed model this writes. A `BridgeMda` caller (the MDA
    /// agent-side `\Junk`-train re-seal, leg 2) names the served actor here —
    /// trusted-naming, bounded by `require_local_mail_serving` (the same model
    /// as `fetch_spam_model`). A `User`/`Admin`
    /// caller leaves it **empty** (⇒ the connection's actor is the subject) or
    /// names itself; naming any other actor is **rejected**even an admin writes only their own model,
    /// and a mismatch errors rather than silently writing self).
    /// `#[serde(default)]` ⇒ empty/absent from a client that only ever writes
    /// its own model (the pre-leg-2 wire shape decodes cleanly).
    #[serde(default, with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// Optional deployment-baseline **holder copy** stored atomically with
    /// the model write (see [`SpamModelHolderCopy`]). Present ⇒ the writing
    /// agent re-sealed the post-mutation model to the aggregation holder
    /// (the actor is opted in); the nest replaces the stored copy for
    /// `(actor, holder_pubkey)` in the same transaction. Absent ⇒ the stored
    /// copy (if any) is left untouched — a writer that sends no copy
    /// merely leaves slightly-stale weights for the next
    /// publish, never drops the contributor (`mail-spam.md` § Encrypted-mode
    /// interaction, freshness ground 5). `#[serde(default)]` ⇒ absent on
    /// the wire decodes as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder_copy: Option<SpamModelHolderCopy>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// A spam-model deployment-baseline **holder copy** riding a
/// `put_spam_model` write — the contributor-side half of the ratified
/// keyless `content.read{spam-model}` shape (`mail-spam.md` § Encrypted-mode
/// interaction, 2026-07-13). The writing agent (client or AUTH'd MDA
/// session), while the actor is opted into the deployment baseline, seals a
/// *copy* of the post-mutation model to the aggregation holder's own pubkey
/// (`fauna_mls::wrapped_blob::seal_spam_model_copy` — a `SpamModelCopyBlob`)
/// and ships it atomically with the model write, so the holder-readable copy
/// never lags the model. The nest stores it **opaque** beside the
/// `spam_models` row and serves it to the holder during a
/// `publish_spam_baseline` drain run only while the paired keyless grant
/// stands; it is deleted on opt-out, grant revocation, or model reset. No
/// key of the user's ever rides this — the copy is sealed to the *holder's*
/// key material.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SpamModelHolderCopy {
    /// The aggregation holder's X25519 pubkey (32 bytes) the copy is sealed
    /// to — the same identity `capability_grants.holder_pubkey` keys grants
    /// by, so the nest can pair copy and grant without parsing either.
    #[serde(with = "serde_bytes")]
    pub holder_pubkey: Vec<u8>,
    /// The sealed copy: `SpamModelCopyBlob` canonical bytes, nest-opaque.
    #[serde(with = "serde_bytes")]
    pub sealed_copy: Vec<u8>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// What a `fauna.bridges.put_spam_model` write did — [`PutSpamModelReply::outcome`].
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PutSpamModelOutcome {
    /// The model (and the history op, if any) committed.
    #[default]
    Written,
    /// The `history_op: Insert` repeated the actor's newest recorded lesson for
    /// that message — the one-lesson rule (`mail-spam.md` § 3) — so the nest
    /// wrote NOTHING: not the model, not the row, not the holder copy. The
    /// stored model is byte-identical to the last accepted write; a writer
    /// carrying a mutated model forward (a multi-message batch) discards this
    /// mutation and continues from the last accepted one.
    DuplicateSignal,
    /// An outcome a newer build added that this build cannot read
    /// (`transport.md` § Schema and forward-compat discipline, rule 3: open,
    /// collapsing). It reads as not written: a writer carrying a mutated model
    /// forward discards its mutation, as for
    /// [`PutSpamModelOutcome::DuplicateSignal`]. Never serialized: a path that
    /// would re-emit it fails instead of replacing the newer value.
    #[serde(other, skip_serializing)]
    Unknown,
}

/// Reply for `fauna.bridges.put_spam_model`: an ack plus the
/// [`PutSpamModelOutcome`] — `written` for the ordinary whole-row replace of
/// the opaque blob (re-writing the same sealed bytes is a no-op-equivalent
/// overwrite), `duplicate_signal` when the one-lesson rule rejected the write.
/// `outcome` is additive: an omitted key decodes as `Written`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PutSpamModelReply {
    /// What the write did — see [`PutSpamModelOutcome`].
    #[serde(default)]
    pub outcome: PutSpamModelOutcome,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.bridges.get_spam_scoring_policy` — the User-reachable
/// read of the admin-effective spam-scoring policy the on-device Fauna-app
/// scorer needs so its INBOX→Junk line matches the MDA/nest byte-for-byte
/// (`mail-spam.md` § Architectural rules "Scoring placement = search
/// placement"). The values are deployment-wide (no per-user override today)
/// and non-secret deployment policy; a `User` may read them (the Admin-only
/// `fetch_config` / `get_mail_config` is unreachable to a client). The caller
/// is implicit (the connection's authenticated actor), so the request carries
/// no fields beyond the forward-compat catch-all.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GetSpamScoringPolicyRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.bridges.get_spam_scoring_policy` — the four admin-effective
/// scoring knobs the on-device scorer must consume so a combined score re-files
/// INBOX→Junk at the identical threshold the MDA/nest use (`mail-spam.md`
/// § Combined-score formula). Projected from the effective
/// [`crate::bridge_routing::SpamPolicyThresholds`] (catalog defaults with the
/// admin `put_spam_policy` override applied) — the same effective values the
/// MDA reads via `fetch_config`, so placement is byte-identical at every
/// scoring position.
///
/// **Deriving `PartialEq` (not `Eq`):** the `extra` catch-all holds
/// `ipld_core::ipld::Ipld` (an unknown newer-peer key could be a float), which
/// is not `Eq` — the same reason [`FetchSpamModelReply`] omits `Eq`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GetSpamScoringPolicyReply {
    /// Effective `spam_folder` tier (0–15 points; 0 = auto-Junk disabled). A
    /// combined score `>= spam_folder_threshold * 1000` (milli) re-files
    /// INBOX→Junk. Projected from `SpamPolicyThresholds
    /// ::max_score_before_spam_folder` (default 5).
    pub spam_folder_threshold: u32,
    /// Effective Tier-2 per-user-Bayesian combined-score weight, milli
    /// (default 700). Must match what the MDA reads via `fetch_config`.
    pub bayesian_weight_milli: u32,
    /// Cold-start sample floor below which the per-user Bayesian term is 0
    /// (default 50).
    pub bayesian_min_samples: u32,
    /// Confidence-ramp full-confidence + baseline-fade horizon (default 200).
    pub bayesian_full_confidence_samples: u32,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ---- Provision ----

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvisionWrappedMlsBlobRequest {
    pub blob: ByteBuf,
    /// The credential the blob is sealed under (kebab-case, "default" for the
    /// first). Nest keys the per-credential `bridge_wrapped_mls_blobs` row on
    /// `(actor_id, credential_id)` — matching `fetch_wrapped_mls_blob` /
    /// `revoke_wrapped_mls_blob` — so multiple credentials per actor coexist
    /// (mail-credentials.md § MUA-username; the MDA fetches by the username's
    /// RFC 5233 sub-address suffix).
    pub credential_id: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvisionMlsSnapshotBlobRequest {
    pub blob: ByteBuf,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Provision/refresh the actor's WebDAV served-set key blob (the MSEK-sealed
/// `WebdavKeysBlob` — `webdav-server.md` § Key model). User-class; the client
/// re-provisions on any served-set or content-key-generation change. Nest keys
/// the single-row-per-actor `bridge_webdav_keys_blobs` table on `actor_id` and
/// stores `blob` opaque (never decodes it). Reply is the shared [`ProvisionReply`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvisionWebdavKeysBlobRequest {
    pub blob: ByteBuf,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvisionWrappedSubmissionTokenRequest {
    pub blob: ByteBuf,
    /// The credential the submission token is sealed under (see
    /// [`ProvisionWrappedMlsBlobRequest::credential_id`]). Keys the
    /// per-credential `bridge_wrapped_submission_tokens` row.
    pub credential_id: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvisionTlsCertBlobRequest {
    pub blob: ByteBuf,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvisionReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ---- DKIM selector management (admin read/manage surface) ----
//
// The nest holds each DKIM private key and no kind carries one. These kinds
// expose the public metadata the admin DNS page renders — the selectors with
// their DNS records — and a retire path. See
// `docs/goal/behavior/mail-bridge-lifecycle.md` § DKIM provisioning (automatic).

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListDkimSelectorsRequest {
    /// Restrict to one domain; `None` enumerates every selector.
    pub domain: Option<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Public metadata for one `(domain, selector)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DkimSelectorInfo {
    pub domain: String,
    pub selector: String,
    /// Epoch-millis when the selector's key was minted.
    pub created_at: u64,
    /// Public DNS TXT record to publish at `<selector>._domainkey.<domain>`.
    pub public_dns_value: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListDkimSelectorsReply {
    pub selectors: Vec<DkimSelectorInfo>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RevokeDkimBlobRequest {
    pub domain: String,
    pub selector: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ---- Bridge service-user enumeration (admin) ----
//
// The admin's deliverability page needs to identify the approved MTA bridge
// to seal DKIM/TLS blobs to (its `bridge_id` feeds `fetch_bridge_pubkey`),
// and to gate provisioning on at least one approved MTA. This read enumerates
// the mail service-user bridges (distinct from the user-facing `fauna.bridges.list`
// of external link bridges). See `mail-bridge-lifecycle.md` § DKIM provisioning UX.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListServiceUsersRequest {
    /// Restrict to one role (`"mta"` / `"mda"`); `None` lists both.
    pub role: Option<String>,
    /// Restrict to one status (`"pending"` / `"approved"` / `"revoked"`);
    /// `None` lists all.
    pub status: Option<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// What a co-resident bridge observed about **its own confinement** at
/// startup, self-reported at `register_service_user`
/// (`security.md` § Co-resident process trust boundary → *Confinement
/// self-probe*).
///
/// **These are PROVISIONING DIAGNOSTICS, never a security attestation.** A
/// self-report from a *compromised* bridge is untrustworthy by definition —
/// the value is that it catches the honest misconfiguration (a compose that
/// bypasses `fauna-sandbox`, a Landlock-less kernel, a docker seccomp policy
/// blocking the landlock syscalls) on a *deployed* box, where the trust-bearing
/// proof — the tier_4 in-domain kernel-LSM probes — cannot run. It exists
/// because a provisioned box carries no ssh key (`testing.md` § Gap 3), so
/// before this the slice-1/4 facts were provable only by a supervised on-box
/// pass; it is the same **no-SSH observable** shape as [`MailEnrollmentStrict`].
///
/// The fields split deliberately along *who measured them*:
///
/// - [`Self::uid`] and [`Self::sealed_store`] are measured **first-hand by the
///   bridge, inside its own sandbox** — the kernel answered, nobody relayed it.
///   `sealed_store` is the property that actually bounds the blast radius ("the
///   sealed store is unreachable from this process"), but it attributes
///   nothing: the DAC UID alone would produce the same denial.
/// - [`Self::landlock`] and [`Self::seccomp`] are **relayed** — the enforcement
///   status is known only to the `fauna-sandbox` wrapper (which learns it from
///   `RulesetStatus` and exports it across `execvp`) and to `/proc/self/status`.
///   Only these attribute a denial to the kernel LSM rather than to DAC.
///
/// Every string field is a small open vocabulary rather than an enum: a newer
/// bridge may report a state this nest predates, and additive-everywhere
/// (`version-compatibility.md`) means that must survive rather than be rejected.
/// Nest bounds them on admission (charset + length) and stores them verbatim.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BridgeConfinement {
    /// Effective UID of the bridge process (`getuid`). The "four distinct
    /// non-root UIDs" fact (slice 1) that previously needed an SSH session.
    pub uid: u32,
    /// Outcome of the bridge's own attempted read of the nest sealed store
    /// (`<data-dir>/nest.db`), from *inside* the sandbox:
    /// `"denied"` (EACCES/EPERM — the expected, healthy answer),
    /// `"readable"` (the confinement is broken — loud),
    /// `"absent"` (ENOENT — inconclusive: a dev/binary-only box may simply have
    /// no nest database at that path), `"unknown"` (any other errno).
    pub sealed_store: String,
    /// Landlock enforcement status relayed by the `fauna-sandbox` wrapper:
    /// `"fully"` / `"partial"` / `"off"` / `"unknown"`. Three enforced-ness
    /// states, not a boolean — current kernels report *partially* enforced
    /// (an ABI-negotiation artifact) with the denials provably working, so
    /// collapsing to a bool would report a correctly-sandboxed box as broken.
    /// `"unknown"` = the wrapper did not run at all (a dev/binary launch), which
    /// is the honest answer rather than a guess.
    pub landlock: String,
    /// Seccomp status from `/proc/self/status`: `"filter"` (2 — the sandbox's
    /// filter is installed), `"strict"` (1), `"off"` (0), `"unknown"`
    /// (unreadable).
    pub seccomp: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Public metadata for one enrolled bridge service-user (no secret material).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ServiceUserInfo {
    pub bridge_id: String,
    /// `"mta"` or `"mda"`.
    pub role: String,
    /// `"pending"` / `"approved"` / `"revoked"`.
    pub status: String,
    #[serde(with = "serde_bytes")]
    pub ed25519_pubkey: Vec<u8>,
    /// Whether the bridge has attested an X25519 pubkey (i.e. is a valid
    /// HPKE seal target for DKIM/TLS blobs).
    pub has_x25519: bool,
    /// Epoch-millis when the enrollment row was created.
    pub created_at: u64,
    /// Epoch-millis of approval, if approved.
    pub approved_at: Option<u64>,
    /// The bridge's last self-reported confinement diagnostics, **admin-class
    /// callers only** (`None` in the User-visible holder view, and `None` for a
    /// bridge that has never reported).
    /// Withheld from the holder view for the same reason the roster itself is
    /// filtered there: UIDs and sandbox status are deployment topology, and a
    /// user minting a capability grant has no business reading it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confinement: Option<BridgeConfinement>,
    /// Epoch-millis when [`Self::confinement`] was reported (the bridge's last
    /// cold boot). `None` alongside a `None` confinement. Distinguishes "this
    /// box is confined" from "some earlier image said so" — a diagnostic that
    /// cannot go stale silently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confinement_reported_at: Option<u64>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Per-mail-role enrollment-strictness diagnostic (security.md § Enrollment
/// proof-of-possession contract): `true` = the nest sees an artifact-blessed
/// pubkey for the role, so `request_enrollment` requires proof of possession;
/// `false` = no blessed registry → the role's enrollment is the lenient
/// trust-any-loopback mode (deploy-safe on a dev/binary-only nest,
/// a PROVISIONING GAP on a router-fronted deployment). Read-only self-report —
/// the no-SSH way a live e2e / admin verifies a deployed box enforces strict
/// enrollment (testing.md § Gap 3 forbids an ssh path).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MailEnrollmentStrict {
    pub mta: bool,
    pub mda: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListServiceUsersReply {
    pub service_users: Vec<ServiceUserInfo>,
    /// Enrollment-strictness diagnostic, admin-class callers only (`None` in
    /// the holder view, and when the key is absent — distinguish "unknown" from "lenient" by presence).
    pub enrollment_strict: Option<MailEnrollmentStrict>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ---- Bridge approval lifecycle (admin) ----
//
// The admin-pane approval flow (`mail-bridge-lifecycle.md` § Pending approval):
// `list_pending_bridges` feeds the approval cards; `approve_pending_bridge` /
// `reject_pending_bridge` flip an enrolled bridge `pending → approved` /
// `→ revoked`. Distinct from `list_service_users` (the full Bridges-detail
// roster) so the approval-card feed has a no-arg call. Reuses `ServiceUserInfo`
// for the per-bridge projection (public metadata only — no secret material).

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListPendingBridgesRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListPendingBridgesReply {
    pub bridges: Vec<ServiceUserInfo>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApprovePendingBridgeRequest {
    #[serde(with = "serde_bytes")]
    pub ed25519_pubkey: Vec<u8>,
    /// The role the admin is approving (`"mta"` / `"mda"`); validated against
    /// the pending row's enrolled role (the current model fixes role at
    /// pre-registration, so approve confirms rather than re-assigns it).
    pub role: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApprovePendingBridgeReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RejectPendingBridgeRequest {
    #[serde(with = "serde_bytes")]
    pub ed25519_pubkey: Vec<u8>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RejectPendingBridgeReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ---- Bridge self-enrollment (zero-touch, loopback-gated) ----
//
// `request_enrollment` (`mail-bridge-lifecycle.md` § Cold boot / § Pending
// approval): a bridge that has just generated a fresh keypair announces itself
// to nest over the **anonymous pre-identity WS** to create a `pending`
// enrollment row, so it surfaces in `list_pending_bridges` for the admin to
// approve from their Fauna app — the one enrollment surface (the out-of-band
// HTTP pre-register route it replaced is removed). It is gated
// to a **loopback peer** at the dispatch layer (a same-host process is already
// inside the deployment trust boundary, so no signature is required — loopback
// IS the proof); a remote source IP is refused (cross-IP bridge auth is not yet
// designed). Idempotent: re-announcing returns the row's current status without
// clobbering an already approved/revoked row. `role_hint` seeds the pending
// row's role (the bridge derives it from its keyfile basename, e.g.
// `mta.key` → `"mta"`); the admin confirms it at approval, and the bridge's
// authoritative operating role still comes from `whoami` post-approval.

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RequestEnrollmentRequest {
    #[serde(with = "serde_bytes")]
    pub ed25519_pubkey: Vec<u8>,
    /// Role this bridge intends to serve (`"mta"` / `"mda"`), derived bridge-side
    /// from its keyfile basename. Seeds the pending row + approval card; the
    /// admin confirms it at approval (`approve_pending_bridge` validates the
    /// supplied role against this row's role).
    pub role_hint: String,
    /// Advisory stable bridge id (display only). When empty or the first-boot
    /// placeholder, nest synthesizes `"<role>-<pubkey-prefix>"`.
    pub bridge_id: String,
    /// The bridge's x25519 public key (32B), bound at enrollment under the same
    /// proof-of-possession signature as `enrollment_sig` ("the
    /// bridge signs its x25519 pubkey within the same artifact-key attestation",
    /// `security.md` § Co-resident process trust boundary). Nest binds it
    /// set-once (`upsert_bridge_x25519`), so the later `register_service_user`
    /// re-attestation only confirms it. **Absent** is admitted by the lenient mode (any loopback peer);
    /// **required** when nest has a blessed registry for the role (strict mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x25519_pubkey: Option<ByteBuf>,
    /// Ed25519 signature over
    /// [`enrollment_signed_message`]`(role_hint, ed25519_pubkey, x25519_pubkey)`,
    /// proving possession of the artifact-blessed key the nest has on record for
    /// the role (closes the signature-free self-enrollment hole). **Absent**
    /// is admitted by the lenient mode; **required + verified** when nest has a blessed
    /// registry for the role (`FAUNA_BLESSED_{MTA,MDA}_PUBKEY`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrollment_sig: Option<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The exact bytes a bridge signs at `fauna.bridges.request_enrollment` to prove
/// possession of its **artifact-blessed** Ed25519 key and, in the same
/// signature, bind its x25519 public key: a fixed domain tag, then
/// `ed25519_pubkey ‖ x25519_pubkey ‖ role`. **Single source of the signed-message
/// contract** — nest's verifier (`bridge_blob_handlers::check_enrollment_authorization`)
/// and the Go bridge's signer build the message here so they cannot drift apart
/// (mirrors [`crate::auth::handshake_signed_message`]).
///
/// A *static, context-bound* signature is deliberate (no nonce / second round):
/// for proof-of-possession of a *blessed* pubkey, replay is harmless — replaying
/// `(pubkey, sig)` only re-asserts the same blessed identity, and the value the
/// attacker would need (the private half) stays UID-isolated on disk. The domain
/// tag + role bind the signature to this purpose so it can't be reused as any
/// other Ed25519 assertion. Pure byte assembly — intentionally crypto-free so the
/// lean default protocol build stays so (signing/verifying live in the consumers).
pub fn enrollment_signed_message(
    role: &str,
    ed25519_pubkey: &[u8; 32],
    x25519_pubkey: &[u8; 32],
) -> Vec<u8> {
    const DOMAIN: &[u8] = b"fauna.bridges.enroll.v1";
    let mut msg = Vec::with_capacity(DOMAIN.len() + 64 + role.len());
    msg.extend_from_slice(DOMAIN);
    msg.extend_from_slice(ed25519_pubkey);
    msg.extend_from_slice(x25519_pubkey);
    msg.extend_from_slice(role.as_bytes());
    msg
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestEnrollmentReply {
    /// Current enrollment status: `"pending"` / `"approved"` / `"revoked"`.
    pub status: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ---- Mail enable lifecycle (admin) ----
//
// `set_mail_enabled` (`mail-bridge-lifecycle.md` § Default-off on first claim):
// the deployment-wide toggle that materializes the `/data/imap-enabled` flag
// file Stage 0's supervisor run-script gates on, and signals the supervisor
// sidekick socket. The reply carries only `{ ok }` (per § Wire shapes).

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetMailEnabledRequest {
    pub enabled: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetMailEnabledReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// `set_caldav_enabled` (`caldav-server.md` § Independent enablement): the
// deployment-wide CalDAV toggle, the calendar twin of `set_mail_enabled`. Email
// (SMTP/IMAP) and CalDAV gate independently — one MDA bridge serves both and one
// credential authenticates both, but a user can run calendar without email
// (CalDAV needs only the HTTPS surface, not MX/DKIM/SPF/port-25). Materializes
// the `/data/caldav-enabled` flag the MDA's s6 run-script gates on (alongside
// `/data/imap-enabled`), and signals the supervisor. The reply carries only
// `{ ok }`, mirroring `SetMailEnabledReply`.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetCalDavEnabledRequest {
    pub enabled: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetCalDavEnabledReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// `set_carddav_enabled` (CardDAV design, § 5 — Independent enablement,
// tracked internally): the deployment-wide CardDAV toggle, the contacts twin of
// `set_caldav_enabled`. Mail (SMTP/IMAP), CalDAV, and CardDAV gate
// independently — one MDA bridge serves all three and one credential
// authenticates them, but a user can run contacts without email or calendar
// (CardDAV needs only the HTTPS surface, not MX/DKIM/SPF/port-25, and rides the
// **same** DAV listener as CalDAV — no separate port). Materializes the
// `/data/carddav-enabled` flag the MDA's s6 run-script gates on (alongside
// `/data/imap-enabled` + `/data/caldav-enabled`), and signals the supervisor.
// The reply carries only `{ ok }`, mirroring `SetCalDavEnabledReply`.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetCardDavEnabledRequest {
    pub enabled: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetCardDavEnabledReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// `set_webdav_enabled` (`docs/goal/behavior/webdav-server.md` § Independent
// enablement): the deployment-wide WebDAV toggle, the files twin of
// `set_carddav_enabled`. Mail (SMTP/IMAP), CalDAV, CardDAV, and WebDAV gate
// independently — one MDA bridge serves all four and one credential
// authenticates them, but a user can run files without email/calendar/contacts
// (WebDAV needs only the HTTPS surface, not MX/DKIM/SPF/port-25, and rides the
// **same** DAV listener as CalDAV/CardDAV — no separate port). Materializes the
// `/data/webdav-enabled` flag the MDA's s6 run-script gates on (alongside
// `/data/imap-enabled` + `/data/caldav-enabled` + `/data/carddav-enabled`), and
// signals the supervisor. The reply carries only `{ ok }`, mirroring
// `SetCardDavEnabledReply`. Harmless-on: nothing is served until a set is
// individually flagged `folders.webdav_enabled`.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetWebDavEnabledRequest {
    pub enabled: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetWebDavEnabledReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// `set_caldav_port` (`caldav-server.md` § Network exposure — admin-settable
// CalDAV port): the deployment-wide CalDAV listener port the MDA binds, an
// **admin choice** (a product invariant — the client UI is the one user-config
// surface, so a port a human would pick is client UI + nest state, never
// a config file/env). Default `bridge_routing::DEFAULT_CALDAV_PORT` (8443). The
// MDA reads it from `fetch_config` (`FetchConfigReply.caldav_port`) and binds it
// when no operator-hatch pins the listener (the bare-IP / desktop case); a
// `config_changed` push (`CALDAV_PORT`) makes a running MDA re-fetch and, if the
// port changed, exit cleanly so s6 restarts it on the new port. The reply
// carries only `{ ok }`, mirroring `SetCalDavEnabledReply`.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetCaldavPortRequest {
    /// The CalDAV listener port (1–65535; 0 is rejected — not a bindable port).
    pub port: u16,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetCaldavPortReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// `get_caldav_port` (`caldav-server.md` § Network exposure — admin-settable
// CalDAV port; § Implementation status — Client endpoint display, the
// `MuaInstructions.caldav_port` read-path): the **user-readable** read twin of
// `set_caldav_port`. The bridge roles read the effective port via `fetch_config`
// (`FetchConfigReply.caldav_port`), and the admin reads the full config via the
// Admin read-twin, but a **regular user's** mail-settings page needs only the
// single non-sensitive port number to display the CalDAV connection detail for a
// third-party calendar app — so this is a `User | Admin` read that returns just
// the effective port (the persisted singleton, or `DEFAULT_CALDAV_PORT` when
// unset). The request carries no fields (the port is a nest-wide singleton, not
// caller-scoped). Mirrors the `extra` forward-compat convention of the
// `Set…`/`Get…` pairs above.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GetCaldavPortRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GetCaldavPortReply {
    /// The effective CalDAV listener port (the admin-set singleton, or
    /// `bridge_routing::DEFAULT_CALDAV_PORT` = 8443 when unset).
    pub port: u16,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// `set_auto_enable_mail_for_new_users` (`mail-policy-config.md` § Tier-2 new-user
// mail defaults): the deployment-wide policy deciding whether a freshly-
// registered user's client auto-provisions its own mailbox on first setup
// (default-**on** — the works-out-of-box invariant extended to every user).
// Unlike `set_mail_enabled` this drives **no** bridge state (the bridge never
// reads it) — it is a client-read deployment default surfaced on
// `fauna.setup.status` (`SetupStatusReply.auto_enable_mail_for_new_users`); the
// nest cannot mint the mailbox itself (the MSEK is client-held). Admin-only; NOT
// a per-user control (mail is user-controlled — `admin.md` § Don't do these).
// The reply carries only `{ ok }`.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetAutoEnableMailForNewUsersRequest {
    pub enabled: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetAutoEnableMailForNewUsersReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ---- Per-actor IMAP/CalDAV-serving opt-out (user) ----
//
// `set_mail_serving_enabled` (`deployment-home-with-public-relay.md`
// § MUA reach): the **per-actor, user-set** toggle for "serve my mail/calendar
// on this nest". Orthogonal to the deployment-wide admin `set_mail_enabled` /
// `set_caldav_enabled` (whole-nest, listener binding) — this is User-class and
// **caller-scoped**: it sets the *calling* actor's own row, never an arbitrary
// actor's (the handler keys on the authenticated `actor_id`, so the request
// carries only `enabled`). Default ON / absent ⇒ ON; only the deployment user
// who reads on a paired residential nest flips their own row OFF. The nest-side
// serving gates consult it per request, so no `fetch_config` field and no
// `config_changed` push are involved. Reply carries only `{ ok }`.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetMailServingEnabledRequest {
    pub enabled: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetMailServingEnabledReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// `get_mail_serving_enabled`: read the per-actor serving flag. A `User` caller
// always reads its OWN flag (the `actor_id` field is ignored — caller-scoped); an
// `Admin` caller reads the actor named by `actor_id` (the admin's **read-only**
// audit view per § MUA reach — the admin may see but not set another user's
// flag). An empty `actor_id` ⇒ the caller's own. `enabled` reflects the
// default-on semantics: a never-set actor reads back `true`.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GetMailServingEnabledRequest {
    /// Admin audit target. Empty ⇒ the caller's own actor. Ignored (forced to
    /// the caller) for non-admin callers.
    #[serde(with = "serde_bytes", default)]
    pub actor_id: Vec<u8>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GetMailServingEnabledReply {
    pub enabled: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ---- Service-user re-keying (admin) ----
//
// `revoke_service_user` (`mail-bridge-lifecycle.md` § Service-user re-keying):
// revoke an **already-approved** bridge so the admin can rotate its key — the
// bridge's next `whoami` returns `revoked`, it shuts down gracefully, the
// supervisor restarts it, and it generates a fresh keypair → a new pending
// approval. Distinct from `reject_pending_bridge` (the *pending*-phase, pubkey-
// keyed reject affordance on the approval card) — this is the *running*-phase
// "Rotate bridge service-user key" affordance keyed by `bridge_actor_id` (which
// for a bridge service user is its ed25519 pubkey). Reply is `{ ok }` per
// § Wire shapes.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RevokeServiceUserRequest {
    #[serde(with = "serde_bytes")]
    pub bridge_actor_id: Vec<u8>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RevokeServiceUserReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ---- Revoke ----

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RevokeWrappedMlsBlobRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    pub credential_id: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RevokeWrappedSubmissionTokenRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    pub credential_id: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RevokeReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ---- Bridge enrollment ----

// `Default` so fixtures can use `..Default::default()`: this struct keeps
// growing additive fields (mlkem_ek, confinement), and hand-listed fixtures
// turn every growth into a merge conflict on the grown axis — two branches
// each adding a field collide on exactly the line the other grew.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RegisterServiceUserRequest {
    #[serde(with = "serde_bytes")]
    pub ed25519_pubkey: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub x25519_pubkey: Vec<u8>,
    /// Post-quantum sibling (PQ-CAP-2): the bridge's 1184-byte ML-KEM-768
    /// encapsulation key (`fauna_pq_kem::MLKEM768_ENCAPS_KEY_LEN`), derived from
    /// the bridge's Ed25519 identity seed (`fauna.bridge.service-user-mlkem.v1`,
    /// context-separated from mail/subscriptions) and published here alongside
    /// `x25519_pubkey`. The nest stores it on `bridge_service_users.mlkem_ek` so
    /// the client mint can seal capability grants X-Wing to
    /// `from_parts(mlkem_ek, x25519_pubkey)`. `None` (from a bridge that publishes no
    /// ek, e.g. the atproto bridge) leaves the holder classical-only — the mint
    /// degrades to the classical wrap. `skip_serializing_if` keeps a
    /// classical-bridge request byte-identical to the pre-PQ-CAP wire (matching
    /// the Go mirror's `omitempty`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mlkem_ek: Option<ByteBuf>,
    pub role: String,
    pub bridge_id: String,
    /// What the bridge observed about its own confinement at startup, probed
    /// from *inside* its sandbox. See [`BridgeConfinement`] — provisioning
    /// diagnostics, never an attestation.
    ///
    /// It rides *this* call rather than `request_enrollment` because this one is
    /// authenticated and post-approval (so the report lands on a row an admin
    /// actually reads) and runs on **every cold boot** (so the report tracks the
    /// image currently running). `None` is the non-probing / never-reported shape;
    /// `skip_serializing_if` keeps such a request free of the key, matching
    /// the Go mirror's `omitempty`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confinement: Option<BridgeConfinement>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegisterServiceUserReply {
    pub enrollment_request_id: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ---- Audit ----

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReportAuthEventRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    pub credential_id: String,
    pub result: String, // "ok" or "fail"
    pub source_ip: String,
    pub occurred_at: u64,
    pub reason: Option<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReportAuthEventReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ---- Capability grants (fauna.capabilities.*) ----
//
// A user-minted, scope-limited, revocable capability held by a
// content-processing bridge (the dual of the mail MDA). Unlike DKIM/TLS
// (deployment infrastructure → Admin-minted), a content capability is the
// owner's own data access → OWNER-minted, holder-fetched. The `grant_blob`
// / `grants` fields carry a canonical-encoded `fauna-mls::wrapped_blob::
// GrantBlob` (opaque to the nest — sealed to the holder's pubkey). Design
// spec § Phase 2 Step 2 § 2.3.

/// `fauna.capabilities.mint` — the content-owning user deposits a
/// client-built [`GrantBlob`](../../fauna-mls). The nest stores it keyed
/// `(owner, grant_id)` and never opens it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MintGrantRequest {
    /// Canonical-encoded `GrantBlob` (HPKE-sealed to the holder pubkey).
    pub grant_blob: ByteBuf,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MintGrantReply {
    /// The stored grant's 16-byte id (echoed from the blob's `ix.grant_id`),
    /// the handle a later `renew`/`revoke` names.
    pub grant_id: ByteBuf,
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.capabilities.fetch` — a holder (the authenticated service-user)
/// pulls the grants sealed to it. The holder identity is the authenticated
/// caller — there is deliberately **no** field to spoof; the nest serves
/// only grants whose `holder == caller's enrolled x25519`, omitting
/// revoked/expired ones (that omission is how honest-box revocation bites).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FetchGrantsRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FetchGrantsReply {
    /// 0..n canonical-encoded `GrantBlob`s for this holder.
    pub grants: Vec<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.capabilities.renew` — the owner extends a grant's window and, for
/// an epoch-sealed kind, appends the next window's wrapped keys (the client
/// mints them). Master-key today = a window bump only (`appended_keys` empty).
///
/// A renewal **slides** the window rather than growing it: the client
/// re-centres it on the renewal instant (`new_epoch_start`, one mint-length
/// of history behind the new end), and the nest drops every per-epoch wrap
/// below the new start, so a bounded grant renewed for years stays the same
/// size (`encryption-at-rest.md` § Capability tiering → *Content-sealing
/// epochs*, the retention ruling).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RenewGrantRequest {
    pub grant_id: ByteBuf,
    /// The new `window.epoch_start` — never earlier than the stored one (a
    /// backward move is refused; widening into the past would need wraps the
    /// renew cannot prove) and never past `new_epoch_end`. Absent (a client
    /// that does not slide, or the folder kind's generation-indexed renew)
    /// keeps the stored start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_epoch_start: Option<u64>,
    /// The new `window.epoch_end`.
    pub new_epoch_end: u64,
    /// 0..n canonical-encoded `WrappedScopeKey`s to append (empty for a
    /// master-key grant).
    pub appended_keys: Vec<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RenewGrantReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.capabilities.revoke` — the owner deletes the `(owner, grant_id)`
/// row; the holder's next `fetch` returns nothing and it goes dark (honest
/// box).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RevokeGrantRequest {
    pub grant_id: ByteBuf,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RevokeGrantReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The per-owner capability-grant cap — **one number both ends must agree on**,
/// so it lives on the wire plane rather than in either end's private code.
///
/// Nest-side it is a storage bound: a holder is a *shared* co-resident bridge,
/// so without a per-owner cap one abusive owner minting many 64 KiB grants
/// would bloat every holder `fetch` and the nest DB — a cross-user DoS. 256 is
/// generous headroom over the natural count (well under ten) while bounding
/// each owner's contribution to the aggregate fetch at `256 × 64 KiB = 16 MiB`.
/// A *renew* / re-mint of an existing `grant_id` is a replace, not a new row,
/// so it is always allowed even at the cap.
///
/// Client-side it is the reconcile sweep's truncation bound: an honest nest's
/// [`ReconcileGrantsReply`] is ≤ this many ids by construction, so a longer one
/// is a hostile answer and the client processes only this many per sweep
/// (`ui/nests.md` § Trust facet — grants → *Reconcile*, the *Bounded by the
/// existing per-owner cap* bullet). A client truncating at a different number
/// than the nest caps at is a latent bug, which is why there is exactly one
/// definition; `fauna_nest::db::capability_grants` re-exports this one.
pub const MAX_GRANTS_PER_OWNER: usize = 256;

/// `fauna.capabilities.reconcile` — the one admissible owner-side nest read
/// (`ui/nests.md` § Trust facet — grants → *Reconcile*, ratified 2026-08-15).
/// The owner enumerates every grant id it holds on this nest so its own
/// signed log can revoke whatever it does not recognize as live. No request
/// fields: owner-scoped from the authenticated caller.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReconcileGrantsRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReconcileGrantsReply {
    /// Every `(owner, grant_id)` this caller holds on this nest — ALL rows,
    /// expired included. Ids only, nothing else: a `GrantBlob` carries no
    /// owner signature, so a scope/holder/window field here would be
    /// unverifiable nest-authored display data (the same reason the audit
    /// view is never a nest read). The client judges liveness against its
    /// own log, never against this reply.
    pub grant_ids: Vec<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ---- The re-score drain plane (design § 2.5 step 4 / § 2.6) ----
//
// Two holder-gated RPCs that let a capability holder (an MDA / content-processor
// service-user) drain the versioned re-score obligation without the nest ever
// reading content (`encryption-at-rest.md:256`, `key-material-hierarchy.md` rule
// #4): the nest serves a *content-free* worklist (which of the holder's granted
// owners' content is behind on which factor) and accepts a *content-free* score
// write-back (a re-computed `ScoreEntry` per item). The unseal + re-run happens
// off-box, at the holder, under its `content.read{kind}` grant.

/// One unit of re-score work: an item whose `content_scores.scorer_version` for
/// `factor` is behind the current `model_versions` version, scoped to an owner
/// the holder holds a `content.read{kind}` grant for. The holder maps
/// `content_id` → the sealed record, unseals it under the grant key, re-runs the
/// `factor` scorer, and submits a fresh row via `fauna.capabilities.submit_scores`
/// stamped at `to_version`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RescoreUnit {
    /// The 32-byte content id (`content_scores.content_id`) — the holder's
    /// handle to fetch + unseal the sealed record.
    pub content_id: ByteBuf,
    /// The content kind (`mail`, …) — selects the sealed store + the grant
    /// scope the holder needs.
    pub content_kind: String,
    /// The owning actor — whose `content.read{kind}` grant the holder wields to
    /// unseal, and whose row the re-score writes back under.
    pub owner_actor_id: ByteBuf,
    /// The scoring factor to re-run (`clamav` / `rspamd` / …; a
    /// `fauna_core::scoring::factor::*` string).
    pub factor: String,
    /// The version the row was last scored at (`< to_version`) — advisory,
    /// lets the holder skip factors it can't re-compute.
    pub from_version: u32,
    /// The current model version for `factor` (`model_versions[factor]`) — the
    /// version the holder stamps the re-scored row with.
    pub to_version: u32,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.capabilities.rescore_worklist` — a holder asks "what re-processing do
/// I owe?" The holder identity is the authenticated caller (no field to spoof);
/// the nest intersects the holder's `content.read{kind}` grants with the
/// per-factor obligation gap and returns only work for owners the holder can
/// actually unseal (a metadata-confidentiality boundary — never leak which
/// *other* users have stale content).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RescoreWorklistRequest {
    /// Max work-units to return this call (`0` → a server default cap). The
    /// holder loops until an empty reply drains the backlog (no silent
    /// truncation — a full batch means "more remain").
    pub limit: u32,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RescoreWorklistReply {
    /// 0..limit work-units, lowest-`from_version`-first (most-behind drains
    /// first). Empty → nothing owed for this holder's grants.
    pub units: Vec<RescoreUnit>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One re-scored item the holder writes back: the `entries` replace the
/// `(content_id, factor)` rows in `content_scores` (INSERT OR REPLACE). Writing
/// a factor row **is** the `content.label-write` operation at the key-access
/// layer (`content-scoring.md` § scoring-metadata bus); the nest authz's each
/// row against the holder's `content.label-write` grant for `owner_actor_id`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SubmitScoreRow {
    pub content_id: ByteBuf,
    pub content_kind: String,
    pub owner_actor_id: ByteBuf,
    /// Unix seconds the re-score ran (the row's `scored_at`).
    pub scored_at: u64,
    /// The re-computed score rows for this item (one per re-run factor), each
    /// stamped with the current `scorer_version`.
    pub entries: Vec<fauna_core::scoring::ScoreEntry>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.capabilities.submit_scores` — a holder writes back re-computed scores
/// after draining a worklist. Content-free (only `ScoreEntry` metadata crosses;
/// the nest never sees the plaintext it was derived from). Each row is authz'd
/// against the holder's `content.label-write` grant for its `owner_actor_id`;
/// an unauthorized row fails the whole batch (fail-closed, no partial write).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SubmitScoresRequest {
    pub rows: Vec<SubmitScoreRow>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SubmitScoresReply {
    /// Count of `(content_id, factor)` rows written.
    pub written: u32,
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ---- The spam-baseline publish drain (`mail-spam.md` § Encrypted-mode
//      interaction, ratified 2026-07-13) ----
//
// The third instance of the holder-pull drain plane: `publish_spam_baseline`
// pokes the granted holder (`fauna.bridges.spam_baseline_publish` push), the
// holder pulls a worklist of **sealed-to-holder model copies** whose paired
// keyless `content.read{spam-model}` grants stand, unseals each with its OWN
// service-user key (never any key of a user's), merges additively off-box,
// and submits its merged half + contributor count back against the pending
// run. The nest never reads a sealed copy and merges no model of its own —
// the holder's half IS the baseline, and the k-anonymity floor applies to
// its contributor count.

/// One sealed-to-holder model copy served on a spam-baseline worklist: a
/// `spam_model_holder_copies` row whose owner is opted in, whose paired
/// `content.read{spam-model}` grant to the calling holder stands, and who
/// still has a current `spam_models` row (a reset model serves no copy).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SpamBaselineCopy {
    /// The contributing actor — whose `content.read{spam-model}` grant the
    /// holder wields (audit attribution; the holder never needs any key of
    /// this user's, the copy is sealed to the holder's own key material).
    pub owner_actor_id: ByteBuf,
    /// The sealed copy: `SpamModelCopyBlob` canonical bytes, exactly as the
    /// contributing agent stored them via `put_spam_model`'s `holder_copy`.
    pub sealed_copy: ByteBuf,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.capabilities.spam_baseline_worklist` — the poked holder asks "which
/// sealed contributor copies may I merge for this publish run?" Valid only
/// while the named run is pending (the worklist exists only inside a publish
/// window — outside one the surface stays quiet even to an enrolled holder).
/// The holder identity is the authenticated caller (no field to spoof).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SpamBaselineWorklistRequest {
    /// The 16-byte pending-run id from the `spam_baseline_publish` push.
    pub run_id: ByteBuf,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SpamBaselineWorklistReply {
    /// The grant-gated sealed copies the holder may unseal + merge for this
    /// run. Empty → nothing mergeable (the holder submits an empty half).
    pub copies: Vec<SpamBaselineCopy>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.capabilities.submit_spam_baseline` — the holder writes back its
/// off-box merge for a pending publish run: the additively-merged plaintext
/// half (`SpamModel` bytes — an aggregate over ≥1 contributors, not any
/// single user's model) plus its contributor count. Idempotent against an
/// unknown / expired run (`ok: false`, never an error — the publish may have
/// timed out while the holder merged).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SubmitSpamBaselineRequest {
    /// The pending-run id the worklist was pulled for.
    pub run_id: ByteBuf,
    /// The holder's merged half: plaintext `SpamModel` serialization of the
    /// additive merge over every copy it could unseal + decode. **Empty ⇒
    /// nothing merged** (no reaching copies, or none decoded).
    #[serde(with = "serde_bytes")]
    pub merged_model: Vec<u8>,
    /// How many contributor copies were merged into `merged_model` — counted
    /// toward the k-anonymity floor nest-side, defensively clamped there to
    /// the sealed-candidate population (the holder is trusted infra, but the
    /// nest never lets a claimed count exceed what it served).
    pub contributors: u32,
    /// How many served copies the holder could NOT unseal / decode —
    /// advisory diagnostics for the publish reply's erosion count.
    pub unreadable: u32,
    /// The contributing actors (32 bytes each, the worklist's
    /// `owner_actor_id`) whose copies the holder merged into `merged_model` —
    /// additive, 2026-09-27 (`mail-spam.md` § Cold start Path 2 → *A
    /// contributor's departure withdraws the baseline*). The nest records
    /// exactly these as summed in its inclusion record, so a sealed candidate
    /// the holder skipped holds no inclusion row and its departure withdraws
    /// nothing. A name outside the run's sealed-candidate population is
    /// dropped nest-side (the same clamp `contributors` gets). **Empty ⇒
    /// nothing named, so nothing counted**: the nest folds no merged half
    /// that names no contributor.
    #[serde(default)]
    pub merged_contributors: Vec<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SubmitSpamBaselineReply {
    /// `true` ⇒ the submission reached the pending run; `false` ⇒ no such
    /// run (already submitted, timed out, or never existed) — idempotent,
    /// mirroring revoke's idempotency, never an error.
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn enrollment_signed_message_is_deterministic_and_field_bound() {
        let ed = [0x11u8; 32];
        let x = [0x22u8; 32];
        let base = enrollment_signed_message("mta", &ed, &x);
        // Deterministic (static, context-bound — no nonce).
        assert_eq!(base, enrollment_signed_message("mta", &ed, &x));
        // Domain tag prefixes the message.
        assert!(base.starts_with(b"fauna.bridges.enroll.v1"));
        // Every input is bound: changing any one changes the bytes.
        assert_ne!(base, enrollment_signed_message("mda", &ed, &x));
        assert_ne!(base, enrollment_signed_message("mta", &[0x99u8; 32], &x));
        assert_ne!(base, enrollment_signed_message("mta", &ed, &[0x99u8; 32]));
        // Layout: DOMAIN ‖ ed25519 ‖ x25519 ‖ role.
        let dom = b"fauna.bridges.enroll.v1".len();
        assert_eq!(&base[dom..dom + 32], &ed);
        assert_eq!(&base[dom + 32..dom + 64], &x);
        assert_eq!(&base[dom + 64..], b"mta");
    }

    #[test]
    fn request_enrollment_request_signed_fields_round_trip() {
        let r = RequestEnrollmentRequest {
            ed25519_pubkey: vec![0x33; 32],
            role_hint: "mda".into(),
            bridge_id: "b1".into(),
            x25519_pubkey: Some(ByteBuf::from(vec![0x44; 32])),
            enrollment_sig: Some(ByteBuf::from(vec![0x55; 64])),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: RequestEnrollmentRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
    }

    #[test]
    fn request_enrollment_request_without_signed_fields_decodes_with_none() {
        // A request omitting the signed fields entirely — they must default to
        // `None` (lenient path), not fail to decode (forward-compat, rule 4).
        let unsigned = RequestEnrollmentRequest {
            ed25519_pubkey: vec![0x66; 32],
            role_hint: "mta".into(),
            bridge_id: String::new(),
            ..Default::default()
        };
        let bytes = encode_canonical(&unsigned).unwrap();
        let decoded: RequestEnrollmentRequest = decode(&bytes).unwrap();
        assert_eq!(decoded.x25519_pubkey, None);
        assert_eq!(decoded.enrollment_sig, None);
        assert_eq!(decoded, unsigned);
    }

    #[test]
    fn fetch_wrapped_mls_blob_request_round_trip() {
        let r = FetchWrappedMlsBlobRequest {
            actor_id: vec![0u8; 32],
            credential_id: "cred-1".into(),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: FetchWrappedMlsBlobRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
    }

    #[test]
    fn fetch_wrapped_mls_blob_reply_round_trip() {
        let r = FetchWrappedMlsBlobReply {
            blob: Some(ByteBuf::from(vec![0xAA; 64])),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: FetchWrappedMlsBlobReply = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
    }

    #[test]
    fn fetch_wrapped_mls_blob_reply_not_found_round_trip() {
        let r = FetchWrappedMlsBlobReply {
            blob: None,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: FetchWrappedMlsBlobReply = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
    }

    #[test]
    fn fetch_spam_model_request_round_trip() {
        let r = FetchSpamModelRequest {
            actor_id: vec![7u8; 32],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: FetchSpamModelRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
    }

    #[test]
    fn fetch_spam_model_reply_round_trip() {
        let r = FetchSpamModelReply {
            blob: Some(ByteBuf::from(vec![0xBE; 128])),
            stored_sealed: true,
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: FetchSpamModelReply = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
    }

    #[test]
    fn fetch_spam_model_reply_baseline_round_trip() {
        let r = FetchSpamModelReply {
            blob: Some(ByteBuf::from(vec![0xBE; 128])),
            stored_sealed: true,
            baseline: Some(ByteBuf::from(vec![0x7B; 64])),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: FetchSpamModelReply = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
    }

    #[test]
    fn fetch_spam_model_reply_decodes_with_baseline_key_absent() {
        // A nest with no baseline skip-serializes `None`, omitting the
        // `baseline` KEY entirely; `#[serde(default)]` decodes it
        // absent — the agent simply performs no local fold.
        let r = FetchSpamModelReply {
            blob: Some(ByteBuf::from(vec![0xBE; 16])),
            stored_sealed: true,
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: FetchSpamModelReply = decode(&bytes).unwrap();
        assert_eq!(decoded.baseline, None);
    }

    #[test]
    fn fetch_spam_model_reply_decodes_with_stored_sealed_key_absent() {
        // A reply omitting the `stored_sealed` KEY entirely;
        // `#[serde(default)]` decodes it `false` — no stored model, so a
        // train position starts from an empty one.
        #[derive(Serialize)]
        struct UnsealedFetchSpamModelReply {
            blob: Option<ByteBuf>,
        }
        let old = UnsealedFetchSpamModelReply {
            blob: Some(ByteBuf::from(vec![0xBE; 16])),
        };
        let bytes = encode_canonical(&old).unwrap();
        let decoded: FetchSpamModelReply = decode(&bytes).unwrap();
        assert!(!decoded.stored_sealed);
        assert_eq!(decoded.blob, Some(ByteBuf::from(vec![0xBE; 16])));
    }

    #[test]
    fn fetch_spam_model_reply_untrained_round_trip() {
        let r = FetchSpamModelReply {
            blob: None,
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: FetchSpamModelReply = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
    }

    #[test]
    fn fetch_spam_model_reply_round_trips_contribute_signal() {
        // Piece (b): the opt-in-contributor write signal — `contribute_baseline`
        // + the volunteered `holder_seal_target` (X-Wing-capable: carries the
        // holder ML-KEM ek).
        let r = FetchSpamModelReply {
            blob: Some(ByteBuf::from(vec![0xBE; 96])),
            stored_sealed: true,
            contribute_baseline: true,
            holder_seal_target: Some(HolderSealTarget {
                x25519_pubkey: vec![0x11; 32],
                // ML-KEM-768 encaps key length (opaque bytes for the round-trip).
                mlkem_ek: Some(ByteBuf::from(vec![0x22; 1184])),
                ..Default::default()
            }),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: FetchSpamModelReply = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
    }

    #[test]
    fn fetch_spam_model_reply_round_trips_classical_holder_target() {
        // A holder that has published no ML-KEM ek ⇒ classical X25519 seal
        // target (`mlkem_ek: None`, skip-serialized).
        let r = FetchSpamModelReply {
            blob: Some(ByteBuf::from(vec![0xBE; 16])),
            stored_sealed: true,
            contribute_baseline: true,
            holder_seal_target: Some(HolderSealTarget {
                x25519_pubkey: vec![0x33; 32],
                mlkem_ek: None,
                ..Default::default()
            }),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: FetchSpamModelReply = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
        assert!(
            decoded
                .holder_seal_target
                .as_ref()
                .unwrap()
                .mlkem_ek
                .is_none()
        );
    }

    #[test]
    fn fetch_spam_model_reply_decodes_with_contribute_keys_absent() {
        // A pre-piece-(b) nest omits BOTH `contribute_baseline` and
        // `holder_seal_target` keys; `#[serde(default)]` decodes them
        // `false`/`None` — the write attaches no copy (safe: the plaintext-row
        // merge still counts a server-written model).
        #[derive(Serialize)]
        struct PreBFetchSpamModelReply {
            blob: Option<ByteBuf>,
            stored_sealed: bool,
        }
        let old = PreBFetchSpamModelReply {
            blob: Some(ByteBuf::from(vec![0xBE; 16])),
            stored_sealed: true,
        };
        let bytes = encode_canonical(&old).unwrap();
        let decoded: FetchSpamModelReply = decode(&bytes).unwrap();
        assert!(!decoded.contribute_baseline);
        assert_eq!(decoded.holder_seal_target, None);
    }

    #[test]
    fn put_spam_model_request_round_trip() {
        let r = PutSpamModelRequest {
            sealed_model: vec![0xAB; 96],
            sample_count: 42,
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: PutSpamModelRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
    }

    #[test]
    fn put_spam_model_request_defaults_absent_sample_count_to_zero() {
        // `sample_count` is advisory (`#[serde(default)]`): a peer that omits it
        // decodes to 0 rather than failing — additive/forward-compat on the wire.
        let full = PutSpamModelRequest {
            sealed_model: vec![0x01; 8],
            ..Default::default()
        };
        let bytes = encode_canonical(&full).unwrap();
        let decoded: PutSpamModelRequest = decode(&bytes).unwrap();
        assert_eq!(decoded.sample_count, 0);
        assert_eq!(decoded.sealed_model, vec![0x01; 8]);
        // `history_op` is `#[serde(default)]` ⇒ absent decodes to `None`.
        assert_eq!(decoded.history_op, None);
        // `actor_id` is `#[serde(default)]` ⇒ absent decodes to empty (a
        // `User`/`Admin` client never sends it; the MDA leg-2 write does).
        assert!(decoded.actor_id.is_empty());
    }

    #[test]
    fn put_spam_model_request_round_trips_actor_id() {
        // The MDA (leg 2) names the served actor in `actor_id` for its
        // trusted-naming write-back; additive `#[serde(default, serde_bytes)]`.
        let r = PutSpamModelRequest {
            sealed_model: vec![0x77; 64],
            actor_id: vec![0x99; 32],
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: PutSpamModelRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
        assert_eq!(decoded.actor_id, vec![0x99; 32]);
    }

    #[test]
    fn put_spam_model_request_decodes_with_actor_id_key_absent() {
        // A request omitting the `actor_id` KEY entirely (not just an
        // empty value); `#[serde(default)]` decodes it empty (⇒ the caller
        // itself) instead of failing — the additive wire guarantee.
        #[derive(Serialize)]
        struct ActorlessPutSpamModelRequest {
            #[serde(with = "serde_bytes")]
            sealed_model: Vec<u8>,
        }
        let old = ActorlessPutSpamModelRequest {
            sealed_model: vec![0x01; 8],
        };
        let bytes = encode_canonical(&old).unwrap();
        let decoded: PutSpamModelRequest = decode(&bytes).unwrap();
        assert!(decoded.actor_id.is_empty());
        assert_eq!(decoded.sealed_model, vec![0x01; 8]);
    }

    #[test]
    fn put_spam_model_request_round_trips_holder_copy() {
        // The deployment-baseline holder copy rides the same kind atomically
        // (`mail-spam.md` § Encrypted-mode interaction, 2026-07-13). Additive:
        // absent on the wire decodes `None` (covered above via
        // `..Default::default()` in the sibling tests); present round-trips.
        let r = PutSpamModelRequest {
            sealed_model: vec![0x11; 64],
            holder_copy: Some(SpamModelHolderCopy {
                holder_pubkey: vec![0x44; 32],
                sealed_copy: vec![0x55; 128],
                ..Default::default()
            }),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: PutSpamModelRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
        let copy = decoded.holder_copy.unwrap();
        assert_eq!(copy.holder_pubkey, vec![0x44; 32]);
        assert_eq!(copy.sealed_copy, vec![0x55; 128]);
        // `skip_serializing_if` keeps a copy-less write byte-identical to the
        // pre-copy wire (no `null` key lands in the canonical encoding — the
        // dag-cbor nested-Option guarantee).
        let plain = PutSpamModelRequest {
            sealed_model: vec![0x11; 64],
            ..Default::default()
        };
        let plain_bytes = encode_canonical(&plain).unwrap();
        let redecoded: PutSpamModelRequest = decode(&plain_bytes).unwrap();
        assert_eq!(redecoded.holder_copy, None);
    }

    #[test]
    fn put_spam_model_request_round_trips_history_op_insert() {
        // A client-path train: the model re-seal carries its sealed audit row
        // (option (a) — one atomic kind). The sealed subject/delta are opaque
        // bytes the nest stores verbatim.
        use crate::bridge_routing::{SpamHistoryOp, SpamLabel, TrainingSource};
        let r = PutSpamModelRequest {
            sealed_model: vec![0x11; 64],
            sample_count: 7,
            history_op: Some(SpamHistoryOp::Insert {
                message_id: vec![0x22; 32],
                mailbox: "INBOX".to_string(),
                sealed_subject: vec![0x33; 48],
                sealed_delta: vec![0x44; 80],
                label: SpamLabel::Spam,
                source: TrainingSource::ImapJunkMove,
            }),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: PutSpamModelRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
    }

    #[test]
    fn put_spam_model_request_round_trips_history_op_delete() {
        // A client-path undo: the re-sealed (inverse-applied) model + the removal
        // of the consumed audit row, atomically.
        use crate::bridge_routing::SpamHistoryOp;
        let r = PutSpamModelRequest {
            sealed_model: vec![0x55; 64],
            sample_count: 6,
            history_op: Some(SpamHistoryOp::Delete {
                history_id: vec![0x66; 16],
            }),
            ..Default::default()
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: PutSpamModelRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
    }

    #[test]
    fn put_spam_model_reply_round_trip() {
        let r = PutSpamModelReply::default();
        assert_eq!(r.outcome, PutSpamModelOutcome::Written);
        let bytes = encode_canonical(&r).unwrap();
        let decoded: PutSpamModelReply = decode(&bytes).unwrap();
        assert_eq!(decoded, r);

        let dup = PutSpamModelReply {
            outcome: PutSpamModelOutcome::DuplicateSignal,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&dup).unwrap();
        let decoded: PutSpamModelReply = decode(&bytes).unwrap();
        assert_eq!(decoded, dup);
    }

    /// A nest from before the `outcome` field replies a bare ack (an empty
    /// map); it decodes as `Written`, the only outcome such a nest could
    /// produce. And the wire string is the snake_case the Go bridge matches.
    #[test]
    fn put_spam_model_reply_without_outcome_decodes_as_written() {
        let bare: BTreeMap<String, Value> = BTreeMap::new();
        let bytes = encode_canonical(&bare).unwrap();
        let decoded: PutSpamModelReply = decode(&bytes).unwrap();
        assert_eq!(decoded.outcome, PutSpamModelOutcome::Written);

        let mut dup: BTreeMap<String, Value> = BTreeMap::new();
        dup.insert("outcome".into(), Value::String("duplicate_signal".into()));
        let bytes = encode_canonical(&dup).unwrap();
        let decoded: PutSpamModelReply = decode(&bytes).unwrap();
        assert_eq!(decoded.outcome, PutSpamModelOutcome::DuplicateSignal);
        assert!(decoded.extra.is_empty(), "the known key is not a stray");
    }

    #[test]
    fn get_spam_scoring_policy_request_round_trip() {
        let r = GetSpamScoringPolicyRequest::default();
        let bytes = encode_canonical(&r).unwrap();
        let decoded: GetSpamScoringPolicyRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
    }

    #[test]
    fn get_spam_scoring_policy_reply_round_trip() {
        let r = GetSpamScoringPolicyReply {
            spam_folder_threshold: 5,
            bayesian_weight_milli: 700,
            bayesian_min_samples: 50,
            bayesian_full_confidence_samples: 200,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: GetSpamScoringPolicyReply = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
    }

    #[test]
    fn list_dkim_selectors_round_trip() {
        let req = ListDkimSelectorsRequest {
            domain: Some("example.com".into()),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: ListDkimSelectorsRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);

        let reply = ListDkimSelectorsReply {
            selectors: vec![
                DkimSelectorInfo {
                    domain: "example.com".into(),
                    selector: "2026a".into(),
                    created_at: 1_700_000_000_000,
                    public_dns_value: "v=DKIM1; k=ed25519; p=AAAA".into(),
                    extra: Default::default(),
                },
                DkimSelectorInfo {
                    domain: "example.com".into(),
                    selector: "2026b".into(),
                    created_at: 1_700_000_001_000,
                    public_dns_value: String::new(),
                    extra: Default::default(),
                },
            ],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ListDkimSelectorsReply = decode(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn list_dkim_selectors_request_all_domains_round_trip() {
        let req = ListDkimSelectorsRequest {
            domain: None,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: ListDkimSelectorsRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn list_service_users_round_trip() {
        let req = ListServiceUsersRequest {
            role: Some("mta".into()),
            status: Some("approved".into()),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: ListServiceUsersRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);

        let reply = ListServiceUsersReply {
            service_users: vec![ServiceUserInfo {
                bridge_id: "mta-1".into(),
                role: "mta".into(),
                status: "approved".into(),
                ed25519_pubkey: vec![0xAB; 32],
                has_x25519: true,
                created_at: 1_700_000_000_000,
                approved_at: Some(1_700_000_100_000),
                confinement: Some(BridgeConfinement {
                    uid: 1001,
                    sealed_store: "denied".into(),
                    landlock: "partial".into(),
                    seccomp: "filter".into(),
                    extra: Default::default(),
                }),
                confinement_reported_at: Some(1_700_000_200_000),
                ..Default::default()
            }],
            enrollment_strict: Some(MailEnrollmentStrict {
                mta: true,
                mda: false,
                extra: Default::default(),
            }),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ListServiceUsersReply = decode(&bytes).unwrap();
        assert_eq!(decoded, reply);

        // Additive-compat: a reply with no
        // `enrollment_strict` key at all (the holder view) must still decode, as `None` —
        // "unknown", distinct from an explicit lenient report.
        let old = ListServiceUsersReply {
            service_users: vec![],
            enrollment_strict: None,
            extra: Default::default(),
        };
        let mut bytes = encode_canonical(&old).unwrap().to_vec();
        // Simulate the absent key by re-encoding through a raw map with
        // the key stripped.
        let mut as_map: BTreeMap<String, Value> = decode(&bytes).unwrap();
        as_map.remove("enrollment_strict");
        bytes = encode_canonical(&as_map).unwrap().to_vec();
        let decoded: ListServiceUsersReply = decode(&bytes).unwrap();
        assert_eq!(decoded.enrollment_strict, None);
    }

    /// The confinement self-probe wire (`security.md` § Co-resident process
    /// trust boundary → *Confinement self-probe*) is additive in **both**
    /// directions, and this pins both: a probing bridge's request round-trips
    /// whole, and a non-probing bridge's request — which omits the key entirely —
    /// still decodes, as `None`.
    #[test]
    fn register_service_user_confinement_round_trip() {
        let req = RegisterServiceUserRequest {
            ed25519_pubkey: vec![0x11; 32],
            x25519_pubkey: vec![0x22; 32],
            mlkem_ek: None,
            role: "mta".into(),
            bridge_id: "mta-1".into(),
            confinement: Some(BridgeConfinement {
                uid: 1001,
                sealed_store: "denied".into(),
                landlock: "partial".into(),
                seccomp: "filter".into(),
                extra: Default::default(),
            }),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: RegisterServiceUserRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);

        // A non-probing bridge omits the key entirely — `skip_serializing_if`
        // must keep it off the wire, so the request carries no `confinement`
        // key at all.
        let non_probing = RegisterServiceUserRequest {
            confinement: None,
            ..req.clone()
        };
        let absent_bytes = encode_canonical(&non_probing).unwrap();
        let as_map: BTreeMap<String, Value> = decode(&absent_bytes).unwrap();
        assert!(
            !as_map.contains_key("confinement"),
            "a non-probing bridge must not emit the key at all: {as_map:?}"
        );
        let decoded: RegisterServiceUserRequest = decode(&absent_bytes).unwrap();
        assert_eq!(decoded.confinement, None);
    }

    /// A newer bridge may report a confinement state this build predates —
    /// additive-everywhere means that must survive the round trip rather than
    /// be rejected, which is why these fields are open strings and not enums.
    #[test]
    fn bridge_confinement_tolerates_an_unknown_future_state() {
        let c = BridgeConfinement {
            uid: 1002,
            sealed_store: "denied".into(),
            // A state no build today emits.
            landlock: "fully_with_network".into(),
            seccomp: "filter".into(),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&c).unwrap();
        let decoded: BridgeConfinement = decode(&bytes).unwrap();
        assert_eq!(decoded, c);
    }

    #[test]
    fn revoke_dkim_blob_request_round_trip() {
        let r = RevokeDkimBlobRequest {
            domain: "example.com".into(),
            selector: "2026a".into(),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: RevokeDkimBlobRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
    }

    #[test]
    fn report_auth_event_round_trip() {
        let r = ReportAuthEventRequest {
            actor_id: vec![0u8; 32],
            credential_id: "cred-1".into(),
            result: "ok".into(),
            source_ip: "10.0.0.1".into(),
            occurred_at: 1_700_000_000,
            reason: None,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: ReportAuthEventRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
    }

    #[test]
    fn list_pending_bridges_round_trip() {
        let req = ListPendingBridgesRequest {};
        let bytes = encode_canonical(&req).unwrap();
        let decoded: ListPendingBridgesRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);

        let reply = ListPendingBridgesReply {
            bridges: vec![ServiceUserInfo {
                bridge_id: "mta-1".into(),
                role: "mta".into(),
                status: "pending".into(),
                ed25519_pubkey: vec![0x07; 32],
                has_x25519: false,
                created_at: 1_700_000_000_000,
                approved_at: None,
                ..Default::default()
            }],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ListPendingBridgesReply = decode(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn approve_pending_bridge_round_trip() {
        let req = ApprovePendingBridgeRequest {
            ed25519_pubkey: vec![0x11; 32],
            role: "mta".into(),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: ApprovePendingBridgeRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);

        let reply = ApprovePendingBridgeReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ApprovePendingBridgeReply = decode(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn reject_pending_bridge_round_trip() {
        let req = RejectPendingBridgeRequest {
            ed25519_pubkey: vec![0x22; 32],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: RejectPendingBridgeRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);

        let reply = RejectPendingBridgeReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: RejectPendingBridgeReply = decode(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn revoke_service_user_round_trip() {
        let req = RevokeServiceUserRequest {
            bridge_actor_id: vec![0x33; 32],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: RevokeServiceUserRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);

        let reply = RevokeServiceUserReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: RevokeServiceUserReply = decode(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn set_mail_enabled_round_trip() {
        for enabled in [true, false] {
            let req = SetMailEnabledRequest {
                enabled,
                extra: Default::default(),
            };
            let bytes = encode_canonical(&req).unwrap();
            let decoded: SetMailEnabledRequest = decode(&bytes).unwrap();
            assert_eq!(decoded, req);
        }
        let reply = SetMailEnabledReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: SetMailEnabledReply = decode(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn webdav_keys_blob_kinds_round_trip() {
        let fetch_req = FetchWebdavKeysBlobRequest {
            actor_id: vec![0x11; 32],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&fetch_req).unwrap();
        let decoded: FetchWebdavKeysBlobRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, fetch_req);

        for blob in [Some(ByteBuf::from(vec![1u8, 2, 3])), None] {
            let reply = FetchWebdavKeysBlobReply {
                blob,
                extra: Default::default(),
            };
            let bytes = encode_canonical(&reply).unwrap();
            let decoded: FetchWebdavKeysBlobReply = decode(&bytes).unwrap();
            assert_eq!(decoded, reply);
        }

        let prov = ProvisionWebdavKeysBlobRequest {
            blob: ByteBuf::from(vec![0xAA; 16]),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&prov).unwrap();
        let decoded: ProvisionWebdavKeysBlobRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, prov);
    }

    #[test]
    fn set_caldav_enabled_round_trip() {
        for enabled in [true, false] {
            let req = SetCalDavEnabledRequest {
                enabled,
                extra: Default::default(),
            };
            let bytes = encode_canonical(&req).unwrap();
            let decoded: SetCalDavEnabledRequest = decode(&bytes).unwrap();
            assert_eq!(decoded, req);
        }
        let reply = SetCalDavEnabledReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: SetCalDavEnabledReply = decode(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn set_caldav_port_round_trip() {
        for port in [8443u16, 443, 9443, 65535, 1] {
            let req = SetCaldavPortRequest {
                port,
                extra: Default::default(),
            };
            let bytes = encode_canonical(&req).unwrap();
            let decoded: SetCaldavPortRequest = decode(&bytes).unwrap();
            assert_eq!(decoded, req);
        }
        let reply = SetCaldavPortReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: SetCaldavPortReply = decode(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn get_caldav_port_round_trip() {
        let req = GetCaldavPortRequest::default();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: GetCaldavPortRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);
        for port in [8443u16, 443, 9443, 65535, 1] {
            let reply = GetCaldavPortReply {
                port,
                extra: Default::default(),
            };
            let bytes = encode_canonical(&reply).unwrap();
            let decoded: GetCaldavPortReply = decode(&bytes).unwrap();
            assert_eq!(decoded, reply);
        }
    }

    #[test]
    fn set_mail_serving_enabled_round_trip() {
        for enabled in [true, false] {
            let req = SetMailServingEnabledRequest {
                enabled,
                extra: Default::default(),
            };
            let bytes = encode_canonical(&req).unwrap();
            let decoded: SetMailServingEnabledRequest = decode(&bytes).unwrap();
            assert_eq!(decoded, req);
        }
        let reply = SetMailServingEnabledReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: SetMailServingEnabledReply = decode(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn get_mail_serving_enabled_round_trip() {
        // Self-read (empty actor_id) and admin audit (explicit actor_id).
        for actor_id in [Vec::new(), vec![0x44u8; 32]] {
            let req = GetMailServingEnabledRequest {
                actor_id,
                extra: Default::default(),
            };
            let bytes = encode_canonical(&req).unwrap();
            let decoded: GetMailServingEnabledRequest = decode(&bytes).unwrap();
            assert_eq!(decoded, req);
        }
        for enabled in [true, false] {
            let reply = GetMailServingEnabledReply {
                enabled,
                extra: Default::default(),
            };
            let bytes = encode_canonical(&reply).unwrap();
            let decoded: GetMailServingEnabledReply = decode(&bytes).unwrap();
            assert_eq!(decoded, reply);
        }
    }

    #[test]
    fn capability_grant_rpc_round_trips() {
        let mint_req = MintGrantRequest {
            grant_blob: ByteBuf::from(vec![0xAB; 40]),
            ..Default::default()
        };
        let bytes = encode_canonical(&mint_req).unwrap();
        assert_eq!(decode::<MintGrantRequest>(&bytes).unwrap(), mint_req);

        let mint_reply = MintGrantReply {
            grant_id: ByteBuf::from(vec![0x22; 16]),
            ok: true,
            ..Default::default()
        };
        let bytes = encode_canonical(&mint_reply).unwrap();
        assert_eq!(decode::<MintGrantReply>(&bytes).unwrap(), mint_reply);

        let fetch_req = FetchGrantsRequest::default();
        let bytes = encode_canonical(&fetch_req).unwrap();
        assert_eq!(decode::<FetchGrantsRequest>(&bytes).unwrap(), fetch_req);

        let fetch_reply = FetchGrantsReply {
            grants: vec![ByteBuf::from(vec![1u8; 8]), ByteBuf::from(vec![2u8; 8])],
            ..Default::default()
        };
        let bytes = encode_canonical(&fetch_reply).unwrap();
        assert_eq!(decode::<FetchGrantsReply>(&bytes).unwrap(), fetch_reply);

        let renew_req = RenewGrantRequest {
            grant_id: ByteBuf::from(vec![0x33; 16]),
            new_epoch_start: Some(500),
            new_epoch_end: 999,
            appended_keys: vec![ByteBuf::from(vec![9u8; 12])],
            ..Default::default()
        };
        let bytes = encode_canonical(&renew_req).unwrap();
        assert_eq!(decode::<RenewGrantRequest>(&bytes).unwrap(), renew_req);
        // A renew that omits the start: the key is absent on the wire
        // and decodes as `None` (the nest then keeps the stored start).
        let keyless = RenewGrantRequest {
            new_epoch_start: None,
            ..renew_req
        };
        let bytes = encode_canonical(&keyless).unwrap();
        assert_eq!(decode::<RenewGrantRequest>(&bytes).unwrap(), keyless);

        let renew_reply = RenewGrantReply {
            ok: true,
            ..Default::default()
        };
        let bytes = encode_canonical(&renew_reply).unwrap();
        assert_eq!(decode::<RenewGrantReply>(&bytes).unwrap(), renew_reply);

        let revoke_req = RevokeGrantRequest {
            grant_id: ByteBuf::from(vec![0x44; 16]),
            ..Default::default()
        };
        let bytes = encode_canonical(&revoke_req).unwrap();
        assert_eq!(decode::<RevokeGrantRequest>(&bytes).unwrap(), revoke_req);

        let revoke_reply = RevokeGrantReply {
            ok: true,
            ..Default::default()
        };
        let bytes = encode_canonical(&revoke_reply).unwrap();
        assert_eq!(decode::<RevokeGrantReply>(&bytes).unwrap(), revoke_reply);
    }

    #[test]
    fn rescore_drain_plane_rpc_round_trips() {
        let wl_req = RescoreWorklistRequest {
            limit: 128,
            ..Default::default()
        };
        let bytes = encode_canonical(&wl_req).unwrap();
        assert_eq!(decode::<RescoreWorklistRequest>(&bytes).unwrap(), wl_req);

        let wl_reply = RescoreWorklistReply {
            units: vec![RescoreUnit {
                content_id: ByteBuf::from(vec![0x77; 32]),
                content_kind: "mail".into(),
                owner_actor_id: ByteBuf::from(vec![0x11; 32]),
                factor: "rspamd".into(),
                from_version: 1,
                to_version: 2,
                ..Default::default()
            }],
            ..Default::default()
        };
        let bytes = encode_canonical(&wl_reply).unwrap();
        assert_eq!(decode::<RescoreWorklistReply>(&bytes).unwrap(), wl_reply);

        let sub_req = SubmitScoresRequest {
            rows: vec![SubmitScoreRow {
                content_id: ByteBuf::from(vec![0x77; 32]),
                content_kind: "mail".into(),
                owner_actor_id: ByteBuf::from(vec![0x11; 32]),
                scored_at: 1_700_000_000,
                entries: vec![fauna_core::scoring::ScoreEntry {
                    factor: "rspamd".into(),
                    score: 250,
                    tier: 2,
                    scorer_version: 2,
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let bytes = encode_canonical(&sub_req).unwrap();
        assert_eq!(decode::<SubmitScoresRequest>(&bytes).unwrap(), sub_req);

        let sub_reply = SubmitScoresReply {
            written: 1,
            ok: true,
            ..Default::default()
        };
        let bytes = encode_canonical(&sub_reply).unwrap();
        assert_eq!(decode::<SubmitScoresReply>(&bytes).unwrap(), sub_reply);
    }

    #[test]
    fn spam_baseline_drain_rpc_round_trips() {
        // The spam-baseline publish drain (`mail-spam.md` § Encrypted-mode
        // interaction, ratified 2026-07-13) — the third drain-plane instance.
        let wl_req = SpamBaselineWorklistRequest {
            run_id: ByteBuf::from(vec![0xAB; 16]),
            ..Default::default()
        };
        let bytes = encode_canonical(&wl_req).unwrap();
        assert_eq!(
            decode::<SpamBaselineWorklistRequest>(&bytes).unwrap(),
            wl_req
        );

        let wl_reply = SpamBaselineWorklistReply {
            copies: vec![SpamBaselineCopy {
                owner_actor_id: ByteBuf::from(vec![0x11; 32]),
                sealed_copy: ByteBuf::from(vec![0xC1; 96]),
                ..Default::default()
            }],
            ..Default::default()
        };
        let bytes = encode_canonical(&wl_reply).unwrap();
        assert_eq!(
            decode::<SpamBaselineWorklistReply>(&bytes).unwrap(),
            wl_reply
        );

        let sub_req = SubmitSpamBaselineRequest {
            run_id: ByteBuf::from(vec![0xAB; 16]),
            merged_model: vec![0xEE; 64],
            contributors: 2,
            unreadable: 1,
            merged_contributors: vec![ByteBuf::from(vec![0x11; 32]), ByteBuf::from(vec![0x22; 32])],
            ..Default::default()
        };
        let bytes = encode_canonical(&sub_req).unwrap();
        assert_eq!(
            decode::<SubmitSpamBaselineRequest>(&bytes).unwrap(),
            sub_req
        );

        // A submit that omits the field (no `merged_contributors` key at all)
        // decodes with the names empty (the default) — which the nest reads as
        // nothing named, so nothing counted.
        #[derive(Serialize)]
        struct PreFieldSubmit {
            run_id: ByteBuf,
            #[serde(with = "serde_bytes")]
            merged_model: Vec<u8>,
            contributors: u32,
            unreadable: u32,
        }
        let old = PreFieldSubmit {
            run_id: ByteBuf::from(vec![0xAB; 16]),
            merged_model: vec![0xEE; 64],
            contributors: 2,
            unreadable: 0,
        };
        let bytes = encode_canonical(&old).unwrap();
        let decoded = decode::<SubmitSpamBaselineRequest>(&bytes).unwrap();
        assert_eq!(decoded.contributors, 2);
        assert!(decoded.merged_contributors.is_empty());
        assert!(decoded.extra.is_empty(), "a known key never lands in extra");

        // Empty merged_model = "nothing merged" is a valid wire value.
        let empty_sub = SubmitSpamBaselineRequest {
            run_id: ByteBuf::from(vec![0xAB; 16]),
            ..Default::default()
        };
        let bytes = encode_canonical(&empty_sub).unwrap();
        assert_eq!(
            decode::<SubmitSpamBaselineRequest>(&bytes).unwrap(),
            empty_sub
        );

        for ok in [true, false] {
            let sub_reply = SubmitSpamBaselineReply {
                ok,
                ..Default::default()
            };
            let bytes = encode_canonical(&sub_reply).unwrap();
            assert_eq!(
                decode::<SubmitSpamBaselineReply>(&bytes).unwrap(),
                sub_reply
            );
        }
    }

    #[test]
    fn spam_baseline_worklist_request_preserves_unknown_fields() {
        // A newer peer sends an extra key; it must round-trip through `extra`
        // (transport.md § forward-compat discipline, rule 4).
        let mut with_extra = SpamBaselineWorklistRequest {
            run_id: ByteBuf::from(vec![0x0F; 16]),
            ..Default::default()
        };
        with_extra
            .extra
            .insert("future_field".into(), Value::Integer(3.into()));
        let bytes = encode_canonical(&with_extra).unwrap();
        let decoded: SpamBaselineWorklistRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, with_extra);
        assert_eq!(
            decoded.extra.get("future_field"),
            Some(&Value::Integer(3.into()))
        );
    }

    #[test]
    fn rescore_worklist_request_preserves_unknown_fields() {
        // A newer peer sends an extra key; it must round-trip through `extra`
        // (transport.md § forward-compat discipline, rule 4).
        let mut with_extra = RescoreWorklistRequest {
            limit: 4,
            ..Default::default()
        };
        with_extra
            .extra
            .insert("future_field".into(), Value::Integer(9.into()));
        let bytes = encode_canonical(&with_extra).unwrap();
        let decoded: RescoreWorklistRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, with_extra);
        assert_eq!(
            decoded.extra.get("future_field"),
            Some(&Value::Integer(9.into()))
        );
    }

    #[test]
    fn capability_grant_request_preserves_unknown_fields() {
        // A newer peer sends an extra key; it must round-trip through `extra`
        // (transport.md § forward-compat discipline, rule 4).
        let mut with_extra = MintGrantRequest {
            grant_blob: ByteBuf::from(vec![0xCD; 4]),
            ..Default::default()
        };
        with_extra
            .extra
            .insert("future_field".into(), Value::Integer(7.into()));
        let bytes = encode_canonical(&with_extra).unwrap();
        let decoded: MintGrantRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, with_extra);
        assert_eq!(
            decoded.extra.get("future_field"),
            Some(&Value::Integer(7.into()))
        );
    }
}
