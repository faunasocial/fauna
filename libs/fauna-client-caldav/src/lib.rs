//! Shared-Rust calendar/scheduling crate for the **events.md Decision-B** path:
//! a Fauna app reads/writes its OWN calendars + events over the *encrypted*
//! `fauna.bridges.*` CalDAV RPCs against the `bridge_caldav_*` store — the SAME
//! store + sealing scheme the mail-bridge MDA serves to Apple Calendar, so a
//! Fauna app and a CalDAV MUA see the same data
//! (`docs/goal/behavior/caldav-server.md` § Event resources, `docs/goal/ui/events.md`
//! § Persistence).
//!
//! **Not** `fauna-client-calendars` (plural) — that is the *legacy*
//! `fauna.calendars.*` plaintext-`content`-table façade Decision B retires.
//!
//! What lives here (events.md § Where logic lives — all shared Rust):
//! - the typed `fauna.bridges.*` event call surface ([`CalDavClient`]), generic
//!   over the WS-RPC transport (`R: RpcRequester`) so native + UniFFI + wasm
//!   share one kind-composition (mirrors [`fauna-client-calendars`] /
//!   `fauna-client-conversations`);
//! - **sealing** ([`seal_event_body`] / [`unseal_event_body`]) — the client seals
//!   the canonical VEVENT to its own `credential_id="default"` recipient pubkey,
//!   the EXACT HPKE scheme + single-`MailRecordEnvelope` wire shape the MDA's
//!   CalDAV PUT uses, so MDA-written and client-written events are mutually
//!   readable;
//! - the **writer** ([`generate_ical`], re-exported from `fauna_core::ical`,
//!   reused — `caldav-writer-reuses-fauna-core`) + the `interested↔TENTATIVE`
//!   projection ([`partstat_from_fauna`] / [`fauna_status_from_partstat`]);
//! - the deterministic [`personal_calendar_id`].
//!
//! WASM-safe: web links this crate (writer + sealing are WASM-safe). The
//! iCalendar *parser* (`fauna_mail::parse_icalendar`) is native-only, so the
//! **read path** ([`CalDavClient::query_events_decoded`] / [`decode_event_entry`]
//! — unseal → `parse_icalendar` → typed [`DecodedEvent`]) plus the recurrence
//! re-exports ([`expand_recurrence`]) live behind a
//! `cfg(not(target_arch = "wasm32"))` gate; the web parse story is a separate
//! slice (`caldav-server.md` § iCalendar parsing rules). The write/seal/transport
//! surface here stays portable.

use std::collections::BTreeMap;

use fauna_protocol::RpcRequester;
use fauna_protocol::bridge_routing::{
    DeleteEventReply, DeleteEventRequest, EventEntry, ListCalendarsReply, ListCalendarsRequest,
    ProvisionCalendarReply, ProvisionCalendarRequest, PutEventCiphertextReply,
    PutEventCiphertextRequest, QueryEventsReply, QueryEventsRequest, SyncCalendarSinceReply,
    SyncCalendarSinceRequest,
};
use serde::{Deserialize, Serialize};

// The WASM-safe iCalendar surface from `fauna_core::ical` (hand-rolled writer +
// flat-fields reader — events.md § Where logic lives). Re-exported so a client
// reaches the whole calendar read/write surface through this one crate (priority
// #2) rather than depending on `fauna-core` directly: the writer
// (`generate_ical`/`generate_ical_multi`), the flat-fields reader
// (`parse_ical`/`parse_ical_multi` — the inverse used to render a `DecodedEvent`
// without walking the parse tree, and to import `.ics`), the roster reader
// (`parse_ical_attendees`), and the symmetric PARTSTAT helpers.
pub use fauna_core::ical::{
    AttendeeInfo, EventFields, ITipMethod, ImipMessage, apply_reply_to_roster, build_event_imip,
    epoch_secs_to_ical_utc, fauna_status_from_partstat, generate_ical, generate_ical_multi,
    generate_itip, parse_ical, parse_ical_attendees, parse_ical_multi, parse_ical_organizer,
    partstat_from_fauna, render_stored_event,
};
pub use fauna_protocol::bridge_routing;

// Client-side read-mutate-rewrite helpers (RSVP / add-attendee / reminder /
// iMIP-inputs) shared by all seven apps' Events shells — WASM-safe, pure,
// lifted from the linux lead (priority #2). See `mutate.rs`.
mod mutate;
pub use mutate::{
    EventRewrite, add_attendee, apply_rsvp, imip_inputs, imip_reply_for_rsvp,
    imip_request_for_invite, set_reminder,
};

// The `"events"` rail of `fauna_protocol::drafts::DRAFT_RAILS` — the at-rest
// shape of an in-progress `event-form` compose, shared by all seven apps' Events
// shells (priority #2). WASM-safe: pure serde over owned `String`s, no parser.
// See `drafts.rs` and `docs/goal/ui/events.md` § Persistence.
pub mod drafts;
pub mod refused_changes;

// The per-session memo over the inbound rule's succession lookup, and the one
// resolver both shipped scheduling sinks hand the apply — each supplying only
// its platform's dial. See `organizer_succession.rs`.
mod organizer_succession;
pub use organizer_succession::{
    MemoizedSuccessionResolver, SuccessionDialer, SuccessionLookup, SuccessionMemo,
};

pub use refused_changes::{RefusedChangeView, refused_change_view};

// Per-calendar RFC 6578 sync-token state — the shared seam that makes
// `fauna.bridges.sync_calendar_since` usable (events.md § Where logic lives puts
// the delta poll in shared Rust). The RPC shipped in Phase D.6 with no consumer
// in any of the seven apps; what was missing was this state + its paging and
// fall-back-to-full-read rules. See `delta_sync.rs`.
pub mod delta_sync;

// The native-only iCalendar parse + recurrence-expansion surface (the
// crate-backed `icalendar`/`rrule` parser the MDA also uses — caldav-server.md
// § iCalendar parsing rules). Re-exported so a consumer of the read path has the
// full parsed tree + RRULE expansion in one place without a direct `fauna-mail`
// dependency. Not available on wasm: these ride the `icalendar` feature, i.e. the
// icalendar/rrule crates caldav-server.md § iCalendar parsing rules calls
// native/MDA-only.
#[cfg(not(target_arch = "wasm32"))]
pub use fauna_mail::{
    ExpandedOccurrence, ICalComponent, ICalDocument, ICalError, ICalParameter, ICalProperty,
    expand_recurrence, parse_icalendar,
};
// The iMIP MIME extraction is NOT part of that gate. It rides fauna-mail's
// `parser` feature — `dep:mail-parser`, a pure-Rust MIME walk — which the
// Cargo manifest now pulls on wasm32 too, because the WASM-safe inbound
// scheduling impl below needs to reach the `text/calendar` part of an iMIP
// message on every app, the web SPA included.
pub use fauna_mail::{TextCalendarPart, extract_text_calendar_part};

/// The deterministic `Personal` calendar id: `blake3("personal")` (256-bit, so
/// the goal-doc's `[:32]` is the whole hash), so the lazy-`Personal` calendar a
/// CalDAV MUA triggers on first PROPFIND and the one a Fauna app provisions
/// land on the SAME `bridge_caldav_calendars` row (caldav-server.md § Lazy
/// "Personal" calendar).
///
/// Re-exported from [`fauna_protocol::dav_identity`], the single Rust owner of
/// the DAV identity contract. Agreement with the MDA's Go `personalCalendarID()`
/// is held by `libs/fauna-protocol/tests/dav_identity_cross_language.rs`, not by
/// a human keeping two copies equal.
pub use fauna_protocol::dav_identity::personal_calendar_id;

/// Resolve an Events-page calendar selection against the calendars that
/// actually exist right now.
///
/// `docs/goal/ui/events.md` § Layout & flow + § User actions: clicking a
/// `calendar-item` **selects** that calendar and narrows the page to it, while
/// **no** selection shows the union of every owned calendar (creating a
/// calendar targets it for authoring but does not force a selection). A
/// selection whose calendar has since vanished — deleted here, or by a CalDAV
/// MUA against the same `bridge_caldav_*` store — falls back to the union
/// rather than scoping to nothing, so the page can never strand the user on a
/// permanently blank list with no way back.
///
/// Returns `Some(id)` only for a *live* selection; `None` means "show the
/// union". The `calendar-visibility` display filter composes onto the `None`
/// arm — [`calendar_is_displayed`] owns that composition.
#[must_use]
pub fn resolve_calendar_selection<I, S>(selected: Option<&str>, existing_ids: I) -> Option<&str>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let id = selected?;
    existing_ids
        .into_iter()
        .any(|existing| existing.as_ref() == id)
        .then_some(id)
}

/// Does an event on `calendar_id` belong on the Events page right now?
///
/// The whole page-scope composition in one call — `docs/goal/ui/events.md`
/// § Where logic lives → *"Which calendars display"*:
///
/// 1. **A live selection wins.** A `calendar-item` selection (resolved through
///    [`resolve_calendar_selection`]'s staleness rule internally — callers pass
///    the *raw* stored selection) narrows the page to exactly that calendar.
///    The `calendar-visibility` toggles do NOT apply on this arm: selecting a
///    calendar shows it even while its visibility checkbox is unchecked.
/// 2. **With no (live) selection, the visibility toggles filter the union.**
///    An **empty** visible-set means "no filter" — the full union, the
///    pre-first-load state — not "hide everything". This empty-set case is
///    where the per-app copies had drifted before the lift.
///
/// Visibility is client-side display state, never persisted server-side, and
/// never a privacy/sharing control (events.md § Where logic lives).
///
/// Deliberately one composed function rather than exposed halves: taking the
/// raw selection makes skipping the staleness resolve unrepresentable, and
/// owning both arms makes "visibility accidentally applied to a selection"
/// unrepresentable — each was a real drift in the pre-lift per-app copies
/// (linux's `CalendarViewState::shows_calendar` is the proven pattern this
/// generalizes).
///
/// Note `calendar_id` need not appear in `existing_ids`: cached events can
/// reference a just-deleted calendar, and on the union arm they follow the
/// visible-set verbatim (shown when it is empty or still names that id) —
/// mirroring the proven linux behavior rather than inventing a stricter rule.
#[must_use]
pub fn calendar_is_displayed<I, S, J, T>(
    selected: Option<&str>,
    existing_ids: I,
    visible_calendars: J,
    calendar_id: &str,
) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
    J: IntoIterator<Item = T>,
    T: AsRef<str>,
{
    if let Some(live) = resolve_calendar_selection(selected, existing_ids) {
        return calendar_id == live;
    }
    let mut any_visible_named = false;
    for visible in visible_calendars {
        any_visible_named = true;
        if visible.as_ref() == calendar_id {
            return true;
        }
    }
    !any_visible_named
}

/// The lazy-`Personal` calendar's default display name and colour. A Fauna app
/// and the MDA seal the SAME default metadata for the same
/// [`personal_calendar_id`], so an invite a client materializes
/// ([`CalDavClient::apply_inbound_request`]) and the lazy collection a CalDAV
/// MUA's first PROPFIND triggers land on one row (caldav-server.md § Lazy
/// "Personal" calendar).
///
/// Re-exported from [`fauna_protocol::dav_identity`]; agreement with the MDA's
/// Go `defaultDisplayname` / `defaultColor` is pinned by
/// `libs/fauna-protocol/tests/dav_identity_cross_language.rs`.
pub use fauna_protocol::dav_identity::{DEFAULT_CALENDAR_COLOR, DEFAULT_CALENDAR_DISPLAYNAME};

/// The default metadata shape for the lazy `Personal` calendar — the Rust mirror
/// of the MDA's `lazyProvisionPersonal` metadata (`{Displayname, Color}`,
/// description omitted). Used by [`CalDavClient::apply_inbound_request`] to
/// idempotently ensure the recipient's default calendar exists before
/// materializing an unsolicited invite into it.
#[must_use]
pub fn default_personal_metadata() -> CalendarMetadata {
    CalendarMetadata {
        displayname: DEFAULT_CALENDAR_DISPLAYNAME.to_string(),
        color: DEFAULT_CALENDAR_COLOR.to_string(),
        description: String::new(),
        ..Default::default()
    }
}

/// The CalDAV `uid_hash` for an event's plaintext iCalendar `UID`:
/// `blake3(uid)` (256-bit), so a row a Fauna app writes and the row a CalDAV
/// MUA PUTs for the same `UID` collide on the SAME `bridge_caldav_events` dedup
/// key (caldav-server.md § Event resources — "the path component is the blake3
/// hash of the event's plaintext `UID`"). The plaintext `UID` stays inside the
/// sealed body; only this hash is exposed.
///
/// Re-exported from [`fauna_protocol::dav_identity`], which owns the one rule
/// both DAV rails use (the CardDAV crate re-exports the same function). Its
/// agreement with the MDA's Go `blake3.Sum256([]byte(uid))` is pinned by
/// `libs/fauna-protocol/tests/dav_identity_cross_language.rs`.
pub use fauna_protocol::dav_identity::uid_hash;

// ── Sealing (events.md § Where logic lives — "sealing is shared Rust, not RPC glue") ──

/// Seal/unseal failure for the event-body crypto layer — the shared
/// [`fauna_mls::wrapped_blob::dav_body::SealError`], which CardDAV's card bodies
/// use too. Was a byte-identical twin of CardDAV's until 2026-08-23.
pub use fauna_mls::wrapped_blob::dav_body::SealError;

/// Seal `plaintext` (the canonical VEVENT `.ics` bytes, or the index-hint bytes)
/// to the actor's own `credential_id="default"` recipient pubkey derived from
/// `msek` (the `fauna.state.mail` MSEK), producing the `encrypted_body` wire bytes.
///
/// This is the EXACT scheme the MDA's CalDAV PUT uses
/// (`internal/mda/caldav/put.go` → `mailfauna.EncryptToRecipient`): a single
/// [`MailRecordEnvelope`](fauna_mls::wrapped_blob::MailRecordEnvelope) (HPKE `DHKEM(X25519,HKDF-SHA256)` + `HKDF-SHA256` +
/// `ChaCha20-Poly1305`, AAD `for_mail_record()`) serialized canonical. Same
/// `msek` ⇒ same recipient keypair, so the bytes are interchangeable.
pub use fauna_mls::wrapped_blob::dav_body::seal_dav_body as seal_event_body;

/// Post-quantum (X-Wing, ML-KEM-768 ∥ X25519) sibling of [`seal_event_body`]
/// (S3d leg D2c). Derives the actor's **own** X-Wing keypair from `msek` (the
/// ML-KEM half via `fauna.mail.recipient-mlkem.v1`, the X25519 half reused from
/// the classical recipient key) and seals the body to it. The produced
/// [`MailRecordEnvelope`](fauna_mls::wrapped_blob::MailRecordEnvelope) self-describes its X-Wing suite, so the MDA and every
/// Fauna app open it through the same suite-dispatching [`unseal_event_body`].
///
/// [`CalDavClient::seal_and_put_event`] always seals new event bodies with it —
/// no capability token gates the suite (`post-quantum.md` § Capability
/// negotiation, the 2026-09-24 ruling); the classical [`seal_event_body`] stays
/// for the index-hint and for the MDA-sealed bodies (the Go mail bridge's
/// `EncryptToRecipient`). Because the
/// key is derived locally from the actor's own MSEK it is always FIPS-203 valid,
/// so there is no untrusted-published-ek seal-error to degrade from (PQ-4(b)'s
/// self-DoS vector is vacuous here — an error would be a genuine bug and is
/// surfaced loudly rather than silently degraded). The index-hint stays classical.
pub use fauna_mls::wrapped_blob::dav_body::seal_dav_body_xwing as seal_event_body_xwing;

/// Unseal an `encrypted_body` produced by [`seal_event_body`] **or by the MDA** —
/// both derive the same recipient keypair from `msek`, so a Fauna app reads
/// MDA-written events and vice versa. Returns the plaintext VEVENT bytes.
///
/// A single-shot convenience — opening more than one body under the same
/// `msek` (a calendar list, an event page)? Derive one [`DavRecipientKeys`]
/// and call its `unseal` per item instead, or every item pays its own X-Wing
/// keygen again .
pub use fauna_mls::wrapped_blob::dav_body::unseal_dav_body as unseal_event_body;

/// The actor's own recipient keypair for this crate's sealed bodies (calendar
/// metadata, event bodies, the Fauna-extension sidecar), derived once from
/// `msek` and reused across a whole batch — [`unseal_calendar_metadata`],
/// [`unseal_fauna_ext`], [`decode_event_entry`] and [`decode_event_entry_flat`]
/// all take one of these instead of a bare `msek`, so `list_calendars` /
/// `query_events` render N rows with a single keygen instead of N
/// .
pub use fauna_mls::wrapped_blob::dav_body::DavRecipientKeys;

// ── Fauna-extension sidecar (caldav-server.md § Event resources) ──────────────

/// The Fauna-only extension sidecar sealed alongside an event's canonical
/// VEVENT — the data with no faithful iCalendar home, **never** served to a
/// CalDAV MUA, read only by Fauna apps (caldav-server.md § Event resources).
///
/// Sealed exactly like the VEVENT body (single [`MailRecordEnvelope`](fauna_mls::wrapped_blob::MailRecordEnvelope) to the
/// actor's own recipient key — [`seal_fauna_ext`]) and stored opaquely in the
/// nest `encrypted_fauna_ext` column. The sidecar is a Fauna-only *refinement*
/// layer, authoritative only where it speaks: a MUA edit (no sidecar) preserves
/// it, a Fauna write replaces it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaunaEventExt {
    /// The attendee CAL-ADDRESS values this sidecar marks **Interested** — the
    /// asymmetric refinement over the wire `PARTSTAT=TENTATIVE`
    /// (caldav-server.md § RSVP semantics). Matched case-insensitively, with a
    /// leading `mailto:` tolerated ([`FaunaEventExt::is_interested`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interested_attendees: Vec<String>,
    /// Per-attendee nest-url resolution hints for the mailbox-less WS-RPC invite
    /// path (auto-resolved via `resolve_attendee_transport`, never manually
    /// entered): CAL-ADDRESS → nest base URL. Empty when every attendee is
    /// email-reachable (iMIP) or unresolved.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attendee_nest_urls: BTreeMap<String, String>,
    /// **Organizer binding, half 1** — 64-hex id of the nest-attested author of
    /// the inbound `REQUEST` that created this event. Together with
    /// [`Self::organizer_home_nest_url`] it is the principal a later inbound
    /// `CANCEL` / updating `REQUEST` must come from (caldav-server.md § Who may
    /// mutate an existing event over the inbound rail). `None` on every event
    /// that did not arrive over the scheduling rail (local, MUA-PUT, imported) —
    /// such an event is *unbound* and falls back to
    /// resolve-or-refuse, never to "anyone may".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizer_actor_id: Option<String>,
    /// **Organizer binding, half 2** — the home nest of the channel that
    /// creating `REQUEST` arrived on, as this client's own nest stamped it
    /// (`Some("")` = the recipient's own nest). An author attestation is only
    /// as honest as the nest that made it, so the binding always carries both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizer_home_nest_url: Option<String>,
}

/// Normalize a CAL-ADDRESS for sidecar matching: trim, lower-case, drop a
/// leading `mailto:`. So `MAILTO:Alice@example.com` and `alice@example.com` match.
fn normalize_caladdr(s: &str) -> String {
    let lower = s.trim().to_ascii_lowercase();
    lower.strip_prefix("mailto:").unwrap_or(&lower).to_string()
}

impl FaunaEventExt {
    /// True iff this sidecar explicitly marks `attendee` Interested. `attendee`
    /// is a CAL-ADDRESS (leading `mailto:` tolerated); matched case-insensitively.
    #[must_use]
    pub fn is_interested(&self, attendee: &str) -> bool {
        let norm = normalize_caladdr(attendee);
        self.interested_attendees
            .iter()
            .any(|a| normalize_caladdr(a) == norm)
    }

    /// The mailbox-less WS-RPC nest URL recorded for `attendee` in
    /// [`Self::attendee_nest_urls`], if any. `attendee` is a CAL-ADDRESS
    /// (leading `mailto:` tolerated); matched case-insensitively, mirroring
    /// [`Self::is_interested`] (so a non-normalized stored key still matches).
    /// `None` ⇒ the attendee is email-reachable (or was never resolved) — the
    /// Slice-5 dispatch fork routes that one over iMIP email instead.
    #[must_use]
    pub fn nest_url_for(&self, attendee: &str) -> Option<&str> {
        let norm = normalize_caladdr(attendee);
        self.attendee_nest_urls
            .iter()
            .find(|(addr, _)| normalize_caladdr(addr) == norm)
            .map(|(_, url)| url.as_str())
    }
}

/// Seal a [`FaunaEventExt`] to the actor's own recipient key (the SAME scheme as
/// [`seal_event_body`]), producing the `encrypted_fauna_ext` wire bytes. The
/// payload is canonical dag-CBOR so the bytes are deterministic.
pub fn seal_fauna_ext(ext: &FaunaEventExt, msek: &[u8; 32]) -> Result<Vec<u8>, SealError> {
    fauna_mls::wrapped_blob::dav_body::seal_dav_body_typed(ext, msek)
}

/// Post-quantum (X-Wing) sibling of [`seal_fauna_ext`] — the sidecar is standing-
/// key body content that rides the event, so it follows the event body's suite
/// selection (S3d leg D2c). [`unseal_fauna_ext`] opens either suite.
pub fn seal_fauna_ext_xwing(ext: &FaunaEventExt, msek: &[u8; 32]) -> Result<Vec<u8>, SealError> {
    fauna_mls::wrapped_blob::dav_body::seal_dav_body_typed_xwing(ext, msek)
}

/// Unseal an `encrypted_fauna_ext` produced by [`seal_fauna_ext`] back into a
/// typed [`FaunaEventExt`]. Returns [`SealError::Unseal`] on a wrong key /
/// tampered ciphertext or a non-decodable payload. Takes a pre-derived
/// [`DavRecipientKeys`] rather than a bare `msek` — the sidecar rides the
/// event body, so [`decode_event_entry`] already has one to hand it
/// .
pub fn unseal_fauna_ext(
    sealed: &[u8],
    keys: &DavRecipientKeys,
) -> Result<FaunaEventExt, SealError> {
    fauna_mls::wrapped_blob::dav_body::unseal_dav_body_typed(sealed, keys)
}

// ── Calendar collection metadata (caldav-server.md § Standard properties) ─────

/// The decrypted shape of a calendar collection's `encrypted_metadata` blob
/// (`bridge_caldav_calendars.encrypted_metadata`) — the MUA-visible DAV
/// properties a Fauna app and the MDA both read.
///
/// Defined once in `fauna_protocol::dav_identity` (the interop contract with the
/// MDA's Go `EncryptedCollectionMetadata` lives there, beside the Personal
/// calendar's id, name and colour) and sealed here with the SAME
/// single-[`MailRecordEnvelope`](fauna_mls::wrapped_blob::MailRecordEnvelope)
/// scheme ([`seal_event_body`] ≡ Go `EncryptToRecipient`);
/// [`unseal_calendar_metadata`] decodes with `fauna_cbor::decode_strict`, which
/// rejects non-canonical key order. The client-side `visibility` toggle is
/// **not** stored here (events.md § State & data shape — it is a local display flag).
pub use fauna_protocol::dav_identity::CalendarMetadata;

/// Seal a [`CalendarMetadata`] to the actor's own recipient key (the SAME scheme
/// as [`seal_event_body`] / the MDA's `SealCollectionMetadata`), producing the
/// `encrypted_metadata` wire bytes for `provision_calendar`.
pub fn seal_calendar_metadata(
    meta: &CalendarMetadata,
    msek: &[u8; 32],
) -> Result<Vec<u8>, SealError> {
    fauna_mls::wrapped_blob::dav_body::seal_dav_body_typed(meta, msek)
}

/// Unseal a calendar's `encrypted_metadata` (written by a Fauna app or the
/// MDA) back into a typed [`CalendarMetadata`]. An empty blob yields the default
/// (treated as "no metadata stored yet", mirroring the Go
/// `UnsealCollectionMetadata` empty-ciphertext path). Takes a pre-derived
/// [`DavRecipientKeys`] rather than a bare `msek` — `list_calendars` calls
/// this once per calendar, so a caller listing N calendars derives one keypair
/// up front instead of N .
pub fn unseal_calendar_metadata(
    sealed: &[u8],
    keys: &DavRecipientKeys,
) -> Result<CalendarMetadata, SealError> {
    fauna_mls::wrapped_blob::dav_body::unseal_dav_body_typed_or_default(sealed, keys)
}

// ── RSVP projection (caldav-server.md § RSVP semantics) ───────────────────────

/// Verbatim wire `PARTSTAT` → Fauna RSVP status, the asymmetric read **fallback**
/// (used when the sidecar does *not* mark the attendee).
///
/// Distinct from [`fauna_status_from_partstat`] (the *symmetric* inverse, which
/// maps `TENTATIVE`→`"interested"` for the lossy write round-trip): here a bare
/// `TENTATIVE` is **Tentative**, never Interested. The `interested` refinement
/// comes ONLY from the sidecar — a stock client picking Tentative shows as
/// Tentative in Fauna too (caldav-server.md § RSVP semantics).
#[must_use]
pub fn rsvp_status_verbatim(partstat: &str) -> &'static str {
    fauna_core::rsvp::RsvpState::from_partstat_verbatim(partstat).as_str()
}

/// The asymmetric attendee RSVP projection (caldav-server.md § RSVP semantics):
/// render **`"interested"`** iff `ext` explicitly marks `attendee` Interested,
/// else the verbatim wire `PARTSTAT` ([`rsvp_status_verbatim`]). `attendee` is
/// the CAL-ADDRESS as it appears in the VEVENT (`mailto:` prefix tolerated).
#[must_use]
pub fn project_attendee_rsvp(
    partstat: &str,
    attendee: &str,
    ext: Option<&FaunaEventExt>,
) -> &'static str {
    if let Some(ext) = ext
        && ext.is_interested(attendee)
    {
        return "interested";
    }
    rsvp_status_verbatim(partstat)
}

// ── iMIP dispatch ─────────────────────────────────────────────────────────────
//
// `ImipMessage` + `build_event_imip` now live in `fauna_core::ical` (lifted
// 2026-06-05, priority #2 — one impl shared by this crate's native/web apps
// AND the Go MDA's server-side auto-schedule gateway, which reaches them via the
// `fauna_mail::icalendar::build_event_imip_from_ics` UniFFI export). They are
// re-exported above (`pub use fauna_core::ical::{… build_event_imip, ImipMessage …}`)
// so the linux/native call sites are unchanged. caldav-server.md § Where logic lives.

// ── Transport: the typed `fauna.bridges.*` calendar call surface ──

/// Error from the high-level [`CalDavClient::seal_and_put_event`] — either the
/// client-side seal failed, or the WS-RPC transport did.
#[derive(Debug)]
pub enum PutEventError<E> {
    /// Sealing the VEVENT (or its index hint) failed before any RPC was sent.
    Seal(SealError),
    /// The `put_event_ciphertext` RPC failed.
    Transport(E),
}

impl<E: core::fmt::Display> core::fmt::Display for PutEventError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PutEventError::Seal(e) => write!(f, "{e}"),
            PutEventError::Transport(e) => write!(f, "put_event_ciphertext transport: {e}"),
        }
    }
}

/// Outcome of [`CalDavClient::import_ical_events`]: how many VEVENTs (out of
/// `total` parsed from the `.ics` text) were imported vs. skipped (unparseable,
/// or rejected by the nest). Every platform surfaces this the same way — a plain
/// counts triple, not an error — because a partial import is the expected
/// outcome for a real-world `.ics` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IcsImportOutcome {
    pub imported: u32,
    pub skipped: u32,
    pub total: u32,
}

/// Typed `fauna.bridges.*` calendar call surface, generic over the WS-RPC
/// transport (`R: RpcRequester`): native call sites pass `Arc<NestClient>`, the
/// UniFFI wrapper the same, the wasm SPA its `WsRpcClient`. A Fauna app calls
/// these against its OWN actor (nest caller-scopes every non-MDA caller —
/// `bridge_caldav_handlers::require_caller_scope`). Errors propagate as the
/// transport's `R::Error`.
pub struct CalDavClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> CalDavClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.bridges.provision_calendar` — MKCOL (`update_metadata=false`) or
    /// PROPPATCH (`true`). The client seals `encrypted_metadata` before calling.
    pub async fn provision_calendar(
        &self,
        req: ProvisionCalendarRequest,
    ) -> Result<ProvisionCalendarReply, R::Error> {
        self.nest
            .request("fauna.bridges.provision_calendar", req)
            .await
    }

    /// `fauna.bridges.list_calendars` — the actor's calendars (sealed metadata +
    /// ctag / highestmodseq / event_count per entry). Pure read.
    pub async fn list_calendars(
        &self,
        req: ListCalendarsRequest,
    ) -> Result<ListCalendarsReply, R::Error> {
        self.nest.request("fauna.bridges.list_calendars", req).await
    }

    /// `fauna.bridges.query_events` — paginated REPORT (calendar-query /
    /// multiget) over one collection. Returns sealed event bodies; the caller
    /// unseals via [`unseal_event_body`]. Pure read.
    pub async fn query_events(
        &self,
        req: QueryEventsRequest,
    ) -> Result<QueryEventsReply, R::Error> {
        self.nest.request("fauna.bridges.query_events", req).await
    }

    /// `fauna.bridges.put_event_ciphertext` — create/update an event by
    /// `uid_hash`. The body must already be sealed; prefer
    /// [`Self::seal_and_put_event`] which seals + builds the request.
    pub async fn put_event_ciphertext(
        &self,
        req: PutEventCiphertextRequest,
    ) -> Result<PutEventCiphertextReply, R::Error> {
        self.nest
            .request("fauna.bridges.put_event_ciphertext", req)
            .await
    }

    /// `fauna.bridges.delete_event` — tombstone an event by `uid_hash`.
    pub async fn delete_event(
        &self,
        req: DeleteEventRequest,
    ) -> Result<DeleteEventReply, R::Error> {
        self.nest.request("fauna.bridges.delete_event", req).await
    }

    /// `fauna.bridges.sync_calendar_since` — RFC 6578 sync-collection: changed
    /// events + tombstones since a `sync_token`. Pure read.
    pub async fn sync_calendar_since(
        &self,
        req: SyncCalendarSinceRequest,
    ) -> Result<SyncCalendarSinceReply, R::Error> {
        self.nest
            .request("fauna.bridges.sync_calendar_since", req)
            .await
    }

    /// Headline write op for the Events page: build the canonical VEVENT from
    /// `event` + `attendees` (via the reused [`generate_ical`] writer), seal it
    /// to `msek`, and PUT it to the actor's own calendar in one round-trip. The
    /// caller supplies `uid_hash = blake3(plaintext UID)` (the dedup key; the
    /// plaintext UID stays inside the sealed body).
    ///
    /// `fauna_ext` is the optional Fauna-extension sidecar (the `interested`
    /// RSVP refinement + per-attendee nest-url hints — caldav-server.md
    /// § Event resources). A Fauna app passes `Some(..)` to write/replace it;
    /// `None` writes no sidecar (and on an UPDATE the nest **preserves** the
    /// prior one, the MUA-edit-keeps-Fauna-refinement invariant). Sealed
    /// client-side with the same scheme as the body, so nest only sees ciphertext.
    #[allow(clippy::too_many_arguments)]
    pub async fn seal_and_put_event(
        &self,
        actor_id: &[u8; 32],
        calendar_id: &[u8; 32],
        uid_hash: &[u8; 32],
        msek: &[u8; 32],
        event: &EventFields,
        attendees: &[AttendeeInfo],
        organizer_email: &str,
        fauna_ext: Option<&FaunaEventExt>,
        timestamp: i64,
        if_match: Option<String>,
    ) -> Result<PutEventCiphertextReply, PutEventError<R::Error>> {
        // The shared stored-body renderer stamps DTSTAMP from the write time
        // (a VEVENT without one is invisible to every CalDAV MUA — GAP 2,
        // caldav-server.md § Event resources); the nest's placement of an
        // emailed invitation renders through the same function.
        let ics = render_stored_event(event, attendees, organizer_email, timestamp);
        // S3d leg D2c: seal the event body + Fauna-ext sidecar (standing-key
        // content) with X-Wing — no capability gate (`post-quantum.md`
        // § Capability negotiation, the 2026-09-24 ruling). The body
        // MailRecordEnvelope self-describes its suite, so a reader opens either.
        // The index-hint stays classical.
        let encrypted_body =
            seal_event_body_xwing(ics.as_bytes(), msek).map_err(PutEventError::Seal)?;
        // v1 index hint: sealing empty plaintext still yields a non-empty
        // envelope (nest requires `encrypted_index_hint` non-empty). Rich
        // search-index hints are a follow-up — the MDA tokenizes via the
        // native-only `fauna_mail` tokenizer, which is not WASM-safe.
        let encrypted_index_hint = seal_event_body(b"", msek).map_err(PutEventError::Seal)?;
        let encrypted_fauna_ext = match fauna_ext {
            Some(ext) => Some(seal_fauna_ext_xwing(ext, msek).map_err(PutEventError::Seal)?),
            None => None,
        };
        let ciphertext_size = encrypted_body.len() as u32;
        let req = PutEventCiphertextRequest {
            actor_id: actor_id.to_vec(),
            calendar_id: calendar_id.to_vec(),
            uid_hash: uid_hash.to_vec(),
            encrypted_body,
            encrypted_index_hint,
            timestamp,
            ciphertext_size,
            if_match,
            encrypted_fauna_ext,
        };
        self.put_event_ciphertext(req)
            .await
            .map_err(PutEventError::Transport)
    }

    /// Import a `.ics` file's VEVENTs into `calendar_id`: parse (shared
    /// [`parse_ical_multi`]), then [`Self::seal_and_put_event`] each one,
    /// counting imports vs. skips. A UID-less VEVENT gets a fallback UID from
    /// `fallback_uid(index)` — every platform mints its own scheme (a
    /// device-scoped random UID, an index-suffixed one, …), so this stays a
    /// caller-supplied closure rather than a fixed generator. v1 imports event
    /// fields only — attendee rosters on imported events are a follow-up.
    #[allow(clippy::too_many_arguments)]
    pub async fn import_ical_events(
        &self,
        actor_id: &[u8; 32],
        calendar_id: &[u8; 32],
        msek: &[u8; 32],
        organizer_email: &str,
        ics_text: &str,
        timestamp: i64,
        mut fallback_uid: impl FnMut(usize) -> String,
    ) -> IcsImportOutcome {
        let parsed = parse_ical_multi(ics_text);
        let total = parsed.len() as u32;
        let (mut imported, mut skipped) = (0u32, 0u32);
        for (idx, result) in parsed.into_iter().enumerate() {
            let Ok(mut fields) = result else {
                skipped += 1;
                continue;
            };
            if fields.uid.is_empty() {
                fields.uid = fallback_uid(idx);
            }
            let hash = uid_hash(&fields.uid);
            match self
                .seal_and_put_event(
                    actor_id,
                    calendar_id,
                    &hash,
                    msek,
                    &fields,
                    &[],
                    organizer_email,
                    None,
                    timestamp,
                    None,
                )
                .await
            {
                Ok(_) => imported += 1,
                Err(_) => skipped += 1,
            }
        }
        IcsImportOutcome {
            imported,
            skipped,
            total,
        }
    }
}

// ── Native read path: sealed event body → typed VEVENT (events.md § Where logic ─
// lives — "iCalendar serialize/parse"). Native-only because the parser is the
// crate-backed `fauna_mail::parse_icalendar` (the `icalendar`/`rrule` crates are
// not WASM-safe; caldav-server.md § iCalendar parsing rules). Web gets a separate
// parse story; the seal/unseal/transport surface above is already portable.

/// A single calendar event decoded from a [`bridge_routing::EventEntry`]: the
/// `query_events` / `sync_calendar_since` row metadata paired with its unsealed +
/// parsed canonical VEVENT. Feed [`Self::document`] to [`expand_recurrence`] for
/// RRULE expansion within a view window, or `Self::ics` to `fauna_core::ical`'s
/// high-level `parse_ical` for the flat [`EventFields`] view.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedEvent {
    /// Server-assigned 32-byte event id (deterministic blake3 hash).
    pub event_id: Vec<u8>,
    /// Blake3 hash of the plaintext CalDAV UID (the dedup key).
    pub uid_hash: Vec<u8>,
    /// ETag for conditional requests (`If-Match` / `If-None-Match`).
    pub etag: String,
    /// Modseq at which this event was last written.
    pub modseq: i64,
    /// Epoch seconds (CalDAV CREATED / LAST-MODIFIED surrogate).
    pub internal_date: i64,
    /// The unsealed canonical VEVENT bytes as UTF-8 `.ics` — the exact text the
    /// MDA serves to a CalDAV MUA. Kept alongside [`Self::document`] so a consumer
    /// can re-PUT verbatim or run `fauna_core::ical::parse_ical` for flat fields.
    pub ics: String,
    /// The parsed VCALENDAR tree.
    pub document: ICalDocument,
    /// The decoded Fauna-extension sidecar, when the row carried one. `None` for
    /// MUA-written events / pre-sidecar rows. Feed it (with each attendee's wire
    /// PARTSTAT) to [`project_attendee_rsvp`] for the asymmetric RSVP render.
    pub fauna_ext: Option<FaunaEventExt>,
}

/// Failure decoding one [`bridge_routing::EventEntry`] into a [`DecodedEvent`]
/// (native) or a [`FlatEvent`] (WASM-safe) — either the seal layer rejected the
/// body, or it was not valid iCalendar. Transport-free + WASM-safe (it carries no
/// native parser type), so [`decode_event_entry`] and [`decode_event_entry_flat`]
/// share it across `query_events`, `sync_calendar_since`, and offline-cached entries.
#[derive(Debug, Clone, PartialEq)]
pub enum DecodeEventError {
    /// HPKE-open failed (wrong `msek` / tampered body) or the bytes were not a
    /// valid [`MailRecordEnvelope`](fauna_mls::wrapped_blob::MailRecordEnvelope).
    Unseal(SealError),
    /// The unsealed body was not UTF-8 or not parseable iCalendar.
    Parse(String),
    /// The row carried an `encrypted_fauna_ext` sidecar that failed to unseal /
    /// decode (it seals to the same key as the body, so a failure here on a
    /// body that *did* decode indicates corruption — surfaced rather than
    /// silently dropping the `interested` refinement).
    Sidecar(SealError),
}

impl core::fmt::Display for DecodeEventError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DecodeEventError::Unseal(e) => write!(f, "{e}"),
            DecodeEventError::Parse(m) => write!(f, "parse event body: {m}"),
            DecodeEventError::Sidecar(e) => write!(f, "decode fauna sidecar: {e}"),
        }
    }
}

impl std::error::Error for DecodeEventError {}

/// Unseal + parse one [`bridge_routing::EventEntry`] (from a `query_events` or
/// `sync_calendar_since` reply) into a typed [`DecodedEvent`]. The
/// `encrypted_body` is opened with a pre-derived [`DavRecipientKeys`] rather
/// than a bare `msek` — [`CalDavClient::query_events_decoded`] derives one per
/// page and reuses it across every entry instead of paying a fresh X-Wing
/// keygen per event  — then parsed via the crate-backed
/// `fauna_mail::parse_icalendar`.
#[cfg(not(target_arch = "wasm32"))]
pub fn decode_event_entry(
    entry: &EventEntry,
    keys: &DavRecipientKeys,
) -> Result<DecodedEvent, DecodeEventError> {
    let plaintext = keys
        .unseal(&entry.encrypted_body)
        .map_err(DecodeEventError::Unseal)?;
    let ics = String::from_utf8(plaintext)
        .map_err(|e| DecodeEventError::Parse(format!("VEVENT body not UTF-8: {e}")))?;
    let document =
        parse_icalendar(ics.as_bytes()).map_err(|e| DecodeEventError::Parse(e.to_string()))?;
    let fauna_ext = match &entry.encrypted_fauna_ext {
        Some(sealed) => Some(unseal_fauna_ext(sealed, keys).map_err(DecodeEventError::Sidecar)?),
        None => None,
    };
    Ok(DecodedEvent {
        event_id: entry.event_id.clone(),
        uid_hash: entry.uid_hash.clone(),
        etag: entry.etag.clone(),
        modseq: entry.modseq,
        internal_date: entry.internal_date,
        ics,
        document,
        fauna_ext,
    })
}

// ── WASM-safe flat read path (the web parse story) ────────────────────────────
//
// The web Events page cannot link the crate-backed `fauna_mail::parse_icalendar`
// (the `icalendar`/`rrule` crates are not WASM-safe; caldav-server.md § iCalendar
// parsing rules), so it reaches the read path through the WASM-safe primitives:
// `unseal_event_body` → `fauna_core::ical::parse_ical` (flat `EventFields`) +
// `parse_ical_attendees` + the sidecar-authoritative `project_attendee_rsvp`. This
// is the named "web needs a wasm read-path follow-up" gap (events.md § Implementation
// status). Built here (priority #2) rather than reimplemented in the wasm glue, so
// web and the native flat-render path share one decode. The native `DecodedEvent`
// (above) keeps the richer parse tree + RRULE expansion the MUA-facing MDA needs.

/// One attendee in a [`FlatEvent`] — the rendered Events-page roster row. `rsvp`
/// is the **projected** status (`going` / `interested` / `tentative` / `declined`
/// / `invited`): [`project_attendee_rsvp`] applied, so the sidecar's `interested`
/// refinement is already resolved (a bare wire `TENTATIVE` with no sidecar renders
/// `tentative`, never `interested`). Mirrors the linux lead's `CalDavAttendee`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatAttendee {
    /// Display name (iCalendar `CN`); empty when the VEVENT carried none.
    pub name: String,
    /// Attendee email (the CAL-ADDRESS with any leading `mailto:` stripped).
    pub email: String,
    /// Projected RSVP status (sidecar-authoritative — see the struct docs).
    pub rsvp: String,
}

/// A single calendar event decoded into the WASM-safe **flat** view the web
/// Events page renders — the WASM-safe counterpart of the native [`DecodedEvent`].
/// Built by [`decode_event_entry_flat`] from a `query_events` /
/// `sync_calendar_since` [`EventEntry`]: the unsealed canonical `.ics`, its flat
/// [`EventFields`], the attendee roster with the asymmetric RSVP projection
/// applied, and the decoded Fauna sidecar (when present).
#[derive(Debug, Clone, PartialEq)]
pub struct FlatEvent {
    /// Server-assigned 32-byte event id.
    pub event_id: Vec<u8>,
    /// Blake3 hash of the plaintext CalDAV UID — the Events-page row id (web
    /// hex-encodes it; the same `uid_hash` a mutate-rewrite re-PUTs against).
    pub uid_hash: Vec<u8>,
    /// ETag for conditional requests (`If-Match`).
    pub etag: String,
    /// Modseq at which this event was last written.
    pub modseq: i64,
    /// Epoch seconds (CalDAV CREATED / LAST-MODIFIED surrogate).
    pub internal_date: i64,
    /// The unsealed canonical VEVENT `.ics` — re-PUT verbatim by the
    /// read-mutate-rewrite helpers ([`apply_rsvp`] / [`set_reminder`] /
    /// [`add_attendee`]).
    pub ics: String,
    /// Flat event fields (`summary`, `dtstart`/`dtend`, `location`,
    /// `description`, `alarm`, …) via [`parse_ical`].
    pub fields: EventFields,
    /// Attendee roster with the asymmetric RSVP projection already applied.
    pub attendees: Vec<FlatAttendee>,
    /// The decoded Fauna-extension sidecar, when the row carried one. `None` for
    /// MUA-written / pre-sidecar rows.
    pub fauna_ext: Option<FaunaEventExt>,
}

/// Unseal + flat-parse one [`EventEntry`] into a [`FlatEvent`] — the WASM-safe
/// mirror of the native [`decode_event_entry`]. Uses only WASM-safe primitives
/// (`unseal_event_body` → `fauna_core::ical::parse_ical`, [`parse_ical_attendees`],
/// `unseal_fauna_ext`, [`project_attendee_rsvp`]), so web links it. The roster
/// `rsvp` is sidecar-authoritative (an `interested` marking overrides the wire
/// `TENTATIVE`; caldav-server.md § RSVP semantics). Takes a pre-derived
/// [`DavRecipientKeys`] rather than a bare `msek`, same reason as
/// [`decode_event_entry`] .
pub fn decode_event_entry_flat(
    entry: &EventEntry,
    keys: &DavRecipientKeys,
) -> Result<FlatEvent, DecodeEventError> {
    let plaintext = keys
        .unseal(&entry.encrypted_body)
        .map_err(DecodeEventError::Unseal)?;
    let ics = String::from_utf8(plaintext)
        .map_err(|e| DecodeEventError::Parse(format!("VEVENT body not UTF-8: {e}")))?;
    let fields = parse_ical(&ics).map_err(|e| DecodeEventError::Parse(e.to_string()))?;
    let fauna_ext = match &entry.encrypted_fauna_ext {
        Some(sealed) => Some(unseal_fauna_ext(sealed, keys).map_err(DecodeEventError::Sidecar)?),
        None => None,
    };
    let attendees = parse_ical_attendees(&ics)
        .into_iter()
        .map(|a| FlatAttendee {
            rsvp: project_attendee_rsvp(&a.partstat, &a.email, fauna_ext.as_ref()).to_string(),
            name: a.name,
            email: a.email,
        })
        .collect();
    Ok(FlatEvent {
        event_id: entry.event_id.clone(),
        uid_hash: entry.uid_hash.clone(),
        etag: entry.etag.clone(),
        modseq: entry.modseq,
        internal_date: entry.internal_date,
        ics,
        fields,
        attendees,
        fauna_ext,
    })
}

/// `true` iff `self_email` organizes an event whose VEVENT `ORGANIZER` CAL-ADDRESS
/// is `organizer_email`: the event carries no organizer (a solo event in the
/// actor's own calendar) **or** the organizer matches the actor's email
/// (case-insensitive). Gates the author-only affordances (delete / invite); the
/// RSVP affordance shows when this is `false` (an event the actor was invited to).
///
/// Single-sourced here (priority #2) so the native `FfiCaldavClient` (`map_event`)
/// and the web wasm `flat_event_to_web_json` seam share one predicate rather than
/// each re-deriving it — the caldav organizer is an **email**, never an actor id,
/// so an actor-id compare would be wrong on every app.
pub fn organized_by_me(organizer_email: &str, self_email: &str) -> bool {
    organizer_email.is_empty()
        || (!self_email.is_empty() && organizer_email.eq_ignore_ascii_case(self_email))
}

/// Outcome of [`CalDavClient::query_events_decoded`]: a page of [`DecodedEvent`]s
/// plus the calendar's `highestmodseq` reference and a `more` pagination flag, or
/// the `CalendarNotFound` signal a fresh client gets before it provisions.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, PartialEq)]
pub enum DecodedEventsPage {
    /// Events decoded successfully.
    Ok {
        /// Decoded events, in the wire order (`event_id ASC`).
        events: Vec<DecodedEvent>,
        /// Current calendar `highestmodseq` (a stable reference for the next
        /// incremental sync), echoed from `QueryEventsReply::Ok`.
        highestmodseq: i64,
        /// `true` iff more pages remain (resume with `after_event_id`).
        more: bool,
    },
    /// No calendar row exists for `(actor_id, calendar_id)`.
    CalendarNotFound,
}

/// Error from a calendar read — the transport failed, or a returned event could
/// not be unsealed/parsed. Shared by the native [`CalDavClient::query_events_decoded`]
/// and the WASM-safe per-calendar flat read the inbound scheduling chain drives,
/// which is why it carries no `cfg`: both decode paths fail the same two ways.
#[derive(Debug)]
pub enum ReadEventsError<E> {
    /// The `query_events` RPC failed.
    Transport(E),
    /// One of the returned events could not be decoded (unseal/parse).
    Decode(DecodeEventError),
    /// The nest answered with an outcome this build does not know
    /// ([`QueryEventsReply::Unknown`]). An error, never `CalendarNotFound`:
    /// that would read as an empty calendar and forget the sync token.
    UnknownOutcome,
}

/// The message for [`ReadEventsError::UnknownOutcome`], shared with the wasm
/// glue that reads a calendar without this crate's error type.
pub const UNKNOWN_QUERY_OUTCOME: &str =
    "query_events: the nest answered with an outcome this version does not know";

impl<E: core::fmt::Display> core::fmt::Display for ReadEventsError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ReadEventsError::Transport(e) => write!(f, "query_events transport: {e}"),
            ReadEventsError::Decode(e) => write!(f, "{e}"),
            ReadEventsError::UnknownOutcome => f.write_str(UNKNOWN_QUERY_OUTCOME),
        }
    }
}

/// Outcome of an inbound iTIP `REPLY` — [`CalDavClient::apply_inbound_reply_from_mail`]
/// on the mail rail, the `REPLY` arm of
/// [`CalDavClient::apply_inbound_scheduling_from_message`] on the sealed rail —
/// whether it was merged into a stored event, and if not, why not. Every
/// non-error terminal of "did this reply change anything" so the inbound loops
/// can log the disposition without treating a no-op as a failure.
#[derive(Debug, Clone, PartialEq)]
pub enum InboundReplyOutcome {
    /// The `REPLY` was **refused** — it speaks for an attendee its sender
    /// cannot be shown to be (caldav-server.md § Who may mutate an existing
    /// event over the inbound rail). On the sealed rail the sender is the
    /// nest-attested origin; on the mail rail it is the address the delivery
    /// door authenticated and stamped into the sealed copy (*The mail rail*) —
    /// no stamp refuses [`RefusalReason::SenderUnauthenticated`], a stamp
    /// naming anyone other than the attendee refuses
    /// [`RefusalReason::NotTheAttendee`]. The stored event is left untouched;
    /// the caller surfaces the refusal
    /// ([`SchedulingApplyOutcome::refused_change_record`] /
    /// [`InboundReplyOutcome::refused_mail_change_record`]).
    Refused {
        /// The `uid_hash` the reply addressed — what ties the refusal to the
        /// event on the surface that reports it. Carried here for the same
        /// reason [`InboundRequestOutcome::Refused`] carries it: a refused
        /// message that named no event would be unreportable.
        uid_hash: [u8; 32],
        /// The stored event's title, or the reply's own `SUMMARY` when no
        /// event is stored under that UID. Empty when neither names one.
        summary: String,
        /// Why it was refused.
        reason: RefusalReason,
    },
    /// The reply matched a stored event and at least one rostered attendee's
    /// `PARTSTAT` changed; the event was re-PUT with the merged roster.
    Applied {
        /// The `uid_hash` of the updated event.
        uid_hash: [u8; 32],
        /// The merged roster after applying the reply.
        attendees: Vec<AttendeeInfo>,
    },
    /// No calendar of the actor holds an event with the reply's `UID` (a stale
    /// reply, or one for an event this actor never organized) — nothing merged.
    NoMatchingEvent,
    /// The event was found but the responder is not on its roster (RFC 5546
    /// §3.2.3 — the organizer tracks only invited attendees), or the reply did
    /// not change any attendee's PARTSTAT, so nothing was re-PUT.
    NoMatchingAttendee,
    /// The reply `.ics` carried no `UID`, so no stored event could be addressed.
    NoUid,
    /// (From [`CalDavClient::apply_inbound_reply_from_mail`]) the message had no
    /// `text/calendar` part, or its `method` was not `REPLY` — ordinary mail.
    NotCalendarReply,
}

/// Outcome of [`CalDavClient::apply_inbound_request`] / [`apply_inbound_cancel`]
/// — the recipient-side counterpart of [`InboundReplyOutcome`] for an inbound
/// iTIP `REQUEST` / `CANCEL` (caldav-server.md § Server-side auto-schedule). A
/// **mailbox-less** Fauna attendee (CalDAV enabled, email disabled) has no MTA
/// to seal an inbound invite onto their calendar the way a mail-enabled user
/// gets it server-side (§ :220), so their client applies it directly: a
/// `REQUEST` materializes the event (into the lazy `Personal` calendar when
/// unseen, or updates the matching row on an organizer re-send); a `CANCEL`
/// removes it.
#[derive(Debug, Clone, PartialEq)]
pub enum InboundRequestOutcome {
    /// The message was **refused** — not applied, and the stored event (if any)
    /// left untouched — because its origin is not the principal allowed to make
    /// this change (caldav-server.md § Who may mutate an existing event over the
    /// inbound rail). A successful outcome, not an error: the drain must not
    /// retry it, and the app surfaces it.
    Refused {
        /// The `uid_hash` the message addressed.
        uid_hash: [u8; 32],
        /// The title the **stored** event carries — what the user knows the
        /// event as, never what the refused message called it (a forged
        /// `CANCEL` chooses its own `SUMMARY` as freely as it chooses its
        /// `ORGANIZER`). A refused *creating* `REQUEST` has no stored event, so
        /// there this is the message's own title: naming what someone tried to
        /// plant is the point of the row.
        summary: String,
        /// Why it was refused.
        reason: RefusalReason,
    },
    /// (`REQUEST`) No calendar held the invite's `UID`, so the event was
    /// materialized into the recipient's default lazy `Personal` calendar
    /// (provisioned first if absent — [`personal_calendar_id`]).
    Created {
        /// The `uid_hash` of the materialized event.
        uid_hash: [u8; 32],
        /// The calendar the event landed in (the `Personal` calendar).
        calendar_id: [u8; 32],
    },
    /// (`REQUEST`) The invite's `UID` already existed in one of the recipient's
    /// calendars (an organizer re-send / reschedule), so that row was updated
    /// in place — the recipient's local Fauna sidecar is preserved (a `None`
    /// sidecar on the re-PUT, so the nest keeps the prior one).
    Updated {
        /// The `uid_hash` of the updated event.
        uid_hash: [u8; 32],
        /// The calendar the matching event lives in.
        calendar_id: [u8; 32],
    },
    /// (`CANCEL`) The matching event was found and tombstoned.
    Cancelled {
        /// The `uid_hash` of the cancelled event.
        uid_hash: [u8; 32],
        /// The calendar the cancelled event lived in.
        calendar_id: [u8; 32],
    },
    /// (`CANCEL`) No calendar of the recipient holds an event with the message's
    /// `UID` (a stale or duplicate cancel) — nothing was removed.
    NoMatchingEvent,
    /// The message `.ics` carried no `UID`, so no event could be addressed.
    NoUid,
}

/// Where an inbound scheduling iMIP came from, as far as anything its sender
/// does not control can say — the calendar-side twin of
/// `fauna_conversations::backend::SchedulingOrigin` (kept separate so neither
/// crate depends on the other; the sinks copy the two fields across).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboundOrigin {
    /// 64-hex id of the actor the channel's home nest authenticated as the
    /// record's poster. `None` = the nest gave no answer (a record a nest-side
    /// writer appended with no authenticated sender, or a page whose authors
    /// read faulted) — never permission.
    pub author: Option<String>,
    /// The channel's home-nest base URL as the recipient's own nest stamped it;
    /// empty = the recipient's own nest.
    pub home_nest_url: String,
}

/// A principal on the scheduling rail: an actor **and** the nest vouching for
/// it. Equality is what the inbound-mutation rule compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulingPrincipal {
    /// 64-hex actor id.
    pub actor_id: String,
    /// Home-nest base URL; empty = the recipient's own nest.
    pub home_nest_url: String,
}

impl SchedulingPrincipal {
    fn matches(&self, actor_id: &str, home_nest_url: &str) -> bool {
        self.actor_id.eq_ignore_ascii_case(actor_id)
            && nest_urls_eq(&self.home_nest_url, home_nest_url)
    }
}

/// Nest base URLs compare case-insensitively with a trailing `/` ignored.
fn nest_urls_eq(a: &str, b: &str) -> bool {
    a.trim()
        .trim_end_matches('/')
        .eq_ignore_ascii_case(b.trim().trim_end_matches('/'))
}

/// Resolves a CAL-ADDRESS to the principal it names, or gives **no answer**.
/// Used only where no binding exists to compare against: at creation (to refuse
/// a definitively spoofed `ORGANIZER`), for an *unbound* stored event
/// (resolve-or-refuse), and for the attendee a `REPLY` speaks for. `None` must
/// mean "could not say" — unreachable domain, not a Fauna actor, a transport
/// fault — and every caller treats it as the absence of permission. wasm-safe
/// (static dispatch, no `Send` bound). The shipped implementations live beside
/// the two `SchedulingSink`s that hand them in — native in
/// `fauna-client-conversations`, web in `fauna-wasm` — and both answer
/// addresses through the one [`DiscoveryPrincipalResolver`] over
/// [`AnonAttendeeDiscovery`]; each adds its platform's verified succession
/// walk.
#[allow(async_fn_in_trait)] // static-dispatch only, like `RpcRequester`
pub trait PrincipalResolver {
    /// The principal `caladdr` names, or `None` for no answer.
    async fn resolve_principal(&self, caladdr: &str) -> Option<SchedulingPrincipal>;

    /// The actor id (64-hex) the identity `bound` names has **verifiably
    /// succeeded to** — where the account ended up, never an intermediate hop —
    /// or `None` for *no answer* (never succeeded, unreachable, unverifiable, or
    /// a platform with no lookup).
    ///
    /// Asked only after the plain comparison has missed, for a bound event
    /// (`caldav-server.md` § Who may mutate an existing event over the inbound
    /// rail → *A succeeded organizer*). An implementation owes the verification
    /// rule of `identity-succession.md` § The succession statement: the signed
    /// statements are fetched from **`bound.home_nest_url`** (empty = the
    /// recipient's own nest) — the old identity's home nest, never any URL the
    /// inbound message declares — and verified against that identity's
    /// registration chain. A chain handed over by the party asserting the
    /// succession is a hint to go fetch, never the anchor. The default is *no
    /// answer*, which leaves the refusal exactly where it was.
    async fn resolve_successor(&self, bound: &SchedulingPrincipal) -> Option<String> {
        let _ = bound;
        None
    }
}

/// The resolver that knows nothing: always *no answer*. Bound events still
/// mutate (the comparison is local); unbound ones are refused, and so are a
/// succeeded organizer and every `REPLY`. No shipped sink passes it — it is
/// the floor the rule is tested against.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPrincipalResolver;

impl PrincipalResolver for NoPrincipalResolver {
    async fn resolve_principal(&self, _caladdr: &str) -> Option<SchedulingPrincipal> {
        None
    }
}

/// Why an inbound scheduling message was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    /// The home nest attested no author for the record, so nothing identifies
    /// who sent a message that would change an existing event.
    NoAttestedAuthor,
    /// The message's origin is not the principal the event is bound to (or, for
    /// an unbound event, not the one its stored `ORGANIZER` resolves to).
    NotTheOrganizer,
    /// The message tries to change the event's `ORGANIZER` line.
    OrganizerChanged,
    /// The event is unbound and its stored `ORGANIZER` resolves to no Fauna
    /// principal — nobody can be shown to hold authority over it on this rail.
    OrganizerUnresolvable,
    /// A creating `REQUEST` whose `ORGANIZER` definitively resolves to someone
    /// other than its origin — a spoofed organizer / UID squat.
    SpoofedOrganizer,
    /// A `REPLY` speaking for an attendee who definitively resolves to someone
    /// other than its origin.
    NotTheAttendee,
    /// A `REPLY` speaking for an attendee whose address resolves to no Fauna
    /// principal — nothing shows that its origin is the one it speaks for.
    AttendeeUnresolvable,
    /// A mailed `REPLY` whose sealed copy carries no authenticated-sender
    /// stamp — the door verified nobody (caldav-server.md § Who may mutate an
    /// existing event over the inbound rail → *The mail rail*: no stamp is no
    /// answer, and no answer refuses). The mail-rail twin of
    /// [`Self::NoAttestedAuthor`].
    SenderUnauthenticated,
}

impl RefusalReason {
    /// The at-rest / display vocabulary for this reason — what a
    /// [`fauna_core::data::RefusedSchedulingChange`] row stores in its `reason`
    /// field.
    ///
    /// **This crate owns the vocabulary because it owns the decision.** The
    /// at-rest row stores the plain string rather than an enum so that a build
    /// which does not name a newer reason round-trips it verbatim instead of
    /// losing it on the next rewrite of the ledger row (`fauna_core::data` states that
    /// rule in full on `MemberUnattestedReason`).
    #[must_use]
    pub fn as_wire(&self) -> &'static str {
        match self {
            Self::NoAttestedAuthor => "no_attested_author",
            Self::NotTheOrganizer => "not_the_organizer",
            Self::OrganizerChanged => "organizer_changed",
            Self::OrganizerUnresolvable => "organizer_unresolvable",
            Self::SpoofedOrganizer => "spoofed_organizer",
            Self::NotTheAttendee => "not_the_attendee",
            Self::AttendeeUnresolvable => "attendee_unresolvable",
            Self::SenderUnauthenticated => "sender_unauthenticated",
        }
    }

    /// The reason a stored row names, or `None` for a token this build does not
    /// know — a surface renders that row as *refused* without a reason rather
    /// than hiding it (fail-visible, the rule every adjudication surface here
    /// follows).
    #[must_use]
    pub fn from_wire(raw: &str) -> Option<Self> {
        Some(match raw {
            "no_attested_author" => Self::NoAttestedAuthor,
            "not_the_organizer" => Self::NotTheOrganizer,
            "organizer_changed" => Self::OrganizerChanged,
            "organizer_unresolvable" => Self::OrganizerUnresolvable,
            "spoofed_organizer" => Self::SpoofedOrganizer,
            "not_the_attendee" => Self::NotTheAttendee,
            "attendee_unresolvable" => Self::AttendeeUnresolvable,
            "sender_unauthenticated" => Self::SenderUnauthenticated,
            _ => return None,
        })
    }
}

/// Error from [`CalDavClient::apply_inbound_request`] / [`apply_inbound_cancel`]
/// — and of every inbound `REPLY` merge, on either rail. Genuine transport /
/// decode / write failures only (`Seal`/`Provision`/`Delete` are the
/// request/cancel paths'; a `REPLY` never produces them); the expected "didn't
/// apply" terminals live in [`InboundRequestOutcome`] / [`InboundReplyOutcome`].
#[derive(Debug)]
pub enum InboundScheduleError<E> {
    /// The message `.ics` was not parseable iCalendar.
    Parse(String),
    /// Sealing the default `Personal`-calendar metadata failed before any RPC.
    Seal(SealError),
    /// `list_calendars` transport failed.
    ListCalendars(E),
    /// `query_events` transport or one event's decode failed while searching.
    Read(ReadEventsError<E>),
    /// `provision_calendar` (ensure-`Personal`) transport failed.
    Provision(E),
    /// Materialize / re-PUT of the event failed (seal or transport).
    Put(PutEventError<E>),
    /// `delete_event` (the `CANCEL` path) transport failed.
    Delete(E),
}

impl<E: core::fmt::Display> core::fmt::Display for InboundScheduleError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            InboundScheduleError::Parse(m) => write!(f, "parse schedule ics: {m}"),
            InboundScheduleError::Seal(e) => write!(f, "seal personal metadata: {e}"),
            InboundScheduleError::ListCalendars(e) => write!(f, "list_calendars transport: {e}"),
            InboundScheduleError::Read(e) => write!(f, "{e}"),
            InboundScheduleError::Provision(e) => write!(f, "provision_calendar transport: {e}"),
            InboundScheduleError::Put(e) => write!(f, "{e}"),
            InboundScheduleError::Delete(e) => write!(f, "delete_event transport: {e}"),
        }
    }
}

impl<E: core::fmt::Debug + core::fmt::Display> std::error::Error for InboundScheduleError<E> {}

/// Outcome of [`CalDavClient::apply_inbound_scheduling_from_message`] — the
/// METHOD-routed disposition of an inbound scheduling iMIP that arrived over the
/// mailbox-less WS-RPC rail (a `WelcomeChannelKind::Scheduling` channel;
/// caldav-server.md § Server-side auto-schedule, Half-1). `REQUEST`/`CANCEL`
/// share [`InboundRequestOutcome`] (the recipient materializes / tombstones an
/// event); `REPLY` uses [`InboundReplyOutcome`] (an organizer merges an RSVP).
/// The unified return the receive-loop scheduling drain logs without re-parsing —
/// the scheduling twin of how `apply_inbound_reply_from_mail` returns the bare
/// [`InboundReplyOutcome`] off the mail rail.
#[derive(Debug, Clone, PartialEq)]
pub enum SchedulingApplyOutcome {
    /// A `REQUEST` (materialize / update in place) or a `CANCEL` (tombstone) was
    /// applied to the recipient's calendar.
    Request(InboundRequestOutcome),
    /// A `REPLY` was merged into the organizer's stored event.
    Reply(InboundReplyOutcome),
    /// The message had no `text/calendar` part, or its `method` was not one of
    /// `REQUEST` / `REPLY` / `CANCEL` — nothing applied (the cheap common case for
    /// any non-scheduling traffic that reaches the drain).
    NotScheduling,
}

impl SchedulingApplyOutcome {
    /// `true` when the message was refused by the inbound-mutation rule — what
    /// a sink logs at `warn` and an app surfaces.
    pub fn is_refused(&self) -> bool {
        matches!(
            self,
            Self::Request(InboundRequestOutcome::Refused { .. })
                | Self::Reply(InboundReplyOutcome::Refused { .. })
        )
    }

    /// The **user-facing record** of this refusal, or `None` when nothing was
    /// refused (`caldav-server.md` § Who may mutate an existing event over the
    /// inbound rail → *Surfacing*).
    ///
    /// The one place a refusal becomes a row, so that both `SchedulingSink`
    /// implementors — native and web — only persist what this composes and no
    /// app carries any of it (priority #2; the same "the sinks only pass
    /// values through" split the rule's enforcement already has).
    ///
    /// `method` is the refused iTIP `METHOD`, which the caller holds from its
    /// own routing; `now_secs` is the caller's clock (this crate stays
    /// clock-free, as every other entry point here does).
    ///
    /// ⚠ **The author comes from `origin`, never from the `.ics`.** A
    /// co-attendee's forged `CANCEL` carries the real organizer's `ORGANIZER`
    /// line verbatim — that is the attack the rule exists to stop — so a row
    /// built from the message would name the victim as the culprit.
    #[must_use]
    pub fn refused_change_record(
        &self,
        method: &str,
        origin: &InboundOrigin,
        now_secs: i64,
    ) -> Option<fauna_core::data::RefusedSchedulingChange> {
        let (uid_hash, summary, reason) = match self {
            Self::Request(InboundRequestOutcome::Refused {
                uid_hash,
                summary,
                reason,
            })
            | Self::Reply(InboundReplyOutcome::Refused {
                uid_hash,
                summary,
                reason,
            }) => (uid_hash, summary, reason),
            _ => return None,
        };
        Some(refused_row(
            uid_hash,
            summary,
            *reason,
            method,
            RefusedParty {
                author: origin
                    .author
                    .as_deref()
                    .map(|a| a.trim().to_ascii_lowercase())
                    .filter(|a| !a.is_empty()),
                author_home_nest_url: origin.home_nest_url.clone(),
                sender_address: String::new(),
            },
            now_secs,
        ))
    }
}

impl InboundReplyOutcome {
    /// The **user-facing record** of a refused **mailed** `REPLY`, or `None`
    /// when this outcome is not a refusal — the mail-rail twin of
    /// [`SchedulingApplyOutcome::refused_change_record`] (`caldav-server.md`
    /// § Who may mutate an existing event over the inbound rail → *Surfacing*
    /// and *The mail rail*).
    ///
    /// `raw_rfc5322` is the SAME sealed-copy bytes the outcome came from
    /// ([`CalDavClient::apply_inbound_reply_from_mail`]'s input): the row's
    /// sender is read off that copy's `X-Fauna-Authenticated-Sender` stamp
    /// here, by the one reader the gate used, so no caller can name anyone
    /// else. The row carries no actor (`author: None`, no home nest) and
    /// names the stamped address in `sender_address` — empty when the copy
    /// carried no stamp, which is exactly why it was refused.
    ///
    /// ⚠ **Never the message's `From:`** — a line the sender writes.
    #[must_use]
    pub fn refused_mail_change_record(
        &self,
        raw_rfc5322: &[u8],
        now_secs: i64,
    ) -> Option<fauna_core::data::RefusedSchedulingChange> {
        let Self::Refused {
            uid_hash,
            summary,
            reason,
        } = self
        else {
            return None;
        };
        Some(refused_row(
            uid_hash,
            summary,
            *reason,
            "REPLY",
            RefusedParty {
                author: None,
                author_home_nest_url: String::new(),
                sender_address: fauna_mail::sender_auth::read_authenticated_sender_stamp(
                    raw_rfc5322,
                )
                .unwrap_or_default(),
            },
            now_secs,
        ))
    }
}

/// Who a refused-change row names — the attested actor pair on the sealed
/// rail, the door-authenticated address on the mail rail.
struct RefusedParty {
    author: Option<String>,
    author_home_nest_url: String,
    sender_address: String,
}

/// The one place a refusal becomes a [`fauna_core::data::RefusedSchedulingChange`]
/// row, for both rails.
fn refused_row(
    uid_hash: &[u8; 32],
    summary: &str,
    reason: RefusalReason,
    method: &str,
    party: RefusedParty,
    now_secs: i64,
) -> fauna_core::data::RefusedSchedulingChange {
    fauna_core::data::RefusedSchedulingChange {
        uid_hash: hex::encode(uid_hash),
        author: party.author,
        author_home_nest_url: party.author_home_nest_url,
        sender_address: party.sender_address,
        method: method.trim().to_ascii_uppercase(),
        reason: reason.as_wire().to_string(),
        summary: summary.to_string(),
        first_refused_at: now_secs,
        last_refused_at: now_secs,
        // The counting is the `fauna.state.refused-scheduling-changes` merge's:
        // it owns whether this is a new row or another attempt on one already
        // on file.
        occurrences: 0,
        dismissed_through: 0,
        extra: Default::default(),
    }
}

/// Whose authority a `REPLY` is judged under — the one input that differs
/// between the two rails' otherwise identical look → roster-bound → authorize
/// → write chain (`CalDavClient::apply_reply_under`).
enum ReplyAuthority<'a, P> {
    /// The sealed scheduling rail: the nest-attested origin, checked by
    /// resolving each on-roster attendee (`CalDavClient::authorize_reply`).
    Rail {
        origin: &'a InboundOrigin,
        resolver: &'a P,
    },
    /// The mail rail: the address the delivery door authenticated and stamped
    /// into the sealed copy, `None` when it stamped nothing
    /// ([`authorize_mailed_reply`]).
    Mail {
        authenticated_sender: Option<&'a str>,
    },
}

/// The reply's `ATTENDEE` lines the stored roster already carries — the only
/// ones [`apply_reply_to_roster`] acts on, matched exactly as it matches them,
/// so the only ones either rail's check has to authorize.
fn on_roster_reply_attendees(
    stored_roster: &[AttendeeInfo],
    reply_ics: &str,
) -> impl Iterator<Item = AttendeeInfo> {
    parse_ical_attendees(reply_ics).into_iter().filter(|a| {
        stored_roster
            .iter()
            .any(|s| s.email.eq_ignore_ascii_case(&a.email))
    })
}

/// May the sender the delivery door authenticated speak for every attendee on
/// `stored_roster` a mailed `REPLY` names? (caldav-server.md § Who may mutate
/// an existing event over the inbound rail → *The mail rail*.)
///
/// `authenticated_sender` is the sealed copy's `X-Fauna-Authenticated-Sender`
/// stamp ([`fauna_mail::sender_auth::read_authenticated_sender_stamp`]) —
/// `None` refuses [`RefusalReason::SenderUnauthenticated`]: no stamp is no
/// answer, and no answer refuses. Each on-roster `ATTENDEE` the reply speaks
/// for must BE that address (case-insensitive, `mailto:` stripped), else
/// [`RefusalReason::NotTheAttendee`]. **Nothing inside the `.ics` is evidence
/// of authority** — its `ATTENDEE` lines are exactly what a spoofer writes —
/// and nothing is resolved: the door already named the sender, so this dials
/// no one.
fn authorize_mailed_reply(
    stored_roster: &[AttendeeInfo],
    reply_ics: &str,
    authenticated_sender: Option<&str>,
) -> Result<(), RefusalReason> {
    let Some(sender) = authenticated_sender else {
        return Err(RefusalReason::SenderUnauthenticated);
    };
    let sender = normalize_caladdr(sender);
    if on_roster_reply_attendees(stored_roster, reply_ics)
        .any(|attendee| normalize_caladdr(&attendee.email) != sender)
    {
        return Err(RefusalReason::NotTheAttendee);
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
impl<R: RpcRequester> CalDavClient<R> {
    /// Headline read op for the Events page: `query_events` (RPC) → unseal +
    /// parse each returned body → typed [`DecodedEvent`]s. The mirror of
    /// [`Self::seal_and_put_event`]; together they are the full encrypted-store
    /// round-trip a Fauna app drives against its own calendar. Takes a
    /// pre-derived [`DavRecipientKeys`] rather than a bare `msek` — a caller
    /// gathering across several calendars (`query_invited_events`,
    /// `find_event`) derives ONE and passes it to every page, so N events
    /// across M calendars still cost one keygen, not M or N .
    pub async fn query_events_decoded(
        &self,
        req: QueryEventsRequest,
        keys: &DavRecipientKeys,
    ) -> Result<DecodedEventsPage, ReadEventsError<R::Error>> {
        match self
            .query_events(req)
            .await
            .map_err(ReadEventsError::Transport)?
        {
            QueryEventsReply::Ok {
                events,
                highestmodseq,
                more,
            } => {
                let decoded = events
                    .iter()
                    .map(|e| decode_event_entry(e, keys))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(ReadEventsError::Decode)?;
                Ok(DecodedEventsPage::Ok {
                    events: decoded,
                    highestmodseq,
                    more,
                })
            }
            QueryEventsReply::CalendarNotFound => Ok(DecodedEventsPage::CalendarNotFound),
            QueryEventsReply::Unknown => Err(ReadEventsError::UnknownOutcome),
        }
    }
}

// ── The mailbox-less inbound scheduling rail (WASM-safe — every app) ────────
//
// Split out of the native-only impl above deliberately. What
// `caldav-server.md` § iCalendar parsing rules makes native/MDA-only is the
// *crate-backed* parser `fauna_mail::icalendar::{parse_icalendar,
// expand_recurrence}` (the `icalendar`/`rrule` crates are not WASM-safe), and
// the only thing above that needs it is `query_events_decoded`'s
// `DecodedEvent`. The inbound-apply chain below touches none of it: it reads
// `.ics` through the hand-rolled, WASM-safe `fauna_core::ical::parse_ical`
// family — the same “web parse story” `decode_event_entry_flat` above already
// rides — plus `fauna_mail::parser::extract_text_calendar_part`, which is a
// MIME walk over `mail_parser` and is already in web's graph.
//
// So these methods were gated out of wasm only by sharing an impl block
// with one native-only sibling, never by the ratified design. Web needs them
// for the inbound half of the mailbox-less rail: an invitation sent from
// another calendar has to land on the SPA's Events page too
// (`docs/features/calendar-and-events.md` outcome 10). Keep the split honest — a method added here must stay clear of the
// crate-backed parser, or it belongs above.
impl<R: RpcRequester> CalDavClient<R> {
    /// One calendar's events in the WASM-safe [`FlatEvent`] view; `None` when the
    /// calendar row does not exist.
    ///
    /// Deliberately **not** [`Self::query_events_decoded`]. Everything the
    /// inbound `REQUEST` / `CANCEL` / `REPLY` chain reads off a stored row is its
    /// `uid_hash` and its verbatim `.ics` — which it then re-parses with the
    /// WASM-safe `parse_ical` family a few lines later — so decoding through the
    /// crate-backed `fauna_mail::parse_icalendar` would build a rich VEVENT tree
    /// only to drop it, and would pin the whole chain to native for no gain.
    /// [`decode_event_entry_flat`] gives both fields on every target.
    async fn events_flat_in_calendar(
        &self,
        actor_id: &[u8; 32],
        calendar_id: &[u8; 32],
        keys: &DavRecipientKeys,
    ) -> Result<Option<Vec<FlatEvent>>, ReadEventsError<R::Error>> {
        let reply = self
            .query_events(QueryEventsRequest {
                actor_id: actor_id.to_vec(),
                calendar_id: calendar_id.to_vec(),
                since_modseq: None,
                after_event_id: None,
                limit: 0,
            })
            .await
            .map_err(ReadEventsError::Transport)?;
        match reply {
            QueryEventsReply::Ok { events, .. } => Ok(Some(
                events
                    .iter()
                    .map(|e| decode_event_entry_flat(e, keys))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(ReadEventsError::Decode)?,
            )),
            QueryEventsReply::CalendarNotFound => Ok(None),
            QueryEventsReply::Unknown => Err(ReadEventsError::UnknownOutcome),
        }
    }

    /// Export one calendar as a single RFC 5545 `.ics` document — the export
    /// half of `events.md` § Import / Export, the mirror of
    /// [`Self::import_ical_events`]: read every stored event, re-serialize each
    /// through the shared [`generate_ical_multi`] writer (event fields plus the
    /// attendee roster), and hand back the text for the app to write to its
    /// downloads location. An empty or missing calendar is an empty VCALENDAR.
    /// A stored row that will not unseal or parse is left out rather than
    /// failing the file — one bad row must not make a whole calendar
    /// unexportable. WASM-safe (the body unseal and the flat parser), so every app, web
    /// included, exports through this one call.
    pub async fn export_calendar_ics(
        &self,
        actor_id: &[u8; 32],
        calendar_id: &[u8; 32],
        keys: &DavRecipientKeys,
    ) -> Result<String, ReadEventsError<R::Error>> {
        let reply = self
            .query_events(QueryEventsRequest {
                actor_id: actor_id.to_vec(),
                calendar_id: calendar_id.to_vec(),
                since_modseq: None,
                after_event_id: None,
                limit: 0,
            })
            .await
            .map_err(ReadEventsError::Transport)?;
        let entries = match reply {
            QueryEventsReply::Ok { events, .. } => events,
            QueryEventsReply::CalendarNotFound => Vec::new(),
            // Never an empty export: a calendar this build cannot read must not
            // look like a calendar with no events.
            QueryEventsReply::Unknown => return Err(ReadEventsError::UnknownOutcome),
        };
        let events: Vec<(EventFields, Vec<AttendeeInfo>, String)> = entries
            .iter()
            .filter_map(|e| {
                // Only the body: an export carries the VEVENT, never the
                // Fauna sidecar, so a sidecar that will not open costs nothing.
                let ics = String::from_utf8(keys.unseal(&e.encrypted_body).ok()?).ok()?;
                let fields = parse_ical(&ics).ok()?;
                Some((fields, parse_ical_attendees(&ics), String::new()))
            })
            .collect();
        Ok(generate_ical_multi(&events))
    }

    /// Re-seal and re-PUT `stored` with the `merged` roster a `REPLY` produced
    /// from `prior_roster` — the write half every `REPLY` path shares. Skips the
    /// re-PUT when nothing changed (responder off-roster, or same PARTSTAT) so
    /// a no-op doesn't churn the event's modseq / SEQUENCE.
    #[allow(clippy::too_many_arguments)]
    async fn put_reply_merge(
        &self,
        actor_id: &[u8; 32],
        calendar_id: &[u8; 32],
        target: &[u8; 32],
        msek: &[u8; 32],
        stored: &FlatEvent,
        prior_roster: &[AttendeeInfo],
        merged: Vec<AttendeeInfo>,
        timestamp: i64,
    ) -> Result<InboundReplyOutcome, InboundScheduleError<R::Error>> {
        if merged == prior_roster {
            return Ok(InboundReplyOutcome::NoMatchingAttendee);
        }

        let mut fields =
            parse_ical(&stored.ics).map_err(|e| InboundScheduleError::Parse(e.to_string()))?;
        fields.sequence = fields.sequence.saturating_add(1);
        let organizer = parse_ical_organizer(&stored.ics).unwrap_or_default();

        self.seal_and_put_event(
            actor_id,
            calendar_id,
            target,
            msek,
            &fields,
            &merged,
            &organizer,
            stored.fauna_ext.as_ref(),
            timestamp,
            None,
        )
        .await
        .map_err(InboundScheduleError::Put)?;

        Ok(InboundReplyOutcome::Applied {
            uid_hash: *target,
            attendees: merged,
        })
    }

    /// The one `REPLY` chain both rails run — the v1 client-driven half of
    /// caldav-server.md § *The one operation with a cost*: the organizer's
    /// Fauna app merges the responder's `PARTSTAT` into its stored event and
    /// re-PUTs it, so their calendar (and any CalDAV MUA they use) reflects the
    /// response on next sync — **with the inbound mutation rule in front of the
    /// write** (§ Who may mutate an existing event over the inbound rail).
    /// `authority` is the only thing the rails differ in.
    ///
    /// **Look first, roster-bound second, authorize third, write last.** The
    /// stored event is located by UID across **all** the actor's calendars (a
    /// `REPLY` carries no collection hint) before anything is checked, and only
    /// the attendees its stored roster already carries are authorized — the
    /// ones the merge can act on. The sealed rail is stranger-reachable and
    /// every resolution dials the address's own host (on web, a browser
    /// `WebSocket`), so an unknown UID, an off-roster address, or a `REPLY`
    /// that would change nothing dials no one: each comes back as the no-op it
    /// is (`NoMatchingEvent` / `NoMatchingAttendee`), never as a refusal. One
    /// calendar walk serves the lookup, the check, the refusal's subject and
    /// the merge.
    ///
    /// The merge itself ([`apply_reply_to_roster`]) changes only the
    /// responding attendees' `PARTSTAT`; the Fauna sidecar is preserved
    /// **verbatim** (a remote `TENTATIVE` renders Tentative, never the
    /// Fauna-local *interested* refinement — § RSVP semantics), and `SEQUENCE`
    /// bumps so a downstream MUA treats the re-PUT as an update. `timestamp` is
    /// the re-PUT's epoch-seconds CREATED/LAST-MODIFIED surrogate
    /// (caller-supplied so the crate stays clock-free).
    async fn apply_reply_under<P: PrincipalResolver>(
        &self,
        actor_id: &[u8; 32],
        msek: &[u8; 32],
        reply_ics: &str,
        timestamp: i64,
        authority: ReplyAuthority<'_, P>,
    ) -> Result<InboundReplyOutcome, InboundScheduleError<R::Error>> {
        let reply_fields =
            parse_ical(reply_ics).map_err(|e| InboundScheduleError::Parse(e.to_string()))?;
        if reply_fields.uid.trim().is_empty() {
            return Ok(InboundReplyOutcome::NoUid);
        }
        let target = uid_hash(&reply_fields.uid);
        let Some((calendar_id, stored)) = self.find_stored_event(actor_id, msek, &target).await?
        else {
            return Ok(InboundReplyOutcome::NoMatchingEvent);
        };
        let prior_roster = parse_ical_attendees(&stored.ics);
        let merged = apply_reply_to_roster(&prior_roster, reply_ics);
        if merged == prior_roster {
            return Ok(InboundReplyOutcome::NoMatchingAttendee);
        }
        let verdict = match authority {
            ReplyAuthority::Rail { origin, resolver } => {
                Self::authorize_reply(&prior_roster, reply_ics, origin, resolver).await
            }
            ReplyAuthority::Mail {
                authenticated_sender,
            } => authorize_mailed_reply(&prior_roster, reply_ics, authenticated_sender),
        };
        if let Err(reason) = verdict {
            // The STORED title names the event the user holds; the message's
            // own SUMMARY is the fallback only when the stored one is empty.
            let summary = if stored.fields.summary.is_empty() {
                reply_fields.summary
            } else {
                stored.fields.summary.clone()
            };
            return Ok(InboundReplyOutcome::Refused {
                uid_hash: target,
                summary,
                reason,
            });
        }
        self.put_reply_merge(
            actor_id,
            &calendar_id,
            &target,
            msek,
            &stored,
            &prior_roster,
            merged,
            timestamp,
        )
        .await
    }

    /// The sealed rail's `REPLY`: [`Self::apply_reply_under`] with the
    /// nest-attested `origin` as the authority, each on-roster attendee
    /// resolved through `resolver` ([`Self::authorize_reply`]).
    async fn apply_authorized_reply<P: PrincipalResolver>(
        &self,
        actor_id: &[u8; 32],
        msek: &[u8; 32],
        reply_ics: &str,
        timestamp: i64,
        origin: &InboundOrigin,
        resolver: &P,
    ) -> Result<InboundReplyOutcome, InboundScheduleError<R::Error>> {
        self.apply_reply_under(
            actor_id,
            msek,
            reply_ics,
            timestamp,
            ReplyAuthority::Rail { origin, resolver },
        )
        .await
    }

    /// The **mail rail's** `REPLY` — the one entry point for an iMIP `REPLY`
    /// that arrived as email into the organizer's INBOX. Extracts the
    /// `text/calendar` part from the raw inbound RFC 5322 message and, when it
    /// is an iTIP `REPLY`, runs the same gated chain as the sealed rail
    /// ([`Self::apply_reply_under`]) under the **door-authenticated sender**:
    /// the sealed copy's `X-Fauna-Authenticated-Sender` stamp, read off these
    /// raw bytes ([`fauna_mail::sender_auth::read_authenticated_sender_stamp`]).
    /// The reply is applied only for the on-roster attendee that stamp names;
    /// no stamp refuses `SenderUnauthenticated`, another address refuses
    /// `NotTheAttendee` (caldav-server.md § Who may mutate an existing event
    /// over the inbound rail → *The mail rail*). A refusal is persisted by the
    /// caller through [`InboundReplyOutcome::refused_mail_change_record`] over
    /// the same bytes.
    ///
    /// Returns [`InboundReplyOutcome::NotCalendarReply`] when the message has
    /// no calendar part or its method is not `REPLY` (the overwhelming common
    /// case), so the inbound-mail loop can call it once per received message
    /// without pre-parsing. Keeps the "is this a schedulable REPLY?" gate and
    /// the sender check in shared Rust so every app shares them (priority #2).
    /// The caller must not hand it a copy its own spam scorer filed Junk
    /// (invitation rule 3).
    pub async fn apply_inbound_reply_from_mail(
        &self,
        actor_id: &[u8; 32],
        msek: &[u8; 32],
        raw_rfc5322: &[u8],
        timestamp: i64,
    ) -> Result<InboundReplyOutcome, InboundScheduleError<R::Error>> {
        let Some(part) = extract_text_calendar_part(raw_rfc5322) else {
            return Ok(InboundReplyOutcome::NotCalendarReply);
        };
        if part
            .method
            .as_deref()
            .is_none_or(|m| !m.eq_ignore_ascii_case("REPLY"))
        {
            return Ok(InboundReplyOutcome::NotCalendarReply);
        }
        let stamp = fauna_mail::sender_auth::read_authenticated_sender_stamp(raw_rfc5322);
        self.apply_reply_under::<NoPrincipalResolver>(
            actor_id,
            msek,
            &part.ics,
            timestamp,
            ReplyAuthority::Mail {
                authenticated_sender: stamp.as_deref(),
            },
        )
        .await
    }

    /// Apply an inbound scheduling iMIP that arrived over the **mailbox-less
    /// WS-RPC rail** (a `WelcomeChannelKind::Scheduling` one-off MLS channel;
    /// caldav-server.md § Server-side auto-schedule, Half-1) — the recipient
    /// receive-loop drain's single entry point. Extract the `text/calendar` part
    /// of the raw RFC 5322 message (the *same* bytes the email rail carries, so the
    /// extract is single-sourced with [`Self::apply_inbound_reply_from_mail`] —
    /// priority #2) and route by METHOD: `REQUEST` → [`Self::apply_inbound_request`]
    /// (materialize / update), `CANCEL` → [`Self::apply_inbound_cancel`]
    /// (tombstone), `REPLY` → the gated reply merge (`apply_authorized_reply`). A
    /// message with no calendar part or an unrecognized METHOD is
    /// [`SchedulingApplyOutcome::NotScheduling`] — so the drain can call this once
    /// per channel message without pre-parsing, exactly as the mail loop calls
    /// `apply_inbound_reply_from_mail`. `timestamp` is the write's epoch-seconds
    /// CREATED/LAST-MODIFIED surrogate (used by `REQUEST` / `REPLY`; `CANCEL` is a
    /// delete and needs none).
    ///
    /// **Authorization lives here and below, once for every app**
    /// (caldav-server.md § Who may mutate an existing event over the inbound
    /// rail): `origin` is the record's nest-attested author + the channel's
    /// home nest, and a message whose origin may not make the change comes back
    /// `Refused` with the stored event untouched.
    pub async fn apply_inbound_scheduling_from_message<P: PrincipalResolver>(
        &self,
        actor_id: &[u8; 32],
        msek: &[u8; 32],
        raw_rfc5322: &[u8],
        timestamp: i64,
        origin: &InboundOrigin,
        resolver: &P,
    ) -> Result<SchedulingApplyOutcome, InboundScheduleError<R::Error>> {
        let Some(part) = extract_text_calendar_part(raw_rfc5322) else {
            return Ok(SchedulingApplyOutcome::NotScheduling);
        };
        let Some(method) = part.method.as_deref() else {
            return Ok(SchedulingApplyOutcome::NotScheduling);
        };
        if method.eq_ignore_ascii_case("REQUEST") {
            self.apply_inbound_request(actor_id, msek, &part.ics, timestamp, origin, resolver)
                .await
                .map(SchedulingApplyOutcome::Request)
        } else if method.eq_ignore_ascii_case("CANCEL") {
            self.apply_inbound_cancel(actor_id, msek, &part.ics, origin, resolver)
                .await
                .map(SchedulingApplyOutcome::Request)
        } else if method.eq_ignore_ascii_case("REPLY") {
            self.apply_authorized_reply(actor_id, msek, &part.ics, timestamp, origin, resolver)
                .await
                .map(SchedulingApplyOutcome::Reply)
        } else {
            Ok(SchedulingApplyOutcome::NotScheduling)
        }
    }

    /// Apply an inbound iTIP `REQUEST` to the recipient's calendar — the
    /// **mailbox-less** counterpart of the server-side seal-to-recipient a
    /// mail-enabled user gets from the MTA (caldav-server.md § Server-side
    /// auto-schedule, :220). A mailbox-less Fauna attendee (CalDAV-on /
    /// email-off) has no MTA path, so their client materializes the invite:
    /// the event is located by `uid_hash(REQUEST.UID)` across the actor's
    /// calendars; if found it is updated in place ([`InboundRequestOutcome::Updated`]
    /// — an organizer re-send, the recipient's Fauna sidecar preserved), else it
    /// is materialized into the recipient's lazy `Personal` calendar
    /// ([`InboundRequestOutcome::Created`] — provisioned idempotently first,
    /// mirroring the MDA's `lazyProvisionPersonal`). The organizer is
    /// authoritative for a REQUEST, so the parsed `SEQUENCE` is preserved
    /// verbatim (not bumped — unlike the reply merge). `timestamp` is the
    /// write's epoch-seconds CREATED/LAST-MODIFIED surrogate.
    ///
    /// **Authorization.** A hit on an existing event is a *mutation* and must
    /// come from the principal the event is bound to
    /// ([`Self::authorize_organizer_mutation`]). A miss is the designed
    /// stranger reach: the event is created and **bound** to `origin` in its
    /// sealed sidecar — unless the `ORGANIZER` definitively resolves to someone
    /// else, which is a spoof and is refused.
    pub async fn apply_inbound_request<P: PrincipalResolver>(
        &self,
        actor_id: &[u8; 32],
        msek: &[u8; 32],
        request_ics: &str,
        timestamp: i64,
        origin: &InboundOrigin,
        resolver: &P,
    ) -> Result<InboundRequestOutcome, InboundScheduleError<R::Error>> {
        let fields =
            parse_ical(request_ics).map_err(|e| InboundScheduleError::Parse(e.to_string()))?;
        if fields.uid.trim().is_empty() {
            return Ok(InboundRequestOutcome::NoUid);
        }
        let target = uid_hash(&fields.uid);
        let attendees = parse_ical_attendees(request_ics);
        let organizer = parse_ical_organizer(request_ics).unwrap_or_default();

        // A REQUEST carries no collection hint: an existing row anywhere is an
        // organizer re-send (update in place), otherwise materialize into the
        // recipient's lazy Personal calendar.
        let (calendar_id, created, binding) =
            match self.find_stored_event(actor_id, msek, &target).await? {
                Some((existing, stored)) => {
                    let rebind = match Self::authorize_organizer_mutation(
                        &stored,
                        request_ics,
                        origin,
                        resolver,
                    )
                    .await
                    {
                        Ok(rebind) => rebind,
                        Err(reason) => {
                            return Ok(InboundRequestOutcome::Refused {
                                uid_hash: target,
                                // The STORED title, never the rewriting
                                // message's: the user is being told about the
                                // event they hold, and the sender chooses its
                                // own `SUMMARY` as freely as its `ORGANIZER`.
                                summary: stored.fields.summary.clone(),
                                reason,
                            });
                        }
                    };
                    // Admitted through a verified succession: re-bind to the
                    // successor, every other sidecar field carried over, so the
                    // line is walked once and the retired id stops matching.
                    let rebound = rebind.map(|successor| FaunaEventExt {
                        organizer_actor_id: Some(successor),
                        ..stored.fauna_ext.clone().unwrap_or_default()
                    });
                    (existing, false, rebound)
                }
                None => {
                    // Bind on first use — but never to an origin the ORGANIZER
                    // line definitively contradicts.
                    let binding = match origin.author.as_deref() {
                        Some(author) => {
                            if let Some(named) = resolver.resolve_principal(&organizer).await
                                && !named.matches(author, &origin.home_nest_url)
                            {
                                return Ok(InboundRequestOutcome::Refused {
                                    uid_hash: target,
                                    // Nothing is stored under this UID — this
                                    // branch is the miss — so the title can
                                    // only be the one the message brought.
                                    summary: fields.summary.clone(),
                                    reason: RefusalReason::SpoofedOrganizer,
                                });
                            }
                            Some(FaunaEventExt {
                                organizer_actor_id: Some(author.to_ascii_lowercase()),
                                organizer_home_nest_url: Some(origin.home_nest_url.clone()),
                                ..Default::default()
                            })
                        }
                        // No attested author: the invite still lands (creation
                        // is open), unbound — later mutation is resolve-or-refuse.
                        None => None,
                    };
                    let personal = personal_calendar_id();
                    self.ensure_personal_calendar(actor_id, msek, &personal)
                        .await?;
                    (personal, true, binding)
                }
            };

        // On an UPDATE the sidecar is `None` — write none, so the nest preserves
        // the recipient's prior Fauna refinement AND the organizer binding (the
        // MUA-edit-keeps-refinement invariant) — unless the update was admitted
        // through a succession, which re-binds. A fresh materialize writes the
        // binding. `fields.sequence` is preserved verbatim (not bumped — unlike
        // the reply merge at `apply_reply_under`): the sender has just been
        // shown to be the organizer, who is authoritative for a REQUEST.
        self.seal_and_put_event(
            actor_id,
            &calendar_id,
            &target,
            msek,
            &fields,
            &attendees,
            &organizer,
            binding.as_ref(),
            timestamp,
            None,
        )
        .await
        .map_err(InboundScheduleError::Put)?;

        Ok(if created {
            InboundRequestOutcome::Created {
                uid_hash: target,
                calendar_id,
            }
        } else {
            InboundRequestOutcome::Updated {
                uid_hash: target,
                calendar_id,
            }
        })
    }

    /// Locate the actor's stored event with `target` uid_hash, and the calendar
    /// holding it, if any. An inbound REQUEST/CANCEL carries no collection hint
    /// (like a REPLY), so search every calendar by uid_hash. Returns the event
    /// itself because the authorization check reads it (its `ORGANIZER` line
    /// and sidecar binding).
    async fn find_stored_event(
        &self,
        actor_id: &[u8; 32],
        msek: &[u8; 32],
        target: &[u8; 32],
    ) -> Result<Option<([u8; 32], FlatEvent)>, InboundScheduleError<R::Error>> {
        let keys = DavRecipientKeys::derive(msek);
        let listing = self
            .list_calendars(ListCalendarsRequest {
                actor_id: actor_id.to_vec(),
            })
            .await
            .map_err(InboundScheduleError::ListCalendars)?;
        for cal in &listing.calendars {
            let Ok(calendar_id) = <[u8; 32]>::try_from(cal.calendar_id.as_slice()) else {
                continue; // a non-32-byte id can't address an event; skip defensively.
            };
            let Some(events) = self
                .events_flat_in_calendar(actor_id, &calendar_id, &keys)
                .await
                .map_err(InboundScheduleError::Read)?
            else {
                continue;
            };
            if let Some(stored) = events
                .into_iter()
                .find(|e| e.uid_hash.as_slice() == target.as_slice())
            {
                return Ok(Some((calendar_id, stored)));
            }
        }
        Ok(None)
    }

    /// May `origin` change or cancel `stored`? The whole inbound-mutation rule
    /// (caldav-server.md § Who may mutate an existing event over the inbound
    /// rail), in order:
    ///
    /// 1. no attested author → refuse (nothing identifies the sender);
    /// 2. the message may not change the `ORGANIZER` line;
    /// 3. a **bound** event compares its sealed `(author, home nest)` binding
    ///    against the origin — locally, no lookup;
    /// 4. an **unbound** event (local, MUA-PUT, imported) resolves
    ///    its stored `ORGANIZER`: no answer refuses, a different principal
    ///    refuses.
    ///
    /// Nothing inside the message's `.ics` is ever evidence of authority — every
    /// co-attendee holds the organizer's address verbatim — and the UID is never
    /// treated as a secret.
    ///
    /// `Ok(Some(successor))` is the one widening: the event is BOUND, the plain
    /// comparison missed, the message's home nest is still the bound one, and
    /// the resolver verified that the bound identity succeeded to the author
    /// (§ *A succeeded organizer*). The caller re-binds an update to it.
    async fn authorize_organizer_mutation<P: PrincipalResolver>(
        stored: &FlatEvent,
        message_ics: &str,
        origin: &InboundOrigin,
        resolver: &P,
    ) -> Result<Option<String>, RefusalReason> {
        let Some(author) = origin.author.as_deref() else {
            return Err(RefusalReason::NoAttestedAuthor);
        };
        let stored_organizer = parse_ical_organizer(&stored.ics).unwrap_or_default();
        let message_organizer = parse_ical_organizer(message_ics).unwrap_or_default();
        if normalize_caladdr(&stored_organizer) != normalize_caladdr(&message_organizer) {
            return Err(RefusalReason::OrganizerChanged);
        }
        let binding = stored.fauna_ext.as_ref().and_then(|ext| {
            Some(SchedulingPrincipal {
                actor_id: ext.organizer_actor_id.clone()?,
                home_nest_url: ext.organizer_home_nest_url.clone()?,
            })
        });
        let Some(bound) = binding else {
            // Unbound: resolve-or-refuse. Discovery already answers with whoever
            // the address names TODAY, so there is no line to walk here.
            let named = resolver
                .resolve_principal(&stored_organizer)
                .await
                .ok_or(RefusalReason::OrganizerUnresolvable)?;
            return if named.matches(author, &origin.home_nest_url) {
                Ok(None)
            } else {
                Err(RefusalReason::NotTheOrganizer)
            };
        };
        if bound.matches(author, &origin.home_nest_url) {
            return Ok(None);
        }
        // The local comparison missed. Only the actor-id half may be resolved
        // through a succession — a different nest is refused with no lookup.
        if !nest_urls_eq(&bound.home_nest_url, &origin.home_nest_url) {
            return Err(RefusalReason::NotTheOrganizer);
        }
        match resolver.resolve_successor(&bound).await {
            Some(successor) if successor.eq_ignore_ascii_case(author) => {
                Ok(Some(successor.to_ascii_lowercase()))
            }
            _ => Err(RefusalReason::NotTheOrganizer),
        }
    }

    /// May `origin` speak for every attendee on `stored_roster` a `REPLY`
    /// names?
    ///
    /// A `REPLY` changes the organizer's stored event, so it answers to the
    /// mutation rule rather than to "no answer applies": each on-roster
    /// `ATTENDEE` it speaks for must resolve to exactly its origin, and *no
    /// answer* — no attested author, or an address that resolves to no Fauna
    /// principal — refuses (caldav-server.md § Who may mutate an existing event
    /// over the inbound rail). The organizer's copy holds no principal per
    /// attendee to compare against (`attendee_nest_urls` records a delivery
    /// URL, not an actor), so the check is always a resolution.
    ///
    /// Only addresses the stored roster already carries are resolved — matched
    /// exactly as [`apply_reply_to_roster`] matches them — because the merge
    /// ignores every other line: resolving one would add no authority and hand
    /// the sender a free choice of the host this seat dials.
    async fn authorize_reply<P: PrincipalResolver>(
        stored_roster: &[AttendeeInfo],
        reply_ics: &str,
        origin: &InboundOrigin,
        resolver: &P,
    ) -> Result<(), RefusalReason> {
        let Some(author) = origin.author.as_deref() else {
            return Err(RefusalReason::NoAttestedAuthor);
        };
        for attendee in on_roster_reply_attendees(stored_roster, reply_ics) {
            let speaks_for = resolver
                .resolve_principal(&attendee.email)
                .await
                .ok_or(RefusalReason::AttendeeUnresolvable)?;
            if !speaks_for.matches(author, &origin.home_nest_url) {
                return Err(RefusalReason::NotTheAttendee);
            }
        }
        Ok(())
    }

    /// Idempotently ensure the actor's lazy `Personal` calendar row exists
    /// before a materialize PUT — the encrypted `put_event_ciphertext` returns
    /// `CalendarNotFound` rather than auto-creating (`bridge_caldav_handlers.rs`).
    /// Mirrors the MDA's Go `lazyProvisionPersonal`: seal the default metadata
    /// and MKCOL-provision. The reply variant is not inspected — any successful
    /// provision means the row now exists (`Created` / `AlreadyExists`, or
    /// `Conflict` when a prior re-seal's HPKE-fresh bytes differ; the existing
    /// row wins either way), and the subsequent PUT is the real existence gate.
    async fn ensure_personal_calendar(
        &self,
        actor_id: &[u8; 32],
        msek: &[u8; 32],
        personal: &[u8; 32],
    ) -> Result<(), InboundScheduleError<R::Error>> {
        let sealed = seal_calendar_metadata(&default_personal_metadata(), msek)
            .map_err(InboundScheduleError::Seal)?;
        self.provision_calendar(ProvisionCalendarRequest {
            actor_id: actor_id.to_vec(),
            calendar_id: personal.to_vec(),
            encrypted_metadata: sealed,
            update_metadata: false,
        })
        .await
        .map_err(InboundScheduleError::Provision)?;
        Ok(())
    }

    /// Apply an inbound iTIP `CANCEL` to the recipient's calendar: locate the
    /// event by `uid_hash(CANCEL.UID)` across the actor's calendars and
    /// tombstone it ([`Self::delete_event`]). Returns
    /// [`InboundRequestOutcome::Cancelled`] when removed, or
    /// [`InboundRequestOutcome::NoMatchingEvent`] for a stale/duplicate cancel.
    /// — but only for the principal the event is bound to
    /// ([`Self::authorize_organizer_mutation`]); anyone else's `CANCEL` comes
    /// back [`InboundRequestOutcome::Refused`] and the event survives.
    pub async fn apply_inbound_cancel<P: PrincipalResolver>(
        &self,
        actor_id: &[u8; 32],
        msek: &[u8; 32],
        cancel_ics: &str,
        origin: &InboundOrigin,
        resolver: &P,
    ) -> Result<InboundRequestOutcome, InboundScheduleError<R::Error>> {
        let fields =
            parse_ical(cancel_ics).map_err(|e| InboundScheduleError::Parse(e.to_string()))?;
        if fields.uid.trim().is_empty() {
            return Ok(InboundRequestOutcome::NoUid);
        }
        let target = uid_hash(&fields.uid);
        match self.find_stored_event(actor_id, msek, &target).await? {
            Some((calendar_id, stored)) => {
                if let Err(reason) =
                    Self::authorize_organizer_mutation(&stored, cancel_ics, origin, resolver).await
                {
                    return Ok(InboundRequestOutcome::Refused {
                        uid_hash: target,
                        // The stored title — the cancel's own is the sender's.
                        summary: stored.fields.summary.clone(),
                        reason,
                    });
                }
                self.delete_event(DeleteEventRequest {
                    actor_id: actor_id.to_vec(),
                    calendar_id: calendar_id.to_vec(),
                    uid_hash: target.to_vec(),
                    if_match: None,
                })
                .await
                .map_err(InboundScheduleError::Delete)?;
                Ok(InboundRequestOutcome::Cancelled {
                    uid_hash: target,
                    calendar_id,
                })
            }
            None => Ok(InboundRequestOutcome::NoMatchingEvent),
        }
    }
}

/// The iTIP `METHOD` of a raw RFC 5322 scheduling message, upper-cased —
/// `REQUEST` / `CANCEL` / `REPLY`, or empty when the message carries no
/// `text/calendar` part or no method at all.
///
/// Exists so a `SchedulingSink` can name the method on a refused-change record
/// ([`SchedulingApplyOutcome::refused_change_record`]) **without re-parsing the
/// message itself**: the extract is the same one
/// [`CalDavClient::apply_inbound_scheduling_from_message`] routes on, so the
/// two can never disagree about what a message was. Both sinks call this rather
/// than each sniffing the bytes their own way (priority #2).
#[must_use]
pub fn scheduling_method(raw_rfc5322: &[u8]) -> String {
    extract_text_calendar_part(raw_rfc5322)
        .and_then(|part| part.method)
        .map(|m| m.trim().to_ascii_uppercase())
        .unwrap_or_default()
}

/// [`PrincipalResolver`] over the anon `by_handle` discovery the attendee
/// resolver already uses — the **address** half of the inbound-mutation rule's
/// resolver, one implementation for every app: the native sink's resolver in
/// `fauna-client-conversations` and the web sink's in `fauna-wasm` both wrap
/// this one (each adding its platform's succession walk). *No answer* covers
/// every failure: a malformed address, an unreachable or non-Fauna domain, a
/// transport fault — none of them is permission.
///
/// **The recipient's own nest is recognised by identity, not by URL.** The
/// rail reports a same-nest channel's home as empty, so a principal found on
/// this client's own nest must come back with an empty home to compare equal.
/// The nest that answered for the domain is that nest exactly when its
/// `fauna.nest.info` `nest_id` — read over the same discovery connection — is
/// the one `own_nest_url` reports; comparing URL spellings instead would fail a
/// client that reaches its nest by a LAN address or a second name. Either
/// identity unknown → not folded, so the principal keeps the answering nest's
/// URL and an own-nest origin compares unequal: the fail-closed side.
pub struct DiscoveryPrincipalResolver<D> {
    /// The anon discovery surface.
    pub discovery: D,
    /// Base URL of the recipient's own nest, however this client reaches it.
    pub own_nest_url: String,
}

impl<D: AttendeeDiscovery> PrincipalResolver for DiscoveryPrincipalResolver<D> {
    async fn resolve_principal(&self, caladdr: &str) -> Option<SchedulingPrincipal> {
        let (localpart, domain) = split_caladdr(caladdr).ok()?;
        let found = self
            .discovery
            .resolve_actor(&domain, &localpart)
            .await
            .ok()
            .flatten()?;
        let home_nest_url = if self.is_own_nest(found.nest_id.as_deref()).await {
            String::new()
        } else {
            found.nest_url
        };
        Some(SchedulingPrincipal {
            actor_id: found.actor_id,
            home_nest_url,
        })
    }
}

impl<D: AttendeeDiscovery> DiscoveryPrincipalResolver<D> {
    /// Is the nest identified by `nest_id` this client's own? Only a positive
    /// identity match says yes; an unknown identity on either side says no.
    async fn is_own_nest(&self, nest_id: Option<&str>) -> bool {
        let Some(found) = nest_id.filter(|id| !id.is_empty()) else {
            return false;
        };
        match self.discovery.nest_id(&self.own_nest_url).await {
            Ok(own) => !own.is_empty() && own.eq_ignore_ascii_case(found),
            Err(_) => false,
        }
    }
}

// ── Attendee transport resolution (TRACK B Half-1 Slice 2) ───────────────────
//
// The organizer's dispatch fork (Slice 5) must decide, per attendee CAL-ADDRESS,
// whether to deliver the iMIP by **email** (the universal layer — external
// addresses and mail-enabled Fauna handles alike) or over the **WS-RPC sealed
// MLS rail** (a mailbox-less Fauna user: CalDAV enabled, email disabled). The
// decision lives in shared Rust so all seven apps route identically (priority
// #2; `caldav-server.md` § Where logic lives — "dispatch routing (email vs
// WS-RPC) … live in shared Rust … v1 invokes it client-driven").
//
// It is a policy over two anon cross-nest discovery facts: does the address
// resolve to a Fauna actor (`fauna.actor.by_handle`), and is that actor's nest
// email-enabled (`fauna.setup.status.email_enabled`)? Only a *positively
// confirmed* mailbox-less Fauna actor rides the WS-RPC rail; every other outcome
// — external domain, unreachable nest, or a mail-enabled Fauna handle — falls
// back to email, which is always a safe default (`caldav-server.md`
// § Scheduling & invitations — "email-based invites are the universal layer").

/// The transport a single attendee's iMIP must ride — the output of
/// [`resolve_attendee_transport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttendeeTransport {
    /// Deliver the iMIP as an email via the bridge MTA: an external address
    /// (Apple / Gmail / Outlook) **or** a mail-enabled Fauna handle. The
    /// universal fallback whenever the attendee is not a positively-confirmed
    /// mailbox-less Fauna user.
    MailReachable,
    /// A mailbox-less Fauna attendee (CalDAV enabled, email disabled) — deliver
    /// over the WS-RPC sealed MLS welcome rail to their nest. Carries the inputs
    /// the Slice-3 sealed-delivery rail + the [`FaunaEventExt::attendee_nest_urls`]
    /// sidecar need: the resolved actor and its nest base URL.
    MailboxlessFauna {
        /// 64-char hex of the resolved 32-byte attendee actor public key.
        actor_id: String,
        /// The attendee's nest base URL, resolved automatically via anon
        /// `by_handle` discovery — never manually entered.
        nest_url: String,
    },
}

/// A Fauna actor resolved from a CAL-ADDRESS by [`AttendeeDiscovery::resolve_actor`]:
/// the actor id (hex) plus the base URL and identity of the nest that answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredActor {
    /// 64-char hex of the resolved 32-byte actor public key.
    pub actor_id: String,
    /// The base URL of the nest serving this handle's domain.
    pub nest_url: String,
    /// 64-char hex of the answering nest's Ed25519 key, as its
    /// `fauna.nest.info` reported it on the discovery connection (hop 2 of the
    /// chain, `federation.md` § Peer-auth model); `None` when that hop gave no
    /// answer. What [`DiscoveryPrincipalResolver`] recognises the recipient's
    /// own nest by.
    pub nest_id: Option<String>,
}

/// The anon cross-nest discovery surface [`resolve_attendee_transport`] and
/// [`DiscoveryPrincipalResolver`] depend on, abstracted so the resolution
/// **policy** is unit-testable without a live nest (the production impl
/// [`AnonAttendeeDiscovery`] opens anonymous connections to peer nests; tests
/// supply a static map). Native `async fn in trait`, mirroring
/// [`fauna_protocol::RpcRequester`] — static-dispatch only (callers take
/// `D: AttendeeDiscovery`, never `dyn`), so per-impl `Send` inference holds and
/// the production future stays `tokio::spawn`-able. wasm-safe: the browser's
/// inbound-scheduling resolver runs over it too.
#[allow(async_fn_in_trait)] // see RpcRequester module docs: static-dispatch only
pub trait AttendeeDiscovery {
    /// Discovery transport error (connect / RPC). Bounded `Display` so the
    /// resolver's [`ResolveError`] can surface it.
    type Error: core::fmt::Display;

    /// Anon `fauna.actor.by_handle(localpart)` against the nest serving `domain`.
    /// `Ok(Some(actor))` when the handle resolves to a Fauna actor there;
    /// `Ok(None)` when it does not — an external / non-Fauna address, or a domain
    /// that hosts no reachable Fauna nest (both deliver by email). `Err` only on
    /// a transport fault to a *reachable* Fauna nest.
    async fn resolve_actor(
        &self,
        domain: &str,
        localpart: &str,
    ) -> Result<Option<DiscoveredActor>, Self::Error>;

    /// Anon `fauna.setup.status` against a nest base URL → its `email_enabled`
    /// flag (`fauna_protocol::discovery::SetupStatusReply::email_enabled`).
    /// `false` ⇒ a mailbox-less nest ⇒ the WS-RPC rail.
    async fn nest_email_enabled(&self, nest_url: &str) -> Result<bool, Self::Error>;

    /// Anon `fauna.nest.info` against a nest base URL → its `nest_id` (64-char
    /// hex Ed25519 key). How [`DiscoveryPrincipalResolver`] learns the identity
    /// of the recipient's own nest, to compare against
    /// [`DiscoveredActor::nest_id`].
    async fn nest_id(&self, nest_url: &str) -> Result<String, Self::Error>;
}

/// Error from [`resolve_attendee_transport`] / [`resolve_attendee_nest_urls`].
#[derive(Debug)]
pub enum ResolveError<E> {
    /// The CAL-ADDRESS was not a parseable `[mailto:]localpart@domain`.
    MalformedAddress(String),
    /// An anon discovery RPC failed against a reachable nest (an *unreachable*
    /// domain is folded into `Ok(None)` = email, not an error).
    Discovery(E),
}

impl<E: core::fmt::Display> core::fmt::Display for ResolveError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ResolveError::MalformedAddress(a) => write!(f, "malformed CAL-ADDRESS: {a}"),
            ResolveError::Discovery(e) => write!(f, "attendee discovery: {e}"),
        }
    }
}

impl<E: core::fmt::Debug + core::fmt::Display> std::error::Error for ResolveError<E> {}

/// Split a CAL-ADDRESS (`mailto:Alice@example.com` / `alice@example.com`, any case)
/// into `(localpart, domain)`, both lower-cased ([`normalize_caladdr`] — DNS and
/// Fauna handles are case-insensitive). `Err(addr)` on a missing or empty `@`
/// part.
fn split_caladdr(addr: &str) -> Result<(String, String), String> {
    let norm = normalize_caladdr(addr);
    match norm.rsplit_once('@') {
        Some((localpart, domain)) if !localpart.is_empty() && !domain.is_empty() => {
            Ok((localpart.to_string(), domain.to_string()))
        }
        _ => Err(addr.to_string()),
    }
}

/// Decide whether `addr`'s iMIP rides **email** or the **WS-RPC sealed rail**
/// (`caldav-server.md` § Server-side auto-schedule). Resolves the address to a
/// Fauna actor via anon discovery, then checks that nest's `email_enabled`: a
/// resolved actor on an email-**disabled** nest is the only
/// [`AttendeeTransport::MailboxlessFauna`] case; every other outcome — not a
/// Fauna actor, or a mail-enabled one — is [`AttendeeTransport::MailReachable`].
pub async fn resolve_attendee_transport<D: AttendeeDiscovery>(
    discovery: &D,
    addr: &str,
) -> Result<AttendeeTransport, ResolveError<D::Error>> {
    let (localpart, domain) = split_caladdr(addr).map_err(ResolveError::MalformedAddress)?;
    match discovery
        .resolve_actor(&domain, &localpart)
        .await
        .map_err(ResolveError::Discovery)?
    {
        None => Ok(AttendeeTransport::MailReachable),
        Some(DiscoveredActor {
            actor_id, nest_url, ..
        }) => {
            if discovery
                .nest_email_enabled(&nest_url)
                .await
                .map_err(ResolveError::Discovery)?
            {
                Ok(AttendeeTransport::MailReachable)
            } else {
                Ok(AttendeeTransport::MailboxlessFauna { actor_id, nest_url })
            }
        }
    }
}

/// Resolve every attendee CAL-ADDRESS and build the [`FaunaEventExt::attendee_nest_urls`]
/// sidecar entries for the **mailbox-less** ones (keyed by the normalized
/// CAL-ADDRESS → nest base URL); email-reachable attendees contribute no entry.
/// An organizer stores this map on the event so the Slice-3 rail and any later
/// re-send know where each mailbox-less invite goes without re-resolving. A
/// [`ResolveError`] for any one address aborts the batch.
pub async fn resolve_attendee_nest_urls<D: AttendeeDiscovery>(
    discovery: &D,
    attendees: &[String],
) -> Result<BTreeMap<String, String>, ResolveError<D::Error>> {
    let mut urls = BTreeMap::new();
    for addr in attendees {
        if let AttendeeTransport::MailboxlessFauna { nest_url, .. } =
            resolve_attendee_transport(discovery, addr).await?
        {
            urls.insert(normalize_caladdr(addr), nest_url);
        }
    }
    Ok(urls)
}

/// Opens an anonymous (pre-identity) WS-RPC connection to a nest base URL — the
/// one platform-divergent piece of anon discovery (caldav-server.md § Who may
/// mutate an existing event over the inbound rail → *Where it lives*). The
/// production connector is this platform's own anon client; a test may supply
/// one that reaches an in-process nest.
#[allow(async_fn_in_trait)] // static-dispatch only, like `AttendeeDiscovery`
pub trait AnonConnector {
    /// The connection: anything that speaks the `RpcRequester` seam and can say
    /// whether a failure was the nest's refusal.
    type Client: fauna_protocol::RpcRequester<Error: fauna_protocol::RpcErrorClass>;

    /// Open a fresh connection to `base_url` (`http(s)://…`).
    async fn connect(
        &self,
        base_url: &str,
    ) -> Result<Self::Client, <Self::Client as fauna_protocol::RpcRequester>::Error>;
}

/// This platform's anon connection: the native `AnonymousNestClient` (reqwest +
/// tokio-tungstenite over TLS), or the browser's `AnonymousWsRpcClient` (a gloo
/// `WebSocket`) — the same pair the conversations cross-nest resolve dials
/// (`ConversationsClient::actor_by_handle_remote`, both arms).
#[derive(Debug, Default, Clone, Copy)]
pub struct PlatformAnonConnector;

#[cfg(not(target_arch = "wasm32"))]
impl AnonConnector for PlatformAnonConnector {
    type Client = fauna_anon_client::AnonymousNestClient;

    async fn connect(
        &self,
        base_url: &str,
    ) -> Result<Self::Client, fauna_anon_client::AnonClientError> {
        fauna_anon_client::AnonymousNestClient::connect(base_url).await
    }
}

#[cfg(target_arch = "wasm32")]
impl AnonConnector for PlatformAnonConnector {
    type Client = fauna_rpc_wasm::AnonymousWsRpcClient;

    async fn connect(&self, base_url: &str) -> Result<Self::Client, fauna_rpc_wasm::WsRpcError> {
        // Returns at once; the browser completes the handshake asynchronously,
        // so an unreachable nest surfaces as the first request's error.
        fauna_rpc_wasm::AnonymousWsRpcClient::connect(base_url)
    }
}

/// `by_handle` codes that mean "a nest answered: no such Fauna actor here" —
/// the `Ok(None)` of [`AttendeeDiscovery::resolve_actor`], never an error
/// (mirrors `ConversationsClient::actor_by_handle`'s not_found→None mapping).
const NOT_A_FAUNA_ACTOR_CODES: &[&str] = &["fauna.actor.not_found", "fauna.handle.invalid"];

/// Anonymous cross-nest discovery over any [`AnonConnector`] — the one body
/// [`AnonAttendeeDiscovery`] runs on every platform. `resolve_handle_domain`
/// names the nest to dial (`fauna_provisioning::probe`, pure and wasm-clean);
/// one fresh connection per query (the anon clients have no reconnect
/// supervisor).
#[derive(Debug, Default, Clone, Copy)]
pub struct AnonDiscovery<C>(pub C);

impl<C: AnonConnector> AttendeeDiscovery for AnonDiscovery<C> {
    type Error = <C::Client as fauna_protocol::RpcRequester>::Error;

    async fn resolve_actor(
        &self,
        domain: &str,
        localpart: &str,
    ) -> Result<Option<DiscoveredActor>, Self::Error> {
        use fauna_protocol::RpcErrorClass;
        use fauna_protocol::RpcRequester;
        use fauna_protocol::discovery::{
            ActorByHandleReply, ActorByHandleRequest, NestInfoReply, NestInfoRequest,
        };

        let target = fauna_provisioning::probe::resolve_handle_domain(domain);
        // A domain hosting no reachable Fauna nest (an external mail domain like
        // example.net) fails the anon connect — that means "not a mailbox-less
        // Fauna actor", i.e. deliver by email, NOT a hard error.
        let Ok(client) = self.0.connect(&target.base_url).await else {
            return Ok(None);
        };
        let reply = client
            .request::<_, ActorByHandleReply>(
                "fauna.actor.by_handle",
                ActorByHandleRequest {
                    handle: localpart.to_string(),
                    domain: None,
                    extra: BTreeMap::new(),
                },
            )
            .await;
        let actor_id = match reply {
            Ok(reply) => reply.actor_id,
            Err(e)
                if e.as_rpc_error()
                    .is_some_and(|rpc| NOT_A_FAUNA_ACTOR_CODES.contains(&rpc.code.as_str())) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        // Hop 2 of the chain (`federation.md` § Peer-auth model), on the same
        // connection: WHICH nest answered for the domain. No answer is `None`,
        // never a failed resolve — the actor was found.
        let nest_id = client
            .request::<_, NestInfoReply>("fauna.nest.info", NestInfoRequest::default())
            .await
            .ok()
            .map(|info| info.nest_id)
            .filter(|id| !id.is_empty());
        Ok(Some(DiscoveredActor {
            actor_id,
            nest_url: target.base_url,
            nest_id,
        }))
    }

    async fn nest_email_enabled(&self, nest_url: &str) -> Result<bool, Self::Error> {
        use fauna_protocol::RpcRequester;
        use fauna_protocol::discovery::{SetupStatusReply, SetupStatusRequest};

        let client = self.0.connect(nest_url).await?;
        let reply: SetupStatusReply = client
            .request("fauna.setup.status", SetupStatusRequest::default())
            .await?;
        Ok(reply.email_enabled)
    }

    async fn nest_id(&self, nest_url: &str) -> Result<String, Self::Error> {
        use fauna_protocol::RpcRequester;
        use fauna_protocol::discovery::{NestInfoReply, NestInfoRequest};

        let client = self.0.connect(nest_url).await?;
        let info: NestInfoReply = client
            .request("fauna.nest.info", NestInfoRequest::default())
            .await?;
        Ok(info.nest_id)
    }
}

/// The production [`AttendeeDiscovery`] — [`AnonDiscovery`] over this
/// platform's anon client, native and browser alike. The resolution *policy*
/// is unit-tested against a mock above; the shared body runs against real nest
/// handlers in `conformance_caldav_scheduling_mailbox_less.rs`, and the real
/// dial end-to-end in the tier_3 cross-nest e2e.
#[derive(Debug, Default, Clone, Copy)]
pub struct AnonAttendeeDiscovery;

impl AttendeeDiscovery for AnonAttendeeDiscovery {
    type Error = <AnonDiscovery<PlatformAnonConnector> as AttendeeDiscovery>::Error;

    async fn resolve_actor(
        &self,
        domain: &str,
        localpart: &str,
    ) -> Result<Option<DiscoveredActor>, Self::Error> {
        AnonDiscovery(PlatformAnonConnector)
            .resolve_actor(domain, localpart)
            .await
    }

    async fn nest_email_enabled(&self, nest_url: &str) -> Result<bool, Self::Error> {
        AnonDiscovery(PlatformAnonConnector)
            .nest_email_enabled(nest_url)
            .await
    }

    async fn nest_id(&self, nest_url: &str) -> Result<String, Self::Error> {
        AnonDiscovery(PlatformAnonConnector).nest_id(nest_url).await
    }
}

// ── Slice 5: the organizer dispatch fork ───────────────────────────────────
//
// `imip_request_for_invite` builds the iMIP body + the email-shaped roster;
// `resolve_attendee_transport` decides each recipient's transport. The
// **dispatch fork** ties them together: route the email-reachable subset to the
// bridge MTA (one fanned email) and each mailbox-less Fauna attendee to the
// WS-RPC sealed MLS rail (Slice 3's `deliver_scheduling_imip`). The routing lives
// here — shared, co-located with its two inputs — while the rails are injected via
// the `ImipDispatch` seam (the outbound twin of Slice 4's inbound `SchedulingSink`),
// because `fauna-client-caldav` reaches neither an `EmailClient` nor the
// conversations MLS backend (the glue crate `fauna-client-conversations` does).

/// The two outbound rails an organizer's iMIP `REQUEST`/`CANCEL` can ride,
/// injected into [`dispatch_imip_request`]. The dispatch *routing decision*
/// (email vs WS-RPC, per [`resolve_attendee_transport`]) lives in shared Rust —
/// co-located with [`imip_request_for_invite`] (the body) and the resolver — while
/// the *rails themselves* stay in the glue crate that can reach both an
/// `EmailClient` and the conversations MLS backend
/// (`fauna_client_conversations::NestImipDispatch`). The **outbound twin** of
/// Slice 4's inbound [`SchedulingSink`]: `fauna-client-caldav` owns the calendar
/// logic (build / resolve / partition / apply), `fauna-client-conversations` owns
/// the MLS + email transports (`caldav-server.md` § Where logic lives — "dispatch
/// routing (email vs WS-RPC) … live in shared Rust"; priority #2).
///
/// Native `async fn in trait`, static-dispatch only (callers take `X: ImipDispatch`,
/// never `dyn`), mirroring [`AttendeeDiscovery`] — so per-impl `Send` inference
/// holds and the production future stays `tokio::spawn`-able.
#[cfg(not(target_arch = "wasm32"))]
#[allow(async_fn_in_trait)] // see AttendeeDiscovery / RpcRequester: static-dispatch only
pub trait ImipDispatch {
    /// Rail error (transport / send). Bounded `Display` so [`dispatch_imip_request`]
    /// can fold it into the [`DispatchReport`].
    type Error: core::fmt::Display;

    /// Fan **one** iMIP email to every email-reachable recipient over the bridge
    /// MTA (`fauna.email.send`) — the universal layer. Called at most once per
    /// dispatch, with the full email-reachable subset.
    async fn send_email(
        &self,
        recipients: Vec<String>,
        raw_rfc5322: Vec<u8>,
    ) -> Result<(), Self::Error>;

    /// Deliver the iMIP to **one** mailbox-less Fauna attendee over the WS-RPC
    /// sealed MLS welcome rail (Slice 3's `deliver_scheduling_imip`). `peer_domain`
    /// is `Some(domain)` for an attendee on a **foreign** nest, `None` same-nest —
    /// derived from the attendee's `handle@domain` (NOT the resolved nest URL),
    /// the routing `deliver_scheduling_imip` expects. `raw_rfc5322` is the *same*
    /// bytes the email rail carries (priority #2: one iMIP, two transports).
    async fn deliver_mailboxless(
        &self,
        actor_id_hex: &str,
        peer_domain: Option<String>,
        raw_rfc5322: Vec<u8>,
    ) -> Result<(), Self::Error>;
}

/// What [`dispatch_imip_request`] did — a best-effort tally the call site can
/// surface (the roster is already persisted, so a rail failure is collected here,
/// not raised). `errors` is non-empty iff a send/deliver failed; the function's own
/// `Err` is reserved for a resolve fault (malformed CAL-ADDRESS / discovery
/// transport error), which aborts the batch before any send.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DispatchReport {
    /// `true` once the single email fan-out succeeded (false if there were no
    /// email-reachable recipients, or the send errored — see `errors`).
    pub email_sent: bool,
    /// The number of email-reachable recipients the one email was addressed to.
    pub email_recipients: usize,
    /// The number of mailbox-less Fauna attendees the WS-RPC rail delivered to.
    pub mailboxless_delivered: usize,
    /// Per-rail failures (`"email send: …"` / `"mailbox-less <addr>: …"`),
    /// best-effort: collected, not raised.
    pub errors: Vec<String>,
}

/// The organizer **dispatch fork** (Slice 5, `caldav-server.md` § Server-side
/// auto-schedule + § Where logic lives): build the iMIP `REQUEST` for `roster`,
/// then route each recipient per [`resolve_attendee_transport`] — every
/// email-reachable attendee into **one** fanned email ([`ImipDispatch::send_email`]),
/// every mailbox-less Fauna attendee over the WS-RPC sealed MLS rail
/// ([`ImipDispatch::deliver_mailboxless`], one delivery each). The *same*
/// `raw_rfc5322` rides both transports (priority #2).
///
/// Returns the [`DispatchReport`] tally — best-effort, since the roster is already
/// persisted. `Err` only on a resolve fault (a malformed CAL-ADDRESS or a discovery
/// transport error against a reachable nest), which aborts before any send.
/// `imip_request_for_invite` returning `None` (no email-shaped attendee to notify)
/// yields a default (empty) report.
///
/// Shared so linux's `invite_to_event`, the native `FfiCaldavClient`, and the web
/// seam fork identically (priority #2) — no per-app partition logic; the
/// server-side gateway lifts the same routing into the MDA (§ Where logic lives).
#[cfg(not(target_arch = "wasm32"))]
pub async fn dispatch_imip_request<D, X>(
    discovery: &D,
    dispatch: &X,
    fields: &EventFields,
    roster: &[AttendeeInfo],
    organizer: &str,
    now_secs: i64,
) -> Result<DispatchReport, ResolveError<D::Error>>
where
    D: AttendeeDiscovery,
    X: ImipDispatch,
{
    let mut report = DispatchReport::default();
    // The iMIP body (the same bytes both rails carry) + the email-shaped roster
    // minus the organizer; `None` ⇒ nobody to notify.
    let Some(message) = imip_request_for_invite(fields, roster, organizer, now_secs) else {
        return Ok(report);
    };
    // The organizer's own domain — a mailbox-less attendee on it is same-nest
    // (`peer_domain = None`); any other domain is a foreign nest to relay to.
    let organizer_domain = split_caladdr(organizer).ok().map(|(_, domain)| domain);

    let mut email_recipients = Vec::new();
    for addr in &message.recipients {
        match resolve_attendee_transport(discovery, addr).await? {
            AttendeeTransport::MailReachable => email_recipients.push(addr.clone()),
            AttendeeTransport::MailboxlessFauna { actor_id, .. } => {
                // peer_domain from the attendee `handle@domain`, NOT the nest URL
                // (the routing `deliver_scheduling_imip` expects); `None` same-nest.
                let peer_domain = split_caladdr(addr).ok().and_then(|(_, domain)| {
                    if Some(&domain) == organizer_domain.as_ref() {
                        None
                    } else {
                        Some(domain)
                    }
                });
                match dispatch
                    .deliver_mailboxless(&actor_id, peer_domain, message.raw_rfc5322.clone())
                    .await
                {
                    Ok(()) => report.mailboxless_delivered += 1,
                    Err(e) => report.errors.push(format!("mailbox-less {addr}: {e}")),
                }
            }
        }
    }

    if !email_recipients.is_empty() {
        report.email_recipients = email_recipients.len();
        match dispatch
            .send_email(email_recipients, message.raw_rfc5322.clone())
            .await
        {
            Ok(()) => report.email_sent = true,
            Err(e) => report.errors.push(format!("email send: {e}")),
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{RecordingRequester, block_on};

    const MSEK: [u8; 32] = [7u8; 32];

    // ── resolve_calendar_selection ─────────────────────────────────────────

    /// No selection is the no-selection union (events.md § Layout & flow).
    #[test]
    fn no_selection_resolves_to_the_union() {
        assert_eq!(resolve_calendar_selection(None, ["aa", "bb"]), None);
    }

    /// An explicit, still-live selection narrows to exactly that calendar.
    #[test]
    fn a_live_selection_narrows_to_that_calendar() {
        assert_eq!(
            resolve_calendar_selection(Some("bb"), ["aa", "bb"]),
            Some("bb")
        );
    }

    /// A selection whose calendar has vanished (deleted here or by a CalDAV
    /// MUA) falls back to the union rather than scoping the page to nothing.
    #[test]
    fn a_stale_selection_falls_back_to_the_union() {
        assert_eq!(resolve_calendar_selection(Some("gone"), ["aa", "bb"]), None);
    }

    /// The same fallback when no calendar exists at all — an empty list can
    /// never make a selection live.
    #[test]
    fn a_selection_against_no_calendars_falls_back_to_the_union() {
        let none: [&str; 0] = [];
        assert_eq!(resolve_calendar_selection(Some("aa"), none), None);
    }

    /// Owned `String` ids resolve the same as borrowed ones (the shape every
    /// app actually holds — a `Vec<CalendarRow>`'s `id` field).
    // The `Vec` IS the shape under test (see the doc comment above); clippy's
    // "an array would do" is true of this assertion and false of its point.
    #[allow(clippy::useless_vec)]
    #[test]
    fn owned_string_ids_resolve_the_same() {
        let owned = vec!["aa".to_string(), "bb".to_string()];
        assert_eq!(
            resolve_calendar_selection(Some("aa"), owned.iter()),
            Some("aa")
        );
    }

    // ── calendar_is_displayed ──────────────────────────────────────────────

    const NO_VISIBLE: [&str; 0] = [];

    /// A live selection narrows the page to exactly that calendar.
    #[test]
    fn a_selection_displays_only_that_calendar() {
        assert!(calendar_is_displayed(
            Some("aa"),
            ["aa", "bb"],
            NO_VISIBLE,
            "aa"
        ));
        assert!(!calendar_is_displayed(
            Some("aa"),
            ["aa", "bb"],
            ["bb"],
            "bb"
        ));
    }

    /// Visibility composes onto the no-selection arm ONLY (events.md § Where
    /// logic lives): a selected calendar displays even while its visibility
    /// checkbox is unchecked.
    #[test]
    fn a_selection_overrides_an_unchecked_visibility_box() {
        assert!(calendar_is_displayed(
            Some("aa"),
            ["aa", "bb"],
            ["bb"],
            "aa"
        ));
    }

    /// The staleness rule is applied internally: a vanished selection falls
    /// through to the union arm, where the visibility filter takes over.
    #[test]
    fn a_stale_selection_falls_through_to_the_filtered_union() {
        assert!(calendar_is_displayed(
            Some("gone"),
            ["aa", "bb"],
            NO_VISIBLE,
            "aa"
        ));
        assert!(calendar_is_displayed(
            Some("gone"),
            ["aa", "bb"],
            ["bb"],
            "bb"
        ));
        assert!(!calendar_is_displayed(
            Some("gone"),
            ["aa", "bb"],
            ["bb"],
            "aa"
        ));
    }

    /// An empty visible-set means "no filter" — the full union, the
    /// pre-first-load state — never "hide everything". The empty-set case is
    /// where the per-app copies had drifted.
    #[test]
    fn an_empty_visible_set_is_the_full_union() {
        assert!(calendar_is_displayed(None, ["aa", "bb"], NO_VISIBLE, "aa"));
        assert!(calendar_is_displayed(None, ["aa", "bb"], NO_VISIBLE, "bb"));
    }

    /// A non-empty visible-set filters the union to exactly its members.
    #[test]
    fn a_non_empty_visible_set_filters_the_union() {
        assert!(calendar_is_displayed(None, ["aa", "bb"], ["aa"], "aa"));
        assert!(!calendar_is_displayed(None, ["aa", "bb"], ["aa"], "bb"));
    }

    /// A cached event on a just-deleted calendar follows the visible-set
    /// verbatim on the union arm (the proven linux behavior, pinned so a
    /// stricter existing-ids rule can't sneak in).
    #[test]
    fn a_deleted_calendars_event_follows_the_visible_set_verbatim() {
        assert!(calendar_is_displayed(None, ["aa"], NO_VISIBLE, "zz"));
        assert!(calendar_is_displayed(None, ["aa"], ["zz"], "zz"));
        assert!(!calendar_is_displayed(None, ["aa"], ["aa"], "zz"));
    }

    // ── personal_calendar_id ───────────────────────────────────────────────
    //
    // These re-derive blake3 independently of `fauna_protocol::dav_identity`, so
    // they prove this crate's RE-EXPORTS still resolve to the documented
    // derivation. The Rust↔Go half of the contract is pinned separately by
    // `libs/fauna-protocol/tests/dav_identity_cross_language.rs`.

    #[test]
    fn personal_calendar_id_is_blake3_of_personal() {
        let id = personal_calendar_id();
        assert_eq!(id, *blake3::hash(b"personal").as_bytes());
        assert_eq!(id.len(), 32);
    }

    #[test]
    fn personal_calendar_id_is_deterministic() {
        assert_eq!(personal_calendar_id(), personal_calendar_id());
    }

    #[test]
    fn uid_hash_is_blake3_of_uid() {
        // Hex of this is the `.ics` path component; deterministic + 32 bytes.
        let uid = "evt-123@fauna.test";
        assert_eq!(uid_hash(uid), *blake3::hash(uid.as_bytes()).as_bytes());
        assert_eq!(uid_hash(uid).len(), 32);
        assert_eq!(uid_hash(uid), uid_hash(uid));
        assert_ne!(uid_hash("a@x"), uid_hash("b@x"));
    }

    // ── seal / unseal round-trip ───────────────────────────────────────────

    #[test]
    fn seal_then_unseal_recovers_plaintext() {
        let body = b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let sealed = seal_event_body(body, &MSEK).expect("seal");
        assert!(!sealed.is_empty(), "sealed body is non-empty");
        assert_ne!(
            sealed.as_slice(),
            body,
            "stored bytes are ciphertext, not plaintext"
        );
        let opened = unseal_event_body(&sealed, &MSEK).expect("unseal");
        assert_eq!(opened, body);
    }

    #[test]
    fn xwing_seal_then_unseal_recovers_plaintext() {
        // S3d leg D2c: the X-Wing body seal round-trips through the suite-
        // dispatching unseal_event_body (the reader opens either suite). The
        // X-Wing envelope carries the 1120-byte ct as `enc` vs classical's
        // 32-byte ephemeral pubkey, so it is ~1088 B larger — a deterministic
        // signal that the hybrid suite was actually selected.
        let body = b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:pq\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let classical = seal_event_body(body, &MSEK).expect("classical seal");
        let hybrid = seal_event_body_xwing(body, &MSEK).expect("xwing seal");
        assert!(
            hybrid.len() > classical.len() + 1000,
            "X-Wing envelope (len {}) must be ~1088 B larger than classical (len {})",
            hybrid.len(),
            classical.len()
        );
        let opened = unseal_event_body(&hybrid, &MSEK).expect("unseal hybrid");
        assert_eq!(opened, body);
    }

    #[test]
    fn xwing_seal_with_wrong_msek_fails() {
        let hybrid = seal_event_body_xwing(b"secret-vevent", &MSEK).expect("xwing seal");
        let wrong = [9u8; 32];
        assert!(matches!(
            unseal_event_body(&hybrid, &wrong),
            Err(SealError::Unseal(_))
        ));
    }

    #[test]
    fn xwing_fauna_ext_seal_round_trips() {
        // The Fauna-ext sidecar X-Wing seal round-trips through the suite-
        // dispatching unseal_fauna_ext (leg D2c — the sidecar rides the event).
        let ext = FaunaEventExt {
            interested_attendees: vec!["mailto:a@example.com".into()],
            ..Default::default()
        };
        let sealed = seal_fauna_ext_xwing(&ext, &MSEK).expect("xwing seal ext");
        let opened =
            unseal_fauna_ext(&sealed, &DavRecipientKeys::derive(&MSEK)).expect("unseal ext");
        assert_eq!(opened, ext);
    }

    #[test]
    fn seal_empty_plaintext_yields_nonempty_envelope() {
        // The nest handler requires `encrypted_index_hint` to be non-empty; the
        // HPKE envelope around empty plaintext is itself non-empty.
        let sealed = seal_event_body(b"", &MSEK).expect("seal empty");
        assert!(!sealed.is_empty());
        assert_eq!(unseal_event_body(&sealed, &MSEK).expect("unseal"), b"");
    }

    #[test]
    fn unseal_with_wrong_msek_fails() {
        let sealed = seal_event_body(b"secret-vevent", &MSEK).expect("seal");
        let wrong = [9u8; 32];
        assert!(matches!(
            unseal_event_body(&sealed, &wrong),
            Err(SealError::Unseal(_))
        ));
    }

    #[test]
    fn unseal_rejects_garbage_bytes() {
        assert!(matches!(
            unseal_event_body(b"not-a-valid-envelope", &MSEK),
            Err(SealError::Unseal(_))
        ));
    }

    #[test]
    fn calendar_metadata_seal_roundtrip() {
        let meta = CalendarMetadata {
            displayname: "Work".into(),
            color: "#3273dc".into(),
            description: "team cal".into(),
            ..Default::default()
        };
        let sealed = seal_calendar_metadata(&meta, &MSEK).expect("seal");
        assert!(!sealed.is_empty());
        let opened =
            unseal_calendar_metadata(&sealed, &DavRecipientKeys::derive(&MSEK)).expect("unseal");
        assert_eq!(opened, meta);
    }

    #[test]
    fn calendar_metadata_description_omitted_when_empty() {
        // `description` is `skip_serializing_if` empty (mirrors the Go
        // `cbor:"description,omitempty"`), so it round-trips as the default.
        let meta = CalendarMetadata {
            displayname: "Personal".into(),
            color: "#3273dc".into(),
            description: String::new(),
            ..Default::default()
        };
        let sealed = seal_calendar_metadata(&meta, &MSEK).expect("seal");
        let opened =
            unseal_calendar_metadata(&sealed, &DavRecipientKeys::derive(&MSEK)).expect("unseal");
        assert_eq!(opened, meta);
        assert!(opened.description.is_empty());
    }

    #[test]
    fn calendar_metadata_empty_blob_is_default() {
        // Mirrors Go `UnsealCollectionMetadata` empty-ciphertext path.
        assert_eq!(
            unseal_calendar_metadata(&[], &DavRecipientKeys::derive(&MSEK)).expect("empty"),
            CalendarMetadata::default()
        );
    }

    // ── projection re-exports ──────────────────────────────────────────────

    #[test]
    fn projection_helpers_round_trip_interested_to_tentative() {
        // `interested` is Fauna-native → projects to `TENTATIVE` on the wire.
        assert_eq!(partstat_from_fauna("interested"), "TENTATIVE");
        assert_eq!(partstat_from_fauna("going"), "ACCEPTED");
        // The symmetric inverse is lossy by design (TENTATIVE → interested); the
        // sidecar is authoritative for the `interested` refinement.
        assert_eq!(fauna_status_from_partstat("ACCEPTED"), "going");
    }

    #[test]
    fn asymmetric_rsvp_projection_sidecar_is_authoritative() {
        // caldav-server.md § RSVP semantics. The verbatim fallback maps a bare
        // TENTATIVE to "tentative" (NOT "interested") — Interested comes only
        // from the sidecar.
        assert_eq!(rsvp_status_verbatim("TENTATIVE"), "tentative");
        assert_eq!(rsvp_status_verbatim("ACCEPTED"), "going");
        assert_eq!(rsvp_status_verbatim("DECLINED"), "declined");
        assert_eq!(rsvp_status_verbatim("NEEDS-ACTION"), "invited");

        let ext = FaunaEventExt {
            interested_attendees: vec!["mailto:Alice@example.com".into()],
            ..Default::default()
        };
        // Alice is marked interested → "interested" despite the wire TENTATIVE,
        // and the match is case-insensitive + mailto-tolerant.
        assert_eq!(
            project_attendee_rsvp("TENTATIVE", "alice@example.com", Some(&ext)),
            "interested"
        );
        // Bob, same wire TENTATIVE but NOT in the sidecar → verbatim "tentative".
        assert_eq!(
            project_attendee_rsvp("TENTATIVE", "bob@example.com", Some(&ext)),
            "tentative"
        );
        // No sidecar at all → verbatim.
        assert_eq!(
            project_attendee_rsvp("TENTATIVE", "alice@example.com", None),
            "tentative"
        );
        // The sidecar never overrides a non-TENTATIVE wire value to interested
        // — it only refines; here Alice ACCEPTED stays "going" (interested
        // marking is meaningful only against a TENTATIVE wire value, but the
        // projection rule renders interested whenever the sidecar says so,
        // matching the goal doc's "iff the sidecar marks that attendee").
        assert_eq!(
            project_attendee_rsvp("ACCEPTED", "carol@example.com", Some(&ext)),
            "going"
        );
    }

    // ── WASM-safe flat decode (the web read path) ──────────────────────────

    #[test]
    fn decode_event_entry_flat_round_trips_fields_and_projected_rsvp() {
        // The WASM-safe web read path (caldav-server.md § iCalendar parsing
        // rules): seal a VEVENT + sidecar into an EventEntry, decode flat via the
        // WASM-safe `fauna_core::ical::parse_ical` (NOT the native
        // `fauna_mail::parse_icalendar`), and assert the flat fields + the
        // asymmetric (sidecar-authoritative) RSVP projection — the same render
        // the native `decode_event_entry` produces, without the non-WASM parser.
        let fields = EventFields {
            summary: "Standup".into(),
            dtstart: "2026-06-10T09:00:00Z".into(),
            dtend: "2026-06-10T09:15:00Z".into(),
            location: "Room 1".into(),
            description: "daily".into(),
            uid: "evt-1@fauna.test".into(),
            status: "confirmed".into(),
            ..Default::default()
        };
        let attendees = vec![
            AttendeeInfo {
                name: "Alice".into(),
                email: "alice@example.com".into(),
                partstat: "TENTATIVE".into(),
                fauna_status: "interested".into(),
            },
            AttendeeInfo {
                name: "Bob".into(),
                email: "bob@example.com".into(),
                partstat: "ACCEPTED".into(),
                fauna_status: "going".into(),
            },
        ];
        let ics = generate_ical(&fields, &attendees, "organizer@example.com");
        // Sidecar marks Alice Interested → the asymmetric projection overrides
        // her bare wire TENTATIVE to "interested"; Bob (not in sidecar) stays.
        let ext = FaunaEventExt {
            interested_attendees: vec!["alice@example.com".into()],
            ..Default::default()
        };
        let entry = EventEntry {
            event_id: vec![1u8; 32],
            uid_hash: uid_hash(&fields.uid).to_vec(),
            encrypted_body: seal_event_body(ics.as_bytes(), &MSEK).expect("seal body"),
            encrypted_index_hint: seal_event_body(b"", &MSEK).expect("seal hint"),
            etag: "etag-1".into(),
            modseq: 42,
            internal_date: 1_700_000_000,
            encrypted_fauna_ext: Some(seal_fauna_ext(&ext, &MSEK).expect("seal ext")),
            ..Default::default()
        };

        let flat =
            decode_event_entry_flat(&entry, &DavRecipientKeys::derive(&MSEK)).expect("decode flat");
        assert_eq!(flat.event_id, vec![1u8; 32]);
        assert_eq!(flat.uid_hash, uid_hash(&fields.uid).to_vec());
        assert_eq!(flat.etag, "etag-1");
        assert_eq!(flat.modseq, 42);
        assert_eq!(flat.internal_date, 1_700_000_000);
        assert_eq!(flat.fields.summary, "Standup");
        assert_eq!(flat.fields.location, "Room 1");
        assert_eq!(flat.fields.uid, "evt-1@fauna.test");
        let alice = flat
            .attendees
            .iter()
            .find(|a| a.email == "alice@example.com")
            .expect("alice rostered");
        assert_eq!(alice.rsvp, "interested", "sidecar overrides bare TENTATIVE");
        let bob = flat
            .attendees
            .iter()
            .find(|a| a.email == "bob@example.com")
            .expect("bob rostered");
        assert_eq!(bob.rsvp, "going");
        assert!(flat.fauna_ext.is_some());
        // The unsealed `.ics` is preserved verbatim for a re-PUT mutate-rewrite.
        assert!(flat.ics.contains("BEGIN:VEVENT"));
    }

    #[test]
    fn decode_event_entry_flat_without_sidecar_uses_verbatim_partstat() {
        let fields = EventFields {
            summary: "Solo".into(),
            dtstart: "2026-06-10T09:00:00Z".into(),
            dtend: "2026-06-10T10:00:00Z".into(),
            uid: "evt-2@fauna.test".into(),
            status: "confirmed".into(),
            ..Default::default()
        };
        let attendees = vec![AttendeeInfo {
            name: "Carol".into(),
            email: "carol@example.com".into(),
            partstat: "TENTATIVE".into(),
            fauna_status: "interested".into(),
        }];
        let ics = generate_ical(&fields, &attendees, "organizer@example.com");
        let entry = EventEntry {
            uid_hash: uid_hash(&fields.uid).to_vec(),
            encrypted_body: seal_event_body(ics.as_bytes(), &MSEK).expect("seal"),
            encrypted_index_hint: seal_event_body(b"", &MSEK).expect("seal hint"),
            encrypted_fauna_ext: None,
            ..Default::default()
        };
        let flat =
            decode_event_entry_flat(&entry, &DavRecipientKeys::derive(&MSEK)).expect("decode");
        assert!(flat.fauna_ext.is_none());
        // No sidecar → bare wire TENTATIVE renders "tentative", never "interested".
        assert_eq!(flat.attendees[0].rsvp, "tentative");
    }

    #[test]
    fn decode_event_entry_flat_wrong_msek_fails_unseal() {
        let ics = generate_ical(&EventFields::default(), &[], "o@example.com");
        let entry = EventEntry {
            encrypted_body: seal_event_body(ics.as_bytes(), &MSEK).expect("seal"),
            ..Default::default()
        };
        assert!(matches!(
            decode_event_entry_flat(&entry, &DavRecipientKeys::derive(&[9u8; 32])),
            Err(DecodeEventError::Unseal(_))
        ));
    }

    #[test]
    fn fauna_ext_seal_round_trips_and_rejects_wrong_key() {
        const MSEK: [u8; 32] = [9u8; 32];
        let mut urls = BTreeMap::new();
        urls.insert(
            "bob@example.com".to_string(),
            "https://nest.example".to_string(),
        );
        let ext = FaunaEventExt {
            interested_attendees: vec!["alice@example.com".into()],
            attendee_nest_urls: urls,
            ..Default::default()
        };
        let sealed = seal_fauna_ext(&ext, &MSEK).expect("seal");
        assert_eq!(
            unseal_fauna_ext(&sealed, &DavRecipientKeys::derive(&MSEK)).expect("unseal"),
            ext
        );
        assert!(
            unseal_fauna_ext(&sealed, &DavRecipientKeys::derive(&[0u8; 32])).is_err(),
            "wrong key rejected"
        );
        assert!(
            unseal_fauna_ext(&[], &DavRecipientKeys::derive(&MSEK)).is_err(),
            "a zero-length sidecar ciphertext must not authenticate as a default sidecar \
             ( — the DAV-body dedup briefly made this an Ok(default))"
        );
    }

    // ── wire-contract: every method sends its exact kind + a round-tripping payload ──

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.bridges.provision_calendar" => {
                fauna_protocol::encode_canonical(&ProvisionCalendarReply::Created)
            }
            "fauna.bridges.list_calendars" => {
                fauna_protocol::encode_canonical(&ListCalendarsReply { calendars: vec![] })
            }
            "fauna.bridges.query_events" => {
                fauna_protocol::encode_canonical(&QueryEventsReply::Ok {
                    events: vec![],
                    highestmodseq: 0,
                    more: false,
                })
            }
            "fauna.bridges.put_event_ciphertext" => {
                fauna_protocol::encode_canonical(&PutEventCiphertextReply::Created {
                    event_id: vec![0x11u8; 32],
                    etag: "etag-1".into(),
                    modseq: 1,
                })
            }
            "fauna.bridges.delete_event" => {
                fauna_protocol::encode_canonical(&DeleteEventReply::Deleted {
                    event_id: vec![0x22u8; 32],
                    modseq: 2,
                })
            }
            "fauna.bridges.sync_calendar_since" => {
                fauna_protocol::encode_canonical(&SyncCalendarSinceReply::Ok {
                    changed: vec![],
                    expunged: vec![],
                    new_sync_token: "0".into(),
                    more: false,
                    stale: false,
                })
            }
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    fn id32(b: u8) -> Vec<u8> {
        vec![b; 32]
    }

    #[test]
    fn provision_calendar_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = CalDavClient::new(rec.clone());
        block_on(client.provision_calendar(ProvisionCalendarRequest {
            actor_id: id32(0xAA),
            calendar_id: personal_calendar_id().to_vec(),
            encrypted_metadata: b"sealed".to_vec(),
            ..Default::default()
        }))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.bridges.provision_calendar");
        let req: ProvisionCalendarRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.calendar_id, personal_calendar_id().to_vec());
    }

    #[test]
    fn list_calendars_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = CalDavClient::new(rec.clone());
        block_on(client.list_calendars(ListCalendarsRequest {
            actor_id: id32(0xAA),
        }))
        .expect("infallible mock");
        let (kind, _) = rec.recorded();
        assert_eq!(kind, "fauna.bridges.list_calendars");
    }

    #[test]
    fn query_events_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = CalDavClient::new(rec.clone());
        block_on(client.query_events(QueryEventsRequest {
            actor_id: id32(0xAA),
            calendar_id: id32(0xBB),
            since_modseq: None,
            after_event_id: None,
            limit: 0,
        }))
        .expect("infallible mock");
        let (kind, _) = rec.recorded();
        assert_eq!(kind, "fauna.bridges.query_events");
    }

    #[test]
    fn delete_event_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = CalDavClient::new(rec.clone());
        block_on(client.delete_event(DeleteEventRequest {
            actor_id: id32(0xAA),
            calendar_id: id32(0xBB),
            uid_hash: id32(0xCC),
            if_match: None,
        }))
        .expect("infallible mock");
        let (kind, _) = rec.recorded();
        assert_eq!(kind, "fauna.bridges.delete_event");
    }

    #[test]
    fn sync_calendar_since_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = CalDavClient::new(rec.clone());
        block_on(client.sync_calendar_since(SyncCalendarSinceRequest {
            actor_id: id32(0xAA),
            calendar_id: id32(0xBB),
            sync_token: "0".into(),
            limit: 0,
            mua_id: None,
        }))
        .expect("infallible mock");
        let (kind, _) = rec.recorded();
        assert_eq!(kind, "fauna.bridges.sync_calendar_since");
    }

    #[test]
    fn seal_and_put_event_seals_the_generated_vevent_and_sends_put() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = CalDavClient::new(rec.clone());

        let event = EventFields {
            summary: "Standup".into(),
            dtstart: "2026-06-02T09:00:00Z".into(),
            dtend: "2026-06-02T09:15:00Z".into(),
            uid: "evt-1@fauna.test".into(),
            ..Default::default()
        };
        let ext = FaunaEventExt {
            interested_attendees: vec!["alice@fauna.test".into()],
            ..Default::default()
        };
        let reply = block_on(client.seal_and_put_event(
            &[0xAAu8; 32],
            &personal_calendar_id(),
            &[0xCCu8; 32],
            &MSEK,
            &event,
            &[],
            "alice@fauna.test",
            Some(&ext),
            1_700_000_000,
            None,
        ))
        .expect("seal + put succeeds");
        assert!(matches!(reply, PutEventCiphertextReply::Created { .. }));

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.bridges.put_event_ciphertext");
        let req: PutEventCiphertextRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        // ciphertext_size is consistent, and the sealed body unseals back to the
        // exact ICS the writer produced — proving the full write path.
        assert_eq!(req.ciphertext_size as usize, req.encrypted_body.len());
        let opened = unseal_event_body(&req.encrypted_body, &MSEK).expect("unseal stored body");
        // seal_and_put_event stamps the RFC-5545-mandatory DTSTAMP from the write
        // timestamp (GAP 2: a VEVENT without DTSTAMP fails go-ical's encoder on
        // the MDA serve path, so it must ride the stored body).
        let mut stamped = event.clone();
        stamped.dtstamp = epoch_secs_to_ical_utc(1_700_000_000);
        let expected_ics = generate_ical(&stamped, &[], "alice@fauna.test");
        assert!(
            expected_ics.contains("DTSTAMP:20231114T221320Z"),
            "the write path must emit DTSTAMP: {expected_ics}"
        );
        assert_eq!(String::from_utf8(opened).unwrap(), expected_ics);
        assert!(!req.encrypted_index_hint.is_empty());
        // The sidecar rode the same write, sealed client-side, and unseals back.
        let sealed_ext = req
            .encrypted_fauna_ext
            .expect("sidecar present on Fauna write");
        assert_eq!(
            unseal_fauna_ext(&sealed_ext, &DavRecipientKeys::derive(&MSEK))
                .expect("unseal sidecar"),
            ext
        );
    }

    #[test]
    fn import_ical_events_puts_every_vevent_and_fills_missing_uids() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = CalDavClient::new(rec.clone());

        let with_uid = EventFields {
            summary: "Has a UID".into(),
            dtstart: "2026-06-02T09:00:00Z".into(),
            dtend: "2026-06-02T09:15:00Z".into(),
            uid: "evt-1@fauna.test".into(),
            ..Default::default()
        };
        let uid_less = EventFields {
            summary: "No UID".into(),
            dtstart: "2026-06-03T09:00:00Z".into(),
            dtend: "2026-06-03T09:15:00Z".into(),
            ..Default::default()
        };
        let ics_text = generate_ical_multi(&[
            (with_uid, vec![], "alice@fauna.test".into()),
            (uid_less, vec![], "alice@fauna.test".into()),
        ]);

        let mut fallback_calls = Vec::new();
        let outcome = block_on(client.import_ical_events(
            &[0xAAu8; 32],
            &personal_calendar_id(),
            &MSEK,
            "alice@fauna.test",
            &ics_text,
            1_700_000_000,
            |idx| {
                fallback_calls.push(idx);
                format!("fallback-{idx}@fauna.test")
            },
        ));

        assert_eq!(
            outcome,
            IcsImportOutcome {
                imported: 2,
                skipped: 0,
                total: 2,
            }
        );
        // The fallback only fires for the UID-less VEVENT (index 1) — the one
        // with a real UID must not be overwritten.
        assert_eq!(fallback_calls, vec![1]);
        assert_eq!(
            rec.kinds()
                .iter()
                .filter(|k| **k == "fauna.bridges.put_event_ciphertext")
                .count(),
            2
        );
    }

    // ── native read path: decode_event_entry + query_events_decoded ─────────

    /// Build a wire `EventEntry` whose `encrypted_body` is `ics` sealed under
    /// `MSEK` — exactly the bytes `query_events` returns for a stored event.
    /// `ext` is the optional sealed Fauna sidecar.
    #[cfg(not(target_arch = "wasm32"))]
    fn sealed_entry_with_ext(ics: &str, ext: Option<&FaunaEventExt>) -> bridge_routing::EventEntry {
        let body = seal_event_body(ics.as_bytes(), &MSEK).expect("seal body");
        let size = body.len() as u32;
        bridge_routing::EventEntry {
            event_id: vec![1u8; 32],
            uid_hash: vec![2u8; 32],
            encrypted_body: body,
            encrypted_index_hint: seal_event_body(b"", &MSEK).expect("seal hint"),
            etag: "etag-x".into(),
            modseq: 5,
            ciphertext_size: size,
            internal_date: 1_700_000_000,
            encrypted_fauna_ext: ext.map(|e| seal_fauna_ext(e, &MSEK).expect("seal sidecar")),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn sealed_entry(ics: &str) -> bridge_routing::EventEntry {
        sealed_entry_with_ext(ics, None)
    }

    fn lunch_ics() -> String {
        generate_ical(
            &EventFields {
                summary: "Lunch".into(),
                dtstart: "2026-06-02T12:00:00Z".into(),
                dtend: "2026-06-02T13:00:00Z".into(),
                uid: "u@fauna.test".into(),
                ..Default::default()
            },
            &[],
            "a@fauna.test",
        )
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn vevent_prop<'a>(decoded: &'a DecodedEvent, name: &str) -> &'a str {
        let vevent = decoded
            .document
            .components
            .iter()
            .find(|c| c.name == "VEVENT")
            .expect("a VEVENT component");
        &vevent
            .properties
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("VEVENT has a {name} property"))
            .value
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn decode_event_entry_unseals_and_parses() {
        let ics = lunch_ics();
        let decoded = decode_event_entry(&sealed_entry(&ics), &DavRecipientKeys::derive(&MSEK))
            .expect("decode");
        // Metadata carries through verbatim.
        assert_eq!(decoded.etag, "etag-x");
        assert_eq!(decoded.modseq, 5);
        assert_eq!(decoded.uid_hash, vec![2u8; 32]);
        // The unsealed `.ics` is byte-identical to what the writer produced.
        assert_eq!(decoded.ics, ics);
        // And the parsed tree exposes the canonical VEVENT fields.
        assert_eq!(vevent_prop(&decoded, "SUMMARY"), "Lunch");
        assert_eq!(vevent_prop(&decoded, "UID"), "u@fauna.test");
        // No sidecar on this MUA-style entry.
        assert!(decoded.fauna_ext.is_none());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn decode_event_entry_surfaces_sidecar_for_projection() {
        let ext = FaunaEventExt {
            interested_attendees: vec!["alice@fauna.test".into()],
            ..Default::default()
        };
        let entry = sealed_entry_with_ext(&lunch_ics(), Some(&ext));
        let decoded = decode_event_entry(&entry, &DavRecipientKeys::derive(&MSEK)).expect("decode");
        let decoded_ext = decoded.fauna_ext.expect("sidecar decoded");
        assert_eq!(decoded_ext, ext);
        // The asymmetric projection over the decoded sidecar: Alice (TENTATIVE
        // on the wire, marked in the sidecar) renders Interested; an unmarked
        // attendee renders the verbatim wire value.
        assert_eq!(
            project_attendee_rsvp("TENTATIVE", "alice@fauna.test", Some(&decoded_ext)),
            "interested"
        );
        assert_eq!(
            project_attendee_rsvp("TENTATIVE", "dave@fauna.test", Some(&decoded_ext)),
            "tentative"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn decode_event_entry_wrong_msek_is_unseal_error() {
        let entry = sealed_entry(&lunch_ics());
        let err = decode_event_entry(&entry, &DavRecipientKeys::derive(&[0u8; 32]))
            .expect_err("wrong key must fail");
        assert!(matches!(err, DecodeEventError::Unseal(_)));
    }

    /// Minimal requester that answers `query_events` with a fixed reply (the
    /// `RecordingRequester` above always returns an empty page, so the decoded
    /// read path needs one that carries a real sealed event / `CalendarNotFound`).
    #[cfg(not(target_arch = "wasm32"))]
    struct QueryRequester {
        reply: QueryEventsReply,
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl RpcRequester for QueryRequester {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            assert_eq!(kind, "fauna.bridges.query_events");
            let bytes = fauna_protocol::encode_canonical(&self.reply).expect("encode reply");
            Ok(fauna_protocol::decode_strict(&bytes).expect("decode reply"))
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn query_req() -> QueryEventsRequest {
        QueryEventsRequest {
            actor_id: id32(0xAA),
            calendar_id: id32(0xBB),
            since_modseq: None,
            after_event_id: None,
            limit: 0,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn query_events_decoded_returns_decoded_page() {
        let ics = lunch_ics();
        let client = CalDavClient::new(QueryRequester {
            reply: QueryEventsReply::Ok {
                events: vec![sealed_entry(&ics)],
                highestmodseq: 9,
                more: false,
            },
        });
        let page =
            block_on(client.query_events_decoded(query_req(), &DavRecipientKeys::derive(&MSEK)))
                .expect("decoded page");
        match page {
            DecodedEventsPage::Ok {
                events,
                highestmodseq,
                more,
            } => {
                assert_eq!(events.len(), 1);
                assert_eq!(highestmodseq, 9);
                assert!(!more);
                assert_eq!(events[0].ics, ics);
                assert_eq!(vevent_prop(&events[0], "SUMMARY"), "Lunch");
            }
            other => panic!("expected Ok page, got {other:?}"),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn query_events_decoded_calendar_not_found_passthrough() {
        let client = CalDavClient::new(QueryRequester {
            reply: QueryEventsReply::CalendarNotFound,
        });
        let page =
            block_on(client.query_events_decoded(query_req(), &DavRecipientKeys::derive(&MSEK)))
                .expect("ok result");
        assert_eq!(page, DecodedEventsPage::CalendarNotFound);
    }

    /// Export is the inverse of import: what `export_calendar_ics` writes,
    /// `parse_ical_multi` (the import parser) reads back as the same events.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn export_calendar_ics_round_trips_through_the_import_parser() {
        let dinner = generate_ical(
            &EventFields {
                summary: "Dinner".into(),
                dtstart: "2026-06-02T19:00:00Z".into(),
                dtend: "2026-06-02T21:00:00Z".into(),
                uid: "d@fauna.test".into(),
                ..Default::default()
            },
            &[],
            "a@fauna.test",
        );
        let client = CalDavClient::new(QueryRequester {
            reply: QueryEventsReply::Ok {
                events: vec![sealed_entry(&lunch_ics()), sealed_entry(&dinner)],
                highestmodseq: 3,
                more: false,
            },
        });
        let ics = block_on(client.export_calendar_ics(
            &[0xAAu8; 32],
            &[0xBBu8; 32],
            &DavRecipientKeys::derive(&MSEK),
        ))
        .expect("export");
        let reread: Vec<EventFields> = parse_ical_multi(&ics)
            .into_iter()
            .map(|r| r.expect("exported VEVENT parses"))
            .collect();
        let summaries: Vec<&str> = reread.iter().map(|e| e.summary.as_str()).collect();
        // The row that will not parse is dropped, never the whole file.
        assert_eq!(summaries, vec!["Lunch", "Dinner"]);
        assert_eq!(reread[1].uid, "d@fauna.test");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn export_calendar_ics_missing_calendar_is_an_empty_vcalendar() {
        let client = CalDavClient::new(QueryRequester {
            reply: QueryEventsReply::CalendarNotFound,
        });
        let ics = block_on(client.export_calendar_ics(
            &[0xAAu8; 32],
            &[0xBBu8; 32],
            &DavRecipientKeys::derive(&MSEK),
        ))
        .expect("export");
        assert!(ics.contains("BEGIN:VCALENDAR"), "{ics}");
        assert!(parse_ical_multi(&ics).is_empty());
    }

    // ── a newer nest's outcome (the unknown arm, transport.md § Rule 3) ──────

    /// Answers every call with the same bytes: what a newer nest sends, encoded
    /// by a twin enum that carries one outcome this build cannot name (the real
    /// enum's `Unknown` arm refuses to serialize, so it cannot stand in).
    #[cfg(not(target_arch = "wasm32"))]
    struct NewerNest(Vec<u8>);

    #[cfg(not(target_arch = "wasm32"))]
    impl NewerNest {
        fn new() -> Self {
            #[derive(serde::Serialize)]
            #[serde(tag = "outcome", rename_all = "snake_case")]
            enum NewerOutcome {
                FromTheFuture,
            }
            Self(
                fauna_protocol::encode_canonical(&NewerOutcome::FromTheFuture)
                    .expect("encode twin")
                    .to_vec(),
            )
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl RpcRequester for NewerNest {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            _kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            Ok(fauna_protocol::decode_strict(&self.0).expect("a newer outcome must decode"))
        }
    }

    /// A read that meets an outcome it cannot name is an error — never
    /// `CalendarNotFound`, which would empty the list and forget the token.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_newer_query_outcome_is_an_error_never_calendar_not_found() {
        let client = CalDavClient::new(NewerNest::new());
        let keys = DavRecipientKeys::derive(&MSEK);
        let err = block_on(client.query_events_decoded(query_req(), &keys))
            .expect_err("an unknown outcome must not read as a page");
        assert!(matches!(err, ReadEventsError::UnknownOutcome), "{err:?}");

        let err = block_on(client.events_flat_in_calendar(&[0xAAu8; 32], &[0xBBu8; 32], &keys))
            .expect_err("an unknown outcome must not read as a missing calendar");
        assert!(matches!(err, ReadEventsError::UnknownOutcome), "{err:?}");
    }

    /// An export that cannot read the calendar fails; it never hands back an
    /// empty VCALENDAR as if the calendar held no events.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_newer_query_outcome_never_exports_an_empty_calendar() {
        let client = CalDavClient::new(NewerNest::new());
        let err = block_on(client.export_calendar_ics(
            &[0xAAu8; 32],
            &[0xBBu8; 32],
            &DavRecipientKeys::derive(&MSEK),
        ))
        .expect_err("export must fail");
        assert!(matches!(err, ReadEventsError::UnknownOutcome), "{err:?}");
    }

    /// The write replies hand the unknown outcome back as `Unknown`: the write
    /// may have landed, so a caller re-reads rather than assuming either way.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_newer_write_outcome_reaches_the_caller_as_unknown() {
        let client = CalDavClient::new(NewerNest::new());
        let put = block_on(client.put_event_ciphertext(PutEventCiphertextRequest {
            actor_id: id32(0xAA),
            calendar_id: id32(0xBB),
            uid_hash: id32(0xCC),
            encrypted_body: vec![1],
            encrypted_index_hint: vec![1],
            timestamp: 0,
            ciphertext_size: 1,
            if_match: None,
            encrypted_fauna_ext: None,
        }))
        .expect("infallible");
        assert_eq!(put, PutEventCiphertextReply::Unknown);

        let del = block_on(client.delete_event(DeleteEventRequest {
            actor_id: id32(0xAA),
            calendar_id: id32(0xBB),
            uid_hash: id32(0xCC),
            if_match: None,
        }))
        .expect("infallible");
        assert_eq!(del, DeleteEventReply::Unknown);

        let provision = block_on(client.provision_calendar(ProvisionCalendarRequest::default()))
            .expect("infallible");
        assert_eq!(provision, ProvisionCalendarReply::Unknown);
    }

    // ── inbound REQUEST / CANCEL apply (Slice 1 — the mailbox-less recipient) ─

    /// A stateful multi-kind mock for the inbound REQUEST/CANCEL roundtrip:
    /// serves `list_calendars` + `query_events` from seeded state and records
    /// every call so a test can assert which writes (provision / put / delete)
    /// the apply path issued. (The single-kind `QueryRequester` / write-only
    /// `RecordingRequester` can't drive the search-then-write flow.)
    #[cfg(not(target_arch = "wasm32"))]
    struct ScheduleRequester {
        calendars: Vec<bridge_routing::CalendarEntry>,
        events: std::collections::BTreeMap<Vec<u8>, Vec<EventEntry>>,
        calls: std::sync::Mutex<Vec<(&'static str, Vec<u8>)>>,
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl ScheduleRequester {
        fn new(
            calendars: Vec<bridge_routing::CalendarEntry>,
            events: std::collections::BTreeMap<Vec<u8>, Vec<EventEntry>>,
        ) -> Self {
            Self {
                calendars,
                events,
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        /// All recorded payloads for `kind`, in call order.
        fn payloads(&self, kind: &str) -> Vec<Vec<u8>> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _)| *k == kind)
                .map(|(_, p)| p.clone())
                .collect()
        }

        fn count(&self, kind: &str) -> usize {
            self.payloads(kind).len()
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl RpcRequester for ScheduleRequester {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            self.calls.lock().unwrap().push((kind, bytes.to_vec()));
            let reply = match kind {
                "fauna.bridges.list_calendars" => {
                    fauna_protocol::encode_canonical(&ListCalendarsReply {
                        calendars: self.calendars.clone(),
                    })
                }
                "fauna.bridges.query_events" => {
                    let req: QueryEventsRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode query req");
                    let events = self
                        .events
                        .get(&req.calendar_id)
                        .cloned()
                        .unwrap_or_default();
                    fauna_protocol::encode_canonical(&QueryEventsReply::Ok {
                        events,
                        highestmodseq: 0,
                        more: false,
                    })
                }
                "fauna.bridges.provision_calendar" => {
                    fauna_protocol::encode_canonical(&ProvisionCalendarReply::Created)
                }
                "fauna.bridges.put_event_ciphertext" => {
                    fauna_protocol::encode_canonical(&PutEventCiphertextReply::Created {
                        event_id: vec![0x11u8; 32],
                        etag: "etag-1".into(),
                        modseq: 1,
                    })
                }
                "fauna.bridges.delete_event" => {
                    fauna_protocol::encode_canonical(&DeleteEventReply::Deleted {
                        event_id: vec![0x22u8; 32],
                        modseq: 2,
                    })
                }
                other => panic!("ScheduleRequester: unhandled kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn cal_entry(calendar_id: &[u8; 32]) -> bridge_routing::CalendarEntry {
        bridge_routing::CalendarEntry {
            calendar_id: calendar_id.to_vec(),
            encrypted_metadata: vec![],
            ctag: 0,
            highestmodseq: 0,
            event_count: 0,
            created_at: 0,
        }
    }

    /// A sealed `EventEntry` whose `uid_hash` is the real `blake3(uid)` (the
    /// search key) — unlike the fixed-uid_hash `sealed_entry` helper.
    #[cfg(not(target_arch = "wasm32"))]
    fn sealed_entry_for_uid(ics: &str, uid: &str) -> EventEntry {
        let body = seal_event_body(ics.as_bytes(), &MSEK).expect("seal body");
        EventEntry {
            event_id: vec![1u8; 32],
            uid_hash: uid_hash(uid).to_vec(),
            encrypted_body: body,
            encrypted_index_hint: seal_event_body(b"", &MSEK).expect("seal hint"),
            etag: "etag-x".into(),
            modseq: 5,
            ciphertext_size: 0,
            internal_date: 1_700_000_000,
            encrypted_fauna_ext: None,
        }
    }

    // ── inbound-mutation authorization fixtures ─────────────────────────────
    // (caldav-server.md § Who may mutate an existing event over the inbound rail)

    /// alice@example.com — the organizer every fixture `.ics` names.
    #[cfg(not(target_arch = "wasm32"))]
    const ORGANIZER_ACTOR: &str =
        "a11ce0000000000000000000000000000000000000000000000000000000a11c";
    /// A co-attendee who holds the UID and the organizer's address — the
    /// attacker the rule exists to stop.
    #[cfg(not(target_arch = "wasm32"))]
    const MALLORY_ACTOR: &str = "ba5eba11000000000000000000000000000000000000000000000000000ba5e0";
    /// The identity the organizer's account succeeded to.
    #[cfg(not(target_arch = "wasm32"))]
    const SUCCESSOR_ACTOR: &str =
        "5ecce55000000000000000000000000000000000000000000000000005ecce55";

    /// The legitimate origin: the organizer, on the recipient's own nest.
    #[cfg(not(target_arch = "wasm32"))]
    fn organizer_origin() -> InboundOrigin {
        InboundOrigin {
            author: Some(ORGANIZER_ACTOR.into()),
            home_nest_url: String::new(),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn mallory_origin() -> InboundOrigin {
        InboundOrigin {
            author: Some(MALLORY_ACTOR.into()),
            home_nest_url: String::new(),
        }
    }

    /// A static CAL-ADDRESS → principal map; anything absent is *no answer*.
    #[cfg(not(target_arch = "wasm32"))]
    struct MapResolver(BTreeMap<String, SchedulingPrincipal>);

    #[cfg(not(target_arch = "wasm32"))]
    impl PrincipalResolver for MapResolver {
        async fn resolve_principal(&self, caladdr: &str) -> Option<SchedulingPrincipal> {
            self.0.get(&normalize_caladdr(caladdr)).cloned()
        }
    }

    /// bob@example.com — the attendee every fixture `REPLY` speaks for.
    #[cfg(not(target_arch = "wasm32"))]
    const BOB_ACTOR: &str = "b0b0000000000000000000000000000000000000000000000000000000000b0b";

    /// bob, replying from the recipient's own nest.
    #[cfg(not(target_arch = "wasm32"))]
    fn bob_origin() -> InboundOrigin {
        InboundOrigin {
            author: Some(BOB_ACTOR.into()),
            home_nest_url: String::new(),
        }
    }

    /// Resolves the fixture organizer and bob, the fixture attendee, on the
    /// recipient's own nest.
    #[cfg(not(target_arch = "wasm32"))]
    fn fan_resolver() -> MapResolver {
        let mut m = BTreeMap::new();
        m.insert(
            "alice@example.com".to_string(),
            SchedulingPrincipal {
                actor_id: ORGANIZER_ACTOR.into(),
                home_nest_url: String::new(),
            },
        );
        m.insert(
            "bob@example.com".to_string(),
            SchedulingPrincipal {
                actor_id: BOB_ACTOR.into(),
                home_nest_url: String::new(),
            },
        );
        MapResolver(m)
    }

    /// A stored event carrying an organizer binding in its sealed sidecar.
    #[cfg(not(target_arch = "wasm32"))]
    fn bound_entry_for_uid(ics: &str, uid: &str, actor: &str, home: &str) -> EventEntry {
        let ext = FaunaEventExt {
            organizer_actor_id: Some(actor.into()),
            organizer_home_nest_url: Some(home.into()),
            ..Default::default()
        };
        EventEntry {
            encrypted_fauna_ext: Some(seal_fauna_ext(&ext, &MSEK).expect("seal ext")),
            ..sealed_entry_for_uid(ics, uid)
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn cancel_ics(uid: &str) -> String {
        generate_itip(
            ITipMethod::Cancel,
            &EventFields {
                uid: uid.into(),
                status: "cancelled".into(),
                ..Default::default()
            },
            &[],
            "alice@example.com",
            "2026-06-13T12:00:00Z",
        )
    }

    /// One stored event in one calendar.
    #[cfg(not(target_arch = "wasm32"))]
    fn requester_holding(entry: EventEntry) -> std::sync::Arc<ScheduleRequester> {
        let cal = [0xC7u8; 32];
        let mut events = std::collections::BTreeMap::new();
        events.insert(cal.to_vec(), vec![entry]);
        std::sync::Arc::new(ScheduleRequester::new(vec![cal_entry(&cal)], events))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn assert_untouched(rec: &ScheduleRequester) {
        assert_eq!(
            rec.count("fauna.bridges.delete_event"),
            0,
            "nothing deleted"
        );
        assert_eq!(
            rec.count("fauna.bridges.put_event_ciphertext"),
            0,
            "nothing rewritten"
        );
    }

    /// The refusal becomes a row that names **the sender**, not the organizer
    /// whose address the forged message carries.
    ///
    /// This is the property the whole surface hangs on. A co-attendee's
    /// `CANCEL` is byte-identical to the organizer's, `ORGANIZER: alice`
    /// included, so a row built from the `.ics` would tell the victim that
    /// *alice* tried to cancel her own event — naming the wrong person, and
    /// naming the one party the rule exists to protect.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_refusal_record_names_the_attested_sender_not_the_forged_organizer() {
        let uid = "victim-1@example.com";
        let rec = requester_holding(bound_entry_for_uid(
            &request_ics(uid),
            uid,
            ORGANIZER_ACTOR,
            "",
        ));
        let client = CalDavClient::new(rec);
        // Her CANCEL calls the event something else — an attacker names the
        // message, so the row must not take its title from there either.
        let forged = build_event_imip(
            ITipMethod::Cancel,
            &EventFields {
                summary: "Free Pizza".into(),
                uid: uid.into(),
                status: "cancelled".into(),
                ..Default::default()
            },
            &[AttendeeInfo {
                email: "bob@example.com".into(),
                partstat: "NEEDS-ACTION".into(),
                ..Default::default()
            }],
            "alice@example.com",
            "2026-06-13T12:00:00Z",
        )
        .expect("build imip")
        .raw_rfc5322;
        let outcome = block_on(client.apply_inbound_scheduling_from_message(
            &ACTOR,
            &MSEK,
            &forged,
            1_700_000_000,
            &mallory_origin(),
            &fan_resolver(),
        ))
        .expect("apply");
        assert!(outcome.is_refused());

        let row = outcome
            .refused_change_record("CANCEL", &mallory_origin(), 1_700_000_500)
            .expect("a refused outcome yields a row");
        assert_eq!(
            row.author.as_deref(),
            Some(MALLORY_ACTOR),
            "the row names the nest-ATTESTED sender"
        );
        assert!(
            !String::from_utf8_lossy(&forged).contains(MALLORY_ACTOR),
            "and nothing in the message itself names her — which is why the \
             attested origin is the only honest source"
        );
        assert_eq!(row.uid_hash, hex::encode(uid_hash(uid)));
        assert_eq!(row.method, "CANCEL");
        assert_eq!(row.reason, RefusalReason::NotTheOrganizer.as_wire());
        assert_eq!(
            row.summary, "Invite",
            "the STORED event's title, not the forged message's \"Free Pizza\""
        );
        assert_eq!(row.first_refused_at, 1_700_000_500);
        assert_eq!(row.last_refused_at, 1_700_000_500);

        // An applied message is not a row.
        let applied = block_on(client.apply_inbound_scheduling_from_message(
            &ACTOR,
            &MSEK,
            &forged,
            1_700_000_000,
            &organizer_origin(),
            &fan_resolver(),
        ))
        .expect("apply");
        assert!(
            applied
                .refused_change_record("CANCEL", &organizer_origin(), 1)
                .is_none(),
            "the organizer's own cancel applies, and reports nothing"
        );
    }

    /// THE hole: a co-attendee holding the UID and the organizer's address
    /// cancels the victim's event. The CANCEL is byte-identical to the
    /// organizer's own — only the origin differs — and the event must survive.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn cancel_from_a_non_organizer_is_refused_and_the_event_survives() {
        let uid = "victim-1@example.com";
        for entry in [
            // bound at creation → local compare
            bound_entry_for_uid(&request_ics(uid), uid, ORGANIZER_ACTOR, ""),
            // unbound (local / imported) → resolve-or-refuse
            sealed_entry_for_uid(&request_ics(uid), uid),
        ] {
            let rec = requester_holding(entry);
            let client = CalDavClient::new(rec.clone());
            let outcome = block_on(client.apply_inbound_cancel(
                &ACTOR,
                &MSEK,
                &cancel_ics(uid),
                &mallory_origin(),
                &fan_resolver(),
            ))
            .expect("apply");
            assert_eq!(
                outcome,
                InboundRequestOutcome::Refused {
                    uid_hash: uid_hash(uid),
                    // The STORED event's title. `cancel_ics` carries no
                    // SUMMARY of its own, so this can only have come off the
                    // event the user holds — which is the row the surface
                    // shows them.
                    summary: "Invite".into(),
                    reason: RefusalReason::NotTheOrganizer,
                }
            );
            assert_untouched(&rec);
        }
    }

    /// The same for a REQUEST that would rewrite the existing event.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn request_overwrite_from_a_non_organizer_is_refused() {
        let uid = "victim-2@example.com";
        let rec = requester_holding(bound_entry_for_uid(
            &request_ics(uid),
            uid,
            ORGANIZER_ACTOR,
            "",
        ));
        let client = CalDavClient::new(rec.clone());
        let outcome = block_on(client.apply_inbound_request(
            &ACTOR,
            &MSEK,
            &request_ics(uid),
            1_700_000_000,
            &mallory_origin(),
            &fan_resolver(),
        ))
        .expect("apply");
        assert_eq!(
            outcome,
            InboundRequestOutcome::Refused {
                uid_hash: uid_hash(uid),
                summary: "Invite".into(),
                reason: RefusalReason::NotTheOrganizer,
            }
        );
        assert_untouched(&rec);
    }

    /// Even the organizer may not re-point the `ORGANIZER` line over this rail.
    /// Every other test's forged message carries alice's address verbatim, so
    /// none of them reaches this gate: here the origin IS the organizer and
    /// only the line changes. On an unbound event the line is the comparand a
    /// later mutation resolves, so an admitted rewrite would hand the event to
    /// whoever the new address names.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_organizer_may_not_rewrite_the_organizer_line() {
        let uid = "victim-2b@example.com";
        let repointed = request_ics(uid).replace(
            "ORGANIZER:mailto:alice@example.com",
            "ORGANIZER:mailto:mallory@example.com",
        );
        assert_ne!(
            repointed,
            request_ics(uid),
            "the fixture re-points the line"
        );
        for entry in [
            bound_entry_for_uid(&request_ics(uid), uid, ORGANIZER_ACTOR, ""),
            sealed_entry_for_uid(&request_ics(uid), uid),
        ] {
            let rec = requester_holding(entry);
            let client = CalDavClient::new(rec.clone());
            let outcome = block_on(client.apply_inbound_request(
                &ACTOR,
                &MSEK,
                &repointed,
                1_700_000_000,
                &organizer_origin(),
                &fan_resolver(),
            ))
            .expect("apply");
            assert_eq!(
                outcome,
                InboundRequestOutcome::Refused {
                    uid_hash: uid_hash(uid),
                    summary: "Invite".into(),
                    reason: RefusalReason::OrganizerChanged,
                }
            );
            assert_untouched(&rec);
        }
    }

    /// The right actor id attested by the WRONG nest is not the organizer: a
    /// hostile nest can attest any id, but only for channels homed on itself.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_organizers_actor_id_from_another_nest_is_refused() {
        let uid = "victim-3@example.com";
        let rec = requester_holding(bound_entry_for_uid(
            &request_ics(uid),
            uid,
            ORGANIZER_ACTOR,
            "",
        ));
        let client = CalDavClient::new(rec.clone());
        let forged = InboundOrigin {
            author: Some(ORGANIZER_ACTOR.into()),
            home_nest_url: "https://hostile.example".into(),
        };
        let outcome = block_on(client.apply_inbound_cancel(
            &ACTOR,
            &MSEK,
            &cancel_ics(uid),
            &forged,
            &fan_resolver(),
        ))
        .expect("apply");
        assert!(matches!(
            outcome,
            InboundRequestOutcome::Refused {
                reason: RefusalReason::NotTheOrganizer,
                ..
            }
        ));
        assert_untouched(&rec);
    }

    /// No attested author (the home nest gave no answer) and an unresolvable organizer are
    /// both *no answer* — and no answer is never permission.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn no_answer_refuses_a_mutation() {
        let uid = "victim-4@example.com";
        let rec = requester_holding(sealed_entry_for_uid(&request_ics(uid), uid));
        let client = CalDavClient::new(rec.clone());

        let unattested = InboundOrigin::default();
        let outcome = block_on(client.apply_inbound_cancel(
            &ACTOR,
            &MSEK,
            &cancel_ics(uid),
            &unattested,
            &fan_resolver(),
        ))
        .expect("apply");
        assert!(matches!(
            outcome,
            InboundRequestOutcome::Refused {
                reason: RefusalReason::NoAttestedAuthor,
                ..
            }
        ));

        // Unbound event + a resolver with no answer (web; an external organizer).
        let outcome = block_on(client.apply_inbound_cancel(
            &ACTOR,
            &MSEK,
            &cancel_ics(uid),
            &organizer_origin(),
            &NoPrincipalResolver,
        ))
        .expect("apply");
        assert!(matches!(
            outcome,
            InboundRequestOutcome::Refused {
                reason: RefusalReason::OrganizerUnresolvable,
                ..
            }
        ));
        assert_untouched(&rec);
    }

    /// A bound event needs no resolver at all: the organizer's own CANCEL works
    /// offline / on web (no false positive).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_bound_organizer_cancels_with_no_resolver() {
        let uid = "mine-5@example.com";
        let rec = requester_holding(bound_entry_for_uid(
            &request_ics(uid),
            uid,
            ORGANIZER_ACTOR,
            "",
        ));
        let client = CalDavClient::new(rec.clone());
        let outcome = block_on(client.apply_inbound_cancel(
            &ACTOR,
            &MSEK,
            &cancel_ics(uid),
            &organizer_origin(),
            &NoPrincipalResolver,
        ))
        .expect("apply");
        assert!(matches!(outcome, InboundRequestOutcome::Cancelled { .. }));
        assert_eq!(rec.count("fauna.bridges.delete_event"), 1);
    }

    /// `bound actor id -> verified successor`, counting how often it is asked;
    /// resolves no CAL-ADDRESS at all.
    #[cfg(not(target_arch = "wasm32"))]
    #[derive(Default)]
    struct SuccessionResolver {
        line: BTreeMap<String, String>,
        asked: std::sync::atomic::AtomicUsize,
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl SuccessionResolver {
        fn organizer_succeeded_to(successor: &str) -> Self {
            let mut line = BTreeMap::new();
            line.insert(ORGANIZER_ACTOR.to_string(), successor.to_string());
            Self {
                line,
                asked: Default::default(),
            }
        }

        fn asked(&self) -> usize {
            self.asked.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl PrincipalResolver for SuccessionResolver {
        async fn resolve_principal(&self, _caladdr: &str) -> Option<SchedulingPrincipal> {
            None
        }

        async fn resolve_successor(&self, bound: &SchedulingPrincipal) -> Option<String> {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.line.get(&bound.actor_id.to_ascii_lowercase()).cloned()
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn successor_origin(home_nest_url: &str) -> InboundOrigin {
        InboundOrigin {
            author: Some(SUCCESSOR_ACTOR.into()),
            home_nest_url: home_nest_url.into(),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn held_bound_to_the_organizer(uid: &str) -> std::sync::Arc<ScheduleRequester> {
        requester_holding(bound_entry_for_uid(
            &request_ics(uid),
            uid,
            ORGANIZER_ACTOR,
            "",
        ))
    }

    /// The identity the organizer's account succeeded to cancels an event the
    /// retired identity organized — the binding names a verified LINE, not one
    /// key. Without a verifiable answer the refusal stands exactly as before.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_succeeded_organizers_cancel_is_honoured_only_through_a_verified_succession() {
        let uid = "succeeded-1@example.com";

        let rec = held_bound_to_the_organizer(uid);
        let refused = block_on(CalDavClient::new(rec.clone()).apply_inbound_cancel(
            &ACTOR,
            &MSEK,
            &cancel_ics(uid),
            &successor_origin(""),
            &NoPrincipalResolver,
        ))
        .expect("apply");
        assert!(matches!(
            refused,
            InboundRequestOutcome::Refused {
                reason: RefusalReason::NotTheOrganizer,
                ..
            }
        ));
        assert_untouched(&rec);

        let rec = held_bound_to_the_organizer(uid);
        let resolver = SuccessionResolver::organizer_succeeded_to(SUCCESSOR_ACTOR);
        let outcome = block_on(CalDavClient::new(rec.clone()).apply_inbound_cancel(
            &ACTOR,
            &MSEK,
            &cancel_ics(uid),
            &successor_origin(""),
            &resolver,
        ))
        .expect("apply");
        assert!(matches!(outcome, InboundRequestOutcome::Cancelled { .. }));
        assert_eq!(rec.count("fauna.bridges.delete_event"), 1);
        assert_eq!(resolver.asked(), 1);
    }

    /// A succession somewhere is not permission for everyone: an author who is
    /// not where the bound identity ended up stays refused, and so does the
    /// real successor speaking from any nest but the bound one — the ceremony
    /// runs only on the identity's home nest, and an attestation is only as
    /// honest as the nest making it. The second is refused WITHOUT a lookup.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_succession_admits_only_the_successor_on_the_bound_nest() {
        let uid = "succeeded-2@example.com";

        let rec = held_bound_to_the_organizer(uid);
        let resolver = SuccessionResolver::organizer_succeeded_to(SUCCESSOR_ACTOR);
        let outcome = block_on(CalDavClient::new(rec.clone()).apply_inbound_cancel(
            &ACTOR,
            &MSEK,
            &cancel_ics(uid),
            &mallory_origin(),
            &resolver,
        ))
        .expect("apply");
        assert!(matches!(
            outcome,
            InboundRequestOutcome::Refused {
                reason: RefusalReason::NotTheOrganizer,
                ..
            }
        ));
        assert_untouched(&rec);

        let rec = held_bound_to_the_organizer(uid);
        let resolver = SuccessionResolver::organizer_succeeded_to(SUCCESSOR_ACTOR);
        let outcome = block_on(CalDavClient::new(rec.clone()).apply_inbound_cancel(
            &ACTOR,
            &MSEK,
            &cancel_ics(uid),
            &successor_origin("https://elsewhere.example"),
            &resolver,
        ))
        .expect("apply");
        assert!(matches!(
            outcome,
            InboundRequestOutcome::Refused {
                reason: RefusalReason::NotTheOrganizer,
                ..
            }
        ));
        assert_untouched(&rec);
        assert_eq!(resolver.asked(), 0);
    }

    /// A bound nest the succession dial settles nothing at — unreachable, or
    /// slow past the budget in the hostile case — counting how often it is
    /// asked.
    #[cfg(not(target_arch = "wasm32"))]
    #[derive(Default)]
    struct UnansweringBoundNest {
        dials: std::sync::atomic::AtomicUsize,
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl SuccessionDialer for &UnansweringBoundNest {
        async fn walk(&self, _old: &str, _anchor: &str) -> SuccessionLookup {
            self.dials.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            SuccessionLookup::Unproven
        }
    }

    /// A stranger's repeated CANCELs for a bound event cost the receive loop
    /// ONE succession dial per session, not one per message
    /// (caldav-server.md § Who may mutate an existing event over the inbound
    /// rail → *A succeeded organizer*: a dial that settled nothing is paid
    /// once per bound identity per session). Each message gets a resolver built afresh — as
    /// the native sink builds one per message — over the session's one memo;
    /// every CANCEL is still refused.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_strangers_repeated_cancels_pay_one_succession_dial_per_session() {
        let uid = "succeeded-memo@example.com";
        let rec = held_bound_to_the_organizer(uid);
        let bound_nest = UnansweringBoundNest::default();
        let memo = SuccessionMemo::new();
        for _ in 0..3 {
            let resolver = MemoizedSuccessionResolver {
                addresses: NoPrincipalResolver,
                dialer: &bound_nest,
                own_nest_url: "https://own.example".into(),
                memo: memo.clone(),
            };
            let outcome = block_on(CalDavClient::new(rec.clone()).apply_inbound_cancel(
                &ACTOR,
                &MSEK,
                &cancel_ics(uid),
                &mallory_origin(),
                &resolver,
            ))
            .expect("apply");
            assert!(matches!(
                outcome,
                InboundRequestOutcome::Refused {
                    reason: RefusalReason::NotTheOrganizer,
                    ..
                }
            ));
        }
        assert_untouched(&rec);
        assert_eq!(
            bound_nest.dials.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    /// A bound nest that is up and answers each succession dial with whatever it
    /// is currently scripted to say — a succession can land between two asks.
    #[cfg(not(target_arch = "wasm32"))]
    struct ScriptedBoundNest {
        answer: std::sync::Mutex<SuccessionLookup>,
        dials: std::sync::atomic::AtomicUsize,
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl SuccessionDialer for &ScriptedBoundNest {
        async fn walk(&self, _old: &str, _anchor: &str) -> SuccessionLookup {
            self.dials.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.answer.lock().unwrap().clone()
        }
    }

    /// A definitive "not succeeded" is NOT remembered, so a stranger cannot pin
    /// it for the session: the stranger's CANCEL
    /// is refused on the bound nest's quick "never succeeded", the organizer
    /// then succeeds, and the successor's CANCEL in the SAME session is
    /// honoured (caldav-server.md § Who may mutate an existing event over the
    /// inbound rail → *A succeeded organizer*).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_strangers_cancel_before_the_succession_does_not_pin_it_for_the_session() {
        let uid = "succeeded-unpinned@example.com";
        let rec = held_bound_to_the_organizer(uid);
        let bound_nest = ScriptedBoundNest {
            answer: std::sync::Mutex::new(SuccessionLookup::NotSucceeded),
            dials: Default::default(),
        };
        let memo = SuccessionMemo::new();
        let resolver = || MemoizedSuccessionResolver {
            addresses: NoPrincipalResolver,
            dialer: &bound_nest,
            own_nest_url: "https://own.example".into(),
            memo: memo.clone(),
        };

        let outcome = block_on(CalDavClient::new(rec.clone()).apply_inbound_cancel(
            &ACTOR,
            &MSEK,
            &cancel_ics(uid),
            &mallory_origin(),
            &resolver(),
        ))
        .expect("apply");
        assert!(matches!(
            outcome,
            InboundRequestOutcome::Refused {
                reason: RefusalReason::NotTheOrganizer,
                ..
            }
        ));
        assert_untouched(&rec);

        *bound_nest.answer.lock().unwrap() = SuccessionLookup::Succeeded(SUCCESSOR_ACTOR.into());
        let outcome = block_on(CalDavClient::new(rec.clone()).apply_inbound_cancel(
            &ACTOR,
            &MSEK,
            &cancel_ics(uid),
            &successor_origin(""),
            &resolver(),
        ))
        .expect("apply");
        assert!(matches!(outcome, InboundRequestOutcome::Cancelled { .. }));
        assert_eq!(
            bound_nest.dials.load(std::sync::atomic::Ordering::SeqCst),
            2
        );
    }

    /// The ordinary case never looks anything up: the bound organizer's own
    /// CANCEL is settled by the local comparison, so it still works offline and
    /// cannot be lost to a lookup outage.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_bound_organizers_own_cancel_never_asks_for_a_succession() {
        let uid = "succeeded-3@example.com";
        let rec = held_bound_to_the_organizer(uid);
        let resolver = SuccessionResolver::organizer_succeeded_to(SUCCESSOR_ACTOR);
        let outcome = block_on(CalDavClient::new(rec.clone()).apply_inbound_cancel(
            &ACTOR,
            &MSEK,
            &cancel_ics(uid),
            &organizer_origin(),
            &resolver,
        ))
        .expect("apply");
        assert!(matches!(outcome, InboundRequestOutcome::Cancelled { .. }));
        assert_eq!(resolver.asked(), 0);
    }

    /// An update the successor sends RE-BINDS the event to it, keeping the rest
    /// of the sidecar: the line is walked once, later mutations compare locally
    /// again, and from then on the retired identity no longer matches.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_successors_update_rebinds_the_event_to_the_successor() {
        let uid = "succeeded-4@example.com";
        let rec = held_bound_to_the_organizer(uid);
        let resolver = SuccessionResolver::organizer_succeeded_to(SUCCESSOR_ACTOR);
        let outcome = block_on(CalDavClient::new(rec.clone()).apply_inbound_request(
            &ACTOR,
            &MSEK,
            &request_ics(uid),
            1_700_000_000,
            &successor_origin(""),
            &resolver,
        ))
        .expect("apply");
        assert!(matches!(outcome, InboundRequestOutcome::Updated { .. }));
        let put: PutEventCiphertextRequest =
            fauna_protocol::decode_strict(&rec.payloads("fauna.bridges.put_event_ciphertext")[0])
                .expect("decode put");
        let ext = unseal_fauna_ext(
            put.encrypted_fauna_ext
                .as_deref()
                .expect("a re-binding update writes the sidecar"),
            &DavRecipientKeys::derive(&MSEK),
        )
        .expect("unseal");
        assert_eq!(ext.organizer_actor_id.as_deref(), Some(SUCCESSOR_ACTOR));
        assert_eq!(ext.organizer_home_nest_url.as_deref(), Some(""));
    }

    /// Creation binds the event to its origin in the sealed sidecar — and a
    /// creating REQUEST whose ORGANIZER definitively names someone else (a spoof
    /// / UID squat) is refused.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn creation_binds_the_origin_and_refuses_a_spoofed_organizer() {
        let uid = "fresh-6@example.com";
        let rec = std::sync::Arc::new(ScheduleRequester::new(vec![], Default::default()));
        let client = CalDavClient::new(rec.clone());

        let spoofed = block_on(client.apply_inbound_request(
            &ACTOR,
            &MSEK,
            &request_ics(uid),
            1_700_000_000,
            &mallory_origin(),
            &fan_resolver(),
        ))
        .expect("apply");
        assert_eq!(
            spoofed,
            InboundRequestOutcome::Refused {
                uid_hash: uid_hash(uid),
                // Nothing is stored under this UID (the spoof is a CREATING
                // request), so the title is the one the message brought.
                summary: "Invite".into(),
                reason: RefusalReason::SpoofedOrganizer,
            }
        );
        assert_untouched(&rec);

        let created = block_on(client.apply_inbound_request(
            &ACTOR,
            &MSEK,
            &request_ics(uid),
            1_700_000_000,
            &organizer_origin(),
            &fan_resolver(),
        ))
        .expect("apply");
        assert!(matches!(created, InboundRequestOutcome::Created { .. }));
        let put: PutEventCiphertextRequest =
            fauna_protocol::decode_strict(&rec.payloads("fauna.bridges.put_event_ciphertext")[0])
                .expect("decode put");
        let ext = unseal_fauna_ext(
            put.encrypted_fauna_ext
                .as_deref()
                .expect("binding sidecar written"),
            &DavRecipientKeys::derive(&MSEK),
        )
        .expect("unseal ext");
        assert_eq!(ext.organizer_actor_id.as_deref(), Some(ORGANIZER_ACTOR));
        assert_eq!(ext.organizer_home_nest_url.as_deref(), Some(""));
    }

    /// Build a `METHOD:REQUEST` invite `.ics` for a single attendee.
    #[cfg(not(target_arch = "wasm32"))]
    fn request_ics(uid: &str) -> String {
        let event = EventFields {
            summary: "Invite".into(),
            dtstart: "2026-07-01T15:00:00Z".into(),
            uid: uid.into(),
            ..Default::default()
        };
        let attendees = vec![AttendeeInfo {
            email: "bob@example.com".into(),
            partstat: "NEEDS-ACTION".into(),
            ..Default::default()
        }];
        generate_itip(
            ITipMethod::Request,
            &event,
            &attendees,
            "alice@example.com",
            "2026-06-13T12:00:00Z",
        )
    }

    const ACTOR: [u8; 32] = [0xAAu8; 32];

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn apply_inbound_request_materializes_new_invite_into_personal() {
        // No calendars yet → the invite is unseen → ensure Personal + PUT into it.
        let rec = std::sync::Arc::new(ScheduleRequester::new(
            vec![],
            std::collections::BTreeMap::new(),
        ));
        let client = CalDavClient::new(rec.clone());
        let uid = "inv-new@example.com";
        let outcome = block_on(client.apply_inbound_request(
            &ACTOR,
            &MSEK,
            &request_ics(uid),
            1_700_000_000,
            &organizer_origin(),
            &fan_resolver(),
        ))
        .expect("apply request");

        let target = uid_hash(uid);
        assert_eq!(
            outcome,
            InboundRequestOutcome::Created {
                uid_hash: target,
                calendar_id: personal_calendar_id(),
            }
        );
        // It provisioned Personal (since it didn't exist) before the PUT.
        assert_eq!(rec.count("fauna.bridges.provision_calendar"), 1);
        let prov: ProvisionCalendarRequest =
            fauna_protocol::decode_strict(&rec.payloads("fauna.bridges.provision_calendar")[0])
                .expect("decode provision");
        assert_eq!(prov.calendar_id, personal_calendar_id().to_vec());
        assert!(!prov.update_metadata, "MKCOL/insert path, not PROPPATCH");
        // The PUT targets Personal, keys on the right uid_hash, and its sealed
        // body unseals to the REQUEST's VEVENT.
        assert_eq!(rec.count("fauna.bridges.put_event_ciphertext"), 1);
        let put: PutEventCiphertextRequest =
            fauna_protocol::decode_strict(&rec.payloads("fauna.bridges.put_event_ciphertext")[0])
                .expect("decode put");
        assert_eq!(put.calendar_id, personal_calendar_id().to_vec());
        assert_eq!(put.uid_hash, target.to_vec());
        let body = unseal_event_body(&put.encrypted_body, &MSEK).expect("unseal put body");
        let stored = parse_ical(&String::from_utf8(body).unwrap()).expect("parse stored");
        assert_eq!(stored.summary, "Invite");
        assert_eq!(stored.uid, uid);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn apply_inbound_request_updates_existing_event_in_place() {
        // The invite's UID already lives in calendar C (an organizer re-send) →
        // update C in place, no Personal provision.
        let cal_c = [0xC1u8; 32];
        let uid = "inv-resend@example.com";
        let mut events = std::collections::BTreeMap::new();
        events.insert(
            cal_c.to_vec(),
            vec![sealed_entry_for_uid(&request_ics(uid), uid)],
        );
        let rec = std::sync::Arc::new(ScheduleRequester::new(vec![cal_entry(&cal_c)], events));
        let client = CalDavClient::new(rec.clone());

        let outcome = block_on(client.apply_inbound_request(
            &ACTOR,
            &MSEK,
            &request_ics(uid),
            1_700_000_000,
            &organizer_origin(),
            &fan_resolver(),
        ))
        .expect("apply request");

        assert_eq!(
            outcome,
            InboundRequestOutcome::Updated {
                uid_hash: uid_hash(uid),
                calendar_id: cal_c,
            }
        );
        assert_eq!(
            rec.count("fauna.bridges.provision_calendar"),
            0,
            "an existing event is updated in place — no Personal provision"
        );
        assert_eq!(rec.count("fauna.bridges.put_event_ciphertext"), 1);
        let put: PutEventCiphertextRequest =
            fauna_protocol::decode_strict(&rec.payloads("fauna.bridges.put_event_ciphertext")[0])
                .expect("decode put");
        assert_eq!(put.calendar_id, cal_c.to_vec());
        assert!(
            put.encrypted_fauna_ext.is_none(),
            "no sidecar on the re-PUT → nest preserves the recipient's prior refinement"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn apply_inbound_request_no_uid_is_noop() {
        let rec = std::sync::Arc::new(ScheduleRequester::new(
            vec![],
            std::collections::BTreeMap::new(),
        ));
        let client = CalDavClient::new(rec.clone());
        // A VEVENT with an empty UID can't be addressed.
        let ics = generate_itip(
            ITipMethod::Request,
            &EventFields {
                summary: "No uid".into(),
                ..Default::default()
            },
            &[],
            "alice@example.com",
            "2026-06-13T12:00:00Z",
        );
        let outcome = block_on(client.apply_inbound_request(
            &ACTOR,
            &MSEK,
            &ics,
            1_700_000_000,
            &organizer_origin(),
            &fan_resolver(),
        ))
        .expect("ok");
        assert_eq!(outcome, InboundRequestOutcome::NoUid);
        assert_eq!(rec.count("fauna.bridges.put_event_ciphertext"), 0);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn apply_inbound_cancel_removes_matching_event() {
        let cal_c = [0xC2u8; 32];
        let uid = "inv-cancel@example.com";
        let mut events = std::collections::BTreeMap::new();
        events.insert(
            cal_c.to_vec(),
            vec![sealed_entry_for_uid(&request_ics(uid), uid)],
        );
        let rec = std::sync::Arc::new(ScheduleRequester::new(vec![cal_entry(&cal_c)], events));
        let client = CalDavClient::new(rec.clone());

        // Build a METHOD:CANCEL for the same UID.
        let cancel = generate_itip(
            ITipMethod::Cancel,
            &EventFields {
                uid: uid.into(),
                status: "cancelled".into(),
                ..Default::default()
            },
            &[],
            "alice@example.com",
            "2026-06-13T12:00:00Z",
        );
        let outcome = block_on(client.apply_inbound_cancel(
            &ACTOR,
            &MSEK,
            &cancel,
            &organizer_origin(),
            &fan_resolver(),
        ))
        .expect("apply cancel");

        assert_eq!(
            outcome,
            InboundRequestOutcome::Cancelled {
                uid_hash: uid_hash(uid),
                calendar_id: cal_c,
            }
        );
        assert_eq!(rec.count("fauna.bridges.delete_event"), 1);
        let del: DeleteEventRequest =
            fauna_protocol::decode_strict(&rec.payloads("fauna.bridges.delete_event")[0])
                .expect("decode delete");
        assert_eq!(del.calendar_id, cal_c.to_vec());
        assert_eq!(del.uid_hash, uid_hash(uid).to_vec());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn apply_inbound_cancel_no_matching_event_is_noop() {
        let rec = std::sync::Arc::new(ScheduleRequester::new(
            vec![],
            std::collections::BTreeMap::new(),
        ));
        let client = CalDavClient::new(rec.clone());
        let cancel = generate_itip(
            ITipMethod::Cancel,
            &EventFields {
                uid: "ghost@example.com".into(),
                status: "cancelled".into(),
                ..Default::default()
            },
            &[],
            "alice@example.com",
            "2026-06-13T12:00:00Z",
        );
        let outcome = block_on(client.apply_inbound_cancel(
            &ACTOR,
            &MSEK,
            &cancel,
            &organizer_origin(),
            &fan_resolver(),
        ))
        .expect("ok");
        assert_eq!(outcome, InboundRequestOutcome::NoMatchingEvent);
        assert_eq!(rec.count("fauna.bridges.delete_event"), 0);
    }

    // ── Slice 4: METHOD-routed apply over a raw RFC 5322 iMIP ───────────────
    // `apply_inbound_scheduling_from_message` is the recipient-drain counterpart
    // of `apply_inbound_reply_from_mail` for the mailbox-less WS-RPC rail: the
    // scheduling iMIP rides the SAME raw RFC 5322 bytes the email rail carries,
    // so the extract + METHOD gate is single-sourced here and the conv receive
    // loop hands each `Scheduling` channel message straight to it.

    /// Build a real RFC 5322 iMIP (`build_event_imip` → `text/calendar; method=…`)
    /// for the given METHOD, single attendee `bob@example.com`.
    #[cfg(not(target_arch = "wasm32"))]
    fn imip_bytes(method: ITipMethod, uid: &str, partstat: &str) -> Vec<u8> {
        imip_bytes_for(method, uid, &["bob@example.com"], partstat)
    }

    /// [`imip_bytes`] naming `emails` as its attendees, each at `partstat`.
    #[cfg(not(target_arch = "wasm32"))]
    fn imip_bytes_for(method: ITipMethod, uid: &str, emails: &[&str], partstat: &str) -> Vec<u8> {
        let event = EventFields {
            summary: "Invite".into(),
            dtstart: "2026-07-01T15:00:00Z".into(),
            uid: uid.into(),
            ..Default::default()
        };
        let attendees = emails
            .iter()
            .map(|email| AttendeeInfo {
                email: (*email).into(),
                partstat: partstat.into(),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        build_event_imip(
            method,
            &event,
            &attendees,
            "alice@example.com",
            "2026-06-13T12:00:00Z",
        )
        .expect("build imip")
        .raw_rfc5322
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn scheduling_from_message_routes_request_to_apply_inbound_request() {
        // A REQUEST iMIP with no prior calendars → materialize into Personal,
        // wrapped as `SchedulingApplyOutcome::Request(Created)`.
        let rec = std::sync::Arc::new(ScheduleRequester::new(
            vec![],
            std::collections::BTreeMap::new(),
        ));
        let client = CalDavClient::new(rec.clone());
        let uid = "inv-req@example.com";
        let raw = imip_bytes(ITipMethod::Request, uid, "NEEDS-ACTION");

        let outcome = block_on(client.apply_inbound_scheduling_from_message(
            &ACTOR,
            &MSEK,
            &raw,
            1_700_000_000,
            &organizer_origin(),
            &fan_resolver(),
        ))
        .expect("apply scheduling");

        assert_eq!(
            outcome,
            SchedulingApplyOutcome::Request(InboundRequestOutcome::Created {
                uid_hash: uid_hash(uid),
                calendar_id: personal_calendar_id(),
            })
        );
        assert_eq!(rec.count("fauna.bridges.put_event_ciphertext"), 1);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn scheduling_from_message_routes_cancel_to_apply_inbound_cancel() {
        // A CANCEL iMIP for an event that exists → tombstone it,
        // `SchedulingApplyOutcome::Request(Cancelled)`.
        let cal_c = [0xC4u8; 32];
        let uid = "inv-cxl@example.com";
        let mut events = std::collections::BTreeMap::new();
        events.insert(
            cal_c.to_vec(),
            vec![sealed_entry_for_uid(&request_ics(uid), uid)],
        );
        let rec = std::sync::Arc::new(ScheduleRequester::new(vec![cal_entry(&cal_c)], events));
        let client = CalDavClient::new(rec.clone());
        let raw = imip_bytes(ITipMethod::Cancel, uid, "NEEDS-ACTION");

        let outcome = block_on(client.apply_inbound_scheduling_from_message(
            &ACTOR,
            &MSEK,
            &raw,
            1_700_000_000,
            &organizer_origin(),
            &fan_resolver(),
        ))
        .expect("apply scheduling");

        assert_eq!(
            outcome,
            SchedulingApplyOutcome::Request(InboundRequestOutcome::Cancelled {
                uid_hash: uid_hash(uid),
                calendar_id: cal_c,
            })
        );
        assert_eq!(rec.count("fauna.bridges.delete_event"), 1);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn scheduling_from_message_routes_reply_to_the_gated_reply_merge() {
        // The organizer holds the event (bob NEEDS-ACTION); a REPLY iMIP from
        // bob (ACCEPTED) merges his PARTSTAT → `SchedulingApplyOutcome::Reply(Applied)`.
        let cal_c = [0xC5u8; 32];
        let uid = "inv-rep@example.com";
        let mut events = std::collections::BTreeMap::new();
        events.insert(
            cal_c.to_vec(),
            vec![sealed_entry_for_uid(&request_ics(uid), uid)],
        );
        let rec = std::sync::Arc::new(ScheduleRequester::new(vec![cal_entry(&cal_c)], events));
        let client = CalDavClient::new(rec.clone());
        let raw = imip_bytes(ITipMethod::Reply, uid, "ACCEPTED");

        let outcome = block_on(client.apply_inbound_scheduling_from_message(
            &ACTOR,
            &MSEK,
            &raw,
            1_700_000_000,
            &bob_origin(),
            &fan_resolver(),
        ))
        .expect("apply scheduling");

        match outcome {
            SchedulingApplyOutcome::Reply(InboundReplyOutcome::Applied {
                uid_hash: u,
                attendees,
            }) => {
                assert_eq!(u, uid_hash(uid));
                assert!(
                    attendees
                        .iter()
                        .any(|a| a.email == "bob@example.com" && a.partstat == "ACCEPTED"),
                    "bob's PARTSTAT merged to ACCEPTED, got {attendees:?}"
                );
            }
            other => panic!("expected Reply(Applied), got {other:?}"),
        }
        assert_eq!(rec.count("fauna.bridges.put_event_ciphertext"), 1);
    }

    /// Apply bob's `REPLY` (ACCEPTED) to the organizer's stored event from
    /// `origin`, resolving through `resolver` — the fixture every REPLY
    /// authorization test below shares. Returns the outcome and whether the
    /// stored event was re-PUT.
    #[cfg(not(target_arch = "wasm32"))]
    fn apply_bobs_reply<P: PrincipalResolver>(
        origin: &InboundOrigin,
        resolver: &P,
    ) -> (SchedulingApplyOutcome, usize) {
        let uid = "inv-rep-auth@example.com";
        let (outcome, rec) = apply_reply_to_stored(
            uid,
            &imip_bytes(ITipMethod::Reply, uid, "ACCEPTED"),
            origin,
            resolver,
        );
        (outcome, rec.count("fauna.bridges.put_event_ciphertext"))
    }

    /// Apply the raw `REPLY` iMIP `raw` to an organizer store holding one event
    /// under `stored_uid` (bob its only attendee). Returns the outcome and the
    /// requester, so a test can count the round trips it made.
    #[cfg(not(target_arch = "wasm32"))]
    fn apply_reply_to_stored<P: PrincipalResolver>(
        stored_uid: &str,
        raw: &[u8],
        origin: &InboundOrigin,
        resolver: &P,
    ) -> (SchedulingApplyOutcome, std::sync::Arc<ScheduleRequester>) {
        let cal_c = [0xC5u8; 32];
        let mut events = std::collections::BTreeMap::new();
        events.insert(
            cal_c.to_vec(),
            vec![sealed_entry_for_uid(&request_ics(stored_uid), stored_uid)],
        );
        let rec = std::sync::Arc::new(ScheduleRequester::new(vec![cal_entry(&cal_c)], events));
        let outcome = block_on(
            CalDavClient::new(rec.clone()).apply_inbound_scheduling_from_message(
                &ACTOR,
                &MSEK,
                raw,
                1_700_000_000,
                origin,
                resolver,
            ),
        )
        .expect("apply scheduling");
        (outcome, rec)
    }

    /// Wraps a resolver, counting every CAL-ADDRESS it is asked to resolve —
    /// the only witness that a `REPLY` dialed nobody (on web each resolution is
    /// a browser `WebSocket` to the address's own host).
    #[cfg(not(target_arch = "wasm32"))]
    struct CountingResolver<P> {
        inner: P,
        asked: std::sync::atomic::AtomicUsize,
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl<P> CountingResolver<P> {
        fn new(inner: P) -> Self {
            Self {
                inner,
                asked: Default::default(),
            }
        }

        fn asked(&self) -> usize {
            self.asked.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl<P: PrincipalResolver> PrincipalResolver for CountingResolver<P> {
        async fn resolve_principal(&self, caladdr: &str) -> Option<SchedulingPrincipal> {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.resolve_principal(caladdr).await
        }

        async fn resolve_successor(&self, bound: &SchedulingPrincipal) -> Option<String> {
            self.inner.resolve_successor(bound).await
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_reply_for_an_unknown_uid_resolves_no_address() {
        // Nothing is stored under the REPLY's UID, so it can change nothing:
        // the event is looked up FIRST and no attendee address is dialed.
        let resolver = CountingResolver::new(fan_resolver());
        let raw = imip_bytes(ITipMethod::Reply, "no-such-event@example.com", "ACCEPTED");
        let (outcome, rec) =
            apply_reply_to_stored("inv-rep-auth@example.com", &raw, &bob_origin(), &resolver);
        assert_eq!(
            outcome,
            SchedulingApplyOutcome::Reply(InboundReplyOutcome::NoMatchingEvent)
        );
        assert_eq!(resolver.asked(), 0, "an unknown UID dials nobody");
        assert_eq!(rec.count("fauna.bridges.put_event_ciphertext"), 0);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_reply_naming_only_off_roster_attendees_resolves_no_address_and_is_no_refusal() {
        // A stranger holding the UID names an address the stored roster does
        // not carry — a host of its own choosing. The roster merge would ignore
        // it, so it is never resolved, and a message that can change nothing is
        // `NoMatchingAttendee`, not a refusal (caldav-server.md § Who may
        // mutate an existing event over the inbound rail).
        let uid = "inv-rep-auth@example.com";
        let resolver = CountingResolver::new(fan_resolver());
        let raw = imip_bytes_for(
            ITipMethod::Reply,
            uid,
            &["mallory@tracker.example:8443"],
            "ACCEPTED",
        );
        let (outcome, rec) = apply_reply_to_stored(uid, &raw, &mallory_origin(), &resolver);
        assert_eq!(
            outcome,
            SchedulingApplyOutcome::Reply(InboundReplyOutcome::NoMatchingAttendee)
        );
        assert_eq!(resolver.asked(), 0, "an off-roster address is never dialed");
        assert_eq!(rec.count("fauna.bridges.put_event_ciphertext"), 0);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn an_unchanged_reply_resolves_no_address_and_is_no_refusal() {
        // mallory repeats bob's stored NEEDS-ACTION back: the merge changes
        // nothing, so nothing is resolved and the no-op is `NoMatchingAttendee`
        // — not a refusal, although she is not bob (caldav-server.md § Who may
        // mutate an existing event over the inbound rail).
        let uid = "inv-rep-auth@example.com";
        let resolver = CountingResolver::new(fan_resolver());
        let raw = imip_bytes(ITipMethod::Reply, uid, "NEEDS-ACTION");
        let (outcome, rec) = apply_reply_to_stored(uid, &raw, &mallory_origin(), &resolver);
        assert_eq!(
            outcome,
            SchedulingApplyOutcome::Reply(InboundReplyOutcome::NoMatchingAttendee)
        );
        assert_eq!(resolver.asked(), 0, "an unchanged answer dials nobody");
        assert_eq!(rec.count("fauna.bridges.put_event_ciphertext"), 0);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_reply_resolves_only_its_on_roster_attendees() {
        // bob answers for himself and also names an off-roster address: only
        // bob — an address the organizer already holds — is resolved, and the
        // off-roster line neither dials nor blocks bob's own answer.
        let uid = "inv-rep-auth@example.com";
        let resolver = CountingResolver::new(fan_resolver());
        let raw = imip_bytes_for(
            ITipMethod::Reply,
            uid,
            &["bob@example.com", "stranger@tracker.example"],
            "ACCEPTED",
        );
        let (outcome, rec) = apply_reply_to_stored(uid, &raw, &bob_origin(), &resolver);
        assert!(
            matches!(
                outcome,
                SchedulingApplyOutcome::Reply(InboundReplyOutcome::Applied { .. })
            ),
            "got {outcome:?}"
        );
        assert_eq!(resolver.asked(), 1, "bob alone is resolved");
        assert_eq!(rec.count("fauna.bridges.put_event_ciphertext"), 1);
        // One walk of the calendars serves the lookup, the check and the merge.
        assert_eq!(rec.count("fauna.bridges.list_calendars"), 1);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_refused_reply_walks_the_calendars_once() {
        // The refusal names the stored event it was about from the SAME lookup
        // that gated the check — never a second walk.
        let uid = "inv-rep-auth@example.com";
        let (outcome, rec) = apply_reply_to_stored(
            uid,
            &imip_bytes(ITipMethod::Reply, uid, "ACCEPTED"),
            &mallory_origin(),
            &fan_resolver(),
        );
        assert!(
            matches!(
                outcome,
                SchedulingApplyOutcome::Reply(InboundReplyOutcome::Refused { .. })
            ),
            "got {outcome:?}"
        );
        assert_eq!(rec.count("fauna.bridges.list_calendars"), 1);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_reply_from_someone_other_than_its_attendee_is_refused() {
        // mallory holds the UID and bob's address; the nest attests HER as the
        // poster, so she cannot flip bob's PARTSTAT.
        let (outcome, puts) = apply_bobs_reply(&mallory_origin(), &fan_resolver());
        assert_eq!(
            outcome,
            SchedulingApplyOutcome::Reply(InboundReplyOutcome::Refused {
                // The refusal names the stored event, so the row the user
                // reads can say WHICH event someone tried to answer for.
                uid_hash: uid_hash("inv-rep-auth@example.com"),
                summary: "Invite".into(),
                reason: RefusalReason::NotTheAttendee,
            })
        );
        assert_eq!(puts, 0, "the organizer's stored event is left untouched");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_reply_whose_attendee_resolves_to_no_one_is_refused() {
        // Discovery gives no answer for bob (an outage, or not a Fauna actor):
        // nothing shows the origin speaks for him, so the REPLY is refused —
        // *no answer means refuse*, the same as an unbound event's mutation.
        let (outcome, puts) = apply_bobs_reply(&bob_origin(), &NoPrincipalResolver);
        assert_eq!(
            outcome,
            SchedulingApplyOutcome::Reply(InboundReplyOutcome::Refused {
                // The refusal names the stored event, so the row the user
                // reads can say WHICH event someone tried to answer for.
                uid_hash: uid_hash("inv-rep-auth@example.com"),
                summary: "Invite".into(),
                reason: RefusalReason::AttendeeUnresolvable,
            })
        );
        assert_eq!(puts, 0, "the organizer's stored event is left untouched");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_reply_with_no_attested_author_is_refused() {
        // The home nest attests no poster: no answer, so no mutation.
        let origin = InboundOrigin {
            author: None,
            home_nest_url: String::new(),
        };
        let (outcome, puts) = apply_bobs_reply(&origin, &fan_resolver());
        assert_eq!(
            outcome,
            SchedulingApplyOutcome::Reply(InboundReplyOutcome::Refused {
                // The refusal names the stored event, so the row the user
                // reads can say WHICH event someone tried to answer for.
                uid_hash: uid_hash("inv-rep-auth@example.com"),
                summary: "Invite".into(),
                reason: RefusalReason::NoAttestedAuthor,
            })
        );
        assert_eq!(puts, 0, "the organizer's stored event is left untouched");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_reply_from_its_own_attendee_on_the_home_nest_that_resolved_applies() {
        // bob's REPLY relayed from his own (foreign) nest: the origin's home is
        // the peer URL, and bob resolves to exactly that pair.
        let mut m = BTreeMap::new();
        m.insert(
            "bob@example.com".to_string(),
            SchedulingPrincipal {
                actor_id: BOB_ACTOR.into(),
                home_nest_url: "https://example.com".into(),
            },
        );
        let origin = InboundOrigin {
            author: Some(BOB_ACTOR.into()),
            home_nest_url: "https://example.com/".into(),
        };
        let (outcome, puts) = apply_bobs_reply(&origin, &MapResolver(m.clone()));
        assert!(
            matches!(
                outcome,
                SchedulingApplyOutcome::Reply(InboundReplyOutcome::Applied { .. })
            ),
            "got {outcome:?}"
        );
        assert_eq!(puts, 1);
        // The same actor speaking from a different nest is not bob.
        let elsewhere = InboundOrigin {
            home_nest_url: String::new(),
            ..origin
        };
        let (outcome, puts) = apply_bobs_reply(&elsewhere, &MapResolver(m));
        assert_eq!(
            outcome,
            SchedulingApplyOutcome::Reply(InboundReplyOutcome::Refused {
                // The refusal names the stored event, so the row the user
                // reads can say WHICH event someone tried to answer for.
                uid_hash: uid_hash("inv-rep-auth@example.com"),
                summary: "Invite".into(),
                reason: RefusalReason::NotTheAttendee,
            })
        );
        assert_eq!(puts, 0);
    }

    // ── The mail rail: a mailed REPLY speaks only for its stamped sender ────
    // (caldav-server.md § Who may mutate an existing event over the inbound
    // rail → *The mail rail*.) The sealed copy's `X-Fauna-Authenticated-Sender`
    // stamp — written by the delivery door, never by the sender — is the only
    // evidence of who sent it.

    /// Prepend the delivery door's authenticated-sender stamp for `sender` to
    /// `raw`, exactly as a door does (the one builder, so the fixture cannot
    /// drift from the grammar the reader parses).
    #[cfg(not(target_arch = "wasm32"))]
    fn stamped(sender: &str, raw: &[u8]) -> Vec<u8> {
        let line = fauna_mail::sender_auth::build_authenticated_sender_stamp(sender.into());
        assert!(!line.is_empty(), "fixture sender must be stampable");
        let mut out = format!("{line}\r\n").into_bytes();
        out.extend_from_slice(raw);
        out
    }

    /// Apply the raw mailed `REPLY` `raw` to an organizer store holding one
    /// event under `stored_uid` (bob its only attendee, NEEDS-ACTION) over the
    /// mail rail. Returns the outcome and the requester.
    #[cfg(not(target_arch = "wasm32"))]
    fn apply_mailed_reply_to_stored(
        stored_uid: &str,
        raw: &[u8],
    ) -> (InboundReplyOutcome, std::sync::Arc<ScheduleRequester>) {
        let cal_c = [0xC6u8; 32];
        let mut events = std::collections::BTreeMap::new();
        events.insert(
            cal_c.to_vec(),
            vec![sealed_entry_for_uid(&request_ics(stored_uid), stored_uid)],
        );
        let rec = std::sync::Arc::new(ScheduleRequester::new(vec![cal_entry(&cal_c)], events));
        let outcome = block_on(
            CalDavClient::new(rec.clone()).apply_inbound_reply_from_mail(
                &ACTOR,
                &MSEK,
                raw,
                1_700_000_000,
            ),
        )
        .expect("apply mailed reply");
        (outcome, rec)
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_mailed_reply_stamped_by_its_own_attendee_applies() {
        let uid = "inv-mail@example.com";
        // The door's stamp is case-insensitive against the roster address.
        let raw = stamped(
            "Bob@example.com",
            &imip_bytes(ITipMethod::Reply, uid, "ACCEPTED"),
        );
        let (outcome, rec) = apply_mailed_reply_to_stored(uid, &raw);
        match outcome {
            InboundReplyOutcome::Applied {
                uid_hash: u,
                attendees,
            } => {
                assert_eq!(u, uid_hash(uid));
                assert!(
                    attendees
                        .iter()
                        .any(|a| a.email == "bob@example.com" && a.partstat == "ACCEPTED"),
                    "bob's PARTSTAT merged, got {attendees:?}"
                );
            }
            other => panic!("expected Applied, got {other:?}"),
        }
        assert_eq!(rec.count("fauna.bridges.put_event_ciphertext"), 1);
        assert_eq!(outcome_record_none(&raw), None);
    }

    /// A non-refusal is never a record.
    #[cfg(not(target_arch = "wasm32"))]
    fn outcome_record_none(raw: &[u8]) -> Option<fauna_core::data::RefusedSchedulingChange> {
        InboundReplyOutcome::NoMatchingEvent.refused_mail_change_record(raw, 1)
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_mailed_reply_stamped_by_someone_else_is_refused_and_names_that_sender() {
        // mallory holds the UID and writes bob's ATTENDEE line — and even a
        // forged stamp further down (the door would have stripped it; here it
        // sits below the genuine one, which wins). The door authenticated HER.
        let uid = "inv-mail@example.com";
        let forged = stamped(
            "bob@example.com",
            &imip_bytes(ITipMethod::Reply, uid, "ACCEPTED"),
        );
        let raw = stamped("mallory@example.com", &forged);
        let (outcome, rec) = apply_mailed_reply_to_stored(uid, &raw);
        assert_eq!(
            outcome,
            InboundReplyOutcome::Refused {
                uid_hash: uid_hash(uid),
                summary: "Invite".into(),
                reason: RefusalReason::NotTheAttendee,
            }
        );
        assert_eq!(
            rec.count("fauna.bridges.put_event_ciphertext"),
            0,
            "the organizer's stored event is left untouched"
        );
        assert_eq!(rec.count("fauna.bridges.list_calendars"), 1);

        let row = outcome
            .refused_mail_change_record(&raw, 1_700_000_500)
            .expect("a refusal is a record");
        assert_eq!(row.author, None, "the mail rail names no actor");
        assert_eq!(row.author_home_nest_url, "");
        assert_eq!(row.sender_address, "mallory@example.com");
        assert_eq!(row.method, "REPLY");
        assert_eq!(row.reason, "not_the_attendee");
        assert_eq!(row.summary, "Invite");
        assert_eq!(row.uid_hash, hex::encode(uid_hash(uid)));
        assert_eq!(row.last_refused_at, 1_700_000_500);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_mailed_reply_with_no_stamp_is_refused_as_unauthenticated() {
        // Filed by a door that authenticated nobody (APPEND, import, an
        // off-domain `From:`): no answer, and no answer refuses.
        let uid = "inv-mail@example.com";
        let raw = imip_bytes(ITipMethod::Reply, uid, "ACCEPTED");
        let (outcome, rec) = apply_mailed_reply_to_stored(uid, &raw);
        assert_eq!(
            outcome,
            InboundReplyOutcome::Refused {
                uid_hash: uid_hash(uid),
                summary: "Invite".into(),
                reason: RefusalReason::SenderUnauthenticated,
            }
        );
        assert_eq!(rec.count("fauna.bridges.put_event_ciphertext"), 0);
        let row = outcome
            .refused_mail_change_record(&raw, 1)
            .expect("a refusal is a record");
        assert_eq!(row.author, None);
        assert_eq!(row.sender_address, "", "an unstamped refusal names nobody");
        assert_eq!(row.reason, "sender_unauthenticated");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_mailed_reply_that_can_change_nothing_is_a_no_op_not_a_refusal() {
        let uid = "inv-mail@example.com";
        // Unknown UID, even unstamped: looked up first, nothing to refuse.
        let (outcome, rec) = apply_mailed_reply_to_stored(
            uid,
            &imip_bytes(ITipMethod::Reply, "no-such@example.com", "ACCEPTED"),
        );
        assert_eq!(outcome, InboundReplyOutcome::NoMatchingEvent);
        assert_eq!(rec.count("fauna.bridges.put_event_ciphertext"), 0);

        // Off-roster only, stamped by a stranger: the merge ignores the line.
        let (outcome, rec) = apply_mailed_reply_to_stored(
            uid,
            &stamped(
                "mallory@example.com",
                &imip_bytes_for(ITipMethod::Reply, uid, &["mallory@example.com"], "ACCEPTED"),
            ),
        );
        assert_eq!(outcome, InboundReplyOutcome::NoMatchingAttendee);
        assert_eq!(rec.count("fauna.bridges.put_event_ciphertext"), 0);

        // bob's stored answer repeated back, unstamped: nothing would change.
        let (outcome, rec) =
            apply_mailed_reply_to_stored(uid, &imip_bytes(ITipMethod::Reply, uid, "NEEDS-ACTION"));
        assert_eq!(outcome, InboundReplyOutcome::NoMatchingAttendee);
        assert_eq!(rec.count("fauna.bridges.put_event_ciphertext"), 0);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_mailed_reply_speaking_for_its_sender_and_another_rostered_attendee_is_refused() {
        // bob's own stamp may answer for bob alone — not also for carol.
        let uid = "inv-mail-2@example.com";
        let cal_c = [0xC7u8; 32];
        let stored = {
            let event = EventFields {
                summary: "Pair".into(),
                dtstart: "2026-07-01T15:00:00Z".into(),
                uid: uid.into(),
                ..Default::default()
            };
            let roster = ["bob@example.com", "carol@example.com"]
                .iter()
                .map(|e| AttendeeInfo {
                    email: (*e).into(),
                    partstat: "NEEDS-ACTION".into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>();
            generate_itip(
                ITipMethod::Request,
                &event,
                &roster,
                "alice@example.com",
                "2026-06-13T12:00:00Z",
            )
        };
        let mut events = std::collections::BTreeMap::new();
        events.insert(cal_c.to_vec(), vec![sealed_entry_for_uid(&stored, uid)]);
        let rec = std::sync::Arc::new(ScheduleRequester::new(vec![cal_entry(&cal_c)], events));
        let raw = stamped(
            "bob@example.com",
            &imip_bytes_for(
                ITipMethod::Reply,
                uid,
                &["bob@example.com", "carol@example.com"],
                "DECLINED",
            ),
        );
        let outcome = block_on(
            CalDavClient::new(rec.clone()).apply_inbound_reply_from_mail(
                &ACTOR,
                &MSEK,
                &raw,
                1_700_000_000,
            ),
        )
        .expect("apply mailed reply");
        assert!(
            matches!(
                outcome,
                InboundReplyOutcome::Refused {
                    reason: RefusalReason::NotTheAttendee,
                    ..
                }
            ),
            "got {outcome:?}"
        );
        assert_eq!(rec.count("fauna.bridges.put_event_ciphertext"), 0);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn ordinary_mail_is_not_a_calendar_reply_on_the_mail_rail() {
        let (outcome, rec) = apply_mailed_reply_to_stored(
            "inv-mail@example.com",
            b"From: a@x\r\nTo: b@y\r\nSubject: hi\r\n\r\njust a note\r\n",
        );
        assert_eq!(outcome, InboundReplyOutcome::NotCalendarReply);
        assert_eq!(rec.count("fauna.bridges.list_calendars"), 0);
    }

    #[test]
    fn sender_unauthenticated_round_trips_its_wire_token() {
        assert_eq!(
            RefusalReason::SenderUnauthenticated.as_wire(),
            "sender_unauthenticated"
        );
        assert_eq!(
            RefusalReason::from_wire("sender_unauthenticated"),
            Some(RefusalReason::SenderUnauthenticated)
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn scheduling_from_message_non_calendar_is_noop() {
        // Ordinary mail (no text/calendar part) → NotScheduling, zero RPCs.
        let rec = std::sync::Arc::new(ScheduleRequester::new(
            vec![],
            std::collections::BTreeMap::new(),
        ));
        let client = CalDavClient::new(rec.clone());
        let raw = b"From: a@x\r\nTo: b@y\r\nSubject: hi\r\n\r\njust a note\r\n".to_vec();

        let outcome = block_on(client.apply_inbound_scheduling_from_message(
            &ACTOR,
            &MSEK,
            &raw,
            1_700_000_000,
            &organizer_origin(),
            &fan_resolver(),
        ))
        .expect("apply scheduling");

        assert_eq!(outcome, SchedulingApplyOutcome::NotScheduling);
        assert_eq!(rec.count("fauna.bridges.list_calendars"), 0);
        assert_eq!(rec.count("fauna.bridges.put_event_ciphertext"), 0);
    }

    // ── iMIP dispatch ──────────────────────────────────────────────────────

    fn meeting() -> (EventFields, Vec<AttendeeInfo>) {
        let event = EventFields {
            summary: "Sprint planning".into(),
            dtstart: "2026-07-01T15:00:00Z".into(),
            uid: "sprint-1@example.com".into(),
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

    fn as_text(msg: &ImipMessage) -> String {
        String::from_utf8(msg.raw_rfc5322.clone()).unwrap()
    }

    #[test]
    fn imip_request_targets_attendees_with_method_header() {
        let (event, attendees) = meeting();
        let msg = build_event_imip(
            ITipMethod::Request,
            &event,
            &attendees,
            "alice@example.com",
            "2026-06-04T12:00:00Z",
        )
        .expect("a roster yields a message");
        assert_eq!(msg.recipients, vec!["bob@example.com", "carol@example.com"]);
        let text = as_text(&msg);
        assert!(text.contains("From: alice@example.com\r\n"));
        assert!(text.contains("To: bob@example.com, carol@example.com\r\n"));
        assert!(text.contains("Subject: Invitation: Sprint planning\r\n"));
        assert!(text.contains("Content-Type: text/calendar; charset=UTF-8; method=REQUEST\r\n"));
        // The iTIP body rides in the part, with its own METHOD.
        assert!(text.contains("METHOD:REQUEST"));
        assert!(text.contains("UID:sprint-1@example.com"));
        assert!(text.contains("DTSTAMP:20260604T120000Z"));
    }

    #[test]
    fn imip_request_excludes_organizer_and_dedups() {
        let event = EventFields {
            summary: "Standup".into(),
            uid: "s@example.com".into(),
            ..Default::default()
        };
        let attendees = vec![
            AttendeeInfo {
                email: "alice@example.com".into(), // the organizer, self-listed
                partstat: "ACCEPTED".into(),
                ..Default::default()
            },
            AttendeeInfo {
                email: "bob@example.com".into(),
                partstat: "NEEDS-ACTION".into(),
                ..Default::default()
            },
            AttendeeInfo {
                email: "BOB@example.com".into(), // dup, different case
                partstat: "NEEDS-ACTION".into(),
                ..Default::default()
            },
        ];
        let msg = build_event_imip(
            ITipMethod::Request,
            &event,
            &attendees,
            "alice@example.com",
            "2026-06-04T12:00:00Z",
        )
        .expect("bob is reachable");
        assert_eq!(msg.recipients, vec!["bob@example.com"]);
    }

    #[test]
    fn imip_request_with_no_reachable_recipient_is_none() {
        let event = EventFields {
            uid: "solo@example.com".into(),
            ..Default::default()
        };
        // Only the organizer is on the roster → nobody to invite.
        let attendees = vec![AttendeeInfo {
            email: "alice@example.com".into(),
            ..Default::default()
        }];
        assert!(
            build_event_imip(
                ITipMethod::Request,
                &event,
                &attendees,
                "alice@example.com",
                "2026-06-04T12:00:00Z",
            )
            .is_none()
        );
        // Empty roster → also None.
        assert!(
            build_event_imip(
                ITipMethod::Request,
                &event,
                &[],
                "alice@example.com",
                "2026-06-04T12:00:00Z",
            )
            .is_none()
        );
    }

    #[test]
    fn imip_reply_goes_from_responder_to_organizer() {
        let event = EventFields {
            summary: "Sprint planning".into(),
            uid: "sprint-1@example.com".into(),
            ..Default::default()
        };
        let responder = vec![AttendeeInfo {
            name: "Bob".into(),
            email: "bob@example.com".into(),
            partstat: "ACCEPTED".into(),
            fauna_status: "going".into(),
        }];
        let msg = build_event_imip(
            ITipMethod::Reply,
            &event,
            &responder,
            "alice@example.com",
            "2026-06-04T12:30:00Z",
        )
        .expect("a reply targets the organizer");
        assert_eq!(msg.recipients, vec!["alice@example.com"]);
        let text = as_text(&msg);
        assert!(text.contains("From: bob@example.com\r\n"));
        assert!(text.contains("To: alice@example.com\r\n"));
        assert!(text.contains("Subject: Re: Sprint planning\r\n"));
        assert!(text.contains("Content-Type: text/calendar; charset=UTF-8; method=REPLY\r\n"));
        assert!(text.contains("METHOD:REPLY"));
        assert!(text.contains("REQUEST-STATUS:2.0;Success"));
    }

    #[test]
    fn imip_cancel_uses_cancel_method_and_subject() {
        let (mut event, attendees) = meeting();
        event.status = "cancelled".into();
        event.sequence = 1;
        let msg = build_event_imip(
            ITipMethod::Cancel,
            &event,
            &attendees,
            "alice@example.com",
            "2026-06-04T13:00:00Z",
        )
        .expect("attendees to notify");
        let text = as_text(&msg);
        assert!(text.contains("Subject: Cancelled: Sprint planning\r\n"));
        assert!(text.contains("Content-Type: text/calendar; charset=UTF-8; method=CANCEL\r\n"));
        assert!(text.contains("METHOD:CANCEL"));
        assert!(text.contains("STATUS:CANCELLED"));
    }

    // ── Slice 2: attendee transport resolution ─────────────────────────────
    //
    // The resolver's *policy* is tested against a static `MockDiscovery` (no
    // live nest): every branch of `resolve_attendee_transport`, the sidecar
    // populate, the address parse, and discovery-error propagation. The
    // production `AnonAttendeeDiscovery` (real anon TLS) is live-covered by the
    // Slice-6 tier_3 cross-nest e2e.

    /// A static `AttendeeDiscovery`: `actors` maps a *domain* → its resolved
    /// actor (absent ⇒ not a Fauna actor → email); `email_enabled` maps a
    /// *nest_url* → its flag (absent ⇒ `true`); `nest_ids` maps a *nest_url* →
    /// the `nest_id` its `fauna.nest.info` reports (absent ⇒ that hop errs).
    /// `fail` forces every call to err (the transport-fault path).
    #[cfg(not(target_arch = "wasm32"))]
    struct MockDiscovery {
        actors: std::collections::BTreeMap<String, DiscoveredActor>,
        email_enabled: std::collections::BTreeMap<String, bool>,
        nest_ids: std::collections::BTreeMap<String, String>,
        fail: bool,
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl MockDiscovery {
        fn new() -> Self {
            Self {
                actors: std::collections::BTreeMap::new(),
                email_enabled: std::collections::BTreeMap::new(),
                nest_ids: std::collections::BTreeMap::new(),
                fail: false,
            }
        }
        /// Register a Fauna actor on `domain`, reachable at `nest_url`, with the
        /// given `email_enabled`.
        fn with_actor(mut self, domain: &str, actor_id: &str, nest_url: &str, email: bool) -> Self {
            self.actors.insert(
                domain.to_string(),
                DiscoveredActor {
                    actor_id: actor_id.to_string(),
                    nest_url: nest_url.to_string(),
                    nest_id: self.nest_ids.get(nest_url).cloned(),
                },
            );
            self.email_enabled.insert(nest_url.to_string(), email);
            self
        }
        /// Give the nest reached at `nest_url` the identity `nest_id` — call
        /// before [`Self::with_actor`] so the actors it serves report it.
        fn with_nest(mut self, nest_url: &str, nest_id: &str) -> Self {
            self.nest_ids
                .insert(nest_url.to_string(), nest_id.to_string());
            self
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl AttendeeDiscovery for MockDiscovery {
        type Error = String;

        async fn resolve_actor(
            &self,
            domain: &str,
            _localpart: &str,
        ) -> Result<Option<DiscoveredActor>, String> {
            if self.fail {
                return Err("discovery transport fault".into());
            }
            Ok(self.actors.get(domain).cloned())
        }

        async fn nest_email_enabled(&self, nest_url: &str) -> Result<bool, String> {
            if self.fail {
                return Err("discovery transport fault".into());
            }
            Ok(*self.email_enabled.get(nest_url).unwrap_or(&true))
        }

        async fn nest_id(&self, nest_url: &str) -> Result<String, String> {
            if self.fail {
                return Err("discovery transport fault".into());
            }
            self.nest_ids
                .get(nest_url)
                .cloned()
                .ok_or_else(|| format!("no nest.info answer from {nest_url}"))
        }
    }

    // ── the address resolver: the recipient's own nest is recognised by
    //    identity (caldav-server.md § Who may mutate an existing event over the
    //    inbound rail — the rail reports a same-nest home as empty) ──────────

    /// This client's nest reached by a LAN address, while discovery for its
    /// public domain answers at the public URL — one nest, two spellings.
    #[cfg(not(target_arch = "wasm32"))]
    fn lan_client_discovery() -> MockDiscovery {
        let own_id = "0e".repeat(32);
        MockDiscovery::new()
            .with_nest("https://192.168.1.20:8443", &own_id)
            .with_nest("https://example.com", &own_id)
            .with_actor("example.com", ORGANIZER_ACTOR, "https://example.com", false)
            .with_nest("https://peer.example", &"9e".repeat(32))
            .with_actor("peer.example", MALLORY_ACTOR, "https://peer.example", false)
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn an_own_nest_actor_resolves_to_the_empty_home_whatever_url_reaches_the_nest() {
        let resolver = DiscoveryPrincipalResolver {
            discovery: lan_client_discovery(),
            own_nest_url: "https://192.168.1.20:8443".into(),
        };
        assert_eq!(
            block_on(resolver.resolve_principal("mailto:Alice@example.com")),
            Some(SchedulingPrincipal {
                actor_id: ORGANIZER_ACTOR.into(),
                home_nest_url: String::new(),
            }),
            "the nest that answered for example.com IS this client's nest (same \
             nest_id), so the principal carries the empty own-nest home the rail \
             reports — a URL comparison would have kept https://example.com"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_foreign_nest_actor_keeps_the_answering_nests_url() {
        let resolver = DiscoveryPrincipalResolver {
            discovery: lan_client_discovery(),
            own_nest_url: "https://192.168.1.20:8443".into(),
        };
        assert_eq!(
            block_on(resolver.resolve_principal("mallory@peer.example")),
            Some(SchedulingPrincipal {
                actor_id: MALLORY_ACTOR.into(),
                home_nest_url: "https://peer.example".into(),
            })
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn an_unknown_nest_identity_is_never_folded_onto_the_own_nest() {
        // The answering nest gave no nest.info answer: not folded, even though
        // this client reaches its own nest at that very URL — only a positive
        // identity match says "mine". The same on the other side: no identity
        // for the own nest → no fold.
        let no_found_id = DiscoveryPrincipalResolver {
            discovery: MockDiscovery::new()
                .with_actor("example.com", ORGANIZER_ACTOR, "https://example.com", false)
                .with_nest("https://example.com", &"0e".repeat(32)),
            own_nest_url: "https://example.com".into(),
        };
        assert_eq!(
            block_on(no_found_id.resolve_principal("alice@example.com")).map(|p| p.home_nest_url),
            Some("https://example.com".to_string())
        );
        let no_own_id = DiscoveryPrincipalResolver {
            discovery: MockDiscovery::new()
                .with_nest("https://example.com", &"0e".repeat(32))
                .with_actor("example.com", ORGANIZER_ACTOR, "https://example.com", false),
            own_nest_url: "https://192.168.1.20:8443".into(),
        };
        assert_eq!(
            block_on(no_own_id.resolve_principal("alice@example.com")).map(|p| p.home_nest_url),
            Some("https://example.com".to_string())
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn an_unresolvable_address_is_no_answer() {
        let resolver = DiscoveryPrincipalResolver {
            discovery: lan_client_discovery(),
            own_nest_url: "https://192.168.1.20:8443".into(),
        };
        assert_eq!(
            block_on(resolver.resolve_principal("carol@example.net")),
            None
        );
        assert_eq!(block_on(resolver.resolve_principal("not-an-address")), None);
        let down = DiscoveryPrincipalResolver {
            discovery: MockDiscovery {
                fail: true,
                ..lan_client_discovery()
            },
            own_nest_url: "https://192.168.1.20:8443".into(),
        };
        assert_eq!(block_on(down.resolve_principal("alice@example.com")), None);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn resolve_external_address_is_mail_reachable() {
        // No Fauna actor resolves for example.net → email (the universal layer).
        let disco = MockDiscovery::new();
        let out =
            block_on(resolve_attendee_transport(&disco, "carol@example.net")).expect("resolve");
        assert_eq!(out, AttendeeTransport::MailReachable);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn resolve_mail_enabled_fauna_handle_is_mail_reachable() {
        // A Fauna handle whose nest HAS email enabled → iMIP email, not WS-RPC.
        let disco = MockDiscovery::new().with_actor(
            "example.com",
            "aa".repeat(32).as_str(),
            "https://example.com",
            true,
        );
        let out =
            block_on(resolve_attendee_transport(&disco, "alice@example.com")).expect("resolve");
        assert_eq!(out, AttendeeTransport::MailReachable);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn resolve_mailboxless_fauna_handle_is_ws_rpc() {
        // A Fauna actor on an email-DISABLED nest is the only WS-RPC case.
        let actor = "bb".repeat(32);
        let disco = MockDiscovery::new().with_actor(
            "calonly.example",
            &actor,
            "https://calonly.example",
            false,
        );
        let out = block_on(resolve_attendee_transport(
            &disco,
            "mailto:Bob@CalOnly.Example",
        ))
        .expect("resolve");
        assert_eq!(
            out,
            AttendeeTransport::MailboxlessFauna {
                actor_id: actor,
                nest_url: "https://calonly.example".into(),
            }
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn resolve_malformed_address_errs() {
        let disco = MockDiscovery::new();
        for bad in ["bob", "@example.com", "bob@", "mailto:nope", ""] {
            match block_on(resolve_attendee_transport(&disco, bad)) {
                Err(ResolveError::MalformedAddress(a)) => assert_eq!(a, bad),
                other => panic!("expected MalformedAddress for {bad:?}, got {other:?}"),
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn resolve_propagates_discovery_error() {
        // A transport fault to a reachable nest surfaces as Discovery, never a
        // silent email fallback (the caller decides retry-vs-email).
        let disco = MockDiscovery {
            fail: true,
            ..MockDiscovery::new()
        };
        match block_on(resolve_attendee_transport(&disco, "alice@example.com")) {
            Err(ResolveError::Discovery(e)) => assert_eq!(e, "discovery transport fault"),
            other => panic!("expected Discovery error, got {other:?}"),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn resolve_attendee_nest_urls_populates_only_mailboxless() {
        // Mixed roster: a mail-enabled Fauna handle, a mailbox-less Fauna handle
        // (mixed-case, mailto:), and an external address. Only the mailbox-less
        // one lands in the sidecar, keyed by its NORMALIZED CAL-ADDRESS.
        let disco = MockDiscovery::new()
            .with_actor(
                "example.com",
                "aa".repeat(32).as_str(),
                "https://example.com",
                true,
            )
            .with_actor(
                "calonly.example",
                "bb".repeat(32).as_str(),
                "https://calonly.example",
                false,
            );
        let roster = vec![
            "alice@example.com".to_string(),
            "MAILTO:Bob@CalOnly.Example".to_string(),
            "carol@example.net".to_string(),
        ];
        let urls = block_on(resolve_attendee_nest_urls(&disco, &roster)).expect("resolve roster");
        assert_eq!(urls.len(), 1);
        assert_eq!(
            urls.get("bob@calonly.example").map(String::as_str),
            Some("https://calonly.example")
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn fauna_ext_nest_url_for_matches_case_insensitively() {
        let mut ext = FaunaEventExt::default();
        ext.attendee_nest_urls.insert(
            "bob@calonly.example".into(),
            "https://calonly.example".into(),
        );
        // Stored key matched through a mailto: + mixed-case lookup (mirrors
        // is_interested's normalization).
        assert_eq!(
            ext.nest_url_for("mailto:BOB@CalOnly.Example"),
            Some("https://calonly.example")
        );
        // An email-reachable attendee has no entry.
        assert_eq!(ext.nest_url_for("alice@example.com"), None);
    }

    // ── Slice 5: the organizer dispatch fork ───────────────────────────────
    //
    // The routing is tested against the same static `MockDiscovery` + a recording
    // `MockDispatch`: a mixed roster splits into one fanned email (the
    // email-reachable subset) plus per-attendee mailbox-less WS-RPC deliveries,
    // with `peer_domain` None same-nest / Some(domain) foreign. The production
    // `NestImipDispatch` (real `EmailClient` + `ConversationsSession`) is glue-crate
    // wired + covered by the Slice-6 tier_3 cross-nest e2e.

    /// A recording [`ImipDispatch`]: captures the one `send_email` call and every
    /// `deliver_mailboxless` call so the routing can be asserted. `fail_*` force a
    /// rail error (the best-effort collected-error path).
    /// Recorded `(actor_id, peer_domain, raw_rfc5322)` per mailbox-less delivery.
    #[cfg(not(target_arch = "wasm32"))]
    type MailboxlessCall = (String, Option<String>, Vec<u8>);

    #[cfg(not(target_arch = "wasm32"))]
    #[derive(Default)]
    struct MockDispatch {
        emails: std::sync::Mutex<Vec<(Vec<String>, Vec<u8>)>>,
        mailboxless: std::sync::Mutex<Vec<MailboxlessCall>>,
        fail_email: bool,
        fail_mailboxless: bool,
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl ImipDispatch for MockDispatch {
        type Error = String;

        async fn send_email(
            &self,
            recipients: Vec<String>,
            raw_rfc5322: Vec<u8>,
        ) -> Result<(), String> {
            if self.fail_email {
                return Err("email transport fault".into());
            }
            self.emails.lock().unwrap().push((recipients, raw_rfc5322));
            Ok(())
        }

        async fn deliver_mailboxless(
            &self,
            actor_id_hex: &str,
            peer_domain: Option<String>,
            raw_rfc5322: Vec<u8>,
        ) -> Result<(), String> {
            if self.fail_mailboxless {
                return Err("welcome deliver fault".into());
            }
            self.mailboxless.lock().unwrap().push((
                actor_id_hex.to_string(),
                peer_domain,
                raw_rfc5322,
            ));
            Ok(())
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn dispatch_roster(emails: &[&str]) -> Vec<AttendeeInfo> {
        emails
            .iter()
            .map(|e| AttendeeInfo {
                name: String::new(),
                email: (*e).to_string(),
                partstat: "NEEDS-ACTION".into(),
                fauna_status: "invited".into(),
            })
            .collect()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn dispatch_fields() -> EventFields {
        EventFields {
            summary: "Sprint planning".into(),
            dtstart: "2026-06-20T09:00:00Z".into(),
            dtend: "2026-06-20T10:00:00Z".into(),
            uid: "evt-disp@example.com".into(),
            status: "confirmed".into(),
            ..Default::default()
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn dispatch_forks_email_reachable_and_mailboxless() {
        // organizer on example.com (mail-enabled); roster = an external addr, a
        // mail-enabled Fauna handle, and a mailbox-less Fauna handle on a foreign
        // (email-disabled) nest → ONE email to {external, mail-enabled-fauna} +
        // ONE WS-RPC delivery to the mailbox-less one (peer_domain = Some, foreign).
        let actor = "bb".repeat(32);
        let disco = MockDiscovery::new()
            .with_actor(
                "example.com",
                "aa".repeat(32).as_str(),
                "https://example.com",
                true,
            )
            .with_actor("calonly.example", &actor, "https://calonly.example", false);
        let dispatch = MockDispatch::default();
        let roster = dispatch_roster(&[
            "carol@example.net",
            "alice@example.com",
            "bob@calonly.example",
        ]);
        let report = block_on(dispatch_imip_request(
            &disco,
            &dispatch,
            &dispatch_fields(),
            &roster,
            "org@example.com",
            1_700_000_000,
        ))
        .expect("dispatch");

        // One email, addressed to the two email-reachable attendees only.
        let emails = dispatch.emails.lock().unwrap();
        assert_eq!(emails.len(), 1, "exactly one fanned email");
        let (recipients, body) = &emails[0];
        assert_eq!(recipients.len(), 2);
        assert!(recipients.iter().any(|x| x == "carol@example.net"));
        assert!(recipients.iter().any(|x| x == "alice@example.com"));
        assert!(!recipients.iter().any(|x| x == "bob@calonly.example"));

        // One WS-RPC delivery to the mailbox-less attendee; foreign nest → Some.
        let mbl = dispatch.mailboxless.lock().unwrap();
        assert_eq!(mbl.len(), 1, "exactly one mailbox-less delivery");
        assert_eq!(mbl[0].0, actor, "the resolved actor id");
        assert_eq!(mbl[0].1.as_deref(), Some("calonly.example"));
        // The same iMIP bytes ride both rails (priority #2).
        assert_eq!(&mbl[0].2, body);

        assert!(report.email_sent);
        assert_eq!(report.email_recipients, 2);
        assert_eq!(report.mailboxless_delivered, 1);
        assert!(report.errors.is_empty());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn dispatch_mailboxless_same_nest_uses_no_peer_domain() {
        // A mailbox-less attendee on the organizer's OWN (email-disabled) domain is
        // same-nest → peer_domain = None (local key-package resolution).
        let actor = "cc".repeat(32);
        let disco = MockDiscovery::new().with_actor(
            "calonly.example",
            &actor,
            "https://calonly.example",
            false,
        );
        let dispatch = MockDispatch::default();
        let roster = dispatch_roster(&["bob@calonly.example"]);
        let report = block_on(dispatch_imip_request(
            &disco,
            &dispatch,
            &dispatch_fields(),
            &roster,
            // organizer also on calonly.example (a CalDAV-only, mail-off nest).
            "host@calonly.example",
            1_700_000_000,
        ))
        .expect("dispatch");

        assert!(dispatch.emails.lock().unwrap().is_empty(), "no email rail");
        let mbl = dispatch.mailboxless.lock().unwrap();
        assert_eq!(mbl.len(), 1);
        assert_eq!(mbl[0].1, None, "same-nest ⇒ peer_domain None");
        assert!(!report.email_sent);
        assert_eq!(report.email_recipients, 0);
        assert_eq!(report.mailboxless_delivered, 1);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn dispatch_collects_rail_errors_best_effort() {
        // A mailbox-less deliver fault is collected (the roster is already
        // persisted), not raised; the email rail still fans.
        let disco = MockDiscovery::new()
            .with_actor(
                "example.com",
                "aa".repeat(32).as_str(),
                "https://example.com",
                true,
            )
            .with_actor(
                "calonly.example",
                "bb".repeat(32).as_str(),
                "https://calonly.example",
                false,
            );
        let dispatch = MockDispatch {
            fail_mailboxless: true,
            ..MockDispatch::default()
        };
        let roster = dispatch_roster(&["alice@example.com", "bob@calonly.example"]);
        let report = block_on(dispatch_imip_request(
            &disco,
            &dispatch,
            &dispatch_fields(),
            &roster,
            "org@example.com",
            1_700_000_000,
        ))
        .expect("dispatch does not abort on a rail fault");
        assert!(
            report.email_sent,
            "email rail unaffected by the mailbox-less fault"
        );
        assert_eq!(report.mailboxless_delivered, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("bob@calonly.example"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn dispatch_no_attendees_is_empty_report() {
        // Only the organizer on the roster ⇒ imip_request_for_invite is None ⇒ an
        // empty report, no rail touched.
        let disco = MockDiscovery::new();
        let dispatch = MockDispatch::default();
        let roster = dispatch_roster(&["org@example.com"]);
        let report = block_on(dispatch_imip_request(
            &disco,
            &dispatch,
            &dispatch_fields(),
            &roster,
            "org@example.com",
            1_700_000_000,
        ))
        .expect("dispatch");
        assert_eq!(report, DispatchReport::default());
        assert!(dispatch.emails.lock().unwrap().is_empty());
        assert!(dispatch.mailboxless.lock().unwrap().is_empty());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn dispatch_propagates_resolve_error() {
        // A discovery transport fault aborts the whole batch (the caller decides
        // retry-vs-email), never a silent partial send.
        let disco = MockDiscovery {
            fail: true,
            ..MockDiscovery::new()
        };
        let dispatch = MockDispatch::default();
        let roster = dispatch_roster(&["alice@example.com"]);
        match block_on(dispatch_imip_request(
            &disco,
            &dispatch,
            &dispatch_fields(),
            &roster,
            "org@example.com",
            1_700_000_000,
        )) {
            Err(ResolveError::Discovery(e)) => assert_eq!(e, "discovery transport fault"),
            other => panic!("expected Discovery error, got {other:?}"),
        }
        assert!(dispatch.emails.lock().unwrap().is_empty());
    }
}
