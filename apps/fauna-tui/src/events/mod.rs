//! The events (calendar) page — M5 slice 3
//! (`docs/goal/ui/events.md`; ui.yaml `events`).
//!
//! **Agenda-view CRUD + RSVP + reminders + attendee invite + month/week/day
//! views** (M5 slice 3 parts 1–4): create a calendar, list/create/delete
//! events in it, RSVP **both** on the `event_detail` sub-page and inline on
//! the agenda `event-card` (the `rsvp-button-group` component is a child of
//! both — events.md § components), set/remove a per-event reminder, invite an
//! attendee by email, browse the Outlook-style month grid + week/day time
//! grids (`calendar-view-*`), drill into a month day cell, and import/export
//! the selected calendar as an `.ics` file (`calendar-import-file` /
//! `-import-button` / `-export-button`, § Import / Export — the path is typed,
//! a terminal has no picker; the export lands in [`crate::backups::download_dir`]).
//! NOT built yet: the quick-appearance poll.
//!
//! ⚠ The absent poll does NOT mean external CalDAV writes fail to surface while
//! the user sits on this page. They surface via the `fauna.calendar.changed`
//! push (wired 2026-08-21): `PushEvent::CalendarChanged` folds to
//! `StaleSurfaces { events: true }` and `App::apply_resync` re-runs
//! `events::nav_enter_op`. tui needs no poll for the same reason web and
//! android need none — the push alone carries the while-on-page guarantee
//! (`events.md` § Implementation status today, the quick-appearance matrix, and
//! its `fauna.calendar.changed` push-consumer row). Two sessions have read the
//! line above as "external writes don't appear live on tui" and filed that as a
//! parity gap; the poll is a *backstop* that
//! is genuinely owed, not the mechanism.
//!
//! Consumes `fauna_client_caldav::CalDavClient` directly, exactly as
//! `apps/fauna-linux/src/client.rs` does (events.md § Where logic lives:
//! shared Rust; tui is native like linux, not FFI-mediated). Every op needs
//! the actor's MSEK (the `fauna.state.mail` plane's MSEK), minted by
//! enabling mail/CalDAV — tui has no Settings page yet to drive that through
//! the UI, so e2e mints it via the `events_ensure_mail_enabled` automation
//! command (`automation.rs`; `mail_glue.rs`).

mod agenda;
mod detail;
pub mod drafts;
mod grids;

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_caldav::bridge_routing::{
    DeleteEventRequest, ListCalendarsRequest, ProvisionCalendarRequest, QueryEventsRequest,
};
use fauna_client_caldav::{
    AnonAttendeeDiscovery, CalDavClient, CalendarMetadata, DavRecipientKeys, DecodedEvent,
    DecodedEventsPage, EventFields, add_attendee, apply_rsvp, dispatch_imip_request,
    imip_reply_for_rsvp, imip_request_for_invite, parse_ical, parse_ical_attendees,
    project_attendee_rsvp, seal_calendar_metadata, set_reminder, uid_hash,
    unseal_calendar_metadata,
};
use fauna_client_config::{DavStoreContext, dav_store_context};
use fauna_client_conversations::NestImipDispatch;
use fauna_client_email::EmailClient;
use fauna_conversations::ConversationsSession;
use fauna_core::caltime::{PanDirection, normalize_event_datetime_input, pan};
use fauna_core::identity::ActorKeypair;
use fauna_core::rsvp::RsvpResponse;
use fauna_core::secret::SecretString;
use fauna_i18n::strings::{common as t, errors, events as et};
use tokio::sync::mpsc::UnboundedSender;

use agenda::render_agenda;
use detail::render_detail;
use grids::{clamp_day, date_label, render_day, render_month, render_week, view_mode_label};

use crate::app::{App, UiMessage};
use crate::element::{Element, Field, Gesture};
use crate::pages::Page;

/// The active view is the shared [`fauna_core::caltime::CalendarViewMode`] —
/// one vocabulary and one pan policy across all 7 apps (events.md § Where logic
/// lives). tui keeps only the rendering and the localized labels.
pub use fauna_core::caltime::CalendarViewMode as ViewMode;

/// Which sub-page (ui.yaml `events.sub_pages`) is showing, replacing the
/// agenda list — the wizard/profile-edit convention (one page state enum, no
/// separate overlay flag).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    List,
    CreateCalendar,
    CreateEvent,
    /// The `event_detail` sub-page for the event with this `uid_hash` hex id
    /// (looked up in [`EventsState::events`] — no separate fetch).
    EventDetail(String),
}

#[derive(Debug, Clone)]
pub struct CalendarRow {
    pub id: String,
    pub name: String,
}

/// One roster row on `event_detail`'s `attendee-list` (events.md § Attendee
/// list presentation). `rsvp` is the **projected** status
/// (`fauna_client_caldav::project_attendee_rsvp`) — the asymmetric
/// interested↔TENTATIVE render rule, not the raw wire `PARTSTAT`.
#[derive(Debug, Clone)]
pub struct AttendeeRow {
    pub name: String,
    pub email: String,
    pub rsvp: String,
}

/// `Default` so a fixture can add a field with `..Default::default()` instead
/// of every construction site growing the same axis — two branches independently
/// growing this struct then merge cleanly instead of colliding on that axis.
#[derive(Debug, Clone, Default)]
pub struct EventRow {
    pub id: String,
    pub calendar_id: String,
    pub summary: String,
    pub start: String,
    pub end: String,
    /// The VEVENT `VALARM` offset (e.g. `PT1H`), empty when no reminder is
    /// set (events.md § Reminders).
    pub alarm: String,
    /// The VEVENT `LOCATION`, empty when the event carries none — rendered as
    /// `event-detail-location` (events.md § Element IDs).
    pub location: String,
    /// The VEVENT `DESCRIPTION`, empty when the event carries none — rendered
    /// as `event-detail-description` (events.md § Element IDs).
    pub description: String,
    /// The full roster, RSVP-projected (events.md § Attendee list
    /// presentation) — populated at fetch time, no separate round trip to
    /// open `event_detail`.
    pub attendees: Vec<AttendeeRow>,
    /// The viewer's own projected RSVP status (`""` when not yet on the
    /// roster) — ui.yaml's declared `data.events[].rsvp_status`.
    pub self_rsvp: String,
}

/// Page state: identity inputs captured at the post-auth hook (mirroring
/// `notifications.rs`), plus the calendar/event caches and the
/// create-calendar/create-event draft buffers.
#[derive(Default)]
pub struct EventsState {
    nest: Option<Arc<NestClient>>,
    /// A `SecretString` (not a plain `String`) so the identity secret is
    /// zeroized on drop, like `SettingsState.secret_hex`.
    secret_hex: SecretString,
    /// The account's mail custody (`fauna.state.mail`) — the MSEK every
    /// encrypted CalDAV op seals and unseals under.
    mail: Option<Arc<dyn fauna_client_config::MailStore>>,
    /// The actor's own `handle@domain` — the VEVENT `ORGANIZER` (conv_backend's
    /// `self_address` derivation: empty unless the handle carries `@domain`).
    organizer_email: String,
    pub view_mode: ViewMode,
    pub mode: Mode,
    /// The grid views' focused date — the month/week/day pan target
    /// (`events-prev/next-month` walk it via `caltime::{prev,next}_month`).
    /// Set to today at [`init`]/[`on_nav_enter`]; [`EventsState::focus`] falls
    /// back to today when unset (a `Default`-constructed state, i.e. unit
    /// tests, leaves `focus_month == 0`).
    pub focus_year: i32,
    pub focus_month: u32,
    pub focus_day: u32,
    pub calendars: Vec<CalendarRow>,
    pub selected_calendar: Option<String>,
    /// The `calendar-visibility` display filter's checked set — client-side
    /// display state, **never persisted server-side and never a privacy or
    /// sharing control** (`ui/events.md` § Where logic lives → *Which calendars
    /// display*). Distinct from [`Self::selected_calendar`]: only one of the two
    /// is in force at a time, and
    /// [`fauna_client_caldav::calendar_is_displayed`] — not this field — owns
    /// how they compose. An **empty** set means "no filter" (the full union),
    /// the pre-first-load state; [`seed_visible_calendars`] fills it on load so
    /// a fresh page paints every box checked, mirroring linux.
    pub visible_calendars: std::collections::HashSet<String>,
    /// Every event fetched for the calendars in *fetch* scope
    /// ([`calendars_in_scope`]) — the raw cache, not what the page shows. Paint
    /// reads [`EventsState::displayed_events`], which applies the shared
    /// display predicate; named `_cache` (as linux names its own) so a renderer
    /// reaching for the unfiltered vec has to say so.
    pub events_cache: Vec<EventRow>,
    /// Monotonic token guarding `app.events.events_cache`/`calendars` against a
    /// stale `RefreshCalendars` (the no-selection union, dispatched at
    /// nav-entry) landing after a fresher `RefreshEvents` (a calendar-item
    /// select) — or the reverse. `Cell`, not a plain field, so `nav_enter_op`
    /// can claim it through a shared `&EventsState` — mirroring every other
    /// page's `nav_enter_op(state: &State)` signature rather than deviating
    /// to `&mut State` just for this page. Claimed at dispatch, checked at commit
    /// (`apply_outcome`); the mechanism apple/android/web already converged
    /// on for this exact race (`events.md` § Implementation status today).
    pub events_gen: std::cell::Cell<u64>,
    pub new_calendar_name: String,
    pub new_event_summary: String,
    pub new_event_dtstart: String,
    pub new_event_dtend: String,
    /// The `event-form-description` draft — the VEVENT `DESCRIPTION` this
    /// compose will write (events.md § Layout & flow's `create_event` bullet).
    pub new_event_description: String,
    /// The `event-form-location` draft — the VEVENT `LOCATION`.
    pub new_event_location: String,
    /// The `event-detail-reminder-select` draft (an ISO-8601 offset like
    /// `PT1H`) — applied only on `event-detail-reminder-set` (events.md §
    /// Reminders: "applied *on Set*, never auto-applied on selection").
    pub reminder_draft: String,
    /// `attendee-invite-field` — the invite-form email draft.
    pub attendee_invite_email: String,
    /// The `"events"` rail's autosave tick channel (`events.md` § Persistence).
    /// Each compose-buffer edit sends the whole draft down it; the debounce task
    /// in [`drafts`] coalesces the burst and persists it to `__drafts`. `None`
    /// when persistence is disabled — a malformed identity secret, or any
    /// `Default`-constructed state (i.e. every unit test), which is why the
    /// notify helper is a no-op rather than an unwrap.
    pub(crate) drafts_tx: Option<UnboundedSender<fauna_client_caldav::drafts::EventDrafts>>,
    /// The rail's `DraftsSync`, so `main.rs`'s leave-door flush
    /// (`drafts::flush_now`) can force a save of [`event_draft`](Self::event_draft)
    /// at quit time without a debounce wait. `None` under the same conditions as
    /// [`Self::drafts_tx`].
    pub(crate) drafts_sync: Option<Arc<drafts::EventDraftsSync>>,
    /// The client locale's week start (caltime `0 = Mon … 6 = Sun`) — the month
    /// grid, the week grid, the weekday header and the week range label all read
    /// this one value, so they cannot disagree about which week is on screen
    /// (`events.md` § Week & day timeline views: "week-start follows the client
    /// locale").
    ///
    /// Probed once at [`init`] rather than per paint, exactly like `focus_*`
    /// above: the locale does not change under a running process, and holding
    /// it as state keeps every render pin deterministic instead of reading
    /// whatever `LC_TIME` the test or CI host happens to carry. A
    /// `Default`-constructed state (i.e. unit tests) is Monday, ISO-8601's
    /// default; a test that needs another locale's grid sets the field.
    pub week_start: u32,
    /// **Refused inbound scheduling changes** — someone who may not change an
    /// event on this calendar tried to, and the shared apply refused them
    /// (`inbound-scheduling-authority.md` § *Surfacing*). The OPEN rows only:
    /// the projection (`fauna_client_account_runtime::refused_changes::load_open`,
    /// over the account plane's `fauna.state.refused-scheduling-changes` row)
    /// drops what the owner has dismissed, so this page never re-derives that
    /// rule.
    ///
    /// Loaded with the calendar list rather than on a poll of its own — the
    /// rows change only when the inbound drain refuses something, which is
    /// rare, and a nav-entry refresh is exactly when the user is looking.
    pub refused_changes: Vec<fauna_core::data::RefusedSchedulingChange>,
    /// `calendar-import-file` — the typed path of the `.ics` file to import
    /// (events.md § Import / Export). A plain buffer, not part of the
    /// `event-form` compose the drafts rail persists.
    pub import_path: String,
    /// What the last import or export said on success — "Imported: 2, …" or
    /// "Calendar exported to <path>" — painted as chrome under the buttons.
    /// A failure goes to `error-message` instead, like every op on this page.
    pub ics_notice: String,
}

/// Build the page state at the post-auth hook (`session::establish`).
pub fn init(
    nest: Arc<NestClient>,
    secret_hex: &str,
    handle: &str,
    mail: Arc<dyn fauna_client_config::MailStore>,
) -> EventsState {
    let (fy, fm, fd) = today();
    EventsState {
        nest: Some(nest),
        secret_hex: SecretString::from(secret_hex),
        mail: Some(mail),
        organizer_email: if handle.contains('@') {
            handle.to_string()
        } else {
            String::new()
        },
        focus_year: fy,
        focus_month: fm,
        focus_day: fd,
        week_start: fauna_core::caltime::locale_week_start(),
        ..EventsState::default()
    }
}

/// Reset the sub-page + refocus on today on nav-entry, so a prior test's
/// compose/detail state or panned month never leaks into a fresh visit to the
/// tab (the same reset-on-enter contract [`crate::events::on_nav_enter`]
/// already gave `mode`).
pub fn on_nav_enter(state: &mut EventsState) {
    state.mode = Mode::List;
    state.ics_notice.clear();
    let (fy, fm, fd) = today();
    state.focus_year = fy;
    state.focus_month = fm;
    state.focus_day = fd;
}

/// The calendar-list + events hydration entering this tab implies — the
/// page's leg of the one nav-edge hook (`crate::app::on_nav_enter`), mirroring
/// `contacts::nav_enter_op`. Returns the op; the caller runs it (the agent
/// awaits, the keyboard spawns). `account_store` is the session's handle as
/// the app holds it — `None` until the account store is assembled, when the
/// refused-change list reads empty and the next hydration fills it in.
pub fn nav_enter_op(
    state: &EventsState,
    account_store: Option<fauna_sync_engine::account_runtime::AccountStoreHandle>,
) -> Option<Op> {
    let nest = state.nest.clone()?;
    let query_gen = state.events_gen.get() + 1;
    state.events_gen.set(query_gen);
    Some(Op::RefreshCalendars {
        nest,
        account_store,
        secret_hex: state.secret_hex.clone(),
        mail: state.mail.clone()?,
        organizer_email: state.organizer_email.clone(),
        selected_calendar: state.selected_calendar.clone(),
        query_gen,
    })
}

/// Fire-and-forget the calendar+events hydration — the **post-auth** path
/// only, where there is no driver ack to honour and nothing to race (mirrors
/// `contacts::spawn_refresh`). The nav edge goes through [`nav_enter_op`] so
/// it can be awaited instead.
pub fn spawn_refresh(
    state: &EventsState,
    account_store: Option<fauna_sync_engine::account_runtime::AccountStoreHandle>,
    tx: &UnboundedSender<UiMessage>,
    session_generation: u64,
) {
    crate::app::spawn_nav_refresh!(
        nav_enter_op(state, account_store),
        tx,
        session_generation,
        Events
    );
}

impl EventsState {
    /// The explicitly selected calendar, when it is still listed — the one
    /// calendar-level import/export acts on (events.md § Import / Export). A
    /// stale selection (the calendar was deleted elsewhere) is none, the same
    /// reading the no-selection union gives it.
    pub fn selected_calendar_row(&self) -> Option<&CalendarRow> {
        let selected = self.selected_calendar.as_deref()?;
        self.calendars.iter().find(|c| c.id == selected)
    }

    /// This page's compose buffers as the `"events"` rail's at-rest record —
    /// the five user-authored `event-form` inputs and nothing else
    /// (`events.md` § Persistence: the calendar is per-device view state and
    /// datetimes rest raw as typed, normalization staying at submit).
    pub(crate) fn event_draft(&self) -> fauna_client_caldav::drafts::EventDrafts {
        fauna_client_caldav::drafts::EventDrafts {
            summary: self.new_event_summary.clone(),
            dtstart: self.new_event_dtstart.clone(),
            dtend: self.new_event_dtend.clone(),
            description: self.new_event_description.clone(),
            location: self.new_event_location.clone(),
        }
    }

    /// Apply a restored draft onto the compose buffers. Used on the launch-load
    /// path, where the form is untouched — the restore never clobbers text the
    /// user is already typing, because it lands before there is any.
    pub(crate) fn apply_event_draft(&mut self, draft: fauna_client_caldav::drafts::EventDrafts) {
        self.new_event_summary = draft.summary;
        self.new_event_dtstart = draft.dtstart;
        self.new_event_dtend = draft.dtend;
        self.new_event_description = draft.description;
        self.new_event_location = draft.location;
    }

    /// Clear the compose buffers **and** tick the rail, so a created or
    /// discarded event stops being a draft on every one of the user's devices.
    /// Distinct from a bare `.clear()` on the fields: forgetting the tick would
    /// leave a stale draft that reappears on the next launch for an event the
    /// user already created.
    pub(crate) fn clear_event_compose(&mut self) {
        self.new_event_summary.clear();
        self.new_event_dtstart.clear();
        self.new_event_dtend.clear();
        self.new_event_description.clear();
        self.new_event_location.clear();
        self.notify_drafts();
    }

    /// Tick the drafts autosave with the current compose state. A no-op when
    /// persistence is off (see [`EventsState::drafts_tx`]); a closed channel
    /// means the autosave task retired with the session.
    pub(crate) fn notify_drafts(&self) {
        if let Some(tx) = &self.drafts_tx {
            let _ = tx.send(self.event_draft());
        }
    }

    /// The grid views' focused date, falling back to today when unset (a
    /// `Default`-constructed state — unit tests that don't call [`init`]).
    pub(super) fn focus(&self) -> (i32, u32, u32) {
        if self.focus_month == 0 {
            today()
        } else {
            (self.focus_year, self.focus_month, self.focus_day)
        }
    }

    /// Move the focus one visible range in `direction` — the shared
    /// `caltime::pan` policy, so the distance matches every other app.
    fn pan(&mut self, direction: PanDirection) {
        let (y, m, d) = pan(self.view_mode, self.focus(), direction);
        self.focus_year = y;
        self.focus_month = m;
        self.focus_day = d;
    }

    /// The events the page shows right now — the whole scope composition in one
    /// shared call (`fauna_client_caldav::calendar_is_displayed`): a live
    /// `calendar-item` selection wins outright, and with none the
    /// `calendar-visibility` checkboxes filter the union (an **empty** checked
    /// set being the union itself).
    ///
    /// Computed once per paint by [`elements`] and handed to every renderer, so
    /// the agenda and the three grids cannot drift apart on the empty-set case —
    /// which is exactly how linux's four private copies had drifted before its
    /// own `shows_calendar` unified them (`ui/events.md` § Where logic lives).
    pub(super) fn displayed_events(&self) -> Vec<&EventRow> {
        self.events_cache
            .iter()
            .filter(|ev| {
                fauna_client_caldav::calendar_is_displayed(
                    self.selected_calendar.as_deref(),
                    self.calendars.iter().map(|c| &c.id),
                    &self.visible_calendars,
                    &ev.calendar_id,
                )
            })
            .collect()
    }
}

/// Fill the `calendar-visibility` checked set on a fresh calendar list, so the
/// page paints every box checked instead of leaning on the empty-set-is-union
/// rule — the seeding linux does in `update_calendar_sidebar`, and the reason
/// an unchecked box is a deliberate user act rather than a default.
///
/// A **brand-new** calendar starts visible (it is not in `previous`), and an
/// existing calendar keeps whatever the user chose. Unchecking every box empties
/// the set, which the shared predicate reads as the union — so the next load
/// re-seeds it, exactly as linux behaves.
fn seed_visible_calendars(
    visible: &mut std::collections::HashSet<String>,
    previous: &[CalendarRow],
    calendars: &[CalendarRow],
) {
    for cal in calendars {
        if !previous.iter().any(|p| p.id == cal.id) {
            visible.insert(cal.id.clone());
        }
    }
    if visible.is_empty() {
        visible.extend(calendars.iter().map(|c| c.id.clone()));
    }
}

/// Flip one calendar's `calendar-visibility` checkbox — purely local display
/// state, with no refetch: on the union arm the cache already holds every owned
/// calendar's events (`ui/events.md` § Where logic lives → *Which calendars
/// display*).
pub fn toggle_calendar_visibility(app: &mut App, calendar_id: String) {
    if !app.events.visible_calendars.remove(&calendar_id) {
        app.events.visible_calendars.insert(calendar_id);
    }
}

/// Today (year, month, day) in system local time — the tui's `chrono::Local`
/// analogue of linux's libc `today()` (events.md § Where logic lives keeps the
/// system clock client-side; the pure date math is shared `fauna_core::caltime`).
fn today() -> (i32, u32, u32) {
    use chrono::Datelike;
    let now = chrono::Local::now();
    (now.year(), now.month(), now.day())
}

/// Current local (hour, minute) — the `calendar-current-time` marker's value.
fn now_hm() -> (u32, u32) {
    use chrono::Timelike;
    let now = chrono::Local::now();
    (now.hour(), now.minute())
}

/// The start time a day-cell double-click prefills alongside the cell's date,
/// rendered `HH:MM` from the shared [`fauna_core::caltime::WORKING_DAY_START`].
///
/// This used to be a private `"09:00"` of this client's own — one of five
/// independent copies of the same policy, which is how linux came to compose an
/// all-day event here while everyone else composed at 09:00 (2026-08-12).
fn default_compose_hour() -> String {
    let (h, m) = fauna_core::caltime::WORKING_DAY_START;
    format!("{h:02}:{m:02}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    SwitchView(ViewMode),
    /// `events-prev-month` / `events-next-month` — **pan the visible range**
    /// (events.md § User actions), one range per click: a month in Month view,
    /// a week in Week, a day in Day, and nothing in the date-unfiltered Agenda.
    /// The step policy is shared — `caltime::pan` — so all 7 apps move the same
    /// distance; the ids keep their "month" spelling from ui.yaml.
    PrevMonth,
    NextMonth,
    ShowCreateCalendar,
    CreateCalendarSubmit,
    ShowCreateEvent,
    CreateEventSubmit,
    /// `events-day-cell-{YYYY-MM-DD}`, **single** press — the Outlook month→day
    /// drill-in: focus that date and switch to Day view (`ui/events.md`
    /// § Layout & flow).
    ///
    /// Client glue on purpose: § Where logic lives makes view-mode +
    /// visible-range a *shared-Rust target* that is "currently client glue" on
    /// the legacy-path apps, lifting with the Step 4c events lift. This follows
    /// that same shape rather than inventing a tui-only home for it.
    DrillIntoDay {
        y: i32,
        m: u32,
        d: u32,
    },
    /// Open the new-event compose prefilled with a date, and — from the week/
    /// day time grids — with a time too (`ui/events.md` § Layout & flow;
    /// § Where logic lives: "client glue prefills the compose `event-form`
    /// `dtstart` with the cell's date").
    ///
    /// Two surfaces, one action, because both mean the same thing to the user
    /// and to the compose form: *start a new event here*. The month grid's
    /// `events-day-cell-{YYYY-MM-DD}` double-press carries no time (`at:
    /// None` → [`default_compose_hour`], the shared working start), while an
    /// empty `events-time-slot-{HH-MM}` in the week/day grid carries the
    /// slot's own snapped `(hour, minute)` — the Outlook empty-space
    /// drill-in that events.md § Week & day timeline views ratifies.
    ComposeOnDay {
        y: i32,
        m: u32,
        d: u32,
        /// The slot's snapped `(hour, minute)`, or `None` for a whole-day cell.
        at: Option<(u32, u32)>,
    },
    BackFromDetail,
    DeleteEvent,
    /// `event-detail-rsvp-going|interested|decline` — the answer fed straight to
    /// `fauna_client_caldav::apply_rsvp` (events.md § Layout & flow, shown to
    /// every viewer). Typed to the submission set, so the inbound-only
    /// `tentative` is not expressible here (caldav-server.md § RSVP semantics).
    Rsvp(RsvpResponse),
    /// The reminder `<select>`'s local draft write (events.md § Reminders —
    /// "applied *on Set*, never auto-applied on selection"), so this carries
    /// no `Op`.
    SetReminderOffset(String),
    /// `event-detail-reminder-set` — applies `EventsState::reminder_draft`.
    ReminderSubmit,
    /// `event-detail-reminder-remove` — clears the reminder.
    RemoveReminder,
    /// `attendee-invite-button` — adds `EventsState::attendee_invite_email`
    /// to the roster and fans out the iMIP `REQUEST` (events.md § User
    /// actions: `attendee-invite-field`).
    InviteAttendee,
    /// `refused-change-dismiss[i]` — the owner's *I have seen this* on one
    /// refused-change row, carrying that row's key
    /// (`fauna_core::data::RefusedSchedulingChange::key`).
    ///
    /// The ONLY gesture the list carries: the ruling deliberately gives it no
    /// "apply anyway", because a user cannot adjudicate an authority question
    /// the client could not.
    DismissRefusedChange(String),
    /// `calendar-import-button` — import the `.ics` at
    /// [`EventsState::import_path`] into the selected calendar (events.md
    /// § Import / Export). An empty path answers on `error-message` locally.
    ImportIcs,
    /// `calendar-export-button` — write the selected calendar as one `.ics`
    /// file into the downloads directory and name the file.
    ExportIcs,
}

impl Action {
    /// The wire kind this gesture issues — the offline gate's input
    /// (`crate::element::Gesture::wire_kind`). Exhaustive with no fallback arm,
    /// so a new events gesture must answer the offline question.
    ///
    /// Every write on this page — creating a calendar or event, deleting one,
    /// RSVPing, setting or clearing a reminder, inviting an attendee — ends in
    /// a CalDAV bridge call the shared table classifies `OnlineOnly`, so the
    /// whole mutating half of the page desensitizes together. RSVP, reminders
    /// and invite are read-mutate-rewrite: they re-PUT the canonical VEVENT
    /// text, so the kind that decides them is the **write**, not the read they
    /// open with.
    ///
    /// The whole navigating half is local, and deliberately so: panning the
    /// range, switching view mode, drilling into a day and opening a compose
    /// change nothing on the nest, and greying them would strand a viewer on
    /// whatever range they happened to be looking at when the connection
    /// dropped — with a calendar they can still read.
    pub fn wire_kind(&self) -> Option<&'static str> {
        match self {
            Action::CreateCalendarSubmit => Some("fauna.bridges.provision_calendar"),
            Action::DeleteEvent => Some("fauna.bridges.delete_event"),
            // The four VEVENT writers. `CreateEvent` PUTs a new one; the other
            // three re-PUT an existing one after a local rewrite
            // (`apply_rsvp` / `set_reminder` / `add_attendee`, the last also
            // dispatching its iMIP `REQUEST`, which needs the nest just as
            // much).
            Action::CreateEventSubmit
            | Action::Rsvp(_)
            | Action::ReminderSubmit
            | Action::RemoveReminder
            | Action::InviteAttendee => Some("fauna.bridges.put_event_ciphertext"),
            // Import seals and PUTs every VEVENT in the file; export is the
            // calendar read and nothing else.
            Action::ImportIcs => Some("fauna.bridges.put_event_ciphertext"),
            Action::ExportIcs => Some("fauna.bridges.query_events"),

            // Local. Dismissing a refused-change row is a write to this
            // device's account store (the plane row carries the dismissal and
            // ships it at the next pass), so it needs no nest: reading a
            // notice and acknowledging it is not work that should need a
            // connection.
            Action::DismissRefusedChange(_) => None,

            // Local. View mode, range panning, the month→day drill-in, the two
            // compose openers, backing out of the detail pane, and the reminder
            // `<select>`'s draft — which is applied on Set, never on selection
            // (`events.md` § Reminders), so the select itself issues nothing.
            Action::SwitchView(_)
            | Action::PrevMonth
            | Action::NextMonth
            | Action::ShowCreateCalendar
            | Action::ShowCreateEvent
            | Action::DrillIntoDay { .. }
            | Action::ComposeOnDay { .. }
            | Action::BackFromDetail
            | Action::SetReminderOffset(_) => None,
        }
    }
}

/// Local half of a gesture → its network half (the feed/notifications split:
/// the agent awaits the op, the keyboard spawns it). A separate
/// `select_calendar`/`open_event_detail` entry point (not an [`Action`]
/// variant) carries the target id — see [`select_calendar`]/[`open_event_detail`].
pub fn apply_local(app: &mut App, action: Action) -> Option<Op> {
    let (nest, secret_hex) = (app.events.nest.clone()?, app.events.secret_hex.clone());
    let mail = app.events.mail.clone()?;
    match action {
        Action::SwitchView(mode) => {
            app.events.view_mode = mode;
            open_time_axis_at_working_start(app);
            None
        }
        Action::PrevMonth => {
            app.events.pan(PanDirection::Backward);
            None
        }
        Action::NextMonth => {
            app.events.pan(PanDirection::Forward);
            None
        }
        Action::ShowCreateCalendar => {
            app.events.mode = Mode::CreateCalendar;
            app.events.new_calendar_name.clear();
            None
        }
        Action::CreateCalendarSubmit => Some(Op::CreateCalendar {
            nest,
            secret_hex,
            mail: mail.clone(),
            name: app.events.new_calendar_name.clone(),
        }),
        Action::ShowCreateEvent => {
            // Deliberately does NOT clear: this opener RESUMES the persisted
            // draft (`events.md` § Persistence — "opening the compose resumes
            // the draft; a day-cell gesture starts fresh"). Clearing here is
            // what would make the `"events"` rail inert, since this is the
            // opener a user reaches for after relaunching to finish the event
            // they were writing. The buffers are emptied on a successful
            // create, on discard, and by the day-cell gesture below.
            app.events.mode = Mode::CreateEvent;
            None
        }
        Action::DrillIntoDay { y, m, d } => {
            app.events.focus_year = y;
            app.events.focus_month = m;
            app.events.focus_day = clamp_day(y, m, d);
            app.events.view_mode = ViewMode::Day;
            open_time_axis_at_working_start(app);
            None
        }
        Action::ComposeOnDay { y, m, d, at } => {
            // Focus follows the cell, so a Back out of the compose returns to
            // the month the user was actually looking at rather than to
            // wherever focus happened to be.
            app.events.focus_year = y;
            app.events.focus_month = m;
            app.events.focus_day = clamp_day(y, m, d);
            app.events.mode = Mode::CreateEvent;
            // This gesture means *start a new event here* (§ Layout & flow's
            // `create_event` bullet), so unlike the New Event opener it clears
            // rather than resuming — and the clear ticks the rail, so the
            // abandoned draft stops following the user to their other devices.
            app.events.clear_event_compose();
            // The date is the prefill the goal doc names; the time is the
            // clicked slot's when the grid supplied one and the shared working
            // start otherwise. Either way it is normalized on submit exactly
            // like a typed value (`normalize_event_datetime_input`).
            let hm = at.map_or_else(default_compose_hour, |(h, min)| format!("{h:02}:{min:02}"));
            app.events.new_event_dtstart = format!("{y:04}-{m:02}-{d:02}T{hm}");
            app.events.notify_drafts();
            None
        }
        Action::CreateEventSubmit => Some(Op::CreateEvent {
            nest,
            secret_hex,
            mail: mail.clone(),
            // Fall back to the first calendar when none is explicitly selected
            // (linux parity — the grid views create without a prior
            // `select_calendar`); `EventCreated` then selects it for display.
            calendar_id: app
                .events
                .selected_calendar
                .clone()
                .or_else(|| app.events.calendars.first().map(|c| c.id.clone()))
                .unwrap_or_default(),
            organizer_email: app.events.organizer_email.clone(),
            summary: app.events.new_event_summary.clone(),
            // The A2 rule (events.md § Where logic lives): pad a bare
            // `…THH:MM` to `…THH:MM:00`, unify a space-separated date+time to
            // `T` — the other 6 clients already normalize this text-field
            // input before it reaches the nest.
            dtstart: normalize_event_datetime_input(&app.events.new_event_dtstart),
            dtend: normalize_event_datetime_input(&app.events.new_event_dtend),
            description: app.events.new_event_description.clone(),
            location: app.events.new_event_location.clone(),
        }),
        Action::BackFromDetail => {
            app.events.mode = Mode::List;
            None
        }
        Action::DeleteEvent => {
            let Mode::EventDetail(event_id) = app.events.mode.clone() else {
                return None;
            };
            let calendar_id = app
                .events
                .events_cache
                .iter()
                .find(|e| e.id == event_id)
                .map(|e| e.calendar_id.clone())
                .or_else(|| app.events.selected_calendar.clone())
                .unwrap_or_default();
            Some(Op::DeleteEvent {
                nest,
                secret_hex,
                mail: mail.clone(),
                calendar_id,
                uid_hash_hex: event_id,
                organizer_email: app.events.organizer_email.clone(),
            })
        }
        Action::Rsvp(response) => {
            let (calendar_id, uid_hash_hex) = detail_target(app)?;
            rsvp_op(app, calendar_id, uid_hash_hex, response)
        }
        Action::SetReminderOffset(offset) => {
            app.events.reminder_draft = offset;
            None
        }
        Action::ReminderSubmit => {
            let (calendar_id, uid_hash_hex) = detail_target(app)?;
            Some(Op::SetReminder {
                nest,
                secret_hex,
                mail: mail.clone(),
                calendar_id,
                uid_hash_hex,
                organizer_email: app.events.organizer_email.clone(),
                offset: app.events.reminder_draft.clone(),
            })
        }
        Action::RemoveReminder => {
            let (calendar_id, uid_hash_hex) = detail_target(app)?;
            Some(Op::SetReminder {
                nest,
                secret_hex,
                mail: mail.clone(),
                calendar_id,
                uid_hash_hex,
                organizer_email: app.events.organizer_email.clone(),
                offset: String::new(),
            })
        }
        Action::InviteAttendee => {
            let (calendar_id, uid_hash_hex) = detail_target(app)?;
            Some(Op::InviteAttendee {
                nest,
                secret_hex,
                mail: mail.clone(),
                calendar_id,
                uid_hash_hex,
                organizer_email: app.events.organizer_email.clone(),
                attendee_email: app.events.attendee_invite_email.clone(),
                real_session: app.conversations.real_session.clone(),
            })
        }
        Action::DismissRefusedChange(key) => {
            // Optimistic: drop the row from the painted list now, so the
            // acknowledgement is instant in a terminal, and let the op's
            // re-read be the truth (it also brings in anything another device
            // has raised since).
            app.events.refused_changes.retain(|row| row.key() != key);
            Some(Op::DismissRefusedChange {
                account_store: app.settings.account_store.clone(),
                key,
            })
        }
        Action::ImportIcs => {
            let calendar = app.events.selected_calendar_row()?.clone();
            let path = app.events.import_path.trim().to_string();
            if path.is_empty() {
                // Said, never a silent no-op (events.md § Import / Export) —
                // in typed-path words, since there is no picker to "choose" with.
                app.errors
                    .insert(Page::Events, et::ICS_PATH_REQUIRED.to_string());
                return None;
            }
            app.events.ics_notice.clear();
            Some(Op::ImportIcs {
                nest,
                secret_hex,
                mail: mail.clone(),
                calendar_id: calendar.id,
                organizer_email: app.events.organizer_email.clone(),
                path,
            })
        }
        Action::ExportIcs => {
            let calendar = app.events.selected_calendar_row()?.clone();
            app.events.ics_notice.clear();
            Some(Op::ExportIcs {
                nest,
                secret_hex,
                mail: mail.clone(),
                calendar_id: calendar.id,
                calendar_name: calendar.name,
            })
        }
    }
}

/// Seat the focus ring on the ~08:00 time slot when the week/day grids come
/// up — events.md § Week & day timeline views: "auto-scroll to ~08:00 on first
/// render".
///
/// A GUI app scrolls its viewport directly. In tui the viewport follows the
/// focus ring (`crate::ui::scroll_offset`), so the focus IS the scroll, and
/// without this the 96-row axis opens at 00:00: the user faces seven hours of
/// empty night, and — since a terminal cannot click a row that was never
/// painted — the whole empty-slot quick-create affordance is out of mouse
/// reach on entry. A no-op on the agenda/month views, which paint no axis.
fn open_time_axis_at_working_start(app: &mut App) {
    if !matches!(app.events.view_mode, ViewMode::Week | ViewMode::Day) {
        return;
    }
    let opens_at = grids::time_slot_id(grids::GRID_OPENS_AT_MINUTE);
    if let Some(index) = app
        .page_elements()
        .iter()
        .filter(|e| e.focusable())
        .position(|e| e.id == opens_at)
    {
        app.focus = index;
    }
}

/// The `(calendar_id, uid_hash_hex)` pair for the event currently open on
/// `event_detail` — the shared lookup behind RSVP/reminder submits (mirrors
/// `Action::DeleteEvent`'s inline lookup above).
fn detail_target(app: &App) -> Option<(String, String)> {
    let Mode::EventDetail(event_id) = app.events.mode.clone() else {
        return None;
    };
    Some(event_target(app, event_id))
}

/// Resolve an event id to the `(calendar_id, uid_hash_hex)` pair every
/// per-event [`Op`] carries. The row is normally cached, so the calendar comes
/// off it; the selected-calendar fallback covers a target whose row has since
/// been evicted (a refresh landing under an open detail).
fn event_target(app: &App, event_id: String) -> (String, String) {
    let calendar_id = app
        .events
        .events_cache
        .iter()
        .find(|e| e.id == event_id)
        .map(|e| e.calendar_id.clone())
        .or_else(|| app.events.selected_calendar.clone())
        .unwrap_or_default();
    (calendar_id, event_id)
}

/// **The one RSVP `Op` producer.** Both RSVP surfaces submit through it — the
/// `event_detail` trio (`event-detail-rsvp-*`, via [`Action::Rsvp`]) and the
/// agenda card trio (`event-rsvp-*`, via [`rsvp_event`]) — so the two cannot
/// drift apart.
///
/// The single-helper shape is deliberate, and linux paid for the lesson: its
/// card trio shipped ID-tagged and packed into every agenda row with **no
/// `connect_clicked` at all**, so a user's *Going* click did nothing while the
/// e2e agent still answered `{"ok": true}` — green in `ui-actual`, green in
/// `events.md`'s matrix, and dead on screen (2026-07-31). linux
/// now funnels both surfaces through its own `event_detail::wire_rsvp` for the
/// same reason.
fn rsvp_op(
    app: &App,
    calendar_id: String,
    uid_hash_hex: String,
    response: RsvpResponse,
) -> Option<Op> {
    Some(Op::RsvpEvent {
        nest: app.events.nest.clone()?,
        secret_hex: app.events.secret_hex.clone(),
        mail: app.events.mail.clone()?,
        calendar_id,
        uid_hash_hex,
        organizer_email: app.events.organizer_email.clone(),
        response,
    })
}

/// Card-level inline RSVP (`event-rsvp-going|interested|decline` on an agenda
/// `event-card`) — carries its own target id, so it is dispatched directly
/// rather than through [`Action`]/[`apply_local`], mirroring
/// [`select_calendar`]/[`open_event_detail`].
///
/// Unlike the detail trio this does **not** open, select, or otherwise disturb
/// the page: `events.md` § components makes `rsvp-button-group` a child of both
/// `event-card` and `event_detail`, and the agenda quick-action's whole point is
/// RSVPing without leaving the list.
pub fn rsvp_event(app: &App, event_id: String, response: RsvpResponse) -> Option<Op> {
    let (calendar_id, uid_hash_hex) = event_target(app, event_id);
    rsvp_op(app, calendar_id, uid_hash_hex, response)
}

/// Select a calendar (`calendar-item` click) — carries the target id, so it
/// is dispatched directly rather than through [`Action`]/[`apply_local`]
/// (mirroring [`crate::element::Gesture::OpenProfile`]'s "carries its own
/// target" shape).
pub fn select_calendar(app: &mut App, calendar_id: String) -> Option<Op> {
    app.events.selected_calendar = Some(calendar_id.clone());
    app.events.mode = Mode::List;
    let nest = app.events.nest.clone()?;
    let query_gen = app.events.events_gen.get() + 1;
    app.events.events_gen.set(query_gen);
    Some(Op::RefreshEvents {
        nest,
        secret_hex: app.events.secret_hex.clone(),
        mail: app.events.mail.clone()?,
        calendar_id,
        organizer_email: app.events.organizer_email.clone(),
        query_gen,
    })
}

/// Open the `event_detail` sub-page for `event_id` — purely local, the row
/// (summary/alarm/attendees) is already cached from the last list/create/
/// delete/mutate fetch. Resets the reminder-select draft to the first preset
/// (the `<DropDown>`'s natural default), independent of whether this event
/// currently has a reminder set.
pub fn open_event_detail(app: &mut App, event_id: String) {
    app.events.mode = Mode::EventDetail(event_id);
    app.events.reminder_draft = fauna_core::ical::reminder_presets()
        .first()
        .map(|o| o.value.clone())
        .unwrap_or_default();
    app.events.attendee_invite_email.clear();
}

/// An events (calendar) page editable field (`crate::events`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EventsField {
    /// `calendar-name` — the `create_calendar` sub-page's name buffer.
    CalendarName,
    /// `event-summary` — the `create_event` sub-page's summary buffer.
    Summary,
    /// `event-dtstart` — the combined start-datetime buffer (ui.yaml's
    /// `event-form-start-datetime` component: "all 7 apps implement a
    /// single combined date+time control with id event-dtstart").
    Dtstart,
    /// `event-dtend` — the combined end-datetime buffer (`event-form-end-datetime`).
    Dtend,
    /// `event-form-description` — the VEVENT `DESCRIPTION` buffer.
    Description,
    /// `event-form-location` — the VEVENT `LOCATION` buffer.
    Location,
    /// `attendee-invite-field` — the `event_detail` invite-form email buffer
    /// (the universal `mailto:` attendee mechanism).
    AttendeeInviteEmail,
    /// `calendar-import-file` — the typed `.ics` path to import.
    ImportPath,
}

/// Read an editable field — the create_calendar/create_event draft buffers
/// are local, committed only on submit (the profile-edit-form convention).
pub fn field(state: &EventsState, field: &EventsField) -> String {
    match field {
        EventsField::CalendarName => state.new_calendar_name.clone(),
        EventsField::Summary => state.new_event_summary.clone(),
        EventsField::Dtstart => state.new_event_dtstart.clone(),
        EventsField::Dtend => state.new_event_dtend.clone(),
        EventsField::Description => state.new_event_description.clone(),
        EventsField::Location => state.new_event_location.clone(),
        EventsField::AttendeeInviteEmail => state.attendee_invite_email.clone(),
        EventsField::ImportPath => state.import_path.clone(),
    }
}

/// Write an editable field (see [`field`]). Every variant is a local buffer,
/// so a write lands synchronously and returns no pending work.
///
/// The five `event-form` buffers additionally tick the `"events"` drafts rail
/// ([`EventsState::notify_drafts`]) — this is the ONE door compose text changes
/// through, so persistence cannot miss an edit. `new_calendar_name` and
/// `attendee_invite_email` deliberately do not: neither is part of the
/// `event-form` compose the rail persists (`events.md` § Persistence).
pub fn set_field(state: &mut EventsState, field: EventsField, value: String) {
    match field {
        EventsField::CalendarName => {
            state.new_calendar_name = value;
            return;
        }
        EventsField::AttendeeInviteEmail => {
            state.attendee_invite_email = value;
            return;
        }
        EventsField::ImportPath => {
            state.import_path = value;
            return;
        }
        EventsField::Summary => state.new_event_summary = value,
        EventsField::Dtstart => state.new_event_dtstart = value,
        EventsField::Dtend => state.new_event_dtend = value,
        EventsField::Description => state.new_event_description = value,
        EventsField::Location => state.new_event_location = value,
    }
    state.notify_drafts();
}

/// The network half. Every mutating variant refetches after its write — the
/// fresh event list IS the observable effect (the notifications/contacts
/// refetch-after-mutate convention).
pub enum Op {
    CreateCalendar {
        nest: Arc<NestClient>,
        secret_hex: SecretString,
        mail: Arc<dyn fauna_client_config::MailStore>,
        name: String,
    },
    RefreshEvents {
        nest: Arc<NestClient>,
        secret_hex: SecretString,
        mail: Arc<dyn fauna_client_config::MailStore>,
        calendar_id: String,
        organizer_email: String,
        /// The `events_gen` claimed at dispatch — carried through to
        /// [`Outcome::EventsLoaded`] so a stale reply (superseded by a newer
        /// `RefreshCalendars`/`RefreshEvents`) is dropped at commit, never
        /// overwriting fresher state (`events.md` § Implementation status
        /// today, the no-selection-union race).
        query_gen: u64,
    },
    /// The calendar-list + events hydration entering the Events tab implies
    /// (and `session::establish` fires once at login) — mirrors
    /// `contacts::spawn_refresh`'s "hydrate at login, refetch on every
    /// nav-to-tab" shape. Scopes events to `selected_calendar` when it's
    /// still a live calendar, else unions every owned calendar's events (the
    /// no-selection union rule, events.md § Implementation status today).
    RefreshCalendars {
        nest: Arc<NestClient>,
        /// The session's account-store handle, for the refused-change rows —
        /// `None` until the store is assembled (the list reads empty).
        account_store: Option<fauna_sync_engine::account_runtime::AccountStoreHandle>,
        secret_hex: SecretString,
        mail: Arc<dyn fauna_client_config::MailStore>,
        organizer_email: String,
        selected_calendar: Option<String>,
        /// See [`Op::RefreshEvents::query_gen`] — the same guard, the other
        /// direction: a slow union fetched at nav-entry must not clobber a
        /// faster single-calendar select that started later.
        query_gen: u64,
    },
    CreateEvent {
        nest: Arc<NestClient>,
        secret_hex: SecretString,
        mail: Arc<dyn fauna_client_config::MailStore>,
        calendar_id: String,
        organizer_email: String,
        summary: String,
        dtstart: String,
        dtend: String,
        description: String,
        location: String,
    },
    DeleteEvent {
        nest: Arc<NestClient>,
        secret_hex: SecretString,
        mail: Arc<dyn fauna_client_config::MailStore>,
        calendar_id: String,
        uid_hash_hex: String,
        organizer_email: String,
    },
    /// Read-mutate-rewrite RSVP (`fauna_client_caldav::apply_rsvp`), then a
    /// best-effort iMIP `REPLY` to a different organizer (events.md §
    /// Errors — best-effort, never blocks the local write), matching every
    /// other app (linux `client.rs::rsvp_event`, the FFI/wasm faces).
    RsvpEvent {
        nest: Arc<NestClient>,
        secret_hex: SecretString,
        mail: Arc<dyn fauna_client_config::MailStore>,
        calendar_id: String,
        uid_hash_hex: String,
        organizer_email: String,
        /// The typed answer — `apply_rsvp` re-validates the string form at
        /// the crate boundary, but nothing can construct an invalid one here.
        response: RsvpResponse,
    },
    /// Read-mutate-rewrite reminder set/clear (`fauna_client_caldav::set_reminder`);
    /// an empty `offset` clears the `VALARM` (mirrors linux's `remove_reminder`).
    SetReminder {
        nest: Arc<NestClient>,
        secret_hex: SecretString,
        mail: Arc<dyn fauna_client_config::MailStore>,
        calendar_id: String,
        uid_hash_hex: String,
        organizer_email: String,
        offset: String,
    },
    /// Add an attendee (when `attendee_email` is non-empty) and fan out an
    /// iMIP `REQUEST` to the roster — the combined "type an email → Invite"
    /// action (mirrors linux `client.rs::invite_to_event`). `real_session`
    /// backs the mailbox-less Fauna rail (`NestImipDispatch`); its absence
    /// degrades to email-only, same as linux's defensive fallback.
    InviteAttendee {
        nest: Arc<NestClient>,
        secret_hex: SecretString,
        mail: Arc<dyn fauna_client_config::MailStore>,
        calendar_id: String,
        uid_hash_hex: String,
        organizer_email: String,
        attendee_email: String,
        real_session: Option<Arc<ConversationsSession>>,
    },
    /// Dismiss one refused-change row — the owner's *I have seen this*
    /// (`inbound-scheduling-authority.md` § *Surfacing*). Writes the account
    /// plane's row through the shared door
    /// (`fauna_client_account_runtime::refused_changes::dismiss`) and re-reads
    /// the open rows.
    DismissRefusedChange {
        /// The session's handle as the app held it at the gesture.
        account_store: Option<fauna_sync_engine::account_runtime::AccountStoreHandle>,
        /// `fauna_core::data::RefusedSchedulingChange::key` of the row.
        key: String,
    },
    /// Read the `.ics` at `path` and import its VEVENTs into `calendar_id`
    /// through the shared `CalDavClient::import_ical_events`, then re-read
    /// the calendar so the imported events are what the page shows.
    ImportIcs {
        nest: Arc<NestClient>,
        secret_hex: SecretString,
        mail: Arc<dyn fauna_client_config::MailStore>,
        calendar_id: String,
        organizer_email: String,
        path: String,
    },
    /// Build the calendar's `.ics` with the shared
    /// `CalDavClient::export_calendar_ics` and write it into the downloads
    /// directory under a name taken from the calendar's own.
    ExportIcs {
        nest: Arc<NestClient>,
        secret_hex: SecretString,
        mail: Arc<dyn fauna_client_config::MailStore>,
        calendar_id: String,
        calendar_name: String,
    },
}

#[derive(Debug)]
pub enum Outcome {
    CalendarCreated(Vec<CalendarRow>),
    /// The [`Op::RefreshCalendars`] reply — the calendar list plus the
    /// scoped-or-unioned events (§ Where logic lives above).
    CalendarsAndEventsLoaded {
        calendars: Vec<CalendarRow>,
        events: Vec<EventRow>,
        /// The open refused-change rows, fetched in the same pass — see
        /// [`EventsState::refused_changes`] for why they ride the calendar
        /// hydration rather than a poll of their own.
        refused_changes: Vec<fauna_core::data::RefusedSchedulingChange>,
        query_gen: u64,
    },
    /// A `refused-change-dismiss` landed; carries the re-read open rows (the
    /// dismissed one is gone, and any row another device raised meanwhile is
    /// now here).
    RefusedChangesLoaded(Vec<fauna_core::data::RefusedSchedulingChange>),
    EventsLoaded {
        calendar_id: String,
        events: Vec<EventRow>,
        query_gen: u64,
    },
    EventCreated {
        calendar_id: String,
        events: Vec<EventRow>,
    },
    EventDeleted {
        calendar_id: String,
        events: Vec<EventRow>,
    },
    /// An RSVP, reminder, or invite mutation landed — the fresh event list,
    /// but (unlike `EventCreated`/`EventDeleted`) `event_detail` stays open
    /// on the same event so the updated roster/reminder is visible
    /// immediately. `error` carries a best-effort dispatch failure (events.md
    /// § Errors: surfaced, but never blocks the already-persisted write) —
    /// `None` on a clean mutation.
    EventMutated {
        calendar_id: String,
        events: Vec<EventRow>,
        error: Option<String>,
    },
    /// The `"events"` drafts rail's launch load resolved to a non-empty draft
    /// (`events.md` § Persistence). Boxed because it is much larger than the
    /// other variants and would otherwise widen every `Outcome` moved on this
    /// page's channel.
    ///
    /// `session_generation` is the identity seam the restore was launched
    /// under ([`crate::events::drafts::restore_on_launch`]): the load is a nest
    /// round-trip nothing cancels, so it can resolve after the user has already
    /// switched accounts, and this page's channel is process-wide.
    DraftsLoaded {
        draft: Box<fauna_client_caldav::drafts::EventDrafts>,
        session_generation: u64,
    },
    /// An `.ics` import landed: the counts, and the calendar re-read.
    IcsImported {
        calendar_id: String,
        events: Vec<EventRow>,
        result: fauna_client_caldav::IcsImportOutcome,
    },
    /// An `.ics` export was written to `path`.
    IcsExported {
        path: String,
    },
    Failed(String),
}

impl Op {
    pub async fn run(self) -> Outcome {
        match self {
            Op::CreateCalendar {
                nest,
                secret_hex,
                mail,
                name,
            } => {
                let Some(DavStoreContext {
                    actor_id,
                    msek,
                    prior_mseks,
                }) = caldav_context(mail.as_ref(), &secret_hex).await
                else {
                    return Outcome::Failed(errors::CALENDAR_REQUIRES_MAIL.to_string());
                };
                let metadata = match seal_calendar_metadata(
                    &CalendarMetadata {
                        displayname: name,
                        color: "#3273dc".to_string(),
                        description: String::new(),
                        ..Default::default()
                    },
                    &msek,
                ) {
                    Ok(m) => m,
                    Err(e) => return Outcome::Failed(format!("seal calendar metadata: {e}")),
                };
                let calendar_id = uid_hash(&format!("{}@fauna-tui", uuid::Uuid::new_v4()));
                let client = CalDavClient::new(Arc::clone(&nest));
                if let Err(e) = client
                    .provision_calendar(ProvisionCalendarRequest {
                        actor_id: actor_id.to_vec(),
                        calendar_id: calendar_id.to_vec(),
                        encrypted_metadata: metadata,
                        ..Default::default()
                    })
                    .await
                {
                    return Outcome::Failed(format!("provision_calendar: {e}"));
                }
                match list_calendars_rows(&nest, &actor_id, &msek, &prior_mseks).await {
                    Ok(rows) => Outcome::CalendarCreated(rows),
                    Err(e) => Outcome::Failed(e),
                }
            }
            Op::RefreshCalendars {
                nest,
                account_store,
                secret_hex,
                mail,
                organizer_email,
                selected_calendar,
                query_gen,
            } => {
                // Mail/CalDAV off ⇒ no calendar store. The refused-change
                // rows are fetched anyway: they are the account plane's, not
                // the calendar store's, and a refusal recorded while CalDAV
                // was on is still the user's to read afterwards.
                let refused_changes = fauna_client_account_runtime::refused_changes::load_open(
                    account_store.as_ref(),
                )
                .await;
                let Some(DavStoreContext {
                    actor_id,
                    msek,
                    prior_mseks,
                }) = caldav_context(mail.as_ref(), &secret_hex).await
                else {
                    return Outcome::CalendarsAndEventsLoaded {
                        calendars: vec![],
                        events: vec![],
                        refused_changes,
                        query_gen,
                    };
                };
                let calendars =
                    match list_calendars_rows(&nest, &actor_id, &msek, &prior_mseks).await {
                        Ok(rows) => rows,
                        Err(e) => return Outcome::Failed(e),
                    };
                let mut events = Vec::new();
                for cal in calendars_in_scope(&calendars, &selected_calendar) {
                    match fetch_events_rows(
                        &nest,
                        &actor_id,
                        &cal.id,
                        &msek,
                        &prior_mseks,
                        &organizer_email,
                    )
                    .await
                    {
                        Ok(rows) => events.extend(rows),
                        Err(e) => return Outcome::Failed(e),
                    }
                }
                Outcome::CalendarsAndEventsLoaded {
                    calendars,
                    events,
                    refused_changes,
                    query_gen,
                }
            }
            Op::ImportIcs {
                nest,
                secret_hex,
                mail,
                calendar_id,
                organizer_email,
                path,
            } => {
                let Some(cal_id) = hex32(&calendar_id) else {
                    return Outcome::Failed(et::SELECT_CALENDAR.to_string());
                };
                let ics_text = match tokio::fs::read_to_string(&path).await {
                    Ok(text) => text,
                    Err(e) => return Outcome::Failed(format!("{path}: {e}")),
                };
                let Some(DavStoreContext {
                    actor_id,
                    msek,
                    prior_mseks,
                }) = caldav_context(mail.as_ref(), &secret_hex).await
                else {
                    return Outcome::Failed(errors::CALENDAR_REQUIRES_MAIL.to_string());
                };
                let result = CalDavClient::new(Arc::clone(&nest))
                    .import_ical_events(
                        &actor_id,
                        &cal_id,
                        &msek,
                        &organizer_email,
                        &ics_text,
                        now_secs(),
                        |_| format!("{}@fauna-tui", uuid::Uuid::new_v4()),
                    )
                    .await;
                match fetch_events_rows(
                    &nest,
                    &actor_id,
                    &calendar_id,
                    &msek,
                    &prior_mseks,
                    &organizer_email,
                )
                .await
                {
                    Ok(events) => Outcome::IcsImported {
                        calendar_id,
                        events,
                        result,
                    },
                    Err(e) => Outcome::Failed(e),
                }
            }
            Op::ExportIcs {
                nest,
                secret_hex,
                mail,
                calendar_id,
                calendar_name,
            } => {
                let Some(cal_id) = hex32(&calendar_id) else {
                    return Outcome::Failed(et::SELECT_CALENDAR.to_string());
                };
                let Some(DavStoreContext {
                    actor_id,
                    msek,
                    prior_mseks,
                }) = caldav_context(mail.as_ref(), &secret_hex).await
                else {
                    return Outcome::Failed(errors::CALENDAR_REQUIRES_MAIL.to_string());
                };
                let ics = match CalDavClient::new(Arc::clone(&nest))
                    .export_calendar_ics(
                        &actor_id,
                        &cal_id,
                        &DavRecipientKeys::from_mseks(&msek, &prior_mseks),
                    )
                    .await
                {
                    Ok(ics) => ics,
                    Err(e) => return Outcome::Failed(format!("query_events: {e}")),
                };
                // No save dialog on a terminal: the file lands where every
                // other file this app hands back does (`backups::download_dir`).
                let Some(dir) = crate::backups::download_dir() else {
                    return Outcome::Failed(
                        "no downloads directory to save the calendar into".to_string(),
                    );
                };
                if let Err(e) = tokio::fs::create_dir_all(&dir).await {
                    return Outcome::Failed(format!("{}: {e}", dir.display()));
                }
                let path = unused_export_path(&dir, &calendar_name);
                match tokio::fs::write(&path, ics.as_bytes()).await {
                    Ok(()) => Outcome::IcsExported {
                        path: path.display().to_string(),
                    },
                    Err(e) => Outcome::Failed(format!("{}: {e}", path.display())),
                }
            }
            Op::DismissRefusedChange { account_store, key } => {
                use fauna_client_account_runtime::refused_changes::{dismiss, load_open};
                if let Err(e) = dismiss(account_store.as_ref(), &key).await {
                    return Outcome::Failed(format!("dismiss refused change: {e}"));
                }
                Outcome::RefusedChangesLoaded(load_open(account_store.as_ref()).await)
            }
            Op::RefreshEvents {
                nest,
                secret_hex,
                mail,
                calendar_id,
                organizer_email,
                query_gen,
            } => {
                let Some(DavStoreContext {
                    actor_id,
                    msek,
                    prior_mseks,
                }) = caldav_context(mail.as_ref(), &secret_hex).await
                else {
                    return Outcome::EventsLoaded {
                        calendar_id,
                        events: vec![],
                        query_gen,
                    };
                };
                match fetch_events_rows(
                    &nest,
                    &actor_id,
                    &calendar_id,
                    &msek,
                    &prior_mseks,
                    &organizer_email,
                )
                .await
                {
                    Ok(events) => Outcome::EventsLoaded {
                        calendar_id,
                        events,
                        query_gen,
                    },
                    Err(e) => Outcome::Failed(e),
                }
            }
            Op::CreateEvent {
                nest,
                secret_hex,
                mail,
                calendar_id,
                organizer_email,
                summary,
                dtstart,
                dtend,
                description,
                location,
            } => {
                let Some(cal_id) = hex32(&calendar_id) else {
                    return Outcome::Failed(et::SELECT_CALENDAR.to_string());
                };
                let Some(DavStoreContext {
                    actor_id,
                    msek,
                    prior_mseks,
                }) = caldav_context(mail.as_ref(), &secret_hex).await
                else {
                    return Outcome::Failed(errors::CALENDAR_REQUIRES_MAIL.to_string());
                };
                let uid = format!("{}@fauna-tui", uuid::Uuid::new_v4());
                let fields = EventFields {
                    summary,
                    dtstart,
                    dtend,
                    // `vevent_lines` omits an empty LOCATION/DESCRIPTION
                    // outright, so a blank field writes no property rather than
                    // an empty one — the shape a CalDAV MUA reading the same
                    // VEVENT expects.
                    location,
                    description,
                    uid: uid.clone(),
                    status: "confirmed".to_string(),
                    ..Default::default()
                };
                let client = CalDavClient::new(Arc::clone(&nest));
                if let Err(e) = client
                    .seal_and_put_event(
                        &actor_id,
                        &cal_id,
                        &uid_hash(&uid),
                        &msek,
                        &fields,
                        &[],
                        &organizer_email,
                        None,
                        now_secs(),
                        None,
                    )
                    .await
                {
                    return Outcome::Failed(format!("put_event_ciphertext: {e}"));
                }
                match fetch_events_rows(
                    &nest,
                    &actor_id,
                    &calendar_id,
                    &msek,
                    &prior_mseks,
                    &organizer_email,
                )
                .await
                {
                    Ok(events) => Outcome::EventCreated {
                        calendar_id,
                        events,
                    },
                    Err(e) => Outcome::Failed(e),
                }
            }
            Op::DeleteEvent {
                nest,
                secret_hex,
                mail,
                calendar_id,
                uid_hash_hex,
                organizer_email,
            } => {
                let (Some(cal_id), Some(uh)) = (hex32(&calendar_id), hex32(&uid_hash_hex)) else {
                    return Outcome::Failed(et::SELECT_CALENDAR.to_string());
                };
                let Some(DavStoreContext {
                    actor_id,
                    msek,
                    prior_mseks,
                }) = caldav_context(mail.as_ref(), &secret_hex).await
                else {
                    return Outcome::Failed(errors::CALENDAR_REQUIRES_MAIL.to_string());
                };
                let client = CalDavClient::new(Arc::clone(&nest));
                if let Err(e) = client
                    .delete_event(DeleteEventRequest {
                        actor_id: actor_id.to_vec(),
                        calendar_id: cal_id.to_vec(),
                        uid_hash: uh.to_vec(),
                        if_match: None,
                    })
                    .await
                {
                    return Outcome::Failed(format!("delete_event: {e}"));
                }
                match fetch_events_rows(
                    &nest,
                    &actor_id,
                    &calendar_id,
                    &msek,
                    &prior_mseks,
                    &organizer_email,
                )
                .await
                {
                    Ok(events) => Outcome::EventDeleted {
                        calendar_id,
                        events,
                    },
                    Err(e) => Outcome::Failed(e),
                }
            }
            Op::RsvpEvent {
                nest,
                secret_hex,
                mail,
                calendar_id,
                uid_hash_hex,
                organizer_email,
                response,
            } => {
                let Some(DavStoreContext {
                    actor_id,
                    msek,
                    prior_mseks,
                }) = caldav_context(mail.as_ref(), &secret_hex).await
                else {
                    return Outcome::Failed(errors::CALENDAR_REQUIRES_MAIL.to_string());
                };
                let target = match fetch_decoded_event(
                    &nest,
                    &actor_id,
                    &calendar_id,
                    &msek,
                    &prior_mseks,
                    &uid_hash_hex,
                )
                .await
                {
                    Ok(d) => d,
                    Err(e) => return Outcome::Failed(e),
                };
                let rw = match apply_rsvp(
                    &target.ics,
                    target.fauna_ext.as_ref(),
                    &organizer_email,
                    response.as_str(),
                ) {
                    Ok(rw) => rw,
                    Err(e) => return Outcome::Failed(e),
                };
                if let Err(e) = put_rewrite(&nest, &actor_id, &calendar_id, &msek, &rw).await {
                    return Outcome::Failed(e);
                }
                // Best-effort iMIP REPLY to a different organizer (events.md §
                // Errors) — the same shared dispatch linux/windows/macOS/iOS
                // use. Never surfaced: the RSVP already persisted above.
                if let Some(reply) = imip_reply_for_rsvp(&rw, &organizer_email, now_secs()) {
                    let _ = EmailClient::new(Arc::clone(&nest))
                        .send(reply.recipients, reply.raw_rfc5322)
                        .await;
                }
                match fetch_events_rows(
                    &nest,
                    &actor_id,
                    &calendar_id,
                    &msek,
                    &prior_mseks,
                    &organizer_email,
                )
                .await
                {
                    Ok(events) => Outcome::EventMutated {
                        calendar_id,
                        events,
                        error: None,
                    },
                    Err(e) => Outcome::Failed(e),
                }
            }
            Op::SetReminder {
                nest,
                secret_hex,
                mail,
                calendar_id,
                uid_hash_hex,
                organizer_email,
                offset,
            } => {
                let Some(DavStoreContext {
                    actor_id,
                    msek,
                    prior_mseks,
                }) = caldav_context(mail.as_ref(), &secret_hex).await
                else {
                    return Outcome::Failed(errors::CALENDAR_REQUIRES_MAIL.to_string());
                };
                let target = match fetch_decoded_event(
                    &nest,
                    &actor_id,
                    &calendar_id,
                    &msek,
                    &prior_mseks,
                    &uid_hash_hex,
                )
                .await
                {
                    Ok(d) => d,
                    Err(e) => return Outcome::Failed(e),
                };
                let rw = match set_reminder(&target.ics, target.fauna_ext.as_ref(), &offset) {
                    Ok(rw) => rw,
                    Err(e) => return Outcome::Failed(e),
                };
                if let Err(e) = put_rewrite(&nest, &actor_id, &calendar_id, &msek, &rw).await {
                    return Outcome::Failed(e);
                }
                match fetch_events_rows(
                    &nest,
                    &actor_id,
                    &calendar_id,
                    &msek,
                    &prior_mseks,
                    &organizer_email,
                )
                .await
                {
                    Ok(events) => Outcome::EventMutated {
                        calendar_id,
                        events,
                        error: None,
                    },
                    Err(e) => Outcome::Failed(e),
                }
            }
            Op::InviteAttendee {
                nest,
                secret_hex,
                mail,
                calendar_id,
                uid_hash_hex,
                organizer_email,
                attendee_email,
                real_session,
            } => {
                let Some(DavStoreContext {
                    actor_id,
                    msek,
                    prior_mseks,
                }) = caldav_context(mail.as_ref(), &secret_hex).await
                else {
                    return Outcome::Failed(errors::CALENDAR_REQUIRES_MAIL.to_string());
                };
                let target = match fetch_decoded_event(
                    &nest,
                    &actor_id,
                    &calendar_id,
                    &msek,
                    &prior_mseks,
                    &uid_hash_hex,
                )
                .await
                {
                    Ok(d) => d,
                    Err(e) => return Outcome::Failed(e),
                };
                let attendee_email = attendee_email.trim().to_string();
                // Add to the roster + re-PUT FIRST (events.md § Errors — the
                // roster persists even if the best-effort dispatch below
                // fails), unless the field was left empty (re-send to the
                // existing roster, mirroring linux's empty-field re-send).
                let (fields, roster, organizer) = if attendee_email.is_empty() {
                    match fauna_client_caldav::imip_inputs(&target.ics, &organizer_email) {
                        Ok(parts) => parts,
                        Err(e) => return Outcome::Failed(e),
                    }
                } else {
                    let rw = match add_attendee(
                        &target.ics,
                        target.fauna_ext.as_ref(),
                        &organizer_email,
                        &attendee_email,
                    ) {
                        Ok(rw) => rw,
                        Err(e) => return Outcome::Failed(e),
                    };
                    if let Err(e) = put_rewrite(&nest, &actor_id, &calendar_id, &msek, &rw).await {
                        return Outcome::Failed(e);
                    }
                    (rw.fields, rw.attendees, rw.organizer_email)
                };
                // The shared organizer dispatch fork (caldav-server.md §
                // Scheduling & invitations): resolve each attendee's
                // transport and route email-reachable → the bridge MTA,
                // mailbox-less Fauna → the WS-RPC sealed MLS rail via the
                // real conversations session. Absent it (defensive — a
                // calendar implies a logged-in session), degrade to
                // email-only, mirroring linux's fallback.
                let dispatch_error = match real_session {
                    Some(session) => {
                        let dispatch = NestImipDispatch::new(Arc::clone(&nest), session);
                        match dispatch_imip_request(
                            &AnonAttendeeDiscovery,
                            &dispatch,
                            &fields,
                            &roster,
                            &organizer,
                            now_secs(),
                        )
                        .await
                        {
                            Ok(report) => report.errors.into_iter().next(),
                            Err(e) => Some(e.to_string()),
                        }
                    }
                    None => match imip_request_for_invite(&fields, &roster, &organizer, now_secs())
                    {
                        None => None,
                        Some(message) => EmailClient::new(Arc::clone(&nest))
                            .send(message.recipients, message.raw_rfc5322)
                            .await
                            .err()
                            .map(|e| e.to_string()),
                    },
                };
                match fetch_events_rows(
                    &nest,
                    &actor_id,
                    &calendar_id,
                    &msek,
                    &prior_mseks,
                    &organizer_email,
                )
                .await
                {
                    Ok(events) => Outcome::EventMutated {
                        calendar_id,
                        events,
                        error: dispatch_error,
                    },
                    Err(e) => Outcome::Failed(e),
                }
            }
        }
    }
}

/// Fold an [`Outcome`] back into the page. A failure lands on `error-message`;
/// a load/mutate clears it (the page shows fresh truth), matching
/// `notifications::apply_outcome`.
pub fn apply_outcome(app: &mut App, outcome: Outcome) {
    match outcome {
        Outcome::CalendarCreated(rows) => {
            // Seed here too, not just on the load path: a calendar the user
            // just created must arrive with its box CHECKED. Without this it
            // would render unchecked the moment any box was already checked
            // (the normal state), so the user's brand-new calendar would be
            // filtered out of the union they are looking at.
            seed_visible_calendars(
                &mut app.events.visible_calendars,
                &app.events.calendars,
                &rows,
            );
            app.events.calendars = rows;
            app.events.mode = Mode::List;
            app.errors.remove(&Page::Events);
        }
        // Both variants are dropped outright when a fresher fetch (claimed a
        // later `events_gen`) has since superseded them — a slow
        // `RefreshCalendars` union landing after a faster `RefreshEvents`
        // select (or the reverse) must never clobber the newer result
        // (events.md § Implementation status today, the no-selection-union
        // race apple/android/web already guard against the same way).
        Outcome::CalendarsAndEventsLoaded {
            calendars,
            events,
            refused_changes,
            query_gen,
        } => {
            if query_gen == app.events.events_gen.get() {
                seed_visible_calendars(
                    &mut app.events.visible_calendars,
                    &app.events.calendars,
                    &calendars,
                );
                app.events.calendars = calendars;
                app.events.events_cache = events;
                app.events.refused_changes = refused_changes;
                app.errors.remove(&Page::Events);
            }
        }
        Outcome::RefusedChangesLoaded(rows) => {
            // No `query_gen` guard: the rows are not the calendar/event race's
            // subject (that token guards a union against a single-calendar
            // select — neither of which touches this list), and a dismissal's
            // re-read is always the freshest answer there is.
            app.events.refused_changes = rows;
        }
        Outcome::EventsLoaded {
            calendar_id,
            events,
            query_gen,
        } => {
            if query_gen == app.events.events_gen.get()
                && app.events.selected_calendar.as_deref() == Some(calendar_id.as_str())
            {
                app.events.events_cache = events;
                app.errors.remove(&Page::Events);
            }
        }
        Outcome::EventCreated {
            calendar_id,
            events,
        } => {
            app.events.events_cache = events;
            app.events.selected_calendar = Some(calendar_id);
            app.events.mode = Mode::List;
            // The event exists now, so it is no longer a draft: empty the
            // buffers and tick the rail. Before the rail existed, the reset was
            // the next `ShowCreateEvent` — that opener now resumes instead, so
            // this is where a created event stops being in-progress. Without
            // it, relaunching would restore the text of an event already on the
            // calendar (`events.md` § Persistence).
            app.events.clear_event_compose();
            app.errors.remove(&Page::Events);
        }
        Outcome::EventDeleted {
            calendar_id,
            events,
        } => {
            app.events.events_cache = events;
            app.events.selected_calendar = Some(calendar_id);
            app.events.mode = Mode::List;
            app.errors.remove(&Page::Events);
        }
        // The launch restore (`drafts::restore_on_launch`) — the user's
        // half-written event, from this device or another. It lands before the
        // form is opened, so it never overwrites text being typed.
        //
        // ⚠ Only if it is still THIS session's restore. `session_generation`
        // bumps at every teardown (`App::begin_identity_teardown`), and
        // `session::establish` installs a fresh `EventsState` for the incoming
        // actor — so a load that outlived the switch would paint the DEPARTING
        // actor's half-written event into the next actor's compose buffers,
        // which their first keystroke then autosaves under their own
        // `BackupKey` (`account-scoping.md` § The scoping taxonomy → the
        // in-memory corollary; ).
        Outcome::DraftsLoaded {
            draft,
            session_generation,
        } => {
            if session_generation == app.session_generation {
                app.events.apply_event_draft(*draft)
            } else {
                tracing::debug!(
                    "event drafts: dropping a restore from session generation {session_generation} \
                     (now {})",
                    app.session_generation
                );
            }
        }
        // Unlike Create/Delete, stays on `event_detail` — the mutated event
        // (fresh roster/reminder) is exactly what the panel should now show.
        // `error` (a best-effort dispatch failure) surfaces alongside the
        // fresh roster — it never blocks the already-persisted write.
        Outcome::EventMutated {
            calendar_id,
            events,
            error,
        } => {
            app.events.events_cache = events;
            app.events.selected_calendar = Some(calendar_id);
            match error {
                Some(message) => {
                    app.errors.insert(Page::Events, message);
                }
                None => {
                    app.errors.remove(&Page::Events);
                }
            }
        }
        Outcome::IcsImported {
            calendar_id,
            events,
            result,
        } => {
            // The counts are said whatever calendar is on screen now; the
            // re-read only lands if the user is still looking at the target.
            if app.events.selected_calendar.as_deref() == Some(calendar_id.as_str()) {
                app.events.events_cache = events;
            }
            app.events.import_path.clear();
            app.events.ics_notice = et::IMPORT_RESULT
                .replace("{imported}", &result.imported.to_string())
                .replace("{skipped}", &result.skipped.to_string())
                .replace("{total}", &result.total.to_string());
            app.errors.remove(&Page::Events);
        }
        Outcome::IcsExported { path } => {
            app.events.ics_notice = et::CALENDAR_EXPORTED.replace("{path}", &path);
            app.errors.remove(&Page::Events);
        }
        Outcome::Failed(message) => {
            app.errors.insert(Page::Events, message);
        }
    }
}

/// Where an export of `calendar_name` lands in `dir`: the calendar's own name
/// made safe as a file name, `.ics`, and a ` (2)`, ` (3)`, … suffix rather
/// than overwriting a file already there — a second export of the same
/// calendar is a new file, the way a browser download is. Pure but for the
/// existence probe.
fn unused_export_path(dir: &std::path::Path, calendar_name: &str) -> std::path::PathBuf {
    let stem = ics_file_stem(calendar_name);
    let first = dir.join(format!("{stem}.ics"));
    if !first.exists() {
        return first;
    }
    (2u32..)
        .map(|n| dir.join(format!("{stem} ({n}).ics")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// A calendar name as a file-name stem: path separators and control or
/// reserved characters become `_`, and an empty result falls back to
/// `calendar`.
fn ics_file_stem(calendar_name: &str) -> String {
    let stem: String = calendar_name
        .trim()
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let stem = stem.trim_matches('.').trim();
    if stem.is_empty() {
        "calendar".to_string()
    } else {
        stem.to_string()
    }
}

/// The no-selection union rule (events.md § Implementation status today,
/// mirroring windows' `QueryEventItemsAsync` / apple's `queryEventItems`):
/// scope to `selected` when it still names a live calendar, else every
/// owned calendar is in scope. A stale selection (its calendar deleted
/// elsewhere) falls back to the union rather than silently scoping to
/// nothing. Pure — no network, unit-tested directly.
fn calendars_in_scope<'a>(
    calendars: &'a [CalendarRow],
    selected: &Option<String>,
) -> Vec<&'a CalendarRow> {
    // The live-vs-stale selection rule itself is shared Rust
    // (`events.md` § User actions) — linux resolves the same way, so a
    // calendar deleted out from under a selection reverts to the union on
    // both rather than blanking the page.
    match fauna_client_caldav::resolve_calendar_selection(
        selected.as_deref(),
        calendars.iter().map(|c| &c.id),
    ) {
        Some(id) => calendars.iter().filter(|c| c.id == id).collect(),
        None => calendars.iter().collect(),
    }
}

// ---------------------------------------------------------------------------
// CalDAV op helpers — ported from `apps/fauna-linux/src/client.rs`
// (`caldav_context` / `fetch_calendars` / `query_events_in`).
// ---------------------------------------------------------------------------

/// The actor's identity + mail-sealing key — the two inputs every encrypted
/// CalDAV op needs. `None` when the secret is malformed, the mail custody
/// read fails, or mail/CalDAV is not enabled yet (no MSEK minted —
/// `MailSettingsMachine::enable_mail`). Thin wrapper over the shared
/// [`fauna_client_config::dav_store_context`] (also linux's `caldav_context`
/// and the FFI face's `dav_store_context`) — this seam supplies only what the
/// tui shell knows: the actor id from the connection secret, and the
/// session's mail custody.
async fn caldav_context(
    mail: &dyn fauna_client_config::MailStore,
    secret_hex: &str,
) -> Option<DavStoreContext> {
    let actor_id = ActorKeypair::from_secret_hex(secret_hex).ok()?.actor_id().0;
    dav_store_context(mail, actor_id).await
}

async fn list_calendars_rows(
    nest: &Arc<NestClient>,
    actor_id: &[u8; 32],
    msek: &[u8; 32],
    prior_mseks: &[[u8; 32]],
) -> Result<Vec<CalendarRow>, String> {
    let client = CalDavClient::new(Arc::clone(nest));
    let reply = client
        .list_calendars(ListCalendarsRequest {
            actor_id: actor_id.to_vec(),
        })
        .await
        .map_err(|e| format!("list_calendars: {e}"))?;
    // Derived once for the whole list — every row's metadata reuses it instead
    // of paying its own X-Wing keygen .
    let keys = DavRecipientKeys::from_mseks(msek, prior_mseks);
    Ok(reply
        .calendars
        .iter()
        .filter_map(|e| calendar_row_from_entry(e, &keys))
        .collect())
}

fn calendar_row_from_entry(
    entry: &fauna_client_caldav::bridge_routing::CalendarEntry,
    keys: &DavRecipientKeys,
) -> Option<CalendarRow> {
    match unseal_calendar_metadata(&entry.encrypted_metadata, keys) {
        Ok(meta) => Some(CalendarRow {
            id: fauna_core::format::hex_full(&entry.calendar_id),
            name: meta.displayname,
        }),
        Err(e) => {
            tracing::error!(
                "events::calendar_row_from_entry: decode (cal_id={}) failed: {e}",
                fauna_core::format::hex_full(&entry.calendar_id)
            );
            None
        }
    }
}

async fn fetch_events_rows(
    nest: &Arc<NestClient>,
    actor_id: &[u8; 32],
    calendar_id_hex: &str,
    msek: &[u8; 32],
    prior_mseks: &[[u8; 32]],
    self_email: &str,
) -> Result<Vec<EventRow>, String> {
    let Some(cal_id) = hex32(calendar_id_hex) else {
        return Ok(vec![]);
    };
    let client = CalDavClient::new(Arc::clone(nest));
    let page = client
        .query_events_decoded(
            QueryEventsRequest {
                actor_id: actor_id.to_vec(),
                calendar_id: cal_id.to_vec(),
                since_modseq: None,
                after_event_id: None,
                limit: 0,
            },
            &DavRecipientKeys::from_mseks(msek, prior_mseks),
        )
        .await
        .map_err(|e| format!("query_events: {e}"))?;
    let decoded = match page {
        DecodedEventsPage::Ok { events, .. } => events,
        DecodedEventsPage::CalendarNotFound => vec![],
    };
    Ok(decoded
        .iter()
        .filter_map(|d| event_row_from_decoded(d, calendar_id_hex, self_email))
        .collect())
}

/// Fetch the raw [`DecodedEvent`] (ics + sidecar) a read-mutate-rewrite op
/// needs — RSVP/reminder mutate the canonical VEVENT text directly, unlike
/// [`fetch_events_rows`]'s already-parsed [`EventRow`]s.
async fn fetch_decoded_event(
    nest: &Arc<NestClient>,
    actor_id: &[u8; 32],
    calendar_id_hex: &str,
    msek: &[u8; 32],
    prior_mseks: &[[u8; 32]],
    uid_hash_hex: &str,
) -> Result<DecodedEvent, String> {
    let cal_id = hex32(calendar_id_hex).ok_or_else(|| et::SELECT_CALENDAR.to_string())?;
    let client = CalDavClient::new(Arc::clone(nest));
    let page = client
        .query_events_decoded(
            QueryEventsRequest {
                actor_id: actor_id.to_vec(),
                calendar_id: cal_id.to_vec(),
                since_modseq: None,
                after_event_id: None,
                limit: 0,
            },
            &DavRecipientKeys::from_mseks(msek, prior_mseks),
        )
        .await
        .map_err(|e| format!("query_events: {e}"))?;
    let events = match page {
        DecodedEventsPage::Ok { events, .. } => events,
        DecodedEventsPage::CalendarNotFound => vec![],
    };
    events
        .into_iter()
        .find(|d| fauna_core::format::hex_full(&d.uid_hash) == uid_hash_hex)
        .ok_or_else(|| et::EVENT_NOT_FOUND.to_string())
}

/// Re-PUT an [`fauna_client_caldav::EventRewrite`] (the RSVP/reminder
/// read-mutate-rewrite result) — the shared tail of `Op::RsvpEvent` /
/// `Op::SetReminder`.
async fn put_rewrite(
    nest: &Arc<NestClient>,
    actor_id: &[u8; 32],
    calendar_id_hex: &str,
    msek: &[u8; 32],
    rw: &fauna_client_caldav::EventRewrite,
) -> Result<(), String> {
    let cal_id = hex32(calendar_id_hex).ok_or_else(|| et::SELECT_CALENDAR.to_string())?;
    let client = CalDavClient::new(Arc::clone(nest));
    client
        .seal_and_put_event(
            actor_id,
            &cal_id,
            &uid_hash(&rw.fields.uid),
            msek,
            &rw.fields,
            &rw.attendees,
            &rw.organizer_email,
            rw.fauna_ext.as_ref(),
            now_secs(),
            None,
        )
        .await
        .map(|_| ())
        .map_err(|e| format!("put_event_ciphertext: {e}"))
}

/// Parse the flat fields + the RSVP-projected roster (events.md § Attendee
/// list presentation: `project_attendee_rsvp`, the asymmetric
/// interested↔TENTATIVE render rule) and derive the viewer's own status —
/// everything `event_detail` needs, cached from this one fetch (no separate
/// round trip on open, like `fetch_attendees`/`get_reminder` on other apps).
fn event_row_from_decoded(
    decoded: &DecodedEvent,
    calendar_id_hex: &str,
    self_email: &str,
) -> Option<EventRow> {
    match parse_ical(&decoded.ics) {
        Ok(fields) => {
            let attendees: Vec<AttendeeRow> = parse_ical_attendees(&decoded.ics)
                .into_iter()
                .map(|a| AttendeeRow {
                    rsvp: project_attendee_rsvp(&a.partstat, &a.email, decoded.fauna_ext.as_ref())
                        .to_string(),
                    name: a.name,
                    email: a.email,
                })
                .collect();
            let self_rsvp = attendees
                .iter()
                .find(|a| !self_email.is_empty() && a.email.eq_ignore_ascii_case(self_email))
                .map(|a| a.rsvp.clone())
                .unwrap_or_default();
            Some(EventRow {
                id: fauna_core::format::hex_full(&decoded.uid_hash),
                calendar_id: calendar_id_hex.to_string(),
                summary: fields.summary,
                start: fields.dtstart,
                end: fields.dtend,
                alarm: fields.alarm,
                location: fields.location,
                description: fields.description,
                attendees,
                self_rsvp,
            })
        }
        Err(e) => {
            tracing::error!("events::event_row_from_decoded: parse VEVENT failed: {e}");
            None
        }
    }
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    fauna_core::hex32::decode(s).ok()
}

fn now_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs_or_zero()
}

// ---------------------------------------------------------------------------
// Paint
// ---------------------------------------------------------------------------
// The month/week/day view-rendering block lives in `grids.rs`; the agenda
// event-card list in `agenda.rs`; the `event_detail` sub-page in `detail.rs`.

/// The `refused-change-list` — someone tried to change an event on this
/// calendar and the shared apply refused them (`caldav-server.md` § Who may
/// mutate an existing event over the inbound rail → *Surfacing*).
///
/// **Painted only when there is something to report**, which is the ordinary
/// account's permanent state: an empty list paints no heading and no ids at
/// all, so the page a user sees every day is unchanged by this feature.
///
/// Every row's wording is shared Rust (`fauna_client_caldav::refused_change_view`)
/// — a security notice is exactly the text that must not drift between apps —
/// and the only per-app parts are the handle this device can resolve and the
/// date format a terminal can render.
fn render_refused_changes(app: &App, out: &mut Vec<Element>) {
    let st = &app.events;
    if st.refused_changes.is_empty() {
        return;
    }
    out.push(Element::label(
        ids::REFUSED_CHANGE_LIST,
        et::refused_changes::TITLE,
    ));
    for row in &st.refused_changes {
        // The sender by name where this device knows them, exactly as the
        // member-review surface resolves a flagged person: device-local
        // knowledge, re-derived at paint, never cached into the record. Only
        // a CANDIDATE — the shared view decides whether the nest that
        // attested this author may lend it the name (the principal is the
        // pair), so a foreign nest cannot borrow a real contact's handle.
        let handle = row.author.as_deref().and_then(|actor| {
            let id = fauna_core::identity::ActorId::from_hex(actor).ok()?;
            app.conversations
                .manager
                .as_ref()
                .and_then(|m| m.handle_for_person(&id))
        });
        let view = fauna_client_caldav::refused_change_view(row, handle.as_deref());
        out.push(Element::label(ids::REFUSED_CHANGE_ITEM, String::new()));
        out.push(Element::label(
            ids::REFUSED_CHANGE_TITLE,
            view.headline.resolve(fauna_i18n::strings::lookup),
        ));
        out.push(Element::label(
            ids::REFUSED_CHANGE_AUTHOR,
            view.author.resolve(fauna_i18n::strings::lookup),
        ));
        // The count rides the reason line rather than taking an id of its own:
        // "they are not the organizer" and "tried 47 times" are one thought,
        // and ui.yaml gives the row four texts, not five.
        let reason = match &view.attempts {
            Some(attempts) => format!(
                "{} {}",
                view.reason.resolve(fauna_i18n::strings::lookup),
                attempts.resolve(fauna_i18n::strings::lookup)
            ),
            None => view.reason.resolve(fauna_i18n::strings::lookup),
        };
        out.push(Element::label(ids::REFUSED_CHANGE_REASON, reason));
        out.push(Element::label(
            ids::REFUSED_CHANGE_TIME,
            crate::format::epoch_secs_date(view.last_refused_at.max(0) as u64),
        ));
        out.push(Element::gesture_button(
            ids::REFUSED_CHANGE_DISMISS,
            et::refused_changes::DISMISS,
            true,
            Gesture::Events(Action::DismissRefusedChange(view.key)),
        ));
    }
}

pub fn elements(app: &App) -> Vec<Element> {
    let st = &app.events;
    let mut out = vec![Element::label(ids::PAGE_HEADING, et::TITLE)];

    // `events-view-toggle` — the `calendar-view-controls` component's wrapper
    // id around the 4 view-mode buttons below (a bare container marker, the
    // `mail-rotate-keys-exclude-list` idiom): every other app tags a real
    // segmented-control/group container with this id, and tui's radio group
    // already IS that group — it just never registered the wrapper itself.
    out.push(Element::label(ids::EVENTS_VIEW_TOGGLE, String::new()));

    for (id, mode) in [
        ("calendar-view-agenda", ViewMode::Agenda),
        ("calendar-view-month", ViewMode::Month),
        ("calendar-view-week", ViewMode::Week),
        ("calendar-view-day", ViewMode::Day),
    ] {
        // One-of-N view modes: radio paint, so the ACTIVE view is readable on
        // the group itself (plain buttons gave a live user no answer to
        // "which view am I in" — the control-kind-legibility rule).
        let selected = st.view_mode == mode;
        out.push(
            Element::radio_gesture(
                id,
                view_mode_label(mode),
                selected,
                Gesture::Events(Action::SwitchView(mode)),
            )
            .attr("state", if selected { "on" } else { "off" }),
        );
    }

    // View-controls: the date/range label + range pan buttons (the
    // `calendar-view-controls` component — always present, every view mode).
    //
    // ONE painted row, `[ < ] August 2026 [ > ]`. Stacked on three lines the
    // pan buttons read as orphans — two bare arrows with no visible relation to
    // the range between them — and adjacency is how a terminal expresses
    // relatedness at all, having no card or hbox to put them in (`apps/tui.md`
    // § Rendering → *the constrained-channel principle*). Each keeps its own
    // id, gesture, focus slot and hit-test band; only the geometry changes.
    //
    // The brackets are what keep this off the `Select` idiom: a bare `<`/`>`
    // pair around the label would paint `< August 2026 >`, which the vocabulary
    // reads as "a select showing its current value" — a control that cycles a
    // fixed set, not two independent buttons panning an unbounded range.
    out.push(
        Element::gesture_button(
            ids::EVENTS_PREV_MONTH,
            "<",
            true,
            Gesture::Events(Action::PrevMonth),
        )
        .starts_row(),
    );
    out.push(Element::label(ids::CALENDAR_DATE_LABEL, date_label(st)).inline());
    out.push(
        Element::gesture_button(
            ids::EVENTS_NEXT_MONTH,
            ">",
            true,
            Gesture::Events(Action::NextMonth),
        )
        .inline(),
    );

    out.push(Element::gesture_button(
        ids::NEW_CALENDAR_BTN,
        et::NEW_CALENDAR,
        true,
        Gesture::Events(Action::ShowCreateCalendar),
    ));
    // One sidebar ROW per calendar — the name button that selects, then its
    // `calendar-visibility` checkbox, exactly the two-affordance row linux
    // builds in its hbox (`ui/events.md` § Layout & flow). The pair is one
    // painted row (`starts_row` + `inline`) so the box reads as belonging to
    // the calendar beside it rather than to the list; both keep their own id,
    // gesture, focus slot and hit-test band.
    for cal in &st.calendars {
        out.push(
            Element::gesture_button(
                ids::CALENDAR_ITEM,
                // The row's own separator, exactly how the agenda's inline RSVP
                // trio pads itself: an inline run concatenates its cells with
                // nothing between them, so the padding IS the gap. Substring
                // matching is what reads this text (`calendar_names`), so the
                // trailing space costs the e2e action nothing.
                format!("{} ", cal.name),
                true,
                Gesture::SelectCalendar(cal.id.clone()),
            )
            .starts_row(),
        );
        out.push(
            Element::checkbox_gesture(
                ids::CALENDAR_VISIBILITY,
                "",
                st.visible_calendars.contains(&cal.id),
                Gesture::ToggleCalendarVisibility(cal.id.clone()),
            )
            .inline(),
        );
    }
    // Calendar-level `.ics` import/export, beside the calendar list and only
    // while a calendar is selected — both act on THAT calendar (events.md
    // § Import / Export). The path is typed: a terminal has no picker, so the
    // label says so rather than promising one (the media `file-upload` rule).
    if st.selected_calendar_row().is_some() {
        out.push(
            Element::input(
                ids::CALENDAR_IMPORT_FILE,
                st.import_path.clone(),
                Field::Events(EventsField::ImportPath),
            )
            .labelled(fauna_i18n::strings::media::TYPE_FILE_PATH),
        );
        out.push(
            Element::gesture_button(
                ids::CALENDAR_IMPORT_BUTTON,
                et::IMPORT_ICS,
                true,
                Gesture::Events(Action::ImportIcs),
            )
            .starts_row(),
        );
        out.push(
            Element::gesture_button(
                ids::CALENDAR_EXPORT_BUTTON,
                format!(" {}", et::EXPORT_ICS),
                true,
                Gesture::Events(Action::ExportIcs),
            )
            .inline(),
        );
        if !st.ics_notice.is_empty() {
            out.push(Element::chrome(st.ics_notice.clone()));
        }
    }
    out.push(Element::gesture_button(
        ids::NEW_EVENT_BTN,
        et::NEW_EVENT,
        true,
        Gesture::Events(Action::ShowCreateEvent),
    ));

    match &st.mode {
        // The page's calendar scope, resolved ONCE here and handed to whichever
        // view renders — the agenda and the three grids can then only disagree
        // about *dates*, never about which calendars are in scope
        // (`EventsState::displayed_events`).
        Mode::List => {
            render_refused_changes(app, &mut out);
            let shown = st.displayed_events();
            match st.view_mode {
                // Agenda: the date-unfiltered event-card list (every event
                // across the selected calendar), the canonical CRUD surface.
                ViewMode::Agenda => render_agenda(&shown, &mut out),
                ViewMode::Month => render_month(st, &shown, &mut out),
                ViewMode::Week => render_week(st, &shown, &mut out),
                ViewMode::Day => render_day(st, &shown, &mut out),
            }
        }
        Mode::CreateCalendar => {
            out.push(Element::input(
                ids::CALENDAR_NAME,
                st.new_calendar_name.clone(),
                Field::Events(EventsField::CalendarName),
            ));
            out.push(Element::gesture_button(
                ids::CREATE_CALENDAR,
                t::SAVE,
                true,
                Gesture::Events(Action::CreateCalendarSubmit),
            ));
        }
        Mode::CreateEvent => {
            out.push(Element::input(
                ids::EVENT_SUMMARY,
                st.new_event_summary.clone(),
                Field::Events(EventsField::Summary),
            ));
            out.push(Element::input(
                ids::EVENT_DTSTART,
                st.new_event_dtstart.clone(),
                Field::Events(EventsField::Dtstart),
            ));
            out.push(Element::input(
                ids::EVENT_DTEND,
                st.new_event_dtend.clone(),
                Field::Events(EventsField::Dtend),
            ));
            // The `event-form` component's other two inputs (ui.yaml:
            // "description / location — all platforms"). Both optional: an
            // empty buffer writes no VEVENT property at all.
            out.push(
                Element::input(
                    ids::EVENT_FORM_DESCRIPTION,
                    st.new_event_description.clone(),
                    Field::Events(EventsField::Description),
                )
                .labelled(et::DESCRIPTION),
            );
            out.push(
                Element::input(
                    ids::EVENT_FORM_LOCATION,
                    st.new_event_location.clone(),
                    Field::Events(EventsField::Location),
                )
                .labelled(et::LOCATION),
            );
            out.push(Element::gesture_button(
                ids::CREATE_EVENT,
                t::SAVE,
                true,
                Gesture::Events(Action::CreateEventSubmit),
            ));
        }
        Mode::EventDetail(event_id) => render_detail(st, event_id, &mut out),
    }

    out
}

/// ui.yaml's declared `events.state_fields`
/// (`data.events[].{id,summary,start,end,rsvp_status}`). `rsvp_status` is the
/// viewer's own projected RSVP status (`EventRow::self_rsvp`), empty until
/// they RSVP.
///
/// Reports the events the page **shows** ([`EventsState::displayed_events`]),
/// not the raw cache: declared page state that disagreed with the painted list
/// would let a `calendar-visibility` regression pass an assertion made against
/// this surface.
pub fn state_json(state: &EventsState) -> serde_json::Value {
    serde_json::Value::Array(
        state
            .displayed_events()
            .iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "summary": e.summary,
                    "start": e.start,
                    "end": e.end,
                    "rsvp_status": e.self_rsvp,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    // Test-only cross-module reaches: these date-label formatters have no
    // caller in this file's own production code (`grids::date_label` calls
    // them internally), so importing them at module scope would be a dead
    // import outside `cfg(test)` — imported here instead, scoped to the test
    // that exercises them directly.
    use super::grids::{format_day_label, format_month_label, format_week_label};
    // The grids' painted geometry, read by the paint assertions below so they
    // assert the contract rather than re-typing its numbers.
    use super::grids::{
        DAY_COLUMN_WIDTH, GUTTER_WIDTH, MONTH_CELL_WIDTH, SLOT_MINUTES, SLOTS_PER_DAY,
        WEEK_COLUMN_WIDTH,
    };

    fn cal(id: &str, name: &str) -> CalendarRow {
        CalendarRow {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    fn ev(id: &str, calendar_id: &str, summary: &str) -> EventRow {
        EventRow {
            id: id.to_string(),
            calendar_id: calendar_id.to_string(),
            summary: summary.to_string(),
            start: "2026-07-20T10:00".to_string(),
            end: "2026-07-20T11:00".to_string(),
            ..Default::default()
        }
    }

    /// The empty page paints the chrome only: heading, the view-toggle
    /// container marker, 4 view-mode buttons, the view-controls (prev-month,
    /// date-label, next-month), new-calendar-btn, new-event-btn — no
    /// calendar-item/event-card rows.
    #[test]
    fn the_empty_page_paints_exactly_the_chrome() {
        let app = crate::app::tests::test_app();
        let ids: Vec<String> = elements(&app).iter().map(|e| e.id.clone()).collect();
        assert_eq!(
            ids,
            vec![
                "page-heading",
                "events-view-toggle",
                "calendar-view-agenda",
                "calendar-view-month",
                "calendar-view-week",
                "calendar-view-day",
                "events-prev-month",
                "calendar-date-label",
                "events-next-month",
                "new-calendar-btn",
                "new-event-btn",
            ]
        );
    }

    fn refusal_row(uid: &str, summary: &str) -> fauna_core::data::RefusedSchedulingChange {
        fauna_core::data::RefusedSchedulingChange {
            uid_hash: uid.to_string(),
            author: Some("ba5eba11".to_string() + &"0".repeat(56)),
            author_home_nest_url: String::new(),
            sender_address: String::new(),
            method: "CANCEL".to_string(),
            reason: "not_the_organizer".to_string(),
            summary: summary.to_string(),
            first_refused_at: 1_700_000_000,
            last_refused_at: 1_700_000_000,
            occurrences: 1,
            dismissed_through: 0,
            extra: Default::default(),
        }
    }

    /// An account with nothing refused paints NO refused-change ids at all —
    /// the list is a notice, not a permanent section, and the page a user sees
    /// every day must be unchanged by it.
    ///
    /// Guarded by `the_empty_page_paints_exactly_the_chrome` too; this states
    /// the intent so a later "always paint the container" refactor reads as
    /// the behaviour change it would be.
    #[test]
    fn no_refused_change_ids_are_painted_when_nothing_was_refused() {
        let app = crate::app::tests::test_app();
        assert!(app.events.refused_changes.is_empty());
        assert!(
            !elements(&app)
                .iter()
                .any(|e| e.id.starts_with("refused-change")),
        );
    }

    /// A refused change paints one row: what was tried, who tried it, why it
    /// was refused, when, and the dismiss control — and NOTHING that would
    /// apply it. The ruling forbids an "apply anyway" affordance, so the only
    /// gesture on the row is the dismissal.
    #[test]
    fn a_refused_change_paints_a_row_whose_only_gesture_is_dismiss() {
        let mut app = crate::app::tests::test_app();
        app.events.refused_changes = vec![refusal_row("uid-1", "Kickoff")];
        let els = elements(&app);

        let text = |id: &str| {
            els.iter()
                .find(|e| e.id == id)
                .unwrap_or_else(|| panic!("{id} painted"))
                .text
                .clone()
        };
        assert!(
            text("refused-change-title").contains("Kickoff"),
            "the row names the event: {}",
            text("refused-change-title")
        );
        assert!(
            text("refused-change-author").contains("ba5eba11"),
            "an unresolved sender is named by a short id: {}",
            text("refused-change-author")
        );
        assert!(!text("refused-change-reason").is_empty());
        assert!(!text("refused-change-time").is_empty());

        let gestures: Vec<_> = els
            .iter()
            .filter(|e| e.id.starts_with("refused-change") && e.gesture().is_some())
            .map(|e| e.id.clone())
            .collect();
        assert_eq!(
            gestures,
            vec!["refused-change-dismiss"],
            "the list is informational — dismiss is its ONLY control"
        );
    }

    /// The import/export controls act on a selected calendar, so they paint
    /// only while one is selected, and an import with no path is said on
    /// `error-message` rather than dispatched (events.md § Import / Export).
    #[test]
    fn ics_controls_follow_the_selection_and_an_empty_import_says_so() {
        let mut app = events_test_app();
        app.events.calendars = vec![CalendarRow {
            id: "a".repeat(64),
            name: "Work".to_string(),
        }];
        let painted = |app: &App| {
            elements(app)
                .iter()
                .filter(|e| e.id.starts_with("calendar-import") || e.id == "calendar-export-button")
                .map(|e| e.id.clone())
                .collect::<Vec<_>>()
        };
        assert!(
            painted(&app).is_empty(),
            "no calendar selected → no controls"
        );
        assert!(apply_local(&mut app, Action::ExportIcs).is_none());

        app.events.selected_calendar = Some("a".repeat(64));
        assert_eq!(
            painted(&app),
            vec![
                "calendar-import-file",
                "calendar-import-button",
                "calendar-export-button"
            ]
        );

        app.events.import_path = "   ".to_string();
        assert!(apply_local(&mut app, Action::ImportIcs).is_none());
        assert_eq!(
            app.errors.get(&Page::Events).map(String::as_str),
            Some(et::ICS_PATH_REQUIRED)
        );

        app.events.import_path = "/tmp/work.ics".to_string();
        match apply_local(&mut app, Action::ImportIcs) {
            Some(Op::ImportIcs {
                calendar_id, path, ..
            }) => {
                assert_eq!(calendar_id, "a".repeat(64));
                assert_eq!(path, "/tmp/work.ics");
            }
            _ => panic!("expected an import op"),
        }
        match apply_local(&mut app, Action::ExportIcs) {
            Some(Op::ExportIcs { calendar_name, .. }) => assert_eq!(calendar_name, "Work"),
            _ => panic!("expected an export op"),
        }
    }

    /// An export is named after its calendar, made safe as a file name, and a
    /// second export never overwrites the first.
    #[test]
    fn export_path_is_the_calendar_name_and_never_overwrites() {
        assert_eq!(ics_file_stem("Work"), "Work");
        assert_eq!(ics_file_stem(" a/b\\c:d "), "a_b_c_d");
        assert_eq!(ics_file_stem(".."), "calendar");
        assert_eq!(ics_file_stem(""), "calendar");

        let dir = tempfile::tempdir().expect("tempdir");
        let first = unused_export_path(dir.path(), "Work");
        assert_eq!(first, dir.path().join("Work.ics"));
        std::fs::write(&first, "x").unwrap();
        assert_eq!(
            unused_export_path(dir.path(), "Work"),
            dir.path().join("Work (2).ics")
        );
    }

    /// Dismissing drops the row from the painted list at once (the op's
    /// re-read is the truth, but a terminal must acknowledge immediately) and
    /// writes the dismissal to the account store.
    #[test]
    fn dismissing_a_row_clears_it_and_issues_the_plane_write() {
        // `events_test_app` (not the bare test app): this action's local half
        // rides the page's nest handle, exactly like every other gesture here.
        let mut app = events_test_app();
        app.events.refused_changes = vec![
            refusal_row("uid-1", "Kickoff"),
            refusal_row("uid-2", "Standup"),
        ];
        let key = app.events.refused_changes[0].key();

        let op = apply_local(&mut app, Action::DismissRefusedChange(key.clone()));

        assert_eq!(
            app.events.refused_changes.len(),
            1,
            "only the dismissed row leaves the list"
        );
        assert_eq!(app.events.refused_changes[0].uid_hash, "uid-2");
        match op {
            Some(Op::DismissRefusedChange { key: k, .. }) => assert_eq!(k, key),
            _ => panic!("expected a dismiss op"),
        }
        assert_eq!(
            Action::DismissRefusedChange(key).wire_kind(),
            None,
            "the dismissal is a write to this device's account store — local to \
             the offline gate, so the control stays usable with no nest: \
             acknowledging a notice you have read is not work that should need \
             a connection"
        );
    }

    /// The four view-mode buttons are one-of-N radio options whose paint
    /// carries the ACTIVE view (the control-vocabulary rule, tui.md
    /// § Rendering) — as plain buttons a user had no on-screen answer to
    /// "which view am I in".
    #[test]
    fn the_active_view_mode_is_selected_on_the_group() {
        let mut app = crate::app::tests::test_app();
        app.events.view_mode = ViewMode::Week;
        for (id, selected) in [
            ("calendar-view-agenda", false),
            ("calendar-view-month", false),
            ("calendar-view-week", true),
            ("calendar-view-day", false),
        ] {
            let els = elements(&app);
            let e = els.iter().find(|e| e.id == id).unwrap();
            match &e.role {
                crate::element::Role::Radio { selected: s, .. } => {
                    assert_eq!(*s, selected, "{id} selected state")
                }
                other => panic!("{id} should be a radio option, got {other:?}"),
            }
        }
    }

    /// A loaded calendar list paints one (`calendar-item`,
    /// `calendar-visibility`) pair per calendar, between new-calendar-btn and
    /// new-event-btn — the two-affordance sidebar row of `ui/events.md`
    /// § Layout & flow.
    #[test]
    fn calendars_paint_between_new_calendar_and_new_event() {
        let mut app = crate::app::tests::test_app();
        app.events.calendars = vec![cal("aa", "Work"), cal("bb", "Home")];
        let ids: Vec<String> = elements(&app).iter().map(|e| e.id.clone()).collect();
        assert_eq!(
            ids,
            vec![
                "page-heading",
                "events-view-toggle",
                "calendar-view-agenda",
                "calendar-view-month",
                "calendar-view-week",
                "calendar-view-day",
                "events-prev-month",
                "calendar-date-label",
                "events-next-month",
                "new-calendar-btn",
                "calendar-item",
                "calendar-visibility",
                "calendar-item",
                "calendar-visibility",
                "new-event-btn",
            ]
        );
    }

    /// Each calendar's name button and its visibility box share ONE painted
    /// line — the box reads as belonging to the calendar beside it, not to the
    /// list (`tui.md` § Rendering: assert the paint, not just the ids).
    #[test]
    fn a_calendars_name_and_visibility_box_paint_on_one_line() {
        let mut app = crate::app::tests::test_app();
        app.events.calendars = vec![cal("aa", "Work"), cal("bb", "Home")];
        app.events.visible_calendars = ["aa".to_string()].into_iter().collect();

        let painted = crate::ui::painted_line_texts(&elements(&app));
        let rows: Vec<&String> = painted
            .iter()
            .filter(|line| line.contains("Work") || line.contains("Home"))
            .collect();

        assert_eq!(rows.len(), 2, "one line per calendar: {painted:#?}");
        assert!(
            rows[0].contains("Work") && rows[0].contains("[x]"),
            "the checked calendar paints its own box on its own row: {rows:#?}"
        );
        assert!(
            rows[1].contains("Home") && rows[1].contains("[ ]"),
            "an unchecked calendar paints an empty box on its row: {rows:#?}"
        );
    }

    /// **The pan controls and the range they pan share ONE painted line.**
    /// Stacked on three lines the two arrows read as orphans; adjacency is the
    /// only channel a terminal has for "these belong together" (`tui.md`
    /// § Rendering → *the constrained-channel principle*).
    ///
    /// The brackets are load-bearing, not decoration: without them the row
    /// paints `< August 2026 >`, which the control vocabulary reads as a
    /// `Select` showing its current value — a different control with a
    /// different gesture. This asserts the row is one line AND that it does not
    /// wear the select idiom.
    #[test]
    fn the_pan_controls_share_the_range_label_s_line_without_reading_as_a_select() {
        let app = crate::app::tests::test_app();
        let painted = crate::ui::painted_line_texts(&elements(&app));
        let label = date_label(&app.events);

        let row = painted
            .iter()
            .find(|line| line.contains(&label))
            .unwrap_or_else(|| panic!("no line carries the range label: {painted:#?}"));

        assert!(
            row.contains("[ < ]") && row.contains("[ > ]"),
            "both pan buttons sit on the range's own line, as buttons: {row:?}"
        );
        assert!(
            !row.contains(&format!("< {label} >")),
            "and the row must not paint the Select idiom `< value >`: {row:?}"
        );
    }

    /// An inline button is still a **button**: `calendar-item` sits in a row
    /// beside its visibility box, and a row is geometry — it does not make the
    /// control stop being one. This paints bare text before `Element::cell`
    /// made canvas the marked case, so a calendar name read as a label.
    #[test]
    fn an_inline_button_still_paints_as_a_button() {
        let mut app = crate::app::tests::test_app();
        app.events.calendars = vec![cal("aa", "Work")];

        let painted = crate::ui::painted_line_texts(&elements(&app));
        let row = painted
            .iter()
            .find(|line| line.contains("Work"))
            .unwrap_or_else(|| panic!("no calendar row: {painted:#?}"));

        // The name carries the row's own trailing separator, so the closing
        // bracket does not sit flush against it — assert the bracketing, not
        // the padding.
        assert!(
            row.contains("[ Work") && row.contains("]["),
            "the calendar name is an action, and paints like one — brackets, \
             then its visibility box: {row:?}"
        );
    }

    /// Unchecking a calendar drops **its** events from the no-selection union,
    /// and only its own (`ui/events.md` § Where logic lives → *Which calendars
    /// display*).
    #[test]
    fn unchecking_a_calendar_filters_it_out_of_the_union() {
        let mut app = crate::app::tests::test_app();
        app.events.calendars = vec![cal("aa", "Work"), cal("bb", "Home")];
        app.events.visible_calendars = ["aa".to_string(), "bb".to_string()].into_iter().collect();
        app.events.events_cache = vec![ev("1", "aa", "Standup"), ev("2", "bb", "Dinner")];

        let both: Vec<&str> = app
            .events
            .displayed_events()
            .iter()
            .map(|e| e.summary.as_str())
            .collect();
        assert_eq!(both, vec!["Standup", "Dinner"]);

        toggle_calendar_visibility(&mut app, "bb".to_string());
        let shown: Vec<&str> = app
            .events
            .displayed_events()
            .iter()
            .map(|e| e.summary.as_str())
            .collect();
        assert_eq!(shown, vec!["Standup"], "only Home's events left the union");

        // The declared page state agrees with the paint — otherwise a
        // regression here would pass an assertion made against `state_json`.
        assert_eq!(state_json(&app.events).as_array().unwrap().len(), 1);
    }

    /// An **empty** checked set is "no filter" — the full union, not "hide
    /// everything". This is the case the per-app copies had drifted on before
    /// the shared predicate landed.
    #[test]
    fn an_empty_visible_set_is_the_union_not_a_blank_page() {
        let mut app = crate::app::tests::test_app();
        app.events.calendars = vec![cal("aa", "Work"), cal("bb", "Home")];
        app.events.events_cache = vec![ev("1", "aa", "Standup"), ev("2", "bb", "Dinner")];
        assert!(app.events.visible_calendars.is_empty());
        assert_eq!(app.events.displayed_events().len(), 2);
    }

    /// A live `calendar-item` selection wins outright: selecting a calendar
    /// shows it even while its own box is unchecked, and hides the others even
    /// while theirs are checked.
    #[test]
    fn a_live_selection_overrides_every_visibility_box() {
        let mut app = crate::app::tests::test_app();
        app.events.calendars = vec![cal("aa", "Work"), cal("bb", "Home")];
        app.events.visible_calendars = ["aa".to_string()].into_iter().collect();
        app.events.events_cache = vec![ev("1", "aa", "Standup"), ev("2", "bb", "Dinner")];
        app.events.selected_calendar = Some("bb".to_string());

        let shown: Vec<&str> = app
            .events
            .displayed_events()
            .iter()
            .map(|e| e.summary.as_str())
            .collect();
        assert_eq!(shown, vec!["Dinner"]);
    }

    /// A fresh load checks every box (so an unchecked box is always a
    /// deliberate user act), a brand-new calendar arrives checked, and an
    /// existing calendar keeps whatever the user chose — linux's seeding rule.
    #[test]
    fn a_load_seeds_new_calendars_visible_and_keeps_existing_choices() {
        let mut visible = std::collections::HashSet::new();
        let first = vec![cal("aa", "Work"), cal("bb", "Home")];
        seed_visible_calendars(&mut visible, &[], &first);
        assert_eq!(visible.len(), 2, "a fresh page paints every box checked");

        visible.remove("bb");
        let second = vec![cal("aa", "Work"), cal("bb", "Home"), cal("cc", "Team")];
        seed_visible_calendars(&mut visible, &first, &second);
        assert!(
            visible.contains("cc"),
            "a brand-new calendar starts visible"
        );
        assert!(
            !visible.contains("bb"),
            "a reload must not silently re-check what the user unchecked"
        );
    }

    /// A calendar the user just created arrives with its box CHECKED — it must
    /// not be filtered out of the union they are looking at. Regression pin: the
    /// create path updates `calendars` on its own outcome, so it needs its own
    /// seeding call; without it the new row renders unchecked as soon as any
    /// other box is checked, which is the normal state.
    #[test]
    fn a_freshly_created_calendar_arrives_visible() {
        let mut app = crate::app::tests::test_app();
        app.events.calendars = vec![cal("aa", "Work")];
        app.events.visible_calendars = ["aa".to_string()].into_iter().collect();

        apply_outcome(
            &mut app,
            Outcome::CalendarCreated(vec![cal("aa", "Work"), cal("bb", "Home")]),
        );

        assert!(
            app.events.visible_calendars.contains("bb"),
            "the just-created calendar must be checked: {:?}",
            app.events.visible_calendars
        );
        assert!(app.events.visible_calendars.contains("aa"));
    }

    /// Toggling is purely local — no refetch, because the union arm's cache
    /// already holds every owned calendar's events.
    #[test]
    fn toggling_visibility_returns_no_op_and_flips_both_ways() {
        let mut app = crate::app::tests::test_app();
        app.events.calendars = vec![cal("aa", "Work")];
        app.events.visible_calendars = ["aa".to_string()].into_iter().collect();

        let work = crate::app::gesture_work(
            &mut app,
            Gesture::ToggleCalendarVisibility("aa".to_string()),
        );
        assert!(
            matches!(work, crate::app::GestureWork::None),
            "a visibility toggle never hits the network"
        );
        assert!(!app.events.visible_calendars.contains("aa"));

        toggle_calendar_visibility(&mut app, "aa".to_string());
        assert!(app.events.visible_calendars.contains("aa"));
    }

    /// List mode paints one (event-card, event-card-summary, then the
    /// `rsvp-button-group` trio) group per event, flat-indexed in registration
    /// order (the notifications/conversations bubble-children convention).
    #[test]
    fn list_mode_paints_event_card_groups() {
        let mut app = crate::app::tests::test_app();
        app.events.events_cache = vec![ev("1", "aa", "Standup"), ev("2", "aa", "Lunch")];
        let els = elements(&app);
        let ids: Vec<String> = els.iter().map(|e| e.id.clone()).collect();
        assert_eq!(
            &ids[ids.len() - 10..],
            &[
                "event-card",
                "event-card-summary",
                "event-rsvp-going",
                "event-rsvp-interested",
                "event-rsvp-decline",
                "event-card",
                "event-card-summary",
                "event-rsvp-going",
                "event-rsvp-interested",
                "event-rsvp-decline",
            ]
        );
        assert_eq!(els[els.len() - 9].text, "Standup");
        assert_eq!(els[els.len() - 4].text, "Lunch");
    }

    /// **The index-alignment contract the e2e depends on.** `rsvp_on_card`
    /// (`tests/e2e-unified/actions/events.py`) finds a card by scanning
    /// `event-card-summary` text and then clicks `event-rsvp-{status}` at *the
    /// same flat index*. That is only sound while every event registers exactly
    /// one of each id, in one pass — so pin the correspondence directly rather
    /// than leaving it an implicit property of the paint order above.
    #[test]
    fn card_rsvp_ids_are_index_aligned_with_event_card_summary() {
        let mut app = crate::app::tests::test_app();
        app.events.events_cache = vec![
            ev("1", "aa", "Standup"),
            ev("2", "aa", "Lunch"),
            ev("3", "aa", "Retro"),
        ];
        let els = elements(&app);
        let nth = |id: &str| -> Vec<&Element> { els.iter().filter(|e| e.id == id).collect() };
        let summaries = nth("event-card-summary");
        assert_eq!(summaries.len(), 3);
        for id in [
            "event-rsvp-going",
            "event-rsvp-interested",
            "event-rsvp-decline",
        ] {
            let buttons = nth(id);
            assert_eq!(buttons.len(), 3, "{id} must be 1:1 with event-card-summary");
            for (i, btn) in buttons.iter().enumerate() {
                let crate::element::Role::Button(Gesture::RsvpEvent { event_id, .. }) = &btn.role
                else {
                    panic!("{id}[{i}] is not an RsvpEvent button");
                };
                // The i-th button must target the i-th summary's event.
                let expected = &app.events.events_cache[i].id;
                assert_eq!(
                    event_id, expected,
                    "{id}[{i}] targets {event_id}, but event-card-summary[{i}] is \
                     {:?} (event {expected}) — the e2e's index carry-over is broken",
                    summaries[i].text
                );
            }
        }
    }

    /// The card trio submits for the card's OWN event while the page is in
    /// `Mode::List` — no detail open, nothing selected. This is the whole
    /// difference from [`Action::Rsvp`], which reads `Mode::EventDetail` and
    /// would produce nothing here.
    #[test]
    fn card_rsvp_targets_its_own_event_without_opening_the_detail() {
        let mut app = events_test_app();
        app.events.events_cache = vec![ev("1", "aa", "Standup"), ev("2", "bb", "Lunch")];
        app.events.mode = Mode::List;

        // The detail-page action is inert here — there is no open detail.
        assert!(apply_local(&mut app, Action::Rsvp(RsvpResponse::Going)).is_none());

        let Some(Op::RsvpEvent {
            calendar_id,
            uid_hash_hex,
            response,
            ..
        }) = rsvp_event(&app, "2".to_string(), RsvpResponse::Interested)
        else {
            panic!("card RSVP produced no op");
        };
        // The SECOND card's event and ITS calendar — not the first, and not
        // the selected-calendar fallback.
        assert_eq!(uid_hash_hex, "2");
        assert_eq!(calendar_id, "bb");
        assert_eq!(response, RsvpResponse::Interested);
        // And the page has not moved: the quick action does not drill in.
        assert_eq!(app.events.mode, Mode::List);
    }

    /// "decline" is the button *label*; `"declined"` is the wire response fed
    /// to `apply_rsvp` — the same `partstat_from_fauna` contract the detail
    /// trio honours. A card trio that shipped `"decline"` would round-trip as
    /// an unknown PARTSTAT.
    #[test]
    fn card_decline_button_sends_the_declined_wire_response() {
        let mut app = events_test_app();
        app.events.events_cache = vec![ev("1", "aa", "Standup")];
        let els = elements(&app);
        let btn = els
            .iter()
            .find(|e| e.id == "event-rsvp-decline")
            .expect("card decline button");
        let crate::element::Role::Button(Gesture::RsvpEvent { response, .. }) = &btn.role else {
            panic!("not an RsvpEvent button");
        };
        assert_eq!(*response, RsvpResponse::Declined);
    }

    /// `Mode::CreateCalendar` swaps the event-card list for the calendar-name
    /// form, and does NOT paint the create_event fields.
    #[test]
    fn create_calendar_mode_paints_the_calendar_form() {
        let mut app = crate::app::tests::test_app();
        app.events.mode = Mode::CreateCalendar;
        app.events.events_cache = vec![ev("1", "aa", "Standup")]; // must not paint while in this mode
        let ids: Vec<String> = elements(&app).iter().map(|e| e.id.clone()).collect();
        assert!(ids.contains(&"calendar-name".to_string()));
        assert!(ids.contains(&"create-calendar".to_string()));
        assert!(!ids.contains(&"event-card".to_string()));
        assert!(!ids.contains(&"event-summary".to_string()));
    }

    /// `Mode::CreateEvent` paints the whole `event-form` component — summary +
    /// start + end + description + location — plus submit.
    #[test]
    fn create_event_mode_paints_the_event_form() {
        let mut app = crate::app::tests::test_app();
        app.events.mode = Mode::CreateEvent;
        let ids: Vec<String> = elements(&app).iter().map(|e| e.id.clone()).collect();
        for want in [
            "event-summary",
            "event-dtstart",
            "event-dtend",
            "event-form-description",
            "event-form-location",
            "create-event",
        ] {
            assert!(ids.contains(&want.to_string()), "missing {want}");
        }
    }

    /// The two optional `event-form` inputs reach the VEVENT: `CreateEventSubmit`
    /// carries the description/location buffers into the [`Op`], which is what
    /// `EventFields` is built from. Without this the inputs would paint and be
    /// typed into while the write silently dropped them — the shape that makes a
    /// green create test lie about what it saved.
    #[test]
    fn create_event_submit_carries_the_description_and_location() {
        let mut app = events_test_app();
        app.events.calendars = vec![cal("aa", "Personal")];
        app.events.new_event_summary = "Offsite".to_string();
        app.events.new_event_dtstart = "2026-07-20T10:00".to_string();
        app.events.new_event_dtend = "2026-07-20T11:00".to_string();
        app.events.new_event_description = "Agenda and notes".to_string();
        app.events.new_event_location = "Room 3".to_string();
        let Some(Op::CreateEvent {
            description,
            location,
            ..
        }) = apply_local(&mut app, Action::CreateEventSubmit)
        else {
            panic!("not a CreateEvent op");
        };
        assert_eq!(description, "Agenda and notes");
        assert_eq!(location, "Room 3");
    }

    /// The New Event opener **resumes** rather than clearing — that is what
    /// makes the `"events"` drafts rail mean anything, since this is the opener
    /// a user reaches for after relaunching to finish what they were writing
    /// (`events.md` § Persistence). ⚠ This test asserts the OPPOSITE of the
    /// pre-2026-08-17 `opening_the_compose_clears_the_description_and_location`,
    /// which it replaces; the guarantee that one protected — a second event
    /// never inheriting the first's text — moved to the create path, pinned by
    /// `a_created_event_stops_being_a_draft` below.
    #[test]
    fn opening_the_compose_resumes_the_draft() {
        let mut app = events_test_app();
        app.events.new_event_summary = "half a thought".to_string();
        app.events.new_event_description = "Agenda and notes".to_string();
        app.events.new_event_location = "Room 3".to_string();

        apply_local(&mut app, Action::ShowCreateEvent);

        assert_eq!(app.events.new_event_summary, "half a thought");
        assert_eq!(app.events.new_event_description, "Agenda and notes");
        assert_eq!(app.events.new_event_location, "Room 3");
        assert!(matches!(app.events.mode, Mode::CreateEvent));
    }

    /// The old opener-clears guarantee, moved to where the event actually stops
    /// being in progress. Without this a relaunch would restore the text of an
    /// event already on the calendar, and a second event would inherit the
    /// first's description/location.
    #[test]
    fn a_created_event_stops_being_a_draft() {
        let mut app = events_test_app();
        app.events.new_event_summary = "Offsite".to_string();
        app.events.new_event_dtstart = "2026-09-01T09:00".to_string();
        app.events.new_event_dtend = "2026-09-01T10:00".to_string();
        app.events.new_event_description = "Agenda and notes".to_string();
        app.events.new_event_location = "Room 3".to_string();

        apply_outcome(
            &mut app,
            Outcome::EventCreated {
                calendar_id: "cal-1".to_string(),
                events: Vec::new(),
            },
        );

        assert!(app.events.new_event_summary.is_empty());
        assert!(app.events.new_event_dtstart.is_empty());
        assert!(app.events.new_event_dtend.is_empty());
        assert!(app.events.new_event_description.is_empty());
        assert!(app.events.new_event_location.is_empty());
    }

    /// A day-cell gesture means *start a new event here* (§ Layout & flow), so
    /// it is the opener that DOES clear — the deliberate other half of the
    /// resume/start-fresh split.
    #[test]
    fn the_day_cell_gesture_starts_a_fresh_event() {
        let mut app = events_test_app();
        app.events.new_event_summary = "stale".to_string();
        app.events.new_event_description = "stale".to_string();
        app.events.new_event_location = "stale".to_string();

        apply_local(
            &mut app,
            Action::ComposeOnDay {
                y: 2026,
                m: 9,
                d: 1,
                at: Some((14, 30)),
            },
        );

        assert!(app.events.new_event_summary.is_empty());
        assert!(app.events.new_event_description.is_empty());
        assert!(app.events.new_event_location.is_empty());
        assert_eq!(
            app.events.new_event_dtstart, "2026-09-01T14:30",
            "the clicked cell's date and slot time are still prefilled",
        );
    }

    /// **A restore that outlived the account switch reaches nothing.**
    ///
    /// `account-scoping.md` § The scoping taxonomy → the in-memory corollary:
    /// the loops that WRITE actor-scoped state must be retired by the same
    /// drop, and one holding no cancellation handle needs a seam. tui's events
    /// restore is exactly that loop — a `DraftsSync::load()` on a detached
    /// task, sending on the process-wide `UiMessage` channel — and
    /// `session::establish` installs a *fresh* `EventsState` for the incoming
    /// actor, so a late outcome lands in the next actor's compose buffers,
    /// which their first keystroke then autosaves under their own `BackupKey`.
    ///
    /// Red-verify by dropping the generation comparison in `apply_outcome`:
    /// the departing actor's summary appears in the first assertion below.
    #[test]
    fn a_restore_from_a_departed_session_never_reaches_the_next_actor() {
        let mut app = events_test_app();
        let departing = app.session_generation;

        // The switch: every teardown bumps the counter
        // (`App::begin_identity_teardown`), and the incoming actor's
        // `establish` replaces the page state wholesale.
        app.session_generation = departing.saturating_add(1);
        app.events = EventsState::default();

        apply_outcome(
            &mut app,
            Outcome::DraftsLoaded {
                draft: Box::new(fauna_client_caldav::drafts::EventDrafts {
                    summary: "the departing actor's half-written offsite".into(),
                    dtstart: "2026-09-01T09:00".into(),
                    dtend: "2026-09-01T10:30".into(),
                    description: "and its private notes".into(),
                    location: "the ice floe".into(),
                }),
                session_generation: departing,
            },
        );

        assert!(
            app.events.new_event_summary.is_empty(),
            "the departing actor's draft was painted into the next actor's compose buffers",
        );
        assert!(app.events.new_event_description.is_empty());
        assert!(app.events.new_event_location.is_empty());
        assert!(app.events.new_event_dtstart.is_empty());
        assert!(app.events.new_event_dtend.is_empty());
    }

    /// The launch restore lands through the page's ordinary outcome channel.
    #[test]
    fn a_restored_draft_lands_in_the_compose_buffers() {
        let mut app = events_test_app();

        // Bound before the call: `apply_outcome` takes `&mut app`, so reading
        // `app.session_generation` in the same argument list is E0503.
        let live = app.session_generation;
        apply_outcome(
            &mut app,
            Outcome::DraftsLoaded {
                draft: Box::new(fauna_client_caldav::drafts::EventDrafts {
                    summary: "Quarterly walrus review".into(),
                    dtstart: "2026-09-01T09:00".into(),
                    dtend: "2026-09-01T10:30".into(),
                    description: "bring the herring numbers".into(),
                    location: "the ice floe".into(),
                }),
                session_generation: live,
            },
        );

        assert_eq!(app.events.new_event_summary, "Quarterly walrus review");
        assert_eq!(app.events.new_event_dtstart, "2026-09-01T09:00");
        assert_eq!(app.events.new_event_dtend, "2026-09-01T10:30");
        assert_eq!(
            app.events.new_event_description,
            "bring the herring numbers"
        );
        assert_eq!(app.events.new_event_location, "the ice floe");
    }

    /// The round trip that makes the rail cross-device: what the page holds
    /// serialises, and the bytes restore to the same compose state.
    #[test]
    fn the_compose_round_trips_through_the_rails_at_rest_record() {
        let mut app = events_test_app();
        app.events.new_event_summary = "Offsite".to_string();
        app.events.new_event_dtstart = "2026-09-01T09:00".to_string();
        app.events.new_event_description = "Agenda and notes".to_string();
        app.events.new_event_location = "Room 3".to_string();

        let bytes = app.events.event_draft().snapshot_bytes();

        let mut other_device = events_test_app();
        other_device.events.apply_event_draft(
            fauna_client_caldav::drafts::EventDrafts::restore_from_bytes(&bytes).expect("restore"),
        );

        assert_eq!(other_device.events.new_event_summary, "Offsite");
        assert_eq!(other_device.events.new_event_dtstart, "2026-09-01T09:00");
        assert_eq!(
            other_device.events.new_event_description,
            "Agenda and notes"
        );
        assert_eq!(other_device.events.new_event_location, "Room 3");
    }

    /// `event_detail` paints the time range unconditionally and the
    /// location/description only when the VEVENT carried them — the six-app
    /// shape (events.md § Element IDs). tui had painted NONE of the three until
    /// 2026-08-10, so an event's detail surface never said when it was.
    #[test]
    fn detail_paints_the_time_range_location_and_description() {
        let mut app = crate::app::tests::test_app();
        app.events.events_cache = vec![EventRow {
            location: "Room 3".to_string(),
            description: "Agenda and notes".to_string(),
            ..ev("1", "aa", "Offsite")
        }];
        app.events.mode = Mode::EventDetail("1".to_string());
        let els = elements(&app);
        let text = |id: &str| {
            els.iter()
                .find(|e| e.id == id)
                .unwrap_or_else(|| panic!("missing {id}"))
                .text
                .clone()
        };
        assert_eq!(
            text("event-detail-time"),
            "2026-07-20T10:00 – 2026-07-20T11:00"
        );
        assert_eq!(text("event-detail-location"), "Room 3");
        assert_eq!(text("event-detail-description"), "Agenda and notes");
    }

    /// An event with neither property paints NO location/description row —
    /// rather than an empty one, which would read as "this event has a blank
    /// location" (the rule-5 finding this queue's LEAD carries: a present-but-
    /// wrong line is worse than an absent one). The time range still paints.
    #[test]
    fn detail_omits_an_absent_location_and_description() {
        let mut app = crate::app::tests::test_app();
        app.events.events_cache = vec![ev("1", "aa", "Offsite")];
        app.events.mode = Mode::EventDetail("1".to_string());
        let els = elements(&app);
        assert!(els.iter().any(|e| e.id == "event-detail-time"));
        assert!(!els.iter().any(|e| e.id == "event-detail-location"));
        assert!(!els.iter().any(|e| e.id == "event-detail-description"));
    }

    /// `Mode::EventDetail` paints the summary, back, delete, the RSVP trio,
    /// and the unset-reminder select/set pair, looked up from the cached
    /// event list by id — no separate fetch.
    #[test]
    fn event_detail_mode_paints_the_cached_summary() {
        let mut app = crate::app::tests::test_app();
        app.events.events_cache = vec![ev("1", "aa", "Standup")];
        app.events.mode = Mode::EventDetail("1".to_string());
        let els = elements(&app);
        let summary = els.iter().find(|e| e.id == "event-detail-summary").unwrap();
        assert_eq!(summary.text, "Standup");
        assert!(els.iter().any(|e| e.id == "event-detail-back"));
        assert!(els.iter().any(|e| e.id == "event-delete-btn"));
        for id in [
            "event-detail-rsvp-going",
            "event-detail-rsvp-interested",
            "event-detail-rsvp-decline",
        ] {
            assert!(els.iter().any(|e| e.id == id), "missing {id}");
        }
        assert!(els.iter().any(|e| e.id == "event-detail-reminder-select"));
        assert!(els.iter().any(|e| e.id == "event-detail-reminder-set"));
        assert!(!els.iter().any(|e| e.id == "event-detail-reminder-current"));
        assert!(!els.iter().any(|e| e.id == "event-detail-reminder-remove"));
    }

    /// An event with a non-empty `alarm` paints the current-offset label +
    /// Remove button instead of the select/Set pair (events.md § Reminders'
    /// two-state control) — the label text comes off the shared
    /// `fauna_core::ical::reminder_label` preset map.
    #[test]
    fn event_detail_with_a_reminder_paints_the_current_label() {
        let mut app = crate::app::tests::test_app();
        let mut row = ev("1", "aa", "Standup");
        row.alarm = "PT1H".to_string();
        app.events.events_cache = vec![row];
        app.events.mode = Mode::EventDetail("1".to_string());
        let els = elements(&app);
        let current = els
            .iter()
            .find(|e| e.id == "event-detail-reminder-current")
            .unwrap();
        assert_eq!(current.text, "1 hour before");
        assert!(els.iter().any(|e| e.id == "event-detail-reminder-remove"));
        assert!(!els.iter().any(|e| e.id == "event-detail-reminder-select"));
        assert!(!els.iter().any(|e| e.id == "event-detail-reminder-set"));
    }

    /// One flat-indexed `attendee-item` row marker per roster row, carrying
    /// the `attendee-id` / `attendee-status` children ui.yaml scopes to
    /// `events.event_detail`. Their texts are the BARE display name and the
    /// bare localized status label — exactly what web's spans and windows'
    /// TextBlocks report — so a shared-test `get_text` reads the same value
    /// on all three; tui's `(…)` decoration is paint-only chrome (the test
    /// below asserts the painted line). Children are scoped under their own
    /// row occurrence (the post-card convention), so both flat `attendee-id[1]`
    /// and `scope="attendee-item[1]"` queries resolve.
    #[test]
    fn event_detail_registers_attendee_id_and_status_children_per_row() {
        let mut app = crate::app::tests::test_app();
        let mut row = ev("1", "aa", "Standup");
        row.attendees = vec![
            AttendeeRow {
                name: "Alice".to_string(),
                email: "alice@example.com".to_string(),
                rsvp: "going".to_string(),
            },
            AttendeeRow {
                name: String::new(),
                email: "bob@example.com".to_string(),
                rsvp: "invited".to_string(),
            },
        ];
        app.events.events_cache = vec![row];
        app.events.mode = Mode::EventDetail("1".to_string());
        let els = elements(&app);
        let items: Vec<&Element> = els.iter().filter(|e| e.id == "attendee-item").collect();
        assert_eq!(items.len(), 2);
        let names: Vec<&Element> = els.iter().filter(|e| e.id == "attendee-id").collect();
        let statuses: Vec<&Element> = els.iter().filter(|e| e.id == "attendee-status").collect();
        assert_eq!(names.len(), 2);
        assert_eq!(statuses.len(), 2);
        assert_eq!(names[0].text, "Alice");
        assert_eq!(statuses[0].text, "Going");
        assert_eq!(names[1].text, "bob@example.com");
        assert_eq!(statuses[1].text, "Invited");
        assert_eq!(names[1].path, vec![("attendee-item".to_string(), 1)]);
        assert_eq!(statuses[0].path, vec![("attendee-item".to_string(), 0)]);
        // The roster's container marker (below) rides the same paint.
        let list = els.iter().find(|e| e.id == "attendee-list").unwrap();
        assert_eq!(list.text, "Attendees (2)");
    }

    /// The roster row still paints as ONE line — `<name-or-email> (<status>)`,
    /// events.md § Attendee list presentation's ruled tui compression — while
    /// the name and status are individually-addressable inline cells. Paint
    /// assertion owed by the inline-run rule (`tui.md` § Rendering): the
    /// registry alone cannot see whether the cells share a line.
    #[test]
    fn an_attendee_row_paints_name_and_status_on_one_line() {
        let mut app = crate::app::tests::test_app();
        let mut row = ev("1", "aa", "Standup");
        row.attendees = vec![
            AttendeeRow {
                name: "Alice".to_string(),
                email: "alice@example.com".to_string(),
                rsvp: "going".to_string(),
            },
            AttendeeRow {
                name: String::new(),
                email: "bob@example.com".to_string(),
                rsvp: "invited".to_string(),
            },
        ];
        app.events.events_cache = vec![row];
        app.events.mode = Mode::EventDetail("1".to_string());
        let painted = crate::ui::painted_line_texts(&elements(&app));
        let rows: Vec<&String> = painted
            .iter()
            .filter(|line| line.contains("Alice") || line.contains("bob@example.com"))
            .collect();
        assert_eq!(rows.len(), 2, "one painted line per attendee: {painted:#?}");
        assert!(
            rows[0].contains("Alice (Going)"),
            "name and status compose the ruled one-line row: {rows:#?}"
        );
        assert!(
            rows[1].contains("bob@example.com (Invited)"),
            "a CN-less attendee paints the bare email: {rows:#?}"
        );
    }

    /// `attendee-list` is the roster's container marker — ui.yaml scopes it to
    /// `events.sub_pages.event_detail` and events.md § Attendee list
    /// presentation makes it the thing `attendee-item` rows live in. tui built
    /// the rows and never the container; nothing caught it, because until
    /// 2026-08-10 `lint-ui-elements.py` never walked `sub_pages`.
    ///
    /// It is registered UNCONDITIONALLY (linux's ListBox / windows' ListView
    /// shape, not web/apple's non-empty-only container) so a harness can always
    /// read it — a container that vanishes when empty is indistinguishable from
    /// one that is broken (the "registered, painted, still dead to the harness"
    /// class, `tui.md` § Rendering). Its text is the three honest answers:
    /// counted heading when the roster has rows, the empty-state line when the
    /// event is loaded and has none, and a bare heading while the event is not
    /// yet in the cache — never a settled "no attendees" claim we have no basis
    /// for (the un-hydrated-paint finding this queue carries).
    #[test]
    fn event_detail_registers_the_attendee_list_container_in_all_three_states() {
        let mut app = crate::app::tests::test_app();
        app.events.mode = Mode::EventDetail("1".to_string());

        // (a) un-hydrated: the event is not in the cache — no claim about the roster.
        let els = elements(&app);
        assert_eq!(
            els.iter().find(|e| e.id == "attendee-list").unwrap().text,
            "Attendees"
        );

        // (b) loaded, roster genuinely empty.
        app.events.events_cache = vec![ev("1", "aa", "Standup")];
        let els = elements(&app);
        assert_eq!(
            els.iter().find(|e| e.id == "attendee-list").unwrap().text,
            "No attendees yet."
        );
        assert!(!els.iter().any(|e| e.id == "attendee-item"));

        // (c) loaded with rows — the counted heading, web's richest shape.
        let mut row = ev("1", "aa", "Standup");
        row.attendees = vec![AttendeeRow {
            name: "Alice".to_string(),
            email: "alice@example.com".to_string(),
            rsvp: "going".to_string(),
        }];
        app.events.events_cache = vec![row];
        let els = elements(&app);
        assert_eq!(
            els.iter().find(|e| e.id == "attendee-list").unwrap().text,
            "Attendees (1)"
        );
    }

    /// `data.events[]` is ui.yaml's declared state field — a contract, not
    /// decoration. `rsvp_status` is the viewer's own projected status.
    #[test]
    fn state_json_carries_the_declared_event_fields() {
        let mut row = ev("1", "aa", "Standup");
        row.self_rsvp = "going".to_string();
        let state = EventsState {
            events_cache: vec![row],
            ..EventsState::default()
        };
        let json = state_json(&state);
        assert_eq!(json[0]["id"], "1");
        assert_eq!(json[0]["summary"], "Standup");
        assert_eq!(json[0]["start"], "2026-07-20T10:00");
        assert_eq!(json[0]["end"], "2026-07-20T11:00");
        assert_eq!(json[0]["rsvp_status"], "going");
    }

    /// `organizer_email` derives from the handle only when it already carries
    /// `@domain` (the `conv_backend::self_address` convention) — a bare handle
    /// yields an empty organizer rather than a malformed address.
    #[test]
    fn init_derives_organizer_email_only_for_domain_handles() {
        let nest = fauna_client::NestClient::new(
            "http://127.0.0.1:1".to_string(),
            fauna_core::identity::ActorKeypair::generate(),
        );
        let with_domain = init(Arc::clone(&nest), "aa", "alice@example.com", fake_mail());
        assert_eq!(with_domain.organizer_email, "alice@example.com");
        let bare = init(nest, "aa", "alice", fake_mail());
        assert_eq!(bare.organizer_email, "");
    }

    fn events_test_app() -> crate::app::App {
        let mut app = crate::app::tests::test_app();
        let nest = fauna_client::NestClient::new(
            "http://127.0.0.1:1".to_string(),
            fauna_core::identity::ActorKeypair::generate(),
        );
        app.events = init(nest, "aa", "alice@example.com", fake_mail());
        app
    }

    /// An empty mail custody — mail never enabled.
    fn fake_mail() -> Arc<dyn fauna_client_config::MailStore> {
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty())
    }

    /// Opening `event_detail` resets the reminder draft to the first preset
    /// — the `<DropDown>`'s natural default, independent of whether this
    /// event currently has a reminder.
    #[test]
    fn open_event_detail_resets_the_reminder_draft() {
        let mut app = events_test_app();
        app.events.reminder_draft = "P1D".to_string();
        open_event_detail(&mut app, "1".to_string());
        assert_eq!(app.events.reminder_draft, "PT15M");
    }

    /// The reminder select's local write lands synchronously (no `Op`) — it
    /// only applies on the explicit Set click (events.md § Reminders).
    #[test]
    fn set_reminder_offset_is_a_local_buffer_write() {
        let mut app = events_test_app();
        let op = apply_local(&mut app, Action::SetReminderOffset("PT1H".to_string()));
        assert!(op.is_none());
        assert_eq!(app.events.reminder_draft, "PT1H");
    }

    /// `Action::Rsvp` on an open `event_detail` produces `Op::RsvpEvent`
    /// carrying the target event/calendar and the response value fed to
    /// `fauna_client_caldav::apply_rsvp` (note: "decline" the button label
    /// vs. "declined" the wire response — events.md's `partstat_from_fauna`
    /// contract).
    #[test]
    fn rsvp_action_targets_the_open_detail_event() {
        let mut app = events_test_app();
        app.events.events_cache = vec![ev("1", "aa", "Standup")];
        app.events.mode = Mode::EventDetail("1".to_string());
        let Some(Op::RsvpEvent {
            calendar_id,
            uid_hash_hex,
            organizer_email,
            response,
            ..
        }) = apply_local(&mut app, Action::Rsvp(RsvpResponse::Declined))
        else {
            panic!("expected Op::RsvpEvent");
        };
        assert_eq!(calendar_id, "aa");
        assert_eq!(uid_hash_hex, "1");
        assert_eq!(organizer_email, "alice@example.com");
        assert_eq!(response, RsvpResponse::Declined);
    }

    /// `event_detail` paints the invite-by-email form (events.md § User
    /// actions) — the email field and the button.
    #[test]
    fn event_detail_paints_the_invite_form() {
        let mut app = crate::app::tests::test_app();
        app.events.events_cache = vec![ev("1", "aa", "Standup")];
        app.events.mode = Mode::EventDetail("1".to_string());
        let ids: Vec<String> = elements(&app).iter().map(|e| e.id.clone()).collect();
        assert!(ids.contains(&"attendee-invite-field".to_string()));
        assert!(ids.contains(&"attendee-invite-button".to_string()));
    }

    /// Opening `event_detail` clears any stale invite-form draft from a
    /// prior visit, like the reminder draft.
    #[test]
    fn open_event_detail_resets_the_invite_draft() {
        let mut app = events_test_app();
        app.events.attendee_invite_email = "stale@example.com".to_string();
        open_event_detail(&mut app, "1".to_string());
        assert_eq!(app.events.attendee_invite_email, "");
    }

    /// `Action::InviteAttendee` on an open `event_detail` produces
    /// `Op::InviteAttendee` carrying the target event/calendar, the typed
    /// email, and the (absent, in this unit-test app) real conversations
    /// session — the mailbox-less-rail seam `Op::run` branches on.
    #[test]
    fn invite_attendee_action_targets_the_open_detail_event() {
        let mut app = events_test_app();
        app.events.events_cache = vec![ev("1", "aa", "Standup")];
        app.events.mode = Mode::EventDetail("1".to_string());
        app.events.attendee_invite_email = "guest@example.com".to_string();
        let Some(Op::InviteAttendee {
            calendar_id,
            uid_hash_hex,
            organizer_email,
            attendee_email,
            real_session,
            ..
        }) = apply_local(&mut app, Action::InviteAttendee)
        else {
            panic!("expected Op::InviteAttendee");
        };
        assert_eq!(calendar_id, "aa");
        assert_eq!(uid_hash_hex, "1");
        assert_eq!(organizer_email, "alice@example.com");
        assert_eq!(attendee_email, "guest@example.com");
        assert!(real_session.is_none());
    }

    /// `App::set_field` (the dispatcher every driver `/element/type` call
    /// goes through, not just `events::set_field` in isolation) must route
    /// the invite-form field into `EventsState` — the predicate-routing
    /// predecessor's per-page gate missing a variant here silently sent the
    /// write to the wizard's state instead (caught the hard way:
    /// `test_event_invite_attendee[tui]` went 0 attendees despite a
    /// "successful" click, because the typed email never reached
    /// `attendee_invite_email`). The exhaustive `Field` nesting
    /// (`apps/tui.md` § Target state) now makes that class of bug
    /// unrepresentable, but the regression test stays.
    #[test]
    fn set_field_dispatcher_routes_invite_fields_into_events_state() {
        let mut app = events_test_app();
        let _ = app.set_field(
            Field::Events(EventsField::AttendeeInviteEmail),
            "guest@example.com".to_string(),
        );
        assert_eq!(app.events.attendee_invite_email, "guest@example.com");
    }

    // ── Month / week / day views (M5 slice 3 part 4) ──────────────────────

    fn ev_at(id: &str, summary: &str, start: &str, end: &str) -> EventRow {
        EventRow {
            id: id.to_string(),
            calendar_id: "aa".to_string(),
            summary: summary.to_string(),
            start: start.to_string(),
            end: end.to_string(),
            ..Default::default()
        }
    }

    fn ids_of(app: &crate::app::App) -> Vec<String> {
        elements(app).iter().map(|e| e.id.clone()).collect()
    }

    fn count_id(app: &crate::app::App, id: &str) -> usize {
        elements(app).iter().filter(|e| e.id == id).count()
    }

    fn focus_on(app: &mut crate::app::App, y: i32, m: u32, d: u32) {
        app.events.focus_year = y;
        app.events.focus_month = m;
        app.events.focus_day = d;
    }

    /// Month view registers the `events-month-grid` marker AND one
    /// `events-day-cell-{YYYY-MM-DD}` per painted day — the Outlook drill-in's
    /// addressable surface (`ui/events.md` § Layout & flow) — and no agenda
    /// `event-card` while a grid view is active.
    ///
    /// This test used to assert the exact opposite (`!starts_with(
    /// "events-day-cell-")`), which is what made the two `has_day_cell`-gated
    /// e2e tests skip on tui.
    #[test]
    fn month_view_registers_the_grid_marker_and_a_cell_per_day() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Month;
        focus_on(&mut app, 2026, 7, 15);
        let ids = ids_of(&app);
        assert!(ids.contains(&"events-month-grid".to_string()));
        // Six weeks of seven days, every one addressable — adjacent-month cells
        // included, since clicking one is how a user reaches that day directly.
        assert_eq!(
            ids.iter()
                .filter(|id| id.starts_with("events-day-cell-"))
                .count(),
            42,
            "the month grid paints 6 weeks × 7 days, each its own element"
        );
        // The id is the ISO date, zero-padded — what every other app registers
        // and what the shared `EventsActions.has_day_cell` looks up.
        assert!(ids.contains(&"events-day-cell-2026-07-15".to_string()));
        assert!(ids.contains(&"events-day-cell-2026-07-01".to_string()));
        assert!(!ids.contains(&"event-card".to_string()));
    }

    /// The month grid's painted columns follow the locale week start — header
    /// text AND the grid's own first cell together.
    ///
    /// Asserting the header alone would be coarser than the state it guards:
    /// the header is a static rotation, so it would read correctly even if the
    /// cells kept their old order. The first `events-day-cell-*` id is the
    /// finest-grained witness of which day column 0 actually is, so both are
    /// pinned in the same case.
    #[test]
    fn the_month_grids_columns_follow_the_locale_week_start() {
        // July 2026: the 1st is a Wednesday. Monday-start → the grid opens on
        // Mon Jun 29; Sunday-start → Sun Jun 28; Saturday-start → Sat Jun 27.
        for (ws, first_name, first_cell) in [
            (0, "Mon", "events-day-cell-2026-06-29"),
            (6, "Sun", "events-day-cell-2026-06-28"),
            (5, "Sat", "events-day-cell-2026-06-27"),
        ] {
            let mut app = events_test_app();
            app.events.view_mode = ViewMode::Month;
            app.events.week_start = ws;
            focus_on(&mut app, 2026, 7, 15);

            let header = elements(&app)
                .into_iter()
                .find(|e| e.id == "events-month-grid")
                .expect("the month grid registers its marker")
                .text;
            assert!(
                header.starts_with(first_name),
                "week_start {ws}: header should open on {first_name}, got {header:?}"
            );

            let cells: Vec<String> = ids_of(&app)
                .into_iter()
                .filter(|id| id.starts_with("events-day-cell-"))
                .collect();
            assert_eq!(cells.len(), 42, "week_start {ws}: still 6 weeks × 7 days");
            assert_eq!(
                cells[0], first_cell,
                "week_start {ws}: the grid's first column must be the day the header names"
            );
        }
    }

    /// Single press on a day cell = the month→day drill-in: focus moves to that
    /// date and the view switches to Day (`ui/events.md` § Layout & flow).
    #[test]
    fn a_day_cell_single_press_drills_into_day_view_for_that_date() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Month;
        focus_on(&mut app, 2026, 7, 15);

        let cell = elements(&app)
            .into_iter()
            .find(|e| e.id == "events-day-cell-2026-07-09")
            .expect("every painted day is its own element");
        let crate::element::Role::Button(Gesture::Events(action)) = cell.role else {
            panic!("a day cell must carry a single-press gesture");
        };
        apply_local(&mut app, action);

        assert_eq!(app.events.view_mode, ViewMode::Day);
        assert_eq!(app.events.focus(), (2026, 7, 9), "focus follows the cell");
    }

    /// Double press on a day cell opens the new-event compose prefilled with
    /// that cell's date — the second half of the Outlook model, and a
    /// *different* action from the single press (`ui/events.md` § Layout &
    /// flow; § Where logic lives: "client glue prefills the compose
    /// `event-form` `dtstart` with the cell's date").
    #[test]
    fn a_day_cell_double_press_opens_the_compose_prefilled_with_that_date() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Month;
        focus_on(&mut app, 2026, 7, 15);

        let cell = elements(&app)
            .into_iter()
            .find(|e| e.id == "events-day-cell-2026-07-09")
            .expect("every painted day is its own element");
        let Some(Gesture::Events(action)) = cell.dbl else {
            panic!("a day cell must carry a SECOND gesture, distinct from its first");
        };
        apply_local(&mut app, action);

        assert_eq!(app.events.mode, Mode::CreateEvent);
        // The date is what the goal doc pins; assert the VALUE, not merely that
        // the buffer is non-empty — a presence check here would pass on a stale
        // draft, on today's date, and on the wrong cell alike.
        assert!(
            app.events.new_event_dtstart.starts_with("2026-07-09"),
            "dtstart must be prefilled with the CELL's date, got {:?}",
            app.events.new_event_dtstart
        );
        // Painted, too — `event-dtstart` is what the e2e reads.
        let painted = elements(&app)
            .into_iter()
            .find(|e| e.id == "event-dtstart")
            .map(|e| e.text)
            .expect("the compose paints its start field");
        assert!(
            painted.starts_with("2026-07-09"),
            "the painted field must carry the prefill, got {painted:?}"
        );
    }

    /// The two presses must not collapse into one meaning: a cell's single-press
    /// gesture and its double-press gesture are different actions on the same
    /// element. This is the property `testing.md` point 11 protects — an agent
    /// that aliased `double_click` onto `click` would let a caller believe the
    /// double-press arm ran.
    #[test]
    fn a_day_cells_two_gestures_are_distinct() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Month;
        focus_on(&mut app, 2026, 7, 15);

        let cell = elements(&app)
            .into_iter()
            .find(|e| e.id == "events-day-cell-2026-07-09")
            .expect("every painted day is its own element");
        let crate::element::Role::Button(single) = cell.role.clone() else {
            panic!("a day cell must carry a single-press gesture");
        };
        let double = cell.dbl.clone().expect("and a double-press gesture");
        assert_ne!(
            format!("{single:?}"),
            format!("{double:?}"),
            "drill-in and compose must stay two different actions"
        );
    }

    /// The `calendar-date-label` tracks the view mode: month → "July 2026".
    #[test]
    fn date_label_is_the_month_name_in_month_view() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Month;
        focus_on(&mut app, 2026, 7, 15);
        let label = elements(&app)
            .into_iter()
            .find(|e| e.id == "calendar-date-label")
            .map(|e| e.text)
            .unwrap();
        assert_eq!(label, "July 2026");
    }

    /// Week view registers `calendar-week-grid` and one `calendar-event-block`
    /// per timed event *inside the week window* — an event months out renders
    /// no block.
    #[test]
    fn week_view_blocks_only_events_in_the_week() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Week;
        focus_on(&mut app, 2026, 7, 15); // Wed; Mon-start week Jul 13–19
        app.events.events_cache = vec![
            ev_at("1", "Standup", "2026-07-15T10:00", "2026-07-15T11:00"),
            ev_at("2", "Faraway", "2026-09-20T10:00", "2026-09-20T11:00"),
        ];
        assert!(ids_of(&app).contains(&"calendar-week-grid".to_string()));
        assert_eq!(count_id(&app, "calendar-event-block"), 1);
    }

    /// Day view registers `calendar-day-timeline` and a block only for a timed
    /// event on the focused day.
    #[test]
    fn day_view_blocks_only_events_on_the_day() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Day;
        focus_on(&mut app, 2026, 7, 15);
        app.events.events_cache = vec![
            ev_at("1", "Review", "2026-07-15T14:00", "2026-07-15T15:00"),
            ev_at("2", "Tomorrow", "2026-07-16T14:00", "2026-07-16T15:00"),
        ];
        assert!(ids_of(&app).contains(&"calendar-day-timeline".to_string()));
        assert_eq!(count_id(&app, "calendar-event-block"), 1);
    }

    /// An all-day event lands in `calendar-allday-band`, never as a timed block.
    #[test]
    fn all_day_event_goes_to_the_band_not_a_block() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Day;
        focus_on(&mut app, 2026, 7, 15);
        // Date-only start + a date-only next-day end → is_all_day.
        app.events.events_cache = vec![ev_at("1", "Holiday", "2026-07-15", "2026-07-16")];
        assert!(ids_of(&app).contains(&"calendar-allday-band".to_string()));
        assert_eq!(count_id(&app, "calendar-event-block"), 0);
    }

    /// Overlapping timed events carry the shared `find_overlaps` column
    /// annotation `[col/total]` (the one piece of shared layout math).
    #[test]
    fn overlapping_blocks_carry_the_column_annotation() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Day;
        focus_on(&mut app, 2026, 7, 15);
        app.events.events_cache = vec![
            ev_at("1", "A", "2026-07-15T10:00", "2026-07-15T11:00"),
            ev_at("2", "B", "2026-07-15T10:30", "2026-07-15T11:30"),
        ];
        let blocks: Vec<String> = elements(&app)
            .into_iter()
            .filter(|e| e.id == "calendar-event-block")
            .map(|e| e.text)
            .collect();
        assert_eq!(blocks.len(), 2);
        assert!(
            blocks.iter().all(|b| b.contains("/2]")),
            "expected column annotations, got {blocks:?}"
        );
    }

    /// A 22:00→02:00 (next-day) event clamps to the day boundary via the
    /// shared `day_column_layout` (events.md § Where logic lives) — never the
    /// collapsed 30-minute block the pre-lift hand-rolled interval math
    /// produced by reading only the end's time-of-day and ignoring its date.
    #[test]
    fn midnight_crossing_event_clamps_to_the_day_boundary() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Day;
        focus_on(&mut app, 2026, 7, 15);
        app.events.events_cache = vec![ev_at(
            "1",
            "Overnight",
            "2026-07-15T22:00",
            "2026-07-16T02:00",
        )];
        let block = elements(&app)
            .into_iter()
            .find(|e| e.id == "calendar-event-block")
            .map(|e| e.text)
            .expect("expected a calendar-event-block");
        assert!(
            block.starts_with("22:00\u{2013}24:00"),
            "expected the block clamped to the day boundary (22:00–24:00), got {block:?}"
        );
    }

    /// A timed block opens `event_detail`, the other half of the same
    /// events.md § Week & day timeline views sentence the slot work above
    /// implements (and ui.yaml's own `calendar-event-block` description:
    /// "click → event_detail"). It was an inert label until 2026-08-02 while
    /// the agenda `event-card` beside it had carried the gesture all along.
    #[test]
    fn a_timed_block_opens_the_event_detail() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Day;
        focus_on(&mut app, 2026, 7, 15);
        app.events.events_cache = vec![ev_at(
            "e1",
            "Review",
            "2026-07-15T14:00",
            "2026-07-15T15:00",
        )];
        let block = elements(&app)
            .into_iter()
            .find(|e| e.id == "calendar-event-block")
            .expect("expected a calendar-event-block");
        match block.role {
            crate::element::Role::Button(Gesture::OpenEventDetail(id)) => assert_eq!(id, "e1"),
            other => panic!("a timed block must open its own event's detail, got {other:?}"),
        }
    }

    /// The day timeline tiles one `events-time-slot-{HH-MM}` per quarter hour —
    /// the ratified empty-slot quick-create surface (`ui/events.md` § Week &
    /// day timeline views), 96 of them, addressable by the shared
    /// `EventsActions.has_time_slot`/`click_time_slot`.
    #[test]
    fn the_day_timeline_tiles_a_quarter_hour_slot_across_the_whole_day() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Day;
        focus_on(&mut app, 2026, 7, 15);
        let slots: Vec<String> = ids_of(&app)
            .into_iter()
            .filter(|id| id.starts_with("events-time-slot-"))
            .collect();
        assert_eq!(slots.len(), 96, "24h at 15-minute granularity");
        assert_eq!(slots.first().unwrap(), "events-time-slot-00-00");
        assert_eq!(slots.last().unwrap(), "events-time-slot-23-45");
        assert!(slots.contains(&"events-time-slot-09-15".to_string()));
    }

    /// The week grid repeats the slot ids per day column — seven columns, so
    /// seven `events-time-slot-13-30`s, exactly as linux tiles its 96 markers
    /// in every column. The driver resolves the first showing match, which is
    /// why the week e2e asserts only the TIME half of the prefill.
    #[test]
    fn the_week_grid_tiles_the_slot_ids_once_per_day_column() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Week;
        focus_on(&mut app, 2026, 7, 15);
        assert_eq!(count_id(&app, "events-time-slot-13-30"), 7);
    }

    /// Clicking an empty slot opens the compose prefilled with that column's
    /// date AND the slot's snapped time — the assertion the day-view e2e makes
    /// against `event-dtstart`.
    #[test]
    fn an_empty_slot_composes_at_its_own_date_and_time() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Day;
        focus_on(&mut app, 2026, 7, 15);
        let slot = elements(&app)
            .into_iter()
            .find(|e| e.id == "events-time-slot-09-15")
            .expect("expected a 09:15 slot");
        let Some(crate::element::Role::Button(Gesture::Events(action))) = Some(slot.role) else {
            panic!("an empty slot must be clickable");
        };
        apply_local(&mut app, action);
        assert_eq!(app.events.mode, Mode::CreateEvent);
        assert_eq!(app.events.new_event_dtstart, "2026-07-15T09:15");
    }

    /// A slot an event occupies is NOT a quick-create target: it paints as
    /// untagged chrome, so a click on a busy slot cannot spuriously open a
    /// compose — the bug linux's column-level background gesture shipped until
    /// its 2026-08-01 slot-marker fix. The free slots around it are unaffected.
    #[test]
    fn a_slot_an_event_occupies_is_not_a_quick_create_target() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Day;
        focus_on(&mut app, 2026, 7, 15);
        app.events.events_cache = vec![ev_at(
            "1",
            "Slotted",
            "2026-07-15T14:00",
            "2026-07-15T15:00",
        )];
        let ids = ids_of(&app);
        for busy in ["14-00", "14-15", "14-30", "14-45"] {
            assert!(
                !ids.contains(&format!("events-time-slot-{busy}")),
                "{busy} is inside the 14:00–15:00 event, so it is not empty space"
            );
        }
        assert!(ids.contains(&"events-time-slot-09-15".to_string()));
        assert!(
            ids.contains(&"events-time-slot-15-00".to_string()),
            "the event's exclusive end is free again"
        );
    }

    /// The month cell's double-press carries no time, so it keeps prefilling
    /// the client's default working hour — the `ComposeOnDay { at: None }` arm,
    /// pinned so the slot work above cannot quietly change the month drill-in.
    #[test]
    fn a_month_cell_compose_keeps_the_default_working_hour() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Month;
        focus_on(&mut app, 2026, 7, 15);
        apply_local(
            &mut app,
            Action::ComposeOnDay {
                y: 2026,
                m: 7,
                d: 20,
                at: None,
            },
        );
        assert_eq!(
            app.events.new_event_dtstart,
            format!("2026-07-20T{}", default_compose_hour())
        );
    }

    /// Entering Week or Day view seats the focus ring on the ~08:00 slot, which
    /// is what scrolls the 96-row axis to the working day (`ui/events.md`
    /// § Week & day timeline views — "auto-scroll to ~08:00 on first render").
    /// Without it the axis opens at 00:00 and the whole quick-create affordance
    /// is off-screen, unclickable in a terminal.
    #[test]
    fn entering_a_time_grid_opens_it_at_the_working_start() {
        // The ring indexes `App::page_elements`, so this one needs the app
        // actually *on* the Events page rather than the bare events state the
        // element-shape tests above assert against.
        let mut app = events_test_app();
        app.session = Some(crate::app::tests::test_session());
        app.page = crate::pages::Page::Events;
        app.zone = crate::app::Zone::Page;
        focus_on(&mut app, 2026, 7, 15);
        app.focus = 0;
        apply_local(&mut app, Action::SwitchView(ViewMode::Day));
        assert_eq!(
            app.focused().map(|e| e.id).as_deref(),
            Some("events-time-slot-08-00")
        );
    }

    /// Switching to a view with no time axis leaves the ring alone — the
    /// seating above must not fire on agenda/month.
    #[test]
    fn entering_the_month_view_does_not_move_the_focus_ring() {
        let mut app = events_test_app();
        focus_on(&mut app, 2026, 7, 15);
        app.focus = 2;
        apply_local(&mut app, Action::SwitchView(ViewMode::Month));
        assert_eq!(app.focus, 2);
    }

    /// Every week of the month grid begins its own painted row. Before this the
    /// 42 cells formed one unbroken inline run and the six weeks painted end to
    /// end on a single clipped line — invisible to an e2e suite that reads the
    /// registry rather than the pixels (`crate::element::Element::starts_row`).
    #[test]
    fn each_week_of_the_month_grid_starts_its_own_painted_row() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Month;
        focus_on(&mut app, 2026, 7, 15);
        let cells: Vec<bool> = elements(&app)
            .into_iter()
            .filter(|e| e.id.starts_with("events-day-cell-"))
            .map(|e| e.starts_row)
            .collect();
        assert_eq!(cells.len(), 42, "six weeks of seven days");
        for (i, starts_row) in cells.iter().enumerate() {
            assert_eq!(
                *starts_row,
                i % 7 == 0,
                "cell {i} should {}start a row",
                if i % 7 == 0 { "" } else { "not " }
            );
        }
    }

    /// Split a painted line into its column bands: drop the paint gutter, then
    /// chunk what remains into `width`-wide cells. Every geometry assertion
    /// below reads the grid through this, so a row that lost a cell shows up as
    /// a band count rather than as a string diff nobody can read.
    fn bands(line: &str, width: usize) -> Vec<String> {
        let cols: Vec<char> = line.chars().skip(PAINT_GUTTER).collect();
        cols.chunks(width).map(|c| c.iter().collect()).collect()
    }

    /// Every painted line carries the same two-column prefix — `"  "`, or `"> "`
    /// on the focused element. Equal width either way is exactly what lets a
    /// grid's rows align under a header painted by a *different* element.
    const PAINT_GUTTER: usize = 2;

    /// The month grid paints **six week rows of seven equal cells**, aligned
    /// under the weekday header — asserted at the pixels, not at the registry.
    ///
    /// This is the assertion the month grid did not have when it shipped all 42
    /// day cells on ONE clipped line: the ids were all present and correct, so
    /// every registry test (and the whole e2e suite, which reads the registry on
    /// every app) stayed green. `crate::ui::painted_line_texts` is the seam that
    /// can see it; `Element::starts_row` is the mechanism that fixes it.
    #[test]
    fn the_month_grid_paints_six_week_rows_under_an_aligned_weekday_header() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Month;
        focus_on(&mut app, 2026, 7, 15);

        let mut grid = Vec::new();
        super::grids::render_month(&app.events, &app.events.displayed_events(), &mut grid);
        let painted = crate::ui::painted_line_texts(&grid);

        assert_eq!(
            painted.len(),
            7,
            "the weekday header plus six week rows, each on its own painted line: {painted:#?}"
        );
        for (row, line) in painted.iter().enumerate() {
            let cells = bands(line, MONTH_CELL_WIDTH);
            assert_eq!(
                cells.len(),
                7,
                "row {row} paints {} column bands, not seven: {line:?}",
                cells.len()
            );
            for (column, cell) in cells.iter().enumerate() {
                assert_eq!(
                    cell.chars().count(),
                    MONTH_CELL_WIDTH,
                    "row {row} column {column} is not a full cell — the row is short, so every \
                     column after it has slid left: {line:?}"
                );
                assert!(
                    !cell.trim().is_empty(),
                    "row {row} column {column} painted blank: {line:?}"
                );
            }
        }
    }

    /// The week time axis paints one line per quarter-hour slot, each carrying
    /// the hour gutter plus **seven** equal day columns — and the column header
    /// row lands on the same bands, which is the whole point of naming them.
    #[test]
    fn the_week_time_axis_paints_one_row_per_slot_with_seven_aligned_day_columns() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Week;
        focus_on(&mut app, 2026, 7, 15);

        let mut grid = Vec::new();
        super::grids::render_week(&app.events, &app.events.displayed_events(), &mut grid);
        let painted = crate::ui::painted_line_texts(&grid);

        let axis_rows = SLOTS_PER_DAY as usize;
        assert_eq!(
            painted.len(),
            2 + axis_rows,
            "the week label, the day-name header, and one line per quarter-hour slot"
        );
        let width = PAINT_GUTTER + GUTTER_WIDTH + 7 * WEEK_COLUMN_WIDTH;
        for (row, line) in painted.iter().enumerate().skip(1) {
            assert_eq!(
                line.chars().count(),
                width,
                "axis row {row} is not the gutter plus seven {WEEK_COLUMN_WIDTH}-wide columns: \
                 {line:?}"
            );
        }
        // The header names seven columns, and they land on the SAME bands the
        // slot rows below use — a header on its own bands names the wrong days.
        let day_columns = |line: &str| -> Vec<String> {
            let past_gutter: String = line.chars().skip(PAINT_GUTTER + GUTTER_WIDTH).collect();
            bands(
                &format!("{:width$}{past_gutter}", "", width = PAINT_GUTTER),
                WEEK_COLUMN_WIDTH,
            )
        };
        let header = day_columns(&painted[1]);
        assert_eq!(
            header.len(),
            7,
            "one named column per day: {:?}",
            painted[1]
        );
        for (column, name) in header.iter().enumerate() {
            assert!(
                !name.trim().is_empty(),
                "day column {column} is unnamed: {:?}",
                painted[1]
            );
        }
        assert_eq!(
            day_columns(&painted[2]).len(),
            7,
            "the first slot row must carry the same seven bands as the header: {:?}",
            painted[2]
        );

        // The gutter carries `HH:00` on the hour and nothing on the three
        // quarter rows between — the gridline reading the axis is built on.
        for slot in 0..axis_rows {
            let line = &painted[2 + slot];
            let gutter: String = line.chars().skip(PAINT_GUTTER).take(GUTTER_WIDTH).collect();
            let minute = slot as u32 * SLOT_MINUTES;
            if minute.is_multiple_of(60) {
                assert_eq!(
                    gutter.trim(),
                    format!("{:02}:00", minute / 60),
                    "slot {slot} is on the hour and must label it: {line:?}"
                );
            } else {
                assert!(
                    gutter.trim().is_empty(),
                    "slot {slot} is a quarter row and must leave the gutter blank: {line:?}"
                );
            }
        }
    }

    /// The day timeline is the same axis over ONE column — one painted line per
    /// slot, the single column wide enough for a real summary. The event block
    /// above the axis keeps the canonical `get_text` string; the axis only says
    /// *when* the day is busy.
    #[test]
    fn the_day_time_axis_paints_one_row_per_slot_with_a_single_wide_column() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Day;
        focus_on(&mut app, 2026, 7, 15);

        let mut grid = Vec::new();
        super::grids::render_day(&app.events, &app.events.displayed_events(), &mut grid);
        let painted = crate::ui::painted_line_texts(&grid);

        let axis_rows = SLOTS_PER_DAY as usize;
        assert_eq!(
            painted.len(),
            1 + axis_rows,
            "the day label and one line per quarter-hour slot"
        );
        let width = PAINT_GUTTER + GUTTER_WIDTH + DAY_COLUMN_WIDTH;
        for (row, line) in painted.iter().enumerate().skip(1) {
            assert_eq!(
                line.chars().count(),
                width,
                "axis row {row} is not the gutter plus one {DAY_COLUMN_WIDTH}-wide column: {line:?}"
            );
        }
    }

    /// One agenda card is three painted lines — the card button, its summary,
    /// and the RSVP trio **on a single row**. Ten events would otherwise run
    /// fifty lines; `Element::inline` is what buys the row back, and nothing but
    /// a paint assertion can tell whether it is still doing so.
    #[test]
    fn each_agenda_cards_rsvp_trio_paints_on_one_row() {
        let mut app = events_test_app();
        app.events.events_cache = vec![ev("1", "aa", "Standup"), ev("2", "aa", "Retro")];

        let mut cards = Vec::new();
        super::agenda::render_agenda(&app.events.displayed_events(), &mut cards);
        let painted = crate::ui::painted_line_texts(&cards);

        assert_eq!(
            painted.len(),
            3 * app.events.events_cache.len(),
            "three painted lines per card — button, summary, and ONE rsvp row: {painted:#?}"
        );
        for (card, summary) in ["Standup", "Retro"].iter().enumerate() {
            assert!(
                painted[card * 3].contains(summary),
                "card {card} opens with its own button: {:?}",
                painted[card * 3]
            );
            let rsvp = &painted[card * 3 + 2];
            for label in [
                fauna_i18n::strings::events::rsvp::GOING,
                fauna_i18n::strings::events::rsvp::INTERESTED,
                fauna_i18n::strings::events::rsvp::DECLINE,
            ] {
                assert!(
                    rsvp.contains(label),
                    "card {card}'s rsvp row must carry {label:?} — all three share one line: \
                     {rsvp:?}"
                );
            }
        }
    }

    /// `events-next-month` pans the focus month forward (wrapping the year) and
    /// `events-prev-month` reverses it — the `test_month_navigation` contract.
    #[test]
    fn month_nav_advances_and_reverses_the_focus_month() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Month;
        focus_on(&mut app, 2026, 12, 15);
        apply_local(&mut app, Action::NextMonth);
        assert_eq!((app.events.focus_year, app.events.focus_month), (2027, 1));
        apply_local(&mut app, Action::PrevMonth);
        assert_eq!((app.events.focus_year, app.events.focus_month), (2026, 12));
    }

    /// Month nav clamps the day into a shorter month (Jan 31 → Feb 28, 2026
    /// not being a leap year).
    #[test]
    fn month_nav_clamps_the_day_into_a_shorter_month() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Month;
        focus_on(&mut app, 2026, 1, 31);
        apply_local(&mut app, Action::NextMonth);
        assert_eq!((app.events.focus_month, app.events.focus_day), (2, 28));
    }

    /// **The pan control moves one VISIBLE RANGE, not always a month** — the
    /// cross-app divergence the shared `caltime::pan` policy resolves
    /// (`events.md` § User actions: "pan visible range"). tui previously walked
    /// a whole month in Week and Day view, so one click on the week grid jumped
    /// roughly four weeks.
    #[test]
    fn pan_moves_one_visible_range_per_view_mode() {
        for (mode, forward, backward) in [
            (ViewMode::Month, (2026, 8, 15), (2026, 6, 15)),
            (ViewMode::Week, (2026, 7, 22), (2026, 7, 8)),
            (ViewMode::Day, (2026, 7, 16), (2026, 7, 14)),
        ] {
            let mut app = events_test_app();
            app.events.view_mode = mode;
            focus_on(&mut app, 2026, 7, 15);
            apply_local(&mut app, Action::NextMonth);
            assert_eq!(app.events.focus(), forward, "forward pan in {mode:?}");
            focus_on(&mut app, 2026, 7, 15);
            apply_local(&mut app, Action::PrevMonth);
            assert_eq!(app.events.focus(), backward, "backward pan in {mode:?}");
        }
    }

    /// Panning in Agenda does nothing: the agenda list is date-unfiltered
    /// (`agenda.rs`), so the focus date it would move is not observable there.
    /// Moving it anyway is what made tui's `calendar-date-label` advance above
    /// a list that never changed.
    #[test]
    fn pan_is_inert_in_the_date_unfiltered_agenda() {
        let mut app = events_test_app();
        app.events.view_mode = ViewMode::Agenda;
        focus_on(&mut app, 2026, 7, 15);
        let before = date_label(&app.events);
        apply_local(&mut app, Action::NextMonth);
        assert_eq!(app.events.focus(), (2026, 7, 15));
        assert_eq!(date_label(&app.events), before);
    }

    /// `CreateEventSubmit` falls back to the first calendar when none is
    /// explicitly selected (linux parity — the grid views create without a
    /// prior `select_calendar`).
    #[test]
    fn create_event_falls_back_to_the_first_calendar() {
        let mut app = events_test_app();
        app.events.calendars = vec![cal("aa", "Work"), cal("bb", "Home")];
        app.events.selected_calendar = None;
        app.events.new_event_summary = "X".to_string();
        let Some(Op::CreateEvent { calendar_id, .. }) =
            apply_local(&mut app, Action::CreateEventSubmit)
        else {
            panic!("expected Op::CreateEvent");
        };
        assert_eq!(calendar_id, "aa");
    }

    /// `CreateEventSubmit` normalizes the raw `event-dtstart`/`event-dtend`
    /// text buffers through the shared A2 rule (events.md § Where logic
    /// lives) — a bare `…THH:MM` gets padded to `…THH:MM:00` before it
    /// reaches the nest, matching the other 6 clients.
    #[test]
    fn create_event_submit_normalizes_the_datetime_buffers() {
        let mut app = events_test_app();
        app.events.calendars = vec![cal("aa", "Work")];
        app.events.new_event_summary = "X".to_string();
        app.events.new_event_dtstart = "2026-07-20T10:00".to_string();
        app.events.new_event_dtend = "2026-07-20 11:00".to_string();
        let Some(Op::CreateEvent { dtstart, dtend, .. }) =
            apply_local(&mut app, Action::CreateEventSubmit)
        else {
            panic!("expected Op::CreateEvent");
        };
        assert_eq!(dtstart, "2026-07-20T10:00:00");
        assert_eq!(dtend, "2026-07-20T11:00:00");
    }

    /// The three date labels format as linux's `time_utils` does (chrono
    /// supplies the English names).
    #[test]
    fn date_labels_format_correctly() {
        assert_eq!(format_month_label(2026, 7), "July 2026");
        assert_eq!(
            format_week_label(2026, 7, 15, 0),
            "Jul 13 \u{2013} 19, 2026"
        );
        assert_eq!(format_day_label(2026, 7, 15), "Wednesday, Jul 15, 2026");
    }

    /// The week range label MOVES with the locale's week start — the half of
    /// the fix linux shipped wrong (its grid read the locale, its label
    /// hardcoded Monday, so the two named different weeks).
    ///
    /// 2026-07-15 is a Wednesday. Monday-start → Jul 13–19; Sunday-start →
    /// Jul 12–18; Saturday-start → Jul 11–17. An assertion on the label alone
    /// would be coarser than the state it guards, so each case also pins the
    /// grid's own first day, which is what the label must agree with.
    #[test]
    fn the_week_label_and_the_week_grid_agree_on_every_locale_week_start() {
        use fauna_core::caltime::{CalendarViewMode, visible_days};

        for (ws, expected_label, expected_first_day) in [
            (0, "Jul 13 \u{2013} 19, 2026", (2026, 7, 13)),
            (6, "Jul 12 \u{2013} 18, 2026", (2026, 7, 12)),
            (5, "Jul 11 \u{2013} 17, 2026", (2026, 7, 11)),
        ] {
            assert_eq!(
                format_week_label(2026, 7, 15, ws),
                expected_label,
                "week label for week_start {ws}"
            );
            let days = visible_days(CalendarViewMode::Week, (2026, 7, 15), ws);
            assert_eq!(
                days[0], expected_first_day,
                "grid's own first day for week_start {ws} — the label above must name this week"
            );
        }
    }

    // ── No-selection union (events.md § Implementation status today) ──────

    /// No calendar selected → every owned calendar is in scope (the union
    /// rule the other 6 clients already fixed 2026-07-16→18).
    #[test]
    fn calendars_in_scope_unions_all_when_none_selected() {
        let cals = vec![cal("aa", "Work"), cal("bb", "Home")];
        let scoped = calendars_in_scope(&cals, &None);
        assert_eq!(
            scoped.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["aa", "bb"]
        );
    }

    /// An explicit, still-live selection narrows to just that one calendar.
    #[test]
    fn calendars_in_scope_narrows_to_the_selected_calendar() {
        let cals = vec![cal("aa", "Work"), cal("bb", "Home")];
        let scoped = calendars_in_scope(&cals, &Some("bb".to_string()));
        assert_eq!(
            scoped.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["bb"]
        );
    }

    /// A selection whose target calendar no longer exists (deleted elsewhere)
    /// falls back to the union rather than silently scoping to nothing.
    #[test]
    fn calendars_in_scope_unions_when_the_selected_calendar_no_longer_exists() {
        let cals = vec![cal("aa", "Work")];
        let scoped = calendars_in_scope(&cals, &Some("stale-id".to_string()));
        assert_eq!(
            scoped.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["aa"]
        );
    }

    /// `Outcome::CalendarsAndEventsLoaded` (the nav-enter/session-start
    /// hydration reply) replaces both the calendar list and the events union,
    /// and clears a prior page error.
    #[test]
    fn calendars_and_events_loaded_replaces_both_lists() {
        let mut app = events_test_app();
        app.errors.insert(Page::Events, "stale error".to_string());
        let query_gen = app.events.events_gen.get();
        apply_outcome(
            &mut app,
            Outcome::CalendarsAndEventsLoaded {
                calendars: vec![cal("aa", "Work"), cal("bb", "Home")],
                events: vec![
                    ev_at("1", "A", "2026-07-15T10:00", "2026-07-15T11:00"),
                    ev_at("2", "B", "2026-07-16T10:00", "2026-07-16T11:00"),
                ],
                refused_changes: vec![],
                query_gen,
            },
        );
        assert_eq!(app.events.calendars.len(), 2);
        assert_eq!(app.events.events_cache.len(), 2);
        assert!(!app.errors.contains_key(&Page::Events));
    }

    /// The apple bug, reproduced structurally on tui: a slow
    /// no-selection-union `RefreshCalendars` (dispatched at nav-entry, before
    /// any selection) must not clobber a faster single-calendar
    /// `RefreshEvents` that started — and resolved — later. Without the
    /// `events_gen` guard the union's stale reply would unconditionally
    /// overwrite `app.events.events_cache`, silently re-widening a just-narrowed
    /// view.
    #[test]
    fn a_stale_calendars_and_events_loaded_does_not_clobber_a_fresher_selection() {
        let mut app = events_test_app();
        // The union fetch claims query_gen 1 at nav-entry...
        let union_gen = app.events.events_gen.get() + 1;
        app.events.events_gen.set(union_gen);
        // ...then the user selects "bb", claiming query_gen 2 and resolving first.
        let select_gen = app.events.events_gen.get() + 1;
        app.events.events_gen.set(select_gen);
        app.events.selected_calendar = Some("bb".to_string());
        apply_outcome(
            &mut app,
            Outcome::EventsLoaded {
                calendar_id: "bb".to_string(),
                events: vec![ev_at("1", "Fresh", "2026-07-15T10:00", "2026-07-15T11:00")],
                query_gen: select_gen,
            },
        );
        // The slower union now resolves, carrying the OLDER query_gen — it must be dropped.
        apply_outcome(
            &mut app,
            Outcome::CalendarsAndEventsLoaded {
                calendars: vec![cal("aa", "Work"), cal("bb", "Home")],
                events: vec![
                    ev_at("1", "Fresh", "2026-07-15T10:00", "2026-07-15T11:00"),
                    ev_at("2", "Stale-union", "2026-07-16T10:00", "2026-07-16T11:00"),
                ],
                refused_changes: vec![],
                query_gen: union_gen,
            },
        );
        assert_eq!(
            app.events.events_cache.len(),
            1,
            "the stale union must not re-widen the view"
        );
        assert_eq!(app.events.events_cache[0].summary, "Fresh");
    }

    /// The reverse ordering: a stale `RefreshEvents` (an earlier selection,
    /// superseded before it resolved) must not clobber a fresher
    /// `RefreshCalendars` union that started later and resolved first.
    #[test]
    fn a_stale_events_loaded_does_not_clobber_a_fresher_union() {
        let mut app = events_test_app();
        // An earlier selection of "bb" claims query_gen 1...
        let stale_select_gen = app.events.events_gen.get() + 1;
        app.events.events_gen.set(stale_select_gen);
        // ...then a fresh nav-entry union claims query_gen 2 and resolves first.
        let union_gen = app.events.events_gen.get() + 1;
        app.events.events_gen.set(union_gen);
        apply_outcome(
            &mut app,
            Outcome::CalendarsAndEventsLoaded {
                calendars: vec![cal("aa", "Work"), cal("bb", "Home")],
                events: vec![
                    ev_at("1", "A", "2026-07-15T10:00", "2026-07-15T11:00"),
                    ev_at("2", "B", "2026-07-16T10:00", "2026-07-16T11:00"),
                ],
                refused_changes: vec![],
                query_gen: union_gen,
            },
        );
        // The stale "bb" selection now resolves, carrying the OLDER query_gen.
        app.events.selected_calendar = Some("bb".to_string());
        apply_outcome(
            &mut app,
            Outcome::EventsLoaded {
                calendar_id: "bb".to_string(),
                events: vec![ev_at(
                    "3",
                    "Stale-select",
                    "2026-07-17T10:00",
                    "2026-07-17T11:00",
                )],
                query_gen: stale_select_gen,
            },
        );
        assert_eq!(
            app.events.events_cache.len(),
            2,
            "the stale select must not narrow over the fresh union"
        );
    }

    /// The nav-enter/session-start hook: `Some` whenever a session is live,
    /// carrying the current selection along so a returning visit to an
    /// explicitly-selected calendar doesn't silently widen to the union.
    #[test]
    fn nav_enter_op_carries_the_current_selection() {
        let mut app = events_test_app();
        app.events.selected_calendar = Some("bb".to_string());
        let Some(Op::RefreshCalendars {
            selected_calendar, ..
        }) = nav_enter_op(&app.events, None)
        else {
            panic!("expected Op::RefreshCalendars");
        };
        assert_eq!(selected_calendar, Some("bb".to_string()));
    }
}
