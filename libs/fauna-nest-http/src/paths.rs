//! Nest HTTP path constants — typed homes for the **permanent HTTP residue**
//! that native Rust consumers still call, one home so a path typo surfaces at
//! compile time, not at runtime.
//!
//! Audited 2026-05-11 across: `apps/fauna-linux/src/client.rs`
//! (the residual bearer-authed byte call sites + the calendar/event routes),
//! `libs/fauna-onboarding-machine/src/nest_api/`,
//! `libs/fauna-client/src/auth_client.rs`, and `libs/fauna-launch-machine/` —
//! cross-checked against the nest route table (`bins/fauna-nest/src/lib.rs`).
//! This records what the *consumers call*, not the entire nest API; add a
//! constant when a consumer starts calling a new permanent route. Re-check the
//! audit when a consumer moves.
//!
//! ## Two URL families
//! - `/api/v1/...` — the modern surface. The byte routes below.
//! - `/api/calendars`, `/api/events`, … (no `/v1` segment) — the calendar /
//!   event routes. Legacy form, **kept as-is** because the same routes back the
//!   CalDAV federation surface. Do *not* "helpfully" normalize a `/v1` prefix
//!   onto them. (No `paths` constant — hit via `fauna_core::ical` / CalDAV.)
//!
//! ## Scope — permanent HTTP residue only
//! The WS-RPC-everywhere migration (`docs/goal/architecture/transport.md` +
//! `api-layers.md`) retired every Layer-1 UI request/reply CRUD route to a
//! `fauna.<area>.*` WS-RPC kind, and each migrated path constant was removed
//! with its route (see the "Migrated to WS-RPC kinds" notes below for where
//! each went). What survives here is the permanent HTTP residue — byte / blob
//! transfer plus the zstd-tar data export — which never becomes a WS-RPC kind
//! (DAG-CBOR frames are the wrong shape for a multi-MB octet stream). The
//! authoritative residue inventory + full migration history is
//! `docs/goal/architecture/api-layers.md` (§ HTTP residue) + `transport.md`;
//! this module is just the typed call sites.
//!
//! NOTE: linux (`apps/fauna-linux/src/client.rs`) — the primary consumer —
//! calls these permanent byte routes through the typed constants in this module
//! (blob up/download, data export) for
//! typo-safety, rather than hand-rolling the literals. Add a constant — and
//! route the literal through it — when a consumer starts calling another
//! permanent route.
//!
//! ## `{id}`-templated routes
//! A route with a path parameter gets a small `fn` helper next to the prefix
//! const (e.g. [`blob::by_hash`]). Only the
//! helpers a consumer currently needs are here; more get added as consumers
//! migrate their literals onto these constants.

// ===========================================================================
// PERMANENT HTTP residue — byte / blob transfer + data export. Never becomes a
// WS-RPC kind.
// ===========================================================================

// The auth-bootstrap `POST /api/v1/auth/token` route — the LAST
// `deprecated_http` control-plane twin — was **deleted** at the
// WS-RPC-everywhere endgame: every consumer mints the bearer over the
// pre-identity `fauna.auth.handshake` WS-RPC kind (`fauna-client`'s `AuthClient`
// / `LaunchMachine` over `WsChallengeBearer`; the native apps over the shared
// FFI `mint_bearer`). The `KeypairBearer` HTTP minter + this `auth::TOKEN`
// constant were removed with that cutover (`fauna-nest-http/src/bearer.rs`).
// No `deprecated_http` control-plane twin remains; the silent-challenge
// `challenge`/`verify` twins were deleted earlier in the same rip-out.

// The onboarding wizard's nest surface (`fauna-onboarding-machine`'s `nest_api`)
// migrated to pre-identity WS-RPC kinds on every app;
// the path constants that the removed `reqwest` impl consumed went with it. The
// HTTP twins that still linger (until their non-onboarding callers migrate) are
// registered in `bins/fauna-nest` with string literals, not these constants.

/// **PERMANENT.** Blob / chunk transfer — multi-MB binary; HTTP
/// `application/octet-stream` + connection pooling (+ eventually range
/// requests), not DAG-CBOR frames.
pub mod blob {
    /// `POST` — upload bytes → `{hash}`. Bearer-authed.
    pub const UPLOAD: &str = "/api/v1/blob";
    /// `GET /api/v1/blob/{hash_hex}` — download by content hash. **Public**
    /// (no bearer) — keep on a bare client, not the bearer-attaching path.
    pub fn by_hash(hash_hex: &str) -> String {
        format!("/api/v1/blob/{hash_hex}")
    }

    /// `PUT /api/v1/blob/{cid_b32}` — upload **pure bytes** keyed by their CID
    /// (multibase base32). **Bearer-authed.** The nest verifies
    /// `blake3(body) == cid.digest()` before storing and is idempotent under a
    /// repeat PUT of the same CID.
    ///
    /// This is the route for an *opaque* blob — one the nest must not classify,
    /// thumbnail or seal-check — so it takes no `UploadSidecar`, unlike the
    /// multipart [`UPLOAD`]. The `__index` rail's sealed content-index segments
    /// are the canonical caller (`docs/goal/behavior/content-index.md` § Ingest
    /// triggers, v1): the bytes go here, the reference goes over
    /// `fauna.index.record`, in that order.
    ///
    /// Body cap is the nest's `BLOB_BODY_LIMIT` (10 MiB); a segment is one blob
    /// (no chunk-manifest form on that rail), so writers bound their own size.
    pub fn by_cid(cid_b32: &str) -> String {
        format!("/api/v1/blob/{cid_b32}")
    }
}

/// **PERMANENT.** The content-addressed chunk store — the two GETs the shared
/// client-side file-download walk rides (`fauna_core::file_download::BlobFetcher`;
/// `docs/goal/ui/backups.md` § Where logic lives → *Single-file byte download*).
/// Both are **public** routes (no bearer required — like [`blob::by_hash`]);
/// integrity rests on the content addresses the walk verifies, not the bearer.
pub mod chunk_store {
    /// `POST /api/v1/chunks` — store one chunk body under its **store key**
    /// (`X-Content-Hash`; a sealed body's own ciphertext hash). Bearer-authed
    /// (`BulkWriteAuth`: a session bearer, or a bulk write token). The client's
    /// half of the sync engine's chunk upload, reached without an engine by
    /// the Media page's content-keyed upload (`NestContentApi::post_bytes_keyed`).
    pub const CHUNKS_UPLOAD: &str = "/api/v1/chunks";
    /// `POST /api/v1/manifests` — store a canonical-encoded `ChunkManifest`
    /// (the nest refuses a body that is not one). Bearer-authed as
    /// [`CHUNKS_UPLOAD`].
    pub const MANIFESTS_UPLOAD: &str = "/api/v1/manifests";
    /// `GET /api/v1/manifests/{hash_hex}` — the canonical-encoded `ChunkManifest`
    /// bytes (sealed hash lists included; opening is the walk's job).
    pub fn manifest_by_hash(hash_hex: &str) -> String {
        format!("/api/v1/manifests/{hash_hex}")
    }
    /// `GET /api/v1/chunks/{hash_hex}` — one stored chunk body by **store key**
    /// (the ciphertext content hash).
    pub fn chunk_by_hash(hash_hex: &str) -> String {
        format!("/api/v1/chunks/{hash_hex}")
    }
    /// The answer route of relay serving (`file-sync.md` § Relay serving,
    /// step (3)): `POST /api/v1/chunks/relay/{request_id}` carries the stored
    /// chunk a `fauna.sync.chunk.wanted` push asked for, `DELETE` on the same
    /// path says the seat holds no such chunk. Bearer-authed as
    /// [`CHUNKS_UPLOAD`] (`BulkWriteAuth`), same body limit; the nest takes an
    /// answer only for a request it has pending and only from the actor it
    /// asked — anything else is `404` and touches nothing.
    pub fn chunk_relay_answer(request_id: u64) -> String {
        format!("/api/v1/chunks/relay/{request_id}")
    }
    /// The optional **folder hint** query parameter on [`chunk_by_hash`]
    /// (`?folder=<name>`; phase 5, `file-sync.md` § Content residency). Ignored
    /// on a store hit. On a store **miss** it lets the nest relay the chunk
    /// transiently from a seat that holds it: the hint names the folder the
    /// requester is reading (device selection + the residency lookup), and the
    /// hinted arm is **actor-scoped** — the route requires the session bearer
    /// and resolves the folder as owner-or-member; a hint without a bearer is
    /// `401`, a hint naming a folder the caller cannot read is `404`, same as a
    /// plain miss. Additive: a request that never sends it (only the sync engine does) gets the
    /// unchanged hit-or-404 behaviour.
    pub const FOLDER_HINT_PARAM: &str = "folder";

    /// The folder hint's address form (`?folder_hash=<hex>`): lowercase hex of
    /// the set's `set_name_hash`. Preferred over [`FOLDER_HINT_PARAM`] when
    /// both are sent — a sealed set's name rests blank on the nest, so only
    /// the hash finds it. A malformed value is `404`, never a fall back to the
    /// plaintext hint.
    pub const FOLDER_HASH_HINT_PARAM: &str = "folder_hash";
}

/// **PERMANENT.** Account data export — a streaming `application/zip` byte
/// download. The rest of the old `account` module (`ACCOUNT` / `PROFILE_HANDLE`
/// / `QUOTA` / `AM_I_ADMIN`) migrated to the `fauna.account.*` /
/// `fauna.quota.get` / `fauna.profile.handle.change` WS-RPC kinds; only this
/// byte download stays HTTP.
pub mod account {
    /// `GET` — the bare route, as the nest registers it
    /// (`bins/fauna-nest/src/lib.rs`). **Apps do not fetch this** — see
    /// [`EXPORT_FULL`], which is the URL an app's "Export My Data" issues.
    pub const EXPORT: &str = "/api/v1/export";

    /// `GET` — **the URL every app's "Export My Data" fetches**: the whole
    /// archive, the user's payload bytes included.
    ///
    /// `include_blobs` governs whether the blob store and the four actor-scoped
    /// segment planes ride along; the nest declares it `#[serde(default)]`, so
    /// **omitting it silently yields an index-only archive** — rows and a
    /// manifest, none of the user's actual content. That is not a shape an app
    /// may ask for: `account-data-plane.md` § Nest-side requirements item 1,
    /// *Payload stores* decision (5) rules the full archive the default and
    /// only app-requested form, with no user-facing toggle. The flag's name is
    /// historical (decision 1); its meaning is payload bytes generally.
    ///
    /// Guarded cross-app by
    /// `tests/e2e-unified/tests/test_account_export_carries_payload_bytes.py`.
    pub const EXPORT_FULL: &str = "/api/v1/export?include_blobs=true";
}

// ===========================================================================
// Migrated to WS-RPC kinds — modules removed with their routes. Kept as a
// "where did it go" map for cold reads; the authoritative inventory is
// `docs/goal/architecture/api-layers.md`.
// ===========================================================================

// `paths::feeds` (feed CRUD + feed-post queries + contributors) and
// `paths::posts` (create / fetch / interact) — deleted; the local-user
// surface migrated to the
// `fauna.feed.*` / `fauna.posts.*` WS-RPC kinds. Two cross-nest federation
// routes survive with no `paths` helper (hit directly nest-to-foreign-nest):
// `POST /api/v1/feeds/query` (discovery-feed poll, `peer_query.rs`) and
// `GET /api/v1/posts/{id}` (cross-nest post-body fetch — the `fetch_url`
// `peer_query.rs` builds, stored as the post-index `source`).

// `paths::conversations` (MLS channels, groups, key packages, welcomes) —
// deleted; migrated to the
// `fauna.conversations.*` WS-RPC kinds. The two cross-nest federation routes
// that survive (`GET /api/v1/keypackage/{actor_id}`,
// `POST /api/v1/welcome/{actor_id}`) have no `paths` helper.

// `paths::{contacts,knocks,notifications}` + `inbox::mode_for_actor` — deleted;
// the connection-management
// (knocks/contacts/inbox-mode) + notifications surfaces migrated to the
// `fauna.{knocks,contacts,inbox.mode,notifications}.*` WS-RPC kinds.

// `paths::inbox` (`for_actor`) — deleted in this cleanup. Both
// `/api/v1/inbox/{actor_id}` HTTP twins are gone (`bins/fauna-nest/src/lib.rs`):
// the store-and-forward drain is `fauna.inbox.{fetch,ack}` (caller-scoped peek
// + explicit ack, fixing the GET twin's mark-on-read data loss), the
// client→home-nest origination is `fauna.inbox.send`, and the cross-nest
// signed delivery is `fauna.federation.inbox.deliver`.

// `paths::file_sync` (`snapshot_file`) — deleted in the compat-remnant sweep
// with its route: `GET /api/v1/snapshots/{id}/file/{*path}` and the
// unconstanted `POST /api/v1/snapshots/{id}/restore` served only pre-seal
// plaintext snapshots. Snapshot bytes are reassembled client-side from the
// sealed manifest over WS-RPC (`fauna_core::file_download`); the snapshot
// control plane is the `fauna.filesync.snapshot.*` kinds.

// `paths::search` (`SEARCH`) — deleted in this cleanup; `GET /api/v1/search`
// migrated to the `fauna.search.query` WS-RPC kind
// (`bins/fauna-nest/src/search_handlers.rs`).

// `paths::spam` (`PREFERENCES`) — deleted in this cleanup; the
// `GET|PUT /api/v1/spam/preferences` routes migrated to the
// `fauna.spam.{get_preferences,set_preferences}` WS-RPC kinds
// (`bins/fauna-nest/src/spam_handlers.rs`). The classifier *model* + vocab byte
// GETs stay HTTP — see [`moderation`] above.

// `paths::bridges` (bridges-management + bridge-feeds + email filters +
// EMAIL_SEND) — deleted in the T9+T10 sweep; every app hits the
// `fauna.bridges.*` / `fauna.email.*` WS-RPC kinds via the typed
// `fauna-client-bridges::BridgesClient` / `fauna-client-email::EmailClient`.

// `paths::bluesky` (`AUTH_STATUS`, `THREAD`) — deleted in this cleanup.
// `GET /api/v1/bluesky/auth/status` was deleted with the Bluesky OAuth-status
// rework (`bins/fauna-nest/src/bluesky/auth_routes.rs`); `GET /api/v1/bluesky/thread`
// migrated to the `bluesky.feed.thread` WS-RPC kind
// (`bluesky_handlers::thread_handler`). The surviving Bluesky HTTP routes —
// `GET /api/v1/bluesky/auth/callback` (OAuth redirect) + `GET /api/v1/bluesky/media`
// (media proxy) — are external-protocol residue with no consumer constant.
