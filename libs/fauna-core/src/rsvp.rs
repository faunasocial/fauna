//! RSVP vocabulary — the one closed set every app's attendee status comes from.
//!
//! Two types, because the vocabulary genuinely has two halves and conflating
//! them is what let a typo become a silent `NEEDS-ACTION`:
//!
//! - [`RsvpResponse`] — what a **user can pick**: going / interested / declined
//!   (`caldav-server.md` § Scheduling & invitations → RSVP semantics: "Fauna's
//!   RSVP is going / interested / decline (+ standard tentative / needs-action
//!   **inbound**)"). This is the parameter type of every `rsvp_event` call.
//! - [`RsvpState`] — what an attendee **can be**, which is strictly wider: the
//!   three above plus the three nobody submits — `Tentative` (a stock CalDAV
//!   client's own choice, arriving over the wire), `Invited` (the initial
//!   roster state, `PARTSTAT=NEEDS-ACTION`) and `Waitlisted` (Fauna-native,
//!   carried as `X-FAUNA-STATUS=waitlisted`). This is the type of a rendered
//!   attendee's status.
//!
//! Making the submission set a *different, smaller* type is the point: before
//! this, four call surfaces documented `tentative` as an accepted RSVP value
//! (`fauna-ffi`, `fauna-wasm`, and two windows sites) when
//! [`crate::ical::partstat_from_fauna`] had no `tentative` arm at all — a
//! submitted `"tentative"` fell through to `NEEDS-ACTION`, quietly turning a
//! user's answer into "hasn't answered". Now it is not expressible.
//!
//! **`Interested` is Fauna-native and the projection is lossy and asymmetric**
//! (`ui/events.md` § Snapshot; `caldav-server.md` § RSVP semantics): it has no
//! standard `PARTSTAT`, so it projects *out* as `TENTATIVE` and can only be
//! recovered *in* from the sealed sidecar. That is why two different inbound
//! mappings exist here and both are correct:
//!
//! - [`RsvpState::from_partstat_verbatim`] — the **render** rule. A bare
//!   `TENTATIVE` is [`RsvpState::Tentative`], never `Interested`; the
//!   `Interested` refinement comes only from the sidecar
//!   (`fauna_client_caldav::project_attendee_rsvp` applies it on top).
//! - [`RsvpState::from_partstat_lossy`] — the **write round-trip** inverse,
//!   where `TENTATIVE` folds back to `Interested` because that is the value
//!   this app wrote out.
//!
//! Reading one where the other belongs is exactly the confusion the two names
//! exist to prevent; `fauna_status_from_partstat` and `rsvp_status_verbatim`
//! are the historical `&str` spellings of this pair and now delegate here, so
//! the tables live in one place.

use serde::{Deserialize, Serialize};

/// An attendee's RSVP status — the full closed set, including the three states
/// no user submits (see the module docs).
///
/// The `snake_case` serde repr is the Fauna wire/status string
/// (`"going"` / `"interested"` / `"tentative"` / `"declined"` / `"invited"` /
/// `"waitlisted"`), which is what `AttendeeInfo::fauna_status` holds and what
/// every app compares against today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RsvpState {
    /// `PARTSTAT=ACCEPTED`.
    Going,
    /// Fauna-native "soft yes". Projects to `PARTSTAT=TENTATIVE`; recoverable
    /// only from the sidecar.
    Interested,
    /// A stock client's own `PARTSTAT=TENTATIVE`, with no sidecar marker.
    Tentative,
    /// `PARTSTAT=DECLINED`.
    Declined,
    /// The initial roster state — `PARTSTAT=NEEDS-ACTION`, nobody has answered.
    ///
    /// Named for the Fauna status string every app already compares against
    /// (`"invited"`), not for the iCalendar spelling of the `PARTSTAT` it
    /// projects to. A variant named `NeedsAction` whose `as_str` were
    /// `"invited"` would be a permanent trap for the next reader.
    Invited,
    /// Fauna-native, carried as the `X-FAUNA-STATUS=waitlisted` attendee param
    /// beside `PARTSTAT=NEEDS-ACTION` (`crate::ical` emits and parses it).
    Waitlisted,
}

impl RsvpState {
    /// The Fauna status string — the `snake_case` serde repr.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RsvpState::Going => "going",
            RsvpState::Interested => "interested",
            RsvpState::Tentative => "tentative",
            RsvpState::Declined => "declined",
            RsvpState::Invited => "invited",
            RsvpState::Waitlisted => "waitlisted",
        }
    }

    /// Parse a Fauna status string. Strict — no trimming, no case folding,
    /// mirroring [`crate::nat_mode::NodeMode::from_wire_str`].
    ///
    /// `None` is a real answer: an unrecognized status came from a peer that
    /// knows a word this build does not, and the caller decides whether to fall
    /// back or reject. It is deliberately not folded into [`Self::Invited`]
    /// here — a silent fold is the failure this module exists to end.
    #[must_use]
    pub fn from_fauna_str(s: &str) -> Option<RsvpState> {
        match s {
            "going" => Some(RsvpState::Going),
            "interested" => Some(RsvpState::Interested),
            "tentative" => Some(RsvpState::Tentative),
            "declined" => Some(RsvpState::Declined),
            "invited" => Some(RsvpState::Invited),
            "waitlisted" => Some(RsvpState::Waitlisted),
            _ => None,
        }
    }

    /// The iCalendar `PARTSTAT` this state projects to.
    ///
    /// Lossy by construction on two arms: `Interested` and `Tentative` both
    /// emit `TENTATIVE` (the refinement rides in the sidecar), and `Invited`
    /// and `Waitlisted` both emit `NEEDS-ACTION` (the refinement rides in
    /// `X-FAUNA-STATUS`).
    #[must_use]
    pub fn partstat(self) -> &'static str {
        match self {
            RsvpState::Going => "ACCEPTED",
            RsvpState::Interested | RsvpState::Tentative => "TENTATIVE",
            RsvpState::Declined => "DECLINED",
            RsvpState::Invited | RsvpState::Waitlisted => "NEEDS-ACTION",
        }
    }

    /// The **render** inverse: a bare `TENTATIVE` is [`Self::Tentative`].
    ///
    /// Case-insensitive, and every unrecognized value answers [`Self::Invited`]
    /// — the honest reading of a `PARTSTAT` this build does not know is "this
    /// person has not answered in a way we understand", and the roster must
    /// still render. The `Interested` refinement is applied *on top* of this by
    /// `fauna_client_caldav::project_attendee_rsvp`, never here.
    #[must_use]
    pub fn from_partstat_verbatim(partstat: &str) -> RsvpState {
        match partstat.to_ascii_uppercase().as_str() {
            "ACCEPTED" => RsvpState::Going,
            "TENTATIVE" => RsvpState::Tentative,
            "DECLINED" => RsvpState::Declined,
            _ => RsvpState::Invited,
        }
    }

    /// The **write round-trip** inverse: `TENTATIVE` folds to [`Self::Interested`].
    ///
    /// Correct only where the value being read back is one this app just wrote
    /// (which is the sole context in which `TENTATIVE` is known to have meant
    /// Interested). For anything arriving from a foreign client, use
    /// [`Self::from_partstat_verbatim`].
    #[must_use]
    pub fn from_partstat_lossy(partstat: &str) -> RsvpState {
        match partstat.to_ascii_uppercase().as_str() {
            "ACCEPTED" => RsvpState::Going,
            "TENTATIVE" => RsvpState::Interested,
            "DECLINED" => RsvpState::Declined,
            _ => RsvpState::Invited,
        }
    }
}

impl core::fmt::Display for RsvpState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a user can actually answer — the submission subset of [`RsvpState`].
///
/// `Tentative`, `Invited` and `Waitlisted` are deliberately absent: they are
/// inbound/roster states, not answers a Fauna app offers
/// (`caldav-server.md` § RSVP semantics). Submitting one is now unrepresentable
/// rather than silently becoming `NEEDS-ACTION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RsvpResponse {
    Going,
    Interested,
    Declined,
}

impl RsvpResponse {
    /// The Fauna wire string this response is submitted as.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.state().as_str()
    }

    /// Parse a submitted RSVP value. Strict, and `None` is the whole point:
    /// this is the one gate that turns a stray literal — `"tentative"`,
    /// `"Going"`, `"decline"` — into a rejection the caller must handle,
    /// instead of a `NEEDS-ACTION` the user never chose.
    #[must_use]
    pub fn from_wire_str(s: &str) -> Option<RsvpResponse> {
        match s {
            "going" => Some(RsvpResponse::Going),
            "interested" => Some(RsvpResponse::Interested),
            "declined" => Some(RsvpResponse::Declined),
            _ => None,
        }
    }

    /// The attendee state this response puts the user in.
    #[must_use]
    pub fn state(self) -> RsvpState {
        match self {
            RsvpResponse::Going => RsvpState::Going,
            RsvpResponse::Interested => RsvpState::Interested,
            RsvpResponse::Declined => RsvpState::Declined,
        }
    }

    /// The `PARTSTAT` this response writes to the VEVENT.
    #[must_use]
    pub fn partstat(self) -> &'static str {
        self.state().partstat()
    }

    /// Whether this answer needs the sidecar's `interested` marker set — true
    /// for exactly one variant, which is what makes the projection asymmetric.
    #[must_use]
    pub fn marks_interested(self) -> bool {
        self == RsvpResponse::Interested
    }
}

impl core::fmt::Display for RsvpResponse {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<RsvpResponse> for RsvpState {
    fn from(r: RsvpResponse) -> RsvpState {
        r.state()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fauna_status_strings_round_trip() {
        for state in [
            RsvpState::Going,
            RsvpState::Interested,
            RsvpState::Tentative,
            RsvpState::Declined,
            RsvpState::Invited,
            RsvpState::Waitlisted,
        ] {
            assert_eq!(RsvpState::from_fauna_str(state.as_str()), Some(state));
        }
    }

    /// Strict, and an unknown word is `None` rather than a fold — the silent
    /// fold is the failure this module ends.
    #[test]
    fn unknown_status_strings_are_refused_not_folded() {
        assert_eq!(RsvpState::from_fauna_str("Going"), None);
        assert_eq!(RsvpState::from_fauna_str(" going"), None);
        assert_eq!(RsvpState::from_fauna_str("maybe"), None);
        assert_eq!(RsvpState::from_fauna_str(""), None);
    }

    /// The submission subset is genuinely smaller — and `tentative`, which four
    /// call surfaces used to document as accepted, is refused here rather than
    /// becoming `NEEDS-ACTION`.
    #[test]
    fn only_three_answers_are_submittable() {
        assert_eq!(
            RsvpResponse::from_wire_str("going"),
            Some(RsvpResponse::Going)
        );
        assert_eq!(
            RsvpResponse::from_wire_str("interested"),
            Some(RsvpResponse::Interested)
        );
        assert_eq!(
            RsvpResponse::from_wire_str("declined"),
            Some(RsvpResponse::Declined)
        );
        assert_eq!(
            RsvpResponse::from_wire_str("tentative"),
            None,
            "tentative is an INBOUND state, never an answer a Fauna app offers"
        );
        assert_eq!(RsvpResponse::from_wire_str("invited"), None);
        assert_eq!(RsvpResponse::from_wire_str("waitlisted"), None);
        assert_eq!(
            RsvpResponse::from_wire_str("decline"),
            None,
            "the near-miss windows' NormalizeRsvp used to paper over"
        );
    }

    /// Every response is a state, and its string survives the widening.
    #[test]
    fn a_response_widens_to_the_state_it_names() {
        for r in [
            RsvpResponse::Going,
            RsvpResponse::Interested,
            RsvpResponse::Declined,
        ] {
            assert_eq!(RsvpState::from(r).as_str(), r.as_str());
            assert_eq!(r.partstat(), r.state().partstat());
        }
        assert!(RsvpResponse::Interested.marks_interested());
        assert!(!RsvpResponse::Going.marks_interested());
        assert!(!RsvpResponse::Declined.marks_interested());
    }

    /// The lossy projection out, arm by arm — `Interested` and `Tentative`
    /// share `TENTATIVE`, `Invited` and `Waitlisted` share `NEEDS-ACTION`.
    #[test]
    fn partstat_projection_is_lossy_on_exactly_two_pairs() {
        assert_eq!(RsvpState::Going.partstat(), "ACCEPTED");
        assert_eq!(RsvpState::Interested.partstat(), "TENTATIVE");
        assert_eq!(RsvpState::Tentative.partstat(), "TENTATIVE");
        assert_eq!(RsvpState::Declined.partstat(), "DECLINED");
        assert_eq!(RsvpState::Invited.partstat(), "NEEDS-ACTION");
        assert_eq!(RsvpState::Waitlisted.partstat(), "NEEDS-ACTION");
    }

    /// The two inbound mappings differ on exactly one arm, and that difference
    /// IS the asymmetric rule (`caldav-server.md` § RSVP semantics): a stock
    /// client's Tentative renders as Tentative, while a value this app wrote
    /// reads back as the Interested it meant.
    #[test]
    fn the_two_inbound_mappings_differ_only_on_tentative() {
        for p in ["ACCEPTED", "DECLINED", "NEEDS-ACTION", "X-WEIRD", ""] {
            assert_eq!(
                RsvpState::from_partstat_verbatim(p),
                RsvpState::from_partstat_lossy(p),
                "only TENTATIVE may differ between the render and round-trip inverses"
            );
        }
        assert_eq!(
            RsvpState::from_partstat_verbatim("TENTATIVE"),
            RsvpState::Tentative
        );
        assert_eq!(
            RsvpState::from_partstat_lossy("TENTATIVE"),
            RsvpState::Interested
        );
    }

    /// Case folding on the way in (a foreign client may send `Accepted`), and
    /// an unknown `PARTSTAT` renders as Invited rather than failing the roster.
    #[test]
    fn partstat_parsing_folds_case_and_survives_unknown_values() {
        assert_eq!(
            RsvpState::from_partstat_verbatim("accepted"),
            RsvpState::Going
        );
        assert_eq!(
            RsvpState::from_partstat_verbatim("Tentative"),
            RsvpState::Tentative
        );
        assert_eq!(
            RsvpState::from_partstat_verbatim("DELEGATED"),
            RsvpState::Invited
        );
        assert_eq!(RsvpState::from_partstat_verbatim(""), RsvpState::Invited);
    }

    #[test]
    fn serde_repr_is_the_fauna_status_string() {
        assert_eq!(
            serde_json::to_string(&RsvpState::Waitlisted).unwrap(),
            "\"waitlisted\""
        );
        assert_eq!(
            serde_json::from_str::<RsvpState>("\"tentative\"").unwrap(),
            RsvpState::Tentative
        );
        assert_eq!(
            serde_json::to_string(&RsvpResponse::Interested).unwrap(),
            "\"interested\""
        );
    }
}
