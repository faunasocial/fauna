import SwiftUI
import FaunaKit

struct EventFormView: View {
    let vm: EventsVM

    @Environment(\.dismiss) private var dismiss

    @State private var form = EventComposeForm()

    var body: some View {
        Form {
            Section {
                TextField(L.events.summary, text: $form.summary)
                    .accessibilityIdentifier(Ids.eventSummary)
                    .automationField(Ids.eventSummary, text: $form.summary)
            }

            Section {
                TextField("\(L.events.start) (YYYY-MM-DDTHH:MM)", text: $form.dtstartText)
                    .accessibilityIdentifier(Ids.eventDtstart)
                    .automationField(Ids.eventDtstart, text: $form.dtstartText)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                TextField("\(L.events.end) (YYYY-MM-DDTHH:MM)", text: $form.dtendText)
                    .accessibilityIdentifier(Ids.eventDtend)
                    .automationField(Ids.eventDtend, text: $form.dtendText)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
            }

            Section {
                TextField(L.events.description, text: $form.description, axis: .vertical)
                    .lineLimit(3...6)
                    .accessibilityIdentifier(Ids.eventFormDescription)
                    .automationField(Ids.eventFormDescription, text: $form.description)
                TextField(L.events.location, text: $form.location)
                    .accessibilityIdentifier(Ids.eventFormLocation)
                    .automationField(Ids.eventFormLocation, text: $form.location)
            }

            Section {
                Button(L.events.createEvent) {
                    submitCreateEvent()
                }
                .accessibilityIdentifier(Ids.createEvent)
                .buttonStyle(.borderedProminent)
                .disabled(form.isSubmitDisabled(creatingEvent: vm.creatingEvent))
                .automationActivate(
                    Ids.createEvent,
                    isEnabled: { !form.isSubmitDisabled(creatingEvent: vm.creatingEvent) }
                ) { submitCreateEvent() }
                // The form's one commit; every field above it is the buffer.
                .faunaGate("fauna.bridges.put_event_ciphertext")
                .frame(maxWidth: .infinity)
            }
        }
        .navigationTitle(L.events.newEvent)
        .navigationBarTitleDisplayMode(.inline)
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
        // non-destructive-resume rule): the sheet can already be up when the
        // restore round-trip resolves.
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
        Task {
            await vm.createEvent(request)
            dismiss()
        }
    }
}
