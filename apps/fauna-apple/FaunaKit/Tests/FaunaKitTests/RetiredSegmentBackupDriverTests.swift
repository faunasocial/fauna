import Testing
@testable import FaunaKit

// Headless witnesses that apple's in-app segment-backup upload driver STAYS
// retired (slice-5 flip, 2026-08-15 — `docs/goal/behavior/backup-restore.md`
// § Background Tasks → *Flip status (slice 5)*). The source nest has been the
// segment-backup writer since 2026-07-24; an app that still ships an upload
// driver can only double-write or diverge. The apple peer of android's
// `RetiredSegmentBackupDriverTest`.
//
// These replace `BackupUploadDriverTests.swift`, which pinned the deleted
// driver's iOS-safety invariant ("inert until activate"). That invariant is now
// vacuous — the type is gone on both platforms — so what is worth pinning is the
// *absence*, in the two directions a later session could silently undo it.
//
// Division of labour with shared Rust: what an arm of `FfiHeavyTaskCapability`
// *means* (which kind set it maps to, at both values of `CLIENT_BUILDS_INDEX`)
// is pinned by `libs/fauna-ffi/src/task_delegation.rs`'s `capability_tests`.
// What only apple can see is **which arm this app passes**, which is what these
// assert — via the single `TaskDelegationVM.forThisBuild` the picker itself
// reads, so the test cannot pass while the real page declares something else.

/// The capability declaration is the half that strands users if it rots.
///
/// Deleting driver code while leaving the declaration behind is not a partial
/// fix — it is the *worse* state: the Task-delegation picker keeps offering a
/// `backup-upload` "This device" self-pin that nothing on the box can ever
/// honour, so a user who takes it silently stops being backed up by anyone.
/// That is what linux shipped for four days in 2026-07 (`participants.md` § The
/// assignment picker), and it is invisible to any test that only checks the
/// driver classes are gone.
@Test @MainActor func appleDeclaresNoBackupUploadRunner() {
    #if os(macOS)
    // macOS retracted `backup-upload` but still builds the content index.
    #expect(
        TaskDelegationVM.forThisBuild == .indexOnly,
        """
        macOS must declare `.indexOnly` — it no longer ships a segment-backup \
        upload driver (slice-5 flip), so declaring `.runner` would offer a \
        `backup-upload` self-pin that can only ever wait
        """)
    #else
    #expect(
        TaskDelegationVM.forThisBuild == .viewerOnly,
        "iOS is always battery-mobile and runs no heavy task kind")
    #endif
}

/// iOS's `social.fauna.sync.upload` `BGProcessingTask` is a NARROWING, not a
/// cancel — the sharp edge of this whole arm.
///
/// That task id is **shared with photo backup**. The mail leg was removed from
/// `handleUploadTask`; the registration, the submit and the reschedule all stay,
/// because retiring the id would silently kill photo backup instead. A later
/// cleanup pass that "finishes the job" by dropping the id is the regression
/// this pins.
@Test func iOSKeepsTheSharedUploadTaskIdForPhotoBackup() {
    #expect(
        AppleIdentifiers.BackgroundTask.upload == "social.fauna.sync.upload",
        """
        the upload BGProcessingTask id is shared with photo backup — the \
        slice-5 flip narrowed its handler to the photo leg and must never \
        retire the id itself
        """)
}
