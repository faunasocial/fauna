import SwiftUI

/// The badge color for an attendee's RSVP status, shared by the macOS
/// (`MacEventDetailView`) and iOS (`EventDetailView`) event-detail attendee rows
/// so the mapping lives once (priority #1/#4). `status` is the wire RSVP value
/// from a `fauna.calendar` attendee record.
///
/// Lifted from the per-platform copies, which had drifted: the macOS
/// `statusColor` carried a `waitlisted → orange` case the iOS `rsvpColor` was
/// silently missing (it fell through to `.secondary`). This is the richer macOS
/// mapping, now applied to both.
public func rsvpStatusColor(_ status: String) -> Color {
    switch status {
    case "going": .green
    case "interested": .yellow
    case "declined": .red
    case "invited": .secondary
    case "waitlisted": .orange
    default: .secondary
    }
}
