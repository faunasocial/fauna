import Testing
import SwiftUI
@testable import FaunaKit

// Pins the shared `rsvpStatusColor` mapping lifted from the per-platform
// EventDetail views. The regression guard that matters: `waitlisted → orange`
// (the iOS copy silently lacked it and fell through to `.secondary`), and that
// unknown statuses still fall back to the same color as `invited` (`.secondary`).

@Test func rsvpStatusColorMapsKnownStatuses() {
    #expect(rsvpStatusColor("going") == .green)
    #expect(rsvpStatusColor("interested") == .yellow)
    #expect(rsvpStatusColor("declined") == .red)
    #expect(rsvpStatusColor("waitlisted") == .orange)   // iOS previously missed this
}

@Test func rsvpStatusColorFallsBackForInvitedAndUnknown() {
    // `invited` and any unrecognized status share the `.secondary` fallback;
    // compare them to each other to avoid asserting on the dynamic `.secondary`
    // literal directly.
    #expect(rsvpStatusColor("invited") == rsvpStatusColor("not-a-real-status"))
}
