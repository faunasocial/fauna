import SwiftUI
import FaunaKit

struct EventDetailView: View {
    let vm: EventsVM

    @State private var inviteEmail = ""
    @State private var selectedReminderPreset = ""

    // The reminder picker's preset offsets + labels — the cross-app
    // `select(id, "PT1H")` contract (events.md § Reminders, exactly three) —
    // are the shared `reminderPresets()` catalog, cached once.
    private static let reminderOptions = reminderPresets()

    var body: some View {
        Group {
            if let event = vm.selectedEvent {
                // Eager `ScrollView { VStack }`, NOT a lazy `List { Section }`
                // (rule 6 — apple-e2e-automation.md): an iOS `List` pools its rows,
                // so a state-removed element (the `event-detail-reminder-current`
                // label after `removeReminder` sets `reminderOffset = nil`) lingers
                // ON-SCREEN as a pooled zombie and `has_reminder()` keeps reading it
                // visible — the reminder-remove failure. Eager rendering tears the
                // removed subview down at once. Mirrors `CalendarListView` + every
                // `Admin*View`. Cost: rows lose `.insetGrouped` inset styling.
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        // Details
                        VStack(alignment: .leading, spacing: 8) {
                            automationText(Ids.eventDetailSummary, event.summary)
                                .font(.title3)
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
                        .frame(maxWidth: .infinity, alignment: .leading)

                        Divider()

                        // Attendees. `.contain` keeps BOTH the container id AND the
                        // AttendeeRow child ids queryable (a bare container id would
                        // clobber the children — the memory'd rule).
                        VStack(alignment: .leading, spacing: 8) {
                            Text(L.events.attendeesCount(count: String(vm.selectedEventAttendees.count)))
                                .font(.headline)
                            if vm.selectedEventAttendees.isEmpty {
                                Text(L.events.noAttendees)
                                    .foregroundStyle(.secondary)
                            } else {
                                ForEach(vm.selectedEventAttendees) { att in
                                    AttendeeRow(attendee: att)
                                }
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .accessibilityElement(children: .contain)
                        .accessibilityIdentifier(Ids.attendeeList)
                        .automationValue(Ids.attendeeList, text: { "" })

                        // Invite + delete are organizer-only affordances.
                        if vm.isOrganizer {
                            Divider()
                            // Cross-nest mailbox-less-Fauna delivery is fully automatic
                            // — resolved from the typed CAL-ADDRESS alone via anon
                            // by_handle discovery (events.md § Scheduling).
                            VStack(alignment: .leading, spacing: 8) {
                                Text(L.groups.invite)
                                    .font(.headline)
                                TextField(L.events.invite.emailPlaceholder, text: $inviteEmail)
                                    .textFieldStyle(.roundedBorder)
                                    .accessibilityIdentifier(Ids.attendeeInviteField)
                                    .automationField(Ids.attendeeInviteField, text: $inviteEmail)
                                    .textInputAutocapitalization(.never)
                                    .autocorrectionDisabled()
                                Button(L.events.sendInvite) {
                                    submitInvite()
                                }
                                .accessibilityIdentifier(Ids.attendeeInviteButton)
                                .disabled(inviteEmail.trimmingCharacters(in: .whitespaces).isEmpty || vm.inviting)
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
                            .frame(maxWidth: .infinity, alignment: .leading)

                            Divider()
                            VStack(alignment: .leading, spacing: 8) {
                                Button(L.events.deleteEvent, role: .destructive) {
                                    Task { await vm.deleteEvent() }
                                }
                                .accessibilityIdentifier(Ids.eventDeleteBtn)
                                .disabled(vm.deleting)
                                .automationActivate(
                                    Ids.eventDeleteBtn,
                                    isEnabled: { !vm.deleting }
                                ) { Task { await vm.deleteEvent() } }
                                .faunaGate("fauna.bridges.delete_event")
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }

                        Divider()

                        // RSVP — shown to EVERYONE, the organizer included (decision
                        // 2026-06-29: an organizer may RSVP to their own event, matching
                        // web/linux/windows/android; events.md § Attendee list presentation).
                        VStack(alignment: .leading, spacing: 8) {
                            Text(L.events.rsvp.title)
                                .font(.headline)
                            HStack(spacing: 12) {
                                Button(L.events.rsvp.going) { Task { await vm.rsvp(response: .going) } }
                                    .accessibilityIdentifier(Ids.eventDetailRsvpGoing)
                                    .automationActivate(Ids.eventDetailRsvpGoing) { Task { await vm.rsvp(response: .going) } }
                                    .buttonStyle(.borderedProminent)
                                    .tint(.green)
                                    .controlSize(.small)
                                Button(L.events.rsvp.interested) { Task { await vm.rsvp(response: .interested) } }
                                    .accessibilityIdentifier(Ids.eventDetailRsvpInterested)
                                    .automationActivate(Ids.eventDetailRsvpInterested) { Task { await vm.rsvp(response: .interested) } }
                                    .buttonStyle(.bordered)
                                    .controlSize(.small)
                                Button(L.common.decline) { Task { await vm.rsvp(response: .declined) } }
                                    .accessibilityIdentifier(Ids.eventDetailRsvpDecline)
                                    .automationActivate(Ids.eventDetailRsvpDecline) { Task { await vm.rsvp(response: .declined) } }
                                    .buttonStyle(.bordered)
                                    .tint(.red)
                                    .controlSize(.small)
                            }
                            // One gate on the row covers all three arms — the
                            // shared `EventCardRow` does the same.
                            .faunaGate("fauna.bridges.put_event_ciphertext")
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)

                        Divider()

                        // Reminder (the shared `event-reminder` component — ui.yaml).
                        // Two states: when a reminder is set, the current-offset label
                        // + a Remove button; when not, a preset Picker + an explicit Set
                        // button (matching macOS / linux / web — the cross-app contract
                        // is `select(id, "PT1H")` then `click(reminder-set)`, NOT auto-apply).
                        VStack(alignment: .leading, spacing: 8) {
                            Text(L.events.reminder.title)
                                .font(.headline)
                            if let offset = vm.reminderOffset {
                                HStack {
                                    automationText(
                                        Ids.eventDetailReminderCurrent,
                                        renderLocalizedText(reminderLabel(offset: offset))
                                    )
                                    .fontWeight(.semibold)
                                    Spacer()
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
                            } else {
                                Picker(L.events.remindMe, selection: $selectedReminderPreset) {
                                    Text(L.events.reminder.selectPlaceholder).tag("")
                                    ForEach(Self.reminderOptions, id: \.value) { option in
                                        Text(renderLocalizedText(option.label)).tag(option.value)
                                    }
                                }
                                .accessibilityIdentifier(Ids.eventDetailReminderSelect)
                                .automationSelect(
                                    Ids.eventDetailReminderSelect,
                                    value: { selectedReminderPreset }
                                ) { selectedReminderPreset = $0 }
                                Button(L.events.reminder.set) {
                                    Task { await vm.setReminder(offset: selectedReminderPreset) }
                                }
                                .disabled(selectedReminderPreset.isEmpty || vm.reminderLoading)
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
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)

                        if let error = vm.errorMessage {
                            ErrorBanner(message: error)
                        }
                    }
                    .padding()
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            } else if vm.isLoading {
                ProgressView()
            }
        }
        .navigationTitle(L.events.detailTitle)
        .navigationBarTitleDisplayMode(.inline)
    }

    /// Shared logic — FaunaKit's `submitInvite(vm:inviteEmail:)`.
    private func submitInvite() {
        Task {
            await FaunaKit.submitInvite(vm: vm, inviteEmail: $inviteEmail)
        }
    }

}
