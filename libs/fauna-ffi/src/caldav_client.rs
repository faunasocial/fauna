//! UniFFI façade for the **encrypted-CalDAV Events path** (events.md Decision B):
//! the Events page reads/writes the actor's OWN calendars + events over the
//! encrypted `bridge_caldav_*` store via the `fauna.bridges.*` calendar RPCs +
//! client-side seal/unseal — the SAME store + sealing scheme the mail-bridge MDA
//! serves to Apple Calendar, so a Fauna app and a CalDAV MUA see the same data
//! (events.md § Persistence, caldav-server.md § Event resources).
//!
//! **Not** the legacy `fauna.{calendars,events}.*` plaintext-`content`-table path
//! (the `calendars_client.rs` / `events_client.rs` seams) — Decision B retires it.
//!
//! [`FfiCaldavClient`] wraps `fauna_client_caldav::CalDavClient` (which the
//! Rust-native Linux app calls directly) so Apple / Windows / Android reach the
//! identical surface over UniFFI. It mirrors the Linux lead's
//! `apps/fauna-linux/src/client.rs` caldav orchestration + `views/events/
//! caldav_backend.rs` maps (Step 4b) — the per-op flows (lazy-`Personal`
//! provisioning, read-mutate-rewrite RSVP / reminder / attendee, iMIP fan-out, ICS
//! import/export) live here so the Kotlin/Swift shell stays a thin renderer
//! (priority #2). Construct via [`crate::nest_client::FfiNestClient::caldav`].
//!
//! ## msek sourcing (the events-store gate)
//!
//! The encrypted body + sidecar are sealed/unsealed with `cfg.mail.msek`
//! (the MSEK in the `fauna.state.mail` row, minted only by `enable_mail`). This seam sources it
//! **internally**, via the shared [`fauna_client_config::dav_store_context`]
//! (also tui's and linux's `caldav_context`): rebuild the actor's keypair from
//! the connection's secret, load the actor's mail custody row, take
//! its MSEK. `None` (localhost / IP nest with no mail enabled) **degrades
//! gracefully** — reads return an empty list, writes return a `"Calendar
//! requires mail to be enabled"` error — so the Kotlin layer never touches key
//! material and an un-provisioned nest never crashes the page.
//!
//! Mirror convention (matching `snapshots_client.rs` / `events_client.rs`): only
//! the fields the Events page renders are mirrored. `attendance_mode` / `capacity`
//! (legacy Fauna-social fields with no canonical VEVENT home) are dropped — the
//! encrypted path is RFC-5545 VEVENT only (matches Linux's `event_fields_from_params`).

use std::sync::{Arc, Mutex};

use fauna_client::NestClient;
use fauna_client_account::AccountClient;
use fauna_client_caldav::{
    AnonAttendeeDiscovery, CalDavClient, CalendarMetadata, DavRecipientKeys, DecodedEvent,
    DecodedEventsPage, EventFields, add_attendee, apply_rsvp,
    bridge_routing::{
        DeleteEventRequest, ListCalendarsRequest, ProvisionCalendarRequest, QueryEventsRequest,
    },
    delta_sync::{BackstopVerdict, CalendarSyncTokens, backstop_probe},
    dispatch_imip_request, imip_inputs, imip_reply_for_rsvp, imip_request_for_invite, parse_ical,
    parse_ical_attendees, parse_ical_organizer, personal_calendar_id, project_attendee_rsvp,
    seal_calendar_metadata, set_reminder, uid_hash, unseal_calendar_metadata,
};
#[cfg(feature = "account-runtime")]
use fauna_client_config::dav_store_context as shared_dav_store_context;
use fauna_client_conversations::NestImipDispatch;
use fauna_client_email::EmailClient;
use fauna_conversations::ConversationsSession;
use fauna_protocol::RpcRequester;
use fauna_protocol::discovery::{SetupStatusReply, SetupStatusRequest};
use tokio::sync::OnceCell;

use crate::FfiError;

/// The write-side rejection when no `msek` is available (mail/CalDAV not enabled
/// on this nest). Matches the Linux write-path message so the surfaced error text
/// is uniform across clients.
const MAIL_DISABLED: &str = "Calendar requires mail to be enabled";

// ── reply mirrors (only the Events-page-rendered fields) ───────────────────────

/// FFI mirror of a calendar collection row (Linux `db::CalendarRow`): the hex
/// `calendar_id` + the unsealed display name. `visibility` is a client-side
/// display toggle (events.md § State & data shape — not stored server-side), so
/// it is not carried here; the page tracks it locally.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiCalendarRow {
    /// Hex of the 32-byte `calendar_id` (the write key for `query_events` etc.).
    pub id: String,
    pub name: String,
    /// 7-char hex `#RRGGBB` from the sealed metadata (CalDAV `calendar-color`);
    /// empty when the metadata carried none.
    pub color: String,
}

/// One attendee in an [`FfiCalEvent`] roster, RSVP already projected through the
/// asymmetric sidecar rule ([`project_attendee_rsvp`] — caldav-server.md
/// § RSVP semantics).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiCalAttendee {
    /// CAL-ADDRESS (bare email; `mailto:` stripped).
    pub email: String,
    /// Display name (`CN`), empty when the VEVENT carried none.
    pub name: String,
    /// Projected RSVP: `going | interested | tentative | declined | invited`.
    pub rsvp: String,
}

/// UniFFI face of [`fauna_core::ical::attendee_display`] — the shared
/// `AttendeeRow` text projection (display name, monogram, email-beneath
/// visibility) so the native apps stop hand-rolling the CN→email fallback.
/// The [`AttendeeDisplay`](fauna_core::ical::AttendeeDisplay) Record crosses the
/// boundary directly (fauna-ffi enables `fauna-core/uniffi`), like
/// `value_format::relative_time_display`. See events.md § Attendee list presentation.
///
/// Gated behind the default-on `value-format` feature for the SAME reason
/// `mod value_format` / `provisioning_elapsed` are: a bare `fauna_core` type
/// crosses the boundary, which `uniffi-bindgen-go` emits as an uncompilable
/// cross-namespace import in the Go mail-bridge's `--no-default-features` build
/// (which has no attendee UI, so dropping it is harmless). The feature name is
/// the generic "fauna_core display projection the Go build drops" gate, not a
/// claim this is value-formatting. Native apps + the wasm path keep it.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn attendee_display(name: String, email: String) -> fauna_core::ical::AttendeeDisplay {
    fauna_core::ical::attendee_display(&name, &email)
}

/// UniFFI face of [`fauna_core::ical::reminder_label`] — the shared event-reminder
/// offset→label map (presets → `events.reminder.{min_15,hour_1,day_1}`, non-preset
/// → raw verbatim), returned as a
/// [`LocalizedText`](fauna_core::localized::LocalizedText)
/// each app resolves through its own i18n runtime. Lifts the per-app
/// preset→label maps (linux/windows/apple/android each hard-coded) onto one source
/// of truth (priority #1/#2/#4); shared with the same native apps as
/// `attendee_display`. See events.md § Reminders.
///
/// Gated behind `value-format` for the SAME reason as `attendee_display`: a bare
/// `fauna_core` type crosses the boundary, which `uniffi-bindgen-go` emits as an
/// uncompilable cross-namespace import in the Go mail-bridge's
/// `--no-default-features` build (no reminder UI, so dropping it is harmless).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn reminder_label(offset: String) -> fauna_core::localized::LocalizedText {
    fauna_core::ical::reminder_label(&offset)
}

/// FFI mirror of [`fauna_core::ical::ReminderOption`] — one entry of the
/// reminder preset picker: the ISO-8601 offset `value` the select writes (the
/// cross-app `select(id, "PT1H")` e2e contract — never localized) plus its
/// [`reminder_label`] display label. A fauna-ffi-LOCAL mirror per the
/// [`crate::feed_rules`] precedent, kept under the same gate as
/// `reminder_label` (a `fauna_core` `LocalizedText` crosses the boundary).
#[cfg(feature = "value-format")]
#[derive(uniffi::Record)]
pub struct FfiReminderOption {
    pub value: String,
    pub label: fauna_core::localized::LocalizedText,
}

/// UniFFI face of [`fauna_core::ical::reminder_presets`] — the canonical
/// `PT15M` / `PT1H` / `P1D` reminder preset catalog in picker order, each with
/// its localized label. The ONE list every app's reminder `<DropDown>`
/// renders (events.md § Reminders); lifts the per-app hand-rolled value
/// lists (linux showed raw ISO strings; windows hard-coded English labels).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn reminder_presets() -> Vec<FfiReminderOption> {
    fauna_core::ical::reminder_presets()
        .into_iter()
        .map(|p| FfiReminderOption {
            value: p.value,
            label: p.label,
        })
        .collect()
}

/// UniFFI face of [`fauna_core::ical::rsvp_status_label`] — the shared attendee
/// RSVP attendance-status→label map (`going`/`interested`/`tentative`/`declined`/
/// `waitlisted`/`invited` → `events.rsvp.*`, unknown → capitalized verbatim),
/// returned as a [`LocalizedText`](fauna_core::localized::LocalizedText) each
/// app resolves through its own i18n runtime. Lifts the per-app status→label
/// maps (web/linux/android/apple hard-coded a capitalize, windows showed raw
/// lowercase) onto one source of truth (priority #1/#2/#4); shared with the same
/// native apps as `attendee_display`. The trailing **color** stays an idiomatic
/// per-app render. See events.md § Attendee list presentation.
///
/// Gated behind `value-format` for the SAME reason as `attendee_display`: a bare
/// `fauna_core` type crosses the boundary, which `uniffi-bindgen-go` emits as an
/// uncompilable cross-namespace import in the Go mail-bridge's
/// `--no-default-features` build (no attendee UI, so dropping it is harmless).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn rsvp_status_label(status: String) -> fauna_core::localized::LocalizedText {
    fauna_core::ical::rsvp_status_label(&status)
}

/// FFI mirror of a decoded calendar event — the row metadata + the unsealed,
/// parsed canonical VEVENT fields the Events page renders (list grids + the
/// detail panel + the reminder + attendee roster in one shape).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiCalEvent {
    /// Hex `uid_hash` (the `bridge_caldav_events` dedup key) — the id every write
    /// op (`delete_event`, the rsvp/reminder/invite read-mutate-rewrite) targets.
    /// Identical to Linux's `EventRow::id = hex uid_hash`.
    pub id: String,
    /// The plaintext iCalendar `UID` (stays inside the sealed body); a re-PUT
    /// reuses it so `uid_hash` is stable across edits.
    pub uid: String,
    /// Hex of the owning `calendar_id`.
    pub calendar_id: String,
    pub summary: String,
    /// RFC 3339 start (client windows the grids locally — the encrypted store
    /// serves no server-side date filter since bodies are opaque).
    pub dtstart: String,
    pub dtend: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    /// The VEVENT `ORGANIZER` CAL-ADDRESS (email), empty when the body carried
    /// none (a solo event created with no organizer).
    pub organizer: String,
    /// `true` iff the calling actor organizes this event (organizer empty — a
    /// solo event in the actor's own calendar — or organizer == the actor's
    /// email). Gates the author-only affordances (delete / invite); the RSVP
    /// buttons show when this is `false` (an event the actor was invited to).
    pub organized_by_me: bool,
    /// The single VEVENT `VALARM` reminder offset (e.g. `-PT15M`), `None` when the
    /// event carries no reminder (events.md § Reminders).
    pub reminder: Option<String>,
    pub attendees: Vec<FfiCalAttendee>,
}

/// Outcome of [`FfiCaldavClient::import_calendar_ics`]: how many VEVENTs imported
/// vs. skipped (unparseable). Mirrors Linux's `ics_imported:Imported N (M skipped)`.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiIcsImportResult {
    pub imported: u32,
    pub skipped: u32,
}

// ── FfiCaldavClient ────────────────────────────────────────────────────────────

/// UniFFI handle for the encrypted-CalDAV Events surface. Construct via
/// [`crate::nest_client::FfiNestClient::caldav`]; methods are exposed to Swift as
/// `async throws` and Kotlin as `suspend fun`.
#[derive(uniffi::Object)]
pub struct FfiCaldavClient {
    nest: Arc<NestClient>,
    /// Lazily-resolved `<handle>@<domain>` (the iCalendar ORGANIZER identity for
    /// writes + author-gating). Resolved once per session and cached.
    self_email: OnceCell<String>,
    /// Shared holder of the active [`ConversationsSession`] — the same `Arc<Mutex<…>>`
    /// the parent [`FfiNestClient`] populates from its `conversations_session`
    /// factory at login. Backs the **mailbox-less** organizer-scheduling rail in
    /// [`Self::invite_attendee`] (`NestImipDispatch` →
    /// `ConversationsSession::deliver_scheduling_imip`). Read live (not snapshotted)
    /// so it picks up a session set after `caldav()` was first called; `None` until
    /// the client logs in (every native app builds the session for the receive
    /// drain), in which case the invite degrades to email-only. No per-app glue —
    /// auto-wired in the factory, exactly like the `NestSchedulingSink` receive side.
    scheduling_session: SchedulingSessionHolder,
    /// Per-calendar RFC 6578 sync-tokens for [`Self::query_events_seeded`]'s
    /// backstop poll (`docs/goal/ui/events.md` § Implementation status today —
    /// the shared `delta_sync` seam's UniFFI door). One instance per
    /// `FfiCaldavClient`, i.e. per session — mirrors linux's own
    /// `Arc<Mutex<CalendarSyncTokens>>` on `FaunaClient`. `std::sync::Mutex`,
    /// never held across an await (see that method's own note) — an async
    /// lock would be the wrong tool for a guard that never blocks on I/O.
    sync_tokens: Mutex<CalendarSyncTokens>,
}

/// Shared, late-populated handle to the active conversations session (see
/// [`FfiCaldavClient::scheduling_session`]).
pub(crate) type SchedulingSessionHolder = Arc<Mutex<Option<Arc<ConversationsSession>>>>;

impl FfiCaldavClient {
    pub(crate) fn from_nest(
        nest: Arc<NestClient>,
        scheduling_session: SchedulingSessionHolder,
    ) -> Arc<Self> {
        Arc::new(Self {
            nest,
            self_email: OnceCell::new(),
            scheduling_session,
            sync_tokens: Mutex::new(CalendarSyncTokens::new()),
        })
    }

    fn client(&self) -> CalDavClient<Arc<NestClient>> {
        CalDavClient::new(Arc::clone(&self.nest))
    }

    /// `(actor_id, msek)` for an encrypted-CalDAV op, or `None` when mail/CalDAV is
    /// not enabled (no `msek` minted) or the config load failed. Thin wrapper over
    /// the shared [`dav_store_context`] (CalDAV + CardDAV read the SAME
    /// `cfg.mail.msek`).
    async fn caldav_context(&self) -> Option<([u8; 32], [u8; 32])> {
        dav_store_context(&self.nest).await
    }

    /// The actor's own email (`<handle>@<domain>`) — the VEVENT ORGANIZER for
    /// writes + the identity author-gating compares against. `handle` from
    /// `fauna.account.get`, `domain` from `fauna.setup.status` (the nest's
    /// configured domain == the handle domain in production). Cached; resolution
    /// failure yields `""` (organizer omitted, author-gating off — both safe).
    async fn self_email(&self) -> String {
        self.self_email
            .get_or_init(|| async {
                let handle = AccountClient::new(Arc::clone(&self.nest))
                    .get()
                    .await
                    .ok()
                    .and_then(|r| r.handle)
                    .unwrap_or_default();
                let domain = self
                    .nest
                    .request::<SetupStatusRequest, SetupStatusReply>(
                        "fauna.setup.status",
                        SetupStatusRequest::default(),
                    )
                    .await
                    .map(|r| r.domain)
                    .unwrap_or_default();
                if handle.is_empty() && domain.is_empty() {
                    String::new()
                } else {
                    format!("{handle}@{domain}")
                }
            })
            .await
            .clone()
    }

    /// Query + decode every event in one calendar (`CalendarNotFound` → empty).
    /// The native read primitive every per-event op (rsvp / reminder / delete /
    /// invite / get) and the grids build on. Takes a pre-derived
    /// [`DavRecipientKeys`] rather than a bare `msek` — every caller that loops
    /// over several calendars (`find_event`, `query_invited_events`) derives
    /// one up front and passes it to every call .
    async fn query_decoded(
        &self,
        actor_id: &[u8; 32],
        calendar_id: &[u8; 32],
        keys: &DavRecipientKeys,
    ) -> Result<Vec<DecodedEvent>, FfiError> {
        let page = self
            .client()
            .query_events_decoded(
                QueryEventsRequest {
                    actor_id: actor_id.to_vec(),
                    calendar_id: calendar_id.to_vec(),
                    since_modseq: None,
                    after_event_id: None,
                    limit: 0,
                },
                keys,
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(match page {
            DecodedEventsPage::Ok { events, .. } => events,
            DecodedEventsPage::CalendarNotFound => vec![],
        })
    }

    /// Find a stored event by its hex `uid_hash` across **all** the actor's
    /// calendars (the Events page addresses events by id alone, never carrying the
    /// calendar). Returns the owning `calendar_id` + the decoded event, or `None`.
    async fn find_event(
        &self,
        actor_id: &[u8; 32],
        msek: &[u8; 32],
        uid_hash_hex: &str,
    ) -> Result<Option<([u8; 32], DecodedEvent)>, FfiError> {
        let listing = self
            .client()
            .list_calendars(ListCalendarsRequest {
                actor_id: actor_id.to_vec(),
            })
            .await
            .map_err(|e| e.to_string())?;
        // Derived once for every calendar this searches, instead of once per
        // calendar .
        let keys = DavRecipientKeys::derive(msek);
        for cal in &listing.calendars {
            let Some(calendar_id) = hex32(&hex::encode(&cal.calendar_id)) else {
                continue;
            };
            let events = self.query_decoded(actor_id, &calendar_id, &keys).await?;
            if let Some(ev) = events
                .into_iter()
                .find(|d| hex::encode(&d.uid_hash) == uid_hash_hex)
            {
                return Ok(Some((calendar_id, ev)));
            }
        }
        Ok(None)
    }
}

#[fauna_uniffi_async::export]
impl FfiCaldavClient {
    /// List the actor's calendars (`fauna.bridges.list_calendars` + unseal each
    /// row's metadata). When the actor has none yet, the `Personal` calendar is
    /// lazily provisioned (`personal_calendar_id`, byte-identical to the MDA's) so
    /// the page always has a selectable calendar. Empty when mail/CalDAV is off.
    pub async fn list_calendars(&self) -> Result<Vec<FfiCalendarRow>, FfiError> {
        let Some((actor_id, msek)) = self.caldav_context().await else {
            return Ok(vec![]);
        };
        let client = self.client();
        let mut entries = client
            .list_calendars(ListCalendarsRequest {
                actor_id: actor_id.to_vec(),
            })
            .await
            .map_err(|e| e.to_string())?
            .calendars;
        // Lazy-provision Personal so a fresh actor always has a calendar (mirrors
        // Linux `fetch_calendars`). Re-list after; surface — never swallow — the
        // provision error so a failure isn't indistinguishable from "no calendars".
        if entries.is_empty() {
            let metadata = seal_calendar_metadata(
                &CalendarMetadata {
                    displayname: "Personal".to_string(),
                    color: "#3273dc".to_string(),
                    description: String::new(),
                    ..Default::default()
                },
                &msek,
            )
            .map_err(|e| e.to_string())?;
            client
                .provision_calendar(ProvisionCalendarRequest {
                    actor_id: actor_id.to_vec(),
                    calendar_id: personal_calendar_id().to_vec(),
                    encrypted_metadata: metadata,
                    ..Default::default()
                })
                .await
                .map_err(|e| e.to_string())?;
            entries = client
                .list_calendars(ListCalendarsRequest {
                    actor_id: actor_id.to_vec(),
                })
                .await
                .map_err(|e| e.to_string())?
                .calendars;
        }
        // Derived once for the whole list — every row's metadata reuses it
        // instead of paying its own X-Wing keygen .
        let keys = DavRecipientKeys::derive(&msek);
        Ok(entries
            .iter()
            .filter_map(|e| {
                let meta = unseal_calendar_metadata(&e.encrypted_metadata, &keys).ok()?;
                Some(FfiCalendarRow {
                    id: hex::encode(&e.calendar_id),
                    name: meta.displayname,
                    color: meta.color,
                })
            })
            .collect())
    }

    /// Create a calendar (`fauna.bridges.provision_calendar`) with a fresh random
    /// 32-byte id (only `Personal` is deterministic). `visibility` is a local
    /// display toggle, so only name + colour are sealed into the metadata.
    pub async fn create_calendar(&self, name: String) -> Result<(), FfiError> {
        let Some((actor_id, msek)) = self.caldav_context().await else {
            return Err(FfiError::General {
                msg: MAIL_DISABLED.to_string(),
            });
        };
        let metadata = seal_calendar_metadata(
            &CalendarMetadata {
                displayname: name,
                color: "#3273dc".to_string(),
                description: String::new(),
                ..Default::default()
            },
            &msek,
        )
        .map_err(|e| e.to_string())?;
        let calendar_id = uid_hash(&new_uid());
        self.client()
            .provision_calendar(ProvisionCalendarRequest {
                actor_id: actor_id.to_vec(),
                calendar_id: calendar_id.to_vec(),
                encrypted_metadata: metadata,
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Query + decode every event in one calendar (hex `calendar_id`). Empty when
    /// mail/CalDAV is off. The grids window the result locally by date.
    pub async fn query_events(
        &self,
        calendar_id_hex: String,
    ) -> Result<Vec<FfiCalEvent>, FfiError> {
        let Some(calendar_id) = hex32(&calendar_id_hex) else {
            return Err(FfiError::General {
                msg: "calendar id must be 64 hex chars".to_string(),
            });
        };
        let Some((actor_id, msek)) = self.caldav_context().await else {
            return Ok(vec![]);
        };
        let self_email = self.self_email().await;
        let keys = DavRecipientKeys::derive(&msek);
        let decoded = self.query_decoded(&actor_id, &calendar_id, &keys).await?;
        Ok(decoded
            .iter()
            .filter_map(|d| map_event(d, &calendar_id_hex, &self_email))
            .collect())
    }

    /// [`Self::query_events`]'s COST-saving twin for a periodic refresh
    /// (`docs/goal/ui/events.md` — the delta-sync backstop poll, NOT the
    /// quick-appearance path: that is `fauna.calendar.changed`'s job, which
    /// every app already consumes; this only cuts what the *lossy-push*
    /// backstop poll pays when nothing changed).
    ///
    /// Consults the shared `fauna_client_caldav::delta_sync::backstop_probe`
    /// before paying for a full unseal. Returns `None` when the calendar is
    /// unchanged since the last call through this method **for this
    /// `FfiCaldavClient` instance** — the caller's already-rendered list is
    /// still current and MUST NOT be touched (never applied to the model,
    /// mirrors linux `fetch_events_inner`'s early return). Returns
    /// `Some(events)` on a full (re)read — identical to [`Self::query_events`]
    /// otherwise, including the same "off" and "not found" degradations.
    ///
    /// The token is seeded from THIS read's own `highestmodseq`, never from a
    /// `sync_calendar_since` reply, so the baseline can never disagree with
    /// the event list this call just handed back. The lock is taken and
    /// dropped around each side separately (never held across the
    /// `backstop_probe`/`query_events_decoded` awaits) — the same
    /// `!Send`-avoidance linux's own comment documents.
    pub async fn query_events_seeded(
        &self,
        calendar_id_hex: String,
    ) -> Result<Option<Vec<FfiCalEvent>>, FfiError> {
        let Some(calendar_id) = hex32(&calendar_id_hex) else {
            return Err(FfiError::General {
                msg: "calendar id must be 64 hex chars".to_string(),
            });
        };
        let Some((actor_id, msek)) = self.caldav_context().await else {
            return Ok(Some(vec![]));
        };
        let client = self.client();
        let held = self
            .sync_tokens
            .lock()
            .expect("sync tokens")
            .token(&calendar_id)
            .map(str::to_string);
        match backstop_probe(&client, &actor_id, &calendar_id, held.as_deref(), 0).await {
            BackstopVerdict::Unchanged { next_token } => {
                if let Some(t) = next_token {
                    self.sync_tokens
                        .lock()
                        .expect("sync tokens")
                        .set_token(&calendar_id, t);
                }
                return Ok(None);
            }
            BackstopVerdict::ReadRequired => {}
        }
        let self_email = self.self_email().await;
        let keys = DavRecipientKeys::derive(&msek);
        let page = client
            .query_events_decoded(
                QueryEventsRequest {
                    actor_id: actor_id.to_vec(),
                    calendar_id: calendar_id.to_vec(),
                    since_modseq: None,
                    after_event_id: None,
                    limit: 0,
                },
                &keys,
            )
            .await
            .map_err(|e| e.to_string())?;
        match page {
            DecodedEventsPage::Ok {
                events,
                highestmodseq,
                ..
            } => {
                self.sync_tokens
                    .lock()
                    .expect("sync tokens")
                    .seed(&calendar_id, highestmodseq);
                Ok(Some(
                    events
                        .iter()
                        .filter_map(|d| map_event(d, &calendar_id_hex, &self_email))
                        .collect(),
                ))
            }
            DecodedEventsPage::CalendarNotFound => {
                self.sync_tokens
                    .lock()
                    .expect("sync tokens")
                    .forget(&calendar_id);
                Ok(Some(vec![]))
            }
        }
    }

    /// The events the actor was invited to but does not organize — gathered across
    /// **all** their calendars (the "invited" section's RSVP list). An event
    /// qualifies when the actor is on the roster with a not-yet-final status
    /// (`NEEDS-ACTION`) and is not the organizer. Empty when mail/CalDAV is off.
    pub async fn query_invited_events(&self) -> Result<Vec<FfiCalEvent>, FfiError> {
        let Some((actor_id, msek)) = self.caldav_context().await else {
            return Ok(vec![]);
        };
        let self_email = self.self_email().await;
        if self_email.is_empty() {
            return Ok(vec![]);
        }
        let listing = self
            .client()
            .list_calendars(ListCalendarsRequest {
                actor_id: actor_id.to_vec(),
            })
            .await
            .map_err(|e| e.to_string())?;
        // Derived once for every calendar this gathers across, instead of once
        // per calendar .
        let keys = DavRecipientKeys::derive(&msek);
        let mut out = Vec::new();
        for cal in &listing.calendars {
            let cal_hex = hex::encode(&cal.calendar_id);
            let Some(calendar_id) = hex32(&cal_hex) else {
                continue;
            };
            let decoded = self.query_decoded(&actor_id, &calendar_id, &keys).await?;
            for d in &decoded {
                let Some(ev) = map_event(d, &cal_hex, &self_email) else {
                    continue;
                };
                let invited = !ev.organized_by_me
                    && ev
                        .attendees
                        .iter()
                        .any(|a| a.email.eq_ignore_ascii_case(&self_email) && a.rsvp == "invited");
                if invited {
                    out.push(ev);
                }
            }
        }
        Ok(out)
    }

    /// Create an event: build the canonical VEVENT from the form fields (the
    /// reused `generate_ical` writer), seal it, and PUT it
    /// (`fauna.bridges.put_event_ciphertext`). A fresh `uid` is minted; its
    /// `blake3` is the dedup key. Empty optionals (`dtend` / `location` /
    /// `description`) are omitted from the VEVENT.
    pub async fn create_event(
        &self,
        calendar_id_hex: String,
        summary: String,
        dtstart: String,
        dtend: String,
        location: String,
        description: String,
    ) -> Result<(), FfiError> {
        let Some(calendar_id) = hex32(&calendar_id_hex) else {
            return Err(FfiError::General {
                msg: "calendar id must be 64 hex chars".to_string(),
            });
        };
        let Some((actor_id, msek)) = self.caldav_context().await else {
            return Err(FfiError::General {
                msg: MAIL_DISABLED.to_string(),
            });
        };
        let self_email = self.self_email().await;
        let uid = new_uid();
        let fields = EventFields {
            summary,
            dtstart,
            dtend,
            location,
            description,
            uid: uid.clone(),
            status: "confirmed".to_string(),
            ..Default::default()
        };
        self.client()
            .seal_and_put_event(
                &actor_id,
                &calendar_id,
                &uid_hash(&uid),
                &msek,
                &fields,
                &[],
                &self_email,
                None,
                now_secs(),
                None,
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Fetch one event by hex `uid_hash` (searched across the actor's calendars).
    /// `None` when no such event / mail is off. Carries the full detail (roster,
    /// organizer, reminder) so the detail panel renders without further calls.
    pub async fn get_event(&self, uid_hash_hex: String) -> Result<Option<FfiCalEvent>, FfiError> {
        let Some((actor_id, msek)) = self.caldav_context().await else {
            return Ok(None);
        };
        let self_email = self.self_email().await;
        let Some((calendar_id, decoded)) = self.find_event(&actor_id, &msek, &uid_hash_hex).await?
        else {
            return Ok(None);
        };
        Ok(map_event(&decoded, &hex::encode(calendar_id), &self_email))
    }

    /// Delete an event by hex `uid_hash` (`fauna.bridges.delete_event`), resolving
    /// its calendar across the actor's collections.
    pub async fn delete_event(&self, uid_hash_hex: String) -> Result<(), FfiError> {
        let Some((actor_id, msek)) = self.caldav_context().await else {
            return Err(FfiError::General {
                msg: MAIL_DISABLED.to_string(),
            });
        };
        let Some(uh) = hex32(&uid_hash_hex) else {
            return Err(FfiError::General {
                msg: "event id must be 64 hex chars".to_string(),
            });
        };
        let Some((calendar_id, _)) = self.find_event(&actor_id, &msek, &uid_hash_hex).await? else {
            return Ok(()); // already gone
        };
        self.client()
            .delete_event(DeleteEventRequest {
                actor_id: actor_id.to_vec(),
                calendar_id: calendar_id.to_vec(),
                uid_hash: uh.to_vec(),
                if_match: None,
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// RSVP to an event (read-mutate-rewrite): find it by `uid_hash`, apply the
    /// response (`PARTSTAT` + the asymmetric `interested` sidecar), and re-PUT it.
    /// `response` is a [`fauna_core::rsvp::RsvpResponse`] — `Going | Interested |
    /// Declined` — so an inbound-only value like `tentative` (a stock CalDAV
    /// client's own state, never a Fauna app offer — caldav-server.md § RSVP
    /// semantics) is unrepresentable at this call site rather than merely
    /// refused at runtime by the shared `apply_rsvp`.
    ///
    /// When the event has a *different* organizer, a best-effort iMIP `REPLY` is
    /// sent so the response propagates (caldav-server.md § Responding).
    pub async fn rsvp_event(
        &self,
        uid_hash_hex: String,
        response: fauna_core::rsvp::RsvpResponse,
    ) -> Result<(), FfiError> {
        let Some((actor_id, msek)) = self.caldav_context().await else {
            return Err(FfiError::General {
                msg: MAIL_DISABLED.to_string(),
            });
        };
        let self_email = self.self_email().await;
        let Some((calendar_id, decoded)) = self.find_event(&actor_id, &msek, &uid_hash_hex).await?
        else {
            return Err(FfiError::General {
                msg: "event not found".to_string(),
            });
        };
        let rw = apply_rsvp(
            &decoded.ics,
            decoded.fauna_ext.as_ref(),
            &self_email,
            response.as_str(),
        )
        .map_err(|e| FfiError::General { msg: e })?;
        self.client()
            .seal_and_put_event(
                &actor_id,
                &calendar_id,
                &uid_hash(&rw.fields.uid),
                &msek,
                &rw.fields,
                &rw.attendees,
                &rw.organizer_email,
                rw.fauna_ext.as_ref(),
                now_secs(),
                None,
            )
            .await
            .map_err(|e| e.to_string())?;
        // Notify a *different* organizer with an iMIP REPLY (best-effort — the
        // local RSVP already persisted). The organizer-diff + reply construction
        // is the shared `imip_reply_for_rsvp` (caldav-server.md § auto-schedule),
        // so every app's Events shell sends the identical REPLY.
        if let Some(reply) = imip_reply_for_rsvp(&rw, &self_email, now_secs()) {
            let _ = EmailClient::new(Arc::clone(&self.nest))
                .send(reply.recipients, reply.raw_rfc5322)
                .await;
        }
        Ok(())
    }

    /// Set (`offset` non-empty, e.g. `-PT15M`) or clear (`offset` empty) the single
    /// VEVENT reminder on an event (read-mutate-rewrite, re-PUT). Resolves the
    /// event by `uid_hash` across the actor's calendars.
    pub async fn set_reminder(&self, uid_hash_hex: String, offset: String) -> Result<(), FfiError> {
        let Some((actor_id, msek)) = self.caldav_context().await else {
            return Err(FfiError::General {
                msg: MAIL_DISABLED.to_string(),
            });
        };
        let Some((calendar_id, decoded)) = self.find_event(&actor_id, &msek, &uid_hash_hex).await?
        else {
            return Err(FfiError::General {
                msg: "event not found".to_string(),
            });
        };
        let rw = set_reminder(&decoded.ics, decoded.fauna_ext.as_ref(), &offset)
            .map_err(|e| FfiError::General { msg: e })?;
        self.client()
            .seal_and_put_event(
                &actor_id,
                &calendar_id,
                &uid_hash(&rw.fields.uid),
                &msek,
                &rw.fields,
                &rw.attendees,
                &rw.organizer_email,
                rw.fauna_ext.as_ref(),
                now_secs(),
                None,
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Invite an attendee: add a `mailto:` ATTENDEE to the VEVENT roster, re-PUT
    /// first (so the roster persists even if the send fails), then **fork the iMIP
    /// `REQUEST` per attendee transport** via the shared
    /// `fauna_client_caldav::dispatch_imip_request` (Slice 5): every email-reachable
    /// attendee gets one fanned email (`fauna.email.send`), every **mailbox-less**
    /// Fauna attendee (CalDAV on / email off) gets the iMIP over the WS-RPC sealed
    /// MLS welcome rail (`NestImipDispatch` →
    /// `ConversationsSession::deliver_scheduling_imip`,
    /// caldav-server.md § Server-side auto-schedule). The routing is the byte-identical
    /// shared helper linux's `invite_to_event` uses (priority #2). An empty `email`
    /// re-sends to the existing roster. The mailbox-less rail needs the logged-in
    /// conversations session (auto-wired by the parent `FfiNestClient`); absent it
    /// the dispatch degrades to email-only. Resolves the event by `uid_hash` across
    /// the actor's calendars.
    pub async fn invite_attendee(
        &self,
        uid_hash_hex: String,
        email: String,
    ) -> Result<(), FfiError> {
        let Some((actor_id, msek)) = self.caldav_context().await else {
            return Err(FfiError::General {
                msg: MAIL_DISABLED.to_string(),
            });
        };
        let self_email = self.self_email().await;
        let Some((calendar_id, decoded)) = self.find_event(&actor_id, &msek, &uid_hash_hex).await?
        else {
            return Err(FfiError::General {
                msg: "event not found".to_string(),
            });
        };
        let email = email.trim().to_string();
        let (fields, roster, organizer) = if email.is_empty() {
            imip_inputs(&decoded.ics, &self_email).map_err(|e| FfiError::General { msg: e })?
        } else {
            let rw = add_attendee(
                &decoded.ics,
                decoded.fauna_ext.as_ref(),
                &self_email,
                &email,
            )
            .map_err(|e| FfiError::General { msg: e })?;
            self.client()
                .seal_and_put_event(
                    &actor_id,
                    &calendar_id,
                    &uid_hash(&rw.fields.uid),
                    &msek,
                    &rw.fields,
                    &rw.attendees,
                    &rw.organizer_email,
                    rw.fauna_ext.as_ref(),
                    now_secs(),
                    None,
                )
                .await
                .map_err(|e| e.to_string())?;
            (rw.fields, rw.attendees, rw.organizer_email)
        };
        // The mailbox-less rail needs the logged-in conversations session (its
        // loaded MLS engine), auto-wired by the parent `FfiNestClient`. Present →
        // the shared dispatch fork; absent (not logged in for conversations — should
        // not happen once a calendar exists) → email-only fallback.
        let session = self.scheduling_session.lock().unwrap().clone();
        match session {
            Some(session) => {
                let dispatch = NestImipDispatch::new(Arc::clone(&self.nest), session);
                let report = dispatch_imip_request(
                    &AnonAttendeeDiscovery,
                    &dispatch,
                    &fields,
                    &roster,
                    &organizer,
                    now_secs(),
                )
                .await
                .map_err(|e| e.to_string())?;
                // Best-effort: the roster is persisted, but surface a rail failure
                // (the old email-only path propagated a send error too).
                if let Some(err) = report.errors.first() {
                    return Err(FfiError::General { msg: err.clone() });
                }
            }
            None => {
                if let Some(message) =
                    imip_request_for_invite(&fields, &roster, &organizer, now_secs())
                {
                    EmailClient::new(Arc::clone(&self.nest))
                        .send(message.recipients, message.raw_rfc5322)
                        .await
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(())
    }

    /// Export a calendar as a single RFC 5545 `.ics` string via the shared
    /// `CalDavClient::export_calendar_ics`. The caller writes/shares the file.
    /// Empty calendar → an empty VCALENDAR; mail off → an error.
    pub async fn export_calendar_ics(&self, calendar_id_hex: String) -> Result<String, FfiError> {
        let Some(calendar_id) = hex32(&calendar_id_hex) else {
            return Err(FfiError::General {
                msg: "calendar id must be 64 hex chars".to_string(),
            });
        };
        let Some((actor_id, msek)) = self.caldav_context().await else {
            return Err(FfiError::General {
                msg: MAIL_DISABLED.to_string(),
            });
        };
        Ok(self
            .client()
            .export_calendar_ics(&actor_id, &calendar_id, &DavRecipientKeys::derive(&msek))
            .await
            .map_err(|e| e.to_string())?)
    }

    /// Import a `.ics` string into a calendar via the shared
    /// `CalDavClient::import_ical_events` (parse + seal + PUT each VEVENT). v1
    /// imports event fields only (rosters on imported events are a follow-up).
    /// Returns imported / skipped counts.
    pub async fn import_calendar_ics(
        &self,
        calendar_id_hex: String,
        ics_text: String,
    ) -> Result<FfiIcsImportResult, FfiError> {
        let Some(calendar_id) = hex32(&calendar_id_hex) else {
            return Err(FfiError::General {
                msg: "calendar id must be 64 hex chars".to_string(),
            });
        };
        let Some((actor_id, msek)) = self.caldav_context().await else {
            return Err(FfiError::General {
                msg: MAIL_DISABLED.to_string(),
            });
        };
        let self_email = self.self_email().await;
        let outcome = self
            .client()
            .import_ical_events(
                &actor_id,
                &calendar_id,
                &msek,
                &self_email,
                &ics_text,
                now_secs(),
                |_| new_uid(),
            )
            .await;
        Ok(FfiIcsImportResult {
            imported: outcome.imported,
            skipped: outcome.skipped,
        })
    }
}

// ── helpers ────────────────────────────────────────────────────────────────────

/// `(actor_id, msek)` for an encrypted-DAV op — **shared by [`FfiCaldavClient`]
/// and [`FfiCarddavClient`]** because both DAV surfaces read/write the SAME
/// encrypted mail store keyed by the SAME `cfg.mail.msek` (priority #2 — one msek
/// gate, not two copies). Thin wrapper over the shared
/// [`fauna_client_config::dav_store_context`] (also tui's and linux's
/// `caldav_context` now) — this seam supplies only what the FFI shell knows: the
/// actor keypair rebuilt from the connection's own authenticated secret.
///
/// `None` when mail is not enabled (no `msek` minted) or the config load failed —
/// a successful load with `mail.msek` absent is the legitimate mail-disabled state
/// (empty calendar/address-book list), and a load error degrades the same way
/// rather than crashing the page (the secret is fine — the WS connection
/// authenticated with it).
///
/// The MSEK is the account's mail custody (`fauna.state.mail`), read through
/// this seat's account runtime; a build that hosts no runtime (the
/// `--no-default-features` Go mail-bridge and watchOS builds) holds no custody,
/// so it reads as the mail-disabled state.
#[cfg(feature = "account-runtime")]
pub(crate) async fn dav_store_context(nest: &Arc<NestClient>) -> Option<([u8; 32], [u8; 32])> {
    let actor_id = nest
        .auth()
        .keypair()
        .expect("identity keypair required for encrypted DAV")
        .actor_id()
        .0;
    shared_dav_store_context(crate::account_runtime::mail_store().as_ref(), actor_id).await
}

/// The runtime-less build's twin — see the doc above.
#[cfg(not(feature = "account-runtime"))]
pub(crate) async fn dav_store_context(_nest: &Arc<NestClient>) -> Option<([u8; 32], [u8; 32])> {
    None
}

/// Map a decoded event (unsealed + parsed VEVENT) to the UI [`FfiCalEvent`],
/// projecting each attendee's RSVP through the asymmetric sidecar rule and
/// computing `organized_by_me`. `None` when the body fails to parse.
fn map_event(
    decoded: &DecodedEvent,
    calendar_id_hex: &str,
    self_email: &str,
) -> Option<FfiCalEvent> {
    let fields = parse_ical(&decoded.ics).ok()?;
    let organizer = parse_ical_organizer(&decoded.ics).unwrap_or_default();
    let attendees = parse_ical_attendees(&decoded.ics)
        .into_iter()
        .map(|a| FfiCalAttendee {
            rsvp: project_attendee_rsvp(&a.partstat, &a.email, decoded.fauna_ext.as_ref())
                .to_string(),
            email: a.email,
            name: a.name,
        })
        .collect();
    // A solo event (no ORGANIZER) in the actor's own calendar is theirs; an event
    // whose ORGANIZER is the actor's email is theirs; anything else they were
    // invited to (RSVP applies, author-only affordances hidden). Single-sourced
    // with the web wasm seam in `fauna_client_caldav::organized_by_me`.
    let organized_by_me = fauna_client_caldav::organized_by_me(&organizer, self_email);
    Some(FfiCalEvent {
        id: hex::encode(&decoded.uid_hash),
        uid: fields.uid.clone(),
        calendar_id: calendar_id_hex.to_string(),
        summary: fields.summary.clone(),
        dtstart: fields.dtstart.clone(),
        dtend: opt(&fields.dtend),
        location: opt(&fields.location),
        description: opt(&fields.description),
        organizer,
        organized_by_me,
        reminder: opt(&fields.alarm),
        attendees,
    })
}

/// `Some(s)` when non-empty, else `None` — the `String` → `Option<String>` the
/// UI fields use for absent dtend / location / description / reminder.
fn opt(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Decode a 64-char hex string into a 32-byte array (`calendar_id` / `uid_hash`);
/// `None` on malformed input. Thin `Option`-returning adapter over the shared
/// [`fauna_core::hex32::decode`].
fn hex32(s: &str) -> Option<[u8; 32]> {
    fauna_core::hex32::decode(s).ok()
}

/// A fresh plaintext iCalendar `UID` (`<random-hex>@fauna-android`); its `blake3`
/// becomes the row's `uid_hash` dedup key.
fn new_uid() -> String {
    let mut rnd = [0u8; 16];
    getrandom::fill(&mut rnd).expect("getrandom failed");
    format!("{}@fauna-android", hex::encode(rnd))
}

/// Current Unix epoch seconds — the `internal_date` (CREATED / LAST-MODIFIED
/// surrogate) for a freshly written event.
fn now_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex32_round_trips_and_rejects_bad_len() {
        let h = hex::encode([0xABu8; 32]);
        assert_eq!(hex32(&h), Some([0xABu8; 32]));
        assert_eq!(hex32("deadbeef"), None);
        assert_eq!(hex32("zz"), None);
    }

    #[test]
    fn opt_maps_empty_to_none() {
        assert_eq!(opt(""), None);
        assert_eq!(opt("x"), Some("x".to_string()));
    }

    #[test]
    fn new_uid_is_unique_and_android_scoped() {
        let a = new_uid();
        let b = new_uid();
        assert_ne!(a, b);
        assert!(a.ends_with("@fauna-android"));
    }

    #[test]
    fn map_event_projects_organizer_gating_and_roster() {
        use fauna_client_caldav::{AttendeeInfo, generate_ical, parse_icalendar, uid_hash};
        let fields = EventFields {
            summary: "Standup".into(),
            dtstart: "2026-06-02T09:00:00Z".into(),
            dtend: "2026-06-02T09:15:00Z".into(),
            location: "Room 1".into(),
            description: "daily".into(),
            uid: "evt-1@fauna.test".into(),
            status: "confirmed".into(),
            ..Default::default()
        };
        let attendees = vec![AttendeeInfo {
            name: "Bob".into(),
            email: "bob@fauna.test".into(),
            partstat: "ACCEPTED".into(),
            fauna_status: "going".into(),
        }];
        let ics = generate_ical(&fields, &attendees, "alice@fauna.test");
        let decoded = DecodedEvent {
            event_id: vec![1u8; 32],
            uid_hash: uid_hash("evt-1@fauna.test").to_vec(),
            etag: "1".into(),
            modseq: 1,
            internal_date: 0,
            ics: ics.clone(),
            document: parse_icalendar(ics.as_bytes()).expect("parse"),
            fauna_ext: None,
        };
        // Organizer is alice → alice sees it as hers; bob does not.
        let mine = map_event(&decoded, "cafe", "alice@fauna.test").expect("map");
        assert_eq!(mine.id, hex::encode(uid_hash("evt-1@fauna.test")));
        assert_eq!(mine.calendar_id, "cafe");
        assert!(mine.organized_by_me);
        assert_eq!(mine.dtend.as_deref(), Some("2026-06-02T09:15:00Z"));
        assert_eq!(mine.attendees.len(), 1);
        assert_eq!(mine.attendees[0].rsvp, "going");

        let theirs = map_event(&decoded, "cafe", "bob@fauna.test").expect("map");
        assert!(!theirs.organized_by_me);
    }

    #[cfg(feature = "value-format")]
    #[test]
    fn reminder_label_delegates_preset_and_verbatim() {
        // A canonical preset → its events.reminder.* key; a non-preset offset
        // → the raw value as its own key (rendered verbatim). Mirrors the shared
        // fauna_core::ical::reminder_label contract (events.md § Reminders).
        assert_eq!(reminder_label("PT1H".into()).key, "events.reminder.hour_1");
        assert_eq!(reminder_label("PT30M".into()).key, "PT30M");
    }
}
