import Foundation
import Testing
@testable import FaunaKit

// iOS's `FfiCloudBackupExcluder` implementation (the custodian store's, and
// since 2026-08-26 the account store's). Android's already-built leg
// singled out this callback as the one place this device's real risk lives,
// and demanded a headless assertion rather than inspection: "after the
// build, URL.resourceValues(forKeys: [.isExcludedFromBackupKey]) on the
// created root must report true." Only iOS actually wires this class up, but
// `isExcludedFromBackup` is plain cross-platform Foundation, so these run for
// real via `swift test` on macOS.

@Test func excludeActuallyFlipsTheResourceValue() throws {
    let root = FileManager.default.temporaryDirectory
        .appendingPathComponent("custodian-excluder-test-\(UUID().uuidString)", isDirectory: true)
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: root) }

    // Not excluded yet — a fresh directory has no explicit resource value set.
    // A FRESH `URL` instance per read, never `root` reused: `URL` caches
    // resource values it has already fetched, so reusing the same value for
    // the post-exclude read would silently report the pre-exclude snapshot
    // regardless of what `exclude(root:)` actually did on disk.
    let before = try URL(fileURLWithPath: root.path, isDirectory: true)
        .resourceValues(forKeys: [.isExcludedFromBackupKey])
    #expect(before.isExcludedFromBackup != true)

    try CloudBackupExcluder().exclude(root: root.path)

    let after = try URL(fileURLWithPath: root.path, isDirectory: true)
        .resourceValues(forKeys: [.isExcludedFromBackupKey])
    #expect(
        after.isExcludedFromBackup == true,
        """
        exclude(root:) must actually flip isExcludedFromBackup on the real URL resource — \
        a store whose exclusion silently no-ops looks identical to a correctly excluded \
        one, which is exactly what build_custodian_host trusts this call to prevent
        """)
}

@Test func excludeThrowsAnFfiErrorOnAPathThatCannotBeSet() {
    // A path with no such directory: `setResourceValues` fails, and the
    // failure must surface as a typed `FfiError` (not an unexpected-error
    // status) so the Rust side's "abort the whole build" logs a real message.
    let missing = FileManager.default.temporaryDirectory
        .appendingPathComponent("custodian-excluder-missing-\(UUID().uuidString)", isDirectory: true)

    #expect(throws: FfiError.self) {
        try CloudBackupExcluder().exclude(root: missing.path)
    }
}
