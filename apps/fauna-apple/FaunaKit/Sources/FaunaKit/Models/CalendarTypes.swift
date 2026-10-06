import Foundation

// The Events page is a CalDAV calendar surface over the encrypted `bridge_caldav_*`
// store (events.md § State & data shape, Decision B). These per-app UI types
// mirror the `FfiCaldavClient` records (`libs/fauna-ffi/src/caldav_client.rs`); the
// APIClient maps the FFI records onto them. They are no longer decoded from JSON
// (the legacy `/api/{calendars,events}` HTTP twins are gone) — Codable is retained
// only for incidental reuse; construction is via the FFI mapping in APIClient.
public struct FaunaCalendar: Codable, Identifiable, Equatable {
    public let id: String        // hex calendar_id
    public let name: String
    public let color: String?

    public init(id: String, name: String, color: String? = nil) {
        self.id = id
        self.name = name
        self.color = color
    }
}

public struct EventSummary: Codable, Identifiable, Equatable {
    public let id: String        // hex uid_hash (the encrypted-store write key)
    public let uid: String       // plaintext iCalendar UID (inside the sealed body)
    public let summary: String
    public let dtstart: String
    public let dtend: String
    public let location: String?
    public let calendarId: String?

    public init(id: String, uid: String, summary: String, dtstart: String,
                dtend: String, location: String? = nil, calendarId: String? = nil) {
        self.id = id
        self.uid = uid
        self.summary = summary
        self.dtstart = dtstart
        self.dtend = dtend
        self.location = location
        self.calendarId = calendarId
    }
}

public struct EventDetail: Codable, Identifiable, Hashable {
    public let id: String        // hex uid_hash
    public let uid: String
    public let calendarId: String
    public let summary: String
    public let dtstart: String
    public let dtend: String
    public let description: String?
    public let location: String?
    /// VEVENT ORGANIZER CAL-ADDRESS (email); empty when the body carried none.
    public let organizer: String
    /// True iff the calling actor organizes this event — gates the author-only
    /// affordances (delete / invite). Replaces the old `authorId == myActorId`
    /// check (events.md § State & data shape; computed in the seam).
    public let organizedByMe: Bool
    /// VALARM reminder offset (e.g. `-PT15M`), nil when none.
    public let reminder: String?
    /// The event roster, RSVP already projected through the asymmetric sidecar rule.
    public let attendees: [Attendee]

    public init(id: String, uid: String, calendarId: String, summary: String,
                dtstart: String, dtend: String, description: String? = nil,
                location: String? = nil, organizer: String, organizedByMe: Bool,
                reminder: String? = nil, attendees: [Attendee] = []) {
        self.id = id
        self.uid = uid
        self.calendarId = calendarId
        self.summary = summary
        self.dtstart = dtstart
        self.dtend = dtend
        self.description = description
        self.location = location
        self.organizer = organizer
        self.organizedByMe = organizedByMe
        self.reminder = reminder
        self.attendees = attendees
    }
}

public struct Attendee: Codable, Identifiable, Hashable {
    public let email: String     // CAL-ADDRESS (bare email; mailto: stripped)
    public let name: String      // CN; empty when the VEVENT carried none
    public let rsvp: String      // projected: going | interested | tentative | declined | invited

    public var id: String { email }

    public init(email: String, name: String, rsvp: String) {
        self.email = email
        self.name = name
        self.rsvp = rsvp
    }
}

public struct CreateEventRequest: Codable {
    public let calendarId: String
    public let summary: String
    public let dtstart: String
    public let dtend: String
    public let description: String?
    public let location: String?

    public init(calendarId: String, summary: String, dtstart: String,
                dtend: String, description: String? = nil, location: String? = nil) {
        self.calendarId = calendarId
        self.summary = summary
        self.dtstart = dtstart
        self.dtend = dtend
        self.description = description
        self.location = location
    }
}

public struct CalendarImportResult: Codable {
    public let imported: Int
    public let skipped: Int
    public let updated: Int?
    public let total: Int
    public let errors: [String]?
}

/// One exported calendar as a single `.ics` file — the name to save it under and
/// its bytes (`EventsVM.exportCalendarFile`).
public struct CalendarExportFile: Sendable {
    public let fileName: String
    public let data: Data
}
