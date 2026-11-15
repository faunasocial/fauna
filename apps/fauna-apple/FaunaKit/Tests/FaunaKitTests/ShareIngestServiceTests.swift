import FaunaFFISwift
import Foundation
import Testing

@testable import FaunaKit

// The share plane's ingest door crosses from the iOS app to its File Provider
// extension as an XPC reply (`ShareIngestService`; `p2p-shared-set-build.md`
// § Phone peers — design, decision 3). The reply's codec is pure, so every
// field of the host's outcome is pinned to survive the crossing.
#if !FAUNA_EXCISE_P2P_SHARE && canImport(FileProvider)

    @Test func anIngestOutcomeSurvivesTheServiceChannel() throws {
        let outcome = FfiShareIngestOutcome(
            refused: 1, overlaid: 4, materialized: 2, alreadyCurrent: 3, cursor: 42,
            storageLimited: true)
        let crossed = try ShareIngestReply.decode(ShareIngestReply(outcome).encoded())
        #expect(crossed.outcome == outcome)
    }

    @Test func onlyAPageThatRecordedSomethingSignalsTheEnumerator() {
        let quiet = ShareIngestReply(
            FfiShareIngestOutcome(
                refused: 2, overlaid: 0, materialized: 0, alreadyCurrent: 5, cursor: 7,
                storageLimited: false))
        #expect(!quiet.changedTheTree)
        let landed = ShareIngestReply(
            FfiShareIngestOutcome(
                refused: 0, overlaid: 1, materialized: 0, alreadyCurrent: 0, cursor: 8,
                storageLimited: false))
        #expect(landed.changedTheTree)
    }

    @Test func anUnreadableReplyIsAnErrorNotAnEmptyOutcome() {
        #expect(throws: (any Error).self) {
            try ShareIngestReply.decode(Data("not a reply".utf8))
        }
    }

#endif
