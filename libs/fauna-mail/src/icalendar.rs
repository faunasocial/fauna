//! iCalendar (RFC 5545) parsing + RRULE expansion for the CalDAV MDA.
//!
//! Two surfaces:
//!
//! - [`parse_icalendar`] turns raw VCALENDAR bytes into a low-level
//!   [`ICalDocument`] tree (preserves component nesting + property parameters
//!   so the MDA can run structural validation on PUT bodies: UID present,
//!   DTSTAMP present, DTSTART present per `caldav-server.md § iCalendar
//!   parsing rules`).
//! - [`expand_recurrence`] expands an RRULE-bearing component against a
//!   half-open `[window_start, window_end)` epoch-seconds window. Built on
//!   the `rrule` crate; used by the MDA's REPORT calendar-query time-range
//!   filter and by every app's events.md UX (per Phase E.1 deviation #3:
//!   `teambition/rrule-go` audit resolved in favor of shared Rust).
//!
//! The high-level `EventFields` / `parse_ical(&str)` surface in
//! `fauna-core::ical` is untouched (deviation #8: fauna-core is WASM-safe
//! and hand-rolls iCalendar to avoid crate deps; this module owns the
//! crate-backed superset that the MDA + future client UX needs).

use icalendar::parser as ical_parser;
use rrule::{RRuleSet, Tz};
use std::str::FromStr;

#[cfg(feature = "uniffi")]
use uniffi;

/// A parsed VCALENDAR document tree. The top-level [`Self::components`]
/// holds VCALENDAR's children (VEVENT / VTODO / VTIMEZONE / etc.);
/// [`Self::properties`] holds VCALENDAR's own properties (`VERSION`,
/// `PRODID`, etc.).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ICalDocument {
    pub components: Vec<ICalComponent>,
    pub properties: Vec<ICalProperty>,
}

/// A single component (VEVENT, VTODO, VTIMEZONE, VALARM, ...).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ICalComponent {
    pub name: String,
    pub properties: Vec<ICalProperty>,
    pub sub_components: Vec<ICalComponent>,
}

/// A single property line (`NAME;PARAM=V:value`).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ICalProperty {
    pub name: String,
    pub value: String,
    pub parameters: Vec<ICalParameter>,
}

/// One parameter on a property (UniFFI-friendly record; `(String, String)`
/// tuples don't roundtrip through UniFFI as cleanly as named records).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ICalParameter {
    pub name: String,
    pub value: String,
}

/// One expanded RRULE occurrence; epoch-seconds boundaries.
///
/// `dtend` is computed from the component's original DTEND/DURATION offset
/// plus the expanded `dtstart`; the component itself is the original
/// (un-rotated) input for callers that need its other properties.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ExpandedOccurrence {
    pub component: ICalComponent,
    pub dtstart: i64,
    pub dtend: i64,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
#[cfg_attr(feature = "uniffi", uniffi(flat_error))]
pub enum ICalError {
    #[error("malformed iCalendar body: {0}")]
    Malformed(String),

    #[error("malformed RRULE: {0}")]
    BadRrule(String),

    #[error("malformed DTSTART or DTEND: {0}")]
    BadDateTime(String),
}

// ---------------------------------------------------------------------------
// parse_icalendar
// ---------------------------------------------------------------------------

/// Parse raw VCALENDAR bytes into a low-level component tree.
///
/// Performs no semantic validation beyond structural correctness — the
/// caller (e.g. the CalDAV PUT handler) enforces RFC 5545 invariants
/// (`UID` / `DTSTAMP` / `DTSTART` present on every VEVENT).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn parse_icalendar(bytes: &[u8]) -> Result<ICalDocument, ICalError> {
    let text =
        std::str::from_utf8(bytes).map_err(|e| ICalError::Malformed(format!("not UTF-8: {e}")))?;

    let unfolded = ical_parser::unfold(text);
    let calendar = ical_parser::read_calendar(&unfolded)
        .map_err(|e| ICalError::Malformed(format!("parse: {e}")))?;

    Ok(ICalDocument {
        components: calendar
            .components
            .into_iter()
            .map(convert_component)
            .collect(),
        properties: calendar
            .properties
            .into_iter()
            .map(convert_property)
            .collect(),
    })
}

fn convert_component(c: ical_parser::Component<'_>) -> ICalComponent {
    ICalComponent {
        name: c.name.to_string(),
        properties: c.properties.into_iter().map(convert_property).collect(),
        sub_components: c.components.into_iter().map(convert_component).collect(),
    }
}

fn convert_property(p: ical_parser::Property<'_>) -> ICalProperty {
    ICalProperty {
        name: p.name.to_string(),
        value: p.val.to_string(),
        parameters: p
            .params
            .into_iter()
            .map(|param| ICalParameter {
                name: param.key.to_string(),
                value: param
                    .val
                    .into_iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// expand_recurrence
// ---------------------------------------------------------------------------

/// Expand a component's RRULE inside `[window_start, window_end)`.
///
/// `window_start` and `window_end` are unix epoch seconds. The caller is
/// responsible for passing a component that has a DTSTART; if the component
/// has no RRULE the function returns the single base occurrence when its
/// DTSTART falls inside the window.
///
/// VTIMEZONE-relative DTSTART is honored when the surrounding
/// [`ICalDocument`]'s VTIMEZONE block is reachable — the caller must
/// flatten any VTIMEZONE-anchored component to its UTC instant before
/// calling, or pass a serialized VCALENDAR string via [`parse_icalendar`] +
/// [`expand_recurrence`] in sequence (the public surface accepts a
/// component, not a full calendar; VTIMEZONE-aware expansion is a follow-up
/// once a real CalDAV MUA needs it. For the v1 MDA the time-range filter
/// runs against MUA-PUT bodies whose DTSTART is normalized to UTC by the
/// MUA — every modern CalDAV client does this, including Apple Calendar,
/// Thunderbird, and Evolution).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn expand_recurrence(
    component: &ICalComponent,
    window_start: i64,
    window_end: i64,
) -> Result<Vec<ExpandedOccurrence>, ICalError> {
    if window_end <= window_start {
        return Ok(Vec::new());
    }

    let dtstart = property_value(component, "DTSTART")
        .ok_or_else(|| ICalError::Malformed("component has no DTSTART".to_string()))?;
    let dtstart_secs = parse_ical_datetime_utc(dtstart)?;

    let dtend_offset_secs = match property_value(component, "DTEND") {
        Some(dtend) => parse_ical_datetime_utc(dtend)? - dtstart_secs,
        None => match property_value(component, "DURATION") {
            Some(d) => parse_iso_duration_secs(d)?,
            None => 0,
        },
    };

    let rrule_str = property_value(component, "RRULE");
    let occurrences: Vec<i64> = match rrule_str {
        None => {
            if dtstart_secs >= window_start && dtstart_secs < window_end {
                vec![dtstart_secs]
            } else {
                Vec::new()
            }
        }
        Some(rrule_body) => {
            let dtstart_chrono = chrono::DateTime::<chrono::Utc>::from_timestamp(dtstart_secs, 0)
                .ok_or_else(|| {
                    ICalError::BadDateTime("DTSTART out of representable range".to_string())
                })?
                .with_timezone(&Tz::UTC);

            let rrule_input = format!(
                "DTSTART:{}\nRRULE:{}",
                dtstart_chrono.format("%Y%m%dT%H%M%SZ"),
                rrule_body,
            );

            let set: RRuleSet = RRuleSet::from_str(&rrule_input)
                .map_err(|e| ICalError::BadRrule(format!("{e}")))?;

            let window_start_chrono =
                chrono::DateTime::<chrono::Utc>::from_timestamp(window_start, 0)
                    .ok_or_else(|| ICalError::BadDateTime("window_start out of range".to_string()))?
                    .with_timezone(&Tz::UTC);
            let window_end_chrono =
                chrono::DateTime::<chrono::Utc>::from_timestamp(window_end - 1, 0)
                    .ok_or_else(|| ICalError::BadDateTime("window_end out of range".to_string()))?
                    .with_timezone(&Tz::UTC);

            let set = set.after(window_start_chrono).before(window_end_chrono);
            set.all(u16::MAX)
                .dates
                .into_iter()
                .map(|d| d.timestamp())
                .filter(|&t| t >= window_start && t < window_end)
                .collect()
        }
    };

    Ok(occurrences
        .into_iter()
        .map(|t| ExpandedOccurrence {
            component: component.clone(),
            dtstart: t,
            dtend: t + dtend_offset_secs,
        })
        .collect())
}

fn property_value<'a>(component: &'a ICalComponent, name: &str) -> Option<&'a str> {
    component
        .properties
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
        .map(|p| p.value.as_str())
}

/// Parse an iCalendar UTC datetime (`YYYYMMDDTHHMMSSZ`) or date (`YYYYMMDD`)
/// to epoch seconds. Floating (no `Z`) datetimes are treated as UTC for v1.
fn parse_ical_datetime_utc(s: &str) -> Result<i64, ICalError> {
    let s = s.trim();

    if s.len() == 8 && s.chars().all(|c| c.is_ascii_digit()) {
        let dt = chrono::NaiveDate::parse_from_str(s, "%Y%m%d")
            .map_err(|e| ICalError::BadDateTime(format!("date: {e}")))?
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| ICalError::BadDateTime("invalid midnight".to_string()))?;
        return Ok(dt.and_utc().timestamp());
    }

    let body = s.strip_suffix('Z').unwrap_or(s);
    let dt = chrono::NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S")
        .map_err(|e| ICalError::BadDateTime(format!("datetime: {e}")))?;
    Ok(dt.and_utc().timestamp())
}

fn parse_iso_duration_secs(s: &str) -> Result<i64, ICalError> {
    let s = s.trim();
    let (sign, rest) = if let Some(r) = s.strip_prefix('-') {
        (-1i64, r)
    } else {
        (1, s.strip_prefix('+').unwrap_or(s))
    };
    let rest = rest
        .strip_prefix('P')
        .ok_or_else(|| ICalError::BadDateTime(format!("not ISO 8601 duration: {s}")))?;

    let (days_part, time_part) = match rest.find('T') {
        Some(idx) => (&rest[..idx], &rest[idx + 1..]),
        None => (rest, ""),
    };

    // Saturating against a crafted DURATION from inbound iMIP mail: a huge
    // segment (e.g. `P9999999999999999999W`) would overflow `n * mult` and the
    // running sum — a debug-build panic / release-build silent wrap on
    // attacker-supplied input. `checked_*` turns it into a graceful BadDateTime.
    let mul_add = |total: i64, n: i64, mult: i64| -> Result<i64, ICalError> {
        n.checked_mul(mult)
            .and_then(|p| total.checked_add(p))
            .ok_or_else(|| ICalError::BadDateTime("duration out of range".to_string()))
    };

    let mut total: i64 = 0;
    let mut cur = String::new();
    for c in days_part.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
            continue;
        }
        let n: i64 = cur
            .parse()
            .map_err(|e| ICalError::BadDateTime(format!("days segment: {e}")))?;
        cur.clear();
        match c {
            'W' => total = mul_add(total, n, 7 * 86400)?,
            'D' => total = mul_add(total, n, 86400)?,
            _ => return Err(ICalError::BadDateTime(format!("unknown unit {c}"))),
        }
    }
    if !cur.is_empty() {
        return Err(ICalError::BadDateTime("trailing days digits".to_string()));
    }

    cur.clear();
    for c in time_part.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
            continue;
        }
        let n: i64 = cur
            .parse()
            .map_err(|e| ICalError::BadDateTime(format!("time segment: {e}")))?;
        cur.clear();
        match c {
            'H' => total = mul_add(total, n, 3600)?,
            'M' => total = mul_add(total, n, 60)?,
            'S' => total = mul_add(total, n, 1)?,
            _ => return Err(ICalError::BadDateTime(format!("unknown unit {c}"))),
        }
    }

    // `total >= 0` (sum of non-negative segments), so `sign * total` cannot
    // overflow (the dangerous `i64::MIN` case is unreachable).
    Ok(sign * total)
}

// ---------------------------------------------------------------------------
// generate_ical — UniFFI writer export for the Go MDA auto-schedule gateway
// ---------------------------------------------------------------------------
//
// The iCalendar *writer* is single-sourced in `fauna_core::ical::generate_ical`
// (WASM-safe, shared by web + native apps — caldav-server.md § iCalendar
// parsing rules). The Go MDA's server-side auto-schedule gateway must reach it
// over UniFFI to build iMIP REQUEST/REPLY/CANCEL bodies (§ Scheduling &
// invitations). We do NOT export `generate_ical` with its native `fauna_core`
// input types directly: the Go mail-bridge FFI build pulls `fauna-core` with
// `default-features = false` (libs/fauna-ffi/Cargo.toml), so fauna_core's
// `uniffi` feature is OFF there and its `EventFields`/`AttendeeInfo` are not
// registered as UniFFI records — `uniffi-bindgen-go` would then emit an
// unresolvable bare `fauna_core` cross-namespace import (the same footgun that
// gated `value_format` off the bridge). So the UniFFI surface mirrors the two
// writer input records in *this* (`fauna_mail`) namespace and converts; the
// serialization logic itself is never duplicated.

/// UniFFI mirror of `fauna_core::ical::EventFields` (the iCalendar writer's
/// event input), defined in the `fauna_mail` namespace so the Go MDA binding
/// resolves. Field-for-field identical; the `From` impl below is an exhaustive
/// destructure→construct, so a new `fauna_core::ical::EventFields` field is a
/// compile error here until it is mirrored.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct WriterEventFields {
    pub summary: String,
    pub dtstart: String,
    pub dtend: String,
    pub duration: String,
    pub location: String,
    pub geo: String,
    pub url: String,
    pub rrule: String,
    pub exdates: String,
    pub categories: String,
    pub status: String,
    pub uid: String,
    pub sequence: u32,
    pub alarm: String,
    pub description: String,
    pub recurrence_id: String,
    pub is_all_day: bool,
}

/// UniFFI mirror of `fauna_core::ical::AttendeeInfo` (writer attendee input).
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct WriterAttendeeInfo {
    pub name: String,
    pub email: String,
    pub partstat: String,
    pub fauna_status: String,
}

impl From<WriterEventFields> for fauna_core::ical::EventFields {
    fn from(w: WriterEventFields) -> Self {
        // Exhaustive destructure + construct: this is the compile-time tripwire
        // that keeps the mirror in lock-step with `fauna_core::ical::EventFields`.
        let WriterEventFields {
            summary,
            dtstart,
            dtend,
            duration,
            location,
            geo,
            url,
            rrule,
            exdates,
            categories,
            status,
            uid,
            sequence,
            alarm,
            description,
            recurrence_id,
            is_all_day,
        } = w;
        fauna_core::ical::EventFields {
            summary,
            dtstart,
            dtend,
            duration,
            location,
            geo,
            url,
            rrule,
            exdates,
            categories,
            status,
            uid,
            sequence,
            alarm,
            description,
            recurrence_id,
            is_all_day,
            // DELIBERATELY not mirrored on `WriterEventFields`: `DTSTAMP` is a
            // write-*timestamp* output, not a caller-supplied writer input. The
            // CalDAV write path (`fauna_client_caldav::seal_and_put_event`)
            // stamps it from the write time, and the MDA's iTIP path supplies it
            // via the separate `generate_itip(.., dtstamp)` parameter — so adding
            // it to this UniFFI mirror would only widen the FFI surface with a
            // field nothing on the Go side sets. Keep the mirror narrower than
            // `EventFields` here on purpose (caldav-server.md § Event resources,
            // GAP 2).
            dtstamp: String::new(),
        }
    }
}

impl From<WriterAttendeeInfo> for fauna_core::ical::AttendeeInfo {
    fn from(w: WriterAttendeeInfo) -> Self {
        let WriterAttendeeInfo {
            name,
            email,
            partstat,
            fauna_status,
        } = w;
        fauna_core::ical::AttendeeInfo {
            name,
            email,
            partstat,
            fauna_status,
        }
    }
}

/// Serialize an event to an RFC 5545 VCALENDAR string — the UniFFI entry point
/// for the Go MDA auto-schedule gateway. Thin wrapper over the single-sourced
/// `fauna_core::ical::generate_ical`; the Go MDA reaches it as
/// `mailfauna.GenerateIcal` (sibling of `mailfauna.ParseICalendar`).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn generate_ical(
    event: WriterEventFields,
    attendees: Vec<WriterAttendeeInfo>,
    organizer_email: String,
) -> String {
    let core_attendees: Vec<fauna_core::ical::AttendeeInfo> =
        attendees.into_iter().map(Into::into).collect();
    fauna_core::ical::generate_ical(&event.into(), &core_attendees, &organizer_email)
}

/// UniFFI mirror of `fauna_core::ical::ITipMethod` (the iTIP scheduling method
/// carried by an iMIP message's `METHOD` property), in the `fauna_mail`
/// namespace so the Go MDA binding resolves (same footgun as the writer
/// records above). The `From` impl is an exhaustive match, so a new
/// `fauna_core::ical::ITipMethod` variant is a compile error here until mirrored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum WriterITipMethod {
    /// Organizer invites attendees or pushes an update to an existing event.
    Request,
    /// Attendee responds with their participation status (`PARTSTAT`).
    Reply,
    /// Organizer cancels the event.
    Cancel,
}

impl From<WriterITipMethod> for fauna_core::ical::ITipMethod {
    fn from(m: WriterITipMethod) -> Self {
        match m {
            WriterITipMethod::Request => fauna_core::ical::ITipMethod::Request,
            WriterITipMethod::Reply => fauna_core::ical::ITipMethod::Reply,
            WriterITipMethod::Cancel => fauna_core::ical::ITipMethod::Cancel,
        }
    }
}

/// Build an iTIP/iMIP scheduling message (`METHOD`-tagged VCALENDAR) — the
/// UniFFI entry point for the Go MDA's server-side auto-schedule gateway
/// (caldav-server.md § Server-side auto-schedule). Thin wrapper over the
/// single-sourced `fauna_core::ical::generate_itip` (the VEVENT writer is never
/// duplicated); the Go MDA reaches it as `mailfauna.GenerateItip`, the sibling
/// of `mailfauna.GenerateIcal`. `dtstamp` is an RFC 3339 timestamp (kept a
/// parameter so the writer stays pure/deterministic — caller supplies the
/// construction time).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn generate_itip(
    method: WriterITipMethod,
    event: WriterEventFields,
    attendees: Vec<WriterAttendeeInfo>,
    organizer_email: String,
    dtstamp: String,
) -> String {
    let core_attendees: Vec<fauna_core::ical::AttendeeInfo> =
        attendees.into_iter().map(Into::into).collect();
    fauna_core::ical::generate_itip(
        method.into(),
        &event.into(),
        &core_attendees,
        &organizer_email,
        &dtstamp,
    )
}

/// An iMIP scheduling message ready for the Go MDA's outbound enqueue: the
/// envelope `from` (→ `enqueue_outbound_mail.original_sender`), `recipients`
/// (→ one queue row each), and the raw RFC 5322 message bytes (→ `raw_message`).
/// UniFFI mirror of `fauna_core::ical::ImipMessage`, in the `fauna_mail`
/// namespace (the cross-namespace `fauna_core` Go-binding footgun — same reason
/// the writer records above live here).
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ImipDispatch {
    pub from: String,
    pub recipients: Vec<String>,
    pub raw_rfc5322: Vec<u8>,
}

/// Build an iMIP scheduling email from a raw iCalendar event body — the UniFFI
/// entry point for the Go MDA's server-side auto-schedule gateway
/// (caldav-server.md § Server-side auto-schedule). Parses the event with the
/// single-sourced `fauna_core::ical` reader, then builds the `METHOD`-tagged
/// iMIP message via `fauna_core::ical::build_event_imip` — the *same* impl the
/// native apps use (priority #2), so a server-fanned invite is byte-identical
/// to a client-fanned one. Returns `None` when the body has no `ORGANIZER` or no
/// email-reachable recipient (the gateway then skips the send). `dtstamp` is an
/// RFC 3339 construction timestamp (kept a parameter so the writer stays pure +
/// deterministic — the Go MDA supplies the PUT time).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn build_event_imip_from_ics(
    method: WriterITipMethod,
    raw_ics: String,
    dtstamp: String,
) -> Option<ImipDispatch> {
    let fields = fauna_core::ical::parse_ical(&raw_ics).ok()?;
    let attendees = fauna_core::ical::parse_ical_attendees(&raw_ics);
    let organizer = fauna_core::ical::parse_ical_organizer(&raw_ics)?;
    let msg = fauna_core::ical::build_event_imip(
        method.into(),
        &fields,
        &attendees,
        &organizer,
        &dtstamp,
    )?;
    Some(ImipDispatch {
        from: msg.from,
        recipients: msg.recipients,
        raw_rfc5322: msg.raw_rfc5322,
    })
}

/// An invitation that arrived by email, ready to be placed on the recipient's
/// calendar: its `UID` (the calendar key, hashed by the placing side exactly as a
/// CalDAV PUT hashes it) and the body the calendar stores for it.
#[cfg(feature = "parser")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundInvite {
    pub uid: String,
    pub ics: String,
}

/// Read an emailed invitation out of a raw RFC 5322 message — the inbound half
/// of caldav-server.md § Server-side auto-schedule ("an external organizer
/// invites a Fauna user … drops it on their calendar"). `Some` only for an iMIP
/// `REQUEST` (RFC 6047) whose event carries a `UID`, a `DTSTART` and an
/// `ORGANIZER`; every other message — ordinary mail, a `REPLY`, a `CANCEL`, a
/// broken calendar part — is `None`, so a delivery path can call this once per
/// message. The stored body is re-rendered through the shared writer
/// (`fauna_core::ical::render_stored_event`, the renderer a Fauna app's own PUT
/// uses) rather than stored as the sender wrote it: a stranger's bytes never
/// reach a calendar collection unparsed, so a malformed invitation cannot break
/// the collection for a calendar app. `timestamp` stamps the stored `DTSTAMP`.
/// Exported for the Go MTA (external senders) and called by the nest (senders on
/// its own domain) — one reading of an invitation for both delivery paths.
#[cfg(feature = "parser")]
#[cfg_attr(feature = "uniffi", uniffi::export)]
#[must_use]
pub fn invite_from_mail(raw_rfc5322: Vec<u8>, timestamp: i64) -> Option<InboundInvite> {
    let part = crate::parser::extract_text_calendar_part(&raw_rfc5322)?;
    if !part.method.as_deref()?.eq_ignore_ascii_case("REQUEST") {
        return None;
    }
    let fields = fauna_core::ical::parse_ical(&part.ics).ok()?;
    if fields.uid.trim().is_empty() || fields.dtstart.trim().is_empty() {
        return None;
    }
    let organizer = fauna_core::ical::parse_ical_organizer(&part.ics)?;
    let attendees = fauna_core::ical::parse_ical_attendees(&part.ics);
    Some(InboundInvite {
        uid: fields.uid.clone(),
        ics: fauna_core::ical::render_stored_event(&fields, &attendees, &organizer, timestamp),
    })
}

/// Build the iMIP `REPLY` a calendar app's attendee owes the organizer after
/// answering an invitation by re-storing the event with their `PARTSTAT` changed
/// — the UniFFI entry point for the Go MDA's "Responding" half of
/// caldav-server.md § Server-side auto-schedule. Thin wrapper over the
/// single-sourced `fauna_core::ical::build_attendee_reply_imip` (priority #2):
/// `None` when nothing is owed (no organizer, the attendee is the organizer, not
/// rostered, no answer, or the answer is unchanged from `prior_ics`).
#[cfg_attr(feature = "uniffi", uniffi::export)]
#[must_use]
pub fn build_attendee_reply_from_ics(
    new_ics: String,
    prior_ics: Option<String>,
    attendee_email: String,
    dtstamp: String,
) -> Option<ImipDispatch> {
    let msg = fauna_core::ical::build_attendee_reply_imip(
        &new_ics,
        prior_ics.as_deref(),
        &attendee_email,
        &dtstamp,
    )?;
    Some(ImipDispatch {
        from: msg.from,
        recipients: msg.recipients,
        raw_rfc5322: msg.raw_rfc5322,
    })
}

/// Extract the `ORGANIZER` CAL-ADDRESS (bare email, `mailto:` stripped
/// case-insensitively) from a raw iCalendar event body, or `None` when it has
/// no `ORGANIZER`. The UniFFI entry point for the Go MDA's auto-schedule
/// gateway (caldav-server.md § Server-side auto-schedule). Thin wrapper over the
/// single-sourced `fauna_core::ical::parse_ical_organizer` (priority #2 — the
/// `mailto:`/CAL-ADDRESS parse is never re-implemented in Go); the MDA reaches
/// it as `mailfauna.ParseIcalOrganizer`, the sibling of `BuildEventImipFromICS`.
/// It is the cheap organizer gate the gateway runs on a PUT *before* the
/// (expensive) read-before-write roster diff: only the event's own organizer
/// fans out, and an event whose new roster is empty (all attendees removed)
/// still needs the organizer known to decide whether a `CANCEL` is owed.
#[cfg_attr(feature = "uniffi", uniffi::export)]
#[must_use]
pub fn parse_ical_organizer_from_ics(raw_ics: String) -> Option<String> {
    fauna_core::ical::parse_ical_organizer(&raw_ics)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_iso_duration_valid_and_overflow_safe() {
        // Well-formed durations parse to the right second count.
        assert_eq!(parse_iso_duration_secs("PT1H").unwrap(), 3600);
        assert_eq!(parse_iso_duration_secs("P1D").unwrap(), 86400);
        assert_eq!(parse_iso_duration_secs("P1W").unwrap(), 7 * 86400);
        assert_eq!(parse_iso_duration_secs("-PT1H30M").unwrap(), -(3600 + 1800));
        assert_eq!(
            parse_iso_duration_secs("P1DT2H3M4S").unwrap(),
            86400 + 7200 + 180 + 4
        );
        // A crafted huge segment from inbound iMIP must error, not panic/wrap
        // (B7 — checked arithmetic on attacker-supplied input).
        assert!(parse_iso_duration_secs("P9999999999999999999W").is_err());
        assert!(parse_iso_duration_secs(&format!("PT{}H", i64::MAX)).is_err());
    }

    #[test]
    fn writer_export_generates_and_round_trips_through_parser() {
        // The UniFFI writer mirror produces a VCALENDAR that parses back through
        // this crate's MDA parser — the cross-surface contract (writer in
        // fauna_core, parser here) at the exact wrapper the Go MDA calls.
        let event = WriterEventFields {
            summary: "Team sync".into(),
            dtstart: "2026-06-10T15:00:00Z".into(),
            dtend: "2026-06-10T16:00:00Z".into(),
            uid: "uid-step2-123".into(),
            ..Default::default()
        };
        let attendees = vec![WriterAttendeeInfo {
            name: "Alice".into(),
            email: "alice@example.com".into(),
            partstat: "ACCEPTED".into(),
            fauna_status: "going".into(),
        }];

        let ics = generate_ical(event, attendees, "organizer@example.com".into());

        assert!(ics.contains("BEGIN:VCALENDAR"), "{ics}");
        assert!(ics.contains("BEGIN:VEVENT"), "{ics}");
        assert!(ics.contains("SUMMARY:Team sync"), "{ics}");
        assert!(ics.contains("UID:uid-step2-123"), "{ics}");
        assert!(ics.contains("ATTENDEE"), "{ics}");

        let doc = parse_icalendar(ics.as_bytes()).expect("writer output re-parses");
        let vevent = doc
            .components
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case("VEVENT"))
            .expect("VEVENT present");
        let uid = vevent
            .properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case("UID"))
            .map(|p| p.value.as_str());
        assert_eq!(uid, Some("uid-step2-123"));
    }

    #[test]
    fn writer_event_fields_mirror_matches_fauna_core_round_trip() {
        // The mirror's `From` conversion is value-preserving end to end: build a
        // fully-populated mirror, convert, regenerate via fauna_core directly,
        // and confirm the bytes match the wrapper's output.
        let mirror = WriterEventFields {
            summary: "All hands".into(),
            dtstart: "2026-07-01T09:00:00Z".into(),
            dtend: "2026-07-01T10:30:00Z".into(),
            location: "HQ".into(),
            description: "Quarterly".into(),
            uid: "uid-mirror".into(),
            sequence: 2,
            ..Default::default()
        };
        let via_wrapper = generate_ical(mirror.clone(), vec![], "o@example.com".into());
        let core: fauna_core::ical::EventFields = mirror.into();
        let via_core = fauna_core::ical::generate_ical(&core, &[], "o@example.com");
        assert_eq!(via_wrapper, via_core);
    }

    #[test]
    fn itip_export_generates_request_and_round_trips_through_parser() {
        // The UniFFI iTIP mirror produces a METHOD-tagged VCALENDAR that parses
        // back through this crate's MDA parser — the cross-surface contract at
        // the exact wrapper the Go MDA auto-schedule gateway will call.
        let event = WriterEventFields {
            summary: "Quarterly review".into(),
            dtstart: "2026-06-12T15:00:00Z".into(),
            dtend: "2026-06-12T16:00:00Z".into(),
            uid: "uid-itip-1".into(),
            sequence: 1,
            ..Default::default()
        };
        let attendees = vec![WriterAttendeeInfo {
            name: "Bob".into(),
            email: "bob@example.com".into(),
            partstat: "NEEDS-ACTION".into(),
            fauna_status: String::new(),
        }];

        let ics = generate_itip(
            WriterITipMethod::Request,
            event,
            attendees,
            "organizer@example.com".into(),
            "2026-06-04T12:00:00Z".into(),
        );

        assert!(ics.contains("METHOD:REQUEST"), "{ics}");
        assert!(ics.contains("BEGIN:VEVENT"), "{ics}");
        assert!(
            ics.contains("DTSTAMP:"),
            "iTIP VEVENT must carry DTSTAMP: {ics}"
        );
        assert!(ics.contains("UID:uid-itip-1"), "{ics}");
        assert!(ics.contains("ATTENDEE"), "{ics}");

        let doc = parse_icalendar(ics.as_bytes()).expect("iTIP output re-parses");
        let vevent = doc
            .components
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case("VEVENT"))
            .expect("VEVENT present");
        let uid = vevent
            .properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case("UID"))
            .map(|p| p.value.as_str());
        assert_eq!(uid, Some("uid-itip-1"));
    }

    #[test]
    fn itip_wrapper_matches_fauna_core_for_every_method() {
        // The wrapper + its enum/record mirrors are value-preserving end to end:
        // for each iTIP method the wrapper output equals the fauna_core writer's
        // output (the serialization is single-sourced, never duplicated).
        let mirror = WriterEventFields {
            summary: "Sprint planning".into(),
            dtstart: "2026-07-02T09:00:00Z".into(),
            dtend: "2026-07-02T10:00:00Z".into(),
            uid: "uid-itip-methods".into(),
            sequence: 3,
            ..Default::default()
        };
        let attendee = WriterAttendeeInfo {
            name: "Carol".into(),
            email: "carol@example.com".into(),
            partstat: "ACCEPTED".into(),
            fauna_status: "going".into(),
        };
        let dtstamp = "2026-06-04T12:34:56Z";
        for m in [
            WriterITipMethod::Request,
            WriterITipMethod::Reply,
            WriterITipMethod::Cancel,
        ] {
            let via_wrapper = generate_itip(
                m,
                mirror.clone(),
                vec![attendee.clone()],
                "o@example.com".into(),
                dtstamp.into(),
            );
            let core: fauna_core::ical::EventFields = mirror.clone().into();
            let core_attendee: fauna_core::ical::AttendeeInfo = attendee.clone().into();
            let via_core = fauna_core::ical::generate_itip(
                m.into(),
                &core,
                std::slice::from_ref(&core_attendee),
                "o@example.com",
                dtstamp,
            );
            assert_eq!(
                via_wrapper, via_core,
                "method {m:?} diverged from fauna_core"
            );
        }
    }

    #[test]
    fn build_event_imip_from_ics_fans_request_to_email_reachable_attendees() {
        // The Go MDA auto-schedule gateway feeds a stored event's raw ICS in; the
        // export parses the organizer + roster (shared fauna_core reader), builds
        // the iMIP REQUEST, and returns the envelope ready for the caller-scoped
        // enqueue_outbound_mail (caldav-server.md § Server-side auto-schedule).
        let raw = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//test//EN\r\n\
BEGIN:VEVENT\r\n\
UID:evt-autoschedule\r\n\
DTSTAMP:20260515T120000Z\r\n\
DTSTART:20260601T100000Z\r\n\
DTEND:20260601T110000Z\r\n\
SUMMARY:Design review\r\n\
ORGANIZER:mailto:organizer@example.com\r\n\
ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:bob@example.com\r\n\
ATTENDEE;PARTSTAT=ACCEPTED:mailto:organizer@example.com\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let msg = build_event_imip_from_ics(
            WriterITipMethod::Request,
            raw.to_string(),
            "2026-06-05T12:00:00Z".into(),
        )
        .expect("organizer + a reachable attendee → Some");

        assert_eq!(msg.from, "organizer@example.com");
        // The organizer is excluded; bob is the only reachable recipient.
        assert_eq!(msg.recipients, vec!["bob@example.com".to_string()]);
        let text = String::from_utf8(msg.raw_rfc5322).unwrap();
        assert!(
            text.contains("Content-Type: text/calendar; charset=UTF-8; method=REQUEST"),
            "{text}"
        );
        assert!(text.contains("METHOD:REQUEST"), "{text}");
        assert!(text.contains("From: organizer@example.com"), "{text}");
        assert!(text.contains("To: bob@example.com"), "{text}");
    }

    /// A raw iMIP email built by the same shared writer an organizer's server
    /// uses — the realistic input `invite_from_mail` reads.
    #[cfg(feature = "parser")]
    fn mailed(method: WriterITipMethod) -> Vec<u8> {
        let raw = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//test//EN\r\n\
BEGIN:VEVENT\r\n\
UID:evt-mailed-invite\r\n\
DTSTAMP:20260515T120000Z\r\n\
DTSTART:20260601T100000Z\r\n\
DTEND:20260601T110000Z\r\n\
SUMMARY:Design review\r\n\
ORGANIZER:mailto:organizer@example.com\r\n\
ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:bob@example.com\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
        build_event_imip_from_ics(method, raw.to_string(), "2026-06-05T12:00:00Z".into())
            .expect("an organizer and an attendee → a message")
            .raw_rfc5322
    }

    #[cfg(feature = "parser")]
    #[test]
    fn invite_from_mail_renders_the_stored_body_of_a_mailed_request() {
        let invite = invite_from_mail(mailed(WriterITipMethod::Request), 1_780_000_000)
            .expect("a mailed REQUEST is an invitation");
        assert_eq!(invite.uid, "evt-mailed-invite");
        // A stored calendar object: no METHOD, the write time as DTSTAMP, and
        // the event, organizer and roster the sender wrote.
        assert!(!invite.ics.contains("METHOD:"), "{}", invite.ics);
        assert!(
            invite.ics.contains("DTSTAMP:20260528T202640Z"),
            "{}",
            invite.ics
        );
        for want in [
            "UID:evt-mailed-invite",
            "SUMMARY:Design review",
            "ORGANIZER:mailto:organizer@example.com",
            "mailto:bob@example.com",
        ] {
            assert!(invite.ics.contains(want), "missing {want}: {}", invite.ics);
        }
    }

    #[cfg(feature = "parser")]
    #[test]
    fn invite_from_mail_ignores_everything_but_a_request() {
        assert!(invite_from_mail(mailed(WriterITipMethod::Cancel), 0).is_none());
        assert!(
            invite_from_mail(
                b"From: a@b\r\nSubject: hi\r\n\r\nplain mail\r\n".to_vec(),
                0
            )
            .is_none()
        );
        // A REQUEST whose event lacks a DTSTART is not placed half-understood.
        let broken = String::from_utf8(mailed(WriterITipMethod::Request))
            .unwrap()
            .replace("DTSTART:20260601T100000Z\r\n", "");
        assert!(invite_from_mail(broken.into_bytes(), 0).is_none());
    }

    #[test]
    fn build_event_imip_from_ics_none_without_reachable_recipient() {
        // No ORGANIZER → None (nothing to fan out from).
        let no_org = "\
BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\n\
BEGIN:VEVENT\r\nUID:e\r\nDTSTAMP:20260515T120000Z\r\nDTSTART:20260601T100000Z\r\n\
ATTENDEE:mailto:bob@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        assert!(
            build_event_imip_from_ics(
                WriterITipMethod::Request,
                no_org.to_string(),
                "2026-06-05T12:00:00Z".into()
            )
            .is_none()
        );

        // Organizer is the only attendee → no reachable recipient → None.
        let only_org = "\
BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\n\
BEGIN:VEVENT\r\nUID:e\r\nDTSTAMP:20260515T120000Z\r\nDTSTART:20260601T100000Z\r\n\
ORGANIZER:mailto:organizer@example.com\r\nATTENDEE:mailto:organizer@example.com\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n";
        assert!(
            build_event_imip_from_ics(
                WriterITipMethod::Request,
                only_org.to_string(),
                "2026-06-05T12:00:00Z".into()
            )
            .is_none()
        );
    }

    #[test]
    fn parse_ical_organizer_from_ics_strips_mailto_and_handles_absence() {
        // The Go MDA gateway uses this as the cheap organizer gate on a PUT —
        // even an event with no email-reachable attendee (build_event_imip_from_ics
        // → None) must still surface its organizer so a CANCEL can be owed.
        let with_org = "\
BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\n\
BEGIN:VEVENT\r\nUID:e\r\nDTSTAMP:20260515T120000Z\r\nDTSTART:20260601T100000Z\r\n\
ORGANIZER;CN=Alice:MAILTO:alice@example.com\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n";
        assert_eq!(
            parse_ical_organizer_from_ics(with_org.to_string()).as_deref(),
            Some("alice@example.com"),
        );

        // No ORGANIZER (a personal event) → None, so the gateway skips fan-out.
        let no_org = "\
BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\n\
BEGIN:VEVENT\r\nUID:e\r\nDTSTAMP:20260515T120000Z\r\nDTSTART:20260601T100000Z\r\n\
SUMMARY:Solo\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        assert!(parse_ical_organizer_from_ics(no_org.to_string()).is_none());
    }

    const ONE_EVENT: &[u8] = b"\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//test//EN\r\n\
BEGIN:VEVENT\r\n\
UID:single-event-uid\r\n\
DTSTAMP:20260515T120000Z\r\n\
DTSTART:20260601T100000Z\r\n\
DTEND:20260601T110000Z\r\n\
SUMMARY:Hello\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    fn epoch(year: i32, month: u32, day: u32, hour: u32) -> i64 {
        chrono::NaiveDate::from_ymd_opt(year, month, day)
            .unwrap()
            .and_hms_opt(hour, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp()
    }

    #[test]
    fn parse_single_event() {
        let doc = parse_icalendar(ONE_EVENT).unwrap();
        assert_eq!(doc.components.len(), 1, "VEVENT");
        let v = &doc.components[0];
        assert_eq!(v.name.to_ascii_uppercase(), "VEVENT");
        assert_eq!(property_value(v, "UID"), Some("single-event-uid"));
        assert_eq!(property_value(v, "DTSTART"), Some("20260601T100000Z"));
        assert_eq!(property_value(v, "SUMMARY"), Some("Hello"));
    }

    #[test]
    fn parse_rejects_non_utf8() {
        let bytes = b"\xff\xfeGARBAGE";
        assert!(matches!(
            parse_icalendar(bytes),
            Err(ICalError::Malformed(_))
        ));
    }

    #[test]
    fn parse_empty_yields_empty_document() {
        // Empty bodies parse to an empty document (no components, no
        // properties); structural validation (`VCALENDAR` wrapper + at
        // least one VEVENT with UID/DTSTAMP/DTSTART) is the CalDAV PUT
        // handler's job per caldav-server.md § iCalendar parsing rules.
        let doc = parse_icalendar(b"").unwrap();
        assert!(doc.components.is_empty());
        assert!(doc.properties.is_empty());
    }

    #[test]
    fn expand_no_rrule_inside_window() {
        let doc = parse_icalendar(ONE_EVENT).unwrap();
        let v = &doc.components[0];
        let occ = expand_recurrence(v, epoch(2026, 6, 1, 0), epoch(2026, 6, 2, 0)).unwrap();
        assert_eq!(occ.len(), 1);
        assert_eq!(occ[0].dtstart, epoch(2026, 6, 1, 10));
        assert_eq!(occ[0].dtend, epoch(2026, 6, 1, 11));
    }

    #[test]
    fn expand_no_rrule_outside_window() {
        let doc = parse_icalendar(ONE_EVENT).unwrap();
        let v = &doc.components[0];
        let occ = expand_recurrence(v, epoch(2027, 1, 1, 0), epoch(2027, 2, 1, 0)).unwrap();
        assert!(occ.is_empty());
    }

    #[test]
    fn expand_daily_rrule_seven_days() {
        let body: &[u8] = b"\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//test//EN\r\n\
BEGIN:VEVENT\r\n\
UID:daily-rrule\r\n\
DTSTAMP:20260515T120000Z\r\n\
DTSTART:20260601T100000Z\r\n\
DTEND:20260601T110000Z\r\n\
RRULE:FREQ=DAILY\r\n\
SUMMARY:Daily\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
        let doc = parse_icalendar(body).unwrap();
        let v = &doc.components[0];
        let occ = expand_recurrence(v, epoch(2026, 6, 1, 0), epoch(2026, 6, 8, 0)).unwrap();
        assert_eq!(occ.len(), 7, "seven daily occurrences in seven-day window");
        assert_eq!(occ[0].dtstart, epoch(2026, 6, 1, 10));
        assert_eq!(occ[6].dtstart, epoch(2026, 6, 7, 10));
        // dtend offset preserved (1 h)
        for o in &occ {
            assert_eq!(o.dtend - o.dtstart, 3600);
        }
    }

    #[test]
    fn expand_rrule_count_honored() {
        let body: &[u8] = b"\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//test//EN\r\n\
BEGIN:VEVENT\r\n\
UID:count-rrule\r\n\
DTSTAMP:20260515T120000Z\r\n\
DTSTART:20260601T100000Z\r\n\
RRULE:FREQ=DAILY;COUNT=3\r\n\
SUMMARY:Count3\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
        let doc = parse_icalendar(body).unwrap();
        let v = &doc.components[0];
        let occ = expand_recurrence(v, epoch(2026, 6, 1, 0), epoch(2026, 6, 30, 0)).unwrap();
        assert_eq!(
            occ.len(),
            3,
            "COUNT=3 caps at three even with a wide window"
        );
    }

    #[test]
    fn expand_rrule_until_honored() {
        let body: &[u8] = b"\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//test//EN\r\n\
BEGIN:VEVENT\r\n\
UID:until-rrule\r\n\
DTSTAMP:20260515T120000Z\r\n\
DTSTART:20260601T100000Z\r\n\
RRULE:FREQ=DAILY;UNTIL=20260605T100000Z\r\n\
SUMMARY:Until5th\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
        let doc = parse_icalendar(body).unwrap();
        let v = &doc.components[0];
        let occ = expand_recurrence(v, epoch(2026, 6, 1, 0), epoch(2026, 6, 30, 0)).unwrap();
        assert_eq!(occ.len(), 5, "Jun 1 through Jun 5 inclusive");
    }

    #[test]
    fn expand_empty_window() {
        let doc = parse_icalendar(ONE_EVENT).unwrap();
        let v = &doc.components[0];
        let t = epoch(2026, 6, 1, 10);
        assert!(expand_recurrence(v, t, t).unwrap().is_empty());
        assert!(expand_recurrence(v, t + 1, t).unwrap().is_empty());
    }

    #[test]
    fn expand_component_without_dtstart_errors() {
        let comp = ICalComponent {
            name: "VEVENT".into(),
            properties: vec![ICalProperty {
                name: "UID".into(),
                value: "no-dtstart".into(),
                parameters: vec![],
            }],
            sub_components: vec![],
        };
        assert!(matches!(
            expand_recurrence(&comp, 0, 1),
            Err(ICalError::Malformed(_))
        ));
    }

    #[test]
    fn property_parameters_preserved() {
        let body: &[u8] = b"\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//test//EN\r\n\
BEGIN:VEVENT\r\n\
UID:params-uid\r\n\
DTSTAMP:20260515T120000Z\r\n\
DTSTART;VALUE=DATE:20260601\r\n\
SUMMARY:All-day\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
        let doc = parse_icalendar(body).unwrap();
        let dtstart = doc.components[0]
            .properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case("DTSTART"))
            .unwrap();
        assert!(
            dtstart
                .parameters
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case("VALUE")
                    && p.value.eq_ignore_ascii_case("DATE")),
            "VALUE=DATE parameter preserved"
        );
    }
}
