import Foundation
import Photos
import SwiftData

@Observable
public class PhotoBackupEngine {
    public var isBackingUp = false

    /// How many of THIS pass's assets are still to be processed — the countdown
    /// behind the "{n} remaining" row and the `photo-backup-sync-progress`
    /// readout (`ui/folders.md` § Photo backup: "while a backup pass runs, the
    /// phone shows how far it has got"). Set to the pass's asset count before the
    /// first item, decremented once per asset however that asset leaves the loop,
    /// and cleared on every exit path — so it describes an IN-FLIGHT pass and
    /// never a finished one.
    ///
    /// ⚠ It was **declared and never written** from the day it was added until
    /// 2026-09-20, so `PhotoBackupControlsView`'s `if engine.pendingCount > 0`
    /// row could not render and a pass showed the user only an indeterminate
    /// spinner: the same silent-feature-death shape as B2's dead `UserDefaults`
    /// key and B3's rejected change records. android has always written its twin
    /// (`PhotoBackupEngine.kt`'s `totalScanned`, rendered as
    /// `totalScanned - uploadedCount`), so this was per-app drift in the
    /// direction of the poorer surface — priority #4, resolved toward android's.
    /// Unlike android's, this counter is **per pass** rather than cumulative
    /// across passes; the android twin adopts that at its own pickup.
    public private(set) var pendingCount = 0
    public var completedCount = 0
    public var lastBackupDate: Date?
    public var errorMessage: String?

    /// What the LAST completed pass actually did, step by step — the ingest's
    /// funnel, not a log line. A pass that uploads nothing is the feature's whole
    /// failure surface and it has four causes that look identical from outside the
    /// app (PhotoKit handed us nothing; every asset was already synced; the export
    /// failed; the ingest threw), because three of the four are silent by
    /// construction: two `continue`s and a `catch` that only sets a banner. The
    /// pass now counts each one, so "backup did nothing" always names WHICH
    /// nothing.
    ///
    /// Plain production `Int`s, the same shape `completedCount`/`pendingCount`
    /// already have: a diagnostic a user can be asked for is worth more than one
    /// only a debug build can answer, and the e2e reads them through the shells'
    /// `serializeState()` (convention 11 — a field read, never a round trip).
    /// Reset at the top of every pass, so they describe THAT pass and never
    /// accumulate across runs.
    public private(set) var lastPassAssetsSeen = 0
    public private(set) var lastPassAlreadySynced = 0
    public private(set) var lastPassExportFailed = 0
    public private(set) var lastPassIngestFailed = 0
    public private(set) var lastPassUploaded = 0

    /// How far the pass has got: assets processed, of assets the pass set out to
    /// process. Derived rather than stored, so it cannot disagree with the
    /// countdown that drives it. `ui/folders.md` § Photo backup is what asks for
    /// it ("while a backup pass runs, the phone shows how far it has got") and
    /// `photo-backup-sync-progress` is where the user reads it.
    public var passProgress: (processed: Int, total: Int) {
        (max(0, lastPassAssetsSeen - pendingCount), lastPassAssetsSeen)
    }

    /// The pass **cycle counters** — `started` / `completed`, the two-counter
    /// pigeonhole every other app-side cadence already publishes
    /// (`fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY`, `CONV_RECEIVE_CYCLES_KEY`).
    /// They exist because "a pass ran" is otherwise unattributable: the toggle,
    /// the PhotoKit observer, the `BGProcessingTask` and the Sync-now button all
    /// call `syncNewPhotos()`, so a completion timestamp cannot say WHICH of them
    /// caused this one. Read `started` before the trigger, trigger, then wait for
    /// `completed` to pass the baseline (convention 14 — latency-independent
    /// state, never a settle sleep).
    ///
    /// Both start at 0 on a fresh process and only ever increase. A pass refused
    /// before it began — no host, or the Wi-Fi gate closed — counts as neither; a
    /// pass that THREW counts as completed, because the alternative is a consumer
    /// that deadline-polls its whole budget on a failure the funnel could have
    /// named in one line.
    public private(set) var passesStarted = 0
    public private(set) var passesCompleted = 0

    /// The Photos authorization this device granted, as of the last pass —
    /// `"authorized"`, `"limited"`, `"denied"`, `"restricted"`, `"notDetermined"`.
    /// The one datum that separates "the library is empty" from "we are only
    /// allowed to see the assets the user hand-picked, and they picked none":
    /// a `.limited` grant makes `PHAsset.fetchAssets` return an empty result on a
    /// library full of photos, with no error anywhere.
    public private(set) var lastPassAuthorization = "notDetermined"

    /// The in-process engine host (`FaunaClient.syncHost`), set once the session
    /// builds it. An OS-owned photo library is not a folder, so it gets **no**
    /// watch-dir engine: each asset is exported to a temp file and pushed through
    /// the host's **sealed per-file ingest** (`file-sync.md` § Apple apps —
    /// convergence design, library-ingress bullet). `nil` ⇒ backup is a no-op until
    /// the host arrives.
    public var host: FfiSyncEngineHost?

    private var api: APIClient?
    private var networkMonitor: NetworkMonitor?
    private var modelContext: ModelContext?
    private var deviceId: String?

    /// The resolved photo-library set — its identity (`folderId`, the key the
    /// ingest takes) and its name (the label the snapshot call takes) — cached
    /// for the session. Resolution hits the nest (it may create the set), so it
    /// is done once per launch and lazily — an offline launch must not
    /// permanently disable photo backup.
    private var photoSet: FfiPhotoLibrarySet?

    private let snapshotInterval = 50  // Create snapshot every 50 uploads
    private var uploadsSinceSnapshot = 0

    /// The single-flight gate — see `syncNewPhotos()` and `PhotoBackupPassGate`.
    private let gate = PhotoBackupPassGate()

    public init() {}

    public func configure(
        api: APIClient,
        networkMonitor: NetworkMonitor,
        modelContext: ModelContext,
        deviceId: String
    ) {
        self.api = api
        self.networkMonitor = networkMonitor
        self.modelContext = modelContext
        self.deviceId = deviceId
    }

    /// The folder this device's photo ingress feeds: the wizard-preset
    /// Backup-mode **"Photo Library"** set (`ui/folders.md` § Photo backup →
    /// *Target set model*). The decision is shared Rust, so apple and android
    /// cannot drift on a rule whose failure mode is orphaning the user's photos.
    ///
    /// This must succeed before any ingest: the nest rejects a `changes.record`
    /// into a set with no control-plane row, so ingesting into an unresolved set
    /// uploads chunks that never become files.
    private func photoLibrarySet() async throws -> FfiPhotoLibrarySet {
        if let photoSet { return photoSet }
        guard let api, let deviceId else {
            throw APIError.ffiError("Photo backup is not configured")
        }
        let set = try await api.resolvePhotoLibrarySet(
            stateDir: FaunaClient.syncStateDir,
            deviceId: deviceId,
            deviceLabel: FaunaClient.deviceLabel
        )
        photoSet = set
        return set
    }

    /// Sync all new photos/videos that haven't been backed up yet.
    ///
    /// **Single-flight, with a trailing re-run** (:class:`PhotoBackupPassGate`).
    /// Six call sites across five triggers reach this method — the enable toggle's
    /// grant branch, the PhotoKit change observer, the `BGProcessingTask` body, the
    /// Sync-now button, and a catch-up pass at launch on each of the two shells —
    /// and they are not mutually exclusive: a photo arriving mid-pass wakes the
    /// observer while the scheduler's pass is still running. Overlapping passes
    /// would both export, strip and ingest the same asset (the `PhotoBackupRecord`
    /// marking it synced is written only *after* a successful ingest), and would
    /// reset and then interleave each other's per-pass funnel, so the progress
    /// readout and the diagnostics would describe a blend of two passes.
    ///
    /// ⚠ **The obvious fix — `guard !isBackingUp else { return 0 }` — is the wrong
    /// one, and android ships it** (`PhotoBackupEngine.kt`'s
    /// `if (_isSyncing.value) return 0`). The running pass has already taken its
    /// `PHAsset.fetchAssets` snapshot, so an asset that appears mid-pass is not in
    /// it; refusing the observer's pass means nothing picks that photo up until the
    /// next OS slice, or never if the app closes first. That trades a wasteful
    /// duplicate upload for a silently missed photo — against `ui/folders.md`
    /// § Photo backup's own promise that new photos go up by themselves. So a
    /// request arriving mid-pass is *remembered*, not dropped, and the finishing
    /// pass runs once more to pick up whatever its snapshot could not see.
    ///
    /// Returns the total uploaded across this call's pass and any trailing pass.
    public func syncNewPhotos() async throws -> Int {
        guard host != nil, let networkMonitor, modelContext != nil else { return 0 }
        guard await networkMonitor.shouldSyncPhotos() else { return 0 }

        // Refused because a pass is already in flight — which the gate has now
        // asked to run again on its way out, so this caller's photos are not lost.
        guard await gate.claim() else { return 0 }

        isBackingUp = true
        var uploadedAcrossPasses = 0
        do {
            repeat {
                uploadedAcrossPasses += try await runOnePass()
            } while await gate.finish()
        } catch {
            // Release unconditionally. A pass that threw would almost certainly
            // throw again at once, so the trailing re-run is dropped rather than
            // spun — and nothing is lost by dropping it: the asset that failed has
            // no `PhotoBackupRecord`, so the next trigger of any kind re-attempts
            // it. (`defer` cannot `await`, hence the explicit catch.)
            await gate.release()
            isBackingUp = false
            pendingCount = 0
            throw error
        }
        isBackingUp = false
        pendingCount = 0
        return uploadedAcrossPasses
    }

    /// One pass: reset this pass's funnel, walk the library once, ingest what is
    /// new. The caller owns `isBackingUp` and the single-flight gate, so this body
    /// may assume it is the only pass running.
    private func runOnePass() async throws -> Int {
        guard let host, let modelContext else { return 0 }

        // Bumped synchronously at the in-flight edge, before the first `await` past
        // it — convention 14's "initiated synchronously" corollary, so a consumer
        // that read the baseline cannot miss this pass's start. A trailing re-run
        // is its OWN pass and counts as one, which is what lets a witness assert
        // "exactly one trailing pass" instead of inferring it.
        passesStarted += 1
        defer {
            passesCompleted += 1
            // A pass that threw, or was cancelled mid-flight, must not leave a
            // stale "{n} remaining" row on screen: the countdown describes an
            // IN-FLIGHT pass and nothing else, so every exit path clears it.
            pendingCount = 0
        }

        // This pass's funnel starts empty — these describe one pass, not a total.
        lastPassAuthorization = Self.authorizationLabel(
            PHPhotoLibrary.authorizationStatus(for: .readWrite))
        lastPassAssetsSeen = 0
        lastPassAlreadySynced = 0
        lastPassExportFailed = 0
        lastPassIngestFailed = 0
        lastPassUploaded = 0

        // Resolve (creating or adopting) the target set BEFORE the first ingest.
        // A failure here fails the whole pass rather than uploading into a set the
        // nest does not have — the silent-failure mode this replaced.
        let photoSet: FfiPhotoLibrarySet
        do {
            photoSet = try await photoLibrarySet()
        } catch {
            errorMessage = "Could not prepare the photo library folder: \(error.localizedDescription)"
            throw error
        }

        let fetchOptions = PHFetchOptions()
        fetchOptions.sortDescriptors = [NSSortDescriptor(key: "creationDate", ascending: false)]
        let assets = PHAsset.fetchAssets(with: fetchOptions)
        // The first thing worth knowing about a pass that uploads nothing: whether
        // there was anything to upload. A `.limited` Photos authorization returns
        // only user-selected assets — i.e. usually none — and is indistinguishable
        // from an empty library unless this is counted.
        lastPassAssetsSeen = assets.count
        // The pass's starting position. `pendingCount` counts DOWN from here as
        // each asset leaves the loop, so `passProgress` is how far the pass has
        // got — what `photo-backup-sync-progress` publishes while `isBackingUp`.
        pendingCount = assets.count

        var uploaded = 0

        for i in 0..<assets.count {
            let asset = assets.object(at: i)

            // Each asset leaves this iteration by exactly one of four paths —
            // already-synced, export-failed, ingested, ingest-threw — and three
            // of them are a bare `continue` or a `catch`. A decrement on the
            // success path alone would stall the readout precisely on the passes
            // whose progress matters most, so it goes in a `defer`.
            defer { pendingCount = max(0, pendingCount - 1) }

            // Check if already backed up
            let localId = asset.localIdentifier
            let descriptor = FetchDescriptor<PhotoBackupRecord>(
                predicate: #Predicate { $0.localIdentifier == localId }
            )
            if let existing = try? modelContext.fetch(descriptor).first, existing.state == .synced {
                lastPassAlreadySynced += 1
                continue
            }

            // Export asset to temp file
            // A failed export is a silent skip — the one drop point with no banner
            // and no throw, so without this counter an unreadable asset and an empty
            // library are the same observation.
            guard let tempUrl = try? await exportAsset(asset) else {
                lastPassExportFailed += 1
                continue
            }
            defer { try? FileManager.default.removeItem(at: tempUrl) }

            // Strip EXIF/GPS from the exported copy before any byte leaves the
            // device — the privacy step the deleted Swift upload path applied, and
            // the one android's `ExifStripper` still applies on this same
            // photo-backup path (`apps/android.md`). It belongs *here*, on the
            // library ingress, and NOT in the engine: a folder-sync engine must keep
            // the user's files byte-exact (it syncs their disk, it doesn't rewrite
            // it), so only the photo library — where we upload a copy we exported
            // ourselves — is stripped.
            stripImageMetadata(at: tempUrl)

            // Build path: YYYY/MM/filename.ext
            let remotePath = buildRemotePath(asset: asset, filename: tempUrl.lastPathComponent)

            do {
                // Sealed ingest: the host stages the exported file, runs it through
                // the ordinary sealed chunk pipeline + `changes.record` custody
                // upsert, and deletes the staged copy. There is no watch dir and no
                // reconcile pass, so deleting our temp cannot tombstone the ingested
                // file.
                try await host.ingestFile(
                    folderId: photoSet.folderId, sourcePath: tempUrl.path, relativePath: remotePath
                )

                // Record the backup
                let record = PhotoBackupRecord(
                    localIdentifier: localId,
                    sizeBytes: Int(asset.pixelWidth * asset.pixelHeight),
                    mediaType: asset.mediaType == .video ? "video" : "image",
                    creationDate: asset.creationDate ?? .now,
                    state: .synced,
                    remotePath: remotePath
                )
                modelContext.insert(record)

                uploaded += 1
                lastPassUploaded += 1
                completedCount += 1
                uploadsSinceSnapshot += 1

                // Create snapshot periodically (a control-plane call, never engine
                // work — `fauna.snapshots.create` over the same APIClient every
                // other snapshot gesture uses). Same resolved set as the ingest
                // above: snapshotting a set we did not write to is worse than
                // either bug alone.
                if uploadsSinceSnapshot >= snapshotInterval {
                    _ = try? await api?.createSnapshot(folder: photoSet.name)
                    uploadsSinceSnapshot = 0
                }
            } catch {
                lastPassIngestFailed += 1
                errorMessage = "Failed to upload \(tempUrl.lastPathComponent): \(error.localizedDescription)"
            }
        }

        lastBackupDate = .now
        return uploaded
    }

    /// `PHAuthorizationStatus` as the stable string the funnel reports. Spelled
    /// out rather than derived from `rawValue`, so the wire name cannot silently
    /// change under an OS enum reordering.
    static func authorizationLabel(_ status: PHAuthorizationStatus) -> String {
        switch status {
        case .authorized: return "authorized"
        case .limited: return "limited"
        case .denied: return "denied"
        case .restricted: return "restricted"
        case .notDetermined: return "notDetermined"
        @unknown default: return "unknown"
        }
    }

    /// The registered PhotoKit change observer, **retained here**.
    ///
    /// ⚠ `PHPhotoLibrary.register(_:)` keeps only a WEAK reference to its
    /// observer, so the `PhotoLibraryObserver(engine: self)` this used to build
    /// inline at the call site had no owner at all: it was deallocated as
    /// `startObserving()` returned, and `photoLibraryDidChange` never fired
    /// again. That is the whole of "once backup is on, new photos go up by
    /// themselves" (`ui/folders.md` § Photo backup) — a green toggle over an
    /// inert path, which is the exact failure android shipped for months behind
    /// its own green toggle (§ Photo backup, the `autoPhotoBackup` note). The
    /// witness is `test_photo_backup_unattended.py`.
    private var libraryObserver: PhotoLibraryObserver?

    /// Start observing photo library for new additions. Idempotent — the toggle
    /// calls it on every enable, and registering a second observer would double
    /// every pass.
    public func startObserving() {
        guard libraryObserver == nil else { return }
        let observer = PhotoLibraryObserver(engine: self)
        libraryObserver = observer
        PHPhotoLibrary.shared().register(observer)
    }

    // MARK: - Private helpers

    /// Rewrite the exported temp file without its image metadata (EXIF/IPTC,
    /// C2PA preserved) via the shared `stripMediaMetadata` FFI face
    /// (`fauna_media::process::strip_metadata` — lossless container-segment
    /// removal, never a decode/re-encode; file-sync.md § the photo-backup
    /// ingress strip). Non-image / unparseable bytes pass through unchanged, so
    /// no type gate is needed here — bytes the stripper leaves unchanged are
    /// written back not at all, the file is already what we want to send.
    private func stripImageMetadata(at url: URL) {
        guard let data = try? Data(contentsOf: url) else { return }
        let stripped = stripMediaMetadata(raw: data)
        guard stripped != data else { return }
        try? stripped.write(to: url, options: .atomic)
    }

    private func buildRemotePath(asset: PHAsset, filename: String) -> String {
        let calendar = Calendar.current
        let date = asset.creationDate ?? .now
        let year = calendar.component(.year, from: date)
        let month = String(format: "%02d", calendar.component(.month, from: date))
        return "\(year)/\(month)/\(filename)"
    }

    private func exportAsset(_ asset: PHAsset) async throws -> URL? {
        let resources = PHAssetResource.assetResources(for: asset)
        guard let resource = resources.first else { return nil }

        let tempDir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)

        let filename = resource.originalFilename
        let outputUrl = tempDir.appendingPathComponent(filename)

        return try await withCheckedThrowingContinuation { continuation in
            let options = PHAssetResourceRequestOptions()
            options.isNetworkAccessAllowed = true

            PHAssetResourceManager.default().writeData(for: resource, toFile: outputUrl, options: options) { error in
                if let error {
                    continuation.resume(throwing: error)
                } else {
                    continuation.resume(returning: outputUrl)
                }
            }
        }
    }
}

/// The photo-backup **single-flight gate**: at most one pass runs at a time, and a
/// request that arrives while one is running is remembered so the finishing pass
/// runs exactly once more.
///
/// **Why an `actor` and not two `Bool`s on the engine.** `PhotoBackupEngine` is a
/// plain `@Observable class` and FaunaKit is pinned to `swiftLanguageMode(.v5)`
/// (`apps/fauna-apple/Package.swift`), so nothing serializes its properties and the
/// compiler will not say so. The five triggers enter `syncNewPhotos()` from
/// different tasks, and a check-then-set on an unisolated `Bool` is precisely the
/// race this gate exists to remove — it would read "no pass running" in both tasks
/// and let both through. android's twin has exactly that shape today
/// (`PhotoBackupEngine.kt` sets `_isSyncing` *after* testing it).
///
/// `internal` rather than `private` so `FaunaKitTests` can drive the state machine
/// directly: the engine's own pass needs a live `FfiSyncEngineHost`, a
/// `NetworkMonitor` and a `ModelContext`, none of which is injectable, so the gate
/// is the one seam where the coalescing *logic* is unit-testable at all
/// (`PhotoBackupPassGateTests.swift`). The wiring is witnessed by the ios e2e.
actor PhotoBackupPassGate {
    private var running = false
    private var wanted = false

    /// Claim the right to run a pass. `false` ⇒ a pass is already in flight and has
    /// now been asked to run again when it finishes, so the caller must return
    /// rather than start a second one.
    func claim() -> Bool {
        if running {
            wanted = true
            return false
        }
        running = true
        wanted = false
        return true
    }

    /// Finish a pass. `true` ⇒ a request arrived while this pass ran and THIS caller
    /// owns the trailing pass, so `running` deliberately stays set — the gate is
    /// never open between a pass and its re-run. Consuming the flag here is what
    /// collapses a burst of N mid-pass requests into exactly one trailing pass
    /// instead of N.
    func finish() -> Bool {
        if wanted {
            wanted = false
            return true
        }
        running = false
        return false
    }

    /// Open the gate unconditionally, discarding any remembered request — the
    /// throwing path. A pass that failed would almost certainly fail again at once,
    /// and nothing is lost by not re-running: an asset whose ingest threw has no
    /// `PhotoBackupRecord`, so the next trigger re-attempts it.
    func release() {
        running = false
        wanted = false
    }

    /// Whether a pass is in flight, for tests and diagnostics only — never as a
    /// gate (a read-then-act on this would be the very race `claim()` removes).
    var isRunning: Bool { running }
}

/// Observes photo library changes and triggers backup.
class PhotoLibraryObserver: NSObject, PHPhotoLibraryChangeObserver {
    private weak var engine: PhotoBackupEngine?

    init(engine: PhotoBackupEngine) {
        self.engine = engine
    }

    func photoLibraryDidChange(_ changeInstance: PHChange) {
        Task {
            _ = try? await engine?.syncNewPhotos()
        }
    }
}
