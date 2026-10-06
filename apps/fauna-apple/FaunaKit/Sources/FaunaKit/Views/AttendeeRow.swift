import SwiftUI

/// The canonical event-attendee row — the single 6-client attendee-list shape
/// (user decision 2026-06-23; `docs/goal/ui/events.md` § Attendee list
/// presentation). Replaces the prior undocumented split where web + macOS drew
/// wrapping badge/capsule chips while the other four drew plain rows (and linux
/// omitted inline RSVP). Rows were chosen because neither prior shape was a
/// strict superset and a row scales to a long roster where chips wrap into a
/// dense block.
///
/// Leading → trailing: a monogram avatar (attendees carry no avatar URL — a
/// CalDAV ATTENDEE is just CN + email + PARTSTAT — so we render the initial of
/// the display name), the display name (CN, falling back to the email), the
/// email beneath the name (omitted when the name *is* the email), and a colored
/// RSVP-status capsule: the **label text** from the shared
/// `fauna_core::ical::rsvp_status_label` (UniFFI `rsvpStatusLabel(status:)`,
/// i18n `events.rsvp.*`), the **color** from the client-side `rsvpStatusColor`.
///
/// The display-name fallback, the monogram initial, and the email-beneath
/// visibility are all derived by the shared Rust projection
/// `fauna_core::ical::attendee_display` (UniFFI `attendeeDisplay(name:email:)`),
/// so all 7 apps compute them identically (events.md § Attendee list
/// presentation / § Implementation status). Only the `rsvpStatusColor` (color)
/// stays client-side — the status label is now single-sourced too.
/// `secondaryEmail` is `nil` when CN == email (no double email), replacing the
/// buggy `!name.isEmpty` test.
///
/// Shared by macOS (`MacEventDetailView`) and iOS (`EventDetailView`) so the
/// shape lives once (priority #1/#2/#4). Carries the `attendee-item` automation
/// id; the automation read text stays the display name, unchanged from the
/// per-platform rows it replaces. `attendee-id` (the display-name text) and
/// `attendee-status` (the RSVP capsule text) are BARE texts — no parens/color —
/// matching web/windows/tui; `.accessibilityElement
/// (children: .contain)` on the row keeps `attendee-item` queryable alongside
/// them (the container-clobbers-children trap every indexed row here avoids).
public struct AttendeeRow: View {
    let attendee: Attendee

    public init(attendee: Attendee) {
        self.attendee = attendee
    }

    public var body: some View {
        let display = attendeeDisplay(name: attendee.name, email: attendee.email)
        let name = display.displayName
        return HStack(spacing: 10) {
            Text(display.monogram)
                .font(.caption.weight(.semibold))
                .foregroundStyle(.white)
                .frame(width: 28, height: 28)
                .background(Circle().fill(Color.accentColor))

            VStack(alignment: .leading, spacing: 1) {
                automationText(Ids.attendeeId, name)
                    .font(.callout)
                    .lineLimit(1)
                if let secondaryEmail = display.secondaryEmail {
                    Text(secondaryEmail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }

            Spacer(minLength: 8)

            automationText(Ids.attendeeStatus, renderLocalizedText(rsvpStatusLabel(status: attendee.rsvp)))
                .font(.caption2.weight(.semibold))
                .padding(.horizontal, 8)
                .padding(.vertical, 2)
                .background(rsvpStatusColor(attendee.rsvp).opacity(0.2))
                .foregroundStyle(rsvpStatusColor(attendee.rsvp))
                .clipShape(Capsule())
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.attendeeItem)
        .automationValue(Ids.attendeeItem, text: { name })
    }
}
