import Foundation
import Testing
@testable import FaunaKit

// The nest-place editor's refresh rule (backup-restore.md § 8b, "a refresh never
// overwrites a staged edit"). The decision is shared Rust's `nestPlaceReseed`;
// these pin the Swift wiring — twin of linux's
// `a_rebuild_keeps_a_staged_nest_place_edit`.

private func row(snapshots: Bool?, quiet: Int64?) -> NestPlaceForm {
    nestPlaceFormFromRow(
        nestSnapshots: snapshots,
        nestSnapshotQuietSecs: quiet,
        retentionPolicy: nil,
        versionRetentionMaxVersions: 0,
        versionRetentionMaxAgeDays: 0
    )
}

@Suite struct NestPlaceBuffersTests {
    @Test func firstRefreshPaintsTheRow() {
        var buffers = NestPlaceBuffers()
        let fresh = row(snapshots: true, quiet: 30)
        buffers.refresh(fresh: fresh)
        #expect(buffers.form == fresh)
    }

    @Test func aChangedPolicyKeepsAStagedEdit() {
        var buffers = NestPlaceBuffers()
        buffers.refresh(fresh: row(snapshots: true, quiet: 30))
        buffers.form.policy.quietSecs = "99"  // the user types
        buffers.refresh(fresh: row(snapshots: false, quiet: 60))  // another device changed the policy
        #expect(buffers.form.policy.quietSecs == "99")
    }

    @Test func anUntouchedFormFollowsAChangedPolicy() {
        var buffers = NestPlaceBuffers()
        buffers.refresh(fresh: row(snapshots: true, quiet: 30))
        let changed = row(snapshots: false, quiet: 60)
        buffers.refresh(fresh: changed)
        #expect(buffers.form == changed)
    }

    @Test func aSaveReleasesTheBoxesToTheNestsStoredValue() {
        var buffers = NestPlaceBuffers()
        buffers.refresh(fresh: row(snapshots: true, quiet: 30))
        buffers.form.policy.quietSecs = "99"
        buffers.released()
        let stored = row(snapshots: true, quiet: 99)
        buffers.refresh(fresh: stored)
        #expect(buffers.form == stored)
    }
}
