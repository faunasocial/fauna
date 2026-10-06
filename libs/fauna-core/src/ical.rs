//! Minimal iCalendar (RFC 5545) parser and generator.
//!
//! Converts between iCalendar text and structured [`EventFields`].
//! No external iCalendar crate dependencies — the format is line-based
//! key:value pairs with a small set of known properties.

use crate::content_line::unfold_lines;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// Parsed event fields from iCalendar.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventFields {
    pub summary: String,
    pub dtstart: String,  // RFC 3339
    pub dtend: String,    // RFC 3339
    pub duration: String, // ISO 8601 duration
    pub location: String,
    pub geo: String, // "lat,lon"
    pub url: String,
    pub rrule: String,
    pub exdates: String,    // comma-separated RFC 3339
    pub categories: String, // comma-separated
    pub status: String,     // confirmed/tentative/cancelled
    pub uid: String,
    pub sequence: u32,
    pub alarm: String,         // e.g. "-PT15M"
    pub description: String,   // DESCRIPTION property
    pub recurrence_id: String, // RFC 3339 datetime identifying which recurring instance is overridden
    pub is_all_day: bool,      // true when DTSTART has VALUE=DATE (date-only, no time)
    /// iCalendar `DTSTAMP` (RFC 5545 §3.8.7.2): the UTC date-time the event was
    /// last written, as a compact `YYYYMMDDTHHMMSSZ` string. RFC 5545 makes
    /// **exactly one** DTSTAMP MANDATORY on every VEVENT, and go-ical's
    /// *encoder* — which the mail-bridge MDA runs to serialize an event into a
    /// CalDAV REPORT/GET response — rejects a VEVENT that lacks it. A client
    /// write that omitted DTSTAMP therefore decoded fine but broke the MUA's
    /// REPORT mid-stream, leaving the event invisible to every CalDAV MUA (GAP 2;
    /// caldav-server.md § Event resources). Populated by [`parse_ical`] on read
    /// and set from the write timestamp by the shared writer's callers
    /// (`fauna_client_caldav::CalDavClient::seal_and_put_event`). Empty only for
    /// in-memory fixtures that never reach the wire.
    pub dtstamp: String,
}

/// Attendee info for iCalendar ATTENDEE properties.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AttendeeInfo {
    pub name: String,
    pub email: String,
    pub partstat: String,     // ACCEPTED, TENTATIVE, DECLINED, NEEDS-ACTION
    pub fauna_status: String, // Original fauna status: going, interested, declined, invited, waitlisted
}

/// The text projection for one event-attendee row — the shared derivation behind
/// the 6-client `AttendeeRow` (`docs/goal/ui/events.md` § Attendee list
/// presentation). A CalDAV `ATTENDEE` is just `CN` + email + `PARTSTAT`, so this
/// derives the three display strings every app renders — the CN→email
/// fallback, the generated monogram initial, and the email-beneath visibility —
/// in one place instead of each app hand-rolling them (they had drifted: some
/// rendered the email twice when the CN *was* the email, one showed `?` for an
/// email-only attendee, and whitespace handling varied). The RSVP status/color is
/// deliberately *not* here — it stays a per-app idiomatic map, like
/// [`crate::source_glyph`].
///
/// Crosses the FFI/wasm boundary as-is (UniFFI `attendeeDisplay`, wasm
/// `attendeeDisplay`), mirroring [`crate::format::RelativeTimeDisplay`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AttendeeDisplay {
    /// Primary line: the `CN` when it is a real, distinct name, else the bare email.
    pub display_name: String,
    /// Single uppercased initial of the display name for the generated monogram
    /// avatar; `?` when there is no displayable character.
    pub monogram: String,
    /// The email shown beneath the name — `Some` only when the name is a real `CN`
    /// distinct from the email (otherwise the email would appear twice), else `None`.
    pub secondary_email: Option<String>,
}

/// Derive the [`AttendeeDisplay`] for an attendee from its raw `CN` (`name`, empty
/// when the VEVENT carried none) and bare `email`. See [`AttendeeDisplay`] and
/// `events.md` § Attendee list presentation.
pub fn attendee_display(name: &str, email: &str) -> AttendeeDisplay {
    // A real CN is non-blank and distinct from the address; otherwise the display
    // name (and monogram) fall back to the bare email and nothing shows beneath.
    let has_cn = !name.trim().is_empty() && name.trim() != email.trim();
    let display_name = if has_cn { name } else { email };
    let monogram = display_name
        .trim()
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_else(|| "?".to_string());
    AttendeeDisplay {
        display_name: display_name.to_string(),
        monogram,
        secondary_email: if has_cn {
            Some(email.to_string())
        } else {
            None
        },
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse iCalendar text into [`EventFields`].
///
/// Handles line unfolding (RFC 5545 §3.1) and extracts the first VEVENT block.
pub fn parse_ical(ical_text: &str) -> Result<EventFields> {
    let unfolded = unfold_lines(ical_text);
    let lines: Vec<&str> = unfolded.lines().collect();

    // Find VEVENT block
    let start = lines
        .iter()
        .position(|l| l.trim().eq_ignore_ascii_case("BEGIN:VEVENT"));
    let end = lines
        .iter()
        .position(|l| l.trim().eq_ignore_ascii_case("END:VEVENT"));

    let (start, end) = match (start, end) {
        (Some(s), Some(e)) if s < e => (s, e),
        _ => bail!("No valid VEVENT block found"),
    };

    let vevent_lines = &lines[start + 1..end];

    let mut fields = EventFields::default();
    let mut exdates: Vec<String> = Vec::new();
    let mut in_valarm = false;

    for &line in vevent_lines {
        let trimmed = line.trim();
        if trimmed.eq_ignore_ascii_case("BEGIN:VALARM") {
            in_valarm = true;
            continue;
        }
        if trimmed.eq_ignore_ascii_case("END:VALARM") {
            in_valarm = false;
            continue;
        }

        if in_valarm {
            if let Some((name, _params, value)) = parse_property_line(trimmed)
                && name.eq_ignore_ascii_case("TRIGGER")
            {
                fields.alarm = value.to_string();
            }
            continue;
        }

        if let Some((name, _params, value)) = parse_property_line(trimmed) {
            let name_upper = name.to_ascii_uppercase();
            match name_upper.as_str() {
                "SUMMARY" => fields.summary = unescape_ical(value),
                "DTSTART" => {
                    fields.dtstart = normalize_datetime(value, _params);
                    // Detect all-day events: VALUE=DATE param with 8-digit date
                    if has_value_date_param(_params)
                        && value.trim().len() == 8
                        && value.trim().chars().all(|c| c.is_ascii_digit())
                    {
                        fields.is_all_day = true;
                    }
                }
                "DTEND" => fields.dtend = normalize_datetime(value, _params),
                "DURATION" => fields.duration = value.to_string(),
                "LOCATION" => fields.location = unescape_ical(value),
                "GEO" => {
                    // iCalendar GEO is "lat;lon" — normalize to "lat,lon"
                    fields.geo = value.replace(';', ",");
                }
                "URL" => fields.url = value.to_string(),
                "RRULE" => fields.rrule = value.to_string(),
                "EXDATE" => {
                    // May appear multiple times
                    for d in value.split(',') {
                        let d = d.trim();
                        if !d.is_empty() {
                            exdates.push(normalize_datetime(d, _params));
                        }
                    }
                }
                "CATEGORIES" => fields.categories = value.to_string(),
                "STATUS" => fields.status = value.to_ascii_lowercase(),
                "UID" => fields.uid = value.to_string(),
                "DTSTAMP" => fields.dtstamp = normalize_datetime(value, _params),
                "SEQUENCE" => fields.sequence = value.parse().unwrap_or(0),
                "DESCRIPTION" => fields.description = unescape_ical(value),
                "RECURRENCE-ID" => {
                    fields.recurrence_id = normalize_datetime(value, _params);
                }
                _ => {}
            }
        }
    }

    if !exdates.is_empty() {
        fields.exdates = exdates.join(",");
    }

    Ok(fields)
}

/// Parse all VEVENTs from an iCalendar file. Returns one Result<EventFields> per VEVENT.
pub fn parse_ical_multi(ical_text: &str) -> Vec<anyhow::Result<EventFields>> {
    let unfolded = unfold_lines(ical_text);
    let mut results = Vec::new();
    let lines: Vec<&str> = unfolded.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim().eq_ignore_ascii_case("BEGIN:VEVENT") {
            // Find END:VEVENT
            let start = i;
            while i < lines.len() && !lines[i].trim().eq_ignore_ascii_case("END:VEVENT") {
                i += 1;
            }
            if i < lines.len() {
                // Build a minimal VCALENDAR wrapper for parse_ical
                let block: String = std::iter::once("BEGIN:VCALENDAR")
                    .chain(std::iter::once("VERSION:2.0"))
                    .chain(lines[start..=i].iter().copied())
                    .chain(std::iter::once("END:VCALENDAR"))
                    .collect::<Vec<_>>()
                    .join("\n");
                results.push(parse_ical(&block));
            }
        }
        i += 1;
    }
    results
}

/// Parse a single iCalendar property line into (name, params, value).
///
/// Format: `NAME;PARAM=V;PARAM=V:value`
/// The first colon that is not inside a parameter separates name+params from value.
fn parse_property_line(line: &str) -> Option<(&str, &str, &str)> {
    // The boundary between name+params and value is the first colon outside a
    // DQUOTE-quoted param value — the vCard side needs the identical rule, so
    // it lives in `content_line` rather than here (RFC 5545 § 3.1 / RFC 6350
    // § 3.3 define the same content line).
    let colon_pos = crate::content_line::find_value_colon(line)?;
    let name_params = &line[..colon_pos];
    let value = &line[colon_pos + 1..];

    if let Some(semi_pos) = name_params.find(';') {
        let name = &name_params[..semi_pos];
        let params = &name_params[semi_pos + 1..];
        Some((name, params, value))
    } else {
        Some((name_params, "", value))
    }
}

/// Check whether the parameter string contains `VALUE=DATE` (case-insensitive).
fn has_value_date_param(params: &str) -> bool {
    params
        .split(';')
        .any(|p| p.trim().eq_ignore_ascii_case("VALUE=DATE"))
}

/// Map a Windows timezone name (as used by Outlook / Exchange) to an IANA
/// timezone identifier.  Returns `None` for unrecognised names.
///
/// Covers the ~40 most common Windows timezone names across US, Europe,
/// Asia-Pacific, Americas, and Africa.
fn windows_tz_to_iana(name: &str) -> Option<&'static str> {
    // Match case-insensitively by comparing against lowercase keys.
    let lower = name.to_ascii_lowercase();
    let iana = match lower.as_str() {
        // --- United States ---
        "eastern standard time" => "America/New_York",
        "pacific standard time" => "America/Los_Angeles",
        "central standard time" => "America/Chicago",
        "mountain standard time" => "America/Denver",
        "hawaii-aleutian standard time" | "hawaiian standard time" => "Pacific/Honolulu",
        "alaskan standard time" => "America/Anchorage",
        "atlantic standard time" => "America/Halifax",
        "us eastern standard time" => "America/Indianapolis",
        "us mountain standard time" => "America/Phoenix",

        // --- Europe ---
        "gmt standard time" => "Europe/London",
        "w. europe standard time" => "Europe/Berlin",
        "romance standard time" => "Europe/Paris",
        "central europe standard time" => "Europe/Budapest",
        "central european standard time" => "Europe/Warsaw",
        "e. europe standard time" => "Europe/Chisinau",
        "fle standard time" => "Europe/Kiev",
        "gtb standard time" => "Europe/Bucharest",
        "russian standard time" => "Europe/Moscow",
        "turkey standard time" => "Europe/Istanbul",
        "greece standard time" | "gre standard time" => "Europe/Athens",

        // --- Asia ---
        "tokyo standard time" => "Asia/Tokyo",
        "china standard time" => "Asia/Shanghai",
        "india standard time" => "Asia/Kolkata",
        "singapore standard time" => "Asia/Singapore",
        "korea standard time" => "Asia/Seoul",
        "arab standard time" => "Asia/Riyadh",
        "arabian standard time" => "Asia/Dubai",
        "se asia standard time" => "Asia/Bangkok",
        "west asia standard time" => "Asia/Karachi",
        "iran standard time" => "Asia/Tehran",
        "israel standard time" => "Asia/Jerusalem",
        "taipei standard time" => "Asia/Taipei",

        // --- Pacific / Oceania ---
        "aus eastern standard time" => "Australia/Sydney",
        "new zealand standard time" => "Pacific/Auckland",
        "fiji standard time" => "Pacific/Fiji",
        "samoa standard time" => "Pacific/Apia",

        // --- Americas (non-US) ---
        "sa eastern standard time" => "America/Sao_Paulo",
        "sa pacific standard time" => "America/Bogota",
        "central america standard time" => "America/Guatemala",
        "e. south america standard time" => "America/Sao_Paulo",
        "venezuela standard time" => "America/Caracas",
        "canada central standard time" => "America/Regina",
        "newfoundland standard time" => "America/St_Johns",
        "mexico standard time" => "America/Mexico_City",

        // --- Africa ---
        "south africa standard time" => "Africa/Johannesburg",
        "egypt standard time" => "Africa/Cairo",
        "w. central africa standard time" => "Africa/Lagos",

        _ => return None,
    };
    Some(iana)
}

/// Return a fixed UTC offset string for a well-known IANA timezone.
///
/// This is a **V1 approximation** — it uses standard-time offsets and does
/// not account for daylight saving transitions.  A future version can use a
/// proper tz database.  Returns `None` for unrecognised zones.
fn iana_tz_to_offset(iana: &str) -> Option<&'static str> {
    let off = match iana {
        // Americas
        "America/New_York" | "America/Indianapolis" => "-05:00",
        "America/Chicago" | "America/Mexico_City" => "-06:00",
        "America/Denver" => "-07:00",
        "America/Los_Angeles" => "-08:00",
        "America/Anchorage" => "-09:00",
        "Pacific/Honolulu" => "-10:00",
        "America/Halifax" => "-04:00",
        "America/St_Johns" => "-03:30",
        "America/Sao_Paulo" => "-03:00",
        "America/Bogota" | "America/Guatemala" => "-05:00",
        "America/Caracas" => "-04:00",
        "America/Phoenix" | "America/Regina" => "-07:00",

        // Europe
        "Europe/London" => "+00:00",
        "Europe/Berlin" | "Europe/Paris" | "Europe/Budapest" | "Europe/Warsaw" => "+01:00",
        "Europe/Chisinau" | "Europe/Kiev" | "Europe/Bucharest" | "Europe/Athens"
        | "Europe/Istanbul" => "+02:00",
        "Europe/Moscow" => "+03:00",

        // Asia
        "Asia/Dubai" => "+04:00",
        "Asia/Karachi" => "+05:00",
        "Asia/Kolkata" => "+05:30",
        "Asia/Tehran" => "+03:30",
        "Asia/Bangkok" => "+07:00",
        "Asia/Shanghai" | "Asia/Singapore" | "Asia/Taipei" => "+08:00",
        "Asia/Tokyo" | "Asia/Seoul" => "+09:00",
        "Asia/Jerusalem" => "+02:00",
        "Asia/Riyadh" => "+03:00",

        // Pacific / Oceania
        "Australia/Sydney" => "+10:00",
        "Pacific/Auckland" => "+12:00",
        "Pacific/Fiji" => "+12:00",
        "Pacific/Apia" => "+13:00",

        // Africa
        "Africa/Johannesburg" => "+02:00",
        "Africa/Cairo" => "+02:00",
        "Africa/Lagos" => "+01:00",

        _ => return None,
    };
    Some(off)
}

/// Extract the TZID value from an iCalendar parameter string.
///
/// e.g. `"TZID=Eastern Standard Time"` → `Some("Eastern Standard Time")`
fn extract_tzid(params: &str) -> Option<&str> {
    for param in params.split(';') {
        let param = param.trim();
        if let Some(val) = param.strip_prefix("TZID=") {
            // Strip optional surrounding quotes
            let val = val.trim_matches('"');
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// Normalize an iCalendar datetime value to RFC 3339.
///
/// Handles:
/// - `20260401T100000Z` → `2026-04-01T10:00:00Z`
/// - `20260401T100000` (floating) → `2026-04-01T10:00:00`
/// - `20260401` (date-only) → `2026-04-01`
/// - Already formatted strings pass through.
///
/// When a `TZID` parameter is present the function resolves it (Windows
/// names are mapped via [`windows_tz_to_iana`]) and appends the
/// corresponding UTC offset so that the resulting string is a valid
/// RFC 3339 timestamp.
fn normalize_datetime(value: &str, params: &str) -> String {
    let value = value.trim();

    // If it already looks like RFC 3339, pass through
    if value.contains('-') && value.len() >= 10 {
        return value.to_string();
    }

    // Date-only: 8 digits
    if value.len() == 8 && value.chars().all(|c| c.is_ascii_digit()) {
        return format!("{}-{}-{}", &value[0..4], &value[4..6], &value[6..8]);
    }

    // DateTime: 15 or 16 chars (with optional Z)
    let is_utc = value.ends_with('Z');
    let base = value.trim_end_matches('Z');

    // The compact branches byte-slice `base`, so a multi-byte char would split
    // and panic — a non-ASCII value is unrecognized, passed through.
    if !base.is_ascii() {
        return value.to_string();
    }

    if base.len() == 15 && base.as_bytes().get(8) == Some(&b'T') {
        let formatted = format!(
            "{}-{}-{}T{}:{}:{}",
            &base[0..4],
            &base[4..6],
            &base[6..8],
            &base[9..11],
            &base[11..13],
            &base[13..15],
        );
        if is_utc {
            return format!("{formatted}Z");
        }

        // If a TZID parameter is present, try to resolve an offset.
        if let Some(tzid) = extract_tzid(params) {
            // The TZID might already be an IANA name, or a Windows name.
            let iana = windows_tz_to_iana(tzid).unwrap_or(tzid);
            if let Some(offset) = iana_tz_to_offset(iana) {
                return format!("{formatted}{offset}");
            }
        }

        // Floating (no timezone info) — return without offset.
        formatted
    } else if base.len() == 13 && base.as_bytes().get(8) == Some(&b'T') {
        // Read-tolerance for a seconds-less compact value (`YYYYMMDDTHHMM`) that an
        // earlier to_ical_datetime emitted before the seconds-padding fix — recover
        // it as a parseable RFC 3339 datetime (`HH:MM:00`) rather than returning
        // the unparseable compact string (no data loss for alpha events already
        // written; the forward path now always pads seconds at serialization).
        let formatted = format!(
            "{}-{}-{}T{}:{}:00",
            &base[0..4],
            &base[4..6],
            &base[6..8],
            &base[9..11],
            &base[11..13],
        );
        if is_utc {
            format!("{formatted}Z")
        } else {
            formatted
        }
    } else {
        // Unrecognized — return as-is
        value.to_string()
    }
}

/// Unescape iCalendar text values (RFC 5545 §3.3.11).
fn unescape_ical(value: &str) -> String {
    value
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

/// Escape a text value for iCalendar.
fn escape_ical(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

/// Build the `BEGIN:VEVENT … END:VEVENT` block lines for a single event.
/// Shared by [`generate_ical`] (the METHOD-less stored body) and
/// [`generate_itip`] (a METHOD-tagged scheduling message) so the two paths
/// serialize a VEVENT through one writer.
fn vevent_lines(
    event: &EventFields,
    attendees: &[AttendeeInfo],
    organizer_email: &str,
) -> Vec<String> {
    let mut lines: Vec<String> = vec!["BEGIN:VEVENT".into()];

    // DTSTAMP is RFC-5545-mandatory (exactly one per VEVENT) — go-ical's encoder
    // on the MDA's CalDAV serve path rejects a VEVENT without it, which silently
    // broke the REPORT/GET response for every client-written event (GAP 2). Emit
    // it first so it rides directly after BEGIN:VEVENT. The write path
    // (`seal_and_put_event`) sets `dtstamp` from the write timestamp; a read →
    // re-write round-trip preserves whatever `parse_ical` recovered.
    if !event.dtstamp.is_empty() {
        lines.push(format!("DTSTAMP:{}", to_ical_datetime(&event.dtstamp)));
    }

    if !event.uid.is_empty() {
        lines.push(format!("UID:{}", event.uid));
    }

    if !event.recurrence_id.is_empty() {
        lines.push(format!(
            "RECURRENCE-ID:{}",
            to_ical_datetime(&event.recurrence_id)
        ));
    }

    if !event.summary.is_empty() {
        lines.push(format!("SUMMARY:{}", escape_ical(&event.summary)));
    }

    if !event.dtstart.is_empty() {
        if event.is_all_day {
            lines.push(format!(
                "DTSTART;VALUE=DATE:{}",
                to_ical_date_only(&event.dtstart)
            ));
        } else {
            lines.push(format!("DTSTART:{}", to_ical_datetime(&event.dtstart)));
        }
    }

    if !event.dtend.is_empty() {
        if event.is_all_day {
            lines.push(format!(
                "DTEND;VALUE=DATE:{}",
                to_ical_date_only(&event.dtend)
            ));
        } else {
            lines.push(format!("DTEND:{}", to_ical_datetime(&event.dtend)));
        }
    }

    if !event.duration.is_empty() {
        lines.push(format!("DURATION:{}", event.duration));
    }

    if !event.location.is_empty() {
        lines.push(format!("LOCATION:{}", escape_ical(&event.location)));
    }

    if !event.geo.is_empty() {
        // iCalendar GEO uses semicolon separator
        lines.push(format!("GEO:{}", event.geo.replace(',', ";")));
    }

    if !event.url.is_empty() {
        lines.push(format!("URL:{}", event.url));
    }

    if !event.rrule.is_empty() {
        lines.push(format!("RRULE:{}", event.rrule));
    }

    if !event.exdates.is_empty() {
        for d in event.exdates.split(',') {
            let d = d.trim();
            if !d.is_empty() {
                lines.push(format!("EXDATE:{}", to_ical_datetime(d)));
            }
        }
    }

    if !event.categories.is_empty() {
        lines.push(format!("CATEGORIES:{}", event.categories));
    }

    if !event.status.is_empty() {
        lines.push(format!("STATUS:{}", event.status.to_ascii_uppercase()));
    }

    lines.push(format!("SEQUENCE:{}", event.sequence));

    if !event.description.is_empty() {
        lines.push(format!("DESCRIPTION:{}", escape_ical(&event.description)));
    }

    if !organizer_email.is_empty() {
        lines.push(format!("ORGANIZER:mailto:{organizer_email}"));
    }

    for att in attendees {
        let mut parts = Vec::new();
        if !att.name.is_empty() {
            parts.push(format!("CN={}", att.name));
        }
        parts.push(format!("PARTSTAT={}", att.partstat));
        // `X-FAUNA-STATUS` carries the one state `PARTSTAT` cannot: Waitlisted
        // and Invited both project to `NEEDS-ACTION`, so the param is what
        // tells them apart on the way back in.
        if RsvpState::from_fauna_str(&att.fauna_status) == Some(RsvpState::Waitlisted) {
            parts.push(format!("X-FAUNA-STATUS={}", RsvpState::Waitlisted));
        }
        let params = parts.join(";");
        lines.push(format!(
            "ATTENDEE;{params}:mailto:{email}",
            email = att.email
        ));
    }

    if !event.alarm.is_empty() {
        lines.push("BEGIN:VALARM".into());
        lines.push("ACTION:DISPLAY".into());
        lines.push("DESCRIPTION:Reminder".into());
        lines.push(format!("TRIGGER:{}", event.alarm));
        lines.push("END:VALARM".into());
    }

    lines.push("END:VEVENT".into());
    lines
}

/// Generate a complete iCalendar VCALENDAR string from [`EventFields`].
pub fn generate_ical(
    event: &EventFields,
    attendees: &[AttendeeInfo],
    organizer_email: &str,
) -> String {
    let mut lines: Vec<String> = vec![
        "BEGIN:VCALENDAR".into(),
        "VERSION:2.0".into(),
        "PRODID:-//Fauna//CalDAV//EN".into(),
    ];
    lines.extend(vevent_lines(event, attendees, organizer_email));
    lines.push("END:VCALENDAR".into());

    // Fold lines and join with CRLF (RFC 5545 §3.1)
    let folded: Vec<String> = lines.into_iter().map(|l| fold_line(&l)).collect();
    folded.join("\r\n") + "\r\n"
}

/// iTIP scheduling method (RFC 5546) carried by an iMIP message's `METHOD`
/// property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ITipMethod {
    /// Organizer invites attendees or pushes an update to an existing event.
    Request,
    /// Attendee responds with their participation status (`PARTSTAT`).
    Reply,
    /// Organizer cancels the event.
    Cancel,
}

impl ITipMethod {
    /// The `METHOD` property value (`REQUEST` / `REPLY` / `CANCEL`).
    pub fn as_str(self) -> &'static str {
        match self {
            ITipMethod::Request => "REQUEST",
            ITipMethod::Reply => "REPLY",
            ITipMethod::Cancel => "CANCEL",
        }
    }
}

/// Build an iTIP/iMIP scheduling message (RFC 5546 / RFC 6047): a VCALENDAR
/// carrying a `METHOD` property that wraps the VEVENT. Reuses the same VEVENT
/// serialization as [`generate_ical`] (one writer), adding the iTIP-required
/// `DTSTAMP` (RFC 5545 mandates it on every iTIP VEVENT) and, for a `REPLY`,
/// a `REQUEST-STATUS:2.0;Success` line (RFC 5546 §3.2.3).
///
/// Caller responsibilities per RFC 5546:
/// - `Request`: pass the full attendee roster + organizer; bump `SEQUENCE` on
///   a material reschedule.
/// - `Reply`: pass a single-element `attendees` slice — the responding
///   attendee with their chosen `PARTSTAT`.
/// - `Cancel`: set `event.status = "cancelled"` and bump `SEQUENCE` before
///   calling.
///
/// `dtstamp` is an RFC 3339 timestamp (the message construction time), kept a
/// parameter rather than read from the clock so the writer stays pure,
/// deterministic, and WASM-safe (web shares it; the Go MDA reaches it via the
/// `fauna_mail` UniFFI mirror).
pub fn generate_itip(
    method: ITipMethod,
    event: &EventFields,
    attendees: &[AttendeeInfo],
    organizer_email: &str,
    dtstamp: &str,
) -> String {
    let mut lines: Vec<String> = vec![
        "BEGIN:VCALENDAR".into(),
        "VERSION:2.0".into(),
        "PRODID:-//Fauna//CalDAV//EN".into(),
        format!("METHOD:{}", method.as_str()),
    ];

    // Ensure exactly one DTSTAMP (RFC 5546 mandates it on every iTIP VEVENT):
    // stamp the cloned event from the supplied construction time so vevent_lines
    // emits it once, directly after BEGIN:VEVENT. Setting it on the event
    // (rather than inserting into `body`) keeps a single DTSTAMP even when the
    // input event already carried one from a prior parse — two would fail
    // go-ical's "exactly one DTSTAMP" encoder check.
    let mut event = event.clone();
    if !dtstamp.is_empty() {
        event.dtstamp = to_ical_datetime(dtstamp);
    }
    let mut body = vevent_lines(&event, attendees, organizer_email);
    // A REPLY reports delivery success just before END:VEVENT.
    if method == ITipMethod::Reply {
        let end_idx = body.len() - 1;
        body.insert(end_idx, "REQUEST-STATUS:2.0;Success".into());
    }
    lines.extend(body);
    lines.push("END:VCALENDAR".into());

    let folded: Vec<String> = lines.into_iter().map(|l| fold_line(&l)).collect();
    folded.join("\r\n") + "\r\n"
}

// ---------------------------------------------------------------------------
// iMIP dispatch (caldav-server.md § Scheduling & invitations)
// ---------------------------------------------------------------------------
//
// Building a scheduling email (the iTIP body wrapped as RFC 5322) lives here,
// next to the `generate_itip` writer, so BOTH call sites share one impl
// (priority #2): the client (`fauna-client-caldav` re-exports `build_event_imip`
// for the linux/native Events shells) AND the Go MDA's server-side
// auto-schedule gateway (which reaches it via a thin `fauna-mail` UniFFI export,
// `build_event_imip_from_ics`). All of it is pure + WASM-safe (string assembly
// only), the same constraint the writer carries.

/// An iMIP scheduling email ready for the outbound mail path: the envelope
/// `from` + `recipients` + the raw RFC 5322 message bytes (a `text/calendar`
/// part carrying the iTIP body). A client hands `recipients` + `raw_rfc5322`
/// straight to `EmailClient::send`; the Go MDA gateway hands `from` →
/// `original_sender`, `recipients`, `raw_rfc5322` → `enqueue_outbound_mail`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImipMessage {
    /// The envelope sender (the `From:` header value): the organizer for a
    /// `Request`/`Cancel`, the responding attendee for a `Reply`.
    pub from: String,
    /// Envelope recipients (full `local@domain` mailboxes).
    pub recipients: Vec<String>,
    /// The RFC 5322 message: headers + a `text/calendar; method=…` body.
    pub raw_rfc5322: Vec<u8>,
}

/// Email-reachable envelope recipients for an organizer fan-out: every attendee
/// with a non-empty `email`, excluding the organizer's own address
/// (case-insensitive), deduped while preserving order. This is the full
/// email-shaped roster; in production a Fauna handle domain == the mail domain, so
/// `mailto:<handle>@<domain>` is deliverable over the MTA. A **mailbox-less** Fauna
/// user — CalDAV on, email off — is split back off this list onto the WS-RPC
/// sealed-delivery rail by the **organizer dispatch fork**
/// (`fauna_client_caldav::dispatch_imip_request`, caldav-server.md § Server-side
/// auto-schedule); this function does not know each recipient's transport (it has
/// no nest-discovery), so it returns them all and the fork re-routes.
fn email_reachable_recipients(attendees: &[AttendeeInfo], organizer_email: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for att in attendees {
        let email = att.email.trim();
        if email.is_empty() || email.eq_ignore_ascii_case(organizer_email) {
            continue;
        }
        if !out.iter().any(|e| e.eq_ignore_ascii_case(email)) {
            out.push(email.to_string());
        }
    }
    out
}

/// The `Subject:` line for an iMIP message of the given method.
fn imip_subject(method: ITipMethod, summary: &str) -> String {
    let summary = if summary.trim().is_empty() {
        "(no subject)"
    } else {
        summary.trim()
    };
    match method {
        ITipMethod::Request => format!("Invitation: {summary}"),
        ITipMethod::Reply => format!("Re: {summary}"),
        ITipMethod::Cancel => format!("Cancelled: {summary}"),
    }
}

/// Assemble an RFC 5322 iMIP message: standard headers + a single
/// `text/calendar; method=…` body carrying `itip_body` (RFC 6047).
fn assemble_rfc5322(
    method: ITipMethod,
    from: &str,
    recipients: &[String],
    summary: &str,
    itip_body: &str,
) -> Vec<u8> {
    let subject = imip_subject(method, summary);
    let to = recipients.join(", ");
    format!(
        "From: {from}\r\n\
         To: {to}\r\n\
         Subject: {subject}\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: text/calendar; charset=UTF-8; method={method}\r\n\
         Content-Transfer-Encoding: 8bit\r\n\
         \r\n\
         {itip_body}",
        method = method.as_str(),
    )
    .into_bytes()
}

/// Build the iMIP email for an organizer fan-out (`Request`/`Cancel`) or an
/// attendee response (`Reply`): construct the iTIP `METHOD` message via
/// [`generate_itip`], then wrap it as RFC 5322 ready for the outbound mail path.
/// The iTIP From/To direction is method-defined (caldav-server.md § Server-side
/// auto-schedule):
/// - `Request`/`Cancel`: organizer → the email-reachable attendees;
/// - `Reply`: the responding attendee (`attendees[0]`) → the organizer.
///
/// Returns `None` when there is no reachable recipient (an empty roster, or the
/// organizer is the only attendee), so the caller simply skips the send.
/// `dtstamp` is an RFC 3339 construction timestamp (the caller supplies the
/// clock so this stays pure + WASM-safe).
#[must_use]
pub fn build_event_imip(
    method: ITipMethod,
    event: &EventFields,
    attendees: &[AttendeeInfo],
    organizer_email: &str,
    dtstamp: &str,
) -> Option<ImipMessage> {
    let body = generate_itip(method, event, attendees, organizer_email, dtstamp);
    let (from, recipients) = match method {
        ITipMethod::Request | ITipMethod::Cancel => (
            organizer_email.to_string(),
            email_reachable_recipients(attendees, organizer_email),
        ),
        ITipMethod::Reply => {
            let from = attendees
                .first()
                .map(|a| a.email.trim().to_string())
                .unwrap_or_default();
            let to = organizer_email.trim().to_string();
            let recipients = if to.is_empty() { Vec::new() } else { vec![to] };
            (from, recipients)
        }
    };
    if from.trim().is_empty() || recipients.is_empty() {
        return None;
    }
    let raw_rfc5322 = assemble_rfc5322(method, &from, &recipients, &event.summary, &body);
    Some(ImipMessage {
        from,
        recipients,
        raw_rfc5322,
    })
}

/// Render the body a calendar stores for an event: the METHOD-less VCALENDAR the
/// writer emits, with `DTSTAMP` stamped from the write time. A stored calendar
/// object's DTSTAMP is its last-modified time (RFC 5545 §3.8.7.2), so any stamp
/// the event arrived with — an iTIP message's own construction time — is
/// replaced; and a VEVENT without one would decode but fail go-ical's encoder on
/// the MDA serve path, leaving the event invisible to every calendar app
/// (caldav-server.md § Event resources). The one renderer behind a Fauna app's
/// sealed PUT and the nest's placement of an emailed invitation, so both store
/// byte-identical bodies for the same event.
#[must_use]
pub fn render_stored_event(
    event: &EventFields,
    attendees: &[AttendeeInfo],
    organizer_email: &str,
    timestamp: i64,
) -> String {
    let mut event = event.clone();
    event.dtstamp = epoch_secs_to_ical_utc(timestamp);
    generate_ical(&event, attendees, organizer_email)
}

/// The `PARTSTAT` values that are an attendee's *answer* — the ones RFC 5546
/// §3.2.3 carries in a `REPLY`. `NEEDS-ACTION` is the absence of an answer, and
/// `DELEGATED`/`COMPLETED`/`IN-PROCESS` belong to flows the gateway does not run.
fn is_reply_partstat(partstat: &str) -> bool {
    ["ACCEPTED", "DECLINED", "TENTATIVE"]
        .iter()
        .any(|p| partstat.trim().eq_ignore_ascii_case(p))
}

/// Build the iMIP `REPLY` an attendee's own calendar app owes the organizer after
/// the attendee answers an invitation — the "Responding" half of
/// caldav-server.md § Server-side auto-schedule, for a client that answers by
/// re-PUTting the event with its own `PARTSTAT` changed (every stock CalDAV app
/// does exactly that) rather than through a Fauna app's RSVP.
///
/// `new_ics` is the body the attendee just stored; `prior_ics` the body it
/// replaced (`None` on a create, or when the prior body could not be read).
/// `attendee_email` is the signed-in user. Returns `None` in every case RFC 5546
/// sends nothing: no `ORGANIZER`, the attendee *is* the organizer (the organizer
/// fan-out owns that PUT), the attendee is not on the roster, their `PARTSTAT` is
/// not an answer, or the answer did not change since `prior_ics` — so a calendar
/// app that re-stores an unchanged event on every sync never re-sends a reply.
/// The reply carries exactly the responding attendee, with the address spelled as
/// the roster spells it. The same pure, WASM-safe [`build_event_imip`] builds the
/// message, so a server-sent reply is byte-identical to an app-sent one.
#[must_use]
pub fn build_attendee_reply_imip(
    new_ics: &str,
    prior_ics: Option<&str>,
    attendee_email: &str,
    dtstamp: &str,
) -> Option<ImipMessage> {
    let me = attendee_email.trim();
    if me.is_empty() {
        return None;
    }
    let organizer = parse_ical_organizer(new_ics)?;
    if organizer.trim().eq_ignore_ascii_case(me) {
        return None;
    }
    let responder = parse_ical_attendees(new_ics)
        .into_iter()
        .find(|a| a.email.trim().eq_ignore_ascii_case(me))?;
    if !is_reply_partstat(&responder.partstat) {
        return None;
    }
    if let Some(prior) = prior_ics {
        let unchanged = parse_ical_attendees(prior).iter().any(|a| {
            a.email.trim().eq_ignore_ascii_case(me)
                && a.partstat
                    .trim()
                    .eq_ignore_ascii_case(responder.partstat.trim())
        });
        if unchanged {
            return None;
        }
    }
    let fields = parse_ical(new_ics).ok()?;
    build_event_imip(
        ITipMethod::Reply,
        &fields,
        std::slice::from_ref(&responder),
        &organizer,
        dtstamp,
    )
}

/// Generate a single .ics file containing multiple VEVENTs.
pub fn generate_ical_multi(events: &[(EventFields, Vec<AttendeeInfo>, String)]) -> String {
    let mut out = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Fauna//CalDAV//EN\r\n");
    for (fields, attendees, organizer) in events {
        // Generate VEVENT block (reuse generate_ical internals but only the VEVENT part)
        let full = generate_ical(fields, attendees, organizer);
        // Extract just the VEVENT block
        if let Some(start) = full.find("BEGIN:VEVENT")
            && let Some(end) = full.find("END:VEVENT")
        {
            out.push_str(&full[start..end + "END:VEVENT\r\n".len()]);
        }
    }
    out.push_str("END:VCALENDAR\r\n");
    out
}

/// Format Unix epoch seconds as a compact iCalendar UTC `DTSTAMP`/`DATE-TIME`
/// (`YYYYMMDDTHHMMSSZ`). Pure + WASM-safe (no clock, no date crate): the
/// caller supplies the instant, this only does the civil-date arithmetic
/// (Howard Hinnant's `civil_from_days`), so the writer stays deterministic.
/// Used by the CalDAV write path to stamp the RFC-5545-mandatory `DTSTAMP`
/// from the event's last-write timestamp.
#[must_use]
pub fn epoch_secs_to_ical_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hh, mm, ss) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    let (y, m, d) = crate::caltime::civil_from_days(days);
    format!("{y:04}{m:02}{d:02}T{hh:02}{mm:02}{ss:02}Z")
}

/// Convert an RFC 3339 datetime back to iCalendar format.
///
/// `2026-04-01T10:00:00Z` → `20260401T100000Z`
/// `2026-04-01T10:00:00` → `20260401T100000`
/// `2026-04-01` → `20260401`
fn to_ical_datetime(rfc: &str) -> String {
    let rfc = rfc.trim();

    // Date-only: 2026-04-01
    if rfc.len() == 10 && rfc.chars().nth(4) == Some('-') && !rfc.contains('T') {
        return rfc.replace('-', "");
    }

    // Full datetime
    if let Some(t_pos) = rfc.find('T') {
        let date_part = &rfc[..t_pos];
        let time_and_rest = &rfc[t_pos + 1..];

        let is_utc = time_and_rest.ends_with('Z');

        // If there's a timezone offset like -04:00, strip it for now (V1)
        // Keep Z but strip offsets
        let clean_time = time_and_rest.trim_end_matches('Z');
        // Check for offset like +05:00 or -04:00
        // `get` rather than a byte slice: a multi-byte char straddling the cut
        // (`€0000`) is no offset, never a panic.
        let offset_at = clean_time.len().saturating_sub(6);
        let time_no_offset = match clean_time.get(offset_at..) {
            Some(tail)
                if tail.len() == 6
                    && (tail.starts_with('+') || tail.starts_with('-'))
                    && tail.chars().nth(3) == Some(':') =>
            {
                &clean_time[..offset_at]
            }
            _ => clean_time,
        };

        // Strip fractional seconds (e.g. "00:00:00.0000000" → "00:00:00") so the
        // compact form is exactly 15 chars and normalize_datetime can re-parse it.
        let time_only = if let Some(dot_pos) = time_no_offset.find('.') {
            &time_no_offset[..dot_pos]
        } else {
            time_no_offset
        };

        let date_compact = date_part.replace('-', "");
        // Pad a seconds-less (`HHMM`) or hour-only (`HH`) time to full `HHMMSS` so
        // the emitted iCalendar is well-formed: a bare `T1400` is malformed RFC
        // 5545 that normalize_datetime can't re-parse (it expects 15 chars) and
        // that every app's date parser + external CalDAV clients reject. A
        // client sending datetime-local `YYYY-MM-DDTHH:MM` is the common source.
        let time_compact = match time_only.replace(':', "").as_str() {
            t if t.len() == 4 => format!("{t}00"),
            t if t.len() == 2 => format!("{t}0000"),
            t => t.to_string(),
        };

        let suffix = if is_utc { "Z" } else { "" };
        return format!("{date_compact}T{time_compact}{suffix}");
    }

    // Fallback — return as-is
    rfc.to_string()
}

/// Convert an RFC 3339 date (possibly with time) to a compact date-only
/// string for VALUE=DATE output: `2026-04-01` → `20260401`.
/// If the input contains a `T`, only the date portion is used.
fn to_ical_date_only(rfc: &str) -> String {
    let rfc = rfc.trim();
    let date_part = if let Some(t_pos) = rfc.find('T') {
        &rfc[..t_pos]
    } else {
        rfc
    };
    date_part.replace('-', "")
}

/// Fold a content line so no line exceeds 75 octets (RFC 5545 §3.1).
/// Continuation lines start with a single space.
fn fold_line(line: &str) -> String {
    let bytes = line.as_bytes();
    if bytes.len() <= 75 {
        return line.to_string();
    }

    let mut result = String::with_capacity(bytes.len() + bytes.len() / 75 * 3);

    // First line: up to 75 octets
    let first_end = find_utf8_safe_split(bytes, 75);
    result.push_str(&line[..first_end]);
    let mut pos = first_end;

    // Continuation lines: space + up to 74 octets of content
    while pos < bytes.len() {
        result.push_str("\r\n ");
        let chunk_end = find_utf8_safe_split(bytes, pos + 74).min(bytes.len());
        result.push_str(&line[pos..chunk_end]);
        pos = chunk_end;
    }

    result
}

/// Find a safe byte position to split UTF-8 text at or before `max_pos`.
/// Never splits in the middle of a multi-byte character.
fn find_utf8_safe_split(bytes: &[u8], max_pos: usize) -> usize {
    let max_pos = max_pos.min(bytes.len());
    if max_pos == bytes.len() {
        return max_pos;
    }
    // Walk backwards to find a char boundary
    let mut pos = max_pos;
    while pos > 0 && (bytes[pos] & 0xC0) == 0x80 {
        pos -= 1;
    }
    pos
}

// ---------------------------------------------------------------------------
// Attendee parsing
// ---------------------------------------------------------------------------

/// Parse ATTENDEE properties from iCalendar text and return [`AttendeeInfo`] entries.
///
/// Extracts CN, PARTSTAT, X-FAUNA-STATUS, and the mailto: email from each
/// ATTENDEE line within the first VEVENT block.
pub fn parse_ical_attendees(ical_text: &str) -> Vec<AttendeeInfo> {
    let unfolded = unfold_lines(ical_text);
    let lines: Vec<&str> = unfolded.lines().collect();

    let start = lines
        .iter()
        .position(|l| l.trim().eq_ignore_ascii_case("BEGIN:VEVENT"));
    let end = lines
        .iter()
        .position(|l| l.trim().eq_ignore_ascii_case("END:VEVENT"));

    let (start, end) = match (start, end) {
        (Some(s), Some(e)) if s < e => (s, e),
        _ => return Vec::new(),
    };

    let vevent_lines = &lines[start + 1..end];
    let mut attendees = Vec::new();

    for &line in vevent_lines {
        let trimmed = line.trim();
        if let Some((name, params, value)) = parse_property_line(trimmed) {
            if !name.eq_ignore_ascii_case("ATTENDEE") {
                continue;
            }

            // Extract email from mailto: value
            let email = if let Some(addr) = value.strip_prefix("mailto:") {
                addr.to_string()
            } else {
                value.to_string()
            };

            // Parse parameters
            let mut cn = String::new();
            let mut partstat = "NEEDS-ACTION".to_string();
            let mut x_fauna_status = String::new();

            for param in params.split(';') {
                let param = param.trim();
                if let Some(val) = param.strip_prefix("CN=") {
                    cn = val.to_string();
                } else if let Some(val) = param.strip_prefix("PARTSTAT=") {
                    partstat = val.to_string();
                } else if let Some(val) = param.strip_prefix("X-FAUNA-STATUS=") {
                    x_fauna_status = val.to_string();
                }
            }

            // Determine fauna status: X-FAUNA-STATUS takes priority over PARTSTAT mapping
            let fauna_status =
                if RsvpState::from_fauna_str(&x_fauna_status) == Some(RsvpState::Waitlisted) {
                    RsvpState::Waitlisted.to_string()
                } else {
                    fauna_status_from_partstat(&partstat).to_string()
                };

            attendees.push(AttendeeInfo {
                name: cn,
                email,
                partstat,
                fauna_status,
            });
        }
    }

    attendees
}

/// Extract the `ORGANIZER` CAL-ADDRESS (bare email, `mailto:` stripped
/// case-insensitively) from a VEVENT body. Returns `None` when the body carries
/// no `ORGANIZER` line. The companion to [`parse_ical_attendees`] — a
/// read-mutate-rewrite (RSVP / reminder / inbound-reply merge) re-serializes the
/// event and must preserve its original organizer; the inbound-`REPLY` merge also
/// needs it to route the merged re-PUT. WASM-safe (a plain line scan).
pub fn parse_ical_organizer(ical_text: &str) -> Option<String> {
    let unfolded = unfold_lines(ical_text);
    for line in unfolded.lines() {
        let trimmed = line.trim();
        let Some((name, _params, value)) = parse_property_line(trimmed) else {
            continue;
        };
        if !name.eq_ignore_ascii_case("ORGANIZER") {
            continue;
        }
        // The value is `mailto:<addr>` (case-insensitive scheme) or a bare addr.
        let value = value.trim();
        let addr = value
            .strip_prefix("mailto:")
            .or_else(|| value.strip_prefix("MAILTO:"))
            .unwrap_or(value)
            .trim();
        if !addr.is_empty() {
            return Some(addr.to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// PARTSTAT / Fauna status mapping
// ---------------------------------------------------------------------------

use crate::rsvp::RsvpState;

/// Convert a Fauna RSVP status to an iCalendar PARTSTAT.
///
/// The `&str` spelling of [`RsvpState::partstat`], kept for the call sites that
/// still hold a status string. An unrecognized status answers `NEEDS-ACTION`,
/// which is the safe roster reading — but note that a *submitted* value should
/// never reach here untyped: [`crate::rsvp::RsvpResponse`] is the gate that rejects a stray
/// literal instead of folding it (see that module's docs).
pub fn partstat_from_fauna(status: &str) -> &str {
    match RsvpState::from_fauna_str(status) {
        Some(state) => state.partstat(),
        None => "NEEDS-ACTION",
    }
}

/// Convert an iCalendar PARTSTAT to a Fauna RSVP status — the **lossy write
/// round-trip** inverse, where `TENTATIVE` folds back to `interested`.
///
/// The `&str` spelling of [`RsvpState::from_partstat_lossy`]. For anything
/// arriving from a foreign client, the render rule is
/// `fauna_client_caldav::rsvp_status_verbatim`
/// ([`RsvpState::from_partstat_verbatim`]), which keeps a bare `TENTATIVE`
/// tentative — the asymmetry is `caldav-server.md` § RSVP semantics.
pub fn fauna_status_from_partstat(partstat: &str) -> &str {
    RsvpState::from_partstat_lossy(partstat).as_str()
}

/// Map an event-reminder ISO-8601 duration offset to its human label, as a
/// [`LocalizedText`] each app resolves through its own i18n pipeline. The
/// three canonical presets (`PT15M` / `PT1H` / `P1D` — the values the reminder
/// `<DropDown>` offers cross-app) carry their `events.reminder.*` key; any
/// non-preset offset falls back to the raw value, which `resolve` renders
/// verbatim (no i18n entry exists). Shared across every app so the preset →
/// label contract can't drift per-app (events.md § Reminders); previously
/// hand-rolled identically on linux/web/windows/apple.
pub fn reminder_label(offset: &str) -> crate::localized::LocalizedText {
    use crate::localized::LocalizedText;
    match offset {
        "PT15M" => LocalizedText::key("events.reminder.min_15"),
        "PT1H" => LocalizedText::key("events.reminder.hour_1"),
        "P1D" => LocalizedText::key("events.reminder.day_1"),
        other => LocalizedText::key(other),
    }
}

/// One entry of the reminder preset picker: the ISO-8601 offset `value` the
/// select writes (and the cross-app `select(id, "PT1H")` e2e contract
/// drives — never localized) plus its [`reminder_label`] display label.
pub struct ReminderOption {
    pub value: String,
    pub label: crate::localized::LocalizedText,
}

/// The canonical reminder preset catalog — `PT15M` / `PT1H` / `P1D` in picker
/// order (events.md § Reminders), each paired with its [`reminder_label`].
/// The ONE list every app's reminder `<DropDown>` renders; the value list
/// was previously hand-rolled per client (and linux showed the user the raw
/// ISO strings, windows hard-coded English item labels).
pub fn reminder_presets() -> Vec<ReminderOption> {
    ["PT15M", "PT1H", "P1D"]
        .into_iter()
        .map(|value| ReminderOption {
            value: value.to_string(),
            label: reminder_label(value),
        })
        .collect()
}

/// Map an attendee's RSVP attendance status to its human label, as a
/// [`LocalizedText`] each app resolves through its own i18n pipeline. The
/// canonical vocabulary (events.md § Attendee list presentation) — every value
/// the shared projection [`fauna_client_caldav::project_attendee_rsvp`] can emit
/// (`going` / `interested` / `tentative` / `declined` / `invited`) plus the
/// goal-doc color vocabulary's `waitlisted` — carries its `events.rsvp.*` key;
/// any unknown status falls back to its capitalized form rendered verbatim (no
/// i18n entry), preserving the prior per-app capitalize-the-raw behavior.
/// Shared across every app so the status → label contract can't drift
/// per-app; previously hand-rolled identically on web/linux/android/apple
/// (and shown raw-lowercase on windows). The trailing **color** stays an
/// idiomatic per-app render (events.md § Attendee list presentation), not
/// part of this lift.
pub fn rsvp_status_label(status: &str) -> crate::localized::LocalizedText {
    use crate::localized::LocalizedText;
    match status {
        "going" => LocalizedText::key("events.rsvp.going"),
        "interested" => LocalizedText::key("events.rsvp.interested"),
        "tentative" => LocalizedText::key("events.rsvp.tentative"),
        "declined" => LocalizedText::key("events.rsvp.declined"),
        "waitlisted" => LocalizedText::key("events.rsvp.waitlisted"),
        "invited" => LocalizedText::key("events.rsvp.invited"),
        "" => LocalizedText::default(),
        other => {
            // Unknown status → capitalize the first char and render verbatim
            // (no i18n entry matches, so `resolve` returns the key as-is).
            let mut chars = other.chars();
            let capitalized = match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            };
            LocalizedText::key(capitalized)
        }
    }
}

/// Merge an inbound iTIP `REPLY` into an organizer's stored attendee roster:
/// each responding attendee (matched by email, case-insensitively) has their
/// `PARTSTAT` and derived `fauna_status` updated to the value in the reply,
/// keeping their stored `name`/`email`. A reply from an address not on the
/// roster is ignored (RFC 5546 §3.2.3 — the organizer only tracks invited
/// attendees). Pure + WASM-safe; the input is not mutated.
///
/// This is the v1 client-driven half of "applying a REPLY to the organizer's
/// stored event" (`caldav-server.md` § The one operation with a cost): the
/// caller re-seals + re-PUTs the event with the returned roster.
pub fn apply_reply_to_roster(
    stored_attendees: &[AttendeeInfo],
    reply_ical: &str,
) -> Vec<AttendeeInfo> {
    let mut roster = stored_attendees.to_vec();
    for responder in parse_ical_attendees(reply_ical) {
        if let Some(existing) = roster
            .iter_mut()
            .find(|a| a.email.eq_ignore_ascii_case(&responder.email))
        {
            existing.partstat = responder.partstat;
            existing.fauna_status = responder.fauna_status;
        }
    }
    roster
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminder_label_maps_every_preset() {
        // The three canonical reminder presets each map to their i18n key…
        assert_eq!(reminder_label("PT15M").key, "events.reminder.min_15");
        assert_eq!(reminder_label("PT1H").key, "events.reminder.hour_1");
        assert_eq!(reminder_label("P1D").key, "events.reminder.day_1");
        // …and a non-preset offset falls back to the raw value as the "key",
        // which `LocalizedText::resolve` renders verbatim (no i18n entry exists
        // for it) — preserving the prior per-app `other => other` behavior.
        let fallback = reminder_label("PT30M");
        assert_eq!(fallback.key, "PT30M");
        assert_eq!(fallback.resolve(|_| None::<&str>), "PT30M");
    }

    #[test]
    fn reminder_presets_carry_the_select_contract_values_in_picker_order() {
        // The catalog is the ONE list every app's reminder `<DropDown>`
        // renders (events.md § Reminders). The `value`s are the cross-app
        // `select(id, "PT1H")` e2e driver contract — ISO-8601 offsets, never
        // localized — and the labels ride `reminder_label`, so the pair can't
        // drift from the per-value lookup.
        let presets = reminder_presets();
        assert_eq!(
            presets.iter().map(|p| p.value.as_str()).collect::<Vec<_>>(),
            vec!["PT15M", "PT1H", "P1D"]
        );
        for p in &presets {
            assert_eq!(p.label.key, reminder_label(&p.value).key);
        }
        assert_eq!(presets[1].label.key, "events.reminder.hour_1");
    }

    #[test]
    fn rsvp_status_label_maps_every_status() {
        // Every status the shared projection can emit
        // (`fauna_client_caldav::project_attendee_rsvp` →
        // `going | interested | tentative | declined | invited`) plus the
        // goal-doc color vocabulary's `waitlisted` carry their `events.rsvp.*`
        // i18n key (events.md § Attendee list presentation).
        assert_eq!(rsvp_status_label("going").key, "events.rsvp.going");
        assert_eq!(
            rsvp_status_label("interested").key,
            "events.rsvp.interested"
        );
        assert_eq!(rsvp_status_label("tentative").key, "events.rsvp.tentative");
        assert_eq!(rsvp_status_label("declined").key, "events.rsvp.declined");
        assert_eq!(
            rsvp_status_label("waitlisted").key,
            "events.rsvp.waitlisted"
        );
        assert_eq!(rsvp_status_label("invited").key, "events.rsvp.invited");
        // An unknown status falls back to its capitalized form, rendered
        // verbatim (no i18n entry) — preserving the prior per-app
        // capitalize-the-raw behavior (the 4-of-5-client majority; windows,
        // which showed raw lowercase, converges onto this).
        let fallback = rsvp_status_label("rescinded");
        assert_eq!(fallback.key, "Rescinded");
        assert_eq!(fallback.resolve(|_| None::<&str>), "Rescinded");
        // An empty status resolves to empty (no spurious capitalization).
        assert_eq!(rsvp_status_label("").resolve(|_| None::<&str>), "");
    }

    #[test]
    fn parse_simple_event() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Team Meeting\r\n\
DTSTART:20260401T100000Z\r\n\
DTEND:20260401T110000Z\r\n\
LOCATION:Room 42\r\n\
UID:test-uid-1\r\n\
SEQUENCE:0\r\n\
DESCRIPTION:Weekly sync meeting\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        assert_eq!(fields.summary, "Team Meeting");
        assert_eq!(fields.dtstart, "2026-04-01T10:00:00Z");
        assert_eq!(fields.dtend, "2026-04-01T11:00:00Z");
        assert_eq!(fields.location, "Room 42");
        assert_eq!(fields.uid, "test-uid-1");
        assert_eq!(fields.sequence, 0);
        assert_eq!(fields.description, "Weekly sync meeting");
    }

    #[test]
    fn parse_event_with_rrule() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Standup\r\n\
DTSTART:20260401T090000Z\r\n\
DTEND:20260401T091500Z\r\n\
RRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR\r\n\
EXDATE:20260406T090000Z\r\n\
UID:recurring-1\r\n\
SEQUENCE:1\r\n\
STATUS:CONFIRMED\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        assert_eq!(fields.summary, "Standup");
        assert_eq!(fields.rrule, "FREQ=WEEKLY;BYDAY=MO,WE,FR");
        assert_eq!(fields.exdates, "2026-04-06T09:00:00Z");
        assert_eq!(fields.status, "confirmed");
        assert_eq!(fields.sequence, 1);
    }

    #[test]
    fn parse_folded_lines() {
        // RFC 5545 line folding: continuation lines start with a space.
        // Build the input explicitly to be clear about fold points.
        let ical = "BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:This is a very long summary that needs to be folded across \r\n \
multiple lines in the iCalendar format\r\n\
DTSTART:20260401T100000Z\r\n\
UID:fold-test\r\n\
DESCRIPTION:Line one\\nLine two\\nLine three\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        assert_eq!(
            fields.summary,
            "This is a very long summary that needs to be folded across multiple lines in the iCalendar format"
        );
        assert_eq!(fields.uid, "fold-test");
        assert_eq!(fields.description, "Line one\nLine two\nLine three");
    }

    #[test]
    fn parse_event_with_valarm() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Reminder Test\r\n\
DTSTART:20260401T100000Z\r\n\
UID:alarm-1\r\n\
BEGIN:VALARM\r\n\
TRIGGER:-PT15M\r\n\
ACTION:DISPLAY\r\n\
DESCRIPTION:Event reminder\r\n\
END:VALARM\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        assert_eq!(fields.alarm, "-PT15M");
    }

    #[test]
    fn parse_event_with_geo_and_url() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Geo Test\r\n\
DTSTART:20260401T100000Z\r\n\
UID:geo-1\r\n\
GEO:37.7749;-122.4194\r\n\
URL:https://fauna.social/events/geo-1\r\n\
CATEGORIES:meetup,tech\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        assert_eq!(fields.geo, "37.7749,-122.4194");
        assert_eq!(fields.url, "https://fauna.social/events/geo-1");
        assert_eq!(fields.categories, "meetup,tech");
    }

    #[test]
    fn parse_floating_datetime() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Floating\r\n\
DTSTART:20260401T100000\r\n\
DTEND:20260401T110000\r\n\
UID:float-1\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        assert_eq!(fields.dtstart, "2026-04-01T10:00:00");
        assert_eq!(fields.dtend, "2026-04-01T11:00:00");
    }

    #[test]
    fn parse_duration_field() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Duration Event\r\n\
DTSTART:20260401T100000Z\r\n\
DURATION:PT1H30M\r\n\
UID:dur-1\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        assert_eq!(fields.duration, "PT1H30M");
        assert!(fields.dtend.is_empty());
    }

    #[test]
    fn generate_simple_event() {
        let event = EventFields {
            summary: "Team Meeting".into(),
            dtstart: "2026-04-01T10:00:00Z".into(),
            dtend: "2026-04-01T11:00:00Z".into(),
            location: "Room 42".into(),
            uid: "gen-test-1".into(),
            description: "Weekly sync".into(),
            ..Default::default()
        };

        let ical = generate_ical(&event, &[], "alice@fauna.social");
        assert!(ical.contains("BEGIN:VCALENDAR"));
        assert!(ical.contains("VERSION:2.0"));
        assert!(ical.contains("PRODID:-//Fauna//CalDAV//EN"));
        assert!(ical.contains("BEGIN:VEVENT"));
        assert!(ical.contains("UID:gen-test-1"));
        assert!(ical.contains("SUMMARY:Team Meeting"));
        assert!(ical.contains("DTSTART:20260401T100000Z"));
        assert!(ical.contains("DTEND:20260401T110000Z"));
        assert!(ical.contains("LOCATION:Room 42"));
        assert!(ical.contains("ORGANIZER:mailto:alice@fauna.social"));
        assert!(ical.contains("DESCRIPTION:Weekly sync"));
        assert!(ical.contains("END:VEVENT"));
        assert!(ical.contains("END:VCALENDAR"));
    }

    #[test]
    fn generate_with_attendees() {
        let event = EventFields {
            summary: "Party".into(),
            dtstart: "2026-04-01T18:00:00Z".into(),
            uid: "party-1".into(),
            ..Default::default()
        };

        let attendees = vec![
            AttendeeInfo {
                name: "Bob".into(),
                email: "bob@example.com".into(),
                partstat: "ACCEPTED".into(),
                fauna_status: "going".into(),
            },
            AttendeeInfo {
                name: "Carol".into(),
                email: "carol@example.com".into(),
                partstat: "TENTATIVE".into(),
                fauna_status: "interested".into(),
            },
        ];

        let ical = generate_ical(&event, &attendees, "alice@fauna.social");
        assert!(ical.contains("ATTENDEE;CN=Bob;PARTSTAT=ACCEPTED:mailto:bob@example.com"));
        assert!(ical.contains("ATTENDEE;CN=Carol;PARTSTAT=TENTATIVE:mailto:carol@example.com"));
    }

    #[test]
    fn generate_with_alarm() {
        let event = EventFields {
            summary: "Alarm Test".into(),
            dtstart: "2026-04-01T10:00:00Z".into(),
            uid: "alarm-gen-1".into(),
            alarm: "-PT15M".into(),
            ..Default::default()
        };

        let ical = generate_ical(&event, &[], "");
        assert!(ical.contains("BEGIN:VALARM"));
        assert!(ical.contains("TRIGGER:-PT15M"));
        assert!(ical.contains("ACTION:DISPLAY"));
        assert!(ical.contains("END:VALARM"));
    }

    #[test]
    fn generate_with_rrule_and_exdates() {
        let event = EventFields {
            summary: "Recurring".into(),
            dtstart: "2026-04-01T09:00:00Z".into(),
            uid: "recur-1".into(),
            rrule: "FREQ=WEEKLY;BYDAY=MO".into(),
            exdates: "2026-04-08T09:00:00Z,2026-04-15T09:00:00Z".into(),
            ..Default::default()
        };

        let ical = generate_ical(&event, &[], "");
        assert!(ical.contains("RRULE:FREQ=WEEKLY;BYDAY=MO"));
        assert!(ical.contains("EXDATE:20260408T090000Z"));
        assert!(ical.contains("EXDATE:20260415T090000Z"));
    }

    #[test]
    fn round_trip() {
        let original = EventFields {
            summary: "Round Trip Event".into(),
            dtstart: "2026-04-01T10:00:00Z".into(),
            dtend: "2026-04-01T11:00:00Z".into(),
            location: "Room 42".into(),
            uid: "round-trip-1".into(),
            sequence: 3,
            status: "confirmed".into(),
            description: "A test event".into(),
            categories: "test,demo".into(),
            ..Default::default()
        };

        let ical = generate_ical(&original, &[], "alice@fauna.social");
        let parsed = parse_ical(&ical).unwrap();

        assert_eq!(parsed.summary, original.summary);
        assert_eq!(parsed.dtstart, original.dtstart);
        assert_eq!(parsed.dtend, original.dtend);
        assert_eq!(parsed.location, original.location);
        assert_eq!(parsed.uid, original.uid);
        assert_eq!(parsed.sequence, original.sequence);
        assert_eq!(parsed.status, original.status);
        assert_eq!(parsed.description, original.description);
        assert_eq!(parsed.categories, original.categories);
    }

    #[test]
    fn round_trip_with_special_chars() {
        let original = EventFields {
            summary: "Meeting; with, special\\chars".into(),
            dtstart: "2026-04-01T10:00:00Z".into(),
            uid: "special-1".into(),
            description: "Line one\nLine two\nLine three".into(),
            ..Default::default()
        };

        let ical = generate_ical(&original, &[], "");
        let parsed = parse_ical(&ical).unwrap();

        assert_eq!(parsed.summary, original.summary);
        assert_eq!(parsed.description, original.description);
    }

    #[test]
    fn partstat_mapping() {
        assert_eq!(partstat_from_fauna("going"), "ACCEPTED");
        assert_eq!(partstat_from_fauna("interested"), "TENTATIVE");
        assert_eq!(partstat_from_fauna("declined"), "DECLINED");
        assert_eq!(partstat_from_fauna("invited"), "NEEDS-ACTION");
        assert_eq!(partstat_from_fauna("waitlisted"), "NEEDS-ACTION");
        assert_eq!(partstat_from_fauna("unknown"), "NEEDS-ACTION");
    }

    #[test]
    fn fauna_status_mapping() {
        assert_eq!(fauna_status_from_partstat("ACCEPTED"), "going");
        assert_eq!(fauna_status_from_partstat("TENTATIVE"), "interested");
        assert_eq!(fauna_status_from_partstat("DECLINED"), "declined");
        assert_eq!(fauna_status_from_partstat("NEEDS-ACTION"), "invited");
        assert_eq!(fauna_status_from_partstat("accepted"), "going");
        assert_eq!(fauna_status_from_partstat("unknown"), "invited");
    }

    #[test]
    fn line_folding() {
        let long = "DESCRIPTION:".to_string() + &"x".repeat(100);
        let folded = fold_line(&long);
        // Every line must be <= 75 octets
        for line in folded.split("\r\n") {
            assert!(
                line.len() <= 75,
                "Line too long ({} octets): {:?}",
                line.len(),
                line
            );
        }
        // Unfolding should restore the original
        let unfolded = unfold_lines(&folded);
        assert_eq!(unfolded.trim(), long);
    }

    #[test]
    fn no_vevent_block_errors() {
        let ical = "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n";
        assert!(parse_ical(ical).is_err());
    }

    #[test]
    fn parse_lf_line_endings() {
        // Should work with LF-only line endings too
        let ical = "BEGIN:VCALENDAR\n\
BEGIN:VEVENT\n\
SUMMARY:LF Test\n\
DTSTART:20260401T100000Z\n\
UID:lf-1\n\
END:VEVENT\n\
END:VCALENDAR\n";

        let fields = parse_ical(ical).unwrap();
        assert_eq!(fields.summary, "LF Test");
        assert_eq!(fields.dtstart, "2026-04-01T10:00:00Z");
    }

    #[test]
    fn to_ical_datetime_conversions() {
        assert_eq!(to_ical_datetime("2026-04-01T10:00:00Z"), "20260401T100000Z");
        assert_eq!(to_ical_datetime("2026-04-01T10:00:00"), "20260401T100000");
        assert_eq!(to_ical_datetime("2026-04-01"), "20260401");
        // .ToString("o") in C# emits fractional seconds + offset; strip both so the
        // compact form is exactly 15 chars and normalize_datetime can re-parse it.
        assert_eq!(
            to_ical_datetime("2026-06-24T00:00:00.0000000+02:00"),
            "20260624T000000"
        );
        assert_eq!(
            to_ical_datetime("2026-06-24T14:30:45.1234567-05:00"),
            "20260624T143045"
        );
        assert_eq!(
            to_ical_datetime("2026-06-24T10:00:00.000Z"),
            "20260624T100000Z"
        );

        // A seconds-less time (datetime-local `HH:MM`, what the e2e create form +
        // any client sending `YYYY-MM-DDTHH:MM` produces) MUST pad to full HHMMSS
        // so the emitted iCalendar is well-formed — a bare `T1400` is malformed
        // RFC 5545 and breaks every app's date parse + external CalDAV interop.
        assert_eq!(to_ical_datetime("2026-06-28T14:00"), "20260628T140000");
        assert_eq!(to_ical_datetime("2026-06-28T14:00Z"), "20260628T140000Z");
    }

    #[test]
    fn normalize_datetime_recovers_seconds_less_compact_form() {
        // Read-tolerance for a seconds-less compact value (`YYYYMMDDTHHMM`, 13
        // chars) that an earlier buggy to_ical_datetime stored before the
        // seconds-padding fix — recover it as a parseable RFC 3339 datetime rather
        // than returning the unparseable compact string (no data loss for alpha
        // events already written).
        assert_eq!(
            normalize_datetime("20260628T1400", ""),
            "2026-06-28T14:00:00"
        );
        assert_eq!(
            normalize_datetime("20260628T1400Z", ""),
            "2026-06-28T14:00:00Z"
        );
        // The well-formed 15-char form is unchanged.
        assert_eq!(
            normalize_datetime("20260628T140000Z", ""),
            "2026-06-28T14:00:00Z"
        );
    }

    fn vevent_with_dtstart(dtstart: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:non-ascii-1\r\n\
             DTSTART:{dtstart}\r\nSUMMARY:x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        )
    }

    #[test]
    fn parse_ical_non_ascii_compact_datetime_does_not_panic() {
        // The compact branches are guarded by byte length and a `T` at
        // byte 8 only, so a multi-byte char inside the date made `&base[0..4]`
        // split it and panic — inside the MTA's delivery loop and an app's
        // receive rail. `é` spans bytes 3-4 in both; 13- and 15-byte twins.
        for dtstart in ["202é010T1000", "202é010T100000", "202é010T1000Z"] {
            let fields = parse_ical(&vevent_with_dtstart(dtstart)).expect("parses");
            // Unrecognized compact values pass through unchanged.
            assert_eq!(fields.dtstart, dtstart);
        }
    }

    #[test]
    fn render_stored_event_non_ascii_rfc3339_time_does_not_panic() {
        // Render side: an RFC 3339-looking value passes
        // normalize_datetime unchanged, then to_ical_datetime's `±HH:MM` strip
        // sliced the 7-byte time `€0000` at byte 1, inside `€`.
        let fields = parse_ical(&vevent_with_dtstart("2026-01-01T€0000")).expect("parses");
        let ics = render_stored_event(&fields, &[], "org@example.com", 0);
        assert!(ics.contains("BEGIN:VEVENT"));
    }

    #[test]
    fn round_trip_seconds_less_datetime_stays_parseable() {
        // The exact failure: a create form sends a seconds-less
        // dtstart; it must serialize → parse back to a seconds-bearing RFC 3339
        // string the apps' week/day time-grid date parsers accept, else the
        // event is silently dropped from every grid column (zero
        // calendar-event-block) while the summary-based agenda still shows it.
        let original = EventFields {
            summary: "Standup".into(),
            dtstart: "2026-06-28T14:00".into(),
            dtend: "2026-06-28T15:00".into(),
            uid: "seconds-less-1".into(),
            ..Default::default()
        };
        let ical = generate_ical(&original, &[], "");
        assert!(
            ical.contains("DTSTART:20260628T140000"),
            "well-formed 6-digit time expected, got:\n{ical}"
        );
        assert!(
            !ical.contains("DTSTART:20260628T1400\r"),
            "malformed 4-digit time must not be emitted, got:\n{ical}"
        );
        let parsed = parse_ical(&ical).unwrap();
        assert_eq!(parsed.dtstart, "2026-06-28T14:00:00");
        assert_eq!(parsed.dtend, "2026-06-28T15:00:00");
    }

    #[test]
    fn multiple_exdates() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Multi EXDATE\r\n\
DTSTART:20260401T100000Z\r\n\
UID:exdate-multi\r\n\
EXDATE:20260408T100000Z\r\n\
EXDATE:20260415T100000Z,20260422T100000Z\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        let exdates: Vec<&str> = fields.exdates.split(',').collect();
        assert_eq!(exdates.len(), 3);
        assert_eq!(exdates[0], "2026-04-08T10:00:00Z");
        assert_eq!(exdates[1], "2026-04-15T10:00:00Z");
        assert_eq!(exdates[2], "2026-04-22T10:00:00Z");
    }

    #[test]
    fn parse_rrule_weekly_with_count() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Tuesday Meeting\r\n\
DTSTART:20260401T100000Z\r\n\
DTEND:20260401T110000Z\r\n\
RRULE:FREQ=WEEKLY;BYDAY=TU;COUNT=10\r\n\
UID:rrule-count-1\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        assert_eq!(fields.rrule, "FREQ=WEEKLY;BYDAY=TU;COUNT=10");
        assert_eq!(fields.summary, "Tuesday Meeting");
    }

    #[test]
    fn parse_exdates_comma_separated() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Skipped Instances\r\n\
DTSTART:20260401T100000Z\r\n\
UID:exdate-csv\r\n\
EXDATE:20260408T100000Z,20260415T100000Z\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        let exdates: Vec<&str> = fields.exdates.split(',').collect();
        assert_eq!(exdates.len(), 2);
        assert_eq!(exdates[0], "2026-04-08T10:00:00Z");
        assert_eq!(exdates[1], "2026-04-15T10:00:00Z");
    }

    #[test]
    fn generate_rrule_and_exdates_round_trip() {
        let event = EventFields {
            summary: "Weekly".into(),
            dtstart: "2026-04-01T10:00:00Z".into(),
            uid: "rrule-rt".into(),
            rrule: "FREQ=WEEKLY;BYDAY=TU;COUNT=10".into(),
            exdates: "2026-04-08T10:00:00Z,2026-04-15T10:00:00Z".into(),
            ..Default::default()
        };

        let ical = generate_ical(&event, &[], "");
        assert!(ical.contains("RRULE:FREQ=WEEKLY;BYDAY=TU;COUNT=10"));
        assert!(ical.contains("EXDATE:20260408T100000Z"));
        assert!(ical.contains("EXDATE:20260415T100000Z"));

        // Parse it back
        let parsed = parse_ical(&ical).unwrap();
        assert_eq!(parsed.rrule, "FREQ=WEEKLY;BYDAY=TU;COUNT=10");
        let exdates: Vec<&str> = parsed.exdates.split(',').collect();
        assert_eq!(exdates.len(), 2);
        assert_eq!(exdates[0], "2026-04-08T10:00:00Z");
        assert_eq!(exdates[1], "2026-04-15T10:00:00Z");
    }

    #[test]
    fn parse_recurrence_id() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Modified Instance\r\n\
DTSTART:20260408T110000Z\r\n\
DTEND:20260408T120000Z\r\n\
UID:recur-parent@fauna\r\n\
RECURRENCE-ID:20260408T100000Z\r\n\
SEQUENCE:1\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        assert_eq!(fields.recurrence_id, "2026-04-08T10:00:00Z");
        assert_eq!(fields.summary, "Modified Instance");
        assert_eq!(fields.uid, "recur-parent@fauna");
    }

    #[test]
    fn generate_recurrence_id() {
        let event = EventFields {
            summary: "Modified Instance".into(),
            dtstart: "2026-04-08T11:00:00Z".into(),
            dtend: "2026-04-08T12:00:00Z".into(),
            uid: "recur-parent@fauna".into(),
            recurrence_id: "2026-04-08T10:00:00Z".into(),
            sequence: 1,
            ..Default::default()
        };

        let ical = generate_ical(&event, &[], "");
        assert!(ical.contains("RECURRENCE-ID:20260408T100000Z"));
        assert!(ical.contains("UID:recur-parent@fauna"));

        // Round-trip
        let parsed = parse_ical(&ical).unwrap();
        assert_eq!(parsed.recurrence_id, "2026-04-08T10:00:00Z");
    }

    #[test]
    fn generate_waitlisted_attendee_has_x_fauna_status() {
        let event = EventFields {
            summary: "Capacity Event".into(),
            dtstart: "2026-04-01T18:00:00Z".into(),
            uid: "waitlist-1".into(),
            ..Default::default()
        };

        let attendees = vec![
            AttendeeInfo {
                name: "Alice".into(),
                email: "alice@example.com".into(),
                partstat: "ACCEPTED".into(),
                fauna_status: "going".into(),
            },
            AttendeeInfo {
                name: "Dave".into(),
                email: "dave@example.com".into(),
                partstat: "NEEDS-ACTION".into(),
                fauna_status: "waitlisted".into(),
            },
        ];

        let ical = generate_ical(&event, &attendees, "host@example.com");
        // Unfold to check content (long lines get folded at 75 octets)
        let unfolded = unfold_lines(&ical);
        // Non-waitlisted: no X-FAUNA-STATUS
        assert!(unfolded.contains("ATTENDEE;CN=Alice;PARTSTAT=ACCEPTED:mailto:alice@example.com"));
        assert!(!unfolded.contains("X-FAUNA-STATUS=going"));
        // Waitlisted: includes X-FAUNA-STATUS=waitlisted
        assert!(unfolded.contains(
            "ATTENDEE;CN=Dave;PARTSTAT=NEEDS-ACTION;X-FAUNA-STATUS=waitlisted:mailto:dave@example.com"
        ));
    }

    #[test]
    fn parse_ical_attendees_basic() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Party\r\n\
DTSTART:20260401T180000Z\r\n\
UID:party-1\r\n\
ATTENDEE;CN=Bob;PARTSTAT=ACCEPTED:mailto:bob@example.com\r\n\
ATTENDEE;CN=Carol;PARTSTAT=TENTATIVE:mailto:carol@example.com\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let attendees = parse_ical_attendees(ical);
        assert_eq!(attendees.len(), 2);
        assert_eq!(attendees[0].name, "Bob");
        assert_eq!(attendees[0].email, "bob@example.com");
        assert_eq!(attendees[0].partstat, "ACCEPTED");
        assert_eq!(attendees[0].fauna_status, "going");
        assert_eq!(attendees[1].name, "Carol");
        assert_eq!(attendees[1].partstat, "TENTATIVE");
        assert_eq!(attendees[1].fauna_status, "interested");
    }

    #[test]
    fn parse_ical_organizer_handles_mailto_params_and_absence() {
        // mailto: scheme stripped.
        assert_eq!(
            parse_ical_organizer("ORGANIZER:mailto:alice@x.test\r\n").as_deref(),
            Some("alice@x.test")
        );
        // CN param before the value.
        assert_eq!(
            parse_ical_organizer("ORGANIZER;CN=Alice:mailto:alice@x.test\r\n").as_deref(),
            Some("alice@x.test")
        );
        // Bare address (no mailto: scheme).
        assert_eq!(
            parse_ical_organizer("ORGANIZER:alice@x.test\r\n").as_deref(),
            Some("alice@x.test")
        );
        // Uppercase MAILTO: scheme.
        assert_eq!(
            parse_ical_organizer("ORGANIZER:MAILTO:alice@x.test\r\n").as_deref(),
            Some("alice@x.test")
        );
        // No ORGANIZER line → None.
        assert_eq!(parse_ical_organizer("SUMMARY:no organizer here\r\n"), None);
    }

    #[test]
    fn parse_ical_organizer_in_full_vevent() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Kickoff\r\n\
DTSTART:20260401T180000Z\r\n\
UID:kickoff-1\r\n\
ORGANIZER;CN=Alice Organizer:mailto:alice@fauna.test\r\n\
ATTENDEE;CN=Bob:mailto:bob@fauna.test\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
        assert_eq!(
            parse_ical_organizer(ical).as_deref(),
            Some("alice@fauna.test")
        );
    }

    #[test]
    fn parse_ical_attendees_x_fauna_status_waitlisted() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Full Event\r\n\
DTSTART:20260401T180000Z\r\n\
UID:full-1\r\n\
ATTENDEE;CN=Eve;PARTSTAT=NEEDS-ACTION;X-FAUNA-STATUS=waitlisted:mailto:eve@example.com\r\n\
ATTENDEE;CN=Frank;PARTSTAT=NEEDS-ACTION:mailto:frank@example.com\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let attendees = parse_ical_attendees(ical);
        assert_eq!(attendees.len(), 2);
        // Eve has X-FAUNA-STATUS=waitlisted → fauna_status should be "waitlisted"
        assert_eq!(attendees[0].name, "Eve");
        assert_eq!(attendees[0].partstat, "NEEDS-ACTION");
        assert_eq!(attendees[0].fauna_status, "waitlisted");
        // Frank has plain NEEDS-ACTION → fauna_status should be "invited"
        assert_eq!(attendees[1].name, "Frank");
        assert_eq!(attendees[1].partstat, "NEEDS-ACTION");
        assert_eq!(attendees[1].fauna_status, "invited");
    }

    #[test]
    fn round_trip_waitlisted_attendee() {
        let event = EventFields {
            summary: "Waitlist RT".into(),
            dtstart: "2026-04-01T18:00:00Z".into(),
            uid: "waitlist-rt".into(),
            ..Default::default()
        };

        let attendees = vec![
            AttendeeInfo {
                name: "Alice".into(),
                email: "alice@example.com".into(),
                partstat: "ACCEPTED".into(),
                fauna_status: "going".into(),
            },
            AttendeeInfo {
                name: "Waitlisted".into(),
                email: "wait@example.com".into(),
                partstat: "NEEDS-ACTION".into(),
                fauna_status: "waitlisted".into(),
            },
        ];

        let ical = generate_ical(&event, &attendees, "host@example.com");
        let parsed = parse_ical_attendees(&ical);

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].fauna_status, "going");
        assert_eq!(parsed[1].fauna_status, "waitlisted");
    }

    #[test]
    fn parse_ical_multi_three_events() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Event One\r\n\
DTSTART:20260401T090000Z\r\n\
UID:multi-1\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Event Two\r\n\
DTSTART:20260401T120000Z\r\n\
UID:multi-2\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Event Three\r\n\
DTSTART:20260401T150000Z\r\n\
UID:multi-3\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let results = parse_ical_multi(ical);
        assert_eq!(results.len(), 3);
        let e1 = results[0].as_ref().unwrap();
        assert_eq!(e1.summary, "Event One");
        assert_eq!(e1.uid, "multi-1");
        let e2 = results[1].as_ref().unwrap();
        assert_eq!(e2.summary, "Event Two");
        assert_eq!(e2.uid, "multi-2");
        let e3 = results[2].as_ref().unwrap();
        assert_eq!(e3.summary, "Event Three");
        assert_eq!(e3.uid, "multi-3");
    }

    #[test]
    fn parse_ical_multi_one_valid_one_malformed() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Good Event\r\n\
DTSTART:20260401T090000Z\r\n\
UID:good-1\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let results = parse_ical_multi(ical);
        assert_eq!(results.len(), 2);
        assert!(results[0].is_ok());
        assert_eq!(results[0].as_ref().unwrap().summary, "Good Event");
        // The second VEVENT has no properties but still parses (empty fields, no error
        // from our parser since it produces default EventFields)
        // It should be Ok with empty/default fields
        assert!(results[1].is_ok());
    }

    #[test]
    fn parse_ical_multi_empty() {
        let ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n";
        let results = parse_ical_multi(ical);
        assert!(results.is_empty());
    }

    #[test]
    fn generate_ical_multi_round_trip() {
        let events = vec![
            (
                EventFields {
                    summary: "First".into(),
                    dtstart: "2026-04-01T09:00:00Z".into(),
                    uid: "gen-multi-1".into(),
                    ..Default::default()
                },
                vec![],
                String::new(),
            ),
            (
                EventFields {
                    summary: "Second".into(),
                    dtstart: "2026-04-01T12:00:00Z".into(),
                    uid: "gen-multi-2".into(),
                    location: "Room B".into(),
                    ..Default::default()
                },
                vec![],
                String::new(),
            ),
        ];

        let ics = generate_ical_multi(&events);
        assert!(ics.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(ics.ends_with("END:VCALENDAR\r\n"));

        // Should contain both VEVENTs
        let parsed = parse_ical_multi(&ics);
        assert_eq!(parsed.len(), 2);
        let p1 = parsed[0].as_ref().unwrap();
        assert_eq!(p1.summary, "First");
        assert_eq!(p1.uid, "gen-multi-1");
        let p2 = parsed[1].as_ref().unwrap();
        assert_eq!(p2.summary, "Second");
        assert_eq!(p2.uid, "gen-multi-2");
        assert_eq!(p2.location, "Room B");
    }

    // -----------------------------------------------------------------------
    // Windows timezone mapping tests
    // -----------------------------------------------------------------------

    #[test]
    fn windows_tz_eastern_standard_time() {
        assert_eq!(
            windows_tz_to_iana("Eastern Standard Time"),
            Some("America/New_York")
        );
    }

    #[test]
    fn windows_tz_case_insensitive() {
        assert_eq!(
            windows_tz_to_iana("eastern standard time"),
            Some("America/New_York")
        );
        assert_eq!(
            windows_tz_to_iana("PACIFIC STANDARD TIME"),
            Some("America/Los_Angeles")
        );
    }

    #[test]
    fn windows_tz_unknown_returns_none() {
        assert_eq!(windows_tz_to_iana("Narnia Standard Time"), None);
    }

    #[test]
    fn parse_dtstart_windows_eastern() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Outlook Meeting\r\n\
DTSTART;TZID=Eastern Standard Time:20260401T100000\r\n\
DTEND;TZID=Eastern Standard Time:20260401T110000\r\n\
UID:win-tz-1\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        assert_eq!(fields.dtstart, "2026-04-01T10:00:00-05:00");
        assert_eq!(fields.dtend, "2026-04-01T11:00:00-05:00");
    }

    #[test]
    fn parse_dtstart_windows_w_europe() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Berlin Standup\r\n\
DTSTART;TZID=W. Europe Standard Time:20260401T100000\r\n\
UID:win-tz-2\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        assert_eq!(fields.dtstart, "2026-04-01T10:00:00+01:00");
    }

    #[test]
    fn parse_dtstart_windows_tokyo() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Tokyo Sync\r\n\
DTSTART;TZID=Tokyo Standard Time:20260401T100000\r\n\
UID:win-tz-3\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        assert_eq!(fields.dtstart, "2026-04-01T10:00:00+09:00");
    }

    #[test]
    fn parse_dtstart_unknown_windows_tz_no_crash() {
        // Unknown Windows timezone should not crash — falls through to floating time
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Narnian Meeting\r\n\
DTSTART;TZID=Narnia Standard Time:20260401T100000\r\n\
UID:win-tz-unknown\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        // Falls through gracefully to floating time (no offset)
        assert_eq!(fields.dtstart, "2026-04-01T10:00:00");
    }

    #[test]
    fn parse_dtstart_iana_tzid_still_works() {
        // IANA timezone name in TZID should resolve directly
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:IANA TZ Event\r\n\
DTSTART;TZID=America/New_York:20260401T100000\r\n\
UID:iana-tz-1\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        assert_eq!(fields.dtstart, "2026-04-01T10:00:00-05:00");
    }

    #[test]
    fn parse_dtstart_iana_europe_london() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:London Call\r\n\
DTSTART;TZID=Europe/London:20260401T100000\r\n\
UID:iana-tz-london\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        assert_eq!(fields.dtstart, "2026-04-01T10:00:00+00:00");
    }

    #[test]
    fn parse_dtstart_utc_unaffected_by_tzid_logic() {
        // UTC times (trailing Z) should still produce the Z suffix
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:UTC Event\r\n\
DTSTART:20260401T100000Z\r\n\
UID:utc-still-works\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        assert_eq!(fields.dtstart, "2026-04-01T10:00:00Z");
    }

    #[test]
    fn windows_tz_coverage_spot_check() {
        // Verify a handful of the 40+ mappings are present
        assert_eq!(
            windows_tz_to_iana("Central Standard Time"),
            Some("America/Chicago")
        );
        assert_eq!(
            windows_tz_to_iana("Mountain Standard Time"),
            Some("America/Denver")
        );
        assert_eq!(
            windows_tz_to_iana("Hawaiian Standard Time"),
            Some("Pacific/Honolulu")
        );
        assert_eq!(
            windows_tz_to_iana("Alaskan Standard Time"),
            Some("America/Anchorage")
        );
        assert_eq!(
            windows_tz_to_iana("GMT Standard Time"),
            Some("Europe/London")
        );
        assert_eq!(
            windows_tz_to_iana("Romance Standard Time"),
            Some("Europe/Paris")
        );
        assert_eq!(
            windows_tz_to_iana("Russian Standard Time"),
            Some("Europe/Moscow")
        );
        assert_eq!(windows_tz_to_iana("FLE Standard Time"), Some("Europe/Kiev"));
        assert_eq!(
            windows_tz_to_iana("China Standard Time"),
            Some("Asia/Shanghai")
        );
        assert_eq!(
            windows_tz_to_iana("India Standard Time"),
            Some("Asia/Kolkata")
        );
        assert_eq!(
            windows_tz_to_iana("Singapore Standard Time"),
            Some("Asia/Singapore")
        );
        assert_eq!(
            windows_tz_to_iana("Korea Standard Time"),
            Some("Asia/Seoul")
        );
        assert_eq!(
            windows_tz_to_iana("Arab Standard Time"),
            Some("Asia/Riyadh")
        );
        assert_eq!(
            windows_tz_to_iana("SE Asia Standard Time"),
            Some("Asia/Bangkok")
        );
        assert_eq!(
            windows_tz_to_iana("West Asia Standard Time"),
            Some("Asia/Karachi")
        );
        assert_eq!(
            windows_tz_to_iana("Iran Standard Time"),
            Some("Asia/Tehran")
        );
        assert_eq!(
            windows_tz_to_iana("AUS Eastern Standard Time"),
            Some("Australia/Sydney")
        );
        assert_eq!(
            windows_tz_to_iana("New Zealand Standard Time"),
            Some("Pacific/Auckland")
        );
        assert_eq!(
            windows_tz_to_iana("Fiji Standard Time"),
            Some("Pacific/Fiji")
        );
        assert_eq!(
            windows_tz_to_iana("Samoa Standard Time"),
            Some("Pacific/Apia")
        );
        assert_eq!(
            windows_tz_to_iana("SA Eastern Standard Time"),
            Some("America/Sao_Paulo")
        );
        assert_eq!(
            windows_tz_to_iana("SA Pacific Standard Time"),
            Some("America/Bogota")
        );
        assert_eq!(
            windows_tz_to_iana("Central America Standard Time"),
            Some("America/Guatemala")
        );
        assert_eq!(
            windows_tz_to_iana("E. South America Standard Time"),
            Some("America/Sao_Paulo")
        );
        assert_eq!(
            windows_tz_to_iana("Venezuela Standard Time"),
            Some("America/Caracas")
        );
        assert_eq!(
            windows_tz_to_iana("South Africa Standard Time"),
            Some("Africa/Johannesburg")
        );
        assert_eq!(
            windows_tz_to_iana("Egypt Standard Time"),
            Some("Africa/Cairo")
        );
        assert_eq!(
            windows_tz_to_iana("W. Central Africa Standard Time"),
            Some("Africa/Lagos")
        );
    }

    #[test]
    fn extract_tzid_from_params() {
        assert_eq!(
            extract_tzid("TZID=Eastern Standard Time"),
            Some("Eastern Standard Time")
        );
        assert_eq!(
            extract_tzid("VALUE=DATE-TIME;TZID=America/New_York"),
            Some("America/New_York")
        );
        assert_eq!(
            extract_tzid("TZID=\"W. Europe Standard Time\""),
            Some("W. Europe Standard Time")
        );
        assert_eq!(extract_tzid("VALUE=DATE"), None);
        assert_eq!(extract_tzid(""), None);
    }

    // -----------------------------------------------------------------------
    // All-day event tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_all_day_event() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Holiday\r\n\
DTSTART;VALUE=DATE:20260401\r\n\
DTEND;VALUE=DATE:20260402\r\n\
UID:allday-1\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        assert_eq!(fields.dtstart, "2026-04-01");
        assert_eq!(fields.dtend, "2026-04-02");
        assert!(fields.is_all_day);
    }

    #[test]
    fn parse_non_all_day_event_is_false() {
        let ical = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
SUMMARY:Meeting\r\n\
DTSTART:20260401T100000Z\r\n\
DTEND:20260401T110000Z\r\n\
UID:not-allday\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

        let fields = parse_ical(ical).unwrap();
        assert!(!fields.is_all_day);
    }

    #[test]
    fn generate_all_day_event() {
        let event = EventFields {
            summary: "Holiday".into(),
            dtstart: "2026-04-01".into(),
            dtend: "2026-04-02".into(),
            uid: "allday-gen-1".into(),
            is_all_day: true,
            ..Default::default()
        };

        let ical = generate_ical(&event, &[], "");
        let unfolded = unfold_lines(&ical);
        assert!(unfolded.contains("DTSTART;VALUE=DATE:20260401"));
        assert!(unfolded.contains("DTEND;VALUE=DATE:20260402"));
        // Should NOT contain a T (time component) for DTSTART/DTEND
        assert!(!unfolded.contains("DTSTART:20260401T"));
        assert!(!unfolded.contains("DTEND:20260402T"));
    }

    #[test]
    fn round_trip_all_day_event() {
        let original = EventFields {
            summary: "Conference".into(),
            dtstart: "2026-04-01".into(),
            dtend: "2026-04-03".into(),
            uid: "allday-rt-1".into(),
            is_all_day: true,
            ..Default::default()
        };

        let ical = generate_ical(&original, &[], "");
        let parsed = parse_ical(&ical).unwrap();

        assert_eq!(parsed.dtstart, "2026-04-01");
        assert_eq!(parsed.dtend, "2026-04-03");
        assert!(parsed.is_all_day);
        assert_eq!(parsed.summary, original.summary);
        assert_eq!(parsed.uid, original.uid);
    }

    // -----------------------------------------------------------------------
    // Comprehensive round-trip with ALL fields
    // -----------------------------------------------------------------------

    #[test]
    fn round_trip_all_fields() {
        let original = EventFields {
            summary: "Full Event".into(),
            dtstart: "2026-04-01T10:00:00Z".into(),
            dtend: "2026-04-01T12:00:00Z".into(),
            duration: "PT2H".into(),
            location: "Room 101".into(),
            geo: "37.7749,-122.4194".into(),
            url: "https://fauna.social/events/full-1".into(),
            rrule: "FREQ=WEEKLY;BYDAY=MO,WE;COUNT=10".into(),
            exdates: "2026-04-08T10:00:00Z,2026-04-15T10:00:00Z".into(),
            categories: "work,meeting".into(),
            status: "confirmed".into(),
            uid: "full-rt-1".into(),
            sequence: 5,
            alarm: "-PT15M".into(),
            description: "A comprehensive test event\nwith multiple lines".into(),
            recurrence_id: String::new(),
            is_all_day: false,
            dtstamp: "2026-04-01T09:00:00Z".into(),
        };

        let ical = generate_ical(&original, &[], "organizer@fauna.social");
        // The RFC-5545-mandatory DTSTAMP must be emitted (else go-ical's encoder
        // on the MDA CalDAV serve path rejects the VEVENT — GAP 2).
        assert!(
            ical.contains("DTSTAMP:20260401T090000Z"),
            "DTSTAMP must be written: {ical}"
        );
        let parsed = parse_ical(&ical).unwrap();
        assert_eq!(parsed.dtstamp, original.dtstamp, "DTSTAMP must round-trip");

        assert_eq!(parsed.summary, original.summary);
        assert_eq!(parsed.dtstart, original.dtstart);
        assert_eq!(parsed.dtend, original.dtend);
        assert_eq!(parsed.duration, original.duration);
        assert_eq!(parsed.location, original.location);
        assert_eq!(parsed.geo, original.geo);
        assert_eq!(parsed.url, original.url);
        assert_eq!(parsed.rrule, original.rrule);
        assert_eq!(parsed.exdates, original.exdates);
        assert_eq!(parsed.categories, original.categories);
        assert_eq!(parsed.status, original.status);
        assert_eq!(parsed.uid, original.uid);
        assert_eq!(parsed.sequence, original.sequence);
        assert_eq!(parsed.alarm, original.alarm);
        assert_eq!(parsed.description, original.description);
        assert_eq!(parsed.is_all_day, original.is_all_day);
    }

    #[test]
    fn epoch_secs_to_ical_utc_known_values() {
        // The Unix epoch and a well-known later instant, both UTC.
        assert_eq!(epoch_secs_to_ical_utc(0), "19700101T000000Z");
        // 1_700_000_000 == 2023-11-14T22:13:20Z.
        assert_eq!(epoch_secs_to_ical_utc(1_700_000_000), "20231114T221320Z");
        // Leap-year day boundary: 2024-02-29T23:59:59Z == 1_709_251_199.
        assert_eq!(epoch_secs_to_ical_utc(1_709_251_199), "20240229T235959Z");
    }

    // -----------------------------------------------------------------------
    // iTIP / iMIP scheduling messages (RFC 5546 / RFC 6047)
    // -----------------------------------------------------------------------

    fn sample_meeting() -> (EventFields, Vec<AttendeeInfo>) {
        let event = EventFields {
            summary: "Project kickoff".into(),
            dtstart: "2026-07-01T15:00:00Z".into(),
            dtend: "2026-07-01T16:00:00Z".into(),
            uid: "kickoff-uid-1".into(),
            sequence: 0,
            ..Default::default()
        };
        let attendees = vec![
            AttendeeInfo {
                name: "Bob".into(),
                email: "bob@example.com".into(),
                partstat: "NEEDS-ACTION".into(),
                fauna_status: "invited".into(),
            },
            AttendeeInfo {
                name: "Carol".into(),
                email: "carol@example.com".into(),
                partstat: "NEEDS-ACTION".into(),
                fauna_status: "invited".into(),
            },
        ];
        (event, attendees)
    }

    /// The stored body of `sample_meeting` with Carol's `PARTSTAT` set.
    fn meeting_with_carol(partstat: &str) -> String {
        let (event, mut attendees) = sample_meeting();
        attendees[1].partstat = partstat.into();
        generate_ical(&event, &attendees, "alice@example.com")
    }

    #[test]
    fn attendee_reply_carries_only_the_answer_that_changed() {
        let prior = meeting_with_carol("NEEDS-ACTION");
        let answered = meeting_with_carol("ACCEPTED");
        let msg = build_attendee_reply_imip(
            &answered,
            Some(&prior),
            "Carol@example.com",
            "2026-06-04T12:30:00Z",
        )
        .expect("an accepted invitation owes the organizer a REPLY");
        // The roster's spelling, not the session's, and the organizer alone.
        assert_eq!(msg.from, "carol@example.com");
        assert_eq!(msg.recipients, vec!["alice@example.com".to_string()]);
        let raw = String::from_utf8(msg.raw_rfc5322).unwrap();
        assert!(raw.contains("method=REPLY"), "{raw}");
        assert!(raw.contains("METHOD:REPLY"), "{raw}");
        let body = &raw[raw.find("BEGIN:VCALENDAR").unwrap()..];
        let roster = parse_ical_attendees(body);
        assert_eq!(roster.len(), 1, "a REPLY names only the responder: {body}");
        assert_eq!(roster[0].email, "carol@example.com");
        assert_eq!(roster[0].partstat, "ACCEPTED");
        assert!(body.contains("UID:kickoff-uid-1"), "{body}");
    }

    #[test]
    fn attendee_reply_is_none_when_nothing_was_answered() {
        let prior = meeting_with_carol("ACCEPTED");
        let dtstamp = "2026-06-04T12:30:00Z";
        // Re-storing an unchanged answer (a calendar app syncing) sends nothing.
        assert!(
            build_attendee_reply_imip(&prior, Some(&prior), "carol@example.com", dtstamp).is_none()
        );
        // No answer yet.
        let pending = meeting_with_carol("NEEDS-ACTION");
        assert!(build_attendee_reply_imip(&pending, None, "carol@example.com", dtstamp).is_none());
        // Not on the roster.
        assert!(build_attendee_reply_imip(&prior, None, "dave@example.com", dtstamp).is_none());
        // The organizer's own PUT is the organizer fan-out's, never a REPLY.
        assert!(build_attendee_reply_imip(&prior, None, "alice@example.com", dtstamp).is_none());
        // An event with no organizer has nobody to reply to.
        let (event, attendees) = sample_meeting();
        let orphan = generate_ical(&event, &attendees, "").replace("ORGANIZER:mailto:\r\n", "");
        assert!(build_attendee_reply_imip(&orphan, None, "carol@example.com", dtstamp).is_none());
    }

    #[test]
    fn attendee_reply_fires_on_a_changed_answer_and_on_a_create() {
        let dtstamp = "2026-06-04T12:30:00Z";
        let accepted = meeting_with_carol("ACCEPTED");
        let declined = meeting_with_carol("DECLINED");
        let changed =
            build_attendee_reply_imip(&declined, Some(&accepted), "carol@example.com", dtstamp)
                .expect("changing an answer owes the organizer the new one");
        assert!(
            String::from_utf8(changed.raw_rfc5322)
                .unwrap()
                .contains("PARTSTAT=DECLINED")
        );
        // No prior body readable: the answer is sent rather than lost.
        assert!(build_attendee_reply_imip(&accepted, None, "carol@example.com", dtstamp).is_some());
    }

    #[test]
    fn itip_request_has_method_dtstamp_and_full_roster() {
        let (event, attendees) = sample_meeting();
        let ics = generate_itip(
            ITipMethod::Request,
            &event,
            &attendees,
            "alice@example.com",
            "2026-06-04T12:00:00Z",
        );
        // METHOD precedes the VEVENT, at the VCALENDAR level.
        let method_pos = ics.find("METHOD:REQUEST").expect("METHOD:REQUEST present");
        let vevent_pos = ics.find("BEGIN:VEVENT").expect("VEVENT present");
        assert!(
            method_pos < vevent_pos,
            "METHOD must come before BEGIN:VEVENT"
        );
        // DTSTAMP is mandatory in an iTIP VEVENT (RFC 5545).
        assert!(ics.contains("DTSTAMP:20260604T120000Z"));
        // Organizer + the full attendee roster ride along.
        assert!(ics.contains("ORGANIZER:mailto:alice@example.com"));
        assert!(ics.contains("mailto:bob@example.com"));
        assert!(ics.contains("mailto:carol@example.com"));
        assert!(ics.contains("UID:kickoff-uid-1"));
        // The body round-trips through the WASM-safe parser.
        let parsed = parse_ical(&ics).unwrap();
        assert_eq!(parsed.uid, "kickoff-uid-1");
        assert_eq!(parsed.summary, "Project kickoff");
        assert_eq!(parse_ical_attendees(&ics).len(), 2);
    }

    #[test]
    fn itip_reply_carries_single_attendee_and_request_status() {
        let event = EventFields {
            uid: "kickoff-uid-1".into(),
            dtstart: "2026-07-01T15:00:00Z".into(),
            sequence: 0,
            ..Default::default()
        };
        // A REPLY contains only the responding attendee.
        let responder = vec![AttendeeInfo {
            name: "Bob".into(),
            email: "bob@example.com".into(),
            partstat: "ACCEPTED".into(),
            fauna_status: "going".into(),
        }];
        let ics = generate_itip(
            ITipMethod::Reply,
            &event,
            &responder,
            "alice@example.com",
            "2026-06-04T12:30:00Z",
        );
        assert!(ics.contains("METHOD:REPLY"));
        assert!(ics.contains("REQUEST-STATUS:2.0;Success"));
        assert!(ics.contains("ORGANIZER:mailto:alice@example.com"));
        assert!(ics.contains("ATTENDEE;CN=Bob;PARTSTAT=ACCEPTED:mailto:bob@example.com"));
        // Only the one responding attendee.
        let parsed = parse_ical_attendees(&ics);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].email, "bob@example.com");
        assert_eq!(parsed[0].partstat, "ACCEPTED");
    }

    #[test]
    fn itip_cancel_has_method_cancel() {
        let (mut event, attendees) = sample_meeting();
        event.status = "cancelled".into();
        event.sequence = 1;
        let ics = generate_itip(
            ITipMethod::Cancel,
            &event,
            &attendees,
            "alice@example.com",
            "2026-06-04T13:00:00Z",
        );
        assert!(ics.contains("METHOD:CANCEL"));
        assert!(ics.contains("STATUS:CANCELLED"));
        assert!(ics.contains("SEQUENCE:1"));
        assert!(ics.contains("DTSTAMP:20260604T130000Z"));
    }

    #[test]
    fn itip_request_does_not_disturb_plain_generate_ical() {
        // The METHOD-less writer stays a bare VCALENDAR (no METHOD/DTSTAMP),
        // so the stored-body path is byte-for-byte unchanged by the refactor.
        let (event, attendees) = sample_meeting();
        let plain = generate_ical(&event, &attendees, "alice@example.com");
        assert!(!plain.contains("METHOD:"));
        assert!(!plain.contains("DTSTAMP:"));
        assert!(plain.contains("ORGANIZER:mailto:alice@example.com"));
        assert!(plain.contains("mailto:bob@example.com"));
    }

    #[test]
    fn apply_reply_updates_matching_attendee_partstat() {
        let (_event, stored) = sample_meeting();
        // Bob replies ACCEPTED.
        let reply = generate_itip(
            ITipMethod::Reply,
            &EventFields {
                uid: "kickoff-uid-1".into(),
                ..Default::default()
            },
            &[AttendeeInfo {
                name: "Bob".into(),
                email: "bob@example.com".into(),
                partstat: "ACCEPTED".into(),
                fauna_status: "going".into(),
            }],
            "alice@example.com",
            "2026-06-04T12:30:00Z",
        );
        let merged = apply_reply_to_roster(&stored, &reply);
        assert_eq!(merged.len(), 2);
        let bob = merged
            .iter()
            .find(|a| a.email == "bob@example.com")
            .unwrap();
        assert_eq!(bob.partstat, "ACCEPTED");
        assert_eq!(bob.fauna_status, "going");
        // Carol is untouched.
        let carol = merged
            .iter()
            .find(|a| a.email == "carol@example.com")
            .unwrap();
        assert_eq!(carol.partstat, "NEEDS-ACTION");
    }

    #[test]
    fn apply_reply_matches_email_case_insensitively() {
        let stored = vec![AttendeeInfo {
            name: "Bob".into(),
            email: "Bob@Example.com".into(),
            partstat: "NEEDS-ACTION".into(),
            fauna_status: "invited".into(),
        }];
        let reply = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
METHOD:REPLY\r\n\
BEGIN:VEVENT\r\n\
UID:x\r\n\
ATTENDEE;PARTSTAT=DECLINED:mailto:bob@example.com\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
        let merged = apply_reply_to_roster(&stored, reply);
        assert_eq!(merged[0].partstat, "DECLINED");
        assert_eq!(merged[0].fauna_status, "declined");
    }

    #[test]
    fn apply_reply_ignores_attendee_not_on_roster() {
        let stored = vec![AttendeeInfo {
            name: "Bob".into(),
            email: "bob@example.com".into(),
            partstat: "NEEDS-ACTION".into(),
            fauna_status: "invited".into(),
        }];
        let reply = "\
BEGIN:VCALENDAR\r\n\
METHOD:REPLY\r\n\
BEGIN:VEVENT\r\n\
UID:x\r\n\
ATTENDEE;PARTSTAT=ACCEPTED:mailto:stranger@example.com\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
        let merged = apply_reply_to_roster(&stored, reply);
        // Roster unchanged: stranger is not added, Bob is untouched.
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].partstat, "NEEDS-ACTION");
    }

    // -- attendee_display (the AttendeeRow text projection, events.md § Attendee list presentation) --

    #[test]
    fn attendee_display_cn_distinct_from_email() {
        // A real CN, distinct from the email → show the name, the email beneath, and the name's initial.
        let d = attendee_display("Bob Jones", "bob@example.com");
        assert_eq!(d.display_name, "Bob Jones");
        assert_eq!(d.monogram, "B");
        assert_eq!(d.secondary_email.as_deref(), Some("bob@example.com"));
    }

    #[test]
    fn attendee_display_no_cn_uses_email() {
        // No CN → display the bare email, no email-beneath (nothing to show twice), email's initial.
        let d = attendee_display("", "carol@example.com");
        assert_eq!(d.display_name, "carol@example.com");
        assert_eq!(d.monogram, "C");
        assert_eq!(d.secondary_email, None);
    }

    #[test]
    fn attendee_display_cn_equals_email_omits_secondary() {
        // CN == email (the VEVENT's CN is literally the address) → treat as "no real CN":
        // show the email once, omit the beneath line (the spec's "omitted when the name IS the email").
        let d = attendee_display("dave@example.com", "dave@example.com");
        assert_eq!(d.display_name, "dave@example.com");
        assert_eq!(d.monogram, "D");
        assert_eq!(d.secondary_email, None);
    }

    #[test]
    fn attendee_display_whitespace_only_cn_falls_back_to_email() {
        // A whitespace-only CN is not a real name → fall back to the email (android's isNotBlank
        // behavior; fixes apple's isEmpty path that would render "?").
        let d = attendee_display("   ", "erin@example.com");
        assert_eq!(d.display_name, "erin@example.com");
        assert_eq!(d.monogram, "E");
        assert_eq!(d.secondary_email, None);
    }

    #[test]
    fn attendee_display_monogram_trims_and_uppercases() {
        // Leading whitespace on a real CN must not yield a space monogram; the initial is uppercased.
        let d = attendee_display("  fiona", "fiona@example.com");
        assert_eq!(d.display_name, "  fiona");
        assert_eq!(d.monogram, "F");
        assert_eq!(d.secondary_email.as_deref(), Some("fiona@example.com"));
    }

    #[test]
    fn attendee_display_both_empty_yields_question_mark() {
        let d = attendee_display("", "");
        assert_eq!(d.display_name, "");
        assert_eq!(d.monogram, "?");
        assert_eq!(d.secondary_email, None);
    }
}
