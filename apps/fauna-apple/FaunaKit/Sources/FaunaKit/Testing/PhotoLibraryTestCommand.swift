import Foundation
import Photos

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it.
#if DEBUG

/// The macOS photo-backup venue's two fixture seams over the REAL System Photo
/// Library — `fauna_e2e_agent::PHOTO_BACKUP_SEED_LIBRARY` and
/// `fauna_e2e_agent::PHOTO_BACKUP_REQUEST_ACCESS`.
///
/// **Why these exist at all.** The iOS witnesses seed the simulator's own photo
/// library from outside the app (`simctl addmedia`) and pre-grant Photos by
/// writing the throwaway device's `TCC.db`. macOS has neither: there is no
/// `simctl` for the host, and the user TCC store is SIP-protected. So on macOS the
/// photo goes in through PhotoKit itself — a real `PHAssetCreationRequest`
/// against the real library `photolibraryd` serves, exactly the store the engine
/// then reads with `PHAsset.fetchAssets` — and the Photos grant is the one human
/// "Allow" the venue asks for, given once to a stably signed test bundle id
/// (`tests/e2e-unified/drivers/macos.py` § the photo-library launch mode;
/// `e2e-conventions.md` convention 12's macOS arm).
///
/// **Nothing here stands in for the product.** The engine's path is untouched:
/// these commands put a real asset into the real library and ask the real OS for
/// the real grant. What they do not prove is stated where the harness meets it —
/// the seeding happens inside the app process rather than from a camera, so the
/// change observer is told by the same process that made the change.
///
/// Shared FaunaKit (priority #2) like every other test command; only the macOS
/// shell dispatches it today, because the iOS driver seeds and grants from outside
/// the app, which is the stronger shape where it is available.
public enum PhotoLibraryTestCommand {
    /// Same data-not-callbacks shape as `CustodianPullTestCommand.Outcome`, so the
    /// shell's switch arm owns the result-slot rule in one place.
    public enum Outcome: Equatable {
        /// The command ran; the JSON is what the shell stashes in the result slot.
        case report(String)
        /// The command cannot be honoured; the shell surfaces it as a LOUD TestAgent
        /// failure (convention 11) and leaves the result slot empty.
        case refused(String)
    }

    /// `photo_backup_seed_library` — add the image at `command["path"]` to the
    /// System Photo Library as a new asset whose original filename is the file's
    /// own (the backed-up copy is named after it, which is how a test finds its
    /// photo in Media), and PROVE it landed by fetching it back.
    ///
    /// Refuses rather than prompts when the grant is missing: `performChanges`
    /// under `notDetermined` raises the Photos alert and blocks until a human
    /// answers, which would hang an unattended run on a dialog nobody is watching.
    @MainActor
    public static func seed(_ command: [String: Any]) async -> Outcome {
        guard let path = command["path"] as? String, !path.isEmpty else {
            return .refused("photo_backup_seed_library: no `path` in the command")
        }
        let url = URL(fileURLWithPath: path)
        guard FileManager.default.isReadableFile(atPath: path) else {
            return .refused("photo_backup_seed_library: \(path) is not a readable file")
        }
        let status = PHPhotoLibrary.authorizationStatus(for: .readWrite)
        guard status == .authorized || status == .limited else {
            return .refused(
                "photo_backup_seed_library: Photos access is "
                + "\(PhotoBackupEngine.authorizationLabel(status)), so adding a photo would "
                + "raise the system Photos prompt and block. The venue's one-time grant is "
                + "missing — run `just mac-photos-e2e-grant` and click Allow")
        }

        let placeholder = PlaceholderBox()
        do {
            try await PHPhotoLibrary.shared().performChanges {
                let options = PHAssetResourceCreationOptions()
                options.originalFilename = url.lastPathComponent
                options.shouldMoveFile = false
                let request = PHAssetCreationRequest.forAsset()
                request.addResource(with: .photo, fileURL: url, options: options)
                placeholder.localIdentifier = request.placeholderForCreatedAsset?.localIdentifier
            }
        } catch {
            return .refused(
                "photo_backup_seed_library: PhotoKit refused to add \(url.lastPathComponent): "
                + "\(error.localizedDescription)")
        }

        guard let localId = placeholder.localIdentifier else {
            return .refused(
                "photo_backup_seed_library: the change request produced no asset placeholder "
                + "for \(url.lastPathComponent)")
        }
        // The fetch-back is the proof, for the same reason the iOS driver counts
        // `simctl addmedia`'s output: an add that "succeeds" into nothing is a
        // fixture failure a test would otherwise read as a lost upload.
        let fetched = PHAsset.fetchAssets(withLocalIdentifiers: [localId], options: nil)
        guard fetched.count == 1 else {
            return .refused(
                "photo_backup_seed_library: \(url.lastPathComponent) was accepted as \(localId) "
                + "but fetching it back found \(fetched.count) asset(s)")
        }
        return .report(json([
            "local_identifier": localId,
            "filename": url.lastPathComponent,
            "library_count": PHAsset.fetchAssets(with: nil).count,
        ]))
    }

    /// `photo_backup_request_access` — ask the OS for Photos read-write access and
    /// report its answer. Already decided (the ordinary case after the venue's
    /// one-time grant), this returns at once without any prompt; undecided, it
    /// raises the real system alert and returns once a human has answered it,
    /// which is what the grant recipe waits on.
    @MainActor
    public static func requestAccess() async -> Outcome {
        let status = await withCheckedContinuation { continuation in
            PHPhotoLibrary.requestAuthorization(for: .readWrite) { continuation.resume(returning: $0) }
        }
        return .report(json(["authorization": PhotoBackupEngine.authorizationLabel(status)]))
    }

    private final class PlaceholderBox: @unchecked Sendable {
        var localIdentifier: String?
    }

    private static func json(_ value: [String: Any]) -> String {
        guard let data = try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]),
              let text = String(data: data, encoding: .utf8) else { return "{}" }
        return text
    }
}

#endif
