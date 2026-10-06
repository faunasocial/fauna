import SwiftUI

/// The canonical agenda-list event row: tappable summary/time/location (the
/// `event-card` component) plus an inline `rsvp-button-group` quick action
/// (`event-rsvp-going`/`-interested`/`-decline`) — a child of `event-card` in
/// ui.yaml, shown to EVERY viewer, the organizer included (ratified
/// 2026-06-29, `docs/goal/ui/events.md:22`; no `organizedByMe` gate, matching
/// the already organizer-inclusive `event-detail-rsvp-*` trio).
///
/// Inline card-level RSVP is the richest existing cross-app pattern —
/// web/linux/windows already show it on every agenda row; android is the
/// outlier, gating it to a separate "invited events" quick-list only
/// (priority #4: converge onto the richer shape, don't match the narrower
/// one). Shared by iOS (`CalendarListView`) and macOS (`MacEventListView`) so
/// the row shape lives once (priority #1/#2).
public struct EventCardRow: View {
    let event: EventSummary
    let onSelect: () -> Void
    let onRsvp: (RsvpResponse) -> Void

    public init(event: EventSummary, onSelect: @escaping () -> Void, onRsvp: @escaping (RsvpResponse) -> Void) {
        self.event = event
        self.onSelect = onSelect
        self.onRsvp = onRsvp
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Button(action: onSelect) {
                VStack(alignment: .leading, spacing: 2) {
                    automationText(Ids.eventCardSummary, event.summary)
                        .fontWeight(.semibold)
                    Text("\(event.dtstart) – \(event.dtend)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if let loc = event.location {
                        Text(loc)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .tint(.primary)
            .accessibilityIdentifier(Ids.eventCard)
            .automationActivate(Ids.eventCard) { onSelect() }

            HStack(spacing: 8) {
                Button(L.events.rsvp.going) { onRsvp(.going) }
                    .accessibilityIdentifier(Ids.eventRsvpGoing)
                    .automationActivate(Ids.eventRsvpGoing) { onRsvp(.going) }
                    .tint(.green)
                Button(L.events.rsvp.interested) { onRsvp(.interested) }
                    .accessibilityIdentifier(Ids.eventRsvpInterested)
                    .automationActivate(Ids.eventRsvpInterested) { onRsvp(.interested) }
                Button(L.common.decline) { onRsvp(.declined) }
                    .accessibilityIdentifier(Ids.eventRsvpDecline)
                    .automationActivate(Ids.eventRsvpDecline) { onRsvp(.declined) }
                    .tint(.red)
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            // RSVP is read-mutate-rewrite: it re-PUTs the canonical VEVENT, so
            // the kind that decides it is the WRITE, not the read it opens with
            // (tui's `events::Action::wire_kind` states the same). One gate on
            // the `HStack` covers all three — `.disabled` propagates — while
            // `event-card` above stays live: opening the detail changes nothing
            // on the nest, and greying navigation would strand a viewer on a
            // calendar they can still read.
            .faunaGate("fauna.bridges.put_event_ciphertext")
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .contain)
    }
}
