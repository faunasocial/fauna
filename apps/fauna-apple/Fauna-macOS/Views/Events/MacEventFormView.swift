import SwiftUI
import FaunaKit

struct MacEventFormView: View {
    let vm: EventsVM

    @State private var form = EventComposeForm()

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField(L.events.summary, text: $form.summary)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.eventSummary)
                .automationField(Ids.eventSummary, text: $form.summary)

            // Combined date+time text fields (ui.yaml event-dtstart/event-dtend:
            // a single YYYY-MM-DDTHH:MM text_input on all 7 apps). A typeable
            // field — not a read-only DatePicker — so the in-process driver's
            // clear_and_type drives it, exactly like the iOS EventFormView shell;
            // both apple apps parse/prefill through EventDateInput.
            TextField("\(L.events.start) (YYYY-MM-DDTHH:MM)", text: $form.dtstartText)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.eventDtstart)
                .automationField(Ids.eventDtstart, text: $form.dtstartText)

            TextField("\(L.events.end) (YYYY-MM-DDTHH:MM)", text: $form.dtendText)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.eventDtend)
                .automationField(Ids.eventDtend, text: $form.dtendText)

            TextField(L.events.location, text: $form.location)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.eventFormLocation)
                .automationField(Ids.eventFormLocation, text: $form.location)

            // SwiftUI's TextEditor takes no placeholder, so the description label
            // sits above the box — the same shape the other desktop app uses
            // (linux wraps it in a titled `gtk::Frame(events::DESCRIPTION)`).
            // iOS renders the identical key as a TextField placeholder.
            Text(L.events.description)
                .font(.caption)
                .foregroundStyle(.secondary)

            TextEditor(text: $form.description)
                .frame(height: 60)
                .border(.separator)
                .font(.body)
                .accessibilityIdentifier(Ids.eventFormDescription)
                .automationField(Ids.eventFormDescription, text: $form.description)

            Button(L.events.createEvent) {
                submitCreateEvent()
            }
            .disabled(form.isSubmitDisabled(creatingEvent: vm.creatingEvent))
            .buttonStyle(.borderedProminent)
            .accessibilityIdentifier(Ids.createEvent)
            .automationActivate(Ids.createEvent, isEnabled: { !form.isSubmitDisabled(creatingEvent: vm.creatingEvent) }) {
                submitCreateEvent()
            }
            // The form's one commit; every field above it is the buffer.
            .faunaGate("fauna.bridges.put_event_ciphertext")
        }
        .padding(.vertical, 8)
        .onAppear {
            // Outlook day-cell double-click / empty-slot click → new-event
            // prefilled with that date (events.md § Layout & flow). Seed the
            // YYYY-MM-DDTHH:MM text fields once. A day-cell open already
            // cleared the drafts rail (`EventsVM.beginCompose`), so the
            // resume branch below is mutually exclusive with this one.
            if let prefill = vm.composePrefillDate {
                form.dtstartText = EventDateInput.prefillString(from: prefill)
                form.dtendText = EventDateInput.prefillString(from: prefill.addingTimeInterval(3600))
                vm.composePrefillDate = nil
            } else {
                form.applyResumableDraftIfEmpty(from: vm)
            }
        }
        // A late-arriving launch restore (events.md § Persistence's
        // non-destructive-resume rule: the New Event opener resumes on the
        // OPEN transition, but the restore round-trip can still be in flight
        // when the form is already up — the modal-compose constraint the
        // always-present composers don't have).
        .onChange(of: vm.resumableDraft) { _, _ in form.applyResumableDraftIfEmpty(from: vm) }
        .onChange(of: form.summary) { _, _ in vm.scheduleDraftsSave(from: form) }
        .onChange(of: form.dtstartText) { _, _ in vm.scheduleDraftsSave(from: form) }
        .onChange(of: form.dtendText) { _, _ in vm.scheduleDraftsSave(from: form) }
        .onChange(of: form.description) { _, _ in vm.scheduleDraftsSave(from: form) }
        .onChange(of: form.location) { _, _ in vm.scheduleDraftsSave(from: form) }
    }

    /// The create-event button's action, factored out so the automation sibling
    /// drives the exact same code path the Button does.
    private func submitCreateEvent() {
        let request = form.makeCreateRequest(calendarId: vm.selectedCalendar?.id ?? "")
        Task { await vm.createEvent(request) }
    }
}
