import Foundation
import SwiftUI

/// The succession-chain inputs `MediaMachineVM.configure` hands the machine —
/// `FaunaClient.resolvedMediaPredecessors()`. All empty for an identity that never
/// succeeded.
public struct MediaPredecessors: Sendable {
    /// Bare retired owner keys (read-side custody for the identity's own rows).
    public var backupKeys: [Data]
    /// The registry's attested predecessor ids (ruling (8)(b)).
    public var attestedActorIds: [Data]
    /// The paired walk (ruling (8)(c)): `chainActorIds[i]` owns `chainKeys[i]`.
    public var chainActorIds: [Data]
    public var chainKeys: [Data]

    public init(
        backupKeys: [Data] = [], attestedActorIds: [Data] = [],
        chainActorIds: [Data] = [], chainKeys: [Data] = []
    ) {
        self.backupKeys = backupKeys
        self.attestedActorIds = attestedActorIds
        self.chainActorIds = chainActorIds
        self.chainKeys = chainKeys
    }
}

/// Thin SwiftUI-friendly proxy over the page-level `MediaMachine` (UniFFI,
/// `libs/fauna-media-machine` via `libs/fauna-ffi`). The cross-set all-media
/// browse, the sort/filter/view-toggle view state, and the upload/delete content
/// gestures all live in the shared-Rust machine (`docs/goal/ui/media.md` § State
/// & data shape — observer-driven rendering off the shared snapshot, rule 2);
/// this class:
///
///   1. builds + owns the machine instance (over the session `FfiNestClient`),
///   2. implements `MediaObserver` to translate machine notifications into
///      `@Observable` invalidations on the main actor,
///   3. exposes the latest `MediaPageSnapshot` + small gesture wrappers so SwiftUI
///      views read `vm.snapshot` / drive `vm.setSort(_)` / `vm.uploadFromPath(_)`
///      instead of reaching into `vm.machine` everywhere.
///
/// Shared by the macOS and iOS apps — one FaunaKit VM, identical behaviour on
/// both. Mirrors `DevicesMachineVM`'s observer-box
/// pattern. The page is the **content plane** (`media.md` rule 4 — it reads file
/// sets, never configures them; configuration lives in Settings → Folders).
@MainActor @Observable
public final class MediaMachineVM {
    /// The page-level machine. `nil` until `configure` succeeds. Views forward
    /// gestures through the wrappers below.
    public private(set) var machine: MediaMachine?

    /// One-time connect/build failure (`api.mediaMachine` threw). Page read /
    /// upload / delete failures live on the machine snapshot's `error` instead;
    /// all three are surfaced through `errorMessage`.
    public private(set) var connectError: String?

    /// Client-glue upload failure surfaced before the shared gesture runs — a file
    /// the view couldn't read, or a missing owner key. The machine's snapshot
    /// `error` covers the seal/POST/record half; this covers the bytes-and-key glue
    /// (mirrors linux `show_upload_glue_error`). Cleared on the next upload/refresh.
    public private(set) var glueError: String?

    /// Handed to each machine at construction, and replaced by ``reset()`` so the
    /// machine it drops can no longer reach this VM.
    private var observerBox = MediaObserverBox()
    /// The `APIClient` this VM is scoped to — the identity key. A fresh login mints a
    /// new `APIClient` (`FaunaClient.api` is a `let`), so a different instance is what
    /// signals an account switch. Held strongly, so the comparison is on a live object
    /// rather than a recyclable `ObjectIdentifier`.
    ///
    /// Also captured so the upload gesture can derive the owner key + name the
    /// recording device without the shared explorer view (FaunaKit, no per-app
    /// `AppState`) having to thread them.
    ///
    /// Invariant: `machine` is non-nil only if it was built over this `api` — `api`
    /// moves only after ``reset()`` has dropped the machine.
    private var api: APIClient?
    private var deviceId: String?

    public init() {}

    /// Drop everything this VM holds for the account it was scoped to — the ONE
    /// canonical drop (`account-scoping.md` § The scoping taxonomy, the in-memory
    /// corollary: at the identity change itself, keyed on the identity, with no
    /// hand-listed field set at each caller). ``configure(api:deviceId:predecessors:)``
    /// calls it when the api changes, and the iOS page calls it on the nil-client
    /// phase of a switch or sign-out, so a field added to this VM is dropped at every
    /// site by adding it here and nowhere else.
    ///
    /// Clears `api` too, so a build still suspended for the outgoing account finds its
    /// identity gone and drops its own result instead of landing it (the in-flight
    /// clause). The outgoing machine's notifications are cut loose from this VM with
    /// its observer box.
    public func reset() {
        machine = nil
        connectError = nil
        glueError = nil
        api = nil
        deviceId = nil
        observerBox.target = nil
        observerBox = MediaObserverBox()
        _observerTick &+= 1
    }

    // ── Lifecycle ──────────────────────────────────────────────────────────

    /// Vend the machine from `APIClient` and load the first snapshot. Idempotent
    /// for the *same* `APIClient` — the machine is built once; later calls only
    /// re-`refresh()`. A DIFFERENT `APIClient` (a re-login mints a fresh one) is
    /// another account: the previous account's state is dropped via ``reset()``
    /// **before** building, so a build that then throws leaves the page empty and
    /// retryable — never the previous account's machine paired with the new api.
    /// `deviceId` is the session's hex sync device id (`FaunaClient.deviceId`), the
    /// write-capable recorder for `upload`/`delete`.
    ///
    /// `predecessors` — this session's succession chain, off
    /// `FaunaClient.resolvedMediaPredecessors()`; empty for every identity
    /// that never succeeded. No default, like `APIClient.conversationsSession`'s:
    /// a caller that forgets it must fail to compile rather than render a
    /// successor's inherited media as an empty page.
    public func configure(api: APIClient, deviceId: String, predecessors: MediaPredecessors) async {
        if let current = self.api, current !== api { reset() }
        self.api = api
        self.deviceId = deviceId
        guard machine == nil else {
            await refresh()
            return
        }
        connectError = nil
        observerBox.target = self
        let built: MediaMachine
        do {
            built = try await api.mediaMachine(observer: observerBox)
        } catch {
            // The build suspended: a `reset()` (the switch's nil-client phase) or a
            // `configure` for another api may have run meanwhile, and a failure for
            // the outgoing account must not paint on that account's successor.
            guard self.api === api else { return }
            connectError = DisplayError.message(error)
            return
        }
        // Same re-check for the success path. A concurrent build for this same api
        // that finished first also wins — one machine, one observer.
        guard self.api === api, machine == nil else { return }
        machine = built
        // Write-side label custody for the delete/restore gestures (S8 D2):
        // the same per-actor owner key the upload gesture seals with,
        // injected once so those records seal instead of the nest's
        // post-S9-flip hard refusal (`fauna.sync.path_seal_required`) on an
        // unsealed path. Mirrors linux/tui's inject-at-construction shape.
        if let key = try? api.ownerBackupKeyBytes() {
            built.setOwnerBackupKey(key: key)
        }
        // READ-side custody for a successor, and deliberately a SECOND
        // injection rather than a second key on the line above: that one is
        // the delete/restore seal root, and a retired key must never reach a
        // seal. Without it the media corpus a succession re-pointed to this
        // owner stays dark — every fetch succeeds and only the AEAD tag
        // fails, so the page lists nothing, paints no thumbnail and reports
        // no error (`succession-aftermath.md` § Re-key scope, the
        // `BackupKey` corpus row). Mirrors tui's `media/mod.rs` and web's
        // `wasm-media.ts` build sites.
        if !predecessors.backupKeys.isEmpty {
            built.setPredecessorBackupKeys(keys: predecessors.backupKeys)
        }
        // …and the READER's half of the same walk: the attested predecessor ids,
        // so the listing's judge reads a row a retired identity signed as this
        // account's own (`writer-signed-change-records.md`, ruling (8)(b)).
        // Mirrors tui/linux/web/android.
        if !predecessors.attestedActorIds.isEmpty {
            built.setPredecessorActorIds(ids: predecessors.attestedActorIds)
        }
        // …and the keys PAIRED with those identities, replacing the bare keys
        // above: a row signed as a predecessor opens only under that
        // identity's root and its predecessors' (ruling (8)(c)).
        if !predecessors.chainActorIds.isEmpty {
            built.setPredecessorChain(actorIds: predecessors.chainActorIds, keys: predecessors.chainKeys)
        }
        // The share-link author (`share-links.md` § Where logic lives): the
        // session's identity signs the token and seals its filename, and the
        // links point at this session's nest. After the predecessors, which it
        // reads to open old list names — tui's `media/mod.rs::init` order. A
        // missing secret leaves it unwired; the share gestures then report
        // their own error rather than mint under a wrong key.
        if let secret = try? api.identitySecretBytes() {
            built.setShareAuthor(secret: secret, nestUrl: api.nodeUrl.absoluteString)
        }
        await refresh()
    }

    // ── Observer ───────────────────────────────────────────────────────────
    fileprivate func onMachineChanged() {
        // @Observable picks up via the property accesses below; provoke a
        // tracked-property read on the main actor so SwiftUI re-renders.
        _observerTick &+= 1
    }
    private var _observerTick: UInt64 = 0

    // ── Read surface (read freshly on every access) ──────────────────────────

    /// The whole renderable Media page in one record (cross-set items, filter
    /// options, view state, page error). `nil` until `configure`.
    public var snapshot: MediaPageSnapshot? {
        _ = _observerTick
        return machine?.snapshot()
    }

    /// The page-level `error-message`: the connect failure first, then a
    /// client-glue upload failure, else the machine snapshot's localized `error`.
    public var errorMessage: String? {
        _ = _observerTick
        return firstNonNil(connectError, glueError, machine?.snapshot().error.map(renderLocalizedText))
    }

    // ── View-state gestures (the media-sort-select / -folder-filter / -view-toggle) ──

    /// Re-read the cross-set all-media aggregate. Clears any prior client-glue
    /// error; the machine clears/sets its own snapshot error.
    public func refresh() async {
        glueError = nil
        // The sealed-first path render (S3, `file-sync.md` § Sealed names & paths):
        // pass this reader's owner key, exactly as the other six apps do.
        //
        // ⚠ NOT cosmetic — this is a page-emptying gap, not a label-rendering one.
        // `MediaMachine::render_sealed_paths` OMITS every row whose label the
        // reader cannot open (the ratified non-audience degrade), so a keyless
        // `refresh` drops the caller's own sealed items and the page renders ZERO
        // of them, silently and with no `error-message`. `configure()` above
        // already injects the same key for the write gestures, which is why an
        // upload/delete refresh rendered while the plain page read did not.
        //
        // A failed derivation degrades to `nil` — the pre-S3 plaintext-only path,
        // never a throw: `refresh()` has no error channel the page banner catches.
        let backupKey = try? api?.ownerBackupKeyBytes()
        await machine?.refresh(backupKey: backupKey)
    }

    /// Set the `media-sort-select` key (`"name"` / `"size"` / `"date"`).
    public func setSort(_ value: String) { machine?.setSort(value: value) }

    /// Set the `media-sort-direction`: `true` = descending, `false` = ascending
    /// (the default). Applies to whatever `media-sort-select` key is active; the
    /// ordering itself runs in shared Rust (`MediaSnapshot::view`).
    public func setDescending(_ descending: Bool) { machine?.setDescending(descending: descending) }

    /// Set the `media-folder-filter` scope: a set name, or `nil` for the all-media
    /// default.
    public func setFilter(_ folder: String?) { machine?.setFilter(folder: folder) }

    /// Select a **followed public folder** as the browse scope
    /// (`ui/media.md` § Followed public folders). `value` is the machine-minted
    /// opaque option value, handed back verbatim — never parsed, never composed.
    ///
    /// **Selecting is what FETCHES**, once, on demand: a followed folder's rows
    /// live on its home nest and the follower's own nest keeps no copy, so
    /// `fauna.media.list` structurally cannot serve them and eager aggregation
    /// would put one relayed cross-nest fetch per follow on every Media refresh.
    /// The machine notifies its observer on settle, so the page repaints off the
    /// snapshot rather than off this call's return.
    public func selectFollowedScope(_ value: String) async {
        await machine?.selectFollowedScope(value: value)
    }

    /// Download one file from the active followed scope, addressed by its
    /// relative path.
    ///
    /// **Never `downloadFile`**: the followed read is keyless and structural —
    /// it goes through the scope's own `home_nest_url` fetcher, and a followed
    /// read that touched the name-keyed custody resolver would be a bug even
    /// when it appears to work (`ui/media.md` § Followed public folders owns
    /// why). Throws the machine's error verbatim; `media-item-detail` renders
    /// it on its own status line (`media.error_download`).
    public func downloadFollowed(value: String, relativePath: String) async throws -> Data {
        guard let machine else { throw MediaDownloadUnavailable() }
        return try await machine.downloadFollowed(value: value, relativePath: relativePath)
    }

    /// Download one file of the caller's own or a shared set — the
    /// `media-item-detail-download-button` walk (`ui/media.md` § Element IDs),
    /// keyed by the file's LATEST version row (`manifestHash` +
    /// `contentKeyVersion`). The shared `MediaMachine::download_file` resolves a
    /// shared set's content keys from this reader's custody
    /// (`NestFolderKeyResolver`) and opens an owner-only set under `backupKey`,
    /// so the owner key derived here is the only client glue — the same
    /// `ownerBackupKeyBytes` the upload and thumbnail wiring use. Throws the
    /// machine's (or the key derivation's) error verbatim; the detail renders
    /// it on its own status line.
    public func downloadFile(
        manifestHash: String, contentKeyVersion: UInt64?, folder: String, relativePath: String
    ) async throws -> Data {
        guard let machine, let api else { throw MediaDownloadUnavailable() }
        let backupKey = try api.ownerBackupKeyBytes()
        return try await machine.downloadFile(
            manifestHash: manifestHash, contentKeyVersion: contentKeyVersion,
            folder: folder, relativePath: relativePath, backupKey: backupKey)
    }

    /// Set the `media-view-toggle`: `true` = thumbnail grid, `false` = list.
    public func setViewGrid(_ grid: Bool) { machine?.setViewGrid(grid: grid) }

    // ── Upload (file-upload + upload-button → shared upload_selected gesture) ──

    /// Read the picked file at `path` and upload its bytes into the **selected**
    /// set via the shared `MediaMachine::upload_selected` gesture (seal under the
    /// owner `BackupKey` → POST the blob → record the manifest member → refresh;
    /// `media.md` § Layout & flow / § User actions). Reading the file + deriving the
    /// owner key is the only client glue — the "which set" target policy lives in
    /// the shared machine (priority #1/#2). The recorded member path is the picked
    /// file's basename (matching linux). A read / key failure surfaces on
    /// `error-message`; a successful upload's refresh clears it.
    public func uploadFromPath(_ path: String) async {
        glueError = nil
        let trimmed = path.trimmingCharacters(in: .whitespacesAndNewlines)
        // Nothing chosen: say so on `error-message` — never a silent no-op, which
        // presents as a dead button (`media.md` § User actions; linux + windows
        // surface the same bare `media.file_required`).
        guard !trimmed.isEmpty else {
            glueError = L.media.fileRequired
            return
        }
        guard let machine, let api, let deviceId else { return }

        let url = URL(fileURLWithPath: trimmed)
        let rawBytes: Data
        do {
            rawBytes = try Data(contentsOf: url)
        } catch {
            setGlueUploadError(error.localizedDescription)
            return
        }
        let backupKey: Data
        do {
            backupKey = try api.ownerBackupKeyBytes()
        } catch {
            guard let text = DisplayError.message(error) else { return }
            setGlueUploadError(text)
            return
        }
        let memberPath = url.lastPathComponent
        await machine.uploadSelected(
            deviceId: deviceId, path: memberPath, rawBytes: rawBytes, backupKey: backupKey)
    }

    // ── Thumbnail (per-item media-thumbnail lazy render) ─────────────────────

    /// Fetch + decrypt the thumbnail blob for one `media-item` through the shared
    /// `MediaMachine::fetch_thumbnail` (GET direct-by-hash → content-address
    /// verify → owner-`BackupKey` decrypt — all shared Rust, priority #2),
    /// returning the decoded image bytes the card paints over its placeholder.
    /// Deriving the owner key is the only client glue (same `ownerBackupKeyBytes`
    /// the upload wiring uses). This is a **per-item query**: any failure (no
    /// machine/api yet, a missing key, a bad hash, a decrypt/decode error) returns
    /// `nil` and the card keeps its placeholder — one unreadable thumbnail must
    /// never blank the page or touch `error-message` (`media.md` § Thumbnails).
    /// Mirrors linux `build_media_item`'s lazy per-tile fetch.
    public func fetchThumbnail(hash: String) async -> Data? {
        guard let machine, let api else { return nil }
        let backupKey: Data
        do {
            backupKey = try api.ownerBackupKeyBytes()
        } catch {
            return nil
        }
        return try? await machine.fetchThumbnail(thumbnailHash: hash, backupKey: backupKey)
    }

    /// Delete the member at `path` of `folder` (tombstone via the shared gesture),
    /// then refresh. Pure WS-RPC. Driven by `media-delete-button` → the single
    /// `media-delete-confirm-modal` on `MediaItemDetailView` (both apple targets,
    /// 2026-08-05). This call site predated the affordance by seven weeks — the
    /// "finished capability with no button" `media.md` § Implementation status
    /// today flagged, and the comment here naming the absent ID is what the
    /// ui.yaml spec cited when the family was approved.
    public func deleteItem(folder: String, path: String) async {
        guard let machine, let deviceId else { return }
        await machine.delete(folder: folder, deviceId: deviceId, path: path)
    }

    // ── File versions (media-item-detail / file-version-history) ────────────

    /// Version history for one synced file, oldest→newest — the
    /// `file-version-history` list off the shared `MediaMachine::file_versions`
    /// (`docs/goal/behavior/file-sync.md` § File Versions). Throws the machine's
    /// error verbatim; the caller renders it locally (this surface has its own
    /// load state, distinct from the page `error-message`).
    public func fileVersions(folder: String, path: String, includePruned: Bool = false) async throws -> [FileVersionSummary] {
        guard let machine else { return [] }
        return try await machine.fileVersions(folder: folder, path: path, includePruned: includePruned)
    }

    /// Recover a soft-pruned version (`file-version-undelete-button`,
    /// `file-versions.md` § Retention (3)) — the recovery-browse leg.
    /// Throws the machine's error verbatim, same shape as `fileVersions`; the
    /// caller re-loads the (still `includePruned`) list so the row's badge
    /// clears once the version is live again.
    public func undeleteVersion(path: String, versionNum: Int64) async throws {
        guard let machine else { return }
        try await machine.undeleteVersion(path: path, versionNum: versionNum)
    }

    /// Restore `version` of `path` in `folder` (`file-sync.md` § Restore) — an
    /// ordinary re-point that appends a new version, reversible. The shared
    /// gesture repaints the page snapshot itself (mirrors `deleteItem`), so the
    /// item's rendered size/date in the outer list update on their own; the
    /// caller re-loads the open detail's version list to show the new head.
    public func restoreVersion(folder: String, path: String, version: FileVersionSummary) async {
        guard let machine, let deviceId else { return }
        await machine.restoreVersion(folder: folder, deviceId: deviceId, path: path, version: version)
    }

    // ── Share links (`share-links.md` § Flows; the `share-link-*` surfaces) ──
    //
    // Every piece of state — the create step, the URL revealed only after the
    // registration succeeded, the list's `loaded` bit and row states, the armed
    // revoke — lives in the shared machine's snapshot (`share_create`,
    // `share_links`); these wrappers only forward gestures. The machine
    // notifies its observer on every change, so the surfaces repaint off
    // `snapshot` rather than off these calls' returns.

    /// `share-link-button` — open the create surface on one file. The machine
    /// refuses an ineligible file (no-op), so no surface follows for it.
    public func openShareCreate(folder: String, path: String) {
        machine?.openShareCreate(folder: folder, path: path)
    }

    /// `share-link-expiry-select` — one of `snapshot.shareExpiryOptions`.
    public func setShareExpiry(_ value: String) { machine?.setShareExpiry(value: value) }

    /// `share-link-create-button` — mint, register, then reveal the URL.
    public func createShareLink() async { await machine?.createShareLink() }

    /// `share-link-cancel-button` — also the close after success.
    public func closeShareCreate() { machine?.closeShareCreate() }

    /// `share-link-list-button` — open and load the caller's links.
    public func openShareLinks() async { await machine?.openShareLinks() }

    /// `share-link-list-close-button`.
    public func closeShareLinks() { machine?.closeShareLinks() }

    /// `share-link-revoke-button` — arm the single confirm on one row.
    public func armShareRevoke(tokenId: String) { machine?.armShareRevoke(tokenId: tokenId) }

    /// `share-link-revoke-cancel-button` — a pure disarm.
    public func cancelShareRevoke() { machine?.cancelShareRevoke() }

    /// `share-link-revoke-confirm-button` — the gesture consumes the armed
    /// token first thing, which is what closes the confirm.
    public func confirmShareRevoke() async { await machine?.confirmShareRevoke() }

    /// Surface a client-glue upload failure as the localized `media.error_upload`
    /// banner (mirrors linux `show_upload_glue_error` — reads like a machine
    /// upload error). A successful upload's refresh clears it.
    private func setGlueUploadError(_ detail: String) {
        glueError = renderLocalizedText(
            LocalizedText(key: "media.error_upload", args: ["message": detail]))
    }
}

/// A download pressed before the machine was built (or after `reset()` dropped
/// it). Never silent (convention 11): it reaches the detail's status line like
/// any other download failure (`DisplayError.message` renders it through
/// `description`).
struct MediaDownloadUnavailable: Error, CustomStringConvertible {
    var description: String { L.common.notConnected }
}

/// Trampoline conforming to UniFFI's `MediaObserver`. The machine takes the
/// observer at construction time (`build_media_machine`), so late-binding via
/// `target` lets the VM register itself after `configure`. Mirrors
/// `DevicesObserverBox`.
final class MediaObserverBox: MediaObserver, @unchecked Sendable {
    weak var target: MediaMachineVM?
    func onChanged() {
        notifyOnMainActor(target) { $0.onMachineChanged() }
    }
}
