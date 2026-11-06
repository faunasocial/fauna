import Testing
import Foundation
@testable import FaunaKit

// The tier_1 pin for `nest-trust-holders-status` (`nests.md:125`): a roster the
// nest could not be asked for is not an empty one, so the home row's Now lens
// says so and withholds `nest-trust-empty`. Mirrors the tui's
// `an_unreadable_holder_roster_replaces_the_empty_state` /
// `a_readable_holder_roster_keeps_the_empty_state`.
@Suite("Nest trust holders status")
struct NestTrustHoldersStatusTests {
    @Test("an unreadable holder roster replaces the empty state")
    func unreadableRosterReplacesEmptyState() {
        #expect(trustHoldersStatusText(unreachable: true) == L.nests.holdersStatusUnreachable)
        #expect(!trustHoldersStatusText(unreachable: true).isEmpty)
        #expect(!showsTrustEmptyState(grants: 0, backups: 0, holdersUnreachable: true))
    }

    @Test("a readable holder roster keeps the empty state")
    func readableRosterKeepsEmptyState() {
        #expect(trustHoldersStatusText(unreachable: false).isEmpty)
        #expect(showsTrustEmptyState(grants: 0, backups: 0, holdersUnreachable: false))
    }

    @Test("grants or backups still suppress the empty state")
    func grantsOrBackupsSuppressEmptyState() {
        #expect(!showsTrustEmptyState(grants: 1, backups: 0, holdersUnreachable: false))
        #expect(!showsTrustEmptyState(grants: 0, backups: 1, holdersUnreachable: false))
    }
}
