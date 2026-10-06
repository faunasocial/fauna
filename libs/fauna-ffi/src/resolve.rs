//! UniFFI façade for shared recipient resolution. Three layers, all so no native
//! app (Apple / Windows / Android) re-implements addressing (or scope
//! resolution) in its own language the way the Rust-native Linux app reads it
//! straight from shared crates (priority #2/#4):
//!
//! 1. **Parsing** — [`classify_recipient`] over [`fauna_core::resolve::classify_recipient`]:
//!    the `64-hex actor-id | user@domain | invalid` split.
//! 2. **Network** — [`resolve_nest`] / [`resolve_handle`] over the anonymous
//!    [`AnonymousNestClient`]: the `fauna.nest.resolve` (domain → canonical node URL via
//!    the nest's SRV lookup) and `fauna.actor.by_handle` (handle → actor id on the owning
//!    nest) **pre-identity discovery kinds**, replacing per-app HTTP calls to the
//!    deleted `/api/v1/{resolve-node,actor/by-handle}` twins (api-layers.md discovery
//!    row; twins deleted once all apps adopt).
//!    Both connect anonymously to the target URL — uniform for the local nest *and* a
//!    cross-nest handle (`alice@other.example`), which needs a connection to the *other*
//!    nest — so there is no local-vs-remote branch in the client.
//! 3. **Scope** — [`resolve_calendar_selection`] over
//!    [`fauna_client_caldav::resolve_calendar_selection`]: the Events page's
//!    live-selection-vs-vanished-calendar fallback rule (events.md § Where logic
//!    lives → *"Which calendars the page is scoped to"*).
//!
//! Every export returns built-in scalars / `Vec<String>` (+ the crate-local
//! [`crate::FfiError`]), so no `fauna_core` / `fauna_protocol` custom type crosses the
//! boundary — free of the uniffi-bindgen-go bare-import footgun that gates `value_format`,
//! and the anonymous connector is already in the FFI build via `fauna-onboarding-machine`.

use fauna_anon_client::AnonymousNestClient;
use fauna_protocol::discovery::{NestResolveReply, NestResolveRequest};

use crate::FfiError;

/// UniFFI face of [`fauna_core::resolve::classify_recipient`] — classify a typed
/// "compose / find-user" recipient input into a flat `[kind, actor_id, user, domain]`
/// list. `kind` is `"actor_id"` (slot 1 = lowercased 64-hex), `"handle"` (slots 2,3 =
/// `user`,`domain`), or `"invalid"`; the slots not carried by that kind are empty.
/// Each app re-keys the positional list into a named recipient object in its thin
/// wrapper.
#[uniffi::export]
pub fn classify_recipient(input: String) -> Vec<String> {
    fauna_core::resolve::classify_recipient(&input).into_parts()
}

/// Resolve a domain to its canonical fauna node URL via the nest's SRV lookup —
/// the `fauna.nest.resolve` pre-identity kind on an anonymous connection to
/// `home_url` (the client's own nest, which performs the DNS). Replaces a
/// per-app `GET /api/v1/resolve-node/{domain}` against the deleted twin.
///
/// `async_runtime = "tokio"` (like every other network async export in this
/// crate — e.g. `classify_attendee_transport` below): `AnonymousNestClient::
/// connect` opens a real anon TLS/WS connection, whose tokio socket I/O panics
/// ("no reactor running") unless the future is driven on a tokio runtime,
/// which this attribute arranges.
#[fauna_uniffi_async::export]
pub async fn resolve_nest(home_url: String, domain: String) -> Result<String, FfiError> {
    let client = AnonymousNestClient::connect(&home_url)
        .await
        .map_err(|e| e.to_string())?;
    let reply: NestResolveReply = client
        .request(
            "fauna.nest.resolve",
            NestResolveRequest {
                domain,
                extra: Default::default(),
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(reply.url)
}

/// Resolve a handle to an actor on the nest that owns it — the
/// `fauna.actor.by_handle` pre-identity kind on an anonymous connection to
/// `node_url` (the URL [`resolve_nest`] returned; the local nest or a cross-nest
/// peer). Returns a flat `[actor_id, handle, domain]` (the `addresses` /
/// `addressable` discovery fields are dropped — no client renders them here).
/// Replaces a per-app `GET {node_url}/api/v1/actor/by-handle/{handle}` against
/// the deleted twin.
///
/// `domain` is the typed `@domain` qualifier from the find-user input (the
/// `Handle{user,domain}` classification). Pass it: slots 2,3 are then the TYPED
/// `handle` and `domain` — the dial names the peer, and the reply's own echo is
/// never read for identity (`foreign-handle-resolution.md` § Peer-auth model;
/// the rule lives in [`fauna_client_core::find_user`]) — and it travels as the
/// multi-domain qualifier, so a nest that does not serve it refuses with
/// `fauna.actor.domain_not_local` (`mail-multidomain.md` § Multi-domain handles
/// § Resolution) — surface it as a benign "not found on this domain". `None` (a
/// bare handle, resolved on the home nest) reports the nest's canonical/identity
/// domain.
///
/// `async_runtime = "tokio"` — same reason as `resolve_nest` above:
/// `AnonymousNestClient::connect` needs a driving tokio runtime or its socket
/// I/O panics.
#[fauna_uniffi_async::export]
pub async fn resolve_handle(
    node_url: String,
    handle: String,
    domain: Option<String>,
) -> Result<Vec<String>, FfiError> {
    let client = AnonymousNestClient::connect(&node_url)
        .await
        .map_err(|e| e.to_string())?;
    let found =
        fauna_client_core::find_user::find_user_by_handle(&client, &handle, domain.as_deref())
            .await
            .map_err(|e| e.to_string())?;
    Ok(vec![found.actor_id, found.handle, found.domain])
}

/// UniFFI face of [`fauna_client_caldav::resolve_attendee_transport`] with the
/// production [`AnonAttendeeDiscovery`] — classify ONE attendee CAL-ADDRESS into
/// the rail its server-side auto-schedule iMIP must ride (`caldav-server.md`
/// § Server-side auto-schedule), by **anonymous cross-nest discovery**: anon
/// `fauna.actor.by_handle` against the attendee's nest → that nest's
/// `fauna.setup.status.email_enabled`, connecting directly to the peer (uniform
/// for the local nest *and* a cross-nest handle, exactly like [`resolve_handle`]).
/// This is the *same* decision the native app send path makes via
/// `resolve_attendee_transport`; exposing it here gives the Go MDA gateway's
/// off-box-domain branch one source of truth for "email vs sealed per attendee"
/// (priority #2/#4) instead of re-coding the anon-`by_handle`→`setup.status`
/// decision in Go.
///
/// Returns the flat `[rail, actor_id, nest_url]` (`Vec<String>`, **not** a custom
/// Record — the bare-import footgun this module's header calls out): `rail` is
/// `"sealed"` for a mailbox-less Fauna attendee on an email-disabled peer nest
/// (slots 2,3 = the resolved actor id + that nest's base URL — the inputs the
/// caller's cross-nest keypackage-fetch + sealed delivery need), or `"email"` for
/// every other case (an external address, or a mail-enabled Fauna handle; slots
/// 2,3 empty). The caller degrades to the email rail on any `Err`.
///
/// `async_runtime = "tokio"` (like every other network async export — e.g.
/// `verify_inbound`): the resolver opens a real anon TLS/WS connection, whose tokio
/// socket I/O panics ("no reactor running") unless the future is driven on a tokio
/// runtime, which this attribute arranges.
#[fauna_uniffi_async::export]
pub async fn classify_attendee_transport(addr: String) -> Result<Vec<String>, FfiError> {
    use fauna_client_caldav::{
        AnonAttendeeDiscovery, AttendeeTransport, resolve_attendee_transport,
    };
    match resolve_attendee_transport(&AnonAttendeeDiscovery, &addr)
        .await
        .map_err(|e| e.to_string())?
    {
        AttendeeTransport::MailReachable => {
            Ok(vec!["email".to_string(), String::new(), String::new()])
        }
        AttendeeTransport::MailboxlessFauna { actor_id, nest_url } => {
            Ok(vec!["sealed".to_string(), actor_id, nest_url])
        }
    }
}

/// UniFFI face of [`fauna_client_caldav::resolve_calendar_selection`] (events.md
/// § Where logic lives → *"Which calendars the page is scoped to"*): resolve the
/// Events page's `calendar-item` selection against the calendars that actually
/// exist right now. Returns the selected id back unchanged when it is still
/// live, or `None` (the no-selection union) when it has vanished — deleted
/// locally, or by a CalDAV MUA against the same `bridge_caldav_*` store —
/// rather than stranding the page on a permanently blank list with no way back.
#[uniffi::export]
pub fn resolve_calendar_selection(
    selected: Option<String>,
    existing_ids: Vec<String>,
) -> Option<String> {
    fauna_client_caldav::resolve_calendar_selection(selected.as_deref(), &existing_ids)
        .map(str::to_string)
}

/// UniFFI face of [`fauna_client_caldav::calendar_is_displayed`] (events.md
/// § Where logic lives → *"Which calendars display"*): does an event on
/// `calendar_id` belong on the Events page right now? The whole page-scope
/// composition in one call — the staleness-resolved `calendar-item` selection
/// wins outright (visibility never applies to a selection); with no live
/// selection the `calendar-visibility` toggles filter the union, an **empty**
/// visible-set meaning "no filter" (the full union), never "hide everything".
/// Pass the *raw* stored selection — the staleness rule runs inside.
#[uniffi::export]
pub fn calendar_is_displayed(
    selected: Option<String>,
    existing_ids: Vec<String>,
    visible_calendars: Vec<String>,
    calendar_id: String,
) -> bool {
    fauna_client_caldav::calendar_is_displayed(
        selected.as_deref(),
        &existing_ids,
        &visible_calendars,
        &calendar_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_id_lowercased_in_slot_one() {
        let upper = "A".repeat(64);
        assert_eq!(
            classify_recipient(upper),
            vec![
                "actor_id".to_string(),
                "a".repeat(64),
                String::new(),
                String::new()
            ]
        );
    }

    #[test]
    fn named_handle_splits_into_slots_two_and_three() {
        assert_eq!(
            classify_recipient("alice@fauna.social".to_string()),
            vec![
                "handle".to_string(),
                String::new(),
                "alice".to_string(),
                "fauna.social".to_string()
            ]
        );
    }

    #[test]
    fn malformed_is_invalid() {
        assert_eq!(
            classify_recipient("justtext".to_string()),
            vec![
                "invalid".to_string(),
                String::new(),
                String::new(),
                String::new()
            ]
        );
    }

    #[test]
    fn calendar_selection_live_stays_selected() {
        assert_eq!(
            resolve_calendar_selection(
                Some("bb".to_string()),
                vec!["aa".to_string(), "bb".to_string()]
            ),
            Some("bb".to_string())
        );
    }

    #[test]
    fn calendar_selection_vanished_falls_back_to_union() {
        assert_eq!(
            resolve_calendar_selection(
                Some("gone".to_string()),
                vec!["aa".to_string(), "bb".to_string()]
            ),
            None
        );
    }

    #[test]
    fn calendar_selection_none_stays_union() {
        assert_eq!(
            resolve_calendar_selection(None, vec!["aa".to_string(), "bb".to_string()]),
            None
        );
    }
}
