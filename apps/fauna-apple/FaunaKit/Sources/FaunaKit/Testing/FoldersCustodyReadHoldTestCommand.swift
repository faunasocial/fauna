import Foundation

// Compiled out of release artifacts (testing.md convention 15): both UniFFI
// exports it drives (`foldersHoldCustodyReadsForTest`,
// `foldersReleaseCustodyReadsForTest`) exist only in the `test-helpers` flavor.
#if DEBUG

/// `folders_hold_custody_reads` / `folders_release_custody_reads` — park or
/// release this process's Folders-page mail-custody reads (shared Rust's
/// `fauna_client_folders::webdav_provision::hold_custody_reads_for_test` over
/// the FFI face), so `folder-webdav-toggle` is read while that read is out
/// (`folders.md` § Errors & edge cases, *Own-tier and mail gates* — blocked
/// "still loading", a press refuses with the same reason).
///
/// An account-store read, so no nest reply hold reaches it; the hold is shared
/// Rust's process-wide gate, so it needs no client and pre-auth is fine. Twins
/// of tui's, linux's, web's and android's arms of the same names. One
/// implementation for macOS + iOS, like `FeedReloadHoldTestCommand`.
public enum FoldersCustodyReadHoldTestCommand {
    public static let holdAction = "folders_hold_custody_reads"
    public static let releaseAction = "folders_release_custody_reads"

    /// Returns the refusal sentence, or `nil` once the hold is armed/released.
    public static func apply(_ action: String) -> String? {
        switch action {
        case holdAction:
            foldersHoldCustodyReadsForTest()
            return nil
        case releaseAction:
            foldersReleaseCustodyReadsForTest()
            return nil
        default:
            return "\(action): not a custody-read hold command"
        }
    }
}

#endif
