import SwiftUI
import FaunaKit

struct MacEventDetailView: View {
    let vm: EventsVM

    @State private var inviteEmail = ""
    @State private var selectedReminderPreset = ""

    // The reminder picker's preset offsets + labels — the cross-app
    // `select(id, "PT1H")` contract (events.md § Reminders) — are the shared
    // `reminderPresets()` catalog, cached once.
    private static let reminderOptions = reminderPresets()

    var body: some View {
        ScrollView {
            if let event = vm.selectedEvent {
                VStack(alignment: .leading, spacing: 24) {
                    // Header
                    GroupBox {
                        VStack(alignment: .leading, spacing: 8) {
                            automationText(Ids.eventDetailSummary, event.summary)
                                .font(.title2)
                                .fontWeight(.bold)
                            automationText(Ids.eventDetailTime, "\(event.dtstart) – \(event.dtend)")
                                .foregroundStyle(.secondary)
                            if let loc = event.location {
                                Label(loc, systemImage: "mappin")
                                    .foregroundStyle(.secondary)
                                    .accessibilityIdentifier(Ids.eventDetailLocation)
                                    .automationValue(Ids.eventDetailLocation, text: { loc })
                            }
                            if let desc = event.description {
                                automationText(Ids.eventDetailDescription, desc)
                                    .padding(.top, 4)
                            }
                        }
                        .padding(8)
                    }

                    // Attendees
                    GroupBox(L.events.attendeesCount(count: String(vm.selectedEventAttendees.count))) {
                        if vm.selectedEventAttendees.isEmpty {
                            Text(L.events.noAttendees)
                                .foregroundStyle(.secondary)
                                .padding(8)
                        } else {
                            VStack(spacing: 0) {
                                ForEach(Array(vm.selectedEventAttendees.enumerated()), id: \.element.id) { idx, att in
                                    if idx > 0 { Divider() }
                                    AttendeeRow(attendee: att)
                                }
                            }
                            .padding(8)
                            .accessibilityElement(children: .contain)
                            .accessibilityIdentifier(Ids.attendeeList)
                            .automationValue(Ids.attendeeList, text: { "" })
                        }
                    }

                    // Invite + delete are organizer-only affordances.
                    if vm.isOrganizer {
                        // Invite. Cross-nest mailbox-less-Fauna delivery is fully
                        // automatic — resolved from the typed CAL-ADDRESS alone via
                        // anon by_handle discovery (events.md § Scheduling).
                        GroupBox(L.groups.invite) {
                            VStack(spacing: 8) {
                                TextField(L.events.invite.emailPlaceholder, text: $inviteEmail)
                                    .textFieldStyle(.roundedBorder)
                                    .accessibilityIdentifier(Ids.attendeeInviteField)
                                    .automationField(Ids.attendeeInviteField, text: $inviteEmail)
                                Button(L.groups.invite) {
                                    submitInvite()
                                }
                                .disabled(inviteEmail.trimmingCharacters(in: .whitespaces).isEmpty || vm.inviting)
                                .buttonStyle(.borderedProminent)
                                .controlSize(.small)
                                .accessibilityIdentifier(Ids.attendeeInviteButton)
                                .automationActivate(
                                    Ids.attendeeInviteButton,
                                    isEnabled: { !(inviteEmail.trimmingCharacters(in: .whitespaces).isEmpty || vm.inviting) }
                                ) { submitInvite() }
                                // Invite re-PUTs the VEVENT with the new
                                // attendee and dispatches its iMIP REQUEST —
                                // both need the nest. The field beside it is the
                                // buffer and stays live.
                                .faunaGate("fauna.bridges.put_event_ciphertext")
                            }
                            .padding(8)
                        }

                        // Delete
                        GroupBox(L.common.dangerZone) {
                            Button(L.events.deleteEvent, role: .destructive) {
                                Task { await vm.deleteEvent() }
                            }
                            .disabled(vm.deleting)
                            .accessibilityIdentifier(Ids.eventDeleteBtn)
                            .automationActivate(
                                Ids.eventDeleteBtn,
                                isEnabled: { !vm.deleting }
                            ) { Task { await vm.deleteEvent() } }
                            .faunaGate("fauna.bridges.delete_event")
                            .padding(8)
                        }
                    }

                    // RSVP — shown to EVERYONE, the organizer included (decision
                    // 2026-06-29: an organizer may RSVP to their own event, matching
                    // web/linux/windows/android; events.md § Attendee list presentation).
                    GroupBox(L.events.rsvp.title) {
                        VStack(alignment: .leading, spacing: 8) {
                            HStack(spacing: 12) {
                                Button(L.events.rsvp.going) { Task { await vm.rsvp(response: .going) } }
                                    .buttonStyle(.borderedProminent)
                                    .tint(.green)
                                    .accessibilityIdentifier(Ids.eventDetailRsvpGoing)
                                    .automationActivate(Ids.eventDetailRsvpGoing) { Task { await vm.rsvp(response: .going) } }
                                Button(L.events.rsvp.interested) { Task { await vm.rsvp(response: .interested) } }
                                    .buttonStyle(.bordered)
                                    .accessibilityIdentifier(Ids.eventDetailRsvpInterested)
                                    .automationActivate(Ids.eventDetailRsvpInterested) { Task { await vm.rsvp(response: .interested) } }
                                Button(L.common.decline) { Task { await vm.rsvp(response: .declined) } }
                                    .buttonStyle(.bordered)
                                    .tint(.red)
                                    .accessibilityIdentifier(Ids.eventDetailRsvpDecline)
                                    .automationActivate(Ids.eventDetailRsvpDecline) { Task { await vm.rsvp(response: .declined) } }
                            }
                            // One gate on the row covers all three arms — the
                            // shared `EventCardRow` does the same.
                            .faunaGate("fauna.bridges.put_event_ciphertext")
                        }
                        .padding(8)
                    }

                    // Reminder (the shared `event-reminder` component — ui.yaml).
                    // Two states: when a reminder is set, the current-offset label
                    // + a Remove button; when not, a preset Picker + a Set button.
                    GroupBox(L.events.reminder.title) {
                        if let offset = vm.reminderOffset {
                            HStack {
                                automationText(
                                    Ids.eventDetailReminderCurrent,
                                    renderLocalizedText(reminderLabel(offset: offset))
                                )
                                .fontWeight(.semibold)
                                Button(L.events.removeReminder) {
                                    Task { await vm.removeReminder() }
                                }
                                .disabled(vm.reminderLoading)
                                .controlSize(.small)
                                .accessibilityIdentifier(Ids.eventDetailReminderRemove)
                                .automationActivate(
                                    Ids.eventDetailReminderRemove,
                                    isEnabled: { !vm.reminderLoading }
                                ) { Task { await vm.removeReminder() } }
                                .faunaGate("fauna.bridges.put_event_ciphertext")
                            }
                            .padding(8)
                        } else {
                            HStack {
                                Picker("", selection: $selectedReminderPreset) {
                                    Text(L.events.reminder.selectPlaceholder).tag("")
                                    ForEach(Self.reminderOptions, id: \.value) { option in
                                        Text(renderLocalizedText(option.label)).tag(option.value)
                                    }
                                }
                                .frame(maxWidth: 200)
                                .accessibilityIdentifier(Ids.eventDetailReminderSelect)
                                .automationSelect(
                                    Ids.eventDetailReminderSelect,
                                    value: { selectedReminderPreset }
                                ) { selectedReminderPreset = $0 }
                                Button(L.events.reminder.set) {
                                    Task { await vm.setReminder(offset: selectedReminderPreset) }
                                }
                                .disabled(selectedReminderPreset.isEmpty || vm.reminderLoading)
                                .controlSize(.small)
                                .accessibilityIdentifier(Ids.eventDetailReminderSet)
                                .automationActivate(
                                    Ids.eventDetailReminderSet,
                                    isEnabled: { !selectedReminderPreset.isEmpty && !vm.reminderLoading }
                                ) { Task { await vm.setReminder(offset: selectedReminderPreset) } }
                                // The offset `<select>` beside it is a draft
                                // applied on Set, never on selection
                                // (`events.md` § Reminders) — so the select
                                // issues nothing and stays live.
                                .faunaGate("fauna.bridges.put_event_ciphertext")
                            }
                            .padding(8)
                        }
                    }

                    if let error = vm.errorMessage {
                        ErrorBanner(message: error)
                    }
                }
                .padding()
            }
        }
        .pageTitle(L.events.title)
    }

    /// Shared logic — FaunaKit's `submitInvite(vm:inviteEmail:)`.
    private func submitInvite() {
        Task {
            await FaunaKit.submitInvite(vm: vm, inviteEmail: $inviteEmail)
        }
    }
}
