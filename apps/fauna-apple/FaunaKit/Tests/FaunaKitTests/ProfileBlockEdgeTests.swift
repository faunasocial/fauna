import Testing
@testable import FaunaKit

// Pins the profile block toggle's not-yet-known edge (`profile.md` § User
// actions → `profile-block-button`); mirrors tui
// `the_toggle_is_inert_until_the_open_time_read_lands`.
@Suite struct ProfileBlockEdgeTests {
    @Test func toggleIsInertUntilTheOpenTimeReadLands() {
        var edge = ProfileBlockEdge()
        #expect(!edge.isToggleEnabled)
        #expect(!edge.labelBlocked)

        edge.applyRead(blocked: true)
        #expect(edge.isToggleEnabled)
        #expect(edge.labelBlocked)
    }

    @Test func aReadOfNotBlockedEnablesTheToggle() {
        var edge = ProfileBlockEdge()
        edge.applyRead(blocked: false)
        #expect(edge.isToggleEnabled)
        #expect(!edge.labelBlocked)
    }

    @Test func aLateReadDoesNotRewindThePress() {
        var edge = ProfileBlockEdge()
        edge.applyPress(blocked: true)
        edge.applyRead(blocked: false)
        #expect(edge.isBlocked == true)
    }

    @Test func resetReturnsToNotYetKnown() {
        var edge = ProfileBlockEdge()
        edge.applyPress(blocked: true)
        edge.reset()
        #expect(!edge.isToggleEnabled)
        edge.applyRead(blocked: false)
        #expect(edge.isBlocked == false)
    }
}
