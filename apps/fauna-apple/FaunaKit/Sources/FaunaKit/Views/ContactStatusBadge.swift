import SwiftUI

/// The contact relationship-status capsule — the single apple-family shape for
/// the `contact-status` badge (`docs/goal/ui/contacts.md` § Where logic lives →
/// Status badge text). The **label text** comes from the shared
/// `fauna_core::format::contact_status_label` (UniFFI `contactStatusLabel(status:)`,
/// i18n `common.{pending,accepted,confirmed,blocked}`; unknown status →
/// capitalized verbatim), so the status→label vocabulary can't drift per-app;
/// the **color** is an idiomatic client-side render (kept here in the apple-family
/// shared layer because both targets render the same palette — the same split as
/// `AttendeeRow`'s `rsvpStatusLabel` text + `rsvpStatusColor` color).
///
/// Shared by iOS (`ContactsView`) and macOS (`MacContactListView`,
/// `MacContactDetailView`) so the shape lives once (priority #1/#2/#4) — replaces
/// three byte-identical private badge structs. The status-grouping section
/// headers feed the same shared label so the heading and the badge can't diverge
/// (matching web's `status-heading` + `contact-status` consume). Carries no
/// automation id itself — each call site registers `contact-status`, reading the
/// same localized `contactStatusLabel` text this badge paints (fixed: it used to read the raw wire `status`, which drifted
/// from what was on screen and from every other app's `contact-status` read).
public struct ContactStatusBadge: View {
    /// The roster-grouping display order (`docs/goal/ui/contacts.md` § Contact
    /// roster filter) — was independently hardcoded in `ContactsView` (iOS) and
    /// `MacContactListView` (macOS), a third copy of the same four-status list
    /// this file's own `color` switch already carries; one door so a status
    /// added to one list can't silently miss the others.
    public static let displayOrder: [String] = ["confirmed", "accepted", "pending", "blocked"]

    let status: String

    public init(status: String) {
        self.status = status
    }

    var color: Color {
        switch status {
        case "confirmed": .accentColor
        case "accepted": .green
        case "pending": .orange
        case "blocked": .red
        default: .secondary
        }
    }

    public var body: some View {
        Text(renderLocalizedText(contactStatusLabel(status: status)))
            .tintedCapsuleBadge(color)
    }
}
