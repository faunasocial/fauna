//! The calendar read's four answers and the one predicate that gates every
//! door to an event compose — `docs/goal/ui/events.md` § Errors & edge cases →
//! *No calendar to hold an event*, rulings 2 and 3, the page's instance of
//! `docs/goal/ui/README.md` § Offer only what is possible (rule 6: possibility
//! is decided once, in shared Rust, and the same answer guards the action).
//!
//! A custody read that finds no key is no answer of its own since the key
//! mint at first connect (`docs/goal/behavior/mail-credentials.md` § MSEK
//! lifecycle → *Every account holds the key*, ruling 7; events.md ruling 5):
//! it is the mint in flight, so the read answers *unresolved* for it, the
//! predicate says "loading", and the store-change notice that re-runs an
//! unresolved read picks the key up when it lands.
//!
//! An empty calendar list is none of the answers. Before this read, the load
//! folded "custody not readable yet", "no key" and "the lazy `Personal` was
//! refused" into one empty list, which every app painted as "No calendars
//! yet." while its compose still opened and its save sent an empty calendar id.
//!
//! The read keeps the calendars for a Rust caller ([`CalendarsRead`]); its
//! payload-free [`CalendarsReadState`] is what the predicate decides on and
//! what crosses the UniFFI and wasm faces (the calendars themselves cross as
//! each face's own decrypted rows). The other calendar-write doors (a new
//! calendar, the import) ask [`calendar_write_possibility`] over this same
//! read (events.md § Errors & edge cases → *The other calendar writes*).

use fauna_client_config::DavCustody;
use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;
use fauna_protocol::RpcRequester;
use fauna_protocol::bridge_routing::CalendarEntry;
use serde::{Deserialize, Serialize};

use crate::{CalDavClient, CalendarsWithPersonal};

/// The i18n keys of the reasons [`event_create_possibility`] answers
/// (`i18n/strings/en.yaml` → `events.create_blocked`).
pub mod reason_keys {
    /// The read has not resolved yet.
    pub const LOADING: &str = "events.create_blocked.loading";
    /// The lazy `Personal` was refused; `{error}` carries why.
    pub const PROVISION_FAILED: &str = "events.create_blocked.provision_failed";
}

/// What the calendar read answered: ruling 2's answers, with *no key* folded
/// into *unresolved* (ruling 5, the key mint).
#[derive(Debug, Clone, PartialEq)]
pub enum CalendarsRead {
    /// The read has not finished, the custody read itself failed because the
    /// account store is not up yet, or custody holds no key yet (the mint at
    /// first connect is in flight). A wait: re-run the read when the store
    /// changes. Never painted as an empty list.
    Unresolved,
    /// The key is there, the list was empty and the lazy `Personal` was
    /// refused. The error is carried, not logged; the next read retries.
    ProvisionFailed { error: String },
    /// At least one calendar (never empty).
    Loaded { calendars: Vec<CalendarEntry> },
}

/// [`CalendarsRead`] without its calendars: what [`event_create_possibility`]
/// decides on, and the shape the faces hand across. The serde form is
/// `{"state": "unresolved" | "loaded"}` or `{"state":
/// "provision_failed", "error": "…"}`, the shape the web SPA reads and hands
/// back (it is computed in this process and never decoded off a wire).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CalendarsReadState {
    /// Also the state before any read has run: a page that has not read yet
    /// is loading, never empty.
    #[default]
    Unresolved,
    ProvisionFailed {
        error: String,
    },
    Loaded,
}

impl CalendarsRead {
    /// The answer for a finished listing: [`CalendarsRead::Loaded`] when it
    /// holds a calendar, else the provision's refusal. A listing that is empty
    /// although the provision reported success is a refusal too — a working
    /// account never rests on an empty list, so an empty one is never
    /// "loaded".
    pub fn from_listing(listing: CalendarsWithPersonal) -> Self {
        let CalendarsWithPersonal {
            calendars,
            personal_provision_error,
        } = listing;
        if !calendars.is_empty() {
            return CalendarsRead::Loaded { calendars };
        }
        CalendarsRead::ProvisionFailed {
            error: personal_provision_error
                .unwrap_or_else(|| "the Personal calendar was not listed".to_string()),
        }
    }

    /// The payload-free answer.
    pub fn state(&self) -> CalendarsReadState {
        match self {
            CalendarsRead::Unresolved => CalendarsReadState::Unresolved,
            CalendarsRead::ProvisionFailed { error } => CalendarsReadState::ProvisionFailed {
                error: error.clone(),
            },
            CalendarsRead::Loaded { .. } => CalendarsReadState::Loaded,
        }
    }

    /// The calendars when loaded, else none.
    pub fn calendars(&self) -> &[CalendarEntry] {
        match self {
            CalendarsRead::Loaded { calendars } => calendars,
            _ => &[],
        }
    }
}

impl<R: RpcRequester> CalDavClient<R> {
    /// The Events page's calendar load: the custody answer
    /// ([`fauna_client_config::dav_custody`]) decides *unresolved* without a
    /// call — an unreadable custody and one holding no key yet alike (the
    /// mint is in flight; ruling 5); with the key,
    /// [`Self::list_calendars_with_personal`] runs (the lazy `Personal`
    /// included) and its outcome is *loaded* or *provision failed*. A list
    /// error is the call's error, as before.
    pub async fn read_calendars(&self, custody: &DavCustody) -> Result<CalendarsRead, R::Error> {
        match custody {
            DavCustody::Unreadable(_) | DavCustody::NoKey => Ok(CalendarsRead::Unresolved),
            DavCustody::Ready(ctx) => Ok(CalendarsRead::from_listing(
                self.list_calendars_with_personal(&ctx.actor_id, &ctx.msek)
                    .await?,
            )),
        }
    }
}

/// Can this person open an event compose (and send a create) now? Ruling 3,
/// as ruling 5 amends it:
///
/// - *possible* only when the read is **loaded**;
/// - *blocked* "loading" while **unresolved** (a missing key included: the
///   mint is in flight, never a step for the person);
/// - *blocked* with the provisioning error on **provision failed** (the next
///   read retries the provision).
///
/// It never answers *absent*: every account holds the key, so there is no
/// person to whom the compose is not offered. Every door to the compose asks
/// it, and the submit asks it again ([`event_create_target`]), so what the
/// page offers and what the action accepts cannot disagree.
pub fn event_create_possibility(read: &CalendarsReadState) -> Possibility {
    match read {
        CalendarsReadState::Loaded => Possibility::Possible,
        CalendarsReadState::Unresolved => {
            Possibility::blocked(LocalizedText::key(reason_keys::LOADING))
        }
        CalendarsReadState::ProvisionFailed { error } => Possibility::blocked(
            LocalizedText::key_arg(reason_keys::PROVISION_FAILED, "error", error.clone()),
        ),
    }
}

/// Can this person create a calendar or import an `.ics` now? The other
/// calendar-write doors — `new-calendar-btn`, `create-calendar` and
/// `calendar-import-button` (events.md § Errors & edge cases → *The other
/// calendar writes*) — over the same read as the compose:
///
/// - *possible* only when the read is **loaded**;
/// - *blocked* "loading" while **unresolved** (a missing key included);
/// - *blocked* with the provisioning error on **provision failed**: a new
///   calendar is the same `provision_calendar` call the lazy `Personal` just
///   had refused, so offering it would ask the person to repeat a refused call
///   under another name; the next read retries the provision, and the import
///   needs a listed calendar to write into.
///
/// Its answers coincide with [`event_create_possibility`]'s today and it asks
/// that predicate, so the page shows one reason for every write door; it is
/// its own function because its doors are, and a later ruling may part them.
pub fn calendar_write_possibility(read: &CalendarsReadState) -> Possibility {
    event_create_possibility(read)
}

/// The calendar a new event goes to, from the page's raw selection and the
/// loaded calendars' ids: the selection while it still names a listed
/// calendar ([`crate::resolve_calendar_selection`]'s staleness rule), else the
/// first calendar (events.md § Persistence rules: the calendar is resolved at
/// submit, first-calendar fallback). `None` only for an empty list.
pub fn event_create_calendar<I, S>(selected: Option<&str>, calendar_ids: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let ids: Vec<S> = calendar_ids.into_iter().collect();
    crate::resolve_calendar_selection(selected, ids.iter().map(AsRef::as_ref))
        .map(str::to_owned)
        .or_else(|| ids.first().map(|id| id.as_ref().to_owned()))
}

/// The submit's side of the gate: ask [`event_create_possibility`] first and
/// refuse with its reason; when possible, resolve the target calendar with
/// [`event_create_calendar`]. An app's create goes through this, so no submit
/// can build an empty calendar id. A *possible* answer with no listed id (the
/// app's rows lag the read) refuses as still loading.
pub fn event_create_target<I, S>(
    read: &CalendarsReadState,
    selected: Option<&str>,
    calendar_ids: I,
) -> Result<String, LocalizedText>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    match event_create_possibility(read) {
        Possibility::Possible => event_create_calendar(selected, calendar_ids)
            .ok_or_else(|| LocalizedText::key(reason_keys::LOADING)),
        Possibility::Blocked { reason } => Err(reason),
        // The predicate never answers absent; were it to, the submit refuses
        // as loading rather than build a create with no calendar.
        Possibility::Absent => Err(LocalizedText::key(reason_keys::LOADING)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u8) -> CalendarEntry {
        CalendarEntry {
            calendar_id: vec![id; 32],
            encrypted_metadata: vec![],
            ctag: 0,
            highestmodseq: 0,
            event_count: 0,
            created_at: 0,
        }
    }

    fn blocked(key: &str) -> Possibility {
        Possibility::blocked(LocalizedText::key(key))
    }

    // --- the four answers -------------------------------------------------

    #[test]
    fn a_listing_with_a_calendar_is_loaded() {
        let read = CalendarsRead::from_listing(CalendarsWithPersonal {
            calendars: vec![entry(1)],
            personal_provision_error: None,
        });
        assert_eq!(read.state(), CalendarsReadState::Loaded);
        assert_eq!(read.calendars().len(), 1);
    }

    #[test]
    fn an_empty_listing_with_a_refused_provision_carries_the_error() {
        let read = CalendarsRead::from_listing(CalendarsWithPersonal {
            calendars: vec![],
            personal_provision_error: Some("provision_calendar: refused".into()),
        });
        assert_eq!(
            read.state(),
            CalendarsReadState::ProvisionFailed {
                error: "provision_calendar: refused".into()
            }
        );
        assert!(read.calendars().is_empty());
    }

    /// An empty list is never "loaded", even when the provision said yes.
    #[test]
    fn an_empty_listing_after_a_reported_provision_is_still_not_loaded() {
        let read = CalendarsRead::from_listing(CalendarsWithPersonal::default());
        assert!(matches!(
            read.state(),
            CalendarsReadState::ProvisionFailed { .. }
        ));
    }

    #[test]
    fn unresolved_carries_no_calendars() {
        assert_eq!(
            CalendarsRead::Unresolved.state(),
            CalendarsReadState::Unresolved
        );
        assert!(CalendarsRead::Unresolved.calendars().is_empty());
    }

    #[test]
    fn the_serde_form_is_the_tagged_shape_web_reads() {
        let failed = CalendarsReadState::ProvisionFailed { error: "x".into() };
        let json = serde_json::to_value(&failed).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"state": "provision_failed", "error": "x"})
        );
        assert_eq!(
            serde_json::to_value(CalendarsReadState::Unresolved).unwrap(),
            serde_json::json!({"state": "unresolved"})
        );
        // The SPA hands the read back with its calendars beside the state.
        let back: CalendarsReadState =
            serde_json::from_value(serde_json::json!({"state": "loaded", "calendars": []}))
                .unwrap();
        assert_eq!(back, CalendarsReadState::Loaded);
    }

    // --- the predicate, per answer ----------------------------------------

    #[test]
    fn loaded_is_possible() {
        assert_eq!(
            event_create_possibility(&CalendarsReadState::Loaded),
            Possibility::Possible
        );
    }

    #[test]
    fn unresolved_is_blocked_loading() {
        assert_eq!(
            event_create_possibility(&CalendarsReadState::Unresolved),
            blocked(reason_keys::LOADING)
        );
    }

    #[test]
    fn provision_failed_is_blocked_with_the_error() {
        let read = CalendarsReadState::ProvisionFailed {
            error: "seal Personal metadata: bad key".into(),
        };
        let answer = event_create_possibility(&read);
        let reason = answer.reason().expect("blocked");
        assert_eq!(reason.key, reason_keys::PROVISION_FAILED);
        assert_eq!(
            reason.args.get("error").map(String::as_str),
            Some("seal Personal metadata: bad key")
        );
    }

    /// Ruling 5 as built: no answer is ever *absent* and none names a step —
    /// a missing key is the mint in flight, "loading".
    #[test]
    fn no_answer_is_absent_or_names_a_step() {
        for read in [
            CalendarsReadState::Unresolved,
            CalendarsReadState::Loaded,
            CalendarsReadState::ProvisionFailed { error: "x".into() },
        ] {
            let answer = event_create_possibility(&read);
            assert_ne!(answer, Possibility::Absent, "{read:?}");
            if let Some(reason) = answer.reason() {
                assert!(
                    [reason_keys::LOADING, reason_keys::PROVISION_FAILED]
                        .contains(&reason.key.as_str()),
                    "{read:?} -> {}",
                    reason.key
                );
            }
        }
    }

    // --- the other calendar writes ------------------------------------------

    #[test]
    fn a_calendar_write_is_possible_only_on_a_loaded_read() {
        assert_eq!(
            calendar_write_possibility(&CalendarsReadState::Loaded),
            Possibility::Possible
        );
        assert_eq!(
            calendar_write_possibility(&CalendarsReadState::Unresolved),
            blocked(reason_keys::LOADING)
        );
    }

    /// A refused lazy `Personal` blocks a new calendar and the import with the
    /// provisioning error, the same sentence the compose's door paints.
    #[test]
    fn a_calendar_write_on_a_refused_provision_is_blocked_with_the_error() {
        let read = CalendarsReadState::ProvisionFailed {
            error: "provision_calendar: refused".into(),
        };
        let answer = calendar_write_possibility(&read);
        let reason = answer.reason().expect("blocked");
        assert_eq!(reason.key, reason_keys::PROVISION_FAILED);
        assert_eq!(
            reason.args.get("error").map(String::as_str),
            Some("provision_calendar: refused")
        );
        assert_eq!(answer, event_create_possibility(&read));
    }

    #[test]
    fn a_calendar_write_is_never_absent() {
        for read in [
            CalendarsReadState::Unresolved,
            CalendarsReadState::Loaded,
            CalendarsReadState::ProvisionFailed { error: "x".into() },
        ] {
            assert_ne!(calendar_write_possibility(&read), Possibility::Absent);
        }
    }

    // --- the submit side ---------------------------------------------------

    #[test]
    fn the_target_is_the_live_selection_else_the_first_calendar() {
        let ids = ["aa", "bb"];
        assert_eq!(event_create_calendar(Some("bb"), ids), Some("bb".into()));
        assert_eq!(event_create_calendar(None, ids), Some("aa".into()));
        // A stale selection (a calendar this device no longer lists) falls back.
        assert_eq!(event_create_calendar(Some("zz"), ids), Some("aa".into()));
        assert_eq!(event_create_calendar(None, Vec::<String>::new()), None);
    }

    #[test]
    fn a_submit_on_a_loaded_read_resolves_a_calendar() {
        assert_eq!(
            event_create_target(&CalendarsReadState::Loaded, None, ["aa"]),
            Ok("aa".into())
        );
    }

    /// No submit can build an empty calendar id: every non-loaded answer, and
    /// a loaded answer whose rows are not in yet, refuses in words.
    #[test]
    fn a_submit_without_a_calendar_refuses_with_the_predicate_reason() {
        let none = Vec::<String>::new;
        assert_eq!(
            event_create_target(&CalendarsReadState::Unresolved, None, none()),
            Err(LocalizedText::key(reason_keys::LOADING))
        );
        // A selection on an unresolved read (the key not minted yet) still
        // refuses as loading, never with a credential step.
        assert_eq!(
            event_create_target(&CalendarsReadState::Unresolved, Some("aa"), ["aa"]),
            Err(LocalizedText::key(reason_keys::LOADING))
        );
        assert_eq!(
            event_create_target(&CalendarsReadState::Loaded, None, none()),
            Err(LocalizedText::key(reason_keys::LOADING))
        );
        assert!(matches!(
            event_create_target(
                &CalendarsReadState::ProvisionFailed { error: "e".into() },
                None,
                none()
            ),
            Err(reason) if reason.key == reason_keys::PROVISION_FAILED
        ));
    }

    #[test]
    fn a_page_that_has_not_read_yet_is_unresolved() {
        assert_eq!(
            CalendarsReadState::default(),
            CalendarsReadState::Unresolved
        );
    }
}
