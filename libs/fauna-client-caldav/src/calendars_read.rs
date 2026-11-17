//! The calendar read's four answers and the one predicate that gates every
//! door to an event compose — `docs/goal/ui/events.md` § Errors & edge cases →
//! *No calendar to hold an event*, rulings 2 and 3, the page's instance of
//! `docs/goal/ui/README.md` § Offer only what is possible (rule 6: possibility
//! is decided once, in shared Rust, and the same answer guards the action).
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
//! calendar, the import) get their own predicate on this same read.

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
    /// No key, and this person turns storage on with their first credential.
    pub const ADD_CREDENTIAL: &str = "events.create_blocked.add_credential";
    /// No key, the deployment serves none of CalDAV/CardDAV/mail, and this
    /// person is its admin: the admin Calendar page turns the serving on.
    pub const ADMIN_ENABLE_CALENDAR: &str = "events.create_blocked.admin_enable_calendar";
    /// The refusal a submit gives when the predicate answered *absent* (the
    /// door should never have been shown; the action still refuses in words).
    pub const NOT_HERE: &str = "events.create_blocked.not_here";
}

/// What the calendar read answered: ruling 2's four answers.
#[derive(Debug, Clone, PartialEq)]
pub enum CalendarsRead {
    /// The read has not finished, or the custody read itself failed because
    /// the account store is not up yet. A wait: re-run the read when custody's
    /// first listing lands. Never painted as an empty list.
    Unresolved,
    /// Custody is read and holds no MSEK: calendar storage was never set up
    /// for this account.
    NoKey,
    /// The key is there, the list was empty and the lazy `Personal` was
    /// refused. The error is carried, not logged; the next read retries.
    ProvisionFailed { error: String },
    /// At least one calendar (never empty).
    Loaded { calendars: Vec<CalendarEntry> },
}

/// [`CalendarsRead`] without its calendars: what [`event_create_possibility`]
/// decides on, and the shape the faces hand across. The serde form is
/// `{"state": "unresolved" | "no_key" | "loaded"}` or `{"state":
/// "provision_failed", "error": "…"}`, the shape the web SPA reads and hands
/// back (it is computed in this process and never decoded off a wire).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CalendarsReadState {
    /// Also the state before any read has run: a page that has not read yet
    /// is loading, never empty.
    #[default]
    Unresolved,
    NoKey,
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
            CalendarsRead::NoKey => CalendarsReadState::NoKey,
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
    /// The Events page's calendar load, answering ruling 2's four answers:
    /// the custody answer ([`fauna_client_config::dav_custody`]) decides
    /// *unresolved* and *no key* without a call; with the key,
    /// [`Self::list_calendars_with_personal`] runs (the lazy `Personal`
    /// included) and its outcome is *loaded* or *provision failed*. A list
    /// error is the call's error, as before.
    pub async fn read_calendars(&self, custody: &DavCustody) -> Result<CalendarsRead, R::Error> {
        match custody {
            DavCustody::Unreadable(_) => Ok(CalendarsRead::Unresolved),
            DavCustody::NoKey => Ok(CalendarsRead::NoKey),
            DavCustody::Ready(ctx) => Ok(CalendarsRead::from_listing(
                self.list_calendars_with_personal(&ctx.actor_id, &ctx.msek)
                    .await?,
            )),
        }
    }
}

/// Can this person open an event compose (and send a create) now? Ruling 3:
///
/// - *possible* only when the read is **loaded**;
/// - *blocked* "loading" while **unresolved**;
/// - *blocked* with the provisioning error on **provision failed** (the next
///   read retries the provision);
/// - on **no key**: *blocked* naming the step and its place when this person
///   can take it from this app — any person on a deployment that serves
///   CalDAV, CardDAV or mail (Settings → Mail & Calendar → Add credential, the
///   first credential being the enable), an admin of a deployment that serves
///   none (the admin Calendar page first) — and *absent* for a non-admin on a
///   deployment that serves none of the three.
///
/// Every door to the compose asks it, and the submit asks it again
/// ([`event_create_target`]), so what the page offers and what the action
/// accepts cannot disagree.
pub fn event_create_possibility(
    read: &CalendarsReadState,
    is_admin: bool,
    deployment_serves_dav_or_mail: bool,
) -> Possibility {
    match read {
        CalendarsReadState::Loaded => Possibility::Possible,
        CalendarsReadState::Unresolved => {
            Possibility::blocked(LocalizedText::key(reason_keys::LOADING))
        }
        CalendarsReadState::ProvisionFailed { error } => Possibility::blocked(
            LocalizedText::key_arg(reason_keys::PROVISION_FAILED, "error", error.clone()),
        ),
        CalendarsReadState::NoKey if deployment_serves_dav_or_mail => {
            Possibility::blocked(LocalizedText::key(reason_keys::ADD_CREDENTIAL))
        }
        CalendarsReadState::NoKey if is_admin => {
            Possibility::blocked(LocalizedText::key(reason_keys::ADMIN_ENABLE_CALENDAR))
        }
        CalendarsReadState::NoKey => Possibility::Absent,
    }
}

/// The unblocking step a blocked [`event_create_possibility`] answer names,
/// and so the place ruling 3's one-press jump goes to. `None` when the answer
/// names no step this person takes on another page: *possible*, *loading*,
/// a refused provision (the next read retries it) and *absent*. Computed in
/// this process from the read and handed to the app; never decoded, so it
/// serializes only (a face hands it out, nothing hands it back).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventCreateStep {
    /// Settings → Mail & Calendar → Add credential: the first credential is
    /// the enable that mints the calendar key.
    AddCredential,
    /// The admin area's Calendar page, which turns the deployment's CalDAV
    /// serving on (then the person adds their first credential).
    AdminEnableCalendar,
}

/// The step [`event_create_possibility`]'s reason names, decided by the same
/// inputs, so the jump an app paints beside the reason cannot point anywhere
/// but where the reason says.
pub fn event_create_step(
    read: &CalendarsReadState,
    is_admin: bool,
    deployment_serves_dav_or_mail: bool,
) -> Option<EventCreateStep> {
    match read {
        CalendarsReadState::NoKey if deployment_serves_dav_or_mail => {
            Some(EventCreateStep::AddCredential)
        }
        CalendarsReadState::NoKey if is_admin => Some(EventCreateStep::AdminEnableCalendar),
        _ => None,
    }
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
/// can build an empty calendar id. An *absent* answer still refuses in words
/// (`NOT_HERE`); a *possible* answer with no listed id (the app's rows lag
/// the read) refuses as still loading.
pub fn event_create_target<I, S>(
    read: &CalendarsReadState,
    is_admin: bool,
    deployment_serves_dav_or_mail: bool,
    selected: Option<&str>,
    calendar_ids: I,
) -> Result<String, LocalizedText>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    match event_create_possibility(read, is_admin, deployment_serves_dav_or_mail) {
        Possibility::Possible => event_create_calendar(selected, calendar_ids)
            .ok_or_else(|| LocalizedText::key(reason_keys::LOADING)),
        Possibility::Blocked { reason } => Err(reason),
        Possibility::Absent => Err(LocalizedText::key(reason_keys::NOT_HERE)),
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
    fn unresolved_and_no_key_carry_no_calendars() {
        assert_eq!(
            CalendarsRead::Unresolved.state(),
            CalendarsReadState::Unresolved
        );
        assert_eq!(CalendarsRead::NoKey.state(), CalendarsReadState::NoKey);
        assert!(CalendarsRead::Unresolved.calendars().is_empty());
        assert!(CalendarsRead::NoKey.calendars().is_empty());
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
            serde_json::to_value(CalendarsReadState::NoKey).unwrap(),
            serde_json::json!({"state": "no_key"})
        );
        // The SPA hands the read back with its calendars beside the state.
        let back: CalendarsReadState =
            serde_json::from_value(serde_json::json!({"state": "loaded", "calendars": []}))
                .unwrap();
        assert_eq!(back, CalendarsReadState::Loaded);
    }

    // --- the predicate, per answer and per person/deployment ---------------

    #[test]
    fn loaded_is_possible_for_everyone() {
        for (admin, serves) in [(false, false), (false, true), (true, false), (true, true)] {
            assert_eq!(
                event_create_possibility(&CalendarsReadState::Loaded, admin, serves),
                Possibility::Possible
            );
        }
    }

    #[test]
    fn unresolved_is_blocked_loading_for_everyone() {
        for (admin, serves) in [(false, false), (false, true), (true, false), (true, true)] {
            assert_eq!(
                event_create_possibility(&CalendarsReadState::Unresolved, admin, serves),
                blocked(reason_keys::LOADING)
            );
        }
    }

    #[test]
    fn provision_failed_is_blocked_with_the_error() {
        let read = CalendarsReadState::ProvisionFailed {
            error: "seal Personal metadata: bad key".into(),
        };
        for (admin, serves) in [(false, false), (false, true), (true, false), (true, true)] {
            let answer = event_create_possibility(&read, admin, serves);
            let reason = answer.reason().expect("blocked");
            assert_eq!(reason.key, reason_keys::PROVISION_FAILED);
            assert_eq!(
                reason.args.get("error").map(String::as_str),
                Some("seal Personal metadata: bad key")
            );
        }
    }

    #[test]
    fn no_key_on_a_serving_deployment_names_add_credential_for_anyone() {
        for admin in [false, true] {
            assert_eq!(
                event_create_possibility(&CalendarsReadState::NoKey, admin, true),
                blocked(reason_keys::ADD_CREDENTIAL)
            );
        }
    }

    #[test]
    fn no_key_on_a_deployment_serving_none_names_the_admin_calendar_page_for_an_admin() {
        assert_eq!(
            event_create_possibility(&CalendarsReadState::NoKey, true, false),
            blocked(reason_keys::ADMIN_ENABLE_CALENDAR)
        );
    }

    #[test]
    fn no_key_on_a_deployment_serving_none_is_absent_for_a_non_admin() {
        assert_eq!(
            event_create_possibility(&CalendarsReadState::NoKey, false, false),
            Possibility::Absent
        );
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
            event_create_target(&CalendarsReadState::Loaded, false, true, None, ["aa"]),
            Ok("aa".into())
        );
    }

    /// No submit can build an empty calendar id: every non-loaded answer, and
    /// a loaded answer whose rows are not in yet, refuses in words.
    #[test]
    fn a_submit_without_a_calendar_refuses_with_the_predicate_reason() {
        let none = Vec::<String>::new;
        assert_eq!(
            event_create_target(&CalendarsReadState::Unresolved, false, true, None, none()),
            Err(LocalizedText::key(reason_keys::LOADING))
        );
        assert_eq!(
            event_create_target(&CalendarsReadState::NoKey, false, true, Some("aa"), ["aa"]),
            Err(LocalizedText::key(reason_keys::ADD_CREDENTIAL))
        );
        assert_eq!(
            event_create_target(&CalendarsReadState::NoKey, false, false, None, none()),
            Err(LocalizedText::key(reason_keys::NOT_HERE))
        );
        assert_eq!(
            event_create_target(&CalendarsReadState::Loaded, false, true, None, none()),
            Err(LocalizedText::key(reason_keys::LOADING))
        );
        assert!(matches!(
            event_create_target(
                &CalendarsReadState::ProvisionFailed { error: "e".into() },
                true,
                false,
                None,
                none()
            ),
            Err(reason) if reason.key == reason_keys::PROVISION_FAILED
        ));
    }

    #[test]
    fn the_step_follows_the_reason_it_is_painted_beside() {
        use CalendarsReadState as S;
        for (read, admin, serves) in [
            (S::Loaded, false, true),
            (S::Unresolved, true, true),
            (S::ProvisionFailed { error: "x".into() }, true, true),
            (S::NoKey, true, true),
            (S::NoKey, false, true),
            (S::NoKey, true, false),
            (S::NoKey, false, false),
        ] {
            let step = event_create_step(&read, admin, serves);
            let key = event_create_possibility(&read, admin, serves)
                .reason()
                .map(|r| r.key.clone());
            let expected = match key.as_deref() {
                Some(reason_keys::ADD_CREDENTIAL) => Some(EventCreateStep::AddCredential),
                Some(reason_keys::ADMIN_ENABLE_CALENDAR) => {
                    Some(EventCreateStep::AdminEnableCalendar)
                }
                _ => None,
            };
            assert_eq!(step, expected, "{read:?} admin={admin} serves={serves}");
        }
    }

    #[test]
    fn a_page_that_has_not_read_yet_is_unresolved() {
        assert_eq!(
            CalendarsReadState::default(),
            CalendarsReadState::Unresolved
        );
    }
}
