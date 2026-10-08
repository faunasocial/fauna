using FaunaApp.Core.Models;

namespace FaunaApp.Core.Services;

/// <summary>
/// The <b>permanent HTTP residue</b> to a FaunaNest server. This is NOT a
/// migration backlog: byte-bulk exceeds the WS-RPC frame budget and never
/// becomes a WS-RPC payload, and the health ping is an off-the-shelf monitoring
/// convention. See api-layers.md § HTTP residue — it owns that inventory, and
/// declares nothing in it migration-pending.
/// Everything else — identity, contacts, search, conversations, feed/posts,
/// events, bridges, backups, notifications, account, email-filters — rides the
/// WS-RPC façade (<see cref="INestRpcClient"/>) and its HTTP twins are deleted
/// nest-side (per-section notes below).
/// <para>
/// What actually crosses the wire as HTTP here: the <b>health ping</b>
/// (IsAvailableAsync / GetServiceStatusAsync) and the <b>blob byte plane</b>
/// (UploadBlobAsync / GetBlobAsync / GetBlobWithC2paAsync — i.e. all media — plus
/// GetContentAsync, the same bearer GET by nest-relative path, for a bridged post's
/// proxied picture).
/// Two members are on this interface but are NOT HTTP: GetAuthTokenAsync mints
/// its bearer over WS-RPC (`fauna.auth.handshake`, via the shared-Rust
/// MintBearer), and ConfigureAsync is a local base-URL setter.
/// </para>
/// <para>
/// Because these legs never join the WS connection, they do not inherit the
/// bearer connection's SPKI-pin trust decision — DirectNestClient's
/// ValidateServerCertificate callback must make the same call independently.
/// </para>
/// Implemented by DirectNestClient (standard nest API); mocked by
/// MockNestHttpClient in tests.
/// </summary>
public interface INestHttpClient : IAsyncDisposable
{
    // ── Health ──

    /// <summary>
    /// Checks if the nest HTTP server is reachable.
    /// </summary>
    Task<bool> IsAvailableAsync(CancellationToken ct = default);

    // ── Identity / auth bootstrap (NOT HTTP — see below) ──
    // Account state (`GetIdentityAsync`), handle availability, client-side
    // identity generation, and handle changes ALL left this plane: account
    // state → `fauna.account.get`; handle availability + identity generation are
    // driven by the shared OnboardingMachine; handle changes →
    // `fauna.profile.handle.change` (INestRpcClient.ChangeHandleAsync). Account
    // creation rides the anonymous WS-RPC `register` kind via the
    // OnboardingMachine.
    //
    // GetAuthTokenAsync is the bearer-bootstrap that the rest of this plane needs
    // before it can send an authenticated byte request — but it is NOT itself an
    // HTTP call any more: it mints over the pre-identity WS-RPC
    // `fauna.auth.handshake` kind (shared-Rust MintBearer). The
    // `POST /api/v1/auth/token` twin was deleted nest-side at the rip-out endgame
    // (api-layers.md § auth). It stays on this interface because it is what hands
    // the residual byte legs their Authorization header, not because it is HTTP.

    Task<string> GetAuthTokenAsync(string nestUrl, CancellationToken ct = default);

    // ── Configuration ──
    // Node info (version / domain) moved to WS-RPC: the admin dashboard reads the
    // running version from fauna.admin.status (INestRpcClient.AdminStatusAsync);
    // the GET /api/v1/node-info HTTP twin was deleted nest-side.

    Task ConfigureAsync(string? nestUrl, CancellationToken ct = default);

    // ── Messaging ──
    // The entire messaging surface left this plane: direct + group send,
    // conversation list/detail, and all community-group HTTP methods
    // (List/GetConversation, List/Create/SendGroupMessage, group members,
    // group messages) were removed as dead code — the Conversations page runs
    // entirely through the shared-Rust `ConversationsManager` (UniFFI), and the
    // nest HTTP twins were deleted in the conversations WS-RPC T8 cutover.
    // See docs/goal/ui/conversations.md.

    // ── Contacts ──
    // The roster, knock actions, AND add-contact knock-send all migrated to
    // WS-RPC (fauna.{contacts,knocks}.* + fauna.inbox.send, INestRpcClient); the
    // unauthenticated `POST /api/v1/inbox/{actor}` twin is gone client-side
    // (federation.md § Federation residue surface). NOTHING contact-related
    // remains on this HTTP plane — actor search left too (see the Search note).

    // ── Backups / Snapshots ──
    // Entirely on the WS-RPC façade (`fauna.filesync.snapshot.*`,
    // `fauna.sync.backup_status`, and the client-side byte walk
    // DownloadSnapshotFileBytesAsync — INestRpcClient); no snapshot route is HTTP.

    // ── Service Status ──

    Task<ServiceStatusInfo> GetServiceStatusAsync(CancellationToken ct = default);

    // Folder list/create/delete moved to the WS-RPC façade
    // (`fauna.folders.{list,create,delete}`, INestRpcClient.Folders*) — the
    // `/api/v1/file-sets*` HTTP twins were deleted nest-side (api-layers.md
    // § Folders). The Media page lists/creates over the seam; the Devices page
    // drives the full surface through DevicesMachine.
    // The sync control-plane HTTP twins (/api/v1/sync/{files,status,changes,
    // devices,register,conflicts}) were all lifted to fauna.sync.* via
    // INestRpcClient or the shared machines (files → the media machine;
    // devices → DevicesMachine; backup-status → BackupStatusAsync;
    // conflicts → ConflictsList/ResolveAsync).
    // The nest deleted these routes, so the HTTP client carries none of them.

    // Bridge Management (`fauna.bridges.*`, INestRpcClient) — the HTTP twins
    // were deleted nest-side; the WS-RPC seam carries list/link/unlink/follows.

    // ── Feeds + posts ──
    // The feed/posts read+write surface migrated to WS-RPC (fauna.feed.* /
    // fauna.posts.* via INestRpcClient) in the ws-rpc-everywhere cutover — the
    // HTTP twins were deleted nest-side. The ONLY feed-adjacent leg still on HTTP
    // is the blob byte plane (UploadBlobAsync / GetBlobAsync, below) — permanent
    // byte-bulk residue. Bridge-feed subscription is NOT on HTTP: it rides
    // `fauna.bridges.feeds.*` (INestRpcClient) and its `/api/v1/bridge-feeds` twin
    // is deleted nest-side.

    // Search migrated to the WS-RPC façade (`fauna.search.query`,
    // INestRpcClient.SearchQueryAsync) — the GET /api/v1/search HTTP twin is
    // deprecated/deleted nest-side (api-layers.md § Search). Full-text hits carry
    // opaque FTS doc-keys, not navigable post/actor ids.

    // Events + event-social (list/create/get/update/delete/rsvp/attendees/invite/
    // co-hosts/discussion) migrated to the WS-RPC façade (`fauna.events.*` /
    // `fauna.calendars.*`, INestRpcClient.Events*); the HTTP twins were dead
    // client-side after the events WS-RPC migration and have been removed.

    // Inbox mode moved to the WS-RPC façade (`fauna.inbox.mode.{get,set}`,
    // INestRpcClient) — the HTTP twins were deleted nest-side.

    // Spam preferences moved to the WS-RPC façade (`fauna.spam.{get,set}_preferences`,
    // INestRpcClient.Spam*) — the `GET|PUT /api/v1/spam/preferences` HTTP twin was
    // deleted nest-side. Moderation stats / actions / train likewise lost their HTTP
    // twins (api-layers.md § Moderation & Spam); their WS-RPC client seam is not built
    // yet (no fauna_client_moderation stats/actions method; train not FFI-exported), so
    // the windows Moderation page degrades those to zero/empty pending the nest surface.


    // ── MLS Key Packages (legacy client-side-MLS bootstrap) ──
    //
    // The keypackage POOL (count + upload) migrated to the WS-RPC
    // FfiConversationsClient seam (INestRpcClient.Keypackage{Count,Upload}Async;
    // conversations.md rule #2 "no client-side MLS"). The last client-side-MLS HTTP
    // call, FetchWelcomes, was removed — welcome receive is now the shared-Rust
    // push-driven loop on ConversationsSession (welcomes WS-pushed per
    // docs/goal/ui/conversations.md § MLS Welcome at-rest).

    // ── Account Management ──
    // Account get / quota / delete moved to the WS-RPC façade
    // (`fauna.account.get`, `fauna.quota.get`, `fauna.account.delete`,
    // INestRpcClient) — the HTTP twins were deleted nest-side.

    /// <summary>
    /// `GET /api/v1/export?include_blobs=true` — the full account archive
    /// (payload bytes included; no toggle — account-data-plane.md § Nest-side
    /// requirements item 1, Payload stores decision (5)). Stays HTTP residue
    /// like the snapshot byte plane above (a streaming `application/zip`
    /// download with no JSON control plane to split). android/apple/web hard-
    /// code the same literal (no shared-Rust UniFFI door exports
    /// `paths::account::EXPORT_FULL`); this mirrors them.
    /// </summary>
    Task<byte[]> ExportAccountDataAsync(CancellationToken ct = default);

    // Notifications moved to the WS-RPC façade (`fauna.notifications.{list,
    // mark_read,count}`, INestRpcClient) — the HTTP twins were deleted nest-side.

    // Event-social (invite / co-hosts / discussion) migrated to WS-RPC with the
    // rest of the events surface (see the Events note above) — twins removed.

    // Bridge Feed Subscriptions (`fauna.bridges.feeds.*`, INestRpcClient) — the
    // HTTP twins were deleted nest-side.

    // Snapshot prune / check / delete moved to the WS-RPC façade
    // (`fauna.filesync.snapshot.{prune,check,delete}`, INestRpcClient) — the
    // deprecated HTTP twins were removed with the rest of the backups migration.

    // ── Blobs ──
    // Encrypted-mode upload: the client seals + sidecars the bytes (shared-Rust
    // process_and_seal) and POSTs multipart/form-data (sidecar + bytes parts) per
    // docs/goal/architecture/encryption-at-rest.md § Media row. The caller picks the
    // per-context UploadAudience; the stored MIME rides in the sidecar (the post's
    // own MediaItem.media_type is supplied separately by the post builder).

    Task<string> UploadBlobAsync(byte[] data, UploadAudience audience, CancellationToken ct = default);

    /// <summary>Upload caller-supplied, ALREADY-SEALED blob bytes with a caller-supplied
    /// DAG-CBOR <c>UploadSidecar</c> — the gated-post path, where the shared
    /// <c>FeedManager::prepare_gated_blob</c> produced the sealed full body and the sidecar is
    /// the <c>PeriodRestrictedPost</c> class (<c>FaunaFfiMethods.GatedPostSidecar()</c>). Unlike
    /// <see cref="UploadBlobAsync"/> this NEVER re-seals (no <c>process_and_seal</c>) — the bytes
    /// are opaque ciphertext already, and re-sealing / the wrong sidecar would corrupt them.
    /// Returns the stored blob's hex hash (mirrors linux <c>fauna_client::upload_gated_post_blob</c>).</summary>
    Task<string> UploadSealedBlobAsync(byte[] sidecarCbor, byte[] sealedBytes, CancellationToken ct = default);

    /// <summary>Upload the multipart parts a shared-Rust seal already produced — thumbnail
    /// first (best-effort), then the primary, whose hex hash is returned. This is
    /// <see cref="UploadBlobAsync"/> MINUS <c>process_and_seal</c>: the caller resolved the
    /// audience and sealed the bytes on the far side of the FFI boundary
    /// (<c>FeedManager::seal_compose_attachment</c>), because a tier's period key must never
    /// cross it (media.md § Encryption at rest → Today's reality). Re-sealing here, or
    /// swapping the sidecar, would corrupt the ciphertext exactly as it would in
    /// <see cref="UploadSealedBlobAsync"/> — the difference from that method is only that
    /// this one carries the optional companion thumbnail. Mirrors linux/tui
    /// <c>fauna_client::upload_prepared_blob</c>.</summary>
    Task<string> UploadPreparedBlobAsync(
        byte[] sidecarCbor,
        byte[] bytes,
        byte[]? thumbnailSidecarCbor,
        byte[]? thumbnailBytes,
        CancellationToken ct = default);

    Task<byte[]> GetBlobAsync(string hash, CancellationToken ct = default);
    Task<(byte[] Data, bool HasC2pa)> GetBlobWithC2paAsync(string hash, CancellationToken ct = default);

    /// <summary>GET a nest-relative content path from the user's own nest with the session
    /// bearer and return its bytes — the by-path sibling of <see cref="GetBlobAsync"/>
    /// (render-model.md § D6c: an app fetches a bridged post's proxied picture exactly as it
    /// fetches <c>/api/v1/blob/&lt;hash&gt;</c>, with a different argument). The bytes are a
    /// third party's public media the nest proxies, so there is nothing to open and no C2PA
    /// header to read. Throws <see cref="System.ArgumentException"/> for a path that is not
    /// nest-relative (<see cref="NestContentPath"/>) — the bearer never leaves the nest.</summary>
    Task<byte[]> GetContentAsync(string path, CancellationToken ct = default);

    // Email Filters (`fauna.email.filters.*`, INestRpcClient) — the HTTP twins
    // were deleted nest-side.

    // ── Admin ──
    // User administration (list / invite codes / invite requests) moved to the
    // admin-users hub over the fauna.admin.* WS-RPC kinds (FfiAdminClient,
    // libs/fauna-ffi/src/admin.rs). The dashboard stats (fauna.admin.stats) and
    // the is-admin gate (fauna.account.am_i_admin) also moved to WS-RPC
    // (INestRpcClient.AdminStatsAsync / AmIAdminAsync) — the /admin/api/stats
    // twin was deleted nest-side, so this HTTP client carries neither.
}
