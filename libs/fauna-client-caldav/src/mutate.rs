//! Client-side **read-mutate-rewrite** helpers for a stored CalDAV event.
//!
//! Each function takes the event's canonical VEVENT `.ics` (the unsealed body)
//! plus its optional decoded Fauna sidecar, applies one local edit (RSVP,
//! attendee add, reminder), and returns the [`EventRewrite`] pieces a caller
//! feeds straight into [`crate::CalDavClient::seal_and_put_event`] to re-PUT the
//! updated event. [`imip_inputs`] is the read-only sibling: it extracts the
//! fields/roster/organizer a caller feeds into [`crate::build_event_imip`] to
//! fan out a scheduling `REQUEST`/`CANCEL`.
//!
//! These are **pure and WASM-safe** (they touch only the `fauna_core::ical`
//! flat-fields reader/writer surface — no native `icalendar`/`rrule` parser, no
//! GTK, no transport), so all seven apps share one implementation (priority #2)
//! rather than each per-app Events shell reimplementing the RSVP/roster/alarm
//! arithmetic. Lifted from the linux lead's `caldav_backend.rs` (events.md
//! Decision B / caldav-server.md § Scheduling & invitations, § RSVP semantics).
//!
//! The asymmetric `interested ↔ TENTATIVE` sidecar rule lives in
//! [`apply_rsvp`] (caldav-server.md § RSVP semantics): a Fauna app renders an
//! attendee "Interested" *iff the sidecar marks them so*, otherwise it renders
//! the wire `PARTSTAT` verbatim — so `interested` writes a `TENTATIVE` PARTSTAT
//! plus a sidecar marker, and any other response clears the marker.

use crate::{
    AttendeeInfo, EventFields, FaunaEventExt, ITipMethod, ImipMessage, build_event_imip,
    epoch_secs_to_ical_utc, parse_ical, parse_ical_attendees, parse_ical_organizer,
};
use fauna_core::rsvp::RsvpResponse;

/// The result of an RSVP / attendee / reminder read-mutate-rewrite: the pieces a
/// caller feeds to [`crate::CalDavClient::seal_and_put_event`] to re-PUT the
/// updated event.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRewrite {
    /// The flat VEVENT fields with the edit applied (and `SEQUENCE` bumped).
    pub fields: EventFields,
    /// The full attendee roster after the edit.
    pub attendees: Vec<AttendeeInfo>,
    /// The organizer CAL-ADDRESS (the VEVENT `ORGANIZER`, falling back to the
    /// caller's own email when the body carried none).
    pub organizer_email: String,
    /// The Fauna sidecar after the edit (`None` collapses an all-default sidecar
    /// so a wire `TENTATIVE` doesn't masquerade as `interested`).
    pub fauna_ext: Option<FaunaEventExt>,
}

/// Apply an RSVP for `self_email` to a stored event's `.ics`, producing the
/// re-PUT pieces. Sets the attendee's wire `PARTSTAT` and the sidecar
/// `interested` marker (added iff the answer is [`RsvpResponse::Interested`],
/// removed otherwise) — the asymmetric rule (caldav-server.md § RSVP
/// semantics). If `self_email` is not yet on the roster it is appended.
/// `SEQUENCE` bumps so MUAs treat it as an update.
///
/// **This is the one gate every RSVP passes through**, whatever door it came
/// in by — linux and tui call it directly, the native apps reach it over
/// UniFFI, web over wasm — so an unrecognized `response` is refused *here*,
/// once, rather than each caller being trusted to have typed one of three
/// literals correctly. Before this it was not checked anywhere: an unknown
/// value flowed through `partstat_from_fauna`'s `_ =>` arm and landed as
/// `NEEDS-ACTION`, turning the user's answer into "hasn't answered" while the
/// call reported success. `tentative` was the live instance — four call
/// surfaces documented it as accepted and the mapping had no arm for it.
pub fn apply_rsvp(
    ics: &str,
    fauna_ext: Option<&FaunaEventExt>,
    self_email: &str,
    response: &str,
) -> Result<EventRewrite, String> {
    let answer = RsvpResponse::from_wire_str(response).ok_or_else(|| {
        format!(
            "unknown RSVP response {response:?} — expected one of {}, {} or {}",
            RsvpResponse::Going,
            RsvpResponse::Interested,
            RsvpResponse::Declined,
        )
    })?;
    let mut fields = parse_ical(ics).map_err(|e| format!("parse VEVENT: {e}"))?;
    fields.sequence = fields.sequence.saturating_add(1);
    let organizer_email = parse_ical_organizer(ics).unwrap_or_else(|| self_email.to_string());

    let mut attendees = parse_ical_attendees(ics);
    let partstat = answer.partstat().to_string();
    if let Some(me) = attendees
        .iter_mut()
        .find(|a| a.email.eq_ignore_ascii_case(self_email))
    {
        me.partstat = partstat;
        me.fauna_status = answer.to_string();
    } else {
        attendees.push(AttendeeInfo {
            name: String::new(),
            email: self_email.to_string(),
            partstat,
            fauna_status: answer.to_string(),
        });
    }

    let mut ext = fauna_ext.cloned().unwrap_or_default();
    ext.interested_attendees
        .retain(|a| !a.eq_ignore_ascii_case(self_email));
    if answer.marks_interested() {
        ext.interested_attendees.push(self_email.to_string());
    }
    let fauna_ext = if ext == FaunaEventExt::default() {
        None
    } else {
        Some(ext)
    };

    Ok(EventRewrite {
        fields,
        attendees,
        organizer_email,
        fauna_ext,
    })
}

/// Add an attendee (by **email address**) to a stored event's roster, producing
/// the re-PUT pieces. Appends `ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:<email>` to
/// the canonical VEVENT and, when this is the first attendee added to an event
/// with no `ORGANIZER`, sets `self_email` as the organizer (an event with a
/// roster must carry one — RFC 5546). Idempotent: an email already on the roster
/// (case-insensitive) is left untouched, so a repeated invite never doubles the
/// line and the re-PUT stays a no-op. `SEQUENCE` bumps only on an actual add, so
/// MUAs treat the roster change as an update.
///
/// Email is the universal attendee identifier (caldav-server.md § Scheduling &
/// invitations — a `mailto:` CAL-ADDRESS that, in production where the handle
/// domain == the mail domain, is simultaneously a deliverable address and a
/// Fauna identity); the iMIP `REQUEST` fan-out then reaches it over the outbound
/// mail path. The sidecar is preserved verbatim.
pub fn add_attendee(
    ics: &str,
    fauna_ext: Option<&FaunaEventExt>,
    self_email: &str,
    email: &str,
) -> Result<EventRewrite, String> {
    let email = email.trim();
    if email.is_empty() {
        return Err("attendee email is empty".to_string());
    }
    let mut fields = parse_ical(ics).map_err(|e| format!("parse VEVENT: {e}"))?;
    let organizer_email = parse_ical_organizer(ics)
        .filter(|o| !o.is_empty())
        .unwrap_or_else(|| self_email.to_string());

    let mut attendees = parse_ical_attendees(ics);
    let already = attendees
        .iter()
        .any(|a| a.email.eq_ignore_ascii_case(email));
    if !already {
        fields.sequence = fields.sequence.saturating_add(1);
        attendees.push(AttendeeInfo {
            name: String::new(),
            email: email.to_string(),
            partstat: "NEEDS-ACTION".to_string(),
            fauna_status: "invited".to_string(),
        });
    }

    Ok(EventRewrite {
        fields,
        attendees,
        organizer_email,
        fauna_ext: fauna_ext.cloned(),
    })
}

/// Set (or clear, when `offset` is empty) the single VEVENT reminder on a stored
/// event's `.ics`, producing the re-PUT pieces. The reminder is the `VALARM`
/// offset (`EventFields::alarm`, e.g. `-PT15M`) — events.md § Reminders ("at most
/// one reminder ... an offset relative to the event start"); it is NOT a separate
/// RPC on the encrypted path. Roster + sidecar are preserved verbatim.
pub fn set_reminder(
    ics: &str,
    fauna_ext: Option<&FaunaEventExt>,
    offset: &str,
) -> Result<EventRewrite, String> {
    let mut fields = parse_ical(ics).map_err(|e| format!("parse VEVENT: {e}"))?;
    fields.alarm = offset.to_string();
    fields.sequence = fields.sequence.saturating_add(1);
    Ok(EventRewrite {
        fields,
        attendees: parse_ical_attendees(ics),
        organizer_email: parse_ical_organizer(ics).unwrap_or_default(),
        fauna_ext: fauna_ext.cloned(),
    })
}

/// The iMIP-dispatch inputs extracted from a stored event's `.ics`: the flat
/// VEVENT fields, the raw attendee roster, and the organizer CAL-ADDRESS (the
/// VEVENT `ORGANIZER`, falling back to `self_email` when the body carries none).
/// Fed to [`crate::build_event_imip`] to fan out a scheduling `REQUEST`/`CANCEL`
/// (caldav-server.md § Scheduling & invitations → Server-side auto-schedule).
pub fn imip_inputs(
    ics: &str,
    self_email: &str,
) -> Result<(EventFields, Vec<AttendeeInfo>, String), String> {
    let fields = parse_ical(ics).map_err(|e| format!("parse VEVENT: {e}"))?;
    let attendees = parse_ical_attendees(ics);
    let organizer = parse_ical_organizer(ics).unwrap_or_else(|| self_email.to_string());
    Ok((fields, attendees, organizer))
}

/// Build the outbound iMIP `REPLY` a responding attendee owes the organizer after
/// an [`apply_rsvp`], or `None` in every case RFC 5546 sends nothing: the actor's
/// own event (no self-reply), an event with no `ORGANIZER`, or one the actor is
/// not rostered on. The caller hands the returned message straight to
/// `EmailClient::send` (best-effort — the local RSVP is already persisted).
///
/// This is the "Responding" half of caldav-server.md § Server-side auto-schedule.
/// It lives here — shared and WASM-safe (priority #2) — because every app's
/// Events shell (linux, the native UniFFI `FfiCaldavClient`, the web wasm seam)
/// otherwise re-inlines the identical organizer-diff + reply-construction, which
/// already drifted (the native faces sent the REPLY; the web wasm seam silently
/// dropped it, so a web user's accept/decline never reached an external organizer)
/// and stamped DTSTAMP two different ways. `now_secs` stamps the reply DTSTAMP via
/// the canonical [`epoch_secs_to_ical_utc`] (`build_event_imip` re-normalizes, so
/// the one format here is authoritative).
#[must_use]
pub fn imip_reply_for_rsvp(
    rw: &EventRewrite,
    self_email: &str,
    now_secs: i64,
) -> Option<ImipMessage> {
    let organizer = rw.organizer_email.trim();
    // Own event / no organizer → nothing to notify.
    if organizer.is_empty() || organizer.eq_ignore_ascii_case(self_email) {
        return None;
    }
    // RFC 5546 §3.2.3: a REPLY carries exactly the responding attendee.
    let me = rw
        .attendees
        .iter()
        .find(|a| a.email.eq_ignore_ascii_case(self_email))?;
    build_event_imip(
        ITipMethod::Reply,
        &rw.fields,
        std::slice::from_ref(me),
        organizer,
        &epoch_secs_to_ical_utc(now_secs),
    )
}

/// Build the outbound iMIP `REQUEST` an organizer fans to an event's
/// email-reachable roster after adding (or refreshing) attendees, or `None` when
/// nobody is email-reachable (RFC 5546 sends nothing). The caller hands the
/// returned message straight to `EmailClient::send` (best-effort — the roster
/// change is already persisted).
///
/// This is the "Organizer fan-out" half of caldav-server.md § Server-side
/// auto-schedule, symmetric to [`imip_reply_for_rsvp`]. It lives here — shared and
/// WASM-safe (priority #2) — so linux's `invite_to_event`, the native
/// `FfiCaldavClient`, and the web wasm `caldavInviteAttendee` seam all fan the
/// byte-identical `REQUEST` instead of each re-inlining the `build_event_imip`
/// call with a per-app DTSTAMP format (the web seam previously fanned *nothing*,
/// so a web user's invite never reached the attendee). `build_event_imip` already
/// excludes the organizer and dedups, so the caller passes the full post-edit
/// roster. `now_secs` stamps the DTSTAMP via the canonical [`epoch_secs_to_ical_utc`].
#[must_use]
pub fn imip_request_for_invite(
    fields: &EventFields,
    roster: &[AttendeeInfo],
    organizer: &str,
    now_secs: i64,
) -> Option<ImipMessage> {
    build_event_imip(
        ITipMethod::Request,
        fields,
        roster,
        organizer,
        &epoch_secs_to_ical_utc(now_secs),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate_ical;

    fn sample_fields() -> EventFields {
        EventFields {
            summary: "Standup".to_string(),
            dtstart: "2026-06-02T09:00:00Z".to_string(),
            dtend: "2026-06-02T09:15:00Z".to_string(),
            location: "Room 1".to_string(),
            description: "daily".to_string(),
            uid: "evt-1@fauna.test".to_string(),
            status: "confirmed".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn imip_inputs_recover_fields_roster_and_organizer() {
        let attendees = vec![
            AttendeeInfo {
                name: "Bob".to_string(),
                email: "bob@fauna.test".to_string(),
                partstat: "NEEDS-ACTION".to_string(),
                fauna_status: "invited".to_string(),
            },
            AttendeeInfo {
                name: "Carol".to_string(),
                email: "carol@fauna.test".to_string(),
                partstat: "NEEDS-ACTION".to_string(),
                fauna_status: "invited".to_string(),
            },
        ];
        let ics = generate_ical(&sample_fields(), &attendees, "alice@fauna.test");
        let (fields, roster, organizer) = imip_inputs(&ics, "fallback@fauna.test").expect("inputs");
        assert_eq!(fields.uid, "evt-1@fauna.test");
        assert_eq!(fields.summary, "Standup");
        assert_eq!(roster.len(), 2);
        assert_eq!(roster[0].email, "bob@fauna.test");
        // The VEVENT ORGANIZER wins over the fallback.
        assert_eq!(organizer, "alice@fauna.test");
    }

    #[test]
    fn imip_inputs_fall_back_to_self_email_without_organizer() {
        // A body with no ORGANIZER line (organizer = "") → fallback.
        let ics = generate_ical(&sample_fields(), &[], "");
        let (_fields, roster, organizer) = imip_inputs(&ics, "me@fauna.test").expect("inputs");
        assert!(roster.is_empty());
        assert_eq!(organizer, "me@fauna.test");
    }

    #[test]
    fn imip_reply_for_rsvp_notifies_a_different_organizer() {
        // An event organized by alice; carol RSVPs "going".
        let ics = generate_ical(&sample_fields(), &[], "alice@fauna.test");
        let rw = apply_rsvp(&ics, None, "carol@fauna.test", "going").expect("rsvp");
        let reply = imip_reply_for_rsvp(&rw, "carol@fauna.test", 1_700_000_000)
            .expect("a different organizer is notified");
        // Addressed TO the organizer, FROM the responder; a single-attendee REPLY.
        assert_eq!(reply.recipients, vec!["alice@fauna.test".to_string()]);
        assert_eq!(reply.from, "carol@fauna.test");
        let raw = String::from_utf8_lossy(&reply.raw_rfc5322);
        assert!(raw.contains("METHOD:REPLY"));
        assert!(raw.contains("PARTSTAT=ACCEPTED"));
        // The DTSTAMP is the canonical compact iCal UTC form (not RFC 3339).
        assert!(raw.contains("DTSTAMP:20231114T221320Z"));
    }

    #[test]
    fn imip_reply_for_rsvp_is_none_for_own_event() {
        // carol both organizes and "responds" — no self-reply (RFC 5546).
        let ics = generate_ical(&sample_fields(), &[], "carol@fauna.test");
        let rw = apply_rsvp(&ics, None, "carol@fauna.test", "going").expect("rsvp");
        assert!(imip_reply_for_rsvp(&rw, "carol@fauna.test", 1_700_000_000).is_none());
    }

    #[test]
    fn imip_reply_for_rsvp_is_none_without_organizer() {
        // A solo event with no ORGANIZER — apply_rsvp falls organizer back to
        // self, so there is no distinct party to notify.
        let ics = generate_ical(&sample_fields(), &[], "");
        let rw = apply_rsvp(&ics, None, "carol@fauna.test", "going").expect("rsvp");
        assert!(imip_reply_for_rsvp(&rw, "carol@fauna.test", 1_700_000_000).is_none());
    }

    #[test]
    fn imip_request_for_invite_targets_the_email_roster() {
        // alice organizes; she invites bob by email (add_attendee → rewrite).
        let ics = generate_ical(&sample_fields(), &[], "alice@fauna.test");
        let rw = add_attendee(&ics, None, "alice@fauna.test", "bob@fauna.test").expect("add");
        let req = imip_request_for_invite(
            &rw.fields,
            &rw.attendees,
            &rw.organizer_email,
            1_700_000_000,
        )
        .expect("a reachable attendee is invited");
        // Addressed TO the attendee, FROM the organizer; the organizer is excluded.
        assert_eq!(req.recipients, vec!["bob@fauna.test".to_string()]);
        assert_eq!(req.from, "alice@fauna.test");
        let raw = String::from_utf8_lossy(&req.raw_rfc5322);
        assert!(raw.contains("METHOD:REQUEST"));
        // The DTSTAMP is the canonical compact iCal UTC form (not RFC 3339).
        assert!(raw.contains("DTSTAMP:20231114T221320Z"));
    }

    #[test]
    fn imip_request_for_invite_is_none_without_a_reachable_roster() {
        // A solo event with no attendees — nobody to REQUEST.
        let ics = generate_ical(&sample_fields(), &[], "alice@fauna.test");
        let (fields, roster, organizer) = imip_inputs(&ics, "alice@fauna.test").expect("inputs");
        assert!(imip_request_for_invite(&fields, &roster, &organizer, 1_700_000_000).is_none());
    }

    #[test]
    fn rsvp_interested_sets_tentative_partstat_and_sidecar() {
        let ics = generate_ical(&sample_fields(), &[], "alice@fauna.test");
        let rw = apply_rsvp(&ics, None, "carol@fauna.test", "interested").expect("rsvp");
        // Appended to the roster (was empty) with TENTATIVE wire PARTSTAT.
        let carol = rw
            .attendees
            .iter()
            .find(|a| a.email == "carol@fauna.test")
            .expect("carol present");
        assert_eq!(carol.partstat, "TENTATIVE");
        // The sidecar marks her Interested (the asymmetric refinement).
        let ext = rw.fauna_ext.expect("sidecar set");
        assert!(ext.is_interested("carol@fauna.test"));
        // Organizer preserved from the original VEVENT.
        assert_eq!(rw.organizer_email, "alice@fauna.test");
    }

    #[test]
    fn rsvp_going_clears_prior_interested_marker() {
        let ext = FaunaEventExt {
            interested_attendees: vec!["carol@fauna.test".to_string()],
            ..Default::default()
        };
        let ics = generate_ical(&sample_fields(), &[], "alice@fauna.test");
        let rw = apply_rsvp(&ics, Some(&ext), "carol@fauna.test", "going").expect("rsvp");
        let carol = rw
            .attendees
            .iter()
            .find(|a| a.email == "carol@fauna.test")
            .expect("carol present");
        assert_eq!(carol.partstat, "ACCEPTED");
        // No interested markers remain ⇒ sidecar collapses to None.
        assert!(rw.fauna_ext.is_none());
    }

    #[test]
    fn set_reminder_writes_alarm_offset_and_preserves_roster() {
        let attendees = vec![AttendeeInfo {
            name: String::new(),
            email: "bob@fauna.test".to_string(),
            partstat: "ACCEPTED".to_string(),
            fauna_status: "going".to_string(),
        }];
        let ics = generate_ical(&sample_fields(), &attendees, "alice@fauna.test");
        let rw = set_reminder(&ics, None, "-PT1H").expect("reminder");
        assert_eq!(rw.fields.alarm, "-PT1H");
        assert_eq!(
            rw.attendees.len(),
            1,
            "roster preserved across reminder edit"
        );
        assert_eq!(rw.organizer_email, "alice@fauna.test");
    }

    #[test]
    fn add_attendee_appends_mailto_roster_and_sets_organizer() {
        // A Fauna-created event starts with an empty roster + no ORGANIZER.
        let ics = generate_ical(&sample_fields(), &[], "");
        let rw = add_attendee(&ics, None, "alice@fauna.test", "guest@example.com").expect("add");
        // The new attendee is on the roster, NEEDS-ACTION (uninvited yet).
        let guest = rw
            .attendees
            .iter()
            .find(|a| a.email == "guest@example.com")
            .expect("guest appended");
        assert_eq!(guest.partstat, "NEEDS-ACTION");
        assert_eq!(guest.fauna_status, "invited");
        // The adder becomes the organizer (an event with a roster needs one).
        assert_eq!(rw.organizer_email, "alice@fauna.test");
        // SEQUENCE bumped from 0 so MUAs treat it as an update.
        assert_eq!(rw.fields.sequence, 1);
        // The appended ATTENDEE survives a writer round-trip as a mailto: line.
        let ics = generate_ical(&rw.fields, &rw.attendees, &rw.organizer_email);
        assert!(ics.contains("mailto:guest@example.com"));
    }

    #[test]
    fn add_attendee_is_idempotent_and_preserves_existing_organizer() {
        let attendees = vec![AttendeeInfo {
            name: "Guest".to_string(),
            email: "guest@example.com".to_string(),
            partstat: "ACCEPTED".to_string(),
            fauna_status: "going".to_string(),
        }];
        let ics = generate_ical(&sample_fields(), &attendees, "alice@fauna.test");
        // Re-adding an attendee already on the roster (case-insensitively) is a
        // no-op: no duplicate line, no SEQUENCE bump, status untouched.
        let rw = add_attendee(&ics, None, "bob@fauna.test", "Guest@Example.com").expect("add");
        assert_eq!(rw.attendees.len(), 1, "no duplicate roster line");
        assert_eq!(rw.attendees[0].partstat, "ACCEPTED", "existing RSVP intact");
        assert_eq!(rw.fields.sequence, 0, "no bump on a no-op add");
        // The existing VEVENT ORGANIZER wins over the adder.
        assert_eq!(rw.organizer_email, "alice@fauna.test");
    }

    #[test]
    fn add_attendee_rejects_empty_email() {
        let ics = generate_ical(&sample_fields(), &[], "alice@fauna.test");
        assert!(add_attendee(&ics, None, "alice@fauna.test", "   ").is_err());
    }
}
