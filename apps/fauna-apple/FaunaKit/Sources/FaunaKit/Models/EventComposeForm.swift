import Foundation

/// Shared mutable state for the create-event form, lifted out of the
/// per-platform `EventFormView` (iOS) / `MacEventFormView` (macOS) shells —
/// same lift shape as `FeedCreateForm` (priority #1/#2). Only the SwiftUI
/// body — an iOS modal `Form` that dismisses on submit vs a macOS inline
/// `VStack` that stays open, a legitimate platform-idiom divergence — stays
/// per-platform; both shells hold this as `@State` and bind to it.
public struct EventComposeForm {
    public var summary = ""
    public var dtstartText = ""
    public var dtendText = ""
    public var description = ""
    public var location = ""

    public init() {}

    public var isEmpty: Bool {
        summary.isEmpty && dtstartText.isEmpty && dtendText.isEmpty
            && description.isEmpty && location.isEmpty
    }

    /// Whether the create-event button should stay disabled — a non-empty
    /// summary and both date fields, and not already submitting.
    public func isSubmitDisabled(creatingEvent: Bool) -> Bool {
        summary.trimmingCharacters(in: .whitespaces).isEmpty
            || dtstartText.isEmpty || dtendText.isEmpty
            || creatingEvent
    }

    /// Populate the form from the rail's live draft — only while the form is
    /// still empty (events.md § Persistence: "whether resuming is safe is the
    /// caller's decision — decline when the compose already holds authored
    /// text"). Read, never consumed: the caller keeps holding `resumableDraft`
    /// until a clear (day-cell / successful create).
    public mutating func applyIfEmpty(_ draft: FfiEventDrafts) {
        guard isEmpty else { return }
        summary = draft.summary
        dtstartText = draft.dtstart
        dtendText = draft.dtend
        description = draft.description
        location = draft.location
    }

    public var asDrafts: FfiEventDrafts {
        FfiEventDrafts(summary: summary, dtstart: dtstartText, dtend: dtendText,
                        description: description, location: location)
    }

    public func makeCreateRequest(calendarId: String) -> CreateEventRequest {
        CreateEventRequest(
            calendarId: calendarId,
            summary: summary,
            dtstart: EventDateInput.parse(dtstartText),
            dtend: EventDateInput.parse(dtendText),
            description: description.isEmpty ? nil : description,
            location: location.isEmpty ? nil : location)
    }

    /// Populate the form from `vm`'s live drafts-rail draft, if the form is
    /// still empty (`applyIfEmpty`'s own doc above has the full rationale) —
    /// was a byte-identical per-shell private wrapper on `EventFormView`
    /// (iOS) / `MacEventFormView` (macOS) until this harvest pass found it
    /// .
    @MainActor
    public mutating func applyResumableDraftIfEmpty(from vm: EventsVM) {
        guard let draft = vm.resumableDraft else { return }
        applyIfEmpty(draft)
    }
}
