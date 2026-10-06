// Package-level type aliases + thin wrappers for the shared-Rust
// iCalendar parsing + RRULE expansion surface
// (libs/fauna-mail/src/icalendar.rs).
//
// The bridge's CalDAV MDA arm consumes these to:
//
//   - Validate PUT bodies on the CalDAV write path
//     (caldav-server.md § iCalendar parsing rules: VEVENT must carry
//     UID + DTSTAMP + DTSTART; missing any → 400 with
//     <error><valid-calendar-object-resource/></error>).
//   - Expand recurring events locally for REPORT calendar-query time-range
//     filters, since nest stores opaque sealed bodies and cannot expand
//     RRULEs server-side (caldav-server.md § Read surface, "Time-range
//     filtering is MDA-local").
//
// The shared-Rust path is the priority-#2 default; this file is a
// one-call wrapper, not a re-implementation. The Go-only legacy
// `bins/fauna-bridge-imap/internal/caldav/backend.go` (retired;
// see git history) used `emersion/go-ical` for
// parsing — the new MDA path goes through this shared-Rust surface.

package mailfauna

import (
	faunaMail "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_mail"
)

// ICalDocument mirrors libs/fauna-mail/src/icalendar.rs::ICalDocument —
// a parsed VCALENDAR top-level: `Components` are its children
// (VEVENT / VTODO / VTIMEZONE / ...); `Properties` are VCALENDAR's own
// (VERSION, PRODID, METHOD, ...).
type ICalDocument = faunaMail.ICalDocument

// ICalComponent is one component inside a VCALENDAR (VEVENT, VTODO,
// VTIMEZONE, VALARM, ...). Nested via `SubComponents` (a VEVENT may
// hold one or more VALARMs; VTIMEZONE holds STANDARD / DAYLIGHT).
type ICalComponent = faunaMail.ICalComponent

// ICalProperty is one property line within a component
// (`NAME;PARAM=V:value`). The MDA's UID extraction on PUT walks
// `Properties` for `NAME=="UID"`.
type ICalProperty = faunaMail.ICalProperty

// ICalParameter is one parameter on a property
// (e.g. `DTSTART;VALUE=DATE:20260601`).
type ICalParameter = faunaMail.ICalParameter

// ExpandedOccurrence is one materialized occurrence from a recurring
// event, scoped to a `[windowStart, windowEnd)` epoch-seconds window.
// `Component` is the original (un-rotated) input — callers that need
// other properties (SUMMARY, LOCATION, …) read it directly.
type ExpandedOccurrence = faunaMail.ExpandedOccurrence

// ICalError carries iCalendar parse / RRULE / datetime failures from
// the shared-Rust surface.
type ICalError = faunaMail.ICalError

// ParseICalendar parses raw VCALENDAR bytes into a low-level component
// tree. Performs no semantic validation beyond structural correctness —
// the caller (the CalDAV PUT handler) enforces RFC 5545 invariants
// (UID / DTSTAMP / DTSTART required per caldav-server.md
// § iCalendar parsing rules).
func ParseICalendar(b []byte) (ICalDocument, error) {
	return faunaMail.ParseIcalendar(b)
}

// ExpandRecurrence expands `component`'s RRULE inside
// `[windowStart, windowEnd)`. Epoch-seconds boundaries. If the
// component has no RRULE, returns the single base occurrence when its
// DTSTART falls inside the window.
//
// VTIMEZONE-relative DTSTART expansion: the v1 surface assumes the
// caller flattens DTSTART to UTC before invoking. Every modern CalDAV
// MUA (Apple Calendar, Thunderbird, Evolution) normalizes DTSTART to
// UTC on the wire; a future tightening lifts VTIMEZONE-aware expansion
// once a real MUA breaks this assumption.
func ExpandRecurrence(component ICalComponent, windowStart, windowEnd int64) ([]ExpandedOccurrence, error) {
	return faunaMail.ExpandRecurrence(component, windowStart, windowEnd)
}

// WriterEventFields mirrors libs/fauna-mail/src/icalendar.rs::WriterEventFields
// — the typed event input to the iCalendar *writer*. It is a UniFFI mirror of
// fauna_core::ical::EventFields (kept in the fauna_mail namespace so this Go
// binding resolves; see the Rust-side comment for the cross-namespace footgun).
type WriterEventFields = faunaMail.WriterEventFields

// WriterAttendeeInfo mirrors libs/fauna-mail/src/icalendar.rs::WriterAttendeeInfo
// — one ATTENDEE for GenerateICal.
type WriterAttendeeInfo = faunaMail.WriterAttendeeInfo

// GenerateICal serializes an event to an RFC 5545 VCALENDAR string via the
// single-sourced shared-Rust writer (fauna_core::ical::generate_ical). This is
// the writer half of the iCalendar surface — the MDA's server-side
// auto-schedule gateway builds iMIP REQUEST/REPLY/CANCEL bodies with it
// (caldav-server.md § Scheduling & invitations). Sibling of ParseICalendar; no
// re-implementation (priority #2).
func GenerateICal(event WriterEventFields, attendees []WriterAttendeeInfo, organizerEmail string) string {
	return faunaMail.GenerateIcal(event, attendees, organizerEmail)
}

// ITipMethod is the iTIP scheduling method (RFC 5546) carried by an iMIP
// message's METHOD property. Mirror of fauna_core::ical::ITipMethod via the
// generated faunaMail enum.
type ITipMethod = faunaMail.WriterITipMethod

const (
	// ITipRequest — organizer invites attendees or pushes an update.
	ITipRequest = faunaMail.WriterITipMethodRequest
	// ITipReply — attendee responds with their PARTSTAT.
	ITipReply = faunaMail.WriterITipMethodReply
	// ITipCancel — organizer cancels the event.
	ITipCancel = faunaMail.WriterITipMethodCancel
)

// GenerateItip builds an iTIP/iMIP scheduling message (a METHOD-tagged
// VCALENDAR wrapping the VEVENT) via the single-sourced shared-Rust writer
// (fauna_core::ical::generate_itip). This is what the MDA's server-side
// auto-schedule gateway calls to fan a REQUEST/CANCEL out to attendees (or a
// REPLY back to an organizer) without a Fauna app (caldav-server.md
// § Server-side auto-schedule). Sibling of GenerateICal; no re-implementation
// (priority #2). dtstamp is an RFC 3339 timestamp (the message construction
// time) — the writer stays pure, so the caller supplies it.
func GenerateItip(method ITipMethod, event WriterEventFields, attendees []WriterAttendeeInfo, organizerEmail, dtstamp string) string {
	return faunaMail.GenerateItip(method, event, attendees, organizerEmail, dtstamp)
}

// ImipDispatch mirrors libs/fauna-mail/src/icalendar.rs::ImipDispatch — an iMIP
// scheduling message ready for the MDA's outbound enqueue: From (the envelope
// sender → enqueue_outbound_mail.original_sender), Recipients (one queue row
// each), RawRfc5322 (the RFC 5322 message bytes → raw_message).
type ImipDispatch = faunaMail.ImipDispatch

// BuildEventImipFromICS builds an iMIP scheduling message from a raw iCalendar
// event body via the single-sourced shared-Rust impl
// (fauna_core::ical::build_event_imip) — the SAME impl a Fauna app uses, so a
// server-fanned invite is byte-identical to a client-fanned one (priority #2).
// The MDA's server-side auto-schedule gateway calls it to fan a REQUEST/CANCEL
// out to a stored event's email-reachable attendees without a Fauna app
// (caldav-server.md § Server-side auto-schedule). Returns nil when the body has
// no ORGANIZER or no email-reachable recipient (the gateway then skips the
// send). dtstamp is an RFC 3339 construction timestamp.
func BuildEventImipFromICS(method ITipMethod, rawICS, dtstamp string) *ImipDispatch {
	return faunaMail.BuildEventImipFromIcs(method, rawICS, dtstamp)
}

// InboundInvite mirrors libs/fauna-mail/src/icalendar.rs::InboundInvite — an
// emailed invitation's UID and the body the calendar stores for it.
type InboundInvite = faunaMail.InboundInvite

// InviteFromMail reads an emailed invitation (an iMIP REQUEST) out of a raw RFC
// 5322 message and renders the body the recipient's calendar stores for it —
// single-sourced shared Rust (fauna_mail::icalendar::invite_from_mail), the same
// reading the nest applies to invitations from senders on its own domain. nil
// for any message that is not a well-formed invitation. `timestamp` stamps the
// stored DTSTAMP.
func InviteFromMail(raw []byte, timestamp int64) *InboundInvite {
	return faunaMail.InviteFromMail(raw, timestamp)
}

// BuildAttendeeReplyFromICS builds the iMIP REPLY an attendee's calendar app owes
// the organizer after answering an invitation by re-storing the event with a
// changed PARTSTAT — the "Responding" half of caldav-server.md § Server-side
// auto-schedule. Single-sourced shared Rust
// (fauna_core::ical::build_attendee_reply_imip). priorICS is the stored body the
// PUT replaced (nil on a create or an unreadable prior). Returns nil when
// nothing is owed: no ORGANIZER, the attendee is the organizer, is not
// rostered, has not answered, or re-stored an unchanged answer.
func BuildAttendeeReplyFromICS(newICS string, priorICS []byte, attendeeEmail, dtstamp string) *ImipDispatch {
	var prior *string
	if len(priorICS) > 0 {
		p := string(priorICS)
		prior = &p
	}
	return faunaMail.BuildAttendeeReplyFromIcs(newICS, prior, attendeeEmail, dtstamp)
}

// ParseIcalOrganizer returns the bare email of a raw iCalendar event body's
// ORGANIZER (`mailto:` stripped case-insensitively), or nil when the body has
// no ORGANIZER. Single-sourced from the shared-Rust
// fauna_core::ical::parse_ical_organizer (priority #2 — the CAL-ADDRESS parse
// is never re-implemented in Go). The MDA's auto-schedule gateway uses it as
// the cheap organizer gate on a PUT, *before* the read-before-write roster
// diff: only the event's organizer fans out, and an event whose new roster is
// empty (all attendees removed) still needs the organizer known so a CANCEL
// can be owed (caldav-server.md § Server-side auto-schedule).
func ParseIcalOrganizer(rawICS string) *string {
	return faunaMail.ParseIcalOrganizerFromIcs(rawICS)
}
