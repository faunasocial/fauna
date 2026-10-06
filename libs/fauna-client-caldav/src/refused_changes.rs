//! How a **refused scheduling change** reads on the Events page — the one
//! projection all 7 apps render (`caldav-server.md` § Who may mutate an
//! existing event over the inbound rail → *Surfacing*; the page's presentation
//! is `ui/events.md` § Refused scheduling changes).
//!
//! The at-rest row ([`fauna_core::data::RefusedSchedulingChange`]) is a
//! security record: hex ids, a wire method, a reason token. Turning that into
//! the four things a person reads — *what was tried*, *who tried it*, *why it
//! was refused*, *how often* — is presentation, and it lives here rather than
//! in seven renderers for the ordinary reason (priority #2): the phrasing of a
//! security notice is exactly the thing that must not drift between apps.
//!
//! Every line is a [`LocalizedText`] key plus arguments, never a finished
//! English string, so each app routes it through its own pipeline
//! (`fauna_core::localized`).
//!
//! # What this deliberately does NOT do
//!
//! It does not resolve the author's handle. That answer is **device-local**
//! knowledge — the threads this device happens to hold — and it belongs to
//! whichever surface has the conversations manager in hand
//! (`ConversationsManager::handle_for_person`, which re-derives rather than
//! caching, for the reason its own docs give: a handle cached at raise time
//! goes stale exactly when it matters). So the caller passes in what it could
//! resolve, and this owns the fallback: a short actor id, or — when the nest
//! attested no author at all — *the sender could not be identified*.
//!
//! A refused **mailed** `REPLY` carries no actor at all: its sender is the
//! address the delivery door authenticated
//! ([`RefusedSchedulingChange::sender_address`]), which names the row as it
//! stands — never a handle this device resolved, and never the message's own
//! `From:` (`caldav-server.md` § Who may mutate an existing event over the
//! inbound rail → *The mail rail*).

use fauna_core::data::RefusedSchedulingChange;
use fauna_core::localized::LocalizedText;

/// One refused-change row as a surface renders it: the `refused-change-item`
/// children, composed once for every app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedChangeView {
    /// The row's identity — what a `refused-change-dismiss` gesture hands back
    /// ([`RefusedSchedulingChange::key`]).
    pub key: String,
    /// `refused-change-title` — what was tried, and to which event.
    pub headline: LocalizedText,
    /// `refused-change-author` — who tried it.
    pub author: LocalizedText,
    /// `refused-change-reason` — why the client refused it.
    pub reason: LocalizedText,
    /// Shown beside the row only when there has been more than one attempt;
    /// one attempt needs no count.
    pub attempts: Option<LocalizedText>,
    /// `refused-change-time` — epoch seconds of the most recent attempt. Left
    /// as an instant: formatting a date is the app's own locale business.
    pub last_refused_at: i64,
}

/// The first 8 hex characters of an actor id, with an ellipsis — the fallback
/// when this device knows the sender by no other name.
///
/// Short rather than full because the row is prose the user reads, and a
/// 64-character id in the middle of a sentence is not a name. The full id
/// stays in the record for anyone who needs to compare it exactly.
fn short_actor(actor: &str) -> String {
    let head: String = actor.chars().take(8).collect();
    format!("{head}…")
}

/// The foreign nest that attested a row's author — absent (`None` from
/// [`attesting_nest`]) for the recipient's own nest, whose stamp is the row's
/// blank [`RefusedSchedulingChange::author_home_nest_url`].
enum AttestingNest {
    /// The host of the nest's base URL, lowercased.
    Host(String),
    /// A non-blank URL with no host this build can read. Still foreign, so it
    /// still lends no handle; the row names the nest by the URL itself.
    Unreadable(String),
}

impl AttestingNest {
    /// What the row prints for the nest.
    fn display(&self) -> String {
        match self {
            Self::Host(host) => host.clone(),
            Self::Unreadable(url) => url.clone(),
        }
    }
}

fn attesting_nest(row: &RefusedSchedulingChange) -> Option<AttestingNest> {
    let url = row.author_home_nest_url.trim();
    if url.is_empty() {
        return None;
    }
    Some(match fauna_core::data::url_host(url) {
        Some(host) => AttestingNest::Host(host),
        None => AttestingNest::Unreadable(url.to_string()),
    })
}

/// Whether the nest that attested a row's author may lend it `handle` — the
/// name THIS device knows that actor id by.
///
/// **The principal is the pair** (`caldav-server.md` § Who may mutate an
/// existing event over the inbound rail): a hostile nest can attest any actor
/// id, but only for channels homed on itself. So a device-known handle names a
/// foreign attestation only when that handle lives on the attesting nest's own
/// domain; anything else would let a stranger's nest borrow the name of a
/// colleague this user really talks to.
///
/// ⚠ **An approximation of the rule's equality, and fail-closed on purpose.**
/// The device holds no home nest per person — only the handle, whose domain is
/// the nest half it knows — and the row holds the attesting nest's base URL.
/// So this compares the handle's domain with that URL's host: a nest served on
/// a different host than its handle domain (`nest.example.com` for
/// `@example.com`) names even a genuine sender by short id and nest, never the
/// reverse. The recipient's own nest (blank URL) lends any handle — it is the
/// one nest this user trusts to attest.
fn nest_vouches_for_handle(nest: Option<&AttestingNest>, handle: &str) -> bool {
    match nest {
        None => true,
        Some(AttestingNest::Unreadable(_)) => false,
        Some(AttestingNest::Host(host)) => handle
            .rsplit_once('@')
            .is_some_and(|(_, domain)| domain.trim().eq_ignore_ascii_case(host)),
    }
}

/// Compose the row a surface renders.
///
/// `handle` is what the caller's device could resolve for
/// [`RefusedSchedulingChange::author`] — `None` when it knows the sender by no
/// name, which is the *ordinary* case here: someone who may not change your
/// calendar is frequently someone you have never messaged. It is a
/// **candidate**, not a verdict: this decides whether the nest that attested
/// the author may lend it ([`nest_vouches_for_handle`]), and a foreign nest's
/// attestation that lends none names that nest — so no app composes the pair.
#[must_use]
pub fn refused_change_view(
    row: &RefusedSchedulingChange,
    handle: Option<&str>,
) -> RefusedChangeView {
    // An untitled event still gets a sentence: the user is being told someone
    // tried to change their calendar, and "" in the middle of it would read
    // like a bug rather than a missing title.
    let title = if row.summary.trim().is_empty() {
        None
    } else {
        Some(row.summary.clone())
    };
    let headline = match (row.method.as_str(), title) {
        ("CANCEL", Some(t)) => {
            LocalizedText::key_arg("events.refused_changes.cancel_attempt", "title", t)
        }
        ("REQUEST", Some(t)) => {
            LocalizedText::key_arg("events.refused_changes.update_attempt", "title", t)
        }
        ("REPLY", Some(t)) => {
            LocalizedText::key_arg("events.refused_changes.reply_attempt", "title", t)
        }
        // An unknown METHOD is still reported — a row this build cannot phrase
        // precisely is never a row it hides (the fail-visible rule every
        // adjudication surface here follows).
        _ => LocalizedText::key("events.refused_changes.other_attempt"),
    };

    let attesting_nest = attesting_nest(row);
    let handle = handle
        .filter(|h| !h.trim().is_empty())
        .filter(|h| nest_vouches_for_handle(attesting_nest.as_ref(), h));
    let sender_address = row.sender_address.trim();
    let author = match (handle, row.author.as_deref(), attesting_nest) {
        // A mail-rail row: no actor, so no handle can be about it — the
        // door-authenticated address is the whole of who sent it.
        (_, None, _) if !sender_address.is_empty() => {
            LocalizedText::key_arg("events.refused_changes.sender", "who", sender_address)
        }
        (Some(h), _, _) => LocalizedText::key_arg("events.refused_changes.sender", "who", h),
        (None, Some(actor), None) if !actor.trim().is_empty() => {
            LocalizedText::key_arg("events.refused_changes.sender", "who", short_actor(actor))
        }
        (None, Some(actor), Some(nest)) if !actor.trim().is_empty() => LocalizedText::key_args(
            "events.refused_changes.sender_via_nest",
            [("who", short_actor(actor)), ("nest", nest.display())],
        ),
        // No attested author at all: the home nest named nobody, which is
        // itself the reason the message was refused.
        _ => LocalizedText::key("events.refused_changes.unknown_sender"),
    };

    let reason = LocalizedText::key(match crate::RefusalReason::from_wire(&row.reason) {
        Some(crate::RefusalReason::NotTheOrganizer) => {
            "events.refused_changes.reason.not_the_organizer"
        }
        Some(crate::RefusalReason::OrganizerChanged) => {
            "events.refused_changes.reason.organizer_changed"
        }
        Some(crate::RefusalReason::OrganizerUnresolvable) => {
            "events.refused_changes.reason.organizer_unresolvable"
        }
        Some(crate::RefusalReason::NoAttestedAuthor) => {
            "events.refused_changes.reason.no_attested_author"
        }
        Some(crate::RefusalReason::SpoofedOrganizer) => {
            "events.refused_changes.reason.spoofed_organizer"
        }
        Some(crate::RefusalReason::NotTheAttendee) => {
            "events.refused_changes.reason.not_the_attendee"
        }
        Some(crate::RefusalReason::AttendeeUnresolvable) => {
            "events.refused_changes.reason.attendee_unresolvable"
        }
        Some(crate::RefusalReason::SenderUnauthenticated) => {
            "events.refused_changes.reason.sender_unauthenticated"
        }
        // A reason a newer build wrote. The row still renders, saying only
        // that the change was refused — never hidden for want of a phrase.
        None => "events.refused_changes.reason.other",
    });

    RefusedChangeView {
        key: row.key(),
        headline,
        author,
        reason,
        attempts: (row.occurrences > 1).then(|| {
            LocalizedText::key_arg(
                "events.refused_changes.attempts",
                "count",
                row.occurrences.to_string(),
            )
        }),
        last_refused_at: row.last_refused_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn row(method: &str, reason: &str) -> RefusedSchedulingChange {
        RefusedSchedulingChange {
            uid_hash: "ab".repeat(32),
            author: Some("ba5eba11".to_string() + &"0".repeat(56)),
            author_home_nest_url: String::new(),
            sender_address: String::new(),
            method: method.into(),
            reason: reason.into(),
            summary: "Kickoff".into(),
            first_refused_at: 100,
            last_refused_at: 200,
            occurrences: 1,
            dismissed_through: 0,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn a_resolved_handle_names_the_sender_and_an_unresolved_one_is_elided() {
        let r = row("CANCEL", "not_the_organizer");

        let known = refused_change_view(&r, Some("mallory@fauna.test"));
        assert_eq!(known.author.key, "events.refused_changes.sender");
        assert_eq!(
            known.author.args.get("who").map(String::as_str),
            Some("mallory@fauna.test")
        );

        let unknown = refused_change_view(&r, None);
        assert_eq!(
            unknown.author.args.get("who").map(String::as_str),
            Some("ba5eba11…"),
            "an unresolved sender is a SHORT id — the row is prose, not a hex dump"
        );

        let nameless = refused_change_view(
            &RefusedSchedulingChange {
                author: None,
                ..r.clone()
            },
            None,
        );
        assert_eq!(
            nameless.author.key, "events.refused_changes.unknown_sender",
            "the nest attested nobody, which is itself why this was refused"
        );
    }

    /// A FOREIGN nest's attestation never borrows a name this device knows.
    ///
    /// A hostile nest can attest any actor id for a channel homed on itself —
    /// including the id of a colleague this user really talks to — so a row
    /// whose author a foreign nest vouched for names that colleague only when
    /// the colleague's handle is on that nest's own domain. Otherwise it reads
    /// as that nest's claim: a short id and the nest that made it.
    #[test]
    fn a_known_handle_names_a_foreign_attestation_only_when_that_nest_hosts_it() {
        let foreign = RefusedSchedulingChange {
            author_home_nest_url: "https://Evil.Example:8443/".into(),
            ..row("REQUEST", "spoofed_organizer")
        };

        let impostor = refused_change_view(&foreign, Some("alice@fauna.test"));
        assert_eq!(
            impostor.author.key, "events.refused_changes.sender_via_nest",
            "the colleague's handle must not name what evil.example attested"
        );
        assert_eq!(
            impostor.author.args.get("who").map(String::as_str),
            Some("ba5eba11…")
        );
        assert_eq!(
            impostor.author.args.get("nest").map(String::as_str),
            Some("evil.example"),
            "the claim reads as the attesting nest's"
        );

        let unknown = refused_change_view(&foreign, None);
        assert_eq!(unknown.author.key, "events.refused_changes.sender_via_nest");
        assert_eq!(
            unknown.author.args.get("nest").map(String::as_str),
            Some("evil.example")
        );

        // The nest vouching for a handle on its own domain is the ordinary
        // cross-nest case, and it names the person.
        let homed = refused_change_view(&foreign, Some("Mallory@EVIL.example"));
        assert_eq!(homed.author.key, "events.refused_changes.sender");
        assert_eq!(
            homed.author.args.get("who").map(String::as_str),
            Some("Mallory@EVIL.example")
        );

        // A bare handle carries no domain to compare, so it cannot be vouched
        // for by a foreign nest.
        let bare = refused_change_view(&foreign, Some("alice"));
        assert_eq!(bare.author.key, "events.refused_changes.sender_via_nest");
    }

    /// A refused mailed `REPLY` has no actor; the address the delivery door
    /// authenticated names it — and no device handle can stand in for it. An
    /// unstamped one names nobody, and says why.
    #[test]
    fn a_mail_rail_row_names_its_authenticated_sender_address() {
        let mailed = RefusedSchedulingChange {
            author: None,
            sender_address: "mallory@fauna.test".into(),
            ..row("REPLY", "not_the_attendee")
        };
        for handle in [None, Some("alice@fauna.test")] {
            let view = refused_change_view(&mailed, handle);
            assert_eq!(view.author.key, "events.refused_changes.sender");
            assert_eq!(
                view.author.args.get("who").map(String::as_str),
                Some("mallory@fauna.test")
            );
            assert_eq!(
                view.reason.key,
                "events.refused_changes.reason.not_the_attendee"
            );
        }

        let unstamped = refused_change_view(
            &RefusedSchedulingChange {
                author: None,
                sender_address: String::new(),
                ..row("REPLY", "sender_unauthenticated")
            },
            None,
        );
        assert_eq!(
            unstamped.author.key,
            "events.refused_changes.unknown_sender"
        );
        assert_eq!(
            unstamped.reason.key,
            "events.refused_changes.reason.sender_unauthenticated"
        );
    }

    /// A method or reason this build does not name still RENDERS. Hiding a row
    /// for want of a phrase is the one failure this surface cannot have.
    #[test]
    fn an_unknown_method_or_reason_still_renders_the_row() {
        let view = refused_change_view(&row("COUNTER", "some_future_reason"), None);
        assert_eq!(view.headline.key, "events.refused_changes.other_attempt");
        assert_eq!(view.reason.key, "events.refused_changes.reason.other");
    }

    #[test]
    fn the_headline_follows_the_method_and_an_untitled_event_still_reads() {
        assert_eq!(
            refused_change_view(&row("CANCEL", "not_the_organizer"), None)
                .headline
                .key,
            "events.refused_changes.cancel_attempt"
        );
        assert_eq!(
            refused_change_view(&row("REQUEST", "not_the_organizer"), None)
                .headline
                .key,
            "events.refused_changes.update_attempt"
        );
        assert_eq!(
            refused_change_view(&row("REPLY", "not_the_attendee"), None)
                .headline
                .key,
            "events.refused_changes.reply_attempt"
        );

        let untitled = refused_change_view(
            &RefusedSchedulingChange {
                summary: "  ".into(),
                ..row("CANCEL", "not_the_organizer")
            },
            None,
        );
        assert_eq!(
            untitled.headline.key, "events.refused_changes.other_attempt",
            "no title ⇒ the sentence names the calendar, not an empty string"
        );
    }

    #[test]
    fn a_count_appears_only_once_there_is_more_than_one_attempt() {
        assert!(
            refused_change_view(&row("CANCEL", "not_the_organizer"), None)
                .attempts
                .is_none()
        );
        let repeated = refused_change_view(
            &RefusedSchedulingChange {
                occurrences: 47,
                ..row("CANCEL", "not_the_organizer")
            },
            None,
        );
        assert_eq!(
            repeated
                .attempts
                .expect("a repeated attempt is counted")
                .args
                .get("count")
                .map(String::as_str),
            Some("47")
        );
    }
}
