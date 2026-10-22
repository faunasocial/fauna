import Foundation

/// The nest-place editor's six staged buffers plus the form last painted from
/// the row — the state `nestPlaceReseed` compares. A value type so the refresh
/// rule is unit-drivable without a view (twin of linux's `NestPlaceEditor`
/// rebuild test and android's injected-rule parameter).
///
/// Every decision lives in shared Rust (`backup-restore.md` § 8b, "a refresh
/// never overwrites a staged edit"); this is the transcription.
struct NestPlaceBuffers: Equatable {
    /// What the six boxes hold right now, typed values included.
    var form: NestPlaceForm
    /// What the boxes were last painted with; `nil` before the first paint and
    /// after a save released them.
    private(set) var seeded: NestPlaceForm?

    init() {
        form = nestPlaceFormFromRow(
            nestSnapshots: nil,
            nestSnapshotQuietSecs: nil,
            retentionPolicy: nil,
            versionRetentionMaxVersions: 0,
            versionRetentionMaxAgeDays: 0
        )
        seeded = nil
    }

    /// A refreshed row arrived: paint it unless the user has staged an edit.
    mutating func refresh(fresh: NestPlaceForm) {
        if let paint = nestPlaceReseed(seeded: seeded, onScreen: form, fresh: fresh) {
            form = paint
            seeded = paint
        }
    }

    /// The save is being sent: release the boxes so its own refresh shows what
    /// the nest stored.
    mutating func released() {
        seeded = nil
    }
}
