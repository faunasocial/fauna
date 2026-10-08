import Foundation

/// Stable UniFFI `AtprotoSettingsObserver` the memoized machine is actually
/// built with — see `APIClient.atprotoSettingsMachine(observer:)`. Retargets
/// (weakly) to whichever page's observer is current; mirrors
/// `AtprotoSettingsObserverBox` (`AtprotoSettingsVM.swift`), one level up.
private final class AtprotoObserverFanout: AtprotoSettingsObserver, @unchecked Sendable {
    weak var target: AtprotoSettingsObserver?
    func onChanged() {
        target?.onChanged()
    }
}

public class APIClient {
    public let nodeUrl: URL
    /// `internal(set)`, not `private(set)`: `APIClientActorAdoptionTests`
    /// seeds/reads this from within the FaunaKit module without a live nest
    /// connection — the three FFI-opaque caches `adoptActor` also drops
    /// cannot be constructed the same way (no bare initializer; only
    /// `ensureNestConnected()` mints one), so this is the one drop the fix
    /// can witness at runtime rather than structurally.
    public internal(set) var currentToken: String?
    var token: String?
    var tokenExpiresAt: Date?
    /// The residual-HTTP leg (blob upload / download / `HEAD`, chunks,
    /// manifests — byte-bulk that stays HTTP/1.1, `transport.md` § HTTP
    /// residue). NOT `URLSession.shared`: that session makes no trust decision,
    /// so it rejected the nest's self-signed floor cert and media broke against
    /// a same-box nest. This one carries ``NestCertTrustSessionDelegate``, the
    /// twin of windows' `DirectNestClient` cert-validation callback — accept
    /// iff WebPKI-valid, OR loopback, OR the served SPKI equals the pin the WS
    /// handshake graduated (security.md § Transport trust).
    private let session: URLSession

    public init(nodeUrl: URL) {
        self.nodeUrl = nodeUrl
        self.session = NestCertTrust.makeSession(nestUrl: nodeUrl)
    }

    deinit {
        // A session with a delegate retains that delegate until invalidated;
        // let in-flight requests finish, then release it with the client.
        session.finishTasksAndInvalidate()
    }

    // MARK: - Auth

    /// Mint a runtime bearer over the shared FFI `mintBearer`
    /// (`fauna.auth.handshake`, WS-RPC) — replacing the former `POST
    /// /api/v1/auth/token` HTTP route, which the nest no longer serves. The shared `mint_bearer` is a faithful 1:1 of the old
    /// route: same `actor_id ‖ timestamp_be` sign + nest-side side effects
    /// (account lockout, auto-register vs. private-nest reject), so behavior is
    /// unchanged — only the transport moves off HTTP. Mirrors `silentSignIn`'s
    /// FFI-over-WS shape. See `docs/goal/architecture/transport.md` § Pre-identity
    /// / HTTP residue and `api-layers.md` § auth.
    public func authenticate(secret: String) async throws {
        // Through `adoptActor`, moved to the FRONT — it used to run after the
        // token was set below, which was fine before `adoptActor` also dropped
        // `token`/`currentToken`/`tokenExpiresAt` on a real switch:
        // calling it after minting the bearer would immediately wipe the
        // token this call is about to set.
        adoptActor(secret)
        let r = try await mintBearer(nestUrl: nodeUrl.absoluteString, secret: hex_to_data(secret))
        self.token = r.token
        self.currentToken = r.token
        // `expiresAt` is absolute unix seconds; refresh 60s early.
        self.tokenExpiresAt = Date(timeIntervalSince1970: TimeInterval(r.expiresAt) - 60)
    }

    /// Result of a successful silent sign-in. Mirrors the
    /// `fauna.auth.verify` reply (`FfiSilentSignInResult`). The launch path
    /// uses `handle`/`domain`/`tier` to refresh the keychain server-data
    /// cache; `token` is also surfaced so the caller can stash it
    /// alongside the `/auth/token` token if desired.
    public struct SilentSignInResult {
        public let token: String
        public let handle: String
        public let domain: String
        public let tier: String
    }

    /// Silent sign-in over the **pre-identity (anonymous)** WS connection:
    /// the `fauna.auth.challenge` → sign(`AUTH_VERIFY_V2 ‖ actor_id ‖ nonce ‖ nest_id`,
    /// domain-tagged and tagged-only since 2026-08-17) →
    /// `fauna.auth.verify` ceremony, via the shared FFI `silentChallenge`
    /// (the launch-machine `WsAuthConnector` the linux app also rides) —
    /// no Swift-side WS-RPC, no HTTP. The `POST /api/v1/auth/{challenge,
    /// verify}` twins were deleted nest-side. See
    /// `docs/goal/architecture/transport.md` § Pre-identity.
    ///
    /// Returns `nil` on `fauna.auth.not_registered` (the old HTTP 404) — the
    /// caller treats it as a normal "not yet registered" outcome and drops
    /// into onboarding. Throws on every reachability fault / invalid secret
    /// (the shared outcome collapses transient vs. terminal into one
    /// retryable error). Used by the launch flow to refresh the
    /// handle/domain/tier server-data cache — see
    /// `docs/goal/architecture/long-term-store.md`.
    public func silentSignIn(secret: String) async throws -> SilentSignInResult? {
        guard let r = try await silentChallenge(
            nestUrl: nodeUrl.absoluteString, secret: hex_to_data(secret))
        else { return nil }
        return SilentSignInResult(token: r.token, handle: r.handle, domain: r.domain, tier: r.tier)
    }

    // MARK: - Inbox

    /// `fauna.inbox.fetch` — peek the per-actor store-and-forward drain over the
    /// shared WS-RPC connection (replaces the retired `GET /api/v1/inbox/{actor}`
    /// twin). The kind is caller-scoped — the nest binds the drain to the
    /// connection actor, so no actor rides the wire and this takes no `actorId`.
    /// **Peek-only — never `ack`s** (ratified read-only-surface policy,
    /// `api-layers.md` § Inbox & Messaging): the watchOS inbox is a glance/display
    /// surface that never durably *applies* items, so acking would re-introduce the
    /// mark-on-read data-loss the `fetch`/`ack` split fixed — only the (still
    /// dormant) durable-apply consumer ever acks. Mirrors android's `fetch(0u)`;
    /// `limit: 0` selects the handler's default page size.
    public func fetchInbox() async throws -> [Data] {
        let reply = try await inboxClient().fetch(limit: 0)
        return reply.items.map { $0.payload }
    }

    /// `fauna.inbox.send` — compose the canonical signed `(ContactRequest, Post)`
    /// tuple with the shared `buildSignedEmail` writer (the single cross-app
    /// composer — never re-inline it, priority #2/#3) and hand it to our home nest
    /// for delivery. `recipientNestUrl=nil` ⇒ same-nest local delivery, faithful to
    /// the retired `POST /api/v1/inbox/{actor}` twin which only ever reached
    /// recipients on this client's own home nest (cross-nest awaits client-side peer
    /// discovery). The nest binds `cr.sender == caller`, so the tuple is signed by
    /// the authed actor (`self.secret`). Mirrors linux `build_and_send`
    /// (`apps/fauna-linux/src/client.rs`); see `federation.md` § Federation residue
    /// surface + `api-layers.md` § Inbox & Messaging.
    public func sendToInbox(recipientActorId: String, subject: String, body: String) async throws {
        guard let secret else { throw APIError.ffiError("No secret for inbox send") }
        let payload = try buildSignedEmail(
            secret: hex_to_data(secret),
            to: hex_to_data(recipientActorId),
            subject: subject,
            body: body,
            nodeUrl: nodeUrl.absoluteString)
        _ = try await inboxClient().send(
            recipientActorId: recipientActorId,
            recipientNestUrl: nil,
            payloadBytes: payload)
    }

    // MARK: - Blobs

    /// Seal + sidecar every blob upload through the shared-Rust
    /// `processAndSealUpload` packer and POST `multipart/form-data` (`sidecar` +
    /// `bytes`), matching the nest contract and the other five apps (windows
    /// `UploadBlobAsync`, linux `upload_staged_blob`, android `uploadBlob`).
    /// The legacy raw-octet-stream POST is retired — every blob upload now rides
    /// the sidecar wire (`docs/goal/ui/media.md` § Encryption at rest; the
    /// nest's per-class verifier + the deferred strict flip,
    /// tracked internally).
    ///
    /// `audience` selects the per-blob seal: `.publicPost` passes the bytes
    /// through as signed plaintext; `.library(backupKey:)` seals under the
    /// owner's `BackupKey` (use ``libraryUploadAudience()`` to derive it). The
    /// MLS-keyed audiences are intentionally absent until the shared-Rust
    /// seal-by-id helper lands (tracked internally).
    public func uploadBlob(data: Data, audience: UploadAudience) async throws -> BlobResponse {
        let payload = try processAndSealUpload(raw: data, audience: mapUploadAudience(audience))
        // Best-effort thumbnail upload: the on-device `process_media` (now real
        // on every native app) renders one for a >300px image, and the nest
        // serves it via `?thumb=1` off the primary's stored sidecar hash — so a
        // thumbnail failure must not block the primary. Mirrors every sibling client.
        if let thumbnail = payload.thumbnail {
            _ = try? await postMultipartBlob(sidecarCbor: thumbnail.sidecarCbor,
                                             sealedBytes: thumbnail.bytes)
        }
        let responseData = try await postMultipartBlob(sidecarCbor: payload.primary.sidecarCbor,
                                                       sealedBytes: payload.primary.bytes)
        return try JSONDecoder().decode(BlobResponse.self, from: responseData)
    }

    /// Upload one blob's **already-prepared** parts — from
    /// ``FfiFeedManager/sealComposeAttachment(raw:)`` — and return the nest's hex
    /// hash. Thumbnail first, best-effort, then the primary: the same two POSTs
    /// ``uploadBlob(data:audience:)`` makes, minus its `processAndSealUpload`.
    ///
    /// That omission is the point. Shared Rust has already resolved the
    /// composer's audience and sealed (or passed through) these bytes under the
    /// post's own `seal_id`; re-running the packer here would re-seal them under
    /// a *different* audience and break the one-key-opens-body-and-photo binding
    /// `ui/media.md` § Encryption at rest requires. windows'
    /// `UploadPreparedBlobAsync` is the same seam for the same reason.
    public func uploadPreparedBlob(primary: ComposeUploadPart,
                                   thumbnail: ComposeUploadPart?) async throws -> String {
        if let thumbnail {
            _ = try? await postMultipartBlob(sidecarCbor: thumbnail.sidecarCbor,
                                             sealedBytes: thumbnail.bytes)
        }
        let responseData = try await postMultipartBlob(sidecarCbor: primary.sidecarCbor,
                                                       sealedBytes: primary.bytes)
        return try JSONDecoder().decode(BlobResponse.self, from: responseData).hash
    }

    /// Derive the owner's `BackupKey` for the `.library` upload audience from the
    /// authed actor's identity seed (`self.secret`), via the shared-Rust
    /// `backupKeyDerive`. The key never leaves the client; the nest holds library
    /// blobs opaque in both storage modes (`docs/goal/ui/media.md` § Encryption
    /// at rest — Library media).
    public func libraryUploadAudience() throws -> UploadAudience {
        guard let secret else { throw APIError.ffiError("No secret for library backup key") }
        return .library(backupKey: try backupKeyDerive(secret: hex_to_data(secret)))
    }

    /// The owner's raw 32-byte library `BackupKey` (`backupKeyDerive`) for the
    /// shared `MediaMachine::upload_selected` gesture, which takes the key bytes
    /// directly (Library audience — owner-only media at rest; `docs/goal/ui/media.md`
    /// § Encryption at rest — Library media). Same derivation as
    /// `libraryUploadAudience`, just the bytes the FFI gesture wants; the key never
    /// leaves the client.
    public func ownerBackupKeyBytes() throws -> Data {
        guard let secret else { throw APIError.ffiError("No secret for library backup key") }
        return try backupKeyDerive(secret: hex_to_data(secret))
    }

    /// The authed actor's raw 32-byte identity secret, for the shared
    /// `MediaMachine::set_share_author` seam — the share-link author signs the
    /// token and derives the filename-seal root from it inside shared Rust
    /// (`docs/goal/behavior/share-links.md` § Where logic lives). Internal to
    /// FaunaKit: the one consumer is `MediaMachineVM`, and the secret never
    /// leaves the client. Mirrors linux/tui's `secret_bytes()` hand-off.
    func identitySecretBytes() throws -> Data {
        guard let secret else { throw APIError.ffiError("No secret for share-link author") }
        return hex_to_data(secret)
    }

    /// Upload a gated post's **already-sealed** full-body blob (from
    /// ``FfiFeedManager/prepareGatedBlob()``) and return the nest's hex hash — which
    /// the caller passes to ``FfiFeedManager/submitGatedPost(uploadedHash:)`` (it
    /// must echo the staged post's `encrypted_ref`). Transport glue only: unlike
    /// ``uploadBlob(data:audience:)`` this does NOT re-seal — the shared post builder
    /// already sealed the bytes. `sidecar` is the caller's own
    /// ``FfiFeedManager/gatedUploadSidecar()`` — `GroupRestrictedPost` for a room
    /// post, a tier's `PeriodRestrictedPost` otherwise — decided off the staged
    /// post, never here (`docs/goal/ui/feed.md` § Encryption at rest). Mirrors
    /// linux's `fauna_client::upload_sealed_post_blob`.
    public func uploadGatedPostBlob(sealed: Data, sidecar: Data) async throws -> String {
        let responseData = try await postMultipartBlob(sidecarCbor: sidecar,
                                                       sealedBytes: sealed)
        return try JSONDecoder().decode(BlobResponse.self, from: responseData).hash
    }

    public func blobUrl(hash: String) -> URL {
        nodeUrl.appendingPathComponent("api/v1/blob/\(hash)")
    }

    /// The URL of a nest-relative content path the nest handed out whole — a
    /// bridged post's `ProxiedImage` path (`/api/v1/bluesky/media?url=…`,
    /// `render-model.md` § D6c). Fetched like ``blobUrl(hash:)``'s bytes, through
    /// ``get(url:)`` with the session bearer. `relativeTo:` keeps the query a
    /// query (see the private `get(_:)`'s note on `appendingPathComponent`).
    public func contentUrl(path: String) -> URL? {
        URL(string: path, relativeTo: nodeUrl)
    }

    /// Read a blob's `x-c2pa` header via `HEAD /api/v1/blob/{hash}` (`ui/media.md`
    /// § C2PA provenance) — **the uploader's own assertion, not a verdict.** The
    /// nest stores `has_c2pa` for a public-post blob without inspecting its bytes,
    /// so anyone posting through a modified client or a raw multipart request can
    /// make this `true` for an image that carries no manifest. It is only the
    /// *pre-filter* of the `c2pa-badge` check: `false` ends it without fetching
    /// anything (the answer for essentially every post), `true` is what buys
    /// ``FeedVM/hasC2pa(_:)``'s byte-level parse. A HEAD request, never a `GET`, so
    /// the pre-filter does not pull the whole blob just to read one header. The
    /// twin of tui's `head_has_c2pa` and android's `checkBlobC2pa`. Best-effort:
    /// any failure (network, non-2xx, missing header) is `false`, which the caller
    /// treats as "no badge".
    public func hasC2paAssertion(hash: String) async -> Bool {
        guard var request = try? await authorizedRequest(blobUrl(hash: hash)) else { return false }
        request.httpMethod = "HEAD"
        guard let (_, response) = try? await session.data(for: request),
              let http = response as? HTTPURLResponse,
              (200...299).contains(http.statusCode)
        else { return false }
        return http.value(forHTTPHeaderField: "x-c2pa") == "true"
    }

    // MARK: - Chunks & Manifests

    public func uploadChunk(hash: String, data: Data) async throws {
        try await postRaw(path: "api/v1/chunks", body: data,
                          contentType: "application/octet-stream")
    }

    public func downloadChunk(hash: String) async throws -> Data {
        try await get("api/v1/chunks/\(hash)")
    }

    public func checkChunks(hashes: [String]) async throws -> [String] {
        let body = try JSONEncoder().encode(["hashes": hashes])
        let data = try await postJSON(path: "api/v1/chunks/check", body: body)
        let response = try JSONDecoder().decode(CheckChunksResponse.self, from: data)
        return response.missing
    }

    public func uploadManifest(data: Data) async throws {
        try await postRaw(path: "api/v1/manifests", body: data,
                          contentType: "application/octet-stream")
    }

    public func downloadManifest(hash: String) async throws -> Data {
        try await get("api/v1/manifests/\(hash)")
    }

    // MARK: - Sync

    /// `fauna.sync.register` over WS-RPC. The connection actor replaces the old
    /// HTTP `actor_id` body field; `actorId` is accepted for source
    /// compatibility but no longer sent.
    public func registerDevice(deviceId: String, label: String, actorId: String) async throws {
        _ = try await syncClient().register(deviceId: deviceId, label: label)
    }

    // MARK: - Snapshots

    /// `fauna.filesync.snapshot.create_folder` over WS-RPC — capture a
    /// point-in-time snapshot of a folder (client-driven "back up now": no
    /// tags, unattributed). The reply's `fileCount`/`totalBytes` drive the
    /// completion banner.
    public func createSnapshot(folder: String) async throws -> SnapshotResponse {
        let r = try await snapshotsClient().snapshotCreateFolder(folder: folder, tags: [])
        return SnapshotResponse(
            id: Int(r.id),
            fileCount: Int(r.fileCount),
            totalBytes: Int(r.totalBytes),
            createdAt: Int(r.createdAt),
            parentId: nil,
            tags: r.tags,
            deviceId: r.deviceId?.hexString)
    }

    // MARK: - Message-kind restore (backups.md §§ Restore …)
    //
    // The restore reads + the restore action ride the shared
    // `libs/fauna-client-snapshots` WS-RPC composition projected as
    // `FfiSnapshotsClient` (`backups.md` § Where logic lives → Restore history /
    // divergence reads + the restore action). These four wrappers are thin
    // FaunaKit consumers of that seam — the FFI row types cross unmapped (they
    // carry only primitives + `Data?`/`String?`, formatted by `RestoreVM`),
    // mirroring android's `ApiClient` restore seam.

    /// `fauna.filesync.snapshot.list` for the local-restore picker
    /// (`restore-snapshot-select`). Owner-implicit, `folder: nil` → all snapshots
    /// (each row labelled by its `messageKind`); matches android/linux, which do
    /// not filter to message-kind client-side (matching keeps priority #1).
    public func fetchMessageKindSnapshots() async throws -> [FfiSnapshotSummary] {
        try await snapshotsClient().snapshotList(messageKind: nil, folder: nil, limit: 0)
    }

    /// `fauna.filesync.snapshot.list_restore_history` — the `restore_history`
    /// rows for this nest (`backups.md` § Restore history). `limit: 0` = all.
    public func fetchRestoreHistory(limit: UInt32 = 0) async throws -> [FfiRestoreHistoryRow] {
        try await snapshotsClient().snapshotListRestoreHistory(limit: limit)
    }

    /// `fauna.filesync.snapshot.list_restore_divergence` — the per-snapshot MUA
    /// divergence rows the banner + forensic modal read (`backups.md`
    /// § Restore divergence).
    public func fetchRestoreDivergence(snapshotId: Int64) async throws -> [FfiRestoreDivergenceRow] {
        try await snapshotsClient().snapshotListRestoreDivergence(snapshotId: snapshotId)
    }

    /// `fauna.filesync.snapshot.restore_message_kind` — replays the pinned
    /// placement manifest server-side and inserts a `restore_history` row
    /// (`backup-restore.md` § 6). Pre-condition: the bridge must NOT currently be
    /// serving the actor (409 otherwise) — the user disables mail via
    /// `mail-settings-enabled-toggle`, restores, then re-enables (the reply's
    /// `note` warns when the wrapped-MLS blob bundle isn't restored yet; progress ends
    /// "Done — restart the bridge."). Not auto-sequenced, matching android/linux.
    public func restoreMessageKind(
        snapshotId: Int64, confirmId: String
    ) async throws -> FfiSnapshotRestoreReply {
        try await snapshotsClient().snapshotRestoreMessageKind(
            snapshotId: snapshotId, confirmId: confirmId)
    }

    /// Download one file's bytes from a snapshot for local save (single-file
    /// restore, backup-restore.md § 3). A direct engine-host-style FFI call
    /// (not `snapshotsClient()`'s WS-RPC), same `nest`+`ownerSecret` idiom as
    /// `removeBackupDestination`/`rotateDeploymentSeed` — the free fn
    /// resolves path -> manifest_hash server-side, exactly like
    /// `restoreSnapshotToDir`.
    public func downloadSnapshotFile(deviceId: String, snapshotId: Int, path: String) async throws -> Data {
        guard let secret else { throw APIError.ffiError("No secret for snapshot download") }
        return try await downloadSnapshotFileBytes(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret),
            deviceId: hex_to_data(deviceId), snapshotId: Int64(snapshotId), path: path)
    }

    /// `fauna.filesync.snapshot.diff` over WS-RPC — added/removed/modified
    /// files between two snapshots of the same folder.
    public func snapshotDiff(a: Int, b: Int) async throws -> SnapshotDiffResponse {
        let r = try await snapshotsClient().snapshotDiff(a: Int64(a), b: Int64(b))
        return SnapshotDiffResponse(
            snapshotA: Int(r.snapshotA),
            snapshotB: Int(r.snapshotB),
            added: r.added.map { DiffEntry(path: $0.path, sizeBytes: Int($0.sizeBytes)) },
            removed: r.removed.map { DiffEntry(path: $0.path, sizeBytes: Int($0.sizeBytes)) },
            modified: r.modified.map {
                ModifiedEntry(path: $0.path, oldSize: Int($0.oldSize), newSize: Int($0.newSize))
            },
            summary: DiffSummary(
                addedCount: Int(r.summary.addedCount),
                removedCount: Int(r.summary.removedCount),
                modifiedCount: Int(r.summary.modifiedCount),
                addedBytes: Int(r.summary.addedBytes),
                removedBytes: Int(r.summary.removedBytes),
                netBytes: Int(r.summary.netBytes)))
    }

    /// `fauna.stats.get` over WS-RPC — per-folder repository storage stats
    /// (the Backups stats popover). Always scoped to a `folder`, so the reply
    /// is always the `.folder` variant; `dedup_ratio_micro` ÷ 1e6 at the edge.
    public func repoStats(folder: String) async throws -> RepoStatsResponse {
        let reply = try await statsClient().statsGet(folder: folder)
        switch reply {
        case let .folder(folder, snapshotCount, _, totalFiles, rawSizeBytes,
                          storedSizeBytes, dedupRatioMicro, storageBackend):
            return RepoStatsResponse(
                folder: folder,
                snapshotCount: Int(snapshotCount),
                totalFiles: Int(totalFiles),
                rawSizeBytes: Int(rawSizeBytes),
                storedSizeBytes: Int(storedSizeBytes),
                dedupRatio: Double(dedupRatioMicro) / 1_000_000.0,
                storageBackend: storageBackend)
        case .global:
            // repoStats always passes a folder, so the nest returns Folder.
            throw APIError.ffiError("stats.get returned global stats for a folder query")
        }
    }

    // MARK: - Node Info & Registration
    //
    // Nothing here any more. The nest deleted every HTTP route this section
    // called: discovery (`GET /api/v1/{node-info,handle-available/{handle}}`)
    // migrated to the pre-identity WS-RPC kinds `fauna.{nest.info,
    // handle.available}`, and `POST /api/v1/register` retired for
    // `fauna.account.register`. The only reader was the admin dashboard's
    // Version card, which now takes `version` off `fauna.admin.status` via the
    // typed `FfiAdminClient` — the same read tui and linux make; the other
    // three methods had no callers on any Apple surface. Anything needing
    // discovery again takes the shared-Rust face, never a REST twin.

    /// `fauna.quota.get` over WS-RPC (the `GET /api/v1/quota` HTTP twin was
    /// deleted nest-side, T3).
    public func fetchQuota() async throws -> QuotaResponse {
        let reply = try await accountClient().quotaGet()
        return QuotaResponse(
            tier: reply.tier,
            inbox: QuotaUsage(usedBytes: Int(reply.inbox.usedBytes), maxBytes: Int(reply.inbox.maxBytes)),
            storage: QuotaUsage(usedBytes: Int(reply.storage.usedBytes), maxBytes: Int(reply.storage.maxBytes)),
            devices: QuotaDevices(used: Int(reply.devices.used), max: Int(reply.devices.max)),
            features: QuotaFeatures(
                versionedBackup: reply.features.versionedBackup,
                bridges: reply.features.bridges,
                maxFeeds: Int(reply.features.maxFeeds)
            )
        )
    }

    /// `fauna.sync.status` over WS-RPC — source liveness for a set.
    public func fetchSyncStatus(folder: String) async throws -> SyncStatus {
        let s = try await syncClient().status(folder: folder)
        return SyncStatus(folder: s.folder, sourceOnline: s.sourceOnline)
    }

    // MARK: - Contacts & Knocks

    public func fetchKnocks(actorId: String) async throws -> [Knock] {
        try await contactsClient().knocksList().map(Knock.init(ffi:))
    }

    public func acceptKnock(actorId: String, peerId: String) async throws {
        try await contactsClient().knocksAccept(peerId: peerId)
    }

    public func blockKnock(actorId: String, peerId: String) async throws {
        try await contactsClient().knocksBlock(peerId: peerId)
    }

    /// Unblock the viewed actor — the guarded clear-the-edge over
    /// `fauna.knocks.unblock` (`ContactStatus` → `None`; a no-op on a non-blocked
    /// edge, so a stale snapshot can't clear a live relationship). The inverse of
    /// `blockKnock`; `contacts.md` § Where logic lives → Unblock. The nest uses
    /// only `peerId` (the target).
    public func unblockKnock(actorId: String, peerId: String) async throws {
        try await contactsClient().knocksUnblock(peerId: peerId)
    }

    public func dismissKnock(actorId: String, peerId: String) async throws {
        try await contactsClient().knocksDismiss(peerId: peerId)
    }

    public func fetchContacts(actorId: String) async throws -> [Contact] {
        try await contactsClient().contactsList().map(Contact.init(ffi:))
    }

    public func confirmContact(actorId: String, peerId: String) async throws {
        try await contactsClient().contactsConfirm(peerId: peerId)
    }

    /// Send a contact request — a knock **is** an inbox message, so it rides
    /// `fauna.inbox.send` (the `POST /api/v1/contacts/{actor}/knock` twin was deleted
    /// in the T4 cutover; `api-layers.md` § Contacts & Knocks). The payload is composed
    /// by the dedicated shared `buildKnockPayload` writer, which bakes in the canonical
    /// `KNOCK_SUBJECT`/`KNOCK_BODY` sentinel (`fauna_client_core::email`) — never
    /// re-inline those as Swift literals, or the wire shape drifts per client
    /// (priority #2/#4). The home nest local-delivers or originates the federation leg
    /// (InboxMode-gated); the recipient sees the knock in `fauna.knocks.list`. Mirrors
    /// windows `NestRpcClient.BuildKnockPayload` and android `buildKnockPayload`.
    /// The nest binds the sender to the authed caller, so `actorId` goes unused — kept
    /// for call-site parity with the sibling `blockKnock`/`confirmContact` methods.
    ///
    /// `recipientNestUrl` is where the knock must go when the peer lives on ANOTHER
    /// nest: `nil` delivers on this nest, `Some(peer)` makes the home nest originate
    /// the federation leg. The Contacts page passes none (its lookup already resolved
    /// the peer on this nest); the profile page passes ``knockRoute(actorId:profileBody:)``
    /// — the shared rule over the profile the page already fetched.
    public func sendKnock(actorId: String, peerId: String, recipientNestUrl: String? = nil) async throws {
        guard let secret else { throw APIError.ffiError("No secret for knock") }
        let payload = try buildKnockPayload(
            secret: hex_to_data(secret),
            to: hex_to_data(peerId),
            nodeUrl: nodeUrl.absoluteString)
        _ = try await inboxClient().send(
            recipientActorId: peerId,
            recipientNestUrl: recipientNestUrl,
            payloadBytes: payload)
    }

    /// Where a knock sent from `actorId`'s PROFILE page must go — `sendKnock`'s
    /// `recipientNestUrl` (`profile.md` § Where logic lives → *Request contact
    /// routing*). Pure shared-Rust rule over the profile `body` the page's open
    /// already fetched (`profileGet`): the profile's own home nest when it is a
    /// different nest from this one, else `nil` (local delivery). No Swift
    /// URL-authority compare, no second fetch.
    ///
    /// (Named `knockRoute` rather than after the FFI free function it wraps, so the
    /// call inside cannot resolve back to this method.)
    public func knockRoute(actorId: String, profileBody: Data) -> String? {
        knockRecipientNestUrl(
            actorId: hex_to_data(actorId),
            profileBody: profileBody,
            ownNestUrl: nodeUrl.absoluteString)
    }

    /// `fauna.family.contact.request` — the SUPERVISED caller's in-app ask to contact
    /// a peer, offered only after a knock came back
    /// ``FfiError/GuardianApprovalRequired(msg:)`` (`family-safety.md` § Child-initiated
    /// contact requests). Pending in the guardian's queue; the caller's own pending
    /// asks ride `familyStatus().contactRequests`.
    public func requestContact(peerId: String) async throws {
        try await familyClient().contactRequest(peerActorId: hex_to_data(peerId))
    }

    /// `fauna.family.feed_source.request` — the SUPERVISED caller's in-app ask to add
    /// the external source their `feed_sources = "block"` policy just refused
    /// (`family-safety.md` § Feed-source approvals). `target` is the follow id / feed
    /// URI and is **empty for a link**; `label` is display-only, never authorizing.
    /// The operation is the shared enum's wire spelling, never a Swift literal — a
    /// grant matches `(bridge, operation, target)` exactly.
    public func requestFeedSource(
        bridgeId: String, operation: FfiFeedSourceOperation, target: String, label: String
    ) async throws {
        try await familyClient().feedSourceRequest(
            bridgeId: bridgeId,
            operation: feedSourceOperationWire(operation: operation),
            target: target,
            label: label)
    }

    /// A resolved compose / find-user recipient. Mirrors android
    /// `ResolveService.ResolvedRecipient` (priority #1/#3): `nodeUrl` is the nest
    /// that owns the recipient — the local nest for a raw actor id, or the
    /// SRV-resolved peer for a cross-nest handle.
    public struct ResolvedRecipient: Equatable {
        public let actorId: String
        public let nodeUrl: String
        public let handle: String?
        public let domain: String?

        public init(actorId: String, nodeUrl: String, handle: String? = nil, domain: String? = nil) {
            self.actorId = actorId
            self.nodeUrl = nodeUrl
            self.handle = handle
            self.domain = domain
        }
    }

    /// Resolve a typed compose / find-user recipient — the parse **and** both
    /// network lookups run in shared Rust over UniFFI (`classifyRecipient`, then
    /// the anonymous `fauna.nest.resolve` / `fauna.actor.by_handle` discovery
    /// kinds), so no Swift actor-id regex, `@`-split, or HTTP to the deleted
    /// `/api/v1/{resolve-node,actor/by-handle}` twins remains (priority #2/#4).
    /// Mirrors android `ResolveService.resolve` (`core/ResolveService.kt`).
    ///
    /// The home nest performs the SRV lookup, so the local nest and a cross-nest
    /// handle (`alice@other.example`) take one uniform path with no
    /// local-vs-remote branch — `resolveNest` returns the owning nest's URL and
    /// `resolveHandle` connects there anonymously.
    /// The **e2e session patch's** self-address — `"<handle>@<domain>"`, or the
    /// empty string when either half is unresolved (never the forbidden
    /// half-composed `"@<host>"` shape).
    ///
    /// Lives here, once, because BOTH shells need it and both previously carried
    /// their own copy — which is exactly how the same defect came to exist twice
    /// (`FaunaMacApp.swift` and `FaunaApp.swift`, priority #1).
    ///
    /// ⚠ **The domain is the nest's canonical identity domain, NEVER the nest URL
    /// host.** `conversations.md` § Self-address: live, never baked is explicit
    /// about this and names the exact consequence of getting it wrong: the
    /// FaunaMls data plane compares a peer's domain against ours to route
    /// same-nest vs. cross-nest, so a wrong domain "silently mis-routes
    /// cross-nest key-package fetch / Welcome delivery". A nest served at
    /// `nest.example.com` whose handles are `@example.com` diverges exactly that
    /// way — and under e2e, where the URL host is a bare `127.0.0.1`, EVERY
    /// same-nest peer read as foreign and every key-package fetch federated
    /// (`test_in_place_mls_add_through_the_ui`, red on macos, green on linux).
    ///
    /// The domain therefore comes from the same `fauna.actor.by_handle` door the
    /// recipient picker resolves PEERS through, with no `@domain` qualifier — the
    /// shared face documents that as reporting "the nest's canonical/identity
    /// domain". Both sides of the comparison are then answered by one nest call,
    /// so they cannot disagree by construction. Production does not use this path
    /// at all: it reads the account registry's own `material.domain`, which has
    /// been correct since 2026-08-03.
    public func e2eSelfAddress() async -> String {
        let handle = (try? await getAccount())?.handle ?? ""
        guard !handle.isEmpty else { return "" }
        // A handle that already carries a qualifier keeps it — pass the local
        // part and let the nest echo the domain it was asked about, the same
        // multi-domain rule `resolveRecipient` follows.
        let local = handle.contains("@") ? String(handle.split(separator: "@")[0]) : handle
        let typed = handle.contains("@") ? String(handle.split(separator: "@")[1]) : nil
        let resolved = try? await resolveHandle(
            nodeUrl: nodeUrl.absoluteString, handle: local, domain: typed)
        let domain = resolved?.count == 3 ? resolved![2] : ""
        return domain.isEmpty ? "" : "\(local)@\(domain)"
    }

    public func resolveRecipient(_ input: String) async throws -> ResolvedRecipient {
        // [kind, actorId, user, domain]; kind ∈ {actor_id, handle, invalid}.
        let parts = classifyRecipient(input: input.trimmingCharacters(in: .whitespaces))
        switch parts[0] {
        case "actor_id":
            return ResolvedRecipient(actorId: parts[1], nodeUrl: nodeUrl.absoluteString)
        case "handle":
            let targetNodeUrl = try await resolveNest(homeUrl: nodeUrl.absoluteString, domain: parts[3])
            // [actorId, handle, domain] — pass the typed @domain qualifier so a
            // multi-domain nest echoes the domain the user actually typed
            // (`mail-multidomain.md` § Resolution). Bare handles resolve against
            // the nest's primary domain.
            let resolved = try await resolveHandle(nodeUrl: targetNodeUrl, handle: parts[2],
                                                   domain: parts[3])
            return ResolvedRecipient(
                actorId: resolved[0], nodeUrl: targetNodeUrl, handle: resolved[1], domain: resolved[2])
        default:
            throw APIError.ffiError("Enter a handle (alice@fauna.social) or 64-char actor ID")
        }
    }

    public func getInboxMode(actorId: String) async throws -> String {
        try await contactsClient().inboxModeGet()
    }

    public func setInboxMode(actorId: String, mode: String) async throws {
        try await contactsClient().inboxModeSet(mode: mode)
    }

    // MARK: - Calendars & Events (encrypted CalDAV over fauna.bridges.* WS-RPC)
    //
    // The Events page is a CalDAV calendar surface over the encrypted
    // `bridge_caldav_*` store (events.md Decision B, ratified 2026-06-01) — the same
    // store the mail-bridge MDA serves to Apple Calendar. These methods drive the
    // `FfiCaldavClient` UniFFI seam (`FfiNestClient.caldav()`), mirroring the android
    // lift; the legacy plaintext `/api/{calendars,events}` HTTP twins
    // are retired. The CalDAV seam is msek-gated: reads degrade to empty when
    // mail/CalDAV is off; writes throw "Calendar requires mail to be enabled".
    //
    // The legacy cross-nest event-invitation inbox (the `fauna.events.*`
    // remote-rsvp + inbox-invitations kinds) was retired wholesale 2026-06-14
    // (events.md § Implementation status today) — vestigial atop the now-empty
    // legacy event store; the unified cross-nest mechanism is iMIP scheduling.

    public func listCalendars() async throws -> [FaunaCalendar] {
        try await caldavClient().listCalendars().map {
            FaunaCalendar(id: $0.id, name: $0.name, color: $0.color.isEmpty ? nil : $0.color)
        }
    }

    public func createCalendar(name: String) async throws {
        try await caldavClient().createCalendar(name: name)
    }

    public func queryEvents(calendarId: String) async throws -> [EventSummary] {
        try await caldavClient().queryEvents(calendarIdHex: calendarId).map(Self.eventSummary)
    }

    /// The single `FfiCaldavClient` instance the Events-page backstop poll
    /// reuses across ticks (`docs/goal/ui/events.md` § Implementation status
    /// today — the delta-sync backstop). `caldavClient()` mints a FRESH
    /// instance every call (cheap, stateless — `nest_client.rs::caldav()`'s
    /// own doc comment), which is correct for `queryEvents`/create/delete but
    /// would silently defeat `queryEventsSeeded` below: its cost saving lives
    /// entirely in the per-instance `CalendarSyncTokens` the Rust side holds,
    /// so a fresh instance per poll tick would report `held=None` forever and
    /// never skip a read. Cached per `APIClient` (i.e. per session, not per
    /// page-visit — mirrors `cachedAtprotoMachine`'s exact reasoning: a fresh
    /// `EventsVM` on every Settings→Events re-entry must not reset the
    /// baseline). Dropped by `adoptActor` on a real actor switch — **not** by
    /// a fresh `APIClient` per account switch, this comment's earlier
    /// premise — an in-place re-point (the SwiftUI ordering
    /// `adoptActor`'s own doc describes) hands a *previous* actor's
    /// `APIClient`, cache included, a new actor's secret, so "no manual
    /// teardown is owed" was false.
    private var cachedSeededCaldavClient: FfiCaldavClient?

    private func seededCaldavClient() async throws -> FfiCaldavClient {
        if let cachedSeededCaldavClient { return cachedSeededCaldavClient }
        // `sameActorSince()` — the same seam `conversationsSession` uses:
        // the only await here is `caldavClient()`'s connect, and a switch
        // landing during it is not stopped by `adoptActor` nil-ing the
        // handle — this line would still write the outgoing actor's client
        // back in immediately after.
        let stillThisActor = sameActorSince()
        let client = try await caldavClient()
        guard stillThisActor() else {
            throw APIError.ffiError("caldav client: actor changed while connecting")
        }
        cachedSeededCaldavClient = client
        return client
    }

    /// `queryEvents`'s COST-saving twin for the Events-page backstop poll —
    /// consults the shared `fauna_client_caldav::delta_sync` seam via
    /// `FfiCaldavClient::query_events_seeded` before paying for a full
    /// unseal. Returns `nil` when the calendar is unchanged since the last
    /// call through `seededCaldavClient()` — the caller's already-rendered
    /// list is still current and MUST NOT be touched (mirrors linux
    /// `fetch_events_inner`'s early return). Returns the full event list
    /// otherwise, identical to `queryEvents` in every other respect
    /// (including the same "off"/"not found" degradations to `[]`, which
    /// `query_events_seeded` returns as `Some([])`, never `nil` — an empty
    /// calendar and an unreachable one both read as "here is the truth: none",
    /// only a genuinely UNCHANGED calendar reads as "don't touch the model").
    public func queryEventsSeeded(calendarId: String) async throws -> [EventSummary]? {
        guard let events = try await seededCaldavClient().queryEventsSeeded(calendarIdHex: calendarId) else {
            return nil
        }
        return events.map(Self.eventSummary)
    }

    public func queryMyEvents(filter: String) async throws -> [EventSummary] {
        // The encrypted store returns the actor's invited (not-yet-organized) events;
        // the legacy `filter` flag has no analogue and is ignored.
        try await caldavClient().queryInvitedEvents().map(Self.eventSummary)
    }

    public func createEvent(_ request: CreateEventRequest) async throws {
        try await caldavClient().createEvent(
            calendarIdHex: request.calendarId,
            summary: request.summary,
            dtstart: request.dtstart,
            dtend: request.dtend,
            location: request.location ?? "",
            description: request.description ?? "")
    }

    public func getEvent(id: String) async throws -> EventDetail {
        guard let ev = try await caldavClient().getEvent(uidHashHex: id) else {
            throw APIError.ffiError("Event not found")
        }
        return Self.eventDetail(ev)
    }

    public func deleteEvent(id: String) async throws {
        try await caldavClient().deleteEvent(uidHashHex: id)
    }

    public func inviteToEvent(eventId: String, email: String) async throws {
        // Email-based attendee invite (the universal mechanism, events.md
        // § User actions): adds a `mailto:` ATTENDEE and fans out the iMIP REQUEST.
        // Cross-nest mailbox-less-Fauna delivery is fully automatic — resolved
        // from the CAL-ADDRESS alone via anon by_handle discovery, no manual URL.
        try await caldavClient().inviteAttendee(uidHashHex: eventId, email: email)
    }

    public func rsvpEvent(eventId: String, response: RsvpResponse) async throws {
        try await caldavClient().rsvpEvent(uidHashHex: eventId, response: response)
    }

    public func setReminder(eventId: String, offset: String) async throws {
        try await caldavClient().setReminder(uidHashHex: eventId, offset: offset)
    }

    public func removeReminder(eventId: String) async throws {
        // The seam treats an empty offset as "clear the reminder".
        try await caldavClient().setReminder(uidHashHex: eventId, offset: "")
    }

    public func importCalendar(calendarId: String, icsText: String) async throws -> CalendarImportResult {
        // There is no skip-vs-overwrite choice to make: the seam upserts by uid_hash.
        let r = try await caldavClient().importCalendarIcs(calendarIdHex: calendarId, icsText: icsText)
        return CalendarImportResult(
            imported: Int(r.imported), skipped: Int(r.skipped),
            updated: nil, total: Int(r.imported) + Int(r.skipped), errors: nil)
    }

    public func exportCalendar(calendarId: String) async throws -> String {
        try await caldavClient().exportCalendarIcs(calendarIdHex: calendarId)
    }

    // FFI record → per-app UI type mapping (mirrors android's `toSummary`/`toDetail`).
    private static func eventSummary(_ e: FfiCalEvent) -> EventSummary {
        EventSummary(id: e.id, uid: e.uid, summary: e.summary,
                     dtstart: e.dtstart, dtend: e.dtend ?? e.dtstart,
                     location: e.location, calendarId: e.calendarId)
    }

    private static func eventDetail(_ e: FfiCalEvent) -> EventDetail {
        EventDetail(id: e.id, uid: e.uid, calendarId: e.calendarId,
                    summary: e.summary, dtstart: e.dtstart, dtend: e.dtend ?? e.dtstart,
                    description: e.description, location: e.location,
                    organizer: e.organizer, organizedByMe: e.organizedByMe,
                    reminder: e.reminder,
                    attendees: e.attendees.map { Attendee(email: $0.email, name: $0.name, rsvp: $0.rsvp) })
    }

    // MARK: - Posts
    //
    // The Feed PAGE is on the shared `fauna_feed::FeedManager` (the `FfiFeedManager`
    // façade — see `feedManager(secret:)` below); it owns the feed/post-list/search/
    // feed-rule/compose surface, so the old HTTP-era `fauna.feed.*` `APIClient`
    // wrappers (listFeeds/getFeed/queryFeedPosts/queryLocalFeedPosts/createFeed/
    // getPostBytes) + their `FeedFFIMapping` types were removed (the lift, priority
    // #1/#2). What remains here is the thin posts glue the interaction-bar +
    // TestAgent still drive directly over `fauna.posts.*` (NOT manager actions).

    /// Publish a signed post. `payload` is the embed-as-bytes wire built by the
    /// shared post codec FFI (`build_post*`); only the final submission rides
    /// `fauna.posts.create`. Used by the macOS TestAgent's `compose.post` helper.
    public func createPost(payload: Data) async throws {
        _ = try await postsClient().postsCreate(body: payload)
    }

    /// Like / repost / reply / quote a post (`fauna.posts.interact`). `body`
    /// carries the reply/quote text (ignored for like/repost). This is client
    /// glue, NOT a `FeedManager` action — the manager owns the post-list snapshot;
    /// the interaction-bar stays a thin posts-client call, the same pattern the
    /// Linux/Android/web feed clients use (`feed.md` § interaction-bar).
    public func interactWithPost(postId: String, action: String, body: String? = nil) async throws {
        _ = try await postsClient().postsInteract(postId: postId, action: action, body: body)
    }

    // MARK: - Notifications

    public func getNotifications(actorId: String, limit: Int = 50) async throws -> (items: [NotificationItem], unread: Int) {
        // The deleted HTTP twin returned the page + `unread_count` in one
        // response; `fauna.notifications.*` splits them into `list` + `count`.
        let client = try await notificationsClient()
        let reply = try await client.list(cursor: nil, limit: Int64(limit))
        let unread = try await client.count()
        return (items: reply.notifications.map(NotificationItem.init(ffi:)), unread: Int(unread))
    }

    /// `fauna.notifications.count` alone — the app-global unread count the shells
    /// refresh off every notification push without fetching the page itself
    /// (`notifications.md` § Architectural rules, rule 4).
    public func unreadNotificationCount() async throws -> Int {
        Int(try await notificationsClient().count())
    }

    public func markNotificationsRead(actorId: String) async throws {
        // `up_to = nil` ⇒ mark everything up to now (the twin's behavior).
        _ = try await notificationsClient().markRead(upTo: nil)
    }

    // MARK: - Bridge Feeds
    //
    // `fauna.bridges.feeds.*` WS-RPC kinds. Feed row ids are `i64` on the wire;
    // the Swift surface keeps them as `String` (existing view-model contract).

    public func subscribeBridgeFeed(bridge: String, feedUri: String,
                             name: String) async throws {
        _ = try await bridgesClient().feedsCreate(bridge: bridge, feedUri: feedUri, name: name)
    }

    public func listBridgeFeeds() async throws -> [BridgeFeedSubscription] {
        try await bridgesClient().feedsList().map(BridgeFeedSubscription.init(ffi:))
    }

    public func unsubscribeBridgeFeed(id: String) async throws {
        guard let feedId = Int64(id) else {
            throw APIError.ffiError("Invalid bridge-feed id: \(id)")
        }
        try await bridgesClient().feedsDelete(id: feedId)
    }

    // MARK: - Email Filters
    //
    // `fauna.email.filters.*` WS-RPC kinds over `FfiEmailClient`. Filters ride
    // the typed `Ffi*` shapes end-to-end (the old `[[String:Any]]`/`Any` HTTP
    // shapes had drifted from the protocol) — composition from the dialog is in
    // PrivacySettingsVM, options/labels in SettingsTypes.swift.

    public func listEmailFilters() async throws -> [FfiEmailFilter] {
        try await emailClient().filtersList()
    }

    @discardableResult
    public func createEmailFilter(name: String, rules: [FfiEmailFilterRule],
                           combination: String, action: FfiEmailFilterAction,
                           priority: Int) async throws -> Int64 {
        try await emailClient().filtersCreate(name: name, rules: rules,
                                              combination: combination, action: action,
                                              priority: Int32(priority))
    }

    /// A fresh fetch (not the cached list row) — the edit form pre-populates
    /// from this, never from `emailFilters`, so a concurrent change elsewhere
    /// can't stage an edit against stale data.
    public func getEmailFilter(id: Int64) async throws -> FfiEmailFilter {
        try await emailClient().filtersGet(id: id)
    }

    public func updateEmailFilter(id: Int64, name: String, rules: [FfiEmailFilterRule],
                           combination: String, action: FfiEmailFilterAction,
                           priority: Int) async throws {
        try await emailClient().filtersUpdate(id: id, name: name, rules: rules,
                                              combination: combination, action: action,
                                              priority: Int32(priority))
    }

    public func deleteEmailFilter(id: Int64) async throws {
        try await emailClient().filtersDelete(id: id)
    }

    // MARK: - Spam Preferences
    //
    // `fauna.spam.{get,set}_preferences` WS-RPC kinds over `FfiSpamClient` (off
    // the deleted-when-all-migrated `GET|PUT /api/v1/spam/preferences` HTTP twin).
    // The wire carries the thresholds as per-mille `u16`; the Swift
    // `SpamPreferences` presents them as a 0.0–1.0 slider, so this seam converts
    // at the boundary via `SpamOptions` (as linux's `privacy.rs` + android's
    // `PrivacySettingsVM` do). There is intentionally no spam-train method: the
    // old `POST /api/v1/spam/train` free-text trainer had no WS-RPC twin and no
    // sibling client offers a free-text trainer (the shared `train-correction`
    // surface acts on stored moderation-queue items, not arbitrary text).

    public func getSpamPreferences() async throws -> SpamPreferences {
        let prefs = try await spamClient().getPreferences()
        return SpamPreferences(
            spamThreshold: SpamOptions.threshold(prefs.spamThreshold),
            phishingThreshold: SpamOptions.threshold(prefs.phishingThreshold)
        )
    }

    /// The viewer's own spam/phishing thresholds in the wire's **per-mille
    /// `u16`** — the shape the shared content-policy render engine consumes
    /// (`ContentPolicyStore`; family-safety.md § Content policy). The sibling
    /// `getSpamPreferences()` exists to drive a 0.0–1.0 slider, so it converts;
    /// this seam deliberately does not, matching linux/android, which read the
    /// per-mille thresholds straight through to `render_verdict_composed`.
    public func spamThresholdsPerMille() async throws -> (spam: UInt16, phishing: UInt16) {
        let prefs = try await spamClient().getPreferences()
        return (spam: prefs.spamThreshold, phishing: prefs.phishingThreshold)
    }

    public func updateSpamPreferences(_ prefs: SpamPreferences) async throws {
        _ = try await spamClient().setPreferences(
            spamThreshold: SpamOptions.perMille(prefs.spamThreshold),
            phishingThreshold: SpamOptions.perMille(prefs.phishingThreshold)
        )
    }

    // MARK: - Account Management
    //
    // The account / quota / am-i-admin / profile-handle / upgrade HTTP twins
    // were deleted nest-side (T3); these ride
    // WS-RPC via the shared `FfiAccountClient`. `GET /api/v1/export` stays HTTP
    // — a streaming `application/zip` download with no JSON control plane to
    // split (api-layers.md § HTTP residue).

    /// `fauna.profile.handle.change` — queue a handle change (delayed +
    /// cancellable; the reply carries the pending-action id, surfaced here as
    /// the new handle for the settings UI).
    public func changeHandle(newHandle: String) async throws -> HandleChangeResponse {
        let reply = try await accountClient().changeHandle(handle: newHandle)
        return HandleChangeResponse(handle: reply.newHandle)
    }

    /// `fauna.account.delete` — queue account deletion (cancellation window).
    public func deleteAccount() async throws {
        _ = try await accountClient().delete()
    }

    /// `fauna.pending_actions.list` — this actor's queued destructive
    /// operations, filtered to still-`pending` rows (an executed/cancelled/
    /// expired action has no cancel window left) — mirrors tui's/linux's/
    /// web's/android's own `list_pending_actions` helper. The wire reply
    /// carries ALL statuses, unfiltered.
    public func pendingActionsList() async throws -> [FfiPendingActionSummary] {
        try await accountClient().pendingActionsList().filter { $0.status == "pending" }
    }

    /// `fauna.pending_actions.cancel` — cancel a scheduled action before it
    /// executes (one click, no confirm — cancelling is the safe direction).
    public func pendingActionCancel(id: Int64) async throws {
        _ = try await accountClient().pendingActionCancel(id: id)
    }

    /// `fauna.account.get` — full account state (handle, tier, quota, eviction,
    /// node policy). No HTTP twin ever existed in this client.
    public func getAccount() async throws -> FfiAccountGetReply {
        try await accountClient().get()
    }

    /// `fauna.account.am_i_admin` — whether the calling actor is a nest admin
    /// (so the client can gate admin UI without a separate round-trip).
    public func amIAdmin() async throws -> Bool {
        try await accountClient().amIAdmin()
    }

    /// `fauna.account.upgrade` — move to a higher tier, consuming an invite
    /// code that authorizes it.
    public func upgrade(tier: String, inviteCode: String) async throws -> FfiUpgradeReply {
        try await accountClient().upgrade(tier: tier, inviteCode: inviteCode)
    }

    /// `GET /api/v1/export` — **stays HTTP** (streaming zip download, residue).
    public func exportData() async throws -> Data {
        // `include_blobs=true` is what makes this the whole archive rather than
        // an index of it — the nest defaults the flag off. Not a user choice:
        // account-data-plane.md § Nest-side requirements item 1, Payload stores
        // decision (5).
        try await get("api/v1/export?include_blobs=true")
    }

    // MARK: - MLS Key Packages

    /// `fauna.conversations.keypackage.count` — non-destructive count of the
    /// remaining one-time key packages for `actorId`, over the shared
    /// `FfiConversationsClient`. The `GET /api/v1/keypackage/{actor}/count`
    /// HTTP twin was deleted nest-side (T8).
    public func getKeyPackageCount(actorId: String) async throws -> Int {
        Int(try await conversationsClient().keypackageCount(actorId: actorId).count)
    }

    /// `fauna.conversations.keypackage.upload` — publish one-time MLS key
    /// packages for the connection's actor (`lastResort: false` for the
    /// consumable Encryption-settings top-up). Raw key-package bytes come from
    /// the local MLS engine and ride the WS-RPC wire natively (no hex/JSON
    /// wrapper). The `POST /api/v1/keypackage/{actor}` HTTP twin was deleted.
    public func publishKeyPackages(actorId: String, packages: [Data]) async throws {
        _ = try await conversationsClient().keypackageUpload(packages: packages, lastResort: false)
    }

    // MARK: - Nostr

    // Nostr control-plane rides the unified `fauna.bridges.*` kinds
    // (`bridge_id:"nostr"`, `NostrProvider`) over the shared bridges WS-RPC
    // client path below; the `/api/v1/nostr/{link,status,settings,follows}`
    // HTTP twins were deleted nest-side (`docs/goal/ui/nostr.md` § WS-RPC
    // migration contract). These methods adapt the generic bridge wire to the
    // Nostr page view types (the standalone page UI is untouched). The native
    // content routes were also migrated to WS-RPC
    // (`nostr.{zaps.total,badges.list,events.publish_signed}`, 2026-07-22);
    // only the relay-WS residue (`/nostr`, NIP-01/NIP-11) stays HTTP — its far
    // end is a non-Fauna Nostr client.

    private static let nostrBridgeId = "nostr"

    public func getNostrStatus() async throws -> NostrStatus {
        let bridges = try await listBridges()
        guard let info = bridges.first(where: { $0.id == Self.nostrBridgeId }) else {
            return NostrStatus(registered: false, linked: false, available: false)
        }
        return NostrStatus(bridge: info)
    }

    public func linkNostrGenerate() async throws -> NostrLinkResponse {
        let reply = try await linkBridge(bridgeId: Self.nostrBridgeId, mode: "generate", fields: [:])
        return NostrLinkResponse(bridge: reply, mode: "generate")
    }

    public func linkNostrImport(nsec: String) async throws -> NostrLinkResponse {
        let reply = try await linkBridge(
            bridgeId: Self.nostrBridgeId, mode: "import", fields: ["nsec": nsec])
        return NostrLinkResponse(bridge: reply, mode: "import")
    }

    public func linkNostrRemote(bunkerUrl: String) async throws -> NostrLinkResponse {
        let reply = try await linkBridge(
            bridgeId: Self.nostrBridgeId, mode: "remote", fields: ["bunker_url": bunkerUrl])
        return NostrLinkResponse(bridge: reply, mode: "remote")
    }

    public func unlinkNostr() async throws {
        try await unlinkBridge(bridgeId: Self.nostrBridgeId)
    }

    /// The succession-aftermath npub confirm banner's owed-check (`nostr.md`
    /// § Key succession and rotation, leg 3) — a direct `libs/fauna-ffi/src/
    /// nostr_npub_confirm.rs` UniFFI door, not the generic bridge control
    /// plane above; same `nest`+`ownerSecret` idiom as
    /// `downloadSnapshotFile`/`removeBackupDestination`.
    public func npubConfirmationOwed() async throws -> Bool {
        guard let secret else { throw APIError.ffiError("No secret for npub confirmation") }
        return try await FaunaFFISwift.npubConfirmationOwed(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret))
    }

    /// "Yes, that's my npub" — records the confirmation; the caller's own
    /// clock, same idiom as every other UniFFI call taking `now`.
    public func confirmNpub() async throws {
        guard let secret else { throw APIError.ffiError("No secret for npub confirmation") }
        try await FaunaFFISwift.confirmNpub(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret),
            now: Int64(Date().timeIntervalSince1970))
    }

    public func updateNostrSettings(_ settings: NostrSettings) async throws {
        try await updateBridgeSettings(bridgeId: Self.nostrBridgeId, settings: settings.asBridgeSettings)
    }

    public func listNostrFollows() async throws -> [NostrFollow] {
        try await listBridgeFollows(bridgeId: Self.nostrBridgeId).map(NostrFollow.init(bridge:))
    }

    // `relayHints` is retained for caller-signature stability but unused: apple's
    // generic bridge-follow path does not carry `extra` (the sole caller passes nil).
    public func addNostrFollow(pubkey: String, petname: String?,
                        relayHints: [String]?) async throws {
        try await addBridgeFollow(bridgeId: Self.nostrBridgeId, followId: pubkey, petname: petname)
    }

    public func removeNostrFollow(npub: String) async throws {
        try await removeBridgeFollow(bridgeId: Self.nostrBridgeId, followId: npub)
    }

    // MARK: - Nostr Connect (bunker)

    // The *Connected apps* section (`nostr-bunker-*`, nostr.md § The nest as
    // the user's NIP-46 signer): a roster with mint/revoke verbs, deliberately
    // NOT `fauna.bridges.set_settings` — so unlike the rest of this page it
    // rides its own typed-call client (`FfiNestClient::nostrBunker()`) over
    // the shared WS-RPC connection, the same wrapper idiom as
    // `searchClient()`/`foldersClient()` above. Mirrors linux/tui (direct
    // Rust, no FFI hop) and android (the first UniFFI-consuming client for
    // this control plane).
    private func nostrBunkerClient() async throws -> FfiNostrBunkerClient {
        try await ensureNestConnected().nostrBunker()
    }

    /// `fauna.nostr.bunker.create_invite` — mint a pending connection; the
    /// reply is the single one-time reveal of the connect string.
    public func nostrBunkerCreateInvite() async throws -> FfiCreateBunkerInviteReply {
        try await nostrBunkerClient().createInvite()
    }

    /// `fauna.nostr.bunker.list` — the caller's connection roster (pending +
    /// active rows).
    public func nostrBunkerList() async throws -> [FfiBunkerAppEntry] {
        try await nostrBunkerClient().list()
    }

    /// `fauna.nostr.bunker.revoke` — immediate disconnect of one connection.
    @discardableResult
    public func nostrBunkerRevoke(connectionId: Int64) async throws -> Bool {
        try await nostrBunkerClient().revoke(connectionId: connectionId)
    }

    // MARK: - Nostr zap signers (NIP-57 trust root)

    // The *Zap signers* section (`nostr-zap-signer-*`, nostr.md § Layout &
    // flow item 7 — the NIP-57 trust root; monetization.md § Zap receipts —
    // the trust model). Same typed-call idiom as `nostrBunkerClient()` above
    // (`FfiNestClient::nostrZapSigners()` over the shared WS-RPC connection).
    // Mirrors linux/tui (direct Rust) and android/web (the other UniFFI/wasm
    // consumers of this control plane).
    //
    // EXCISED BY `FAUNA_EXCISE_PAYMENTS`, because `zaps` is a SUBSET MEMBER of
    // `payments` (ui.yaml's `gated_features:` block; dynamic-features.md
    // § Charter members) and the apple family has exactly one condition for the
    // whole plane (§ Platform-family surface excision — one condition per
    // registry member is what the toolchain supports, and the store-safe FFI is
    // `--no-default-features --features store-safe`, so it drops `zaps` with
    // `payments`). A store-safe `FaunaFFI.xcframework` therefore exports no
    // `FfiNostrZapSignerClient` and no `FfiZapSignerEntry`, and every line below
    // is a compile error in that flavor. The RENDER half is the same condition
    // in NostrSettingsView / NostrVM.
    #if !FAUNA_EXCISE_PAYMENTS

    private func nostrZapSignersClient() async throws -> FfiNostrZapSignerClient {
        try await ensureNestConnected().nostrZapSigners()
    }

    /// `fauna.nostr.zap_signers.list` — the caller's designated signers,
    /// newest first. Empty means this payee believes no zap receipt at all
    /// (the ratified default, not an error).
    public func nostrZapSignersList() async throws -> [FfiZapSignerEntry] {
        try await nostrZapSignersClient().list()
    }

    /// `fauna.nostr.zap_signers.add` — designate a signer. Returns the STORED
    /// row (64-hex, lowercased nest-side) — render that, never the typed
    /// input, since only the stored form ever matches a real receipt.
    public func nostrZapSignersAdd(signerPubkey: String, label: String) async throws -> FfiZapSignerEntry {
        try await nostrZapSignersClient().add(signerPubkey: signerPubkey, label: label)
    }

    /// `fauna.nostr.zap_signers.remove` — stop trusting a signer. Never
    /// gated: removal is de-escalation, so a tier that can only tighten must
    /// not trap a user in a roster they cannot undo.
    @discardableResult
    public func nostrZapSignersRemove(signerPubkey: String) async throws -> Bool {
        try await nostrZapSignersClient().remove(signerPubkey: signerPubkey)
    }

    #endif

    // MARK: - Folders

    // The `/api/v1/file-sets/*` HTTP twins were deleted on nest; these now route
    // over WS-RPC via `foldersClient()` (`fauna.folders.*`) — the same wrapper
    // idiom snapshots/stats/sync use, so the public signatures stay stable and
    // the Backups / Devices / MenuBar / SyncSettings callers are untouched. The
    // mirror→Swift-model mapping (epoch→display string, retention-policy JSON
    // codec, `Int64`→`Int`) is at the edge in the helpers below.

    public func listFolders() async throws -> [FolderResponse] {
        let sets = try await foldersClient().list()
        return sets.map(Self.folderResponse(from:))
    }

    // `retention_policy` is set only by the shared `FolderWizardMachine.submit()`
    // (the Photo Library preset), which calls the nest directly with the canonical
    // `fauna_folders_machine::RetentionPolicy` shape — never through this Swift
    // wrapper (`backup-restore.md` § 8 RULING: the wire field's only canonical
    // serialization is the 2-field `{max_snapshots, max_age_days}` JSON). Don't
    // reintroduce a `retention` param here without a UI that actually collects
    // that shape — see `decodeRetention` below for the read-side codec.
    @discardableResult
    public func createFolder(name: String) async throws -> FolderResponse {
        let created = try await foldersClient().create(
            name: name,
            retentionPolicy: nil)
        return Self.folderResponse(from: created)
    }

    public func updateFolderPaths(name: String, includePaths: [String], excludePaths: [String]) async throws {
        try await foldersClient().update(
            name: name, retentionPolicy: nil,
            includePaths: includePaths, excludePaths: excludePaths)
    }

    public func deleteFolder(name: String) async throws {
        try await foldersClient().delete(name: name)
    }

    public func listFolderDevices(name: String) async throws -> [DeviceInfo] {
        let devices = try await foldersClient().devices(name: name)
        return devices.map(Self.deviceInfo(from:))
    }

    // MARK: - Push Notifications
    //
    // This install's push registration under the connected actor — the shared
    // `fauna_client_push::registration` machine over `FfiPushRegistration`
    // (`common.md` § Push Notifications → Registration). The connection actor
    // is the implicit subscriber. The OS-side half (APNs token, keychain key
    // material, the permission prompt) stays in `PushManager`; the intent bit,
    // the which-actor record, the leave-drops and the presence announce are the
    // shared machine's. `deviceId` is the install's derived device id for this
    // actor (`FaunaAccounts.deviceId(forActorId:)`): the one value the rows are
    // keyed under and the connection announces.
    public func pushRegistration(
        deviceId: String, intentPath: String
    ) async throws -> any PushRegistering {
        guard let actorId = boundActorIdHex else {
            throw APIError.ffiError("No signed-in actor for a push registration")
        }
        return try await ensureNestConnected().pushRegistration(
            intentPath: intentPath, actorId: actorId, deviceId: deviceId)
    }

    // MARK: - Token management

    private var secret: String?

    /// Prime the actor secret synchronously, ahead of the async `authenticate()`
    /// round-trip that normally sets it. `ensureNestConnected()` — the WS-RPC
    /// entry point every read rides — only needs `self.secret` to build the
    /// `FfiNestClient`; it does NOT need the minted bearer (`FfiNestClient.connect()`
    /// self-authenticates from the secret). Without this, a surface that fires the
    /// instant the client is exposed — notably the `am-i-admin` nav gate's
    /// `.task(id: client != nil)`, which races the detached `authenticate()` Task
    /// (set_state path) AND the silent-challenge launch path that never sets
    /// `secret` at all — reaches `guard let secret` while it is still nil, throws,
    /// and the fail-closed one-shot latches `isAdmin = false` forever (a fresh
    /// in-process / launch session has no reconnect to re-fire it). Idempotent —
    /// `authenticate()` re-sets it — and on-pattern with the existing
    /// `if secret == nil { secret = … }` priming in `conversationsSession` /
    /// `feedManager`. Called from `FaunaClient.init`, which already holds the seed.
    public func primeSecret(_ secret: String) {
        if self.secret == nil { self.secret = secret }
    }

    /// Which actor this client's seat is bound to — derived from the stored
    /// secret, **never the secret itself**.
    ///
    /// Two jobs, and the second is why this is not merely diagnostic. As a
    /// *witness* it separates the two ways a call on a rebuilt session fails
    /// identically — the caller holds a *previous* `APIClient` whose actor was
    /// revoked, versus it holds the right one whose socket merely dropped; both
    /// surface as `fauna.protocol.disconnected` at every call site, and no other
    /// line names the actor a seat signs as (`adoptActor`'s warn fires only on a
    /// re-point that actually happens). As a *key* it is what lets a view tell
    /// one live client from another across an account switch — see
    /// `RecoveryKitSection.hydrateKey`, which read only presence until this
    /// existed and so could not see the switch at all.
    ///
    /// `nil` means no secret has reached this client yet — a third distinct
    /// state that must not read as either of the other two. In practice a
    /// `FaunaClient`-owned seat answers from construction (`primeSecret`), so
    /// `nil` here means a genuinely unprimed client rather than a race.
    ///
    /// Memoized against the secret it was derived from: this is read from view
    /// bodies, and the derivation is an FFI hop plus a scalar multiplication.
    /// The cache self-invalidates because it is keyed by the secret's own value,
    /// so `adoptActor` needs no hook here.
    public var boundActorIdHex: String? {
        guard let secret else { return nil }
        if let cached = boundActorCache, cached.secret == secret { return cached.actorIdHex }
        guard let derived = try? actor_id_from_secret(secret) else { return nil }
        boundActorCache = (secret: secret, actorIdHex: derived)
        return derived
    }

    private var boundActorCache: (secret: String, actorIdHex: String)?

    private func ensureAuthenticated() async throws {
        if let tokenExpiresAt, tokenExpiresAt > Date() { return }
        guard let secret else { throw APIError.ffiError("No secret for token refresh") }
        try await authenticate(secret: secret)
    }

    // MARK: - WS-RPC connection (shared Rust NestClient over UniFFI)

    /// The per-actor WS-RPC connection (shared `FfiNestClient`), lazily
    /// opened on first use and cached for the session. The account / quota /
    /// handle kinds ride this instead of the deleted HTTP twins. `FfiNestClient`
    /// does the https→wss + `/api/v1/ws` conversion internally and owns its own
    /// reconnect supervisor + silent-challenge auth, independent of the HTTP
    /// bearer loop above. Mirrors Android's `ensureNestConnected`.
    private var nestClient: FfiNestClient?
    /// The actor secret `nestClient` authenticated as. A cached connection may
    /// only ever be served back for that same actor — see `adoptActor`.
    private var nestClientSecret: String?

    /// Re-point this client at `secretHex` when it is a DIFFERENT actor than the
    /// one already cached, dropping the WS-RPC connection so the next
    /// `ensureNestConnected` re-authenticates.
    ///
    /// **Why this exists.** The nest reads a request's *subscriber/caller
    /// identity from the authenticated connection*, while a stateful manager
    /// signs and derives from the keypair it was constructed with. Those are two
    /// different sources of "who am I", and nothing but this method keeps them
    /// in step. Before it, `ensureNestConnected` returned the cached socket
    /// unconditionally and `feedManager(secret:)`'s `if secret == nil` priming
    /// could not correct a non-nil stale one — so handing a *previous* actor's
    /// `APIClient` a new actor's secret (the SwiftUI ordering where
    /// `session.secretHex` reaches a `.task(id:)` before the replacement
    /// `FaunaClient` propagates through the environment) built a manager with
    /// actor B's keypair riding actor A's socket. Every read still looked
    /// right — it was B's data, fetched with B's key — but every *write* was
    /// attributed to A. Measured 2026-08-25: a self-serve post purchase clicked
    /// by the buyer enrolled the SELLER on their own unlock tier, leaving the
    /// buyer `not_subscribed` and unable to unseal what they had bought.
    ///
    /// Silent by construction: same actor → no-op, so the ordinary re-login and
    /// reconnect paths are untouched.
    ///
    /// **Every other actor-bound cache this client holds is dropped here
    /// too** — one list, so adding a new cache means adding one line here and
    /// nowhere else (mirrors `ActorScope.resetSharedState`'s "one canonical
    /// drop", `account-scoping.md:110-125`). Before this,
    /// `cachedSeededCaldavClient`, `cachedAtprotoMachine`,
    /// `conversationsSessionCache`/`Task`, and the HTTP bearer
    /// (`token`/`tokenExpiresAt`/`currentToken`) all survived a re-point, so a
    /// re-pointed client kept serving the outgoing actor's calendar, Bluesky
    /// credentials, MLS session, and HTTP bearer. Gated on `secret`, not
    /// `nestClientSecret` above: that one goes nil on every drop (same actor
    /// included, while `ensureNestConnected` has not yet reconnected), so two
    /// calls landing back-to-back for the SAME incoming actor — e.g.
    /// `feedManager` and `conversationsSession` both re-pointing before
    /// either has reconnected — bump `actorGeneration` and clear these once,
    /// not once each. A cache whose builder writes it back AFTER an await
    /// (`seededCaldavClient`, `atprotoSettingsMachine`, `conversationsSession`)
    /// additionally checks `sameActorSince()` at the write, because nil-ing
    /// the handle here cannot stop a build already past its await from
    /// writing the outgoing actor's result back in.
    ///
    /// **The dropped WS-RPC client is disconnected, not just nil-ed.** A cached
    /// `nestClient` always has a running reconnect supervisor
    /// (`ensureNestConnected` caches only after `connect()` succeeds), and
    /// dropping the reference does not stop it — no `Drop` impl, and the
    /// supervisor task holds its own `Arc`s (`libs/fauna-client/src/client.rs`,
    /// the 2026-08-22 leaked-socket incident). Nil-ing alone left the outgoing
    /// actor's supervisor redialling with that actor's secret for the life of
    /// the process. `disconnect()` is async and this method stays synchronous
    /// (its callers and the structural test anchor rely on that), so the
    /// disconnect rides a detached task: the old client outlives the nil-ing by
    /// one hop, and its supervisor stops promptly.
    /// A connect still in flight when this runs is `ensureNestConnected`'s own
    /// `sameActorSince()` guard to catch.
    func adoptActor(_ secretHex: String) {
        if let nestClientSecret, nestClientSecret == secretHex { return }
        if nestClientSecret != nil {
            logMessage(level: .warn, target: "fauna.api",
                       message: "APIClient re-pointed at a different actor — "
                              + "dropping the previous actor's WS-RPC connection")
        }
        if let outgoing = nestClient {
            Task.detached { await outgoing.disconnect() }
        }
        nestClient = nil
        nestClientSecret = nil
        if secret != secretHex {
            // Name both seats: a re-point is legitimate on an e2e re-login and
            // a defect on a rebuilt session, and only the pair tells them apart
            // (a successor re-pointed at its retired predecessor was invisible
            // until this line).
            if let previous = secret {
                let from = (try? actor_id_from_secret(previous)) ?? "?"
                let to = (try? actor_id_from_secret(secretHex)) ?? "?"
                logMessage(level: .warn, target: "fauna.api",
                           message: "[launch] APIClient seat re-pointed \(from) → \(to)")
            }
            actorGeneration += 1
            cachedSeededCaldavClient = nil
            cachedAtprotoMachine = nil
            conversationsSessionCache = nil
            conversationsSessionTask = nil
            token = nil
            tokenExpiresAt = nil
            currentToken = nil
        }
        secret = secretHex
    }

    /// Bumped by `adoptActor` on every genuine actor re-point — never on a
    /// same-actor no-op. See `sameActorSince()`.
    private var actorGeneration = 0

    /// Capture the current actor generation on the synchronous path, before
    /// any `await`; the returned predicate answers "is the actor this build
    /// started for still the current one?" — call it again after the last
    /// `await`, before writing a cache that assignment happens after. Mirrors
    /// web's identical seam for the identical hazard — an async singleton
    /// builder assigning actor-scoped state after an await, which a reset
    /// that only nils the handle cannot stop
    /// (`apps/fauna-web/src/lib/actorScope.ts`'s `sameActorSince()`).
    func sameActorSince() -> () -> Bool {
        let started = actorGeneration
        return { [weak self] in self?.actorGeneration == started }
    }

    /// Every client this mints ends either cached in `nestClient` or
    /// disconnected — never merely dropped, since dropping one leaves its
    /// reconnect supervisor running (see `adoptActor`). Two post-`connect()`
    /// cases would otherwise strand one: an actor switch landing mid-connect
    /// (the client authenticated as the outgoing actor, which `adoptActor`
    /// could not see to disconnect), and a concurrent call for the same actor
    /// that cached its own client first.
    /// Ask this session's nest (the relay) for the region content plane's
    /// policies (`region-blocking.md` § How an app obtains its region's
    /// policy) — `RegionStore`'s one door onto the connection. `onlyIfDue`
    /// asks only when the shared cadence says so (the minute tick). Returns
    /// whether the plane's answer changed; a failed connect asks nothing.
    public func refreshRegionPlane(_ plane: FfiRegionPlane, onlyIfDue: Bool) async -> Bool {
        guard let nest = try? await ensureNestConnected() else { return false }
        return onlyIfDue ? await plane.refreshIfDue(nest: nest) : await plane.refresh(nest: nest)
    }

    private func ensureNestConnected() async throws -> FfiNestClient {
        if let nestClient { return nestClient }
        guard let secret else { throw APIError.ffiError("No secret for WS-RPC connection") }
        let stillThisActor = sameActorSince()
        let client = try FfiNestClient(nestUrl: nodeUrl.absoluteString, secret: hex_to_data(secret))
        try await client.connect()
        guard stillThisActor() else {
            await client.disconnect()
            throw APIError.ffiError("WS-RPC connection: actor changed while connecting")
        }
        if let winner = nestClient {
            await client.disconnect()
            return winner
        }
        nestClient = client
        nestClientSecret = secret
        return client
    }

    /// Subscribe to WS-RPC reconnect bumps over the shared `FfiNestClient`. The
    /// returned subscription's async `next()` returns the new counter on every
    /// reconnect-after-first (`nil` once the client tears down). `FaunaClient`
    /// drives one long-lived loop off this to broadcast `.faunaReconnected`, so
    /// live surfaces re-pull their snapshot — the feed has no poll backstop, so
    /// it would otherwise stay stale after a reconnect (`transport.md` § Push
    /// events). Ensuring-connected here opens the WS-RPC socket eagerly at
    /// startup, which is required to observe reconnects (mirrors linux's single
    /// long-lived client).
    public func subscribeReconnects() async throws -> FfiReconnectSubscription {
        try await ensureNestConnected().subscribeReconnects()
    }

    /// Subscribe to **all** inbound pushes over the shared `FfiNestClient` — the
    /// UniFFI twin of consuming `NestClient::subscribe_pushes()` directly (as the
    /// Rust-native Linux app does). The returned subscription's async `next()`
    /// yields each decoded `FfiPushEvent` (`nil` once the client tears down).
    /// `FaunaClient` drives one long-lived loop off this to dispatch each kind to
    /// the surface it touches — `fauna.notification` → the notifications surface,
    /// `resync_required` → the reconnect sweep (`transport.md` § Push events and
    /// `seq` numbering). Ensuring-connected opens the WS-RPC socket eagerly at
    /// startup, required to observe pushes (mirrors `subscribeReconnects`).
    public func subscribePushes() async throws -> FfiPushSubscription {
        try await ensureNestConnected().subscribePushes()
    }

    // MARK: - W3 (account-data-plane.md § Workstreams) account-store runtime (account-data-plane.md § The account store)

    /// Host the W3 account-store runtime in this process, over the shared
    /// `FfiNestClient` seat (`libs/fauna-ffi/src/account_runtime.rs`).
    ///
    /// `FaunaClient.startAccountRuntime()` owns the three inputs and the reasons
    /// for them; this is the transport hop. Returns as soon as the assembly is
    /// spawned — every I/O-bound step sits on a task inside shared Rust, so a
    /// wedged co-located agent cannot hold up the sign-in that called this.
    ///
    /// `ensureNestConnected()` rather than a cached-only read: the account plane
    /// is one of the surfaces that must exist from the first instant of an
    /// authenticated session, and every other post-auth hook here opens the
    /// socket the same lazy way.
    public func startAccountRuntime(
        appDataDir: String, storeContainer: FfiStoreContainer?, ownDeviceId: Data?,
        accounts: FfiAccountRegistry?
    ) async throws {
        try await ensureNestConnected().startAccountRuntime(
            appDataDir: appDataDir, storeContainer: storeContainer,
            ownDeviceId: ownDeviceId, accounts: accounts)
    }

    // EXCISED BY `FAUNA_EXCISE_P2P_SHARE`: the store-safe FFI exports no share plane
    // (the reason is written once, at the top of `SharePlaneModel.swift`).
    #if !FAUNA_EXCISE_P2P_SHARE

    /// Start the **cross-user share plane** for this session over the shared
    /// `FfiNestClient` (`libs/fauna-ffi/src/share_plane.rs`; `p2p.md` § Cross-user
    /// shared-set transfer). `SharePlaneModel.start` owns the inputs and the reasons
    /// for them; this is the transport hop.
    ///
    /// Shared Rust waits (bounded) for the two things the plane rests on — the account
    /// runtime's store and the conversations session — so a caller needs no ordering
    /// against either; it refuses with an error only when one never lands.
    /// `ensureNestConnected()` for the same reason `startAccountRuntime` uses it.
    public func startSharePlane(
        ownerSecret: Data, provisioner: FfiSyncAgentProvisioner, spoolDir: String,
        listener: FfiSharePlaneListener
    ) async throws {
        // The desktop arm of the replica access: the sync agent hosts the replica.
        try await ensureNestConnected().startSharePlane(
            ownerSecret: ownerSecret, access: agentReplicaAccess(provisioner: provisioner),
            spoolDir: spoolDir, listener: listener)
    }

    #endif

    /// Stop this process's account runtime — sign-out / account-switch /
    /// factory-reset, never a plain quit (`FaunaClient.shutdown()` is the one
    /// funnel that calls it, and its doc carries the reason).
    ///
    /// **Deliberately does NOT `ensureNestConnected()`**, unlike every read
    /// above: opening a WS-RPC socket *during a sign-out* would authenticate as
    /// the identity being torn down. The runtime is a process global, so any
    /// live handle tears down the same one — and a client that never opened a
    /// socket never installed a runtime through it either, so the no-op is
    /// correct rather than a gap.
    public func stopAccountRuntime() async {
        guard let nestClient else { return }
        await nestClient.stopAccountRuntime()
        // The teardown forgot the share plane's cell with the session it belonged to
        // (`libs/fauna-ffi/src/account_runtime.rs::teardown`), so the surface's model
        // re-reads `nil` and the peer-transfer section disappears rather than
        // painting the previous account's readings.
        #if !FAUNA_EXCISE_P2P_SHARE
        await SharePlaneModel.shared.refresh()
        #endif
    }

    /// The sign-out-shaped twin of `stopAccountRuntime()` — call in its place
    /// wherever the credential erase that follows takes this machine's
    /// account-store slot (the writer key) with it: a plain switch, or a
    /// reset/logout that leaves that slot in place, keeps `stopAccountRuntime()`
    /// instead (`sync-agent-credentials.md` § Credential model → *The
    /// signed-out reconcile*). Superset of `stopAccountRuntime()` — retires
    /// this machine's enrollment nest-side FIRST, while the runtime still
    /// holds the writer key, then runs the same local teardown.
    ///
    /// **Deliberately does NOT `ensureNestConnected()`**, same reasoning as
    /// `stopAccountRuntime()` above.
    public func stopAccountRuntimeForSignOut() async {
        guard let nestClient else { return }
        await nestClient.stopAccountRuntimeForSignOut()
        // Same as `stopAccountRuntime()` above: the plane's cell went with the runtime.
        #if !FAUNA_EXCISE_P2P_SHARE
        await SharePlaneModel.shared.refresh()
        #endif
    }

    /// Matches windows' `AccountRuntimeStopBudget` (`App.xaml.cs`):
    /// [`releaseAccountScopedStores()`]'s FFI call is synchronous and
    /// ordinarily fast, but it waits on the conversations receive loop
    /// noticing a retire signal, which could wedge. Erasing late is safe —
    /// the erase does not depend on the release having finished, it only
    /// reports worse if it has not — so the wait is bounded rather than
    /// unbounded, and never a sleep-and-poll.
    private static let releaseAccountScopedStoresBudget: Duration = .seconds(30)

    /// Hand over this actor's conversations engine — closing `mls.db` and
    /// releasing `mls.db.lock` — before the shell erases the account-scoped
    /// directories. Idempotent; a no-op when no session was built.
    ///
    /// The work is shared Rust (`FfiNestClient::release_account_scoped_stores`),
    /// so every UniFFI app gets the same release from the same seam
    /// (`account-scoping.md` § Erasure follows scope — *An OPEN store is an
    /// unerasable store*); this wrapper exists only to bound the wait and to
    /// keep the synchronous FFI call off whatever actor called it.
    ///
    /// ⚠ Dropping the caller's Swift handle is NOT a substitute and never
    /// was: the Rust client stashes the session and manager itself, so those
    /// stashes — not a view model's reference — are what keep the store
    /// open.
    public func releaseAccountScopedStores() async {
        guard let nestClient else { return }
        let released = await withTaskGroup(of: Bool.self) { group in
            group.addTask {
                nestClient.releaseAccountScopedStores()
                return true
            }
            group.addTask {
                try? await Task.sleep(for: Self.releaseAccountScopedStoresBudget)
                return false
            }
            defer { group.cancelAll() }
            return await group.next() ?? false
        }
        if !released {
            logMessage(level: .warn, target: "fauna.account",
                       message: "[account-scope] releasing the conversations engine did not "
                              + "finish within the release budget — erasing anyway")
        }
    }

    /// The `account_pump_cycles` e2e state value as raw JSON, or `nil` when this
    /// app has no connected client to ask — which the serializer reports as the
    /// key being **absent**, never a zero (convention 11: an app without the
    /// leg must not read as "a pass has run").
    ///
    /// A plain atomic read of two counters plus two booleans, which is why it is
    /// non-`async`: convention 11's corollary forbids blocking I/O on the state
    /// path. Reached through this accessor rather than the private `nestClient`
    /// for the same reason every other agent read is.
    public func accountPumpCyclesJson() -> String? {
        nestClient?.accountPumpCyclesJson()
    }

    /// The member-side succession report for `data.succession_witness`, as
    /// raw JSON — `nil` before a conversations session has been built
    /// (`succession-propagation.md` § Implementation status today: this
    /// passthrough is the FFI apps' whole state-contract obligation for the
    /// witness, the mechanism itself already arriving with
    /// `conversationsSession(manager:)`). A plain field read behind the FFI
    /// call, same posture as `accountPumpCyclesJson` above.
    public func successionWitnessStateJson() -> String? {
        nestClient?.successionWitnessStateJson()
    }

    /// Run one full account-pump pass now — the `account_pump_now` poke's leg
    /// (`fauna_e2e_agent::ACCOUNT_PUMP_NOW`). `false` when there is no runtime
    /// to poke (pre-auth, or an assembly that has not landed): a quiet, honest
    /// no-op the caller can report, never a silently dropped command.
    ///
    /// The contract is the pass THEN the ceremony drive, so any act the pass
    /// left owed — above all the custody receipt it just minted — posts
    /// without waiting for a production edge (tui's arm, the reference). The
    /// FFI's `accountPumpNow` chains that drive itself, in shared Rust, for
    /// every UniFFI app — never add a second one here.
    public func accountPumpNow() async -> Bool {
        guard let nestClient else { return false }
        return await nestClient.accountPumpNow()
    }

    /// The standing device-cap refusal notice (`devices.md` § Errors & edge
    /// cases), already localized by the nest — `nil` once a slot frees and a
    /// register clears it. `nil` with no runtime is indistinguishable from "no
    /// refusal standing", which is correct: pre-auth there is nothing to warn
    /// about either way.
    public func accountEnrollmentNotice() async -> String? {
        guard let nestClient else { return nil }
        return await nestClient.accountEnrollmentNotice()
    }

    #if DEBUG
    /// The fleet-removal convergence read (`account-data-taxonomy.md` § The
    /// generation machinery → *Fleet-scope reclamation*, clause (4)) — whether
    /// `deviceIdHex`'s plane `fauna.state.device-set` row reads Removed/Enrolled
    /// from THIS app's own account runtime, JSON-encoded. `nil` when there is no
    /// connected client to ask; the caller reports that as the same quiet "not
    /// found" the shared reader answers for a missing store.
    ///
    /// **`#if DEBUG`, and must be — unlike `accountEnrollmentNotice()` above.**
    /// That one is a production export; this is a `test-helpers` UniFFI export
    /// (a real plane-content read, convention 15 rule (a)), so the production FFI
    /// flavor's Swift bindings do not carry it and an ungated declaration compiles
    /// under `mac-debug`/`swift-test` but fails `-c release` (`apple-store-safe-check`,
    /// `mac-release`) with `cannot find … in scope`. Pinned by
    /// `test_apple_seam_gating.py`. Only `DeviceSetStateTestCommand` calls it.
    public func deviceSetStateJson(deviceIdHex: String) async -> String? {
        guard let nestClient else { return nil }
        return await nestClient.deviceSetStateJson(deviceIdHex: deviceIdHex)
    }

    /// The `reconnect_backoff` e2e seam on the live connection
    /// (`FfiNestClient.setReconnectBackoffForTest`, a `test-helpers` export —
    /// hence `#if DEBUG`, like `deviceSetStateJson` above). `false` when there is
    /// no connection to pace; throws on a payload shared Rust refuses. Only
    /// `E2eLoudSurfaces` calls it.
    public func setReconnectBackoffForTest(payloadJson: String) throws -> Bool {
        guard let nestClient else { return false }
        try nestClient.setReconnectBackoffForTest(payloadJson: payloadJson)
        return true
    }

    /// The `launch_token` e2e state value as JSON text
    /// (`FfiNestClient.launchTokenJsonForTest`, a `test-helpers` export — hence
    /// `#if DEBUG`, like the two above), or `nil` when there is no connection to
    /// ask (published as JSON `null`, convention 11). Sync: an atomic read of the
    /// held bearer's anchored deadline. Only `AppStateObservables` calls it.
    public func launchTokenJsonForTest() -> String? {
        nestClient?.launchTokenJsonForTest()
    }

    /// Clear the held session bearer and re-mint it over the silent challenge
    /// (`FfiNestClient.refreshHeldBearerForTest`) — the wrong-clock refresh
    /// witness's ceremony leg. Returns `false` when there is no connection;
    /// throws when the mint fails. Only `LaunchRefreshTokenTestCommand` calls it.
    public func refreshHeldBearerForTest() async throws -> Bool {
        guard let nestClient else { return false }
        try await nestClient.refreshHeldBearerForTest()
        return true
    }
    #endif

    /// Subscribe to the live WS-RPC `ConnectionState` over the shared
    /// `FfiNestClient` — the UniFFI twin of the transport `connection_state()`
    /// watch (sibling of `subscribeReconnects`). The returned subscription's async
    /// `next()` yields the *current* state first, then each transition (`nil` once
    /// the client tears down). `FaunaClient` drives one long-lived loop off this to
    /// publish the global `connection-status` indicator (`transport.md` §
    /// Connection-status indicator). Ensuring-connected here opens the WS-RPC
    /// socket eagerly at startup, which is required to observe the live state.
    public func subscribeConnectionState() async throws -> FfiConnectionStateSubscription {
        try await ensureNestConnected().subscribeConnectionState()
    }

    /// Why the WS-RPC supervisor stopped for good, when the reason is a
    /// session-ending verdict — read on a `.disconnected` from
    /// `subscribeConnectionState()`. `nil` while it runs, and for every other
    /// stop. The classification is shared Rust (`FfiNestClient.sessionEndingVerdict`).
    /// Reads the existing client only — a verdict is about the socket that
    /// stopped, so this never opens a new one.
    public func sessionEndingVerdict() -> FfiSessionEndingVerdict? {
        nestClient?.sessionEndingVerdict()
    }

    /// Subscribe to inbound contact-request (**knock**) pushes over the shared
    /// `FfiNestClient` — the UniFFI twin of linux consuming the broker's
    /// `subscribe_kind("fauna.knock")` directly. `fauna.knock` is a *dedicated*
    /// broker kind, **not** a `FfiPushEvent` variant, so the generic
    /// `subscribePushes` loop never sees it — a knock reaches a mounted contacts
    /// screen only through this seam (`transport.md` § Push events; matches windows
    /// `NestRpcClient.SubscribeKnocks`/android `subscribeKnocks`). The returned
    /// subscription's async `next()` resolves each decoded knock (`nil` once the
    /// client tears down); `FaunaClient` drives one long-lived loop off this to
    /// post `.faunaKnockReceived`, which `ContactsVM` re-fetches on. Ensuring-connected
    /// opens the WS-RPC socket eagerly at startup, required to observe knocks (mirrors
    /// `subscribeReconnects`).
    public func subscribeKnocks() async throws -> FfiKnockSubscription {
        try await ensureNestConnected().subscribeKnocks()
    }

    /// Build the in-process **file-sync engine host** over the shared WS-RPC
    /// connection — apple's byte-sync deployment (`file-sync.md` § Apple apps —
    /// convergence design). The host runs the shared `fauna-sync-engine` in-process:
    /// one resident engine per folder binding on macOS, one-shot passes on iOS.
    /// `FaunaClient` calls this once per authenticated session and holds the handle;
    /// dropping it stops every engine.
    ///
    /// The secret is this client's (it derives the owner `BackupKey` that seals every
    /// chunk and unseals the content-key custody), so it is never passed in
    /// — same reason `conversationsSession` reads it from here. `observer` receives a
    /// per-path re-read signal for the `sync-state-badge` (`SyncStatesStore`).
    /// Resolve the folder this device's photo-library ingress feeds — creating
    /// the wizard-preset Backup-mode "Photo Library" set on first use, or
    /// returning the set this device already bound (never re-creating
    /// it: its rows are the user's photos). The resolve-or-create rule is shared
    /// Rust (`fauna_folders_machine::photo_library`), so apple and android
    /// cannot drift on it.
    public func resolvePhotoLibrarySet(
        stateDir: String,
        deviceId: String,
        deviceLabel: String
    ) async throws -> FfiPhotoLibrarySet {
        try await photoLibrarySet(
            nest: ensureNestConnected(),
            stateDir: stateDir,
            deviceId: deviceId,
            deviceLabel: deviceLabel
        )
    }

    /// Build the in-process **sync engine host** — construct-run-drop engine
    /// work only (photo ingress, restore, per-file state reads;
    /// `sync-engine-deployments.md` § Apple apps — convergence design). It owns
    /// **no resident engine**: residency is the external `fauna-sync-agent`'s
    /// (`sync-agent.md` § Control plane split).
    public func syncEngineHost(
        deviceId: String,
        stateDir: String,
        deviceLabel: String
    ) async throws -> FfiSyncEngineHost {
        guard let secret else { throw APIError.ffiError("No secret for the sync engine host") }
        return try await ensureNestConnected().syncEngineHost(
            secret: hex_to_data(secret),
            deviceId: hex_to_data(deviceId),
            stateDir: stateDir,
            deviceLabel: deviceLabel,
            // The registry, so every engine this host builds carries the
            // account's PAIRED predecessor chain, resolved in Rust off this
            // session's own actor: a row a retired identity signed opens under
            // that identity's root (`writer-signed-change-records.md` ruling
            // (8)(c)). Handed none, the host proves the link by the walk only.
            accounts: FaunaAccounts.registry()
        )
    }

    /// Build this device's **client-device backup custodian** host, or `nil`
    /// when this device is not an enrolled custodian — the ordinary answer on
    /// most devices, and a clean no-op pass rather than an error
    /// (`docs/goal/behavior/backup-destinations.md` § Third destination kind;
    /// the iOS twin of android's `ApiClient.buildCustodianHost`).
    /// Construct-run-drop per pass, mirroring android: the caller builds a
    /// fresh host for each scheduled wake rather than holding one, since the
    /// registry row (an owner might newly enroll, or raise the cap, between
    /// passes) is read fresh every time. `dataDir` is this shell's UNSCOPED
    /// per-user base — `AccountStateDir.base`, never the actor-scoped
    /// subdirectory — because `build_custodian_host` derives the actor-scoped
    /// store location itself (`custodian_store_root`); passing the already-
    /// scoped dir would scope twice and silently produce a store that can
    /// restore nothing (android-leg append).
    public func custodianHost(
        deviceId: String,
        dataDir: String,
        excluder: FfiCloudBackupExcluder
    ) async throws -> FfiCustodianHost? {
        guard let secret else { throw APIError.ffiError("No secret for the custodian host") }
        return try await buildCustodianHost(
            nest: ensureNestConnected(),
            ownerSecret: hex_to_data(secret),
            deviceId: hex_to_data(deviceId),
            dataDir: dataDir,
            exclusion: .excludedByShell(excluder: excluder)
        )
    }

    /// Measure this device's sealed custodian store **app-locally** — the read
    /// behind `backup-orphaned-store-row` on a shell that hosts its own replica
    /// (iOS; `backups.md` § Manage backup destinations → *Reclaim this device's
    /// copy*). The twin of android's `ApiClient.custodianStoreFootprint`.
    ///
    /// A free fn rather than a method on `FfiCustodianHost` because the host does
    /// not exist in the state this read serves: `build_custodian_host` answers
    /// `nil` whenever no registry row names this device, and an **orphaned** store
    /// is precisely that state. `dataDir` is the UNSCOPED per-user base
    /// (`AccountStateDir.base`), never the actor-scoped subdirectory — the
    /// shared side derives the actor scope itself, so a pre-scoped dir would scope
    /// twice and measure a store that holds nothing.
    ///
    /// A store that does not exist is an **empty** store, not an error.
    public func custodianStoreFootprint(dataDir: String) async throws -> FfiCustodianStoreInfo {
        guard let secret else { throw APIError.ffiError("No secret for the custodian store read") }
        return try await FaunaFFISwift.custodianStoreFootprint(
            ownerSecret: hex_to_data(secret), dataDir: dataDir)
    }

    /// Free this device's whole sealed custodian store app-locally (iOS) — the
    /// confirmed `backup-destination-reclaim-button` action and the
    /// `backup-destination-remove-reclaim-checkbox` opt-in.
    ///
    /// The shared side stops this process's own custodian work first (a foreground
    /// push loop has no periodic tick, so waiting it out waits forever) and
    /// answers `stillHosting` — **deleting nothing** — rather than racing work
    /// that will not stop. That refusal is a reported outcome, never an error.
    public func reclaimCustodianStore(dataDir: String) async throws -> FfiCustodianReclaimOutcome {
        guard let secret else { throw APIError.ffiError("No secret for the custodian reclaim") }
        return try await FaunaFFISwift.reclaimCustodianStore(
            ownerSecret: hex_to_data(secret), dataDir: dataDir)
    }

    /// Restore the signed-in nest from this device's sealed store **in this
    /// process** (iOS) — the confirmed `backup-destination-reseed-confirm-button`
    /// action (`backups.md` § Restore after losing the nest). The shared side
    /// runs the whole ceremony and its post-ceremony re-enrollment; a stop is a
    /// reported `stopped` reason, never an error. `deviceId` is this device's
    /// stable sync id (hex); `dataDir` the unscoped base, as for the reclaim.
    public func reseedCustodianStore(deviceId: String, dataDir: String) async throws -> FfiReseedResult {
        guard let secret else { throw APIError.ffiError("No secret for the restore") }
        return try await FaunaFFISwift.reseedCustodianStore(
            nest: ensureNestConnected(), ownerSecret: hex_to_data(secret),
            deviceId: hex_to_data(deviceId), dataDir: dataDir)
    }

    /// The same restore on a desktop, where the sync agent hosts the store and
    /// so runs the ceremony (macOS). The owner's connection is what the shared
    /// re-enrollment writes through once the agent's job is whole.
    public func reseedCustodianStore(
        via provisioner: FfiSyncAgentProvisioner, deviceId: String
    ) async throws -> FfiReseedResult {
        guard let secret else { throw APIError.ffiError("No secret for the restore") }
        return try await provisioner.reseedCustodianStore(
            nest: ensureNestConnected(), ownerSecret: hex_to_data(secret), thisDeviceId: deviceId)
    }

    /// Build the desktop **sync-agent provisioner** over this actor's connection
    /// (`sync-agent.md` § Control plane split + § Credential model, milestone A4).
    /// The secret is this client's (it signs the `RenewBearer` grant and derives
    /// the owner `BackupKey` pushed in the capability — same never-passed-in rule
    /// as `syncEngineHost`). No content keys are provisioned: the agent resolves
    /// every set's keys from this account's custody itself, and shared Rust
    /// provisions only an agent that advertises doing so
    /// (`sync-agent-credentials.md` § Credential model). `predecessorBackupKeys` is the retired owner
    /// keys off `FfiAccountRegistry.predecessorBackupKeys(actorId:)`
    /// (`sync-agent.md` § Credential model → *Retired owner keys after an
    /// identity succession*) — empty for every identity that never succeeded,
    /// the caller resolves it (see `FaunaClient.makeSyncAgentProvisioner`) since
    /// this wrapper has no actor id of its own. `predecessorActorIds` is its
    /// attested-ids sibling (`FfiAccountRegistry.attestedPredecessorActorIds(actorId:)`),
    /// same caller-resolves contract. The caller supplies the two
    /// genuinely platform-specific hooks: how to spawn the agent and where the
    /// app's live bearer comes from ([`APIClientProvisioningBearerSource`]).
    public func syncAgentProvisioner(
        deviceId: String,
        deviceLabel: String,
        predecessorBackupKeys: [Data],
        predecessorActorIds: [Data],
        spawner: FfiAgentSpawner,
        bearerSource: FfiProvisioningBearerSource,
        reachabilityObserver: FfiAgentReachabilityObserver? = nil
    ) async throws -> FfiSyncAgentProvisioner {
        guard let secret else { throw APIError.ffiError("No secret for the sync-agent provisioner") }
        let provisioner = try await ensureNestConnected().syncAgentProvisioner(
            identitySecret: hex_to_data(secret),
            backupKey: backupKeyDerive(secret: hex_to_data(secret)),
            predecessorBackupKeys: predecessorBackupKeys,
            predecessorActorIds: predecessorActorIds,
            deviceId: deviceId,
            deviceLabel: deviceLabel,
            spawner: spawner,
            bearerSource: bearerSource,
            reachabilityObserver: reachabilityObserver,
            // The registry, from which Rust resolves the capability's
            // `predecessor_keys_by_actor` — each retired key PAIRED with its
            // identity, so the agent offers a predecessor-signed row only that
            // predecessor's root (`sync-agent-credentials.md` § Credential
            // model). The two flat lists above stay: the face takes both.
            accounts: FaunaAccounts.registry()
        )
        return provisioner
    }

    /// The app's current minted bearer + its unix-seconds refresh deadline, for
    /// the sync-agent provisioning loop (`FfiProvisioningBearerSource`). The
    /// deadline is `authenticate()`'s early-refresh point (mint expiry − 60 s),
    /// on this device's clock: `mintBearer`'s `expiresAt` is anchored at receipt
    /// (`login.md` § Token lifetime on the client's clock). Conservative is
    /// correct here — the agent renews off it via its own grant. A token without
    /// a deadline (never set apart, but not provable from here) reads as no
    /// bearer, so the tick skips rather than hand the agent a guess.
    public func currentBearerForProvisioning() -> (token: String, expiresAt: UInt64) {
        guard let token = currentToken, let deadline = tokenExpiresAt else { return ("", 0) }
        return (token, UInt64(max(0, deadline.timeIntervalSince1970)))
    }

    /// Build the page-level Devices state machine (`DevicesMachine`, shared Rust)
    /// over the shared WS-RPC connection — the device / folder / conflict reads
    /// (`refresh`), the page write gestures (`removeDevice` / `deleteFolder` /
    /// `resolveConflict` / `setFolderPaths`), and the embedded folder creation
    /// wizard (`openWizard` / `wizard`) all ride `FfiNestClient`. `observer` ticks
    /// on every snapshot change. The Devices page is a dumb renderer of this
    /// machine — no device/folder/conflict HTTP, no client-side wizard logic
    /// (`docs/goal/ui/devices.md` § Broader DevicesSnapshot / § Where logic lives).
    /// Shared by macOS + iOS (the FaunaKit lift, priority #2); mirrors
    /// `conversationsSession()`. `buildDevicesMachine` is the UniFFI free function
    /// (`libs/fauna-ffi/src/devices.rs`).
    public func devicesMachine(observer: DevicesObserver) async throws -> DevicesMachine {
        let nest = try await ensureNestConnected()
        let machine = buildDevicesMachine(nest: nest, observer: observer)
        // READ-side custody for a successor: the registry's PAIRED predecessor
        // chain, widening the label custody the build wired. A succession
        // re-points an owned set to the successor and re-seals nothing, so its
        // name still rests under the predecessor's owner root; unwired, the
        // successor's Folders page drops every set it inherited
        // (`succession-aftermath.md` § Re-key scope, the `BackupKey` corpus row).
        // Never a seal root. The Media seat's twin (`MediaMachineVM.configure`);
        // windows' `BuildDevicesMachineAsync`. Empty for an identity that never
        // succeeded.
        if let actorId = boundActorIdHex {
            let chain = FaunaAccounts.registry().predecessorChain(actorId: actorId)
            if !chain.actorIds.isEmpty {
                machine.setPredecessorChain(actorIds: chain.actorIds, keys: chain.keys)
            }
        }
        // Inject the MLS join-filter the machine uses to decide which *shared-with-me*
        // (`role == "member"`) folders may surface. The nest returns ROSTERED members
        // — it cannot observe an MLS group join (client-side crypto) — so the machine
        // drops every member row this client has not actually joined. **Fail-safe: an
        // unwired machine hides ALL member rows**, so a stranger's rostered-but-
        // un-accepted knock can never appear unbidden in the list (it surfaces only as a
        // `folder-pending-share`) — `folders.md` § Sharing (Member list-visibility).
        //
        // Wired HERE because this is the only place both the machine and the private
        // cached `ConversationsSession` (the one per-actor `MlsEngine`) are in scope, and
        // it is strictly before `DevicesMachineVM.configure`'s first `refresh()`, as the
        // shared fn requires. Best-effort on the session (mirrors windows' non-null
        // guard in `FoldersPage.Page_Loaded`): no session ⇒ shared sets simply stay
        // hidden, never a page error. AWAITS rather than racing an in-flight login-time
        // build (`awaitSharedConversationsSession()`'s doc) — Folders is routinely the
        // first page a user opens right after login, so racing here would silently hide
        // every shared-with-me row on a fast-enough launch.
        if let session = try? await awaitSharedConversationsSession() {
            wireDevicesMlsQuery(devices: machine, session: session)
        }
        // Inject the foreign-set (cross-nest) list source, the twin the line above has
        // always needed: a set shared from ANOTHER nest has no row in this nest's list,
        // so the machine unions in the member's own `fauna.state.folder-keys` records written at
        // share-accept (`folders.md` § Cross-nest shares). Unwired, the machine's
        // source stays `None` and it appends NO foreign rows at all
        // (`libs/fauna-devices-machine/src/machine.rs` — `if let Some(source)`), so a
        // cross-nest shared set is silently absent rather than stale. Linux has wired the
        // native twin (`set_foreign_sets_source`) since Phase 2; apple was never wired,
        // which is why this call is new — the export existed with zero callers fleet-wide.
        //
        // Best-effort exactly like the MLS query above: no secret (pre-login) ⇒ no foreign
        // rows, never a page error. Same `nest` handle the machine was built on.
        if let secret {
            try? wireDevicesForeignSets(
                devices: machine, nest: nest, ownerSecret: hex_to_data(secret))
            // Inject the FOLLOWED public folders source — the rows behind
            // `folder-followed-item` (ui/folders.md § Following a public
            // folder). The records live in the follower's own account store
            // (`fauna.state.follows`; the home nest keeps zero follower state
            // by design), so the shared `StoreFollowedFoldersSource` reads
            // them there and probes each folder's availability.
            //
            // ⚠ Unwired, `DevicesSnapshot.followed` is permanently EMPTY and
            // the section renders no rows at all — the same silent,
            // compiles-and-is-unreachable failure the wasm face shipped for a
            // day (`ui/media.md` § Implementation status today records it).
            // Best-effort like the two seams above: no secret (pre-login) ⇒ no
            // followed rows, never a page error.
            try? wireDevicesFollowedFolders(
                devices: machine, nest: nest, ownerSecret: hex_to_data(secret))
        }
        return machine
    }

    // EXCISED BY `FAUNA_EXCISE_P2P_SHARE` — the `p2p-share` member's ceremony half
    // (the reason is written once, at the top of `SharePlaneModel.swift`).
    #if !FAUNA_EXCISE_P2P_SHARE

    // MARK: - Offline co-present share ceremony (folders page)
    // (`offline-share-*`/`offline-receive-*` ids; docs/goal/behavior/p2p.md
    // § Offline share initiation. No orchestration here — the whole ceremony
    // is shared Rust behind `libs/fauna-ffi/src/offline_share.rs`; apple owes
    // only the same `nest`+`ownerSecret` idiom every other seam here uses.
    // Named apart from the generated free functions of the same base name
    // (`offlineShareBindSeat`/`offlineShareView`/…) — same disambiguation
    // `loadCustodyFacet` uses over `custodyFacetLoad`.

    /// Bind (or re-read) the ceremony seat's listener — the panel-OPEN
    /// trigger, never login (an actor-keyed endpoint for a feature most
    /// people never touch would be waste — the row's own rationale, mirrored
    /// from linux's `bind_offline_share_seat`).
    public func bindOfflineShareSeat() async throws -> FfiCeremonySeat {
        guard let secret else { throw APIError.ffiError("No secret for the offline-share seat") }
        return try await offlineShareBindSeat(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret))
    }

    /// The whole paint decision — every offline-share element reads from
    /// here, never from raw panel/status/seat state directly (mirrors
    /// linux's `OfflineShareState::view()`).
    public func offlineShareViewSnapshot(
        panel: OfflineSharePanel, seat: FfiCeremonySeat?, peerCodeInput: String,
        status: CeremonyStatus
    ) throws -> OfflineShareView {
        guard let secret else { throw APIError.ffiError("No secret for the offline-share view") }
        return try offlineShareView(
            panel: panel, ownerSecret: hex_to_data(secret), seat: seat,
            peerCodeInput: peerCodeInput, status: status)
    }

    /// The typed `offline-share-peer-code-input`'s validity gate —
    /// `actor.isEmpty` is the invalid signal, not `error` being set (an
    /// empty/not-yet-typed input answers `error: nil` by design).
    public func parseOfflinePeerCode(input: String) throws -> PeerCodeParsed {
        guard let secret else { throw APIError.ffiError("No secret for the offline-share peer code") }
        return try offlineShareParsePeerCode(input: input, ownerSecret: hex_to_data(secret))
    }

    /// `offline-share-begin-button` — the whole initiator walk in one round
    /// trip (no interim progress — mirrors linux/tui). Needs an already-bound
    /// `seat` (the row's own 2026-08-20 finding).
    public func beginOfflineShare(seat: FfiCeremonySeat, peerCodeInput: String) async throws
        -> CeremonyStatus
    {
        guard let secret else { throw APIError.ffiError("No secret for offline sharing") }
        return try await offlineShareInitiate(
            nest: try await ensureNestConnected(), seat: seat, ownerSecret: hex_to_data(secret),
            peerCodeInput: peerCodeInput)
    }

    /// The consent card's Accept (`folder-share-accept-button`, the group
    /// arm) — mint + rest the reception key, record the accept, await the
    /// deliver, admit, write through. One act, because it is one decision.
    /// Needs an already-bound `seat`.
    public func consentToOfflineGroupShare(seat: FfiCeremonySeat, scopeId: Data) async throws
        -> CeremonyStatus
    {
        guard let secret else { throw APIError.ffiError("No secret for offline sharing") }
        return try await offlineShareConsent(
            nest: try await ensureNestConnected(), seat: seat, ownerSecret: hex_to_data(secret),
            scopeId: scopeId)
    }

    /// The consent card's Decline (`folder-share-decline-button`, the group
    /// arm) — monotone, fleet-wide, terminal (rule 6). Needs an already-bound
    /// `seat`.
    public func declineOfflineGroupShare(seat: FfiCeremonySeat, scopeId: Data) async throws {
        guard let secret else { throw APIError.ffiError("No secret for offline sharing") }
        try await offlineShareDecline(
            nest: try await ensureNestConnected(), seat: seat, ownerSecret: hex_to_data(secret),
            scopeId: scopeId)
    }

    /// The co-present ceremony's own group-scope listing + consent-card
    /// invitations — needs no bound seat (an account-store read), but takes
    /// the session's seat when one is bound: with the nest unreachable the
    /// shared read answers from its in-memory ceremony record, so a
    /// co-present consent card still paints (`p2p.md` § Offline share
    /// initiation). ANY failure maps to the empty default (mirrors
    /// `loadPendingShares`).
    ///
    /// **Never gated on `ensureNestConnected()` succeeding**, unlike every
    /// other call site here: the read answers from the account store with the
    /// nest unreachable (the FFI call leaves `nest` unread), so reaching the
    /// FFI call — not the WS reaching `Connected` — is the bar. A cached
    /// client is reused as-is; an uncached one is constructed and `connect()`
    /// is attempted but its failure is swallowed rather than thrown, because
    /// `connect()` still starts the client's background reconnect supervisor
    /// even when its own bounded wait for `Connected` times out
    /// (`FfiNestClient.connect`'s own doc) — so a merely-slow nest still gets
    /// used once it answers. No second, Swift-side fallback is added here.
    ///
    /// The uncached client is never stored in `nestClient`: dropping an
    /// `FfiNestClient`/`NestClient` does not stop its reconnect supervisor —
    /// there is no `Drop` impl, and the supervisor task holds its own `Arc`
    /// copies of auth and the dispatcher (`libs/fauna-client/src/client.rs:382-386`,
    /// the 2026-08-22 leaked-socket incident's cause) — so a fresh client left
    /// unstored would redial with the actor's secret for as long as the
    /// object lived. `disconnect()` stops that supervisor outright and is a
    /// no-op when `connect()` never got one running (a failure in
    /// authentication, before the supervisor spawns), so it's called
    /// unconditionally once the read is done.
    public func loadOfflineGroupShares(seat: FfiCeremonySeat? = nil) async -> FfiGroupShareViews {
        guard let secret else { return FfiGroupShareViews(invitations: [], scopes: []) }
        let nest: FfiNestClient
        let ephemeral: FfiNestClient?
        if let cached = nestClient {
            nest = cached
            ephemeral = nil
        } else {
            guard let fresh = try? FfiNestClient(
                nestUrl: nodeUrl.absoluteString, secret: hex_to_data(secret)) else {
                return FfiGroupShareViews(invitations: [], scopes: [])
            }
            try? await fresh.connect()
            nest = fresh
            ephemeral = fresh
        }
        let result = (try? await offlineShareLoadGroupShares(
            nest: nest, ownerSecret: hex_to_data(secret), seat: seat))
            ?? FfiGroupShareViews(invitations: [], scopes: [])
        if let ephemeral {
            await ephemeral.disconnect()
        }
        return result
    }

    #if DEBUG
    /// The `offline_share_drop_connections` TestAgent command's door — drop
    /// every connection a counterpart has open to THIS SESSION's bound
    /// ceremony seat, keeping the listener up, and return how many were
    /// dropped. Routes through the FFI's process-wide session-seat slot
    /// (`offline_share_drop_connections_for_test`,
    /// `libs/fauna-ffi/src/offline_share.rs::SESSION_SEAT`) rather than a
    /// live `FfiCeremonySeat` handle, because no SwiftUI view model here is
    /// reachable from the app shell (`offlineShareSeat` is `@State` private
    /// to each folders view) — the same reason tui/linux read their own
    /// session's seat rather than a UI-owned one. Throws when no seat is
    /// bound for this session; the caller must surface that as a LOUD
    /// TestAgent failure (convention 11), never a silent zero. A `test-helpers`
    /// export, hence `#if DEBUG`, like `setReconnectBackoffForTest` above; only
    /// `OfflineShareTestCommand` (itself `#if DEBUG`) calls it.
    public func dropOfflineShareConnectionsForTest() throws -> UInt32 {
        guard let secret else { throw APIError.ffiError("No secret for the offline-share seat") }
        return try offlineShareDropConnectionsForTest(ownerSecret: hex_to_data(secret))
    }
    #endif

    #endif  // !FAUNA_EXCISE_P2P_SHARE

    // MARK: - T16 custody facet (devices.md § Custody facet, pieces 1–3 + the mint)
    //
    // Every act runs in the shared `fauna_client_custody::run_custody_act`
    // behind `libs/fauna-ffi/src/custody.rs`; the store-writing ones (budget,
    // stop, remove, accept) reach the account store through the FFI's own
    // `account_runtime::handle()`, so apple passes nothing but the usual
    // `nest` + `ownerSecret` (+ the conversations session where the drive
    // POSTS: accept and the mint). Each act answers with the re-folded facet
    // AND its error — the caller repaints from one and puts the other on
    // `error-message` (e2e convention 11). Named apart from the generated free
    // functions of the same base name (`custodyFacetLoad`/`custodyRevoke`/…) —
    // same disambiguation `listBackupDestinations` uses over `backupDestinationsList`.

    /// Load the owner-side custody facet (`custody-holder-*` rows) — a pure fold
    /// over the owner's `fauna.state.custody-ceremony` entries. `nil` on any
    /// failure (unreadable store, not connected yet): the caller keeps its
    /// previously-painted rows rather than blanking live ones, since the facet
    /// rides the page's own load edge and is not machine state.
    public func loadCustodyFacet() async throws -> CustodyFacetView? {
        guard let secret else { throw APIError.ffiError("No secret for custody facet") }
        return try await custodyFacetLoad(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret))
    }

    /// Revoke a custody grant (`custody-holder-revoke-button`) — piece 2's one
    /// gesture, and store-free. Carries the row's **grant id, never a row
    /// index** (a refold re-orders rows) and its accept-bound `custodianKey`
    /// (absent while the ceremony is pending — the control is disabled then).
    /// Returns the act's error string alongside the re-folded facet so a
    /// refused gesture reaches the page `error-message` (e2e convention 11).
    public func revokeCustody(grantId: Data, holder: Data?) async throws -> FfiCustodyActOutcome {
        guard let secret else { throw APIError.ffiError("No secret for custody facet") }
        return try await custodyRevoke(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret),
            grantId: grantId, holder: holder)
    }

    /// Fire one ceremony drive pass — fetches the receipts custodian nests
    /// deposited at this account's nest and folds each through the recorded-
    /// accept verify path, which is what makes owner-side receipt freshness
    /// real (without it the receipt line reads "no receipt yet" forever).
    /// Fire-and-forget and best-effort by design (mirrors android's
    /// `runCatching { … }.onFailure { ShellLog.w(…) }`): needs a live
    /// conversations session to post/read the ceremony channel, so a caller
    /// with none (pre-login, or the session still starting) just skips this
    /// pass — never a page error, since a drive miss only delays freshness by
    /// one visit, and the load edge always re-tries on the next.
    public func driveCustody() async {
        guard let secret, let session = await sharedConversationsSession() else { return }
        do {
            // `await`: the export is async so the pass can be `tokio::spawn`ed —
            // the synchronous one panicked "there is no reactor running" every call.
            try await custodyDrive(
                nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret),
                session: session)
        } catch {
            logMessage(level: .warn, target: "fauna.devices.custody",
                       message: "custodyDrive failed: \(error)")
        }
    }

    /// Accept a custody offer (`custody-offer-accept-button`). `onNest` is the
    /// target select's answer — `false` binds THIS device's principal, `true`
    /// the home nest's pinned identity (offered only where
    /// `custodyOfferShowsTarget` says so). The accept is recorded here and
    /// POSTED by the drive over the conversations session, which is why a
    /// missing session refuses rather than recording an accept nobody sends.
    public func acceptCustody(grantId: Data, onNest: Bool) async throws -> FfiCustodyActOutcome {
        guard let secret else { throw APIError.ffiError("No secret for custody facet") }
        guard let session = await sharedConversationsSession() else {
            throw APIError.ffiError(L.errors.notConnectedToNest)
        }
        return try await custodyAccept(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret),
            session: session, grantId: grantId, onNest: onNest)
    }

    /// Decline a custody offer (`custody-offer-decline-button`) — the card
    /// goes away on the re-folded facet.
    public func declineCustody(grantId: Data) async throws -> FfiCustodyActOutcome {
        guard let secret else { throw APIError.ffiError("No secret for custody facet") }
        return try await custodyDecline(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret), grantId: grantId)
    }

    /// Change a held custody's retained-bytes budget
    /// (`custody-held-budget-input` commit). `cap` is already parsed by the
    /// shared `parseByteSize` — this never sees the typed text.
    public func setCustodyBudget(grantId: Data, cap: UInt64) async throws -> FfiCustodyActOutcome {
        guard let secret else { throw APIError.ffiError("No secret for custody facet") }
        return try await custodySetBudget(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret),
            grantId: grantId, cap: cap)
    }

    /// Stop holding (`custody-held-stop-button`) — pauses the hold, KEEPS the
    /// bytes; `removeCustody` is the reclaim.
    public func stopCustody(grantId: Data) async throws -> FfiCustodyActOutcome {
        guard let secret else { throw APIError.ffiError("No secret for custody facet") }
        return try await custodyStop(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret), grantId: grantId)
    }

    /// Remove a held custody and free its space (`custody-held-remove-button`).
    public func removeCustody(grantId: Data) async throws -> FfiCustodyActOutcome {
        guard let secret else { throw APIError.ffiError("No secret for custody facet") }
        return try await custodyRemove(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret), grantId: grantId)
    }

    /// Whether `offer`'s consent card renders `custody-offer-target-select` —
    /// an advertising offer AND a pinned identity for the home nest (a local
    /// pin-store read). Absent otherwise, never disabled.
    public func custodyOfferShowsTarget(_ offer: CustodyOfferRowView) async -> Bool {
        guard let nest = try? await ensureNestConnected() else { return false }
        return custodyOfferShowsTargetSelect(nest: nest, offer: offer)
    }

    /// The mint flow's host options (`custody-mint-host-select`) — this
    /// account's 1:1 conversations. Empty with no conversations session yet:
    /// there is no channel to send the request over, which the caller answers
    /// with `devices.custody_mint_no_contacts` exactly as for no conversations.
    public func loadCustodyMintCandidates() async throws -> [CustodyMintCandidateView] {
        guard let secret else { throw APIError.ffiError("No secret for custody facet") }
        guard let session = await sharedConversationsSession() else { return [] }
        return try custodyMintCandidates(ownerSecret: hex_to_data(secret), session: session)
    }

    /// Send a custody offer (`custody-mint-confirm-button`) over a
    /// `loadCustodyMintCandidates` row's `host` + `channelHex`, passed back
    /// unchanged — the page picks a row, it never assembles a channel.
    public func mintCustody(host: Data, channelHex: String) async throws -> FfiCustodyActOutcome {
        guard let secret else { throw APIError.ffiError("No secret for custody facet") }
        guard let session = await sharedConversationsSession() else {
            throw APIError.ffiError(L.devices.custodyMintNoContacts)
        }
        return try await custodyMint(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret),
            session: session, host: host, channelHex: channelHex)
    }

    /// Build the page-level **Backups** machine (`BackupsMachine`, shared Rust)
    /// over the shared WS-RPC connection — the Backups page's **snapshot half**:
    /// the folder selector + snapshot reads (`refresh` / `selectFolder`), the
    /// page write gestures (`createSnapshot` / `deleteSnapshot` /
    /// `deleteSnapshotImmediate` / `prunePreview` / `pruneExecute` / `check`) and
    /// the `snapshot-detail-files` read (`openSnapshot` / `closeSnapshotDetail`),
    /// all over `FfiNestClient`. `observer` ticks on every snapshot change. The
    /// page is a dumb renderer of this machine — no per-call snapshot HTTP, no
    /// client-side selection/prune/check logic (`docs/goal/ui/backups.md`
    /// § Snapshot-list shape / § Where logic lives). Shared by macOS + iOS (the
    /// FaunaKit lift, priority #2); mirrors `devicesMachine`.
    /// `buildBackupsMachine` is the UniFFI free function
    /// (`libs/fauna-ffi/src/backups.rs`).
    ///
    /// Unlike `devicesMachine` this needs no post-build wiring: the machine's
    /// label custody is a construction input the free function derives from the
    /// connection's own keypair, so a reader that holds the key renders a sealed
    /// set's `snapshot-detail-files` rather than an empty list — the bug
    /// shape web's chunk had to be handed the owner secret to avoid.
    public func backupsMachine(observer: BackupsObserver) async throws -> BackupsMachine {
        buildBackupsMachine(nest: try await ensureNestConnected(), observer: observer)
    }

    /// Build the page-level **Media** content-plane machine
    /// (`libs/fauna-media-machine` via UniFFI) over the shared WS-RPC connection —
    /// the cross-set all-media browse + the `media-sort-select` /
    /// `media-folder-filter` / `media-view-toggle` view state + the
    /// `upload`/`delete` content gestures (`docs/goal/ui/media.md` § State & data
    /// shape — observer-driven rendering off the shared snapshot). Shared by macOS
    /// + iOS (the FaunaKit lift, priority #2); mirrors `devicesMachine`.
    /// `buildMediaMachine` is the UniFFI free function (`libs/fauna-ffi/src/media.rs`).
    ///
    /// Post-build wiring, exactly one call: `wireMediaFollowedFolders` — the
    /// twin of the Devices page's `wireDevicesFollowedFolders` and of wasm's
    /// `setFollowedMediaSource` — injects the followed public folders as browse
    /// SCOPES (`ui/media.md` § Followed public folders). It had to be
    /// hand-written on the Rust side because `set_followed_media_source` takes
    /// an `Arc<dyn FollowedMediaSource>`, which is not a UniFFI-expressible
    /// argument, so unlike `selectFollowedScope` / `downloadFollowed` — carried
    /// across for free by the impl-level `#[uniffi::export]` — a binding regen
    /// alone would never have produced it.
    ///
    /// ⚠ **Skip it and `MediaPageSnapshot.followed` is permanently empty**, so
    /// `media-folder-filter` offers no followed scope and both gestures have
    /// nothing to act on. Best-effort on the secret (pre-login ⇒ no followed
    /// scopes, never a page error), and it reads the same one implementation
    /// and one availability cache the Devices page does.
    public func mediaMachine(observer: MediaObserver) async throws -> MediaMachine {
        let nest = try await ensureNestConnected()
        let machine = buildMediaMachine(nest: nest, observer: observer)
        if let secret {
            try? wireMediaFollowedFolders(
                media: machine, nest: nest, ownerSecret: hex_to_data(secret))
        }
        return machine
    }

    /// Build the **Community labelers** catalog machine
    /// (`libs/fauna-labeler-catalog-machine` via UniFFI) over the shared WS-RPC
    /// connection — backs both the Personalization home's subscribed-labelers
    /// facet and the standalone labeler-catalog page (content-moderation-and-
    /// ranking.md § Tier-3 community models). Shared by macOS + iOS (the
    /// FaunaKit lift, priority #2); mirrors `mediaMachine`.
    /// `buildLabelerCatalogMachineWithGrants` is the UniFFI free function
    /// (`libs/fauna-ffi/src/labeler_catalog.rs`); the actor secret gives it the
    /// grant seams — subscribing a `wasm` mail labeler mints its per-labeler
    /// grant, unsubscribing revokes it. Throws if no secret is set (the
    /// `mailSettingsMachine` shape).
    public func labelerCatalogMachine(observer: LabelerCatalogObserver) async throws -> LabelerCatalogMachine {
        guard let secret else { throw APIError.ffiError("No secret for labeler-catalog machine") }
        let nest = try await ensureNestConnected()
        return try buildLabelerCatalogMachineWithGrants(
            nest: nest, secret: hex_to_data(secret), observer: observer
        )
    }

    /// Build the **dual-rail** conversations session (FaunaMls + SMTP backends)
    /// over the shared WS-RPC connection — `FfiNestClient.conversationsSession`.
    /// The app layer calls this on login and hands the result to
    /// `ConversationsVM.activate`, which is what makes the conversations-page Send
    /// (`dm-send-button`) issue a real RPC instead of the old no-op. `selfAddress`
    /// is the logged-in `<handle>@<domain>`; `selfSecretHex` is the actor secret
    /// (also primes `self.secret` for the WS connection, since the silent-challenge
    /// launch path never sets it). Shared by macOS + iOS (the FaunaKit lift).
    ///
    /// `manager` is the **live** manager the app is already observing
    /// (`ConversationsVM.manager`). The session is built *over* it
    /// (`FfiNestClient.conversationsSessionOverManager` → shared-Rust
    /// `ConversationsSession::from_manager`, linux's shape), so the real FaunaMls +
    /// SMTP rails are registered onto the manager the UI already renders from.
    /// Passing it is what lets `ConversationsVM.activate` stop *swapping* the
    /// manager — a swap silently discarded every thread the old manager held
    /// (e2e-injected threads vanished the instant the real session activated).
    ///
    /// `deviceIdHex` is this client's stable sync device id (`FaunaClient.deviceId`
    /// — the same one the Devices roster and `taskDelegationView` take). It seats
    /// this login at the advisory `index` lease so the Task-delegation row names
    /// this box as builder-of-record instead of reading Waiting-while-running
    /// (`participants.md` § Coordination primitive → *The `index` kind under the
    /// lease*). **macOS only, and the split lives here rather than at the four call
    /// sites**: iOS queries the synced index without ever building it (the ratified
    /// build-vs-query split, `content-index.md` § Where the index is built), so it
    /// has nothing to coordinate — the same macOS/iOS line
    /// `TaskDelegationVM.forThisBuild` already draws for the picker, drawn once in
    /// shared FaunaKit so the two app shells stay identical (priority #1).
    /// `predecessorBackupKeys` — resolve ONCE post-auth via THIS session's
    /// `FaunaClient.resolvedPredecessorBackupKeys()` — never the class-level
    /// `activeActorIdHex`, which a switch racing this build can move out from
    /// under it — and pass the same list here and to
    /// `syncAgentProvisioner` (`succession-aftermath.md` § Re-key scope owns
    /// the `__mls` re-seal this feeds); empty for every identity that never
    /// succeeded, which costs nothing. No default: every caller must resolve
    /// and pass its own actor's list explicitly, so omitting it is a compile
    /// error rather than a silent empty one.
    public func conversationsSession(
        manager: ConversationsManager, selfAddress: String, selfSecretHex: String,
        deviceIdHex: String, predecessorBackupKeys: [Data]
    ) async throws -> ConversationsSession {
        // Adopt the actor FIRST, synchronously, before consulting the cache
        // below. This used to run inside the Task below, AFTER the
        // guard had already decided whether to reuse `conversationsSessionTask`
        // — so a call for a DIFFERENT actor than the one already cached
        // returned the OUTGOING actor's fully-resolved session
        // unconditionally. Also primes `self.secret` for
        // `ensureNestConnected()` — the silent-challenge launch path
        // authenticates statelessly and never sets it — same effect as
        // before, just earlier.
        adoptActor(selfSecretHex)
        // Reuse an in-flight build rather than racing it: the app layer fires this
        // from a detached `Task` at login and does NOT await it before returning
        // control (`FaunaMacApp`/`FaunaApp`'s launch glue is intentionally
        // fire-and-forget so a slow/failed build never blocks the launch). A caller
        // that reaches `awaitSharedConversationsSession()` before that Task finishes
        // — a fast page load right after login, or a fast e2e `set_state` login
        // whose HTTP ack does not wait for this either — awaits the SAME `Task`
        // instead of racing a still-nil cache (the pre-fix bug: a hard-coded
        // "Conversations session not active" on a call that arrived microseconds
        // too early, in production as much as under e2e). Reaching this line at
        // all means `adoptActor` above did not just clear it — same actor as
        // whichever call started this task.
        if let task = conversationsSessionTask { return try await task.value }
        // Captured AFTER `adoptActor`, so a switch landing while this task is
        // still building is visible below — nil-ing `conversationsSessionTask`
        // in `adoptActor` cannot stop a build already past its await from
        // writing the outgoing actor's result into the cache.
        let stillThisActor = sameActorSince()
        let task = Task<ConversationsSession, Error> {
            let dbPath = Self.conversationsMlsDbPath(selfSecretHex: selfSecretHex)
            try? FileManager.default.createDirectory(
                atPath: (dbPath as NSString).deletingLastPathComponent,
                withIntermediateDirectories: true
            )
            return try await ensureNestConnected().conversationsSessionOverManager(
                manager: manager,
                selfAddress: selfAddress,
                selfSecret: hex_to_data(selfSecretHex),
                mlsDbPath: dbPath,
                indexLeaseDevice: Self.indexLeaseDevice(deviceIdHex: deviceIdHex),
                predecessorBackupKeys: predecessorBackupKeys,
                // The recording device — the same id `serveFolderWebdav` passes
                // its walk, on iOS too — so the launch resume finishes an
                // interrupted served-set re-seal (`webdav-server.md` § Key model (c)).
                recordingDevice: deviceIdHex.isEmpty ? nil : deviceIdHex
            )
        }
        conversationsSessionTask = task
        let session = try await task.value
        guard stillThisActor() else {
            // Deliberately does NOT null `conversationsSessionTask`/`Cache`: by
            // now they may already hold the INCOMING actor's own build (mirrors
            // web's `conversations.ts` — same shape, same reason).
            throw APIError.ffiError("conversations session: actor changed while building")
        }
        // Cache the ONE per-actor session so the folders Sharing flow reuses
        // THIS instance (single MlsEngine per mls_state.db) rather than building a
        // second engine racing the same SQLite file — the constraint documented in
        // `libs/fauna-ffi/src/folders_author.rs` § "ONE MlsEngine per
        // mls_state.db". Login builds this first (`FaunaMacApp`/`FaunaApp`), so it
        // is warm before Settings → Folders is reachable
        // (`awaitSharedConversationsSession()`) for anyone who arrives after it
        // finishes; `conversationsSessionTask` covers anyone who arrives before.
        conversationsSessionCache = session
        return session
    }

    /// This build's `index`-lease seat, as the FFI factory wants it: the raw 32
    /// device-id bytes on macOS, `nil` on iOS.
    ///
    /// Two reasons this is a function and not `hex_to_data(deviceIdHex)` inline:
    ///
    /// 1. **iOS seats nothing.** It queries the synced index and never builds one
    ///    (`content-index.md` § Where the index is built), so a seat there would
    ///    heartbeat a phone as the runner-of-record for work it does not do.
    /// 2. **A malformed id must cost coordination, never the session.** Shared Rust
    ///    *fails the call* on a wrong-length id — deliberately, so a real app bug
    ///    cannot hide as a silently-unseated builder — which means an unexpected
    ///    `FaunaClient.deviceId` (empty, truncated, an e2e session patch that never
    ///    set one) would take the whole conversations rail down with it: no chat, no
    ///    mail receive. Screening here keeps the failure proportional, and matches
    ///    tui/linux, where no device id simply yields no seat and the builder runs
    ///    uncoordinated (`conv_backend.rs`: "losing the *coordination* is the cheap
    ///    half").
    private static func indexLeaseDevice(deviceIdHex: String) -> Data? {
        #if os(macOS)
        let bytes = hex_to_data(deviceIdHex)
        return bytes.count == 32 ? bytes : nil
        #else
        return nil
        #endif
    }

    /// The live conversations session cached by `conversationsSession()` at login.
    /// The owner-side folder Sharing flow (`shareFolder` / `removeFolderMember`)
    /// reuses it so the MLS-group ops ride the SAME per-actor `MlsEngine` as the
    /// chat rail (never a second engine on the same `mls_state.db`).
    private var conversationsSessionCache: ConversationsSession?
    /// The in-flight (or completed) build `Task` `conversationsSession()` started —
    /// `nil` only when conversations was never activated at all (e.g. the e2e mock
    /// backend, `FaunaE2E.realConversations == false`, which never calls
    /// `conversationsSession()`). Kept separately from `conversationsSessionCache` so
    /// `awaitSharedConversationsSession()` can distinguish "never started" (throw
    /// immediately, matching the pre-fix behavior) from "started but not finished yet"
    /// (await it) without re-deriving a `Task` from a plain optional value.
    private var conversationsSessionTask: Task<ConversationsSession, Error>?
    private func awaitSharedConversationsSession() async throws -> ConversationsSession {
        if let conversationsSessionCache { return conversationsSessionCache }
        guard let conversationsSessionTask else {
            throw APIError.ffiError("Conversations session not active — cannot share folder")
        }
        return try await conversationsSessionTask.value
    }

    /// Best-effort access to the shared per-actor conversations session for callers
    /// outside APIClient (the moderation-queue local-detection reader,
    /// `ModerationQueueVM` — `moderation.md` § Layout & flow) — mirrors
    /// `devicesMachine()`'s own best-effort `try? await awaitSharedConversationsSession()`.
    /// `nil` when no session is active yet (not fatal — the caller degrades to the
    /// server rows alone, matching linux's `active_session()` → `None`).
    public func sharedConversationsSession() async -> ConversationsSession? {
        try? await awaitSharedConversationsSession()
    }

    /// Build the conversations-rail **drafts autosync** (`FfiDraftsSync` over the
    /// `fauna.drafts.{get,put}` kinds) on the shared WS-RPC connection — the
    /// stateful launch-gate + last-saved-baseline wrapper the app layer hands to
    /// `ConversationsVM.activate`, which restores the owner's persisted drafts on
    /// launch and autosaves compose edits. The owner `BackupKey` (the seal key, not
    /// a signing key — drafts are owner-only) is derived from the live connection's
    /// identity inside the wrapped `DraftsClient`, so no secret is re-passed. Build
    /// once per session and hold the handle (a fresh one re-closes the launch gate).
    /// Mirrors the android `client.draftsSync(rail)` seam
    /// (`docs/goal/behavior/file-sync.md` § Drafts Sync). Shared by macOS + iOS.
    public func draftsSync(rail: String) async throws -> FfiDraftsSync {
        try await ensureNestConnected().draftsSync(rail: rail)
    }

    /// Build the **events-rail** draft autosync, typed rather than raw bytes —
    /// the events twin of `draftsSync(rail:)` above (`rail = "events"`,
    /// pre-bound). The Events page has no manager to hold the canonical
    /// encoding on any app, so this face carries the record itself
    /// (`libs/fauna-ffi::event_drafts`); build once per session and hold the
    /// handle, exactly like `draftsSync`.
    public func eventDrafts() async throws -> FfiEventDraftsSync {
        try await ensureNestConnected().eventDrafts()
    }

    /// Build the shared **stateful Feed manager** (`fauna_feed::FeedManager` via
    /// the `FfiFeedManager` UniFFI façade) over the shared WS-RPC connection.
    /// `FeedVM.configure` calls this on entering the Feed page and renders the page
    /// entirely from `manager.snapshot()` — the FaunaKit twin of `conversationsSession`
    /// (priority #1/#2; `docs/goal/ui/feed.md` § State & data shape). `secretHex` is
    /// the actor's 32-byte ed25519 signing secret (needed to build + sign posts on
    /// `submit_post`); also primes `self.secret` for the silent-challenge launch path,
    /// exactly as `conversationsSession` does.
    public func feedManager(secret secretHex: String) async throws -> FfiFeedManager {
        // `adoptActor` rather than the old `if secret == nil` priming: it primes
        // the same way on a fresh client, but ALSO drops a cached connection
        // belonging to a different actor, so the manager's keypair and the socket
        // its writes are attributed to can never disagree (see `adoptActor`).
        adoptActor(secretHex)
        return try await ensureNestConnected().feedManager(secret: hex_to_data(secretHex))
    }

    /// Build the shared **stateful Search manager** (`fauna_client_search::SearchManager`
    /// via the `FfiSearchManager` UniFFI façade) over the shared WS-RPC connection —
    /// the `feedManager` twin for the Search page (`docs/goal/ui/search.md` § State &
    /// data shape). `SearchVM.configure` calls this and renders the page entirely from
    /// `manager.snapshot()`. Takes no secret — unlike `feedManager` — because searching
    /// signs nothing.
    public func searchManager() async throws -> FfiSearchManager {
        try await ensureNestConnected().searchManager()
    }

    /// Register this login's sealed local search index (backend 2) on `manager`, so
    /// the Search page merges local rows with the nest's instead of running nest-only
    /// (`search.md` § State & data shape — the local/nest merge). Returns whether an
    /// arm was registered; `false` is a **normal state, not an error** — no
    /// conversations session yet, or this actor has no mail. Call after both
    /// `conversationsSession(...)` and `searchManager()` have run; safe to retry (e.g.
    /// on reconnect) since a `false` result means nothing was attached yet.
    public func attachLocalSearchIndex(manager: FfiSearchManager) async -> Bool {
        guard let nest = try? await ensureNestConnected() else { return false }
        return await nest.attachLocalSearchIndex(manager: manager)
    }

    // MARK: - Critical alerts (critical-alerts.md § Mechanism)

    /// Run the critical-alerts session-start + periodic re-sweep loop
    /// (`libs/fauna-client-alert-sweep::run_alert_sweep_loop`, over
    /// `libs/fauna-ffi`'s `runCriticalAlertSweepLoop` free function) — the one
    /// shared entry point every feeder rides with no page of its own
    /// (critical-alerts.md § Mechanism → *Who runs the detector* + *How
    /// often*; already adopted by tui/linux/web/android/windows). The loop
    /// only returns once the identity tears down (`CriticalAlerts.clearAll`),
    /// so the caller must launch this on a process-lifetime `Task`, never a
    /// view-scoped one — mirrors `subscribeReconnects()`'s ensure-connected
    /// shape.
    public func startCriticalAlertSweepLoop() async throws {
        guard let secret else { throw APIError.ffiError("No secret for critical-alert sweep") }
        try await runCriticalAlertSweepLoop(nest: try await ensureNestConnected(), secret: hex_to_data(secret))
    }

    /// One sweep pass (`runCriticalAlertSweep`) — the same-identity re-establish
    /// arm of `CriticalAlertsHost.startSweepLoop`, where the identity's loop is
    /// already running and a second one would stack.
    public func runCriticalAlertSweepOnce() async throws {
        guard let secret else { throw APIError.ffiError("No secret for critical-alert sweep") }
        try await runCriticalAlertSweep(nest: try await ensureNestConnected(), secret: hex_to_data(secret))
    }

    /// The identity a critical-alert sweep loop covers — the actor AT a nest, so
    /// a re-point at another nest gets a loop sweeping that nest. `nil` with no
    /// secret (no identity to sweep for).
    public var sweepIdentityKey: String? {
        guard let secret, let actor = try? actor_id_from_secret(secret) else { return nil }
        return "\(actor)@\(nodeUrl.absoluteString)"
    }

    // MARK: - Mail-admin machines (shared FaunaKit, macOS + iOS)
    //
    // The user-tier `mail-settings` page and the admin-tier `admin-bridges-pending`
    // / `admin-dns` / `admin-mail` / `admin-aliases` pages are dumb renderers of a
    // shared Rust state machine (priority #2). APIClient owns the WS-RPC connection
    // (+ actor secret + node URL) the `build_*_machine` constructors need, so it
    // vends each machine here rather than exposing those — same idiom as
    // `conversationsSession()` above. Machines live in
    // `libs/fauna-client-{mail-settings,dns}`, surfaced via
    // `libs/fauna-ffi/src/mail_admin.rs`.

    /// Build the user-tier `MailSettingsMachine` (mail-settings page). Needs the
    /// actor secret (to sign submission tokens + key the account plane) and the
    /// deployment node URL (to derive MUA connection details), both private to
    /// APIClient — so it vends the machine. Throws if no secret is set (the page
    /// is only reachable post-login).
    public func mailSettingsMachine() async throws -> MailSettingsMachine {
        guard let secret else { throw APIError.ffiError("No secret for mail-settings machine") }
        let nest = try await ensureNestConnected()
        return try buildMailSettingsMachine(
            nest: nest, secret: hex_to_data(secret), nodeUrl: nodeUrl.absoluteString
        )
    }

    /// Build the `ConnectedAppsMachine` (the Settings → Connected apps page,
    /// `docs/goal/ui/connected-apps.md`) over this session's nest connection.
    /// `mail` is the session's own Mail & Calendar machine: the mail app
    /// passwords are rows of the roster, read, revoked and revealed through it
    /// (`nil` builds a roster without them). Every call the machine makes is a
    /// plain authenticated nest request, so it needs no secret — except an
    /// approve naming a `fauna:records:` scope, which mints the app's
    /// consent-time grant under the actor's own seed; that wiring is applied
    /// here and is best-effort (a build without the account runtime refuses it,
    /// and such an approve is then refused rather than resolved keyless).
    /// A fresh machine per call: the page starts every visit unread.
    public func connectedAppsMachine(
        observer: ConnectedAppsObserver, mail: MailSettingsMachine?
    ) async throws -> ConnectedAppsMachine {
        let machine = buildConnectedAppsMachine(
            nest: try await ensureNestConnected(), observer: observer, mail: mail
        )
        if let secret {
            try? wireConnectedAppsConsentGrant(machine: machine, secret: hex_to_data(secret))
        }
        return machine
    }

    /// The post-claim serving enablement (`onboarding.md` § 3b *Mechanism*) —
    /// the ONE shared-Rust call `MailEnableGlue.applyPostClaimServingEnablement`
    /// makes with the four intents read off the wizard's `LoggedIn` handoff,
    /// replacing the mail/CalDAV/CardDAV/WebDAV per-step firing (priority #2).
    /// Same `secret` + `nodeUrl` idiom as `mailSettingsMachine()` above; the
    /// shared step resolves `am_i_admin` itself over `nest`, so this vends no
    /// admin flag. Each step logs its own failure nest-side, so this only
    /// throws on an undecodable `secret`.
    public func applyPostClaimServingEnablement(
        email: Bool, caldav: Bool, carddav: Bool, webdav: Bool
    ) async throws {
        guard let secret else { throw APIError.ffiError("No secret for post-claim serving enablement") }
        try await FaunaFFISwift.applyPostClaimServingEnablement(
            nest: try await ensureNestConnected(), secret: hex_to_data(secret),
            nodeUrl: nodeUrl.absoluteString,
            email: email, caldav: caldav, carddav: carddav, webdav: webdav
        )
    }

    /// Mark a received conversation/mail message as spam — the live `Insert`
    /// consumer (mail-spam.md § Encrypted-mode interaction + § Wire shapes
    /// `put_spam_model` `history_op`; the `dm-message-mark-as-spam-button` gesture,
    /// gated `!is_own`). Trains the sealed tier-1 model over the retained decrypted
    /// `body` **and** writes a sealed `spam_training_history` row via
    /// `train_spam_model_client_mail` (the `mail-spam` page renders it + offers
    /// per-row undo) — mirrors linux `FaunaClient::mark_message_spam`. Fire-and-
    /// forget, silent on any failure / `ServerPath` degrade: a conversation message
    /// is client-only encrypted content the nest can't read, so there is **no**
    /// server-train fallback (unlike a moderation-queue server-row correction).
    /// `messageId` rides as the row's **opaque** reference (its UTF-8 bytes, never
    /// decoded nest-side); `subject` falls back to a body snippet inside the shared
    /// façade when absent. The mailbox is `INBOX` — a received conversation message
    /// has no IMAP mailbox; it is display metadata on the sealed row only.
    public func markMessageAsSpam(messageId: String, body: String, subject: String?) async {
        guard !body.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        guard let machine = try? await mailSettingsMachine() else { return }
        _ = try? await machine.trainSpamModelClientMail(
            text: body,
            isSpam: true,
            messageId: Data(messageId.utf8),
            mailbox: "INBOX",
            subject: subject ?? ""
        )
    }

    /// Build the admin `BridgeApprovalMachine` (`admin-bridges-pending` page).
    /// Admin-class — needs only the connection handle.
    public func bridgeApprovalMachine() async throws -> BridgeApprovalMachine {
        buildBridgeApprovalMachine(nest: try await ensureNestConnected())
    }

    /// Build the admin `DnsManagementMachine` (`admin-dns` page — the **full**
    /// surface: record matrix + live verify, managed-mode credential store +
    /// publish, and the TLS-cert lifecycle). Uses the **credentialed** builder
    /// (mirrors linux's persistent `dns_machine()`): the actor secret loads the
    /// `fauna.state.dns` tip-sealed DNS-provider credential + ACME account
    /// per-operation (the nest never sees the key — dns-management.md § Where the
    /// credential lives). Throws if no secret is set (the page is admin-only,
    /// post-login). The read/verify surface is a subset, so this strictly
    /// supersedes the old read-only `buildDnsManagementMachine`.
    public func dnsManagementMachine() async throws -> DnsManagementMachine {
        guard let secret else { throw APIError.ffiError("No secret for DNS-management machine") }
        return try buildDnsManagementMachineWithCredentials(
            nest: try await ensureNestConnected(), secret: hex_to_data(secret)
        )
    }

    /// Memoized handle for `atprotoSettingsMachine(observer:)` — built ONCE
    /// per `APIClient` (i.e. per session, not per page-visit). Without this,
    /// each fresh `AtprotoSettingsView`/`AtprotoSettingsVM` (view-local
    /// `@State`, rebuilt on every navigation into Settings → AT Protocol) called
    /// `buildAtprotoSettingsMachine` again, silently resetting the S4-C
    /// custody-check's one-convergence debounce on every re-entry — the same
    /// per-visit-rebuild bug android hit and fixed via `AtprotoSettingsHost.kt`
    /// (confirmed present on apple, `critical-alerts.md`'s per-visit-
    /// rebuild check).
    private var cachedAtprotoMachine: AtprotoSettingsMachine?
    private let atprotoObserverFanout = AtprotoObserverFanout()

    /// Build (once) the **Bluesky** settings-page machine
    /// (`libs/fauna-atproto-settings-machine` via UniFFI) over the shared
    /// WS-RPC connection — the whole `atproto` page (integration-depth
    /// selector, transition card, hosted panel, F1 login-plane groups;
    /// `docs/goal/ui/atproto.md` § State & data shape). Needs the actor
    /// secret: the credential secrets it mints are custodied in `fauna.state.atproto`,
    /// sealed under the BackupKey derived from it (mirrors `mailSettingsMachine()`).
    /// Shared by macOS + iOS (the FaunaKit lift, priority #2).
    ///
    /// The Rust machine binds exactly ONE observer at construction, so a
    /// later page-visit's fresh `observer` cannot re-subscribe directly —
    /// `atprotoObserverFanout` is the one, permanent observer the machine is
    /// actually built with, and simply retargets (weakly) to whichever
    /// caller's `observer` is current. A page a user has since left just
    /// misses the callback (its box has deallocated), matching the "weak —
    /// must never keep a dismissed page's VM alive" rule this codebase
    /// already applies to `AtprotoSettingsVM.liveInstanceForTest`.
    public func atprotoSettingsMachine(observer: AtprotoSettingsObserver) async throws -> AtprotoSettingsMachine {
        atprotoObserverFanout.target = observer
        if let cachedAtprotoMachine { return cachedAtprotoMachine }
        guard let secret else { throw APIError.ffiError("No secret for ATProto settings machine") }
        // See `seededCaldavClient()`'s identical guard: the only await is
        // `ensureNestConnected()`'s connect, and a switch landing during it
        // is not stopped by `adoptActor` nil-ing the handle.
        let stillThisActor = sameActorSince()
        let machine = try buildAtprotoSettingsMachine(
            nest: try await ensureNestConnected(), secret: hex_to_data(secret), observer: atprotoObserverFanout
        )
        guard stillThisActor() else {
            throw APIError.ffiError("ATProto settings machine: actor changed while connecting")
        }
        cachedAtprotoMachine = machine
        return machine
    }

    // MARK: - Backup-destination management (shared FaunaKit, macOS + iOS)
    //
    // The cross-location destination CRUD on the Backups page (`backups.md`
    // § Manage backup destinations) is a dumb renderer of the thin FFI free fns
    // in `libs/fauna-ffi/src/backup_destinations.rs` — **no machine** (priority
    // #2/#1: the desktop apps drive the plane write + the mutate helpers
    // directly, the native UniFFI apps consume these free fns; a machine would
    // force re-opening the landed linux/web/android thin glue). Each fn sequences
    // the same shared logic (resolve identity → mutate the `fauna.state.backup` rows →
    // the plane write) and returns the freshly-persisted list. APIClient owns
    // the WS-RPC handle + actor secret the fns need, so it vends them — same idiom
    // as `mailSettingsMachine()`.

    /// List the owner's configured backup destinations (the
    /// `backup-destination-status-row`s). Throws if no secret (page is post-login).
    public func listBackupDestinations() async throws -> [FfiBackupDestinationView] {
        guard let secret else { throw APIError.ffiError("No secret for backup destinations") }
        return try await backupDestinationsList(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret))
    }

    /// Resolve + record a new destination; returns the updated list. A blank
    /// `name` defaults to the destination's handle domain (the fn handles it).
    public func addBackupDestination(url: String, name: String) async throws
        -> [FfiBackupDestinationView]
    {
        guard let secret else { throw APIError.ffiError("No secret for backup destinations") }
        return try await backupDestinationAdd(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret),
            url: url, name: name)
    }

    /// Rename / change a destination's URL; returns the updated list. A URL change
    /// pointing at a *different* nest throws `FfiError.General` carrying the stable
    /// token `backup-destination-edit-different-nest` (the VM maps it to the i18n
    /// `backups.backup_destination_edit_different_nest`).
    public func editBackupDestination(id: String, url: String, name: String) async throws
        -> [FfiBackupDestinationView]
    {
        guard let secret else { throw APIError.ffiError("No secret for backup destinations") }
        return try await backupDestinationEdit(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret),
            id: id, url: url, name: name)
    }

    /// Drop a destination; returns the updated list. A plain config edit — the
    /// coordinator reconciles the offsite deregistration on its next pass, so it
    /// is crash-recoverable (`backups.md` § Remove).
    public func removeBackupDestination(id: String) async throws -> [FfiBackupDestinationView] {
        guard let secret else { throw APIError.ffiError("No secret for backup destinations") }
        return try await backupDestinationRemove(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret), id: id)
    }

    /// **Keep** a destination an identity succession carried across — the
    /// owner answering "I recognise this" to `backup-destination-unattested-mark`
    /// (`succession-aftermath.md` § Re-key scope → *Adjudicating what the
    /// aftermath carries across*). Records the verdict at rest through the shared
    /// CAS path and returns the re-read list, whose `unattested` is the at-rest
    /// truth. Remove stays `removeBackupDestination` — no second removal path.
    public func keepBackupDestination(id: String) async throws -> [FfiBackupDestinationView] {
        guard let secret else { throw APIError.ffiError("No secret for backup destinations") }
        return try await backupDestinationKeep(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret), id: id)
    }

    /// Enroll THIS device as a client-device custodian destination — the third
    /// destination kind (`docs/goal/ui/backups.md` § Third destination kind —
    /// client device as custodian). No address at all: mints its own
    /// `destination_id`, no `NestBackupKey` grant, no resolve round-trip.
    /// `capacityCapBytes: nil` is a real choice (uncapped), never a substituted
    /// default. Returns the updated list, same idiom as the accessors above.
    public func enrollCustodianBackupDestination(
        custodianDeviceId: String, name: String, capacityCapBytes: UInt64?
    ) async throws -> [FfiBackupDestinationView] {
        guard let secret else { throw APIError.ffiError("No secret for backup destinations") }
        return try await backupDestinationEnrollCustodian(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret),
            custodianDeviceId: custodianDeviceId, name: name, capacityCapBytes: capacityCapBytes)
    }

    // MARK: - Host-address acquisition (domains-and-tls-bootstrap.md § Host-address acquisition)

    /// Report the nest's public IP (`fauna.dns.set_host_address`) so ACME HTTP-01
    /// gates on the strong resolve-check. Thin accessor over the shared
    /// `report_host_address` free fn — all classification (public vs.
    /// private/LAN/`.local`) and the never-publish-a-private-address safety live
    /// in the shared FFI fn (priority #2); apple adds no logic. Never throws
    /// (nest-side refusal/fault surfaces as `.failed`, not an exception) — the
    /// `throws` here is only `ensureNestConnected()`'s.
    public func reportHostAddress() async throws -> FfiHostAddressOutcome {
        return await FaunaFFISwift.reportHostAddress(nest: try await ensureNestConnected())
    }

    // MARK: - Deployment-seed custody leg (box-recovery.md § The plane-era recovery floor)

    /// The custody leg's post-auth entry — thin accessor over the shared
    /// `self_heal_deployment_seed_custody` free fn (capture at store-ready and the
    /// co-admin hand-off self-heal both live in shared Rust; the caller only
    /// projects the outcome onto the recovery-custody banner).
    public func selfHealDeploymentSeedCustody() async throws -> FfiDeploymentSeedSelfHeal {
        guard let secret else { throw APIError.ffiError("No secret for the deployment-seed custody leg") }
        return try await FaunaFFISwift.selfHealDeploymentSeedCustody(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret),
            appDataDir: Self.recoveryConfigDataDir)
    }

    // MARK: - Deployment-seed rotation (admin.md § N Nest → Deployment-identity rotation, apps row 245)

    /// The current admin roster, folded into the rotation confirm surface
    /// (`admin-nest-seed-rotate-*`) — thin accessor over the shared
    /// `seed_rotate_roster` free fn (`fauna.admin.admins.list` + a best-effort
    /// `users.get` per member). Read-only: nothing is rotated by arming.
    public func seedRotateRoster() async throws -> FfiSeedRotationConfirmView {
        return try await FaunaFFISwift.seedRotateRoster(nest: try await ensureNestConnected())
    }

    /// Drive the deployment-seed rotation ceremony (mint → custody → dispatch
    /// → mark) via the shared `rotate_deployment_seed` free fn. **Outlives its
    /// click by design** — the committed rotation tears down this connection
    /// (WS 1001) and the drive's own marking step reconnects, so callers must
    /// not bound this by an agent-command reply budget (mirrors tui's
    /// `PageOp::outlives_click`). The successor seed's custody is the shared
    /// ceremony's own step — nothing is fanned out by the caller.
    public func rotateDeploymentSeed() async throws -> FfiSeedRotationResult {
        guard let secret else { throw APIError.ffiError("No secret for seed rotation") }
        return try await FaunaFFISwift.rotateDeploymentSeed(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret))
    }

    // MARK: - Declared region (admin.md § N Nest → Declared region, row 146)

    /// `fauna.admin.region.get` folded through the shared `admin_region_view` —
    /// thin accessor over the `admin_region_status` free fn, mirroring
    /// `seedRotateRoster` above. Every rendering decision (status/authority/
    /// staleness/can-withdraw wording) is already made server-side; this hop
    /// decides nothing.
    public func adminRegionStatus() async throws -> FfiAdminRegionView {
        try await FaunaFFISwift.adminRegionStatus(nest: try await ensureNestConnected())
    }

    /// `fauna.admin.region.set` — declare/re-declare (`region` non-nil,
    /// already validated by `adminParseRegionCode`) or withdraw (`region ==
    /// nil`). Re-validates regardless of caller validation, since this is the
    /// only path that reaches the wire.
    public func adminSetRegion(_ region: String?) async throws {
        try await FaunaFFISwift.adminSetRegion(nest: try await ensureNestConnected(), region: region)
    }

    /// `fauna.oauth.issuer_key_status` folded through the shared
    /// `issuer_key_view` (authorization-server.md § The issuer) — the one read
    /// `admin-nest-oauth-section` paints its key rows from. Throws on failure:
    /// the caller words it on the section's own reason line, never on the
    /// page (any read failure, transport or nest, leaves the rest of
    /// admin-nest painting). A free function for the same reason
    /// `adminRegionStatus` is one.
    public func adminIssuerKeyStatus() async throws -> FfiIssuerKeyView {
        try await FaunaFFISwift.adminIssuerKeyStatus(nest: try await ensureNestConnected())
    }

    /// `fauna.oauth.rotate_issuer_key` — `admin-nest-oauth-rotate-button`,
    /// dispatched AND worded by the shared fold: the returned sentence is the
    /// section's verdict, success or failure (the FFI face never throws; only
    /// reaching the nest here can).
    public func adminRotateIssuerKey() async throws -> LocalizedText {
        await FaunaFFISwift.adminRotateIssuerKey(nest: try await ensureNestConnected())
    }

    /// `admin-nest-oauth-confirm-button` — exactly `arm`'s kind
    /// (`fauna.oauth.force_rotate_issuer_key` /
    /// `fauna.oauth.force_rotate_session_secret`), dispatched and worded like
    /// `adminRotateIssuerKey`. The caller disarms before calling.
    public func adminForceRotateIssuer(arm: FfiIssuerForcedArm) async throws -> LocalizedText {
        await FaunaFFISwift.adminForceRotateIssuer(nest: try await ensureNestConnected(), arm: arm)
    }

    /// App data directory the shared deployment-seed FFI fns still take
    /// (`selfHealDeploymentSeedCustody`, `deploymentSeedsLocal`, the succession
    /// aftermath) — the box list itself now reads from the device's own store
    /// (`box-recovery.md` § The plane-era recovery floor).
    /// The app's **own** consent domain (`SyncStateDir`'s two-domain doc): on
    /// macOS the user-domain home `~/Library/Application Support/Fauna` — the
    /// replica is the app's file alone, and the container is the sandboxed
    /// extension's root only since 2026-08-25 (a replica that sat there on a
    /// dev box is simply re-converged from the nest; nothing shipped it); on
    /// iOS the app-group container, the app's only domain. iOS e2e keeps the
    /// in-sandbox Application Support dir — the container is machine-global state a
    /// test launch must never touch (testing.md § Cross-app e2e conventions
    /// point 10).
    static var recoveryConfigDataDir: String {
        #if os(macOS)
            let dir = SyncStateDir.userDomainHome
        #else
            if !FaunaE2E.isActive,
                let container = FileProviderCredentialStore.containerURL()
            {
                return container.path
            }
            let dir = SyncStateDir.appSupportSyncDir.deletingLastPathComponent()
        #endif
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.path
    }

    // MARK: - Muted keywords (moderation.md § Muted keywords; content-moderation-and-ranking.md § Q3)

    /// Read the owner's sealed `fauna.state.moderation` muted-keyword list as
    /// the shared page record — the `muted-words` sub-page rows **and** the
    /// `loaded` bit that gates `muted-word-empty` (`docs/goal/ui/README.md`
    /// § *List pages: loading is not empty*). Thin accessor over the shared
    /// input-free `load_muted_words` free fn; no client-side normalization. A caller that
    /// only wants the list (the conversation collapse) reads `.keywords`.
    public func mutedKeywordsList() async throws -> MutedWordsSnapshot {
        return try await FaunaFFISwift.loadMutedWords()
    }

    /// Replace the owner's muted-keywords list (terms and weights) and persist;
    /// returns the server-normalized page record (trim, drop blanks,
    /// case-insensitive dedupe, weights clamped) — callers re-render from this
    /// return value, never a locally-guessed one.
    ///
    /// ⚠ Whole-list intents only — the page's add/remove buttons go through
    /// the delta pair below: sending the page's list wholesale
    /// clobbers a term another device stored since the page loaded.
    public func mutedKeywordsSet(keywords: [MutedKeyword]) async throws -> MutedWordsSnapshot {
        return try await FaunaFFISwift.saveMutedWords(keywords: keywords)
    }

    /// Add one term as a DELTA against the stored list — the shared seam
    /// re-reads it inside its own CAS update, so a concurrent device's term
    /// survives. Re-adding an existing term is a no-op; the
    /// stored normalization returns.
    public func mutedKeywordsAdd(word: String) async throws -> MutedWordsSnapshot {
        return try await FaunaFFISwift.addMutedWord(word: word)
    }

    /// Remove one term — the delta pair's inverse; removing a term already
    /// gone is a success no-op (convergence, not an error).
    public func mutedKeywordsRemove(word: String) async throws -> MutedWordsSnapshot {
        return try await FaunaFFISwift.removeMutedWord(word: word)
    }

    // MARK: - Trained topic factors (topic-factors.md § Authoring surface & picker)
    //
    // Free-function CRUD over the sealed `fauna.state.personalization` trained-factor
    // registry — the same shape as muted keywords above, not a stateful machine
    // (trained topics has no client-side state beyond the returned row list).
    // Consumed by the Personalization home's Trained-topics facet, the create-feed
    // `feed-factor-select` picker's third source, and the post-card train-target
    // sheet.

    /// List the owner's trained-topic-factor registry rows.
    public func trainedTopicsList() async throws -> [FfiTrainedTopicRow] {
        return try await FaunaFFISwift.listTrainedTopics(nest: try await ensureNestConnected())
    }

    /// Mint a new `topic:<hex>` factor + sealed registry entry named `name`.
    /// Returns the full updated row list — callers re-render from this return
    /// value, never a locally-guessed one (mirrors `mutedKeywordsSet`).
    public func trainedTopicsCreate(name: String) async throws -> [FfiTrainedTopicRow] {
        return try await FaunaFFISwift.createTrainedTopic(nest: try await ensureNestConnected(), name: name)
    }

    /// Rename an existing trained factor's display name — the `topic:<hex>` key
    /// and trained model are untouched.
    public func trainedTopicsRename(id: Data, name: String) async throws -> [FfiTrainedTopicRow] {
        return try await FaunaFFISwift.renameTrainedTopic(
            nest: try await ensureNestConnected(), id: id, name: name)
    }

    /// Toggle the factor's `learn_from_engagement` opt-in (engagement-cues.md §
    /// Layer A) — registry-only; flipping it off stops future weak training
    /// without rewriting what engagement already taught.
    public func trainedTopicsSetLearnFromEngagement(id: Data, on: Bool) async throws -> [FfiTrainedTopicRow] {
        return try await FaunaFFISwift.setTrainedTopicEngagement(
            nest: try await ensureNestConnected(), id: id, on: on)
    }

    /// Delete a trained factor (registry remove + `model.delete`). Compositions
    /// still referencing the key stay valid — the zero-term seam makes an
    /// orphan key inert.
    public func trainedTopicsDelete(id: Data) async throws -> [FfiTrainedTopicRow] {
        return try await FaunaFFISwift.deleteTrainedTopic(nest: try await ensureNestConnected(), id: id)
    }

    /// Publish a trained factor as a tier-3 List labeler (topic-factors.md §
    /// Publishing a trained factor) — the pruned content_id→score map, keyed
    /// to the same factor a rename/delete targets. Returns the published
    /// labeler id/version, not an updated row list: publishing doesn't
    /// mutate the registry row itself.
    public func trainedTopicPublishList(
        factorId: Data, name: String, entries: [FfiPublishEntry]
    ) async throws -> FfiPublishedList {
        guard let secret else { throw APIError.ffiError("No secret for trained topics") }
        return try await FaunaFFISwift.trainedTopicPublishList(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret),
            factorId: factorId, name: name, entries: entries)
    }

    /// Publish a trained factor as a tier-3 Model labeler (topic-factors.md §
    /// Publishing a trained factor, v2) — the scrubbed n-gram vocabulary.
    /// `moreDocs`/`lessDocs` are the corpus counters `scrubCorpusForFactor`
    /// returned, passed through UNSHRUNK regardless of what the review
    /// pruned — they say how many public examples the vocabulary was built
    /// from, which stays true however much of it the caller withheld.
    public func trainedTopicPublishModel(
        factorId: Data, name: String, moreDocs: UInt32, lessDocs: UInt32, ngrams: [FfiPublishNgram]
    ) async throws -> FfiPublishedModel {
        guard let secret else { throw APIError.ffiError("No secret for trained topics") }
        return try await FaunaFFISwift.trainedTopicPublishModel(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret),
            factorId: factorId, name: name, moreDocs: moreDocs, lessDocs: lessDocs, ngrams: ngrams)
    }

    // MARK: - Subscriptions (profile Tiers-tab SELF author management)
    //
    // The profile Tiers-tab SELF sections (subscriptions Slice A, monetization.md
    // § Pillar 1 + profile.md). Pure reads + tier edit/delete/reject ride the thin
    // `FfiSubscriptionsClient` (`FfiNestClient.subscriptions()`); the three
    // encrypted-mode mutations that mint a KeyBlob (create / approve / remove) ride
    // the shared `SubscriptionsAuthor` orchestration over its UniFFI free fns,
    // which need the owner secret (the period-key custody seal). Never re-implement
    // the mint client-side (priority #2). Mirrors linux
    // `apps/fauna-linux/src/views/profile/tiers.rs`.

    /// `fauna.subscriptions.tiers.list` — the calling author's own tiers (§1 "My
    /// tiers"), the authenticated own-read over the thin client.
    public func listSubscriptionTiers() async throws -> [FfiTierItem] {
        try await ensureNestConnected().subscriptions().tiersList()
    }

    /// `fauna.subscriptions.requests.list` — the author's pending subscribe /
    /// unsubscribe requests (§2).
    public func listSubscriptionRequests() async throws -> [FfiPendingRequest] {
        try await ensureNestConnected().subscriptions().requestsList()
    }

    /// `fauna.subscriptions.subscribers.list` — the selected tier's roster (§3).
    public func listSubscribers(tierName: String) async throws -> [FfiSubscriberEntry] {
        try await ensureNestConnected().subscriptions().subscribersList(tierName: tierName)
    }

    /// `fauna.subscriptions.tiers.update` — edit an existing tier (thin; the name
    /// is the server key, not editable). Each `nil` field is left unchanged.
    public func updateSubscriptionTier(
        name: String, rank: UInt32?, description: String?, priceHint: String?,
        paymentUrl: String?, autoApprove: Bool?,
        // nil KEEPS the tier's current asking price — this kind has no clear
        // verb for any of its optional fields (monetization.md § The asking
        // price → Editability).
        askingPriceSats: UInt64? = nil
    ) async throws -> Bool {
        try await ensureNestConnected().subscriptions().tiersUpdate(
            name: name, rank: rank, description: description, priceHint: priceHint,
            paymentUrl: paymentUrl, autoApprove: autoApprove,
            askingPriceSats: askingPriceSats)
    }

    /// `fauna.subscriptions.tiers.delete` — drop a tier by name (thin).
    @discardableResult
    public func deleteSubscriptionTier(name: String) async throws -> Bool {
        try await ensureNestConnected().subscriptions().tiersDelete(name: name)
    }

    /// `fauna.subscriptions.requests.reject` — decline a pending request (thin).
    @discardableResult
    public func rejectSubscriptionRequest(requestId: Int64) async throws -> Bool {
        try await ensureNestConnected().subscriptions().requestsReject(requestId: requestId)
    }

    /// One author-pump tick — resume any crash-staged subscriber removal, then
    /// auto-approve every pending `auto_approve` subscribe request (minting the
    /// covering `KeyBlob` per row), in that order, every time
    /// (`monetization.md` § Pillar 1 → *Where the logic lives*: "An app MUST NOT
    /// re-derive either"). In encrypted mode the nest cannot mint, so even a free
    /// rank-0 **follow** enqueues (`Queued`) until the author's client grants it here
    /// (§ The unifying model, grant path 2). Never throws for a bad tick — each
    /// half's fault comes back inside the returned record; this call itself can
    /// still throw for the surrounding seam (no secret, no connection). Driven by
    /// `SubscriptionsAuthorCadence`, not by a UI gesture.
    public func reconcileSubscriptionsOnce() async throws -> FfiReconcilePass {
        guard let secret else { throw APIError.ffiError("No secret for subscriptions") }
        return try await subscriptionsReconcileOnce(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret))
    }

    /// Create a tier via the encrypted-mode author orchestration: records a fresh
    /// client-held period key in custody, then creates the tier server-side.
    /// Returns whether a row was created.
    @discardableResult
    public func createSubscriptionTier(
        name: String, rank: UInt32, description: String?, priceHint: String?,
        paymentUrl: String?, autoApprove: Bool,
        // The tier-form asking-price input is BORN GATED behind
        // `#if !FAUNA_EXCISE_PAYMENTS` at its one call site, like §§4-5 below:
        // the asking price is the monetization gesture, so a store-safe build
        // renders no price field (ratified 2026-08-14, dynamic-features.md
        // § Platform-family surface excision) and this parameter stays nil
        // there. An unpriced tier is permanently valid: a zap on it stays a tip.
        askingPriceSats: UInt64? = nil
    ) async throws -> Bool {
        guard let secret else { throw APIError.ffiError("No secret for subscriptions") }
        return try await subscriptionsCreateTier(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret),
            name: name, rank: rank, description: description, priceHint: priceHint,
            paymentUrl: paymentUrl, autoApprove: autoApprove,
            askingPriceSats: askingPriceSats)
    }

    /// Approve a pending request via the author orchestration: mints + uploads a
    /// roster-covering KeyBlob, retrying roster/rotation races internally.
    @discardableResult
    public func approveSubscriber(request: FfiPendingRequest) async throws -> FfiApproveReply {
        guard let secret else { throw APIError.ffiError("No secret for subscriptions") }
        return try await subscriptionsApproveSubscriber(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret), request: request)
    }

    /// Remove a subscriber via the author orchestration: rotates the period key,
    /// re-mints over the reduced roster, crash-staged before the upload.
    public func removeSubscriber(tierName: String, subscriberId: Data) async throws {
        guard let secret else { throw APIError.ffiError("No secret for subscriptions") }
        try await subscriptionsRemoveSubscriber(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret),
            tierName: tierName, subscriberId: subscriberId)
    }

    // MARK: - Subscriptions (consumer side — `subscription-settings` page)
    //
    // The Slice-B consumer page (monetization.md § Pillar 1 — "what this user
    // subscribes to across all creators, with unsubscribe"). Both reads are thin
    // pass-throughs to the `FfiSubscriptionsClient` (no minting — the consumer
    // never holds a KeyBlob). Mirrors linux `apps/fauna-linux/src/settings/
    // subscriptions.rs` + android `SubscriptionSettingsVM`.

    /// `fauna.subscriptions.mine.list` — the calling actor's own subscriptions
    /// across **every** creator (active + pending, deduped), the consumer-side
    /// enumeration powering the `subscription-settings` page (distinct from the
    /// per-creator `status.get`). Each row carries the creator's nest-resolved
    /// handle, or `nil` → the page renders the hex actor id.
    public func subscriptionMineList() async throws -> [FfiMineSubscription] {
        try await ensureNestConnected().subscriptions().mineList()
    }

    /// `fauna.subscriptions.unsubscribe` — drop the caller's subscription to
    /// `authorId` (thin). Encrypted mode returns `Queued` (the row stays until the
    /// author commits the removal); the consumer page re-reads `mine.list` after.
    @discardableResult
    public func subscriptionUnsubscribe(authorId: Data) async throws -> FfiUnsubscribeReply {
        try await ensureNestConnected().subscriptions().unsubscribe(authorId: authorId)
    }

    // MARK: - Payment providers (Pillar 3 — monetization.md § Pillars 2+3 — app UX)
    //
    // The author-side provider CRUD (profile Tiers-tab §4) + the buyer-side claim
    // redemption (`subscription-settings`), thin pass-throughs to the shared
    // `FfiPaymentsClient` (`FfiNestClient::payments()`) — priority #2, no local
    // re-derivation of the adapter registry or the entitlement engine. Mirrors
    // linux `apps/fauna-linux/src/views/profile/tiers.rs` (provider CRUD) +
    // `settings/subscriptions.rs` (claim redeem).
    //
    // EXCISED BY `FAUNA_EXCISE_PAYMENTS` (`dynamic-features.md` § Platform-family
    // surface excision — the apple family's compile condition). This half the
    // toolchain would find on its own: a store-safe `FaunaFFI.xcframework` exports
    // no `FfiPaymentsClient` and no `paymentsKnownKinds`, so every line below is a
    // compile error in that flavor. The half it does NOT find is the RENDER —
    // see the same condition in ProfileView / SubscriptionSettingsView.
    #if !FAUNA_EXCISE_PAYMENTS

    /// Every registered payment-provider kind (`fauna-payments` adapter registry) —
    /// the §4 form's kind select enumerates these verbatim, unlocalized (mirrors
    /// linux's raw `DropDown::from_strings`; a kind is an adapter identifier, not
    /// user-facing copy). Pure/sync — no nest round-trip.
    public func paymentProviderKinds() -> [String] {
        paymentsKnownKinds()
    }

    /// `fauna.payments.providers.list` — the author's configured providers (§4).
    /// The nest never echoes the webhook secret back (write-only), so a listed
    /// row never carries one to leak.
    public func listPaymentProviders() async throws -> [FfiProviderItem] {
        try await ensureNestConnected().payments().providersList()
    }

    /// `fauna.payments.providers.set` — create or replace a provider config
    /// (thin). The tier must be one of the author's own (`monetization.md` §
    /// Pillar 3 — "first cut: one mapping per provider").
    @discardableResult
    public func setPaymentProvider(kind: String, webhookSecret: String, tier: String) async throws -> Bool {
        try await ensureNestConnected().payments().providersSet(
            kind: kind, webhookSecret: webhookSecret, tier: tier)
    }

    /// `fauna.payments.providers.remove` — deregister a provider by kind (thin).
    /// A webhook to a removed provider then 404s (nest-side, not asserted here).
    @discardableResult
    public func removePaymentProvider(kind: String) async throws -> Bool {
        try await ensureNestConnected().payments().providersRemove(kind: kind)
    }

    /// `fauna.payments.claims.redeem` — bind a post-payment claim code to the
    /// calling actor (`monetization.md` § Pillar 3 Q4's universal fallback
    /// binding). The entitlement lands through Pillar 1's grant queue; the
    /// `subscription-settings` page re-reads `mine.list` on success, where the
    /// queued grant renders exactly like a queued subscribe.
    @discardableResult
    public func redeemPaymentClaim(code: String) async throws -> FfiClaimRedeemReply {
        try await ensureNestConnected().payments().claimsRedeem(code: code)
    }

    /// `fauna.payments.claims.mint` — manually mint a claim code for `tier`
    /// (§5 of the profile Tiers tab; provider is always `"manual"` — a
    /// webhook-minted code never reaches this path). `validUntil` is an
    /// optional expiry in unix seconds; `nil` never expires.
    @discardableResult
    public func mintPaymentClaim(tier: String, validUntil: UInt64?) async throws -> FfiClaimMintReply {
        try await ensureNestConnected().payments().claimsMint(tier: tier, validUntil: validUntil)
    }

    /// `fauna.payments.claims.list` — every manually- or webhook-minted claim
    /// code for the author's tiers (§5's audit list; a webhook-minted code's
    /// only other delivery channel is the provider's HTTP response body).
    public func listPaymentClaims() async throws -> [FfiClaimItem] {
        try await ensureNestConnected().payments().claimsList()
    }
    #endif

    // MARK: - Profile (publish/edit — profile.md § Where logic lives → Profile publish/edit)
    //
    // The text-only profile edit form (display_name / bio / links). Read-modify-
    // write: profileGet → decodeProfile (populate the 3 editable fields) → user
    // edits → buildEditedProfile (sign, preserving the non-display fields) →
    // profileSet. The read-modify-write + the Ed25519 sign live ONCE in shared
    // Rust (`fauna-client-profile`, via the FFI free fns); APIClient only injects
    // the owner secret (priority #2 — never re-sign client-side). Mirrors linux
    // `views/profile/edit.rs` + android `ProfileEditVM`.

    /// `fauna.profile.get` — fetch a user's stored signed profile bytes by hex
    /// `actorId`. Throws `fauna.profile.not_found` when the actor has not
    /// published — the caller treats that as first-publish (start blank) / the
    /// OTHER header's hex fallback.
    public func profileGet(actorId: String) async throws -> Data {
        try await ensureNestConnected().profile().profileGet(actorId: actorId)
    }

    /// `fauna.profile.set` — publish/replace the caller's own profile. `body` is
    /// the signed `EmbedAsBytes` wire from `buildEditedProfile`.
    public func profileSet(body: Data) async throws {
        try await ensureNestConnected().profile().profileSet(body: body)
    }

    /// The profile edit form's base load — the caller's OWN stored profile
    /// bytes, read through the shared read-prove-record
    /// (`FfiProfileClient.loadEditBase`) rather than `profileGet`: it proves a
    /// succession link the base needs and records it in the account registry
    /// BEFORE the form can save over it, so a linkless successor's first edit
    /// never races the per-sign-in aftermath hop (profile.md § After an
    /// identity succession → the linkless bullet). `accounts` is the same
    /// registry `runSuccessionAftermath` hands the FFI. `nil` means a
    /// never-published profile, i.e. a first publish — not an error.
    public func loadProfileEditBase() async throws -> Data? {
        guard let secret else { throw APIError.ffiError("No secret for the profile edit base") }
        return try await ensureNestConnected().profile().loadEditBase(
            ownerSecret: hex_to_data(secret), accounts: FaunaAccounts.registry())
    }

    /// Project a stored profile `body` to the three editable display fields
    /// (`display_name` / `bio` / `links`). Pure shared-Rust decode — no
    /// connection — so it is a plain throwing wrapper over the FFI free fn.
    public func decodeProfile(body: Data) throws -> FfiProfileDisplay {
        try decodeProfileDisplay(body: body)
    }

    /// The read-modify-write sign step for the edit form: overwrite ONLY
    /// `displayName` / `bio` / `links`, preserve every other stored field (or use
    /// first-publish defaults when `baseBody` is `nil`), and Ed25519-sign with the
    /// owner key. The whole read-modify-write lives in shared Rust; this only
    /// supplies the owner secret and the registry's predecessor ids
    /// (`profilePredecessors`). Returns the signed wire ready for `profileSet`.
    public func buildEditedProfile(
        baseBody: Data?, displayName: String?, bio: String?, links: [FfiProfileLink]
    ) throws -> Data {
        guard let secret else { throw APIError.ffiError("No secret for profile edit") }
        // Qualify the FFI module: this instance-method name shadows the free fn.
        return try FaunaFFISwift.buildEditedProfile(
            secret: hex_to_data(secret), baseBody: baseBody,
            predecessors: profilePredecessors(),
            displayName: displayName, bio: bio, links: links)
    }

    /// Whom this identity succeeded from, per the account registry — the only
    /// evidence that admits a stored profile signed by someone else as the
    /// base an edit re-signs (profile.md § After an identity succession, the
    /// successor RE-PUBLISHES). Empty for an identity that never succeeded.
    private func profilePredecessors() -> [String] {
        guard let actorId = boundActorIdHex else { return [] }
        return FaunaAccounts.registry().predecessorsOf(actorId: actorId)
    }

    /// The same read-modify-write sign step as `buildEditedProfile`, additionally
    /// resolving the avatar/banner three-state edit (`FfiProfileImageEdit`
    /// Keep/Clear/Set — profile.md § Where logic lives → Field ownership).
    /// `ProfileEditVM.save()` always calls this (never the text-only
    /// `buildEditedProfile`) since Keep/Keep reproduces the text-only behavior
    /// exactly.
    public func buildEditedProfileWithImages(
        baseBody: Data?, displayName: String?, bio: String?, links: [FfiProfileLink],
        avatar: FfiProfileImageEdit, banner: FfiProfileImageEdit
    ) throws -> Data {
        guard let secret else { throw APIError.ffiError("No secret for profile edit") }
        return try FaunaFFISwift.buildEditedProfileWithImages(
            secret: hex_to_data(secret), baseBody: baseBody,
            predecessors: profilePredecessors(),
            displayName: displayName, bio: bio, links: links,
            avatar: avatar, banner: banner)
    }

    // MARK: - Subscriptions (OTHER-profile subscriber browse — monetization.md § Pillar 1)
    //
    // The Slice-B OTHER-profile offers section (`subscription-offers-section`): a
    // prospective subscriber reads ANOTHER creator's offered tiers + their own
    // status, and subscribes. All three are thin pass-throughs to the
    // `FfiSubscriptionsClient` over the caller's bearer (no minting — the
    // consumer never holds a KeyBlob; the author's client mints on approve).
    // `authorIdHex` is the creator's hex `ActorId` (from the contact-row tap-
    // through / nav-stack `actor_id`). Mirrors linux `views/profile/offers.rs` +
    // android `ProfileOffersVM`.

    /// `fauna.subscriptions.offers.list` — another author's offered tiers
    /// (ascending by rank). The OTHER-profile read; distinct from the SELF
    /// `tiers.list` (which is bearer-scoped to the caller's own tiers).
    public func subscriptionOffersList(authorIdHex: String) async throws -> [FfiTierItem] {
        try await ensureNestConnected().subscriptions().offersList(authorId: hex_to_data(authorIdHex))
    }

    /// `fauna.subscriptions.status.get` — the caller's current subscription state
    /// for `authorIdHex` (the held tier, if any), driving each offer row's status.
    public func subscriptionStatusGet(authorIdHex: String) async throws -> FfiSubscriptionStatus {
        try await ensureNestConnected().subscriptions().statusGet(authorId: hex_to_data(authorIdHex))
    }

    /// `fauna.subscriptions.subscribe`, **publishing** the caller's identity-seed
    /// ML-KEM ek (surface B, S4b) unconditionally (no capability token), so the
    /// author can later wrap hybrid `KeyBlob`s to this subscriber. Plaintext +
    /// auto-approve grants inline (`Approved`); encrypted mode enqueues for the
    /// author's mint (`Queued`). The native twin
    /// of the wasm `subscriptionsSubscribePublishingEk`; mirrors linux
    /// `offers.rs::subscribe_to` / `mod.rs::follow`.
    @discardableResult
    public func subscriptionSubscribe(authorIdHex: String, tier: String) async throws -> FfiSubscribeReply {
        guard let secret else { throw APIError.ffiError("No secret for subscribe") }
        return try await subscriptionsSubscribePublishingEk(
            nest: try await ensureNestConnected(),
            subscriberSecret: hex_to_data(secret),
            authorId: hex_to_data(authorIdHex),
            tier: tier)
    }

    /// Read the per-destination backup status (last-upload time + backlog) for
    /// the configured destinations — the NEST's `fauna.backup.status` projection
    /// over the gated FFI free fn `backupDestinationStatus`, which wraps the
    /// shared `fauna_client_config::read_backup_status` every app now calls
    /// (`backups.md` § Per-destination status read; repointed 2026-07-24,
    /// slice-4 leg (d)).
    ///
    /// The `deviceId` / `dataDir` arguments are **gone**: the nest derives the
    /// owner from the authenticated connection, and there is no local
    /// coordinator state to path-match any more, so the `data_dir`
    /// canonical-path contract retires with them. Empty when zero destinations
    /// are configured. Throws if no secret (page is post-login).
    public func loadBackupDestinationStatus() async throws
        -> [FfiBackupDestinationStatus]
    {
        guard let secret else { throw APIError.ffiError("No secret for backup destinations") }
        return try await backupDestinationStatus(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret))
    }

    /// Run one client-side backup audit pass (`ui/backups.md` § Audit-alert
    /// surface): a real connection to each pinned destination + a real
    /// `fauna.backup.custody.list`, checked against `statePath`'s persisted
    /// observation high-water — never a nest-side read. `statePath` MUST be
    /// actor-scoped (`FaunaClient.backupAuditStatePath`); two accounts sharing
    /// one path would let one account's observation silently suppress the
    /// other's freshness failures.
    ///
    /// `syncStateDir` is the dir the sync engine host keeps its per-folder
    /// `fsid-<ref>.db` files in (`FaunaClient.syncStateDir`): the shared audit
    /// reads this device's synced replica of each covered folder from it as the
    /// mirror plane's population anchor (`backup-destinations.md` § Ordinary-folder
    /// coverage → *Retention + audit*). On macOS that is the user domain the
    /// agent-hosted sets live in; a File-Provider-bound set's db stays in the
    /// container and reads as "no seat" — a declared residual.
    public func backupAuditRunPass(statePath: String, syncStateDir: String) async throws
        -> [FfiDestinationAuditRow]
    {
        guard let secret else { throw APIError.ffiError("No secret for backup audit") }
        return try await FaunaFFISwift.backupAuditRunPass(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret),
            statePath: statePath, syncStateDir: syncStateDir)
    }

    // `buildUploadCoordinatorFromConfig` was deleted at the slice-5 flip
    // 2026-08-15: it existed only to build the in-app segment-backup upload
    // coordinator for macOS's always-on driver and iOS's two construct-run-drop
    // triggers, all three of which are gone (`backup-restore.md` § Background
    // Tasks → *Flip status (slice 5)*). The shared-Rust free fn it wrapped
    // (`build_backup_upload_coordinator`) still exists for the apps that have not
    // flipped yet. The static destination resolvers (`resolve_destination[_connected]`,
    // reached from the add/edit URL check below) are NOT part of this and stay.

    /// Build the admin `LocalDomainMachine` (`admin-dns` page — domain CRUD,
    /// catch-all + role-address designation). Admin-class — needs only the
    /// connection handle (the nest derives authority from the authenticated
    /// caller). Merged onto the `DnsManagementMachine` record matrix by domain
    /// name in the shared `AdminDnsView`, mirroring linux/web/windows.
    public func localDomainsMachine() async throws -> LocalDomainMachine {
        buildLocalDomainsMachine(nest: try await ensureNestConnected())
    }

    /// The connected nest's own id (`SelfNest.id`) — the `target_nest_id` seal
    /// target for DNS TLS-cert issuance (`tls-certificates.md` § C; linux
    /// `resolve_this_nest_id`). Resolved through the same shared
    /// `LinkedNestsMachine` all 7 apps use (uniform `this_nest()` passthrough).
    public func thisNestId() async throws -> Data {
        try await buildLinkedNestsMachine(nest: try await ensureNestConnected())
            .thisNest().id
    }

    /// Build the user-tier `LinkedNestsMachine` for the `nests` settings page
    /// (per-user nest pairing — list / link / unlink — plus the v1 nest-trust
    /// facet: mint / renew / revoke / SetLens over the home nest's
    /// content-processing grants, `docs/goal/ui/nests.md`). Built via the
    /// **mail relay + trust** variant so a both-ends `LinkBoth` auto-provisions
    /// the just-linked home box's mailbox reusing the fleet MSEK (the
    /// home-with-public-relay one-action flow:
    /// `deployment-home-with-public-relay.md` § Pairing) *and* the trust facet's
    /// grant-event-log seams are wired — threading the actor secret like
    /// `mailSettingsMachine()`. The mail-relay hook is a no-op when mail isn't
    /// enabled, so wiring it unconditionally is safe. Falls back to the
    /// hook-less/trust-less `buildLinkedNestsMachine` if no secret is set or
    /// keypair derivation throws, so list/link/unlink still work (only the
    /// mailbox auto-provision + trust facet are skipped) — mirrors the linux
    /// lead `settings/linked_nests.rs::wire_machine`.
    /// NB: this is the *page* machine; the `thisNestId()` passthrough above stays
    /// hook-less (it only resolves the self-nest id, needs no secret).
    public func linkedNestsMachine() async throws -> LinkedNestsMachine {
        let nest = try await ensureNestConnected()
        if let secret,
           let machine = try? buildLinkedNestsMachineWithMailRelayAndTrust(
               nest: nest, secret: hex_to_data(secret))
        {
            return machine
        }
        return buildLinkedNestsMachine(nest: nest)
    }

    /// Mint the one-tap trust offer's default capability-grant set
    /// (`onboarding.md` § 3b-ter) on the just-signed-in nest —
    /// `LinkedNestsAction::MintDefaultSet`, dispatched on a fresh
    /// trust-enabled `LinkedNestsMachine` built for this call (never the
    /// long-lived Nests-page one, which the launch glue that calls this
    /// predates). Mints every option `mint_options` derives, skipping what
    /// already has a live grant to that holder — a genuine no-op, not an
    /// error, when the derivable set is empty (e.g. mail not yet enabled).
    /// Same `nest`+`ownerSecret` idiom as `rotateDeploymentSeed`.
    public func mintDefaultTrustSet() async throws {
        guard let secret else { throw APIError.ffiError("No secret for trust-prompt mint") }
        let machine = try buildLinkedNestsMachineWithTrust(
            nest: try await ensureNestConnected(), secret: hex_to_data(secret))
        try await machine.dispatch(action: .mintDefaultSet)
    }

    /// Build the admin `MailPolicyMachine` (flat `admin-mail` policy page).
    /// Admin-class — needs only the connection handle.
    public func mailPolicyMachine() async throws -> MailPolicyMachine {
        buildMailPolicyMachine(nest: try await ensureNestConnected())
    }

    /// Build the admin `CaldavPolicyMachine` (flat `admin-calendar` page — the
    /// deployment-wide CalDAV-enable toggle, the sibling of `admin-mail`).
    /// Admin-class — needs only the connection handle.
    public func caldavPolicyMachine() async throws -> CaldavPolicyMachine {
        buildCaldavPolicyMachine(nest: try await ensureNestConnected())
    }

    /// Build the admin `CarddavPolicyMachine` (flat `admin-contacts` page — the
    /// deployment-wide CardDAV-enable toggle, the contacts sibling of `admin-calendar`).
    /// Admin-class — needs only the connection handle. No port twin: CardDAV rides the
    /// shared DAV listener `admin-calendar-caldav-port-input` governs.
    public func carddavPolicyMachine() async throws -> CarddavPolicyMachine {
        buildCarddavPolicyMachine(nest: try await ensureNestConnected())
    }

    /// Build the read-only `FfiCarddavClient` (the Contacts page's Address Book
    /// segment — `contacts.md` § Address Book segment; `carddav-server.md` §
    /// Independent enablement). Public so the shared `AddressBookVM` drives the
    /// `list_addressbooks`/`query_cards` reads off it, mirroring `moderationClient()`.
    public func carddavClient() async throws -> FfiCarddavClient {
        try await ensureNestConnected().carddav()
    }

    /// Build the admin `WebdavPolicyMachine` (flat `admin-files` page — the
    /// deployment-wide WebDAV-enable toggle, the files sibling of `admin-contacts`).
    /// Admin-class — needs only the connection handle. No port twin: WebDAV rides the
    /// shared DAV listener `admin-calendar-caldav-port-input` governs.
    public func webdavPolicyMachine() async throws -> WebdavPolicyMachine {
        buildWebdavPolicyMachine(nest: try await ensureNestConnected())
    }

    /// Build the admin `ForwarderMachine` (`admin-aliases` external-forwarder
    /// page). Admin-class — needs only the connection handle.
    public func forwardersMachine() async throws -> ForwarderMachine {
        buildForwardersMachine(nest: try await ensureNestConnected())
    }

    /// Build the user-facing `MailAliasesMachine` (`mail-aliases` settings
    /// sub-page). User-class — the nest derives the owning actor from the
    /// authenticated caller, so (like `forwardersMachine`) it needs only the
    /// connection handle, not the MSEK secret.
    public func mailAliasesMachine() async throws -> MailAliasesMachine {
        buildMailAliasesMachine(nest: try await ensureNestConnected())
    }

    /// Build the user-facing `MailSpamMachine` (`mail-spam` settings sub-page).
    /// User-class like `mailAliasesMachine` (the nest derives the owning actor).
    /// The per-user Bayesian feedback loop is unbuilt today, so the machine
    /// surfaces an honest `unimplemented` rejection via `error-message`.
    /// Takes the caller's keypair + node url: the machine signs client-side
    /// spam-model writes (the tier-1 train inserts) itself, so it needs the
    /// authed actor's secret, not just the connection.
    public func mailSpamMachine() async throws -> MailSpamMachine {
        guard let secret else { throw APIError.ffiError("No secret for mail spam") }
        return try buildMailSpamMachine(
            nest: try await ensureNestConnected(),
            secret: hex_to_data(secret),
            nodeUrl: nodeUrl.absoluteString)
    }

    /// Build the user-facing `MailExportMachine` (`mail-export` wizard sub-page)
    /// **with key custody**, because `MailExportVM` spawns the drive loop after a
    /// `Start`/`Resume` that lands `Running` — custody and the spawn are one
    /// change (`mail-export.md` § Implementation status today). The secret opens
    /// every record under the account's standing key set and wraps the
    /// per-session blob key (§ Key material); `handle` may be empty on a fresh
    /// sign-in, and the VM refreshes it through `setActorHandle` before every
    /// Start / Resume / Download.
    ///
    /// No secret → the custody-less build, never custody without a spawn:
    /// listing, Cancel and Discard keep working and `Start` refuses honestly.
    public func mailExportMachine(handle: String) async throws -> MailExportMachine {
        let nest = try await ensureNestConnected()
        guard let secret else { return buildMailExportMachine(nest: nest) }
        return try buildMailExportMachineWithKeyCustody(
            nest: nest,
            secret: hex_to_data(secret),
            nodeUrl: nodeUrl.absoluteString,
            handle: handle,
            saveDir: Self.mailExportSaveDir().path)
    }

    /// Where § Download flow step 5 writes the recovered `.zip.zst`. The shared
    /// sink writes `<name>.part` and renames it into place only once the archive
    /// is complete and terminated, so a refused download leaves nothing here that
    /// reads as a mailbox.
    ///
    /// - e2e: `FAUNA_E2E_DOWNLOAD_DIR` (`SnapshotFileSaver.e2eDownloadDir`, both
    ///   gates), the directory the driver reads back.
    /// - macOS: the user's Downloads folder — the desktop destination tui and
    ///   linux use; the Done summary names the saved path.
    /// - iOS: an app-owned temporary directory. The user-facing destination is
    ///   the share sheet `MailExportView` presents over the finished file (the
    ///   account data export's iOS shape, `AccountSettingsView.exportAndPresent`),
    ///   so only a complete archive is ever offered.
    static func mailExportSaveDir() -> URL {
        if let dir = SnapshotFileSaver.e2eDownloadDir { return dir }
        #if os(macOS)
        return FileManager.default.urls(for: .downloadsDirectory, in: .userDomainMask).first
            ?? FileManager.default.homeDirectoryForCurrentUser
        #else
        return FileManager.default.temporaryDirectory.appendingPathComponent("mail-export", isDirectory: true)
        #endif
    }

    /// Build the user-facing `MailImportMachine` (`mail-import` wizard
    /// sub-page). User-class — the foreign-mailbox migration wizard. Unlike the
    /// export twin the backend is REAL end to end (nest RPC surface + the
    /// shared-Rust foreign-IMAP client), so a rejection here is a genuine
    /// source/nest error. Credentials never leave the device
    /// (`mailbox-migration.md` § Credential handling).
    public func mailImportMachine() async throws -> MailImportMachine {
        buildMailImportMachine(nest: try await ensureNestConnected())
    }

    /// Build the user-facing `MailListsMachine` (`mail-lists` settings sub-page).
    /// User-class — a person's own mailing lists. The list backend is unbuilt
    /// today → honest `unimplemented` rejection via `error-message`.
    public func mailListsMachine() async throws -> MailListsMachine {
        buildMailListsMachine(nest: try await ensureNestConnected())
    }

    /// Build the user-facing `MailListMembersMachine` (`mail-list-members`
    /// sub-page) scoped to one list. `listIdHex` comes from a rendered `ListView`
    /// row; `listName` is the page heading. Throws on a malformed `listIdHex`
    /// (rejected before any RPC).
    public func mailListMembersMachine(
        listIdHex: String, listName: String
    ) async throws -> MailListMembersMachine {
        try buildMailListMembersMachine(
            nest: try await ensureNestConnected(),
            listIdHex: listIdHex, listName: listName
        )
    }

    /// The conversations-rail MLS store — the ONE MLS engine on apple, **scoped to
    /// the session's account**: `<Application Support>/Fauna/<actor-id-hex>/mls.db`
    /// (`account-scoping.md` § Serialized switching). No account ever opens
    /// another's store; a secret that derives no actor id resolves under
    /// `AccountStateDir`'s `-unresolved-` component, never a sibling's.
    ///
    /// Derived from the session's own secret rather than a process-wide "active
    /// account" global: the actor whose MLS store this is, is by construction the
    /// actor whose session is being built.
    private static func conversationsMlsDbPath(selfSecretHex: String) -> String {
        let hex = (try? actorIdFromSecret(secret: hex_to_data(selfSecretHex))).map { data_to_hex($0) }
        return AccountStateDir.mlsDbPath(actorIdHex: hex)
    }

    /// Typed-call client for the `fauna.account.*` / `fauna.quota.get` /
    /// `fauna.profile.handle.change` kinds over the shared connection.
    private func accountClient() async throws -> FfiAccountClient {
        try await ensureNestConnected().account()
    }

    /// The admin WS-RPC client (`fauna.admin.*` kinds) over the shared
    /// connection — admin-gated nest-side (a non-admin caller gets
    /// permission-denied). Public so the shared `AdminVM` drives the
    /// consolidated `admin-users` hub + dashboard off it, replacing the
    /// deleted `/admin/api/*` HTTP twins (admin.md § Where logic lives).
    public func adminClient() async throws -> FfiAdminClient {
        try await ensureNestConnected().admin()
    }

    /// The gated-feature plane's transparency-read client (`fauna.features.*`
    /// kinds) over the shared connection — `docs/goal/architecture/dynamic-features.md`
    /// § Transparency & auditability. Public so the shared `StatusVM` drives
    /// the `feature-limits-section` off it.
    public func featuresClient() async throws -> FfiFeaturesClient {
        try await ensureNestConnected().features()
    }

    /// The Status snapshot's node read (`fauna.nest.info`) over the shared
    /// connection — `docs/goal/ui/status.md` § State & data shape. Public so
    /// the shared `StatusVM` loads the node leg off it.
    public func statusClient() async throws -> FfiStatusClient {
        try await ensureNestConnected().status()
    }

    /// The admin-nest **NAT-mode control** (`admin-nest-nat-mode-*`, admin.md
    /// § Nest → NAT-mode control) — the shared `AdminNatModeMachine`, the exact
    /// seam + signed-commit ceremony the onboarding wizard's `nat_mode_choice`
    /// page drives. Built directly over `nodeUrl` + the caller's primed secret
    /// (like `taskDelegationView`); the machine rides its **own** pre-identity
    /// WS-RPC connection per call, not the shared `ensureNestConnected()`
    /// transport, so this is a plain sync constructor — no `await` needed.
    public func adminNatModeMachine() throws -> AdminNatModeMachine {
        guard let secret else { throw APIError.ffiError("No secret for NAT-mode control") }
        return AdminNatModeMachine(nestUrl: nodeUrl.absoluteString, secretHex: secret)
    }

    /// The web-content authoring client (`fauna.web.*` kinds) over the shared
    /// connection — drives the user `web-settings` subdomain toggle
    /// (`get/setSubdomainEnabled`, User-class caller-scoped) + the admin
    /// `admin-web` apex picker (`get/setApexActor`, Admin-gated nest-side). Public
    /// so the shared `WebPublishStore` / `AdminWebVM` build off it
    /// (web-content-hosting.md § Admin apex hosting / § Published-post management).
    public func webClient() async throws -> FfiWebClient {
        buildWebClient(nest: try await ensureNestConnected())
    }

    /// `fauna.setup.status` over the shared connection — the authed setup-wizard
    /// read that backs the admin-nest serving-port / router-fronted / host-OS-
    /// maintenance fields (admin.md § N Nest). The native twin of web's
    /// `setupStatus` wasm passthrough + windows' `FfiNestClient.setup_status`; no
    /// `SetupClient` crate — the read is issued directly via the `RpcRequester`.
    public func setupStatus() async throws -> FfiSetupStatus {
        try await ensureNestConnected().setupStatus()
    }

    /// The moderation WS-RPC client (`fauna.moderation.*` kinds) over the shared
    /// connection — connection-actor-scoped (the caller's own flagged / actioned
    /// content). Public so the shared `ModerationQueueVM` drives the standalone
    /// Moderation queue + per-row training corrections off it
    /// (moderation.md § Where logic lives — queue/train through shared Rust).
    public func moderationClient() async throws -> FfiModerationClient {
        try await ensureNestConnected().moderation()
    }

    /// The `mail-spam` page's per-account `mail-spam-threshold-override-input`
    /// (`mail-policy-config.md` § Tier 3; `mail-aliases.md` § Spam-threshold
    /// override) — plain `fauna.bridges.*` RPCs over
    /// `MailAccountClient` directly, not `MailSpamMachine` (same bypass shape
    /// as `moderationClient()` above): `spam_threshold_override_{get,set}`
    /// take only `nest`, no owner secret, since the value is caller-scoped.
    public func spamThresholdOverrideGet() async throws -> UInt32? {
        try await FaunaFFISwift.spamThresholdOverrideGet(nest: try await ensureNestConnected())
    }

    /// Set (or clear, with `nil`) the override; re-reads so the caller only
    /// ever reflects the nest-confirmed value, never the local edit.
    public func spamThresholdOverrideSet(value: UInt32?) async throws -> UInt32? {
        try await FaunaFFISwift.spamThresholdOverrideSet(
            nest: try await ensureNestConnected(), value: value)
    }

    /// The family-safety WS-RPC client (`fauna.family.*` kinds) over the shared
    /// connection — per-target link-authorized nest-side (guardian-ness is checked
    /// against the `guardianships` table, never the admin role). Public so the
    /// shared `FamilyVM` drives the Family surface + the global
    /// `supervised-indicator` off it (family-safety.md § App surface).
    public func familyClient() async throws -> FfiFamilyClient {
        try await ensureNestConnected().family()
    }

    /// The session's own actor id (lowercase hex), derived from THIS client's
    /// own primed secret — never the account registry's active pointer, which
    /// a mid-switch transient could move first (android's `sessionActorHex`
    /// is the reference shape). `nil` before a secret is primed.
    private var sessionActorHex: String? {
        guard let secret else { return nil }
        return (try? actorIdFromSecret(secret: hex_to_data(secret))).map { data_to_hex($0) }
    }

    /// `fauna.family.status` — apple's ONE status-read choke point
    /// (`family-safety.md` § Content policy, clause 2). Every present and
    /// future caller — `ContentPolicyStore`, `ScreenTimeStore`,
    /// `FamilyStatusStore`, `FamilyVM` — reads through here instead of
    /// `familyClient().status()` directly, so persisting on this success path
    /// keeps the clause true for all of them (android's `ApiClient.familyStatus`
    /// is the reference shape). A failed read throws before the persist, which
    /// is clause 1 by construction; the fold — including the graduation gate,
    /// under which a policy naming no guardian persists nothing enforceable —
    /// is the shared `SupervisionSnapshot::from_status` behind the FFI.
    public func familyStatus() async throws -> FfiFamilyStatus {
        let status = try await familyClient().status()
        if let actorHex = sessionActorHex {
            FaunaAccounts.registry().persistSupervisionSnapshot(actorId: actorHex, status: status)
        }
        return status
    }

    /// The session's persisted last-known supervision snapshot
    /// (`family-safety.md` § Content policy, clause 2), or `nil` when there is
    /// nothing to restore — no snapshot has ever landed, or its last read said
    /// unsupervised (the graduation-direction refusal the FFI getter enforces
    /// by construction: `supervisedBy` is non-optional, so an unenforceable
    /// slot cannot be expressed). Read once per session establish, ahead of
    /// the first live read — see `seedSupervisionSnapshot`.
    public func supervisionSnapshot() -> FfiSupervisionSnapshot? {
        sessionActorHex.flatMap { FaunaAccounts.registry().supervisionSnapshot(actorId: $0) }
    }

    /// Build the shared `FfiTaskDelegationView` for the `task-delegation`
    /// Settings sub-page (participants.md § Task delegation). The shared seam
    /// resolves the actor from the nest connection, so no secret crosses here;
    /// `deviceId` seeds this client's own `ParticipantRef.Device` exactly as
    /// `backup_coordinator` encodes it, so the two surfaces agree on "this device".
    public func taskDelegationView(
        deviceId: String, capability: FfiHeavyTaskCapability
    ) async throws -> FfiTaskDelegationView {
        return try await ensureNestConnected().taskDelegationViewForDevice(
            deviceId: hex_to_data(deviceId), capability: capability)
    }

    /// Typed-call client for the `fauna.bridges.*` kinds (list / link / unlink
    /// / settings / follows / feeds) over the shared connection.
    private func bridgesClient() async throws -> FfiBridgesClient {
        try await ensureNestConnected().bridges()
    }

    /// Typed-call client for the `fauna.email.*` kinds (per-account filter CRUD)
    /// over the shared connection.
    private func emailClient() async throws -> FfiEmailClient {
        try await ensureNestConnected().email()
    }

    /// Typed-call client for the `fauna.spam.{get,set}_preferences` kinds over
    /// the shared connection.
    private func spamClient() async throws -> FfiSpamClient {
        try await ensureNestConnected().spam()
    }

    /// Typed-call client for the `fauna.{knocks,contacts,inbox.mode}.*` kinds
    /// (knocks inbox, contact roster, inbox-acceptance policy) over the shared
    /// connection. The connection actor replaces the old HTTP `{actor_id}`
    /// path param.
    private func contactsClient() async throws -> FfiContactsClient {
        try await ensureNestConnected().contacts()
    }

    /// Typed-call client for the `fauna.notifications.*` kinds (list /
    /// mark-read / unread count) over the shared connection.
    private func notificationsClient() async throws -> FfiNotificationsClient {
        try await ensureNestConnected().notifications()
    }

    /// Typed-call client for the `fauna.inbox.{send,fetch,ack}` kinds (the
    /// social-inbox delivery producer + the per-actor store-and-forward drain)
    /// over the shared connection. Caller-scoped by construction — the nest binds
    /// a send's `cr.sender` to the connection actor.
    private func inboxClient() async throws -> FfiInboxClient {
        try await ensureNestConnected().inbox()
    }

    /// Typed-call client for the `fauna.conversations.keypackage.{upload,count}`
    /// kinds (the MLS key-package pool) over the shared connection. Scope is the
    /// pool only — the conversation send/receive rails + cross-nest
    /// `keypackage.fetch` are the deferred FaunaMls rails slice.
    private func conversationsClient() async throws -> FfiConversationsClient {
        try await ensureNestConnected().conversations()
    }

    /// Typed-call client for the encrypted-CalDAV Events surface (the
    /// `fauna.bridges.*` calendar RPCs over the `bridge_caldav_*` store) — calendars,
    /// events, RSVP, reminders, attendee invites, `.ics` import/export. Mirrors the
    /// android `caldavRpc()` seam.
    private func caldavClient() async throws -> FfiCaldavClient {
        try await ensureNestConnected().caldav()
    }

    /// Typed-call client for the `fauna.sync.*` device-sync control-plane kinds
    /// (register / changes / status / files / conflicts) over the shared
    /// connection. Byte transfer (chunks, manifests) stays on HTTP residue.
    private func syncClient() async throws -> FfiSyncClient {
        try await ensureNestConnected().sync()
    }

    /// Typed-call client for the `fauna.filesync.snapshot.*` kinds (the Backups
    /// page snapshot table + create / delete / prune / check / diff) over the
    /// shared connection. Full restore now runs the client-side walk through the
    /// sync engine host (`FaunaClient.restoreSnapshot` → `FfiSyncEngineHost`), not
    /// the retired server-side ZIP route (`backup-restore.md` § 4).
    private func snapshotsClient() async throws -> FfiSnapshotsClient {
        try await ensureNestConnected().snapshots()
    }

    /// Typed-call client for the `fauna.stats.get` kind (the Backups stats
    /// popover's repository storage stats) over the shared connection.
    private func statsClient() async throws -> FfiStatsClient {
        try await ensureNestConnected().stats()
    }

    /// Typed-call client for the `fauna.folders.*` kinds — the Devices/Backups
    /// folder control plane (list / create / update / delete, per-set device
    /// list, member roster, rescan schedule) over the shared connection. The
    /// WS-RPC twins of the deleted `/api/v1/file-sets/*` HTTP routes
    /// (`api-layers.md` § Folders). The exclusive-write lease
    /// (`fauna.folders.lease.*`) is sync-engine territory and not surfaced here.
    private func foldersClient() async throws -> FfiFoldersClient {
        try await ensureNestConnected().folders()
    }

    // MARK: - S8 path-sealing backfill (client-driven catch-up sweep)

    /// The whole S8 seal-backfill sweep (D1, then D3 per OWNED set — skips
    /// `role == "member"` rows, since a member's stamp is one the S9 flip's
    /// scrub cannot attribute to the owner) — the ONE sequencing seam every
    /// UniFFI app now calls at its post-auth hook instead of hand-rolling the
    /// D1-then-D3-skip-member loop itself (`docs/goal/behavior/file-sync.md` § Sealed names & paths →
    /// Implementation status today; the shared `seal_backfill` module).
    /// Call once per identity-connected session start on this custody-wired
    /// client (`foldersClient()` already wires the resolver + owner
    /// `BackupKey`). Best-effort throughout; never fails — `nil` only when
    /// the client itself can't be built. The report carries COUNTS only (S7),
    /// never a set name.
    func runSealBackfillSweep() async -> FfiSealBackfillSweepReport? {
        guard let client = try? await foldersClient() else { return nil }
        return await client.runSealBackfillSweep()
    }

    // MARK: - Inherited email-filter review (`libs/fauna-ffi/src/filter_marks.rs`)

    /// Read the ids of every filter rule still awaiting the owner's verdict
    /// (`fauna_client_config::load_filter_marks` via the shared FFI face).
    func filterMarksList() async throws -> [Int64] {
        try await FaunaFFISwift.filterMarksList()
    }

    /// Record **Keep** — the rule stays and its mark clears. Returns whether
    /// anything was open (a concurrent device's answer is a success no-op).
    func filterMarkKeep(id: Int64) async throws -> Bool {
        try await FaunaFFISwift.filterMarkKeep(filterId: id)
    }

    /// Record **Removed** — ONLY after `deleteEmailFilter` already deleted the
    /// rule: this plane has no second removal mechanism, it only records.
    func filterMarkRemoved(id: Int64) async throws -> Bool {
        try await FaunaFFISwift.filterMarkRemoved(filterId: id)
    }

    // MARK: - Unattested-member review (`libs/fauna-ffi/src/member_review.rs`)
    //
    // `ConversationsManager.evictPersonEverywhere`/`.handleForPerson` are
    // already UniFFI-exported directly on the manager object, so those two
    // are called on `sharedConversationsSession()?.manager()` directly below
    // rather than wrapped here — mirrors android's `conversationsManagerHost.manager`.

    /// Read the open review roster (`fauna_client_config::load_member_reviews`
    /// via the shared FFI face).
    func memberReviewList() async throws -> [FfiMemberReview] {
        try await FaunaFFISwift.memberReviewsList()
    }

    /// The shared row-text parts for one review item — consumed, never
    /// re-derived (`fauna_core::data::review_row_text`'s reason-selection and
    /// unnameable-person rules live once, shared). `handle` must be resolved
    /// BEFORE a Remove call for the same person: `memberReviewHandleForPerson`
    /// reads live membership, so there is no seat left to read one off afterward.
    func memberReviewRowText(person: Data, reasons: [String], handle: String?) throws -> MemberReviewRowText {
        try FaunaFFISwift.memberReviewRowText(person: person, reasons: reasons, handle: handle)
    }

    /// The handle `person` is seated under, in the owner's own conversations —
    /// `nil` when conversations are not up yet or they hold no seat the
    /// manager can name (an ordinary answer, not a failure).
    func memberReviewHandleForPerson(person: Data) async -> String? {
        await sharedConversationsSession()?.manager().handleForPerson(person: person)
    }

    /// The review mark each of `threadId`'s member chips carries —
    /// index-parallel with `ThreadDetail.participantDisplays`/`.participants`.
    /// `roster` is the caller's **cached** `memberReviewList()` result (never a
    /// fresh read here — a member list paints far more often than the ledger
    /// changes). `member_review_marks_for_thread` is the ONE reachable answer
    /// to "is this person under review": `fauna_core::data::is_under_review`
    /// itself carries no UniFFI export, by design (`member_review.rs`'s own
    /// doc) — never hand-roll the byte comparison locally.
    func memberReviewMarksForThread(
        manager: ConversationsManager, threadId: String, roster: [FfiMemberReview]
    ) throws -> [Data?] {
        try FaunaFFISwift.memberReviewMarksForThread(
            manager: manager, threadId: threadId, roster: roster)
    }

    /// Record **Keep** — closes every open item for `person` with no group
    /// changes. Returns whether anything was actually open: a concurrent
    /// device may have already answered, and that is a success no-op, never
    /// an error.
    func memberReviewKeep(person: Data) async throws -> Bool {
        try await FaunaFFISwift.memberReviewKeep(person: person)
    }

    /// Record **Remove** — evicts `person` from every group of the owner's
    /// they are currently in NOW (re-derived, never from the stored item),
    /// then persists only whatever verdict the eviction *earned*. A partial
    /// eviction earns none, so the review item stays open; the returned
    /// `CrossGroupEviction`'s `evicted`/`failed`/`unreachable` fields are what
    /// the caller composes its own outcome message from — the derivation is
    /// shared, the wording per-app (mirrors android's `removeResultMessage`).
    func memberReviewRemove(person: Data) async throws -> CrossGroupEviction {
        guard let manager = await sharedConversationsSession()?.manager() else {
            throw APIError.ffiError("No conversations manager for member review")
        }
        return try await FaunaFFISwift.memberReviewRemove(
            manager: manager, person: person)
    }

    // MARK: - Folder cross-user Sharing (owner side)
    //
    // The owner-side "Shared with" surface on each `folder-row`
    // (`docs/goal/ui/folders.md` § Sharing; the shape the linux LEAD
    // established). Reads ride the thin `FfiFoldersClient`; the share/remove
    // orchestration are the `nest + session + ownerSecret` free fns
    // (`libs/fauna-ffi/src/folders_author.rs`), reusing the ONE live conversations
    // `MlsEngine` via `awaitSharedConversationsSession()`. Owner-only; the recipient side
    // (accept/decline/leave) stays blocked on the `WelcomeKind::Folder` gate.

    /// `fauna.folders.members.list_actors` — the *actor* roster (who the set is
    /// shared with) for the owner-side "Shared with" list. Callers gate this on a
    /// shared set (`FolderSummary.mlsGroupId != nil`); an owner-only set returns
    /// `fauna.folders.not_shared`, which the caller treats as an empty roster.
    public func folderActorMembers(name: String) async throws -> [FfiFolderActorMember] {
        try await foldersClient().membersListActors(name: name)
    }

    // MARK: - Following a public folder
    // (`docs/goal/ui/folders.md` § Following a public folder; behavior authority
    // `docs/goal/behavior/folders.md` § Publicly-synced follow.)

    /// **Follow a public folder** — the owner (a handle *or* a bare 64-hex actor
    /// id, the same superset the share flow takes) plus the folder's plaintext
    /// name. Returns the stored follow list.
    ///
    /// ⚠ **Hands the typed owner straight through, deliberately** — this must
    /// NOT pre-resolve it with `resolveRecipient`. The address rules live once,
    /// in the shared `fauna_client_folders::follow_ops` recipe the FFI façade
    /// runs: hex-or-handle classification, the `fauna.actor.by_handle` hop, the
    /// blank-field refusal, and the folding of absent / private / misspelled
    /// into ONE not-found answer so nothing can probe for the existence of a
    /// sealed folder. Resolving here would be a fourth re-derivation of them and
    /// would re-widen the three causes the nest folded.
    ///
    /// The flow collects no nest url: an empty `home_nest_url` means *homed on
    /// the caller's own nest*, and the address a user types is a handle, not a
    /// nest — the recipe's own ⚠ paragraph owns why.
    public func followPublicFolder(owner: String, folderName: String) async throws
        -> [FfiFollowedFolder]
    {
        guard let secret else { throw APIError.ffiError("No secret for the folder follow") }
        return try await foldersFollowPublic(
            nest: try await ensureNestConnected(),
            ownerSecret: hex_to_data(secret),
            owner: owner,
            folderName: folderName)
    }

    /// **Unfollow** — a purely local removal; nothing is revoked anywhere,
    /// because the home nest never knew this follower existed. Idempotent.
    /// Addressed by the follow's own pinned identity (`homeNestUrl`, `folderId`),
    /// never by its display name, which the owner may have changed.
    @discardableResult
    public func unfollowPublicFolder(homeNestUrl: String, folderId: Int64) async throws
        -> [FfiFollowedFolder]
    {
        guard let secret else { throw APIError.ffiError("No secret for the folder unfollow") }
        return try await foldersUnfollowPublic(
            nest: try await ensureNestConnected(),
            ownerSecret: hex_to_data(secret),
            homeNestUrl: homeNestUrl,
            folderId: folderId)
    }

    /// `fauna.folders.members.list` — the enrolled-**device** roster for one
    /// folder, the `folder-place-row` source (ui/folders.md § Implementation
    /// status today owns the post-create place editor's shape).
    ///
    /// Distinct from `folderActorMembers` above: that one is the cross-USER
    /// "Shared with" list, this one is the per-DEVICE place model. Each entry
    /// arrives already projected through the one shared rule
    /// (`fauna_protocol::folders::place_rows`, applied at the FFI boundary), so
    /// the flag triple is read, never re-derived here — and the roster order IS
    /// the e2e address, so a caller must not filter or re-sort it: seat `j` is
    /// `folder-place-row[j]`. `devices` is the Devices page snapshot's already-unsealed
    /// roster; it NAMES each seat, since a user-chosen device label rests
    /// sealed and the nest sends it empty.
    public func folderDevicePlaces(name: String, devices: [DeviceSummary]) async throws
        -> [FfiFolderMember]
    {
        try await foldersClient().placeRows(name: name, devices: devices)
    }

    /// The shared enrol (`FfiFoldersClient::ensure_place`): write
    /// `deviceIdHex`'s place on `name` at the default point iff it holds none;
    /// never rewrites a place the user chose. `true` = enrolled. The
    /// `folder-on-demand-toggle` ON gesture's nest leg (`on-demand-files.md`,
    /// *Auto-appear default-ON*).
    @discardableResult
    public func ensureFolderPlace(name: String, deviceIdHex: String) async throws -> Bool {
        try await foldersClient().ensurePlace(name: name, deviceId: deviceIdHex)
    }

    /// Every set the account holds, as the File Provider presence plan takes
    /// it — own folders (a delivery seat where `deviceIdHex`'s place accepts)
    /// and the folders shared with the account, read-only unless a writer
    /// (`FfiFoldersClient::presence_sets`, the shared mapping;
    /// `on-demand-files.md` § Shared sets on a capability host, decision 3).
    /// Throws on any failed read: the reconcile must skip, never tear a
    /// domain down on a state it could not see.
    public func folderPresenceSets(deviceIdHex: String) async throws -> [PresenceSet] {
        try await foldersClient().presenceSets(deviceId: deviceIdHex)
    }

    /// Share a folder with one person: resolve the typed handle / actor-id to an
    /// actor (`resolveRecipient`, the shared `fauna.actor.by_handle` path), then
    /// `folders_share` (create the MLS group + bind the content key + deliver the
    /// Welcome). Same-nest only (`memberNestUrl == nil`) — cross-nest by
    /// `handle@domain` is a documented follow-on across all 7 apps, matching linux.
    @discardableResult
    public func shareFolder(name: String, recipientInput: String) async throws -> FfiShareOutcome {
        guard let secret else { throw APIError.ffiError("No secret for folder share") }
        let resolved = try await resolveRecipient(recipientInput)
        let outcome = try await foldersShare(
            nest: try await ensureNestConnected(),
            session: try await awaitSharedConversationsSession(),
            ownerSecret: hex_to_data(secret),
            name: name,
            memberId: hex_to_data(resolved.actorId),
            memberNestUrl: nil,
            // Reader default (pre-Phase-1 semantics). The share-time role select +
            // per-member role/cap editing is the entrusted per-app fan-out of the
            // Phase 1 write plane — linux is the proven shape.
            access: nil
        )
        return outcome
    }

    /// Remove a member from a shared set — derives the set's 32-byte `ChannelId`
    /// from its `mlsGroupId` (`folder_channel_id_from_group_id`, a blake3 KDF that
    /// can't be reproduced in Swift) and calls `folders_remove_member`, which
    /// **rotates the content key** so the removed member can't decrypt post-removal
    /// content (§ key-material-hierarchy M2 Rotate-on-removal); this device's sync
    /// agent re-keys on the rotation's custody write by itself.
    public func removeFolderMember(
        name: String, memberActorIdHex: String, groupIdHex: String
    ) async throws {
        guard let secret else { throw APIError.ffiError("No secret for folder member removal") }
        let channelId = try folderChannelIdFromGroupId(groupIdHex: groupIdHex)
        _ = try await foldersRemoveMember(
            nest: try await ensureNestConnected(),
            session: try await awaitSharedConversationsSession(),
            ownerSecret: hex_to_data(secret),
            name: name,
            channelId: channelId,
            memberId: hex_to_data(memberActorIdHex)
        )
    }

    // MARK: - Folder backup-destination places (owner side)
    //
    // The owner-side "Destination places" surface on each `folder-row`
    // (`docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage).
    // Reads + mutations ride the `nest + ownerSecret` free fns
    // (`libs/fauna-ffi/src/backup_destinations.rs`), the FFI face android
    // minted (no new bindgen owed here). Every mutation re-reads and returns
    // the caller's repaint state — never an optimistic flip, mirroring
    // linux/web/android's posture.

    /// `fauna.backup.destination.list` joined with the sealed config's display
    /// names, for one folder — the section's lazy-on-first-expand read.
    public func listFolderDestinations(folderId: Int64) async throws -> [FfiFolderDestinationPlace] {
        guard let secret else { throw APIError.ffiError("No secret for folder destinations") }
        return try await folderDestinationsList(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret), folderId: folderId)
    }

    /// Attach `folderId` to `destinationId` (`folder-destination-attach-button`),
    /// returning the re-read place list.
    public func attachFolderDestination(
        folderId: Int64, destinationId: String
    ) async throws -> [FfiFolderDestinationPlace] {
        guard let secret else { throw APIError.ffiError("No secret for folder destination attach") }
        return try await folderDestinationAttach(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret),
            folderId: folderId, destinationId: destinationId)
    }

    /// Detach `folderId` from `destinationId` (`folder-destination-detach-button`).
    /// `folderSet` is the attached row's own `__folder/<hex>/<id>` name, carried
    /// by the `FfiFolderDestinationPlace` the detach button's row was built
    /// from — never re-derived here.
    public func detachFolderDestination(
        folderId: Int64, destinationId: String, folderSet: String
    ) async throws -> [FfiFolderDestinationPlace] {
        guard let secret else { throw APIError.ffiError("No secret for folder destination detach") }
        return try await folderDestinationDetach(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret),
            folderId: folderId, destinationId: destinationId, folderSet: folderSet)
    }

    // MARK: - Recipient-side folder sharing (pending-share knocks)
    //
    // The recipient counterpart to the owner-side wrappers above
    // (`libs/fauna-ffi/src/folders_recipient.rs`; folders.md § Sharing —
    // Recipient side). The B2 contact gate already auto-joins a contact's shared set
    // and stages a *stranger's* Welcome un-acked; these surface + act on those
    // staged knocks. Member-list-visibility (a joined shared-with-me set appearing in
    // the list with a "Shared by ‹handle›" badge) rides the machine's injected
    // `MlsQuery` join-filter (wired in `devicesMachine`), and `folder-leave-button`
    // rides `leaveFolderShare` below — both primitives landed nest-side.

    /// `folders_pending_shares` — peek the durable inbox for staged (knocked)
    /// cross-user folder shares (un-acked `channel_type == "folder"` welcomes).
    /// A **peek** (never acks). Post-drain these are strangers-only — a contact's
    /// share was already auto-joined by the B2 gate, so it never knocks.
    public func folderPendingShares() async throws -> [FfiPendingShare] {
        try await foldersPendingShares(nest: try await ensureNestConnected())
    }

    /// Accept a staged share (`folder-share-accept-button`): join the MLS group off
    /// the chat rail (over the ONE live conversations session) + ack the durable row.
    /// Accept **bypasses** the contact gate — the user has explicitly decided.
    public func acceptFolderShare(inboxId: Int64) async throws {
        try await foldersAcceptShare(
            nest: try await ensureNestConnected(),
            session: try await awaitSharedConversationsSession(),
            inboxId: inboxId
        )
    }

    /// Decline a staged share (`folder-share-decline-button`): a bare `ack` of the
    /// durable row — the Welcome is dropped **unprocessed**, so declining never joins.
    public func declineFolderShare(inboxId: Int64) async throws {
        try await foldersDeclineShare(nest: try await ensureNestConnected(), inboxId: inboxId)
    }

    /// Leave a set shared WITH you (`folder-leave-button`) — the self-scoped
    /// `fauna.folders.leave`: the caller drops only their OWN roster row (no
    /// `ownerSecret`, and — unlike the owner's member-removal — **no content-key
    /// rotation**: a voluntary leaver keeps the generations they already held,
    /// `mls-group-key-material.md` § M2). The shared fn then locally forgets the MLS
    /// group over the ONE live conversations session. Idempotent.
    /// `groupIdHex` is the row's raw `mlsGroupId`.
    public func leaveFolderShare(groupIdHex: String) async throws {
        try await foldersLeave(
            nest: try await ensureNestConnected(),
            session: try await awaitSharedConversationsSession(),
            groupId: groupIdHex
        )
    }

    // MARK: - WebDAV serving (the per-set opt-in)
    //
    // `folder-webdav-toggle` — webdav-server.md § Independent enablement point 2.
    // The per-set flag is the ACTUAL exposure gate (the deployment-wide `webdav_enabled`
    // is harmless-on: nothing is served until a set is flagged).

    /// Flip one set's WebDAV serve state (`FoldersAuthor::serve_set`): ON = content-key
    /// genesis/migration + `WebdavKeysBlob` provision; OFF = content-key rotation + blob
    /// re-provision without the set. `mlsGroupIdHex` is the set's raw MLS group id when
    /// the set is shared, `nil` when owner-only. Returns the number of served sets the
    /// re-provisioned blob now carries.
    @discardableResult
    public func serveFolderWebdav(
        name: String, mlsGroupIdHex: String?, enable: Bool
    ) async throws -> UInt32 {
        guard let secret else { throw APIError.ffiError("No secret for WebDAV serve") }
        // This client's recording device (the Media gestures' own): an enable also
        // re-seals the set's pre-serve files onto the served key, recorded under it
        // (`webdav-server.md` § Key model (c)).
        let deviceId = FaunaAccounts.sessionMaterial()?.deviceId
        let served = try await foldersServeSet(
            nest: try await ensureNestConnected(),
            session: try await awaitSharedConversationsSession(),
            ownerSecret: hex_to_data(secret),
            name: name,
            mlsGroupIdHex: mlsGroupIdHex,
            enable: enable,
            deviceId: (deviceId?.isEmpty ?? true) ? nil : deviceId
        )
        return served
    }

    /// Paywall one website-enabled set to a subscription tier (`folder-paywall-tier-select`;
    /// v1 is SET-ONLY — there is no clear path, the nest-side revoke/rotation leg is
    /// not shipped). NOT a `DevicesMachine` config write: it runs the full paywall
    /// orchestration (content-key genesis/re-seal + the nest `web_paywall_tier` flag +
    /// the web-serve-holder `content.read{folder:set}` grant mint) via the shared
    /// `FoldersAuthor::paywall_set` (folders.md § Web paywall). `mlsGroupIdHex` is
    /// the set's raw MLS group id when shared, `nil` when owner-only — same contract
    /// as `serveFolderWebdav` above.
    public func paywallFolder(name: String, mlsGroupIdHex: String?, tier: String) async throws {
        guard let secret else { throw APIError.ffiError("No secret for paywall set") }
        try await foldersPaywallSet(
            nest: try await ensureNestConnected(),
            session: try await awaitSharedConversationsSession(),
            ownerSecret: hex_to_data(secret),
            name: name,
            tier: tier,
            mlsGroupIdHex: mlsGroupIdHex
        )
    }

    /// Whether this actor can serve ANY set over WebDAV — the capability gating the
    /// toggle. Serving seals the keys blob under the MSEK, so an actor with no mail
    /// credential cannot serve; `serve_set` flips the nest flag BEFORE re-provisioning,
    /// so a doomed enable would commit the flag and only then fail `NoMsek`. Hence the
    /// toggle renders DISABLED rather than letting the actor click into that failure.
    ///
    /// Takes **no `ConversationsSession`** — the question reads only the owner's
    /// account store, so the folders page can ask it at render time without the
    /// conversations rail being wired.
    public func canServeWebdav() async throws -> Bool {
        guard let secret else { throw APIError.ffiError("No secret for WebDAV capability") }
        return try await foldersCanServeWebdav(
            nest: try await ensureNestConnected(), ownerSecret: hex_to_data(secret))
    }

    // MARK: - Sync defaults (the page-level default conflict policy)
    //
    // `sync-default-conflict-policy-select` — the global default stamped onto NEWLY
    // created sets (existing sets keep their own per-set policy). Thin accessors over
    // the shared `libs/fauna-ffi/src/sync_prefs.rs` free fns, which seal the value
    // into the owner's encrypted `fauna.state.sync-prefs` (the nest never sees
    // plaintext) — input-free: the owner secret and nest connection are resolved
    // inside the shared seam. `folders.md` § Conflicts; `file-sync.md` § Conflicts (policy).

    /// The stored default conflict policy (`"auto"` | `"latest_wins_always"`), or
    /// `nil` when the owner has never set one (the nest column default `auto` applies).
    public func defaultConflictPolicy() async throws -> String? {
        return try await loadSyncPrefs()
    }

    /// Persist the default conflict policy for new sets; returns the freshly-stored value.
    @discardableResult
    public func setDefaultConflictPolicy(_ policy: String?) async throws -> String? {
        return try await saveSyncPrefs(policy: policy)
    }

    // MARK: - Folder FFI edge mapping
    //
    // The `fauna.folders.*` mirrors (`FfiFolder` / `FfiFolderDevice` /
    // `FfiFolderMember`) cross with `i64` epochs and an opaque JSON-string
    // retention policy; the Swift models the Backups/Devices views bind to want
    // `Int`, a localized date `String?`, and a typed `FolderRetentionPolicy?`.
    // These helpers do that conversion so the public method signatures stay
    // stable.


    private static func folderResponse(from ffi: FfiFolder) -> FolderResponse {
        FolderResponse(
            id: Int(ffi.id),
            name: ffi.name,
            retentionPolicy: decodeRetention(ffi.retentionPolicy),
            cachedSnapshotCount: Int(ffi.cachedSnapshotCount),
            cachedTotalBytes: Int(ffi.cachedTotalBytes),
            cachedLastSnapshotAt: epochDisplayString(ffi.cachedLastSnapshotAt),
            includePaths: ffi.includePaths,
            excludePaths: ffi.excludePaths
        )
    }

    private static func deviceInfo(from ffi: FfiFolderDevice) -> DeviceInfo {
        DeviceInfo(
            deviceId: ffi.deviceId,
            label: ffi.label,
            lastChangeAt: epochDisplayString(ffi.lastChangeAt),
            changeCount: Int(ffi.changeCount)
        )
    }

    /// Decode the wire's opaque `fauna.folders.*` `retention_policy` JSON
    /// string to the canonical shape (`backup-restore.md` § 8 RULING —
    /// `{"max_snapshots": N, "max_age_days": M}`, matching
    /// `fauna_folders_machine::RetentionPolicy`); a malformed or absent
    /// policy maps to `nil`. `FolderRetentionPolicy` is a local `Codable`
    /// mirror rather than the generated FFI type: UniFFI records use the FFI
    /// buffer codec, not JSON `Codable`.
    private static func decodeRetention(_ json: String?) -> FolderRetentionPolicy? {
        guard let json, let data = json.data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(FolderRetentionPolicy.self, from: data)
    }

    /// Format a wire epoch (Unix seconds) as the localized date string the
    /// folder/device captions render. The mirrors carry `i64` epochs while the
    /// Swift models expose `String?`; matches `MailSettingsView`'s edge idiom.
    /// Routes through `ValueFormat.absoluteDate` — the same medium-date/short-time
    /// door every other apple absolute-timestamp surface uses, rather than a
    /// second hand-rolled `DateFormatter.localizedString` call.
    private static func epochDisplayString(_ epoch: Int64?) -> String? {
        guard let epoch else { return nil }
        return ValueFormat.absoluteDate(epochMs: epoch * 1000, withTime: true)
    }

    /// Typed-call client for the `fauna.posts.{create,get,interact}` kinds over
    /// the shared connection.
    private func postsClient() async throws -> FfiPostsClient {
        try await ensureNestConnected().posts()
    }

    /// `fauna.posts.get` → the post's extracted body text — the source of a
    /// moderation server-row spam train on the sealed client-write path
    /// (mail-spam.md § Encrypted-mode interaction; mirrors linux
    /// `train_moderation_flow`'s `posts_get` → `Post::body_text` step and windows'
    /// `PostBodyTextAsync`). `nil` on any fetch/decode miss (an unreadable /
    /// bodyless post) so the caller degrades to the server-side train.
    public func postBodyText(contentId: String) async -> String? {
        guard let bytes = try? await postsClient().postsGet(postId: contentId) else { return nil }
        guard let decoded = try? decodePostFull(data: bytes), !decoded.body.isEmpty else { return nil }
        return decoded.body
    }

    // MARK: - Internal HTTP helpers

    private func authorizedRequest(_ url: URL) async throws -> URLRequest {
        try await ensureAuthenticated()
        var request = URLRequest(url: url)
        if let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        return request
    }

    /// The tail every HTTP verb method below shares: issue the request, validate
    /// the response, hand back the body.
    @discardableResult
    private func send(_ request: URLRequest) async throws -> Data {
        let (data, response) = try await session.data(for: request)
        try checkResponse(response, data: data)
        return data
    }

    /// The request-construction shape every verb below shares too — method,
    /// an optional `Content-Type`, an optional body — leaving only the method
    /// string and the two optionals genuinely per-call (found by a second pass
    /// of the row 7 harvest's shingle scan: the first pass only unified the
    /// `send(_:)` tail above, missing that postJSON/put/deleteWithBody etc.
    /// were themselves identical but for the method string).
    @discardableResult
    private func send(url: URL, method: String, contentType: String? = nil, body: Data? = nil) async throws -> Data {
        var request = try await authorizedRequest(url)
        request.httpMethod = method
        if let contentType {
            request.setValue(contentType, forHTTPHeaderField: "Content-Type")
        }
        if let body {
            request.httpBody = body
        }
        return try await send(request)
    }

    // MARK: - Unified Bridges API
    //
    // `fauna.bridges.*` WS-RPC kinds over the shared `FfiBridgesClient`; the
    // HTTP twins (`/api/v1/bridges/*`) were deleted nest-side. The typed `Ffi*`
    // replies map to the same FaunaKit structs the Bridges views bind to (see
    // BridgeFFIMapping.swift), so the view models are untouched.

    public func listBridges() async throws -> [BridgeInfo] {
        try await bridgesClient().list().map(BridgeInfo.init(ffi:))
    }

    public func linkBridge(bridgeId: String, mode: String, fields: [String: String]) async throws -> BridgeLinkResponse {
        // `mode` is a first-class kind argument; `params` carries the per-mode
        // field values as a typed CBOR map (no `Dictionary<String,Any>` on wire).
        let params = ffiCborMap(from: fields)
        let reply = try await bridgesClient().link(bridgeId: bridgeId, mode: mode, params: params)
        return BridgeLinkResponse(ffi: reply)
    }

    public func unlinkBridge(bridgeId: String) async throws {
        try await bridgesClient().unlink(bridgeId: bridgeId)
    }

    public func updateBridgeSettings(bridgeId: String, settings: [String: Any]) async throws {
        try await bridgesClient().setSettings(bridgeId: bridgeId, settings: ffiCborMap(from: settings))
    }

    public func listBridgeFollows(bridgeId: String) async throws -> [BridgeFollow] {
        try await bridgesClient().listFollows(bridgeId: bridgeId).map(BridgeFollow.init(ffi:))
    }

    public func addBridgeFollow(bridgeId: String, followId: String, petname: String?) async throws {
        try await bridgesClient().addFollow(bridgeId: bridgeId, id: followId, petname: petname, extra: nil)
    }

    public func removeBridgeFollow(bridgeId: String, followId: String) async throws {
        try await bridgesClient().removeFollow(bridgeId: bridgeId, followId: followId)
    }

    // The deleted `/admin/api/*` HTTP twins (stats / status / users / audit /
    // invite-codes / invite-requests) were removed in the no-http-ws-rpc-
    // everywhere migration — the admin surface now drives the `fauna.admin.*`
    // WS-RPC kinds through `adminClient()` (the shared `AdminVM`). admin.md
    // § Don't do these: no `/admin/api/*` from new client surfaces.

    /// `path` may carry a `?query=string` suffix (`exportData`'s
    /// `include_blobs=true`, the channel poll's `?after=`) — `URL(string:
    /// relativeTo:)`, not `appendingPathComponent`, which percent-encodes a
    /// literal `?` into `%3F` and folds the whole "query" into the PATH
    /// instead of parsing it as one: the nest's router then sees a path that
    /// matches no route and (for this nest binary) 200s an HTML fallback
    /// page instead of a 404, so the bug reads as "export returns garbage
    /// bytes" rather than a request failure — confirmed live via a Swift
    /// snippet comparing both constructors' `.absoluteString`.
    private func get(_ path: String) async throws -> Data {
        guard let url = URL(string: path, relativeTo: nodeUrl) else {
            throw APIError.httpError(0, "invalid path: \(path)")
        }
        return try await get(url: url)
    }

    public func get(url: URL) async throws -> Data {
        try await send(authorizedRequest(url))
    }

    private func getDecoded<T: Decodable>(_ path: String) async throws -> T {
        let data = try await get(path)
        return try JSONDecoder().decode(T.self, from: data)
    }

    @discardableResult
    private func postJSON(path: String, body: Data) async throws -> Data {
        try await send(url: nodeUrl.appendingPathComponent(path), method: "POST",
                       contentType: "application/json", body: body)
    }

    @discardableResult
    private func postRaw(path: String, body: Data, contentType: String) async throws -> Data {
        let url = nodeUrl.appendingPathComponent(path)
        return try await postRaw(url: url, body: body, contentType: contentType)
    }

    @discardableResult
    private func postRaw(url: URL, body: Data, contentType: String) async throws -> Data {
        try await send(url: url, method: "POST", contentType: contentType, body: body)
    }

    /// POST one sealed blob to `api/v1/blob` as `multipart/form-data`
    /// (`sidecar` + `bytes`). The two-part body shape lives in
    /// ``buildBlobMultipart(sidecarCbor:sealedBytes:boundary:)`` (shared with the
    /// unit tests); this only attaches the boundary header + auth.
    @discardableResult
    private func postMultipartBlob(sidecarCbor: Data, sealedBytes: Data) async throws -> Data {
        let boundary = "----fauna-\(UUID().uuidString)"
        let body = buildBlobMultipart(sidecarCbor: sidecarCbor, sealedBytes: sealedBytes,
                                      boundary: boundary)
        return try await send(url: nodeUrl.appendingPathComponent("api/v1/blob"), method: "POST",
                              contentType: "multipart/form-data; boundary=\(boundary)", body: body)
    }

    @discardableResult
    private func put(path: String, body: Data) async throws -> Data {
        try await send(url: nodeUrl.appendingPathComponent(path), method: "PUT",
                       contentType: "application/json", body: body)
    }

    @discardableResult
    private func delete(path: String) async throws -> Data {
        try await send(url: nodeUrl.appendingPathComponent(path), method: "DELETE")
    }

    @discardableResult
    private func deleteWithBody(path: String, body: Data) async throws -> Data {
        try await send(url: nodeUrl.appendingPathComponent(path), method: "DELETE",
                       contentType: "application/json", body: body)
    }

    private func checkResponse(_ response: URLResponse, data: Data) throws {
        guard let httpResponse = response as? HTTPURLResponse else {
            throw APIError.httpError(0, "Invalid response type")
        }
        guard (200...299).contains(httpResponse.statusCode) else {
            throw APIError.httpError(httpResponse.statusCode,
                                     String(data: data, encoding: .utf8))
        }
    }

    private func callFFI(_ fn: () throws -> String) throws -> String {
        do { return try fn() }
        catch { throw APIError.ffiError(String(describing: error)) }
    }

    // MARK: - Recovery kit (`ui/settings.md` § Recovery kit)

    // Seven thin passes onto `libs/fauna-ffi/src/recovery.rs`. Every judgment
    // these surfaces need — which actions a status enables, what the sweep did,
    // whether the successor seed actually persisted — is decided in shared Rust
    // and crosses the boundary already decided. Nothing here re-derives any of
    // it, and `RecoveryKitVM` must not either.

    /// Read the section's state: the status line's token plus every action's
    /// enablement, in one round trip, from the **registration chain** rather
    /// than a local flag — so a kit created on another device shows up here.
    public func recoveryKitStatus() async throws -> FfiRecoveryKitStatus {
        guard let secret else { throw APIError.ffiError("No secret for the recovery kit") }
        return try await FaunaKit.recoveryKitStatus(
            nest: try await ensureNestConnected(), secret: hex_to_data(secret))
    }

    /// `recovery-kit-create-button` / `recovery-kit-replace-button` — one
    /// ceremony, two authorization arms. `heldKitInput == nil` is the create
    /// arm (first registration); a pasted kit is the replace arm authorized by
    /// the prior key. The escrow blob is re-put in the same ceremony.
    ///
    /// ⚠ The returned secret is the ONLY copy in existence — display it before
    /// anything else can fail. See `FfiMintedKit.escrowStored`: `false` there is
    /// **not** an error to render, the registration has already landed.
    public func recoveryCreateKit(heldKitInput: String?) async throws -> FfiMintedKit {
        guard let secret else { throw APIError.ffiError("No secret for the recovery kit") }
        return try await FaunaKit.recoveryCreateKit(
            nest: try await ensureNestConnected(), secret: hex_to_data(secret),
            accounts: FaunaAccounts.registry(), heldKitInput: heldKitInput)
    }

    /// Register the kit the onboarding `recovery_kit` screen minted and the user
    /// confirmed, at the first authenticated launch after the wizard — a first
    /// registration of THAT root, never a fresh one (the user has just written
    /// this one down). A thin pass onto the shared
    /// `ceremony::register_deferred_kit` tui, linux and web call; it never errors
    /// on a ceremony failure — Settings' `recovery-kit-status` tells the truth.
    public func recoveryRegisterDeferredKit(kitHex: String) async throws {
        guard let secret else { throw APIError.ffiError("No secret for the recovery kit") }
        try await FaunaKit.recoveryRegisterDeferredKit(
            nest: try await ensureNestConnected(), secret: hex_to_data(secret), kitHex: kitHex)
    }

    /// The `fauna://recovery` URI behind a minted kit's QR **and** copy button —
    /// never the bare `FfiMintedKit.secretHex`, which is only the on-screen
    /// display (`identity-succession.md` § The RecoveryKey, *Which encoding each
    /// affordance carries*: a copied kit must restore knowing its account,
    /// exactly as a scanned one does). A thin pass onto the shared builder tui,
    /// linux and web call (`recovery_kit_display_uri`); the seed and node URL
    /// are this client's own, and the handle is the session account's cached one
    /// — a LOCAL read, deliberately, so the URI is built in the same step that
    /// puts the secret on screen rather than behind a round trip that could fail
    /// after the only copy of the kit exists. No cached handle → an empty one,
    /// and the builder emits the URI without it (the restore then asks for the
    /// account, exactly as for a hand-written code).
    public func recoveryKitDisplayUri(kitSecretHex: String) throws -> String {
        guard let secret else { throw APIError.ffiError("No secret for the recovery kit") }
        let handle = sessionActorHex.flatMap {
            FaunaAccounts.registry().sessionMaterial(actorId: $0)?.handle
        } ?? ""
        return try FaunaKit.recoveryKitDisplayUri(
            kitSecretHex: kitSecretHex, secret: hex_to_data(secret), handle: handle,
            nodeUrl: nodeUrl.absoluteString)
    }

    /// `recovery-kit-lost-button` — the seed-alone replacement, which opens the
    /// 30-day window rather than taking effect now (`FfiMintedKit.landsAt`).
    public func recoveryRequestSeedAloneReplacement() async throws -> FfiMintedKit {
        guard let secret else { throw APIError.ffiError("No secret for the recovery kit") }
        return try await FaunaKit.recoveryRequestSeedAloneReplacement(
            nest: try await ensureNestConnected(), secret: hex_to_data(secret))
    }

    /// `recovery-pending-veto-button` — contest a pending replacement with the
    /// kit in hand. Returns whether anything was actually pending.
    public func recoveryVetoPendingReplacement(heldKitInput: String) async throws -> Bool {
        guard let secret else { throw APIError.ffiError("No secret for the recovery kit") }
        return try await FaunaKit.recoveryVetoPendingReplacement(
            nest: try await ensureNestConnected(), secret: hex_to_data(secret),
            heldKitInput: heldKitInput)
    }

    /// `recovery-kit-escrow-reseal-button` — the no-escrow repair. Restores
    /// phrase recovery **without retiring the held kit**, which is why it is not
    /// `recoveryCreateKit`.
    public func recoveryResealEscrowWithHeldKit(heldKitInput: String) async throws -> Int64 {
        guard let secret else { throw APIError.ffiError("No secret for the recovery kit") }
        return try await FaunaKit.recoveryResealEscrowWithHeldKit(
            nest: try await ensureNestConnected(), secret: hex_to_data(secret),
            accounts: FaunaAccounts.registry(), heldKitInput: heldKitInput)
    }

    /// The let-go's dead read — what the section renders
    /// `recovery-kit-unreadable-status` and its confirm gate from, only while
    /// the answer carries a line (`ui/settings.md` § Recovery kit, the fifth
    /// act). Never throws: no runtime yet, or a failed read, is the empty
    /// answer — a let-go that cannot be shown is not offered.
    public func recoveryDeadGenerations() async -> FfiDeadGenerations {
        await FaunaKit.recoveryDeadGenerations()
    }

    /// `recovery-kit-let-go-button` — let go of the generations the last dead
    /// read listed. The runtime re-reads each one dead before it retires
    /// anything, and the outcome carries the refreshed read.
    public func recoveryLetGo(generationIds: [Data]) async throws -> FfiLetGoOutcome {
        try await FaunaKit.recoveryLetGo(generationIds: generationIds)
    }

    /// `identity-stolen-button` — the whole succession ceremony, irreversible.
    ///
    /// The shared driver persists the successor seed, verifies it by read-back,
    /// sweeps the old identity's MLS groups and records the predecessor link.
    /// This app owes only the store-path resolver — which is a **callback**, not
    /// a path, so resolution (which writes) happens after the successor connects.
    ///
    /// Returns the shared typed outcome (`identity-succession.md`
    /// § Implementation status today, the *typed outcome* ruling): every
    /// ceremony that ran is a value, refusals included — only a failure before
    /// it could start throws.
    ///
    /// ⚠ The caller must NOT tear the session down on the landed arm's
    /// `persisted == false`, nor when `carriesTheOnlySeed`: either takes the
    /// only copy of the successor seed with it.
    public func successionSucceedWithHeldKit(heldKitInput: String) async throws
        -> FfiStolenOutcome
    {
        guard let secret else { throw APIError.ffiError("No secret for the succession ceremony") }
        return try await FaunaKit.successionSucceedWithHeldKit(
            nest: try await ensureNestConnected(),
            nestUrl: nodeUrl.absoluteString,
            oldSecret: hex_to_data(secret),
            kitInput: heldKitInput,
            accounts: FaunaAccounts.registry(),
            storePath: SuccessorStorePath())
    }

    /// `recovery-kit-sweep-retry-button` — finish a sweep the ceremony left
    /// unfinished (`docs/goal/ui/settings.md` § Recovery kit → *Finishing an
    /// unfinished group sweep*).
    ///
    /// The shared driver never fails outright — every arm, including a
    /// transport failure, comes back as an `FfiSweepRetryAnswer` carrying its
    /// own sentence. What CAN throw here is reaching the nest at all
    /// (`ensureNestConnected()`), the same shape every other ceremony call in
    /// this file takes.
    ///
    /// `oldStorePath` is deliberately `RetiredIdentityStorePath`, never this
    /// file's `SuccessorStorePath` — see that type's doc for why the retired
    /// identity's resolver must create nothing.
    public func successionRetryGroupSweep() async throws -> FfiSweepRetryAnswer {
        guard let secret else { throw APIError.ffiError("No secret for the sweep retry") }
        return await FaunaKit.successionRetryGroupSweep(
            nest: try await ensureNestConnected(),
            successorSecret: hex_to_data(secret),
            accounts: FaunaAccounts.registry(),
            oldStorePath: RetiredIdentityStorePath(),
            successorStorePath: SuccessorStorePath())
    }

    /// The unbidden press a relaunch adoption owes — the same ceremony as
    /// ``successionRetryGroupSweep()``, answering with the report to park as
    /// well (`succession_discharge_owed_sweep`).
    public func successionDischargeOwedSweep() async throws -> FfiOwedSweepAnswer {
        guard let secret else { throw APIError.ffiError("No secret for the owed sweep") }
        return await FaunaKit.successionDischargeOwedSweep(
            nest: try await ensureNestConnected(),
            successorSecret: hex_to_data(secret),
            accounts: FaunaAccounts.registry(),
            oldStorePath: RetiredIdentityStorePath(),
            successorStorePath: SuccessorStorePath())
    }

    /// Run the post-succession **aftermath** — legs 1, 2, 4, 7, 6, in the order
    /// and with the barriers the shared driver owns
    /// (`succession-aftermath.md` § Re-key scope's `BackupKey` corpus row).
    /// Thin accessor over the `runSuccessionAftermath` FFI export; the app
    /// supplies only what it alone holds — this session's secret, its data dir,
    /// and the account registry (the ceremony's raise context is parked there by
    /// shared Rust).
    ///
    /// The **sibling half** of `successionSucceedWithHeldKit` above: that hands
    /// the account to the successor and deliberately stops before the aftermath;
    /// this is the pass the successor's first session then owes its inherited
    /// corpus. Best-effort by contract — see `SuccessionAftermath.run`, the one
    /// caller.
    public func runSuccessionAftermath(
        onConfigStageSettled: @escaping @Sendable () -> Void = {},
        onProgress: @escaping @Sendable (FfiAftermathLeg, LocalizedText?) -> Void = { _, _ in }
    ) async throws -> FfiAftermathOutcome {
        guard let secret else { throw APIError.ffiError("No secret for the succession aftermath") }
        return try await FaunaKit.runSuccessionAftermath(
            nest: try await ensureNestConnected(),
            ownerSecret: hex_to_data(secret),
            appDataDir: Self.recoveryConfigDataDir,
            accounts: FaunaAccounts.registry(),
            sink: LoggingAftermathSink(
                onConfigStageSettled: onConfigStageSettled, onProgress: onProgress))
    }
}

// MARK: - Internal types

// `AuthRequest` / `TokenResponse` (the `POST /api/v1/auth/token` DTOs) were
// removed when `authenticate(secret:)` moved to the shared FFI `mintBearer`
// (`fauna.auth.handshake`); the nest no longer serves that route.

// MARK: - Folder types

/// The persisted `fauna.folders.*` `retention_policy` — the canonical
/// per-set creation-wizard policy (`backup-restore.md` § 8 RULING), a local
/// `Codable` mirror of the generated `fauna_folders_machine::RetentionPolicy`
/// (not itself `Codable` — UniFFI records use the FFI buffer codec).
public struct FolderRetentionPolicy: Codable, Hashable {
    public let maxSnapshots: UInt32
    public let maxAgeDays: UInt32

    enum CodingKeys: String, CodingKey {
        case maxSnapshots = "max_snapshots"
        case maxAgeDays = "max_age_days"
    }
}

public struct FolderResponse: Codable, Identifiable, Hashable {
    public let id: Int
    public let name: String
    public let retentionPolicy: FolderRetentionPolicy?
    public let cachedSnapshotCount: Int?
    public let cachedTotalBytes: Int?
    public let cachedLastSnapshotAt: String?
    public let includePaths: [String]?
    public let excludePaths: [String]?

    enum CodingKeys: String, CodingKey {
        case id, name
        case retentionPolicy = "retention_policy"
        case cachedSnapshotCount = "cached_snapshot_count"
        case cachedTotalBytes = "cached_total_bytes"
        case cachedLastSnapshotAt = "cached_last_snapshot_at"
        case includePaths = "include_paths"
        case excludePaths = "exclude_paths"
    }
}

public struct DeviceInfo: Codable, Identifiable {
    public let deviceId: String
    public let label: String
    public let lastChangeAt: String?
    public let changeCount: Int

    public var id: String { deviceId }

    enum CodingKeys: String, CodingKey {
        case deviceId = "device_id"
        case label
        case lastChangeAt = "last_change_at"
        case changeCount = "change_count"
    }
}

// MARK: - Snapshot Diff types

public struct SnapshotDiffResponse: Codable {
    public let snapshotA: Int
    public let snapshotB: Int
    public let added: [DiffEntry]
    public let removed: [DiffEntry]
    public let modified: [ModifiedEntry]
    public let summary: DiffSummary

    enum CodingKeys: String, CodingKey {
        case added, removed, modified, summary
        case snapshotA = "snapshot_a"
        case snapshotB = "snapshot_b"
    }
}

public struct DiffEntry: Codable, Identifiable {
    public let path: String
    public let sizeBytes: Int

    public var id: String { path }

    enum CodingKeys: String, CodingKey {
        case path
        case sizeBytes = "size_bytes"
    }
}

public struct ModifiedEntry: Codable, Identifiable {
    public let path: String
    public let oldSize: Int
    public let newSize: Int

    public var id: String { path }

    enum CodingKeys: String, CodingKey {
        case path
        case oldSize = "old_size"
        case newSize = "new_size"
    }
}

public struct DiffSummary: Codable {
    public let addedCount: Int
    public let removedCount: Int
    public let modifiedCount: Int
    public let addedBytes: Int
    public let removedBytes: Int
    public let netBytes: Int

    enum CodingKeys: String, CodingKey {
        case addedCount = "added_count"
        case removedCount = "removed_count"
        case modifiedCount = "modified_count"
        case addedBytes = "added_bytes"
        case removedBytes = "removed_bytes"
        case netBytes = "net_bytes"
    }
}

// MARK: - Prune types

public struct PruneResponse: Codable {
    public let pruned: Int
    public let remaining: Int
}

// MARK: - Repo Stats types

public struct RepoStatsResponse: Codable {
    public let folder: String?
    public let snapshotCount: Int?
    public let totalFiles: Int?
    public let rawSizeBytes: Int?
    public let storedSizeBytes: Int?
    public let dedupRatio: Double?
    public let storageBackend: String?

    enum CodingKeys: String, CodingKey {
        case folder = "folder"
        case snapshotCount = "snapshot_count"
        case totalFiles = "total_files"
        case rawSizeBytes = "raw_size_bytes"
        case storedSizeBytes = "stored_size_bytes"
        case dedupRatio = "dedup_ratio"
        case storageBackend = "storage_backend"
    }
}

public enum APIError: Error, LocalizedError {
    case httpError(Int, String?)
    case ffiError(String)

    public var errorDescription: String? {
        switch self {
        case .httpError(let code, let body): "HTTP \(code): \(body ?? "")"
        case .ffiError(let msg): "FFI error: \(msg)"
        }
    }
}
