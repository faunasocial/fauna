import Foundation
import Testing
@testable import FaunaKit

/// The header pictures' host pick (`profile.md` § Where logic lives → *Bytes direct,
/// as for a relayed post*): this nest's plane for a profile it served, the
/// `relayed_from` home pinned to its stamped identity for a relayed one.
@Suite struct ProfileHeaderPicturesTests {
    @Test func localProfileUsesThisNest() {
        let read = FfiProfileHeaderRead(body: Data(), relayedFrom: nil, homeNestActorId: nil)
        #expect(ProfileHeaderPictures.home(for: read) == nil)
        #expect(ProfileHeaderPictures.home(for: nil) == nil)
    }

    @Test func relayedProfileUsesTheStampedHome() {
        let read = FfiProfileHeaderRead(
            body: Data(), relayedFrom: "https://home.example", homeNestActorId: "ab12")
        let home = ProfileHeaderPictures.home(for: read)
        #expect(home == FfiPostBlobHome(url: "https://home.example", nestActorId: "ab12"))
    }

    @Test func relayedWithoutIdentityKeepsTheWebPkiFloor() {
        let read = FfiProfileHeaderRead(
            body: Data(), relayedFrom: "https://home.example", homeNestActorId: nil)
        #expect(ProfileHeaderPictures.home(for: read)?.nestActorId == nil)
    }
}
