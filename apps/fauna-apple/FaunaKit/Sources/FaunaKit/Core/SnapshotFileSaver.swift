import Foundation
#if os(macOS)
import AppKit
#endif

/// Saves downloaded file bytes — the backups single-file download
/// (backup-restore.md § 3, ui/backups.md § Where logic lives → Single-file byte
/// download) and the Media detail's `media-item-detail-download-button`
/// (ui/media.md § Element IDs).
///
/// Under e2e, bytes are written straight into a fixed directory with no
/// dialog; otherwise the platform-native save mechanism (`NSSavePanel` on
/// macOS, a share sheet on iOS). ``save(suggestedFileName:data:)`` is that
/// whole choice for a plain download; a caller with its own naming or
/// reporting needs (a calendar export, the account export) composes the e2e
/// half below with its own presentation. Mirrors windows' `ISnapshotFileSaver`
/// / `DirectorySnapshotFileSaver` split (`FaunaApp.Core/Services/
/// ISnapshotFileSaver.cs`).
public enum SnapshotFileSaver {
    /// Hand downloaded `data` to the platform's save path under
    /// `suggestedFileName`: dialog-less into `e2eDownloadDir` under e2e, else
    /// `NSSavePanel` on macOS (a cancelled panel saves nothing, no error) or
    /// the share sheet on iOS. Throws a write failure so the caller can say so.
    @MainActor
    public static func save(suggestedFileName: String, data: Data) throws {
        if let dir = e2eDownloadDir {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            try data.write(to: dir.appendingPathComponent(suggestedFileName))
            return
        }
        #if os(macOS)
        let panel = NSSavePanel()
        panel.nameFieldStringValue = suggestedFileName
        guard panel.runModal() == .OK, let url = panel.url else { return }
        try data.write(to: url)
        #elseif os(iOS)
        let tempUrl = FileManager.default.temporaryDirectory.appendingPathComponent(suggestedFileName)
        try data.write(to: tempUrl)
        ShareSheet.present(items: [tempUrl])
        #endif
    }

    /// Non-nil under e2e (`FaunaE2E.isActive`): `FAUNA_E2E_DOWNLOAD_DIR`,
    /// which the macos/ios e2e drivers always set fresh per launch (mirrors
    /// `FAUNA_E2E_CREDENTIAL_DIR`) — never a Swift-side default derived from
    /// `.applicationSupportDirectory`, which resolves against the real `~`
    /// unless `CFFIXED_USER_HOME` is pinned and would leak concurrent e2e
    /// runs into one shared directory.
    /// **The whole read is `#if DEBUG`, not just the `FaunaE2E.isActive`
    /// predicate** (convention 15, `e2e-automation-surface-gating.md` § The
    /// convention). This is a *redirect* of the user's exported data, so severity
    /// is the payload's: a Release build that merely evaluates a predicate to
    /// `false` is relying on an optimizer to fold the branch, which this family
    /// does not accept as a gate — it is the tui `backups.rs::download_dir()`
    /// defect, whose fix is the same two-gates-both-required shape.
    public static var e2eDownloadDir: URL? {
        #if DEBUG
        guard FaunaE2E.isActive, let dir = E2eEnv.downloadDir else { return nil }
        return URL(fileURLWithPath: dir)
        #else
        return nil
        #endif
    }

    /// Writes `data` into `e2eDownloadDir`, bypassing any dialog. Callers
    /// check `e2eDownloadDir != nil` first and call this instead of
    /// presenting the native panel/share sheet. Returns whether the write
    /// succeeded.
    @discardableResult
    public static func saveForE2E(suggestedFileName: String, data: Data) -> Bool {
        guard let dir = e2eDownloadDir else { return false }
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            try data.write(to: dir.appendingPathComponent(suggestedFileName))
            return true
        } catch {
            return false
        }
    }

    /// `saveForE2E` for a file the user may produce more than once (a calendar's
    /// `.ics` export): a name already taken in `e2eDownloadDir` is never
    /// overwritten — the file lands beside it as `Name 2.ext`, `Name 3.ext`, ….
    /// Returns the URL written, so the caller can name it to the user.
    @discardableResult
    public static func saveForE2ENonOverwriting(suggestedFileName: String, data: Data) -> URL? {
        guard let dir = e2eDownloadDir else { return nil }
        return writeNonOverwriting(into: dir, suggestedFileName: suggestedFileName, data: data)
    }

    /// The write itself, apart from the e2e gate so it is unit-testable: creates
    /// `dir` if needed, then takes the first free name. `.withoutOverwriting` makes
    /// the create-if-absent atomic, so two racing exports cannot both win one name.
    static func writeNonOverwriting(into dir: URL, suggestedFileName: String, data: Data) -> URL? {
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        } catch {
            return nil
        }
        let stem = (suggestedFileName as NSString).deletingPathExtension
        let ext = (suggestedFileName as NSString).pathExtension
        for n in 1...1000 {
            let name = n == 1 ? suggestedFileName : (ext.isEmpty ? "\(stem) \(n)" : "\(stem) \(n).\(ext)")
            let url = dir.appendingPathComponent(name)
            do {
                try data.write(to: url, options: .withoutOverwriting)
                return url
            } catch let error as CocoaError where error.code == .fileWriteFileExists {
                continue
            } catch {
                return nil
            }
        }
        return nil
    }
}
