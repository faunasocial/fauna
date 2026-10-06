import Testing
import Foundation
@testable import FaunaKit

@Test func syncConfigDefaults() {
    let config = SyncConfiguration.platformDefault
    #expect(config.maxConcurrentUploads > 0)
    #expect(config.maxInMemoryFileSize > 0)
}

@Test func syncFileStatesAreDefined() {
    let states: [SyncFileState] = [.synced, .uploading, .downloading, .localOnly, .remoteOnly, .conflict]
    #expect(states.count == 6)
    #expect(SyncFileState.synced != SyncFileState.conflict)
}

/// Every stored photo-backup state maps onto the shared six-state display
/// vocabulary — the map behind `sync-state-badge` (`file-sync.md` § Per-file
/// sync-status display). A 1:1 map, so no state can render as another.
@Test func syncFileStateMapsToDistinctDisplayState() {
    let pairs: [(SyncFileState, SyncDisplayState)] = [
        (.synced, .synced), (.localOnly, .localOnly), (.remoteOnly, .remoteOnly),
        (.uploading, .uploading), (.downloading, .downloading), (.conflict, .conflict),
    ]
    for (stored, display) in pairs {
        #expect(stored.displayState == display)
    }
}

/// Smoke-checks the shared `stripMediaMetadata` FFI binding round-trips Data
/// correctly from Swift; the real strip/preserve coverage (JPEG/PNG, C2PA)
/// lives in `libs/fauna-media/tests/process_test.rs` — no client re-tests it.
@Test func stripMediaMetadataPassesThroughNonImage() {
    let data = Data("not an image".utf8)
    #expect(stripMediaMetadata(raw: data) == data)
}
