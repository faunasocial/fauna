//! NIP-01 WebSocket relay endpoint with NIP-11 info and NIP-42 auth.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};

use fauna_bridge_nostr::filter::matches_any;
use fauna_bridge_nostr::nip01::{ClientMessage, RelayMessage};
use fauna_bridge_nostr::nip11;
use fauna_bridge_nostr::nip42;
use fauna_bridge_nostr::signing::verify_event;
use fauna_bridge_nostr::types::Filter;

use crate::nostr::bunker;
use crate::nostr::db;
use crate::nostr::store;
use crate::routes::AppState;
use crate::ws::{Beat, ServerHeartbeat};

/// Event broadcast to connected Nostr relay clients.
#[derive(Debug, Clone)]
pub struct NostrRelayEvent {
    /// The Nostr event JSON (already serialized).
    pub event_json: String,
    /// The pubkey of the author (hex).
    pub author_pubkey: String,
}

/// Per-recipient rate limiter for the unauthenticated gift-wrap (kind 1059)
/// inbox. Keyed by the recipient (`p`-tag) depositor pubkey hex.
pub type GiftWrapLimiter = governor::RateLimiter<
    String,
    governor::state::keyed::DashMapStateStore<String>,
    governor::clock::DefaultClock,
>;

/// Max unauthenticated gift wraps accepted **per recipient per minute** on the
/// `/nostr` inbox (slice B). A hard-coded Rust constant — never a configuration
/// surface (`docs/goal/ui/nostr.md` § No new configuration surface); advertised
/// via NIP-11. Generous for a real DM inbox while bounding a flood's **rate**
/// and nsec-decrypt cost — the *total* disk bound is a separate cap,
/// [`store::MAX_GIFT_WRAP_INBOX_PER_RECIPIENT`] (a sustained flood at this
/// rate alone would otherwise grow a recipient's inbox without limit).
/// Keyed **per recipient** (not per peer): reliable behind the SNI router (no
/// client-IP dependency) and it bounds the exact owned resource — the
/// depositor's inbox. Because the key space is exactly the box's local
/// depositors (a small, stable set), the limiter's DashMap is inherently
/// bounded and needs no GC sweeper (unlike the per-IP HTTP limiter), matching
/// the domain-keyed ActivityPub `inbox_limiter` precedent.
pub const GIFT_WRAP_INBOX_PER_RECIPIENT_PER_MINUTE: u32 = 30;

/// Max EVENT/REQ/COUNT messages per minute processed on a connection that has
/// not NIP-42-authenticated. A hard-coded constant, never a configuration
/// surface. The per-IP governor and connection limit gate only the HTTP
/// *upgrade*; inside an established socket the read loop would otherwise
/// process unauthenticated queries as fast as they arrive. AUTH is exempt so
/// a legitimate local user can always authenticate out of the budget; CLOSE
/// is exempt because it only frees state. Generous for real use: an
/// unauthenticated reader syncs an outbox with a handful of REQs, and
/// gift-wrap senders are separately bounded per recipient.
pub const UNAUTHED_MSGS_PER_MINUTE: u32 = 60;

/// Pre-query validation of a REQ/COUNT's filters: `Some(reason)` when the
/// request must be refused (NIP-01 `CLOSED`) **before** touching the store —
/// the loud half of the search caps (the structural half is
/// [`fauna_bridge_nostr::nip50::search_tokens`]'s truncation), plus the
/// filter-count cap ([`fauna_bridge_nostr::nip11::MAX_FILTERS_PER_REQ`] —
/// each filter is a store query, so an uncapped list amplifies).
pub fn req_reject_reason(filters: &[Filter]) -> Option<String> {
    use fauna_bridge_nostr::{nip11, nip50};
    if filters.len() > nip11::MAX_FILTERS_PER_REQ {
        return Some(format!(
            "invalid: too many filters (max {})",
            nip11::MAX_FILTERS_PER_REQ
        ));
    }
    for f in filters {
        if let Some(s) = &f.search
            && nip50::exceeds_search_caps(s)
        {
            return Some(format!(
                "invalid: search query exceeds relay caps ({} bytes / {} terms)",
                nip50::MAX_SEARCH_BYTES,
                nip50::MAX_SEARCH_TOKENS
            ));
        }
    }
    None
}

/// Whether any filter *explicitly* asks for kind-1059 gift wraps — the trigger
/// for the NIP-42 `CLOSED auth-required:` on-demand-auth signal on an unauthed
/// REQ/COUNT. Deliberately not "could match 1059": a kindless filter is a
/// normal public read (the silent recipient gate covers it); only a client
/// naming kind 1059 is asking for a DM inbox and needs to be told to AUTH.
pub fn req_asks_for_gift_wraps(filters: &[Filter]) -> bool {
    filters
        .iter()
        .any(|f| f.kinds.as_deref().is_some_and(|ks| ks.contains(&1059)))
}

/// Build the unauthenticated gift-wrap inbox limiter with the hard-coded quota.
pub fn new_gift_wrap_limiter() -> GiftWrapLimiter {
    let quota = governor::Quota::per_minute(
        std::num::NonZeroU32::new(GIFT_WRAP_INBOX_PER_RECIPIENT_PER_MINUTE).unwrap(),
    );
    governor::RateLimiter::dashmap(quota)
}

/// Per-signer rate limiter for the unauthenticated kind-24133 bunker
/// carve-out, keyed by the target signer pubkey — the same shape as the
/// gift-wrap limiter (the key space is the box's registered bunker signers,
/// a small stable set, so the DashMap is inherently bounded).
pub type BunkerLimiter = GiftWrapLimiter;

/// Build the bunker request limiter with the hard-coded quota
/// ([`bunker::BUNKER_REQS_PER_MINUTE`]).
pub fn new_bunker_limiter() -> BunkerLimiter {
    let quota = governor::Quota::per_minute(
        std::num::NonZeroU32::new(bunker::BUNKER_REQS_PER_MINUTE).unwrap(),
    );
    governor::RateLimiter::dashmap(quota)
}

/// GET /nostr — WebSocket upgrade for Nostr clients, and (NIP-11's same-URI
/// rule) the relay information document for a plain GET with
/// `Accept: application/nostr+json`. Real clients resolve NIP-11 from the
/// WebSocket URI itself — `/nostr/info` alone is invisible to them (the N2
/// interop harness caught this: 400 on the fetch every real client makes).
pub async fn ws_handler(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
    // The serving gate (R8 (account-data-plane.md § The ratified decisions)): serve the relay where a user deposited a key
    // (this box is itself a head) OR where a paired head holds a `nostr_push`
    // pairing (this box is its keyless public serving face). Serving ≠ agency
    // — the relay signs/unwraps/seals nothing here; that stays nsec-only
    // (`nostr.md` § The bridging gate → Phase 2).
    if !crate::nostr::nostr_serving_available(&state).await {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "nostr relay unavailable: no Nostr key deposited and no paired head to serve for",
        )
            .into_response();
    }
    let ws = match ws {
        Ok(ws) => ws,
        Err(rejection) => {
            let wants_nip11 = headers
                .get("accept")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|a| a.contains("application/nostr+json"));
            if wants_nip11 {
                return nip11_info_response(&state);
            }
            return rejection.into_response();
        }
    };
    // Transport caps: axum's default WS message limit is ~64 MiB — an
    // unauthenticated frame-size DoS surface (see `nip11::MAX_WS_MESSAGE_BYTES`,
    // shared with the outbound relay client). The HTTP-body `DefaultBodyLimit`
    // layer does not bound WS frames — this does.
    ws.max_message_size(nip11::MAX_WS_MESSAGE_BYTES)
        .max_frame_size(nip11::MAX_WS_MESSAGE_BYTES)
        // (clv, row 89) Generation-scope the whole connection future. Of the
        // six WS upgrade families on this nest this is the one whose duration
        // is not merely *peer*-controlled but **stranger**-controlled: `/nostr`
        // faces arbitrary third-party clients, each free to hold an idle
        // subscription open for as long as it likes. Nothing else reaches it —
        // a relay connection is not a `state.ws` client (no 1001-drain) and not
        // plain HTTP (no idle timeout) — so unscoped, one such client is enough
        // to keep a superseded generation's `Arc<AppState>` alive indefinitely
        // after a deployment-seed rotation.
        .on_upgrade(move |socket| {
            let scope = Arc::clone(&state);
            async move {
                let _ = scope.spawn_scoped(handle_relay_ws(state, socket)).await;
            }
        })
}

/// GET /nostr/info — NIP-11 relay information document (convenience route;
/// the conformance surface is the same-URI serve in `ws_handler`).
pub async fn info_handler(State(state): State<Arc<AppState>>) -> Response {
    // Same serving gate as `ws_handler` (R8): no NIP-11 doc until either a key
    // is deposited on the box or a paired head holds a `nostr_push` pairing.
    if !crate::nostr::nostr_serving_available(&state).await {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "nostr relay unavailable: no Nostr key deposited and no paired head to serve for",
        )
            .into_response();
    }
    nip11_info_response(&state)
}

/// The NIP-11 document response both serving routes share.
fn nip11_info_response(state: &AppState) -> Response {
    // Claim-refreshed identity domain, not the `node.domain` boot seed: a
    // provisioned box boots domainless, so the seed made every fresh nest
    // advertise the hard-coded `fauna.social` placeholder as its own relay name
    // until a restart.
    let domain = state.handle_domain_if_set();
    let info = nip11::fauna_relay_info(
        domain.as_deref().unwrap_or("fauna.social"),
        env!("CARGO_PKG_VERSION"),
    );
    (
        StatusCode::OK,
        [("content-type", "application/nostr+json")],
        info.to_json(),
    )
        .into_response()
}

/// A rejecting NIP-01 `OK false <message>` reply for an event.
fn reject_ok(event_id: &str, message: &str) -> RelayMessage {
    RelayMessage::Ok {
        event_id: event_id.to_string(),
        accepted: false,
        message: message.to_string(),
    }
}

/// Handle an unauthenticated kind-1059 (NIP-17) gift-wrap deposit — the one
/// unauthenticated write the `/nostr` relay accepts (`docs/goal/ui/nostr.md`
/// § The relay event store, read/write policy + inbox role). Accepted only when
/// the wrap's `p` tag names a local **depositor** (an account that deposited an
/// nsec) and both the per-recipient rate limit and total-inbox cap have room.
/// An accepted wrap is stored
/// verbatim (so Nostr clients can fetch it), broadcast live (recipient-gated at
/// the receiver via [`store::gift_wrap_visible_to`]), and fed to the shared S8.9
/// seal-at-rest seam so the owner-readable plaintext never rests. Returns the
/// `OK` to send back to the sender. Public so the slice-B tier_3 integration
/// test can drive the accept path with a real `AppState` (there is no in-crate
/// WebSocket harness).
pub async fn handle_gift_wrap_inbox(
    state: &Arc<AppState>,
    event: fauna_bridge_nostr::types::Event,
) -> RelayMessage {
    let event_id = event.id.clone();

    // The gift wrap's `p` tag identifies the recipient pubkey.
    let recipient = event.tags.iter().find_map(|t| {
        if t.name() == Some("p") {
            t.value().map(|v| v.to_string())
        } else {
            None
        }
    });
    let recipient = match recipient {
        Some(r) => r,
        None => return reject_ok(&event_id, "invalid: gift wrap has no p tag"),
    };

    // Size limit (advertised via NIP-11 `max_message_length`). Membership-
    // neutral (a property of the event, not the recipient) and it bounds the
    // signature-verification cost below.
    let wire = serde_json::to_string(&event).unwrap_or_default();
    if wire.len() > store::MAX_EVENT_SIZE {
        return reject_ok(&event_id, "invalid: event exceeds max size");
    }

    // Verify the gift wrap's signature (its ephemeral one-time key) BEFORE any
    // recipient-specific check. No timestamp window: NIP-59 randomizes
    // gift-wrap `created_at` to obscure timing, so a ±window would wrongly
    // reject valid wraps. **Ordering is load-bearing for privacy:** the
    // depositor-membership gate below returns a distinct `restricted` reject,
    // so were it to run first an *unsigned* junk probe could decide "is pubkey
    // X a local nsec-depositor here?" from the reject reason alone
    // (network-exposure.md § Rulings F4). Requiring a valid signature first
    // means membership is never decidable from an unsigned event.
    if !verify_event(&event) {
        return reject_ok(&event_id, "invalid: bad signature");
    }

    // Scope gate (R8): accept for a local inbox — either a *depositor*
    // (encrypted_privkey set: this box holds the nsec and acts as the agent) or
    // a *proxied* account (signing_mode='proxied', R9) whose actor still holds a
    // non-expired `nostr_push` pairing (the head holds the nsec; this keyless
    // public box serves as the account's inbox and stores the wrap verbatim —
    // the seal seam below self-gates keyless, so no plaintext rests here). A
    // wrap addressed to any other pubkey is not our inbox — reject like any
    // other unauthorized write. Runs only for a validly-signed event (see the
    // ordering note above), so the membership it reveals is not decidable from
    // an unsigned probe, and both misses share the one `restricted:` reason.
    let conn = state.db.conn().await;
    let account = db::get_account_by_pubkey(&conn, &recipient).ok().flatten();
    drop(conn);
    let is_local_inbox = match &account {
        Some(a) if a.encrypted_privkey.is_some() => true,
        Some(a) if a.signing_mode == "proxied" => match hex::decode(&a.actor_id) {
            Ok(actor) => state
                .db
                .actor_has_pairing_with_capability(
                    &actor,
                    fauna_protocol::pair::capability::NOSTR_PUSH,
                )
                .await
                .unwrap_or(false),
            Err(_) => false,
        },
        _ => false,
    };
    if !is_local_inbox {
        return reject_ok(&event_id, "restricted: recipient is not a local inbox");
    }

    // Per-recipient rate limit (a hard-coded const, no config surface). Bounds
    // the unauthenticated inbox's flood rate + nsec-decrypt cost — NOT its
    // total disk (a sustained flood at this rate alone still grows the inbox
    // without bound); the total cap is the separate check right below.
    if let Some(ref limiter) = state.nostr.gift_wrap_limiter
        && limiter.check_key(&recipient).is_err()
    {
        return reject_ok(
            &event_id,
            "rate-limited: too many gift wraps for this recipient",
        );
    }

    // Per-recipient TOTAL cap (a hard-coded const, no config surface). The
    // rate limit above bounds flood *rate*; `MAX_EVENTS_PER_ACCOUNT` cannot
    // help here (gift wraps carry a fresh random sender pubkey per NIP-59
    // send, so there is no stable "account" to count against) — this is what
    // actually bounds the depositor's owned disk.
    let conn = state.db.conn().await;
    let inbox_full = store::gift_wrap_inbox_full(&conn, &recipient);
    drop(conn);
    if matches!(inbox_full, Ok(true)) {
        return reject_ok(&event_id, "rate-limited: inbox is full");
    }

    // Store verbatim (kind 1059 is a Regular class → stored as-is; its `p` tag
    // is indexed so the recipient-gated REQ path can find it). `derived=false`
    // — a genuinely received event, not a Fauna-post materialization.
    let conn = state.db.conn().await;
    let outcome = store::store_event(&conn, &event, false);
    drop(conn);
    match outcome {
        Ok(store::StoreOutcome::Expired) => {
            return reject_ok(&event_id, "invalid: event is expired");
        }
        Ok(store::StoreOutcome::CapExceeded) => {
            return reject_ok(&event_id, "rate-limited: store capacity reached");
        }
        Ok(o) => {
            if o.is_newly_stored() {
                // Broadcast newly-stored wraps to live subscribers. The
                // receiver applies the recipient gate, so only the authed
                // recipient's subscription actually gets it.
                let _ = state.nostr.relay_tx.send(NostrRelayEvent {
                    event_json: wire,
                    author_pubkey: event.pubkey.clone(),
                });
            } else {
                // Duplicate/Superseded: this wrap was already processed once.
                // Do NOT re-run the seal seam — each re-run minted another
                // sealed DM row the events-side caps can't see and duplicated the DM
                // in the owner's inbox. (The seam also dedupes on
                // the wrap's event id for its other callers.)
                return RelayMessage::Ok {
                    event_id,
                    accepted: true,
                    message: "duplicate: already have this event".to_string(),
                };
            }
        }
        Err(e) => {
            tracing::warn!("nostr relay: store gift wrap failed: {e}");
            return reject_ok(&event_id, "error: could not store event");
        }
    }

    // Feed the shared S8.9 seal-at-rest seam (unwrap → D2 seal → the bridged
    // family's deposit;
    // plaintext discarded, fail-closed). A seal failure there does not un-accept
    // the stored wrap — it stays fetchable from the relay like on any relay.
    crate::nostr::sync_worker::process_gift_wrap_inbound(
        &state.db,
        &state.nest_identity.signing_key.to_bytes(),
        event,
    )
    .await;

    RelayMessage::Ok {
        event_id,
        accepted: true,
        message: String::new(),
    }
}

/// Handle an unauthenticated kind-9735 (NIP-57) zap receipt — the ratified
/// category-2 acceptance for zap receipts targeting a local user's events
/// (`nostr.md` § The relay event store, owner-only scope: "events addressed
/// to a local pubkey — … plus reactions/mentions/**zap receipts targeting
/// local users' events**").
///
/// This *must* be an unauthenticated-write carve-out rather than a
/// NIP-42-authed local-author write: a zap receipt is authored by the payee's
/// LNURL/wallet server, which is by construction not a local account, so the
/// generic owner-only gate below rejects every one of them with
/// `restricted:`. That is why this half of category (2) stood unimplemented
/// since ratification while the kind-1059 half shipped.
///
/// **The trust gate is what makes accepting it safe** (`monetization.md`
/// § Zap receipts — the trust model): a kind-9735 is plain signed JSON anyone
/// may mint naming any recipient, and its `bolt11` is never checked against a
/// real Lightning payment — so a valid signature buys it nothing. Only a
/// receipt whose signer the `p`-tagged payee **designated** is stored. A payee
/// who has designated nobody believes nobody.
///
/// **Ordering is load-bearing for privacy (`network-exposure.md` § Rulings
/// F4), mirroring [`handle_gift_wrap_inbox`]:** size then signature-verify
/// run BEFORE the trust lookup, so an *unsigned* probe is rejected
/// identically whether or not the target pubkey is a local account — local
/// membership is never decidable from an unsigned event. For the same reason
/// **every** untrusted verdict collapses to one `restricted:` message: were
/// "no such local payee" distinguishable from "signer not designated", a
/// signed probe could enumerate both local accounts and their trust roots.
/// The [`ZapVerdict::Untrusted`] reason is diagnostics-only by contract.
///
/// Returns `None` when the event is not a parseable zap receipt, so it falls
/// through to the generic gates indistinguishably from any other event.
/// Public so the tier_3 integration test can drive both paths with a real
/// `AppState` (there is no in-crate WebSocket harness).
#[cfg(feature = "zaps")]
pub async fn handle_zap_receipt_inbox(
    state: &Arc<AppState>,
    event: &fauna_bridge_nostr::types::Event,
) -> Option<RelayMessage> {
    let event_id = event.id.clone();

    // Requires the `p` tag; a kind-9735 without one is not a zap receipt this
    // relay can route, and falls through to the generic path.
    let receipt = fauna_bridge_nostr::nip57::parse_zap_receipt(event).ok()?;

    // Size limit (advertised via NIP-11 `max_message_length`). Recipient-
    // neutral (a property of the event) and it bounds the signature-
    // verification cost below.
    let wire = serde_json::to_string(event).unwrap_or_default();
    if wire.len() > store::MAX_EVENT_SIZE {
        return Some(reject_ok(&event_id, "invalid: event exceeds max size"));
    }

    // Verify the receipt's own signature BEFORE the trust lookup (the F4
    // ordering rule above). This proves only that the named signer signed it
    // — the trust question is the next one, and it is the one that matters.
    if !verify_event(event) {
        return Some(reject_ok(&event_id, "invalid: bad signature"));
    }

    // The trust gate — the same shared decision ingress A makes
    // (`zap_ingest::classify_incoming_zap`), so the two doors cannot drift
    // apart. A payee pubkey belonging to no local account resolves to the
    // empty designation set and fails closed with no separate arm.
    let conn = state.db.conn().await;
    let verdict = crate::nostr::zap_ingest::classify_incoming_zap(&conn, event);
    drop(conn);
    let zap = match verdict {
        fauna_bridge_nostr::nip57::ZapVerdict::Trusted(zap) => zap,
        fauna_bridge_nostr::nip57::ZapVerdict::Untrusted(reason) => {
            tracing::debug!(
                event_id = %event_id,
                signer = %event.pubkey,
                payee = %receipt.target_pubkey,
                ?reason,
                "rejecting untrusted zap receipt (ingress B)"
            );
            // One message for every untrusted reason — see the F4 note above.
            return Some(reject_ok(
                &event_id,
                "restricted: zap receipt is not from a designated signer",
            ));
        }
    };

    // **Gate surface `zaps.receipt.ingest`** — the same shared decision ingress
    // A makes, asked BEFORE the verbatim store, because a refused receipt must
    // leave nothing behind: not a stored event, not a summed total, not a tip.
    // This is the door the `restricted:` machine-readable prefix exists for.
    if let Err(e) = crate::nostr::zap_ingest::gate_receipt_ingest(state, &zap).await {
        tracing::debug!(
            event_id = %event_id,
            payee = %receipt.target_pubkey,
            code = %e.code,
            "zap receipt refused by the feature gate (ingress B)"
        );
        return Some(reject_ok(
            &event_id,
            "restricted: zap receipts are limited on this account",
        ));
    }

    // Store verbatim (kind 9735 is a Regular class → stored as-is; its `p`/`e`
    // tags are indexed so the REQ path can find it). `derived=false` — a
    // genuinely received event. Disk is bounded by `store_event`'s own
    // `MAX_EVENTS_PER_ACCOUNT` (per signer pubkey) and `MAX_STORE_EVENTS`
    // caps; no separate limiter is needed, because unlike the gift-wrap inbox
    // this path admits only signers the payee explicitly opted into rather
    // than any stranger.
    let conn = state.db.conn().await;
    let outcome = store::store_event(&conn, event, false);
    match outcome {
        Ok(store::StoreOutcome::Expired) => {
            drop(conn);
            return Some(reject_ok(&event_id, "invalid: event is expired"));
        }
        Ok(store::StoreOutcome::CapExceeded) => {
            drop(conn);
            return Some(reject_ok(&event_id, "rate-limited: store capacity reached"));
        }
        Ok(o) => {
            if !o.is_newly_stored() {
                // Duplicate/Superseded: already processed once. Do not
                // re-record the zap — `insert_zap` is keyed on the event id so
                // it would be a no-op, but returning early keeps the
                // accounting path single-entry, the gift-wrap precedent.
                drop(conn);
                return Some(RelayMessage::Ok {
                    event_id,
                    accepted: true,
                    message: "duplicate: already have this event".to_string(),
                });
            }
            // Resolve the receipt's Fauna coordinates while the connection is
            // still held — the purchase judgement below needs the tier row
            // behind the async `CacheDb`, which is this same mutex.
            let subject = crate::nostr::zap_ingest::resolve_zap_subject(&conn, &zap);
            drop(conn);

            // The tip↔purchase split, made at ingest through the same shared
            // decision ingress A makes (`monetization.md` § Per-post
            // pay-to-unlock). A receipt that meets its target tier's asking
            // price is a sale; anything else stays a tip.
            let purchased = match subject.as_ref() {
                Some(s) => crate::nostr::zap_ingest::apply_zap_purchase(state, &zap, s).await,
                None => None,
            };

            // Record the believed zap through the same shared accounting
            // write ingress A uses, so downstream consumers inherit the trust
            // guarantee structurally whichever door the receipt came through.
            let conn = state.db.conn().await;
            crate::nostr::zap_ingest::record_zap_as(&conn, &zap, purchased.as_deref());
            drop(conn);
            let _ = state.nostr.relay_tx.send(NostrRelayEvent {
                event_json: wire,
                author_pubkey: event.pubkey.clone(),
            });
        }
        Err(e) => {
            drop(conn);
            tracing::warn!("nostr relay: store zap receipt failed: {e}");
            return Some(reject_ok(&event_id, "error: could not store event"));
        }
    }

    Some(RelayMessage::Ok {
        event_id,
        accepted: true,
        message: String::new(),
    })
}

/// Handle an unauthenticated kind-24133 (NIP-46) bunker request — the second
/// unauthenticated-write carve-out beside the gift-wrap inbox (`nostr.md`
/// § The nest as the user's NIP-46 signer, transport bullet). Returns `None`
/// (the caller falls through to the generic auth-required/restricted path) only
/// when the `p` tag names no registered signer *and* this box is not a proxy
/// serving face — so a signed request at a non-signer pubkey on an ordinary box
/// is indistinguishable from any other non-local event. On a **keyless public
/// serving box** (holding a `nostr_push` pairing) a membership miss is instead
/// transported ephemerally (R10, [`bunker_ephemeral_fallthrough`]) — the head
/// hosts the signer, this box is only its relay face.
///
/// **Ordering is load-bearing for privacy (`network-exposure.md` § Rulings
/// F4), mirroring [`handle_gift_wrap_inbox`]:** signature-verify runs BEFORE
/// the signer-registry membership check, so an *unsigned* probe is rejected
/// identically whether or not the target pubkey is a registered signer —
/// membership is never decidable from an unsigned event. Then the per-signer
/// rate limit, then handling. The request is broadcast per ephemeral
/// semantics (kind 24133 is never stored); the response event (signed by the
/// signer key, encrypted to the app in the request's scheme) rides
/// `relay_tx` to the app's live REQ subscription — standard NIP-46
/// operation. Public so the tier_3 integration test can drive it (there is
/// no in-crate WebSocket harness).
pub async fn handle_bunker_request(
    state: &Arc<AppState>,
    event: &fauna_bridge_nostr::types::Event,
) -> Option<RelayMessage> {
    let event_id = event.id.clone();

    // The target signer pubkey from the `p` tag; without one this is not
    // bunker traffic — generic path.
    let signer_pubkey = event.tags.iter().find_map(|t| {
        if t.name() == Some("p") {
            t.value().map(|v| v.to_string())
        } else {
            None
        }
    })?;

    // Size limit — membership-neutral (a property of the event, not the
    // signer), bounds the signature-verification cost below.
    let wire = serde_json::to_string(event).unwrap_or_default();
    if wire.len() > store::MAX_EVENT_SIZE {
        return Some(reject_ok(&event_id, "invalid: event exceeds max size"));
    }

    // F4: verify the request's signature BEFORE any signer-specific check —
    // an unsigned probe must not learn whether a pubkey is a registered
    // bunker signer here (see the ordering note in the doc comment).
    if !verify_event(event) {
        return Some(reject_ok(&event_id, "invalid: bad signature"));
    }

    // Membership: runs only for a validly-signed event.
    let conn = state.db.conn().await;
    let actor = bunker::signer_actor(&conn, &signer_pubkey).ok().flatten();
    drop(conn);
    if actor.is_none() {
        // Not a signer THIS box hosts. On a keyless public *serving* box (one
        // holding a `nostr_push` pairing) — the head's public relay face — the
        // 24133 must be transported like a plain ephemeral relay would (R10;
        // [`bunker_ephemeral_fallthrough`]); on any other box (a pure head, or
        // a box with no pairing) fall through (`None`) to the generic
        // auth-required/restricted path, indistinguishable from any other
        // non-local event.
        return bunker_ephemeral_fallthrough(state, event, &signer_pubkey, wire).await;
    }

    // Per-signer rate limit (a hard constant beside the gift-wrap limiter).
    if let Some(ref limiter) = state.nostr.bunker_limiter
        && limiter.check_key(&signer_pubkey).is_err()
    {
        return Some(reject_ok(
            &event_id,
            "rate-limited: too many bunker requests for this signer",
        ));
    }

    // Broadcast the request per ephemeral semantics (kind 24133 is never
    // stored) — standard relay behavior; subscribers see the ciphertext.
    let _ = state.nostr.relay_tx.send(NostrRelayEvent {
        event_json: wire,
        author_pubkey: event.pubkey.clone(),
    });

    // Decrypt → authorize → execute → build the signed response (the shared
    // core, also driven by the head's NIP-46 proxy subscription). Roster + nsec
    // both co-reside here, so the response is built in-process; on failure the
    // transport event is rejected loudly (never a silent drop).
    let nest_key = state.nest_identity.signing_key.to_bytes();
    let now = crate::db::now_epoch_secs() as u64;
    let conn = state.db.conn().await;
    match bunker::execute_bunker_request(&conn, &nest_key, &signer_pubkey, event, now) {
        Ok(response) => {
            drop(conn);
            // The response rides the live broadcast; the app holds its REQ open.
            let _ = state.nostr.relay_tx.send(NostrRelayEvent {
                event_json: serde_json::to_string(&response).unwrap_or_default(),
                author_pubkey: response.pubkey.clone(),
            });
        }
        Err(e) => {
            drop(conn);
            tracing::warn!("nostr bunker: process request failed: {e}");
            return Some(reject_ok(&event_id, "error: could not process request"));
        }
    }

    Some(RelayMessage::Ok {
        event_id,
        accepted: true,
        message: String::new(),
    })
}

/// The membership-miss path of [`handle_bunker_request`] (R10, part A): a
/// validly-signed, size-checked kind-24133 whose `p`-tag names no signer
/// registered on *this* box.
///
/// A **keyless public serving box** — one holding a `nostr_push` pairing, i.e.
/// the public relay face of a paired head that holds the nsec + bunker roster —
/// must transport such an event as a plain NIP-01 ephemeral relay would:
/// broadcast to live matching subscribers, never stored. That is what carries
/// **both** directions of the bunker leg through the public box — the app's
/// request out to the head's standing subscription, and the head's signer-
/// authored response (which `p`-tags the *app*, so it too misses local
/// membership) back to the app's open REQ.
///
/// The gate is the **pairing-derived** half of serving-availability *only*
/// (`any_pairing_with_capability(NOSTR_PUSH)`), deliberately not the whole
/// [`nostr_serving_available`](crate::nostr::nostr_serving_available): a box
/// that serves purely because it holds a *local nsec deposit* is itself a head,
/// not a transport for others, and turning it into an unconditional open
/// kind-24133 relay for arbitrary pubkeys would be an abuse surface
/// (`network-exposure.md` § Rulings F4). A box that is neither a proxy face nor
/// hosts the signer returns `None` (the pre-Phase-2 fall-through, unchanged).
/// The per-signer rate limiter still binds — keyed on the `p`-tag pubkey — so
/// the open transport is bounded. There is no membership to leak here (bunker
/// signer rows live only on the head), so a proxy box treats every validly-
/// signed 24133 uniformly; F4 blindness is preserved because the signature
/// check already ran ahead of this.
async fn bunker_ephemeral_fallthrough(
    state: &Arc<AppState>,
    event: &fauna_bridge_nostr::types::Event,
    signer_pubkey: &str,
    wire: String,
) -> Option<RelayMessage> {
    let is_proxy_face = state
        .db
        .any_pairing_with_capability(fauna_protocol::pair::capability::NOSTR_PUSH)
        .await
        .unwrap_or(false);
    if !is_proxy_face {
        return None;
    }

    if let Some(ref limiter) = state.nostr.bunker_limiter
        && limiter.check_key(&signer_pubkey.to_string()).is_err()
    {
        return Some(reject_ok(
            &event.id,
            "rate-limited: too many bunker requests for this signer",
        ));
    }

    // Ephemeral broadcast (kind 24133 is 20000–29999 — never stored); fan out
    // to live subscribers only.
    let _ = state.nostr.relay_tx.send(NostrRelayEvent {
        event_json: wire,
        author_pubkey: event.pubkey.clone(),
    });

    Some(RelayMessage::Ok {
        event_id: event.id.clone(),
        accepted: true,
        message: String::new(),
    })
}

async fn handle_relay_ws(state: Arc<AppState>, socket: WebSocket) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Generate NIP-42 challenge
    let challenge = fauna_core::identity::random_hex(16);
    let auth_msg = RelayMessage::Auth(challenge.clone());
    if ws_tx
        .send(Message::Text(auth_msg.to_json().into()))
        .await
        .is_err()
    {
        return;
    }

    let mut authed_pubkey: Option<String> = None;
    let mut subscriptions: HashMap<String, Vec<Filter>> = HashMap::new();

    // Per-connection budget for unauthenticated EVENT/REQ/COUNT processing
    // (see UNAUTHED_MSGS_PER_MINUTE — AUTH/CLOSE exempt, authed conns exempt).
    let unauthed_limiter = governor::RateLimiter::direct(governor::Quota::per_minute(
        std::num::NonZeroU32::new(UNAUTHED_MSGS_PER_MINUTE).unwrap(),
    ));

    // Subscribe to broadcast channel for live events
    let mut live_rx = state.nostr.relay_tx.subscribe();

    // Server half of the heartbeat (`transport.md` § Connection lifecycle).
    //
    // **This endpoint's peer population is the one that had to be established
    // rather than assumed.** Every other WS surface on this nest faces software
    // we ship; `/nostr` faces arbitrary third-party Nostr clients. RFC 6455 makes
    // the Pong mandatory, but "the RFC requires it" is precisely the reasoning
    // that produced the UA-less ActivityPub interop outage, so it is instead
    // pinned by the real-client harness: `tests/nostr_relay_interop.rs`'s
    // `a_real_nostr_client_survives_many_idle_liveness_windows` keeps a
    // `nostr-sdk` connection idle across several windows and then uses it. If a
    // real client ever fails to answer, that test goes red *before* the relay
    // starts dropping live sessions.
    let mut hb = ServerHeartbeat::new(state.ws.heartbeat());

    loop {
        tokio::select! {
            beat = hb.next_beat() => {
                match beat {
                    Beat::Ping => {
                        if ws_tx.send(Message::Ping(bytes::Bytes::new())).await.is_err() {
                            break;
                        }
                    }
                    Beat::Dead => {
                        // `warn`, not `debug`: a connection reaped in silence is
                        // indistinguishable from a quiet night.
                        tracing::warn!(
                            timeout_ms = hb.liveness_timeout().as_millis() as u64,
                            "nostr client answered no heartbeat within the liveness window; \
                             closing dead link",
                        );
                        break;
                    }
                }
                continue;
            }
            // Incoming client message
            msg = ws_rx.next() => {
                // Any inbound frame proves the client is alive, so re-arm before
                // the frame is even inspected — the Pong its WebSocket stack
                // sends below its application code counts exactly as much as a
                // REQ. This is what makes the mechanism safe for a client that
                // is legitimately quiet for hours (a subscriber waiting on live
                // events sends nothing at all).
                hb.re_arm();
                let msg = match msg {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => continue,
                };

                let parsed = ClientMessage::from_json(&msg);

                // Unauthed message budget — checked after parse (parse cost is
                // bounded by the transport cap) and before any store/crypto
                // work. Loud per NIP-01: EVENT gets `OK false`, REQ/COUNT get
                // `CLOSED`, both with the machine-readable `rate-limited:`
                // prefix — never a silent drop.
                if authed_pubkey.is_none()
                    && matches!(
                        parsed,
                        Ok(ClientMessage::Event(_)
                            | ClientMessage::Req { .. }
                            | ClientMessage::Count { .. })
                    )
                    && unauthed_limiter.check().is_err()
                {
                    let reply = match &parsed {
                        Ok(ClientMessage::Event(event)) => reject_ok(
                            &event.id,
                            "rate-limited: too many messages, slow down",
                        ),
                        Ok(ClientMessage::Req { subscription_id, .. })
                        | Ok(ClientMessage::Count { subscription_id, .. }) => {
                            RelayMessage::Closed {
                                subscription_id: subscription_id.clone(),
                                message: "rate-limited: too many messages, slow down"
                                    .into(),
                            }
                        }
                        _ => unreachable!("guarded by the matches! above"),
                    };
                    let _ = ws_tx.send(Message::Text(reply.to_json().into())).await;
                    continue;
                }

                match parsed {
                    Ok(ClientMessage::Auth(event)) => {
                        // The relay tag is matched by *host*, not by exact
                        // string: real clients echo the URL as they normalized
                        // it (scheme, explicit port, trailing slash, bare host
                        // vs. `/nostr` all vary — the N2 interop harness caught
                        // `nostr-sdk` locked out by the old two-exact-forms
                        // check). Recipient-gated gift-wrap serving (slice B)
                        // depends on auth succeeding, so this must not hinge on
                        // which form a client sends.
                        //
                        // Resolved from the claim-refreshed identity domain, not
                        // the `node.domain` boot seed: a provisioned box boots
                        // domainless, so the seed reads `localhost` and every
                        // real client's echoed URL fails the host match — auth,
                        // and the recipient-gated gift-wrap serving behind it,
                        // dead until a restart. Still our *canonical identity*,
                        // never the request `Host` header.
                        let host = state.handle_domain();
                        if nip42::verify_auth_event_for_host(&event, &challenge, &host, "/nostr") {
                            let conn = state.db.conn().await;
                            let is_local = db::get_account_by_pubkey(&conn, &event.pubkey)
                                .ok()
                                .flatten()
                                .is_some();
                            drop(conn);

                            if is_local {
                                authed_pubkey = Some(event.pubkey.clone());
                                let ok = RelayMessage::Ok {
                                    event_id: event.id,
                                    accepted: true,
                                    message: "authenticated".into(),
                                };
                                let _ = ws_tx.send(Message::Text(ok.to_json().into())).await;
                            } else {
                                let ok = RelayMessage::Ok {
                                    event_id: event.id,
                                    accepted: false,
                                    message: "restricted: pubkey not linked to a local account".into(),
                                };
                                let _ = ws_tx.send(Message::Text(ok.to_json().into())).await;
                            }
                        } else {
                            let ok = RelayMessage::Ok {
                                event_id: event.id,
                                accepted: false,
                                message: "auth: invalid signature or challenge".into(),
                            };
                            let _ = ws_tx.send(Message::Text(ok.to_json().into())).await;
                        }
                    }
                    Ok(ClientMessage::Event(event)) => {
                        // Slice B — the NIP-17 gift-wrap inbox: a kind-1059 gift
                        // wrap addressed (`p` tag) to a local depositor is the ONE
                        // unauthenticated write the relay accepts (rate-limited,
                        // per recipient). Handled ahead of the auth-required gate;
                        // a 1059 to a non-depositor still gets `restricted:`, and
                        // every other unauthenticated write still gets
                        // `auth-required:` below (`nostr.md` § The relay event
                        // store — read/write policy + inbox role).
                        if event.kind == 1059 {
                            let ok = handle_gift_wrap_inbox(&state, event).await;
                            let _ = ws_tx.send(Message::Text(ok.to_json().into())).await;
                            continue;
                        }

                        // The NIP-57 zap-receipt carve-out — the ratified
                        // category-2 half for receipts targeting a local
                        // user's events. A receipt is authored by the payee's
                        // LNURL/wallet server, never a local account, so it
                        // can only arrive as an unauthenticated write; the
                        // payee's designated-signer list is what makes
                        // accepting it safe. Not a parseable receipt → falls
                        // through to the generic gates below.
                        // Excised with the `zaps` member: with the carve-out gone
                        // a kind-9735 simply falls through to the generic gates
                        // below, where an unauthenticated write is refused like
                        // any other — the "no listener" property, reached by
                        // deleting the door rather than by adding a refusing arm
                        // (a `cfg(not(...))` here would also be dark to both arms
                        // of `nest-lib-test-check`).
                        #[cfg(feature = "zaps")]
                        if event.kind == fauna_bridge_nostr::nip57::ZAP_RECEIPT_KIND
                            && let Some(ok) = handle_zap_receipt_inbox(&state, &event).await
                        {
                            let _ = ws_tx.send(Message::Text(ok.to_json().into())).await;
                            continue;
                        }

                        // The NIP-46 bunker carve-out: a kind-24133 request
                        // addressed (`p` tag) to a registered bunker signer is
                        // the second unauthenticated write (same F4 ordering
                        // as the gift-wrap inbox). Not bunker-addressed →
                        // falls through to the generic gates below,
                        // indistinguishable from any other event.
                        if event.kind == 24133
                            && let Some(ok) = handle_bunker_request(&state, &event).await
                        {
                            let _ = ws_tx.send(Message::Text(ok.to_json().into())).await;
                            continue;
                        }

                        if authed_pubkey.is_none() {
                            let ok = RelayMessage::Ok {
                                event_id: event.id,
                                accepted: false,
                                message: "auth-required: authenticate first".into(),
                            };
                            let _ = ws_tx.send(Message::Text(ok.to_json().into())).await;
                            continue;
                        }

                        // Size limit (advertised via NIP-11 `max_message_length`).
                        let wire = serde_json::to_string(&event).unwrap_or_default();
                        if wire.len() > store::MAX_EVENT_SIZE {
                            let ok = RelayMessage::Ok {
                                event_id: event.id,
                                accepted: false,
                                message: "invalid: event exceeds max size".into(),
                            };
                            let _ = ws_tx.send(Message::Text(ok.to_json().into())).await;
                            continue;
                        }

                        if !verify_event(&event) {
                            let ok = RelayMessage::Ok {
                                event_id: event.id,
                                accepted: false,
                                message: "invalid: bad signature".into(),
                            };
                            let _ = ws_tx.send(Message::Text(ok.to_json().into())).await;
                            continue;
                        }

                        // Owner-only scope (slice A): the store accepts only
                        // events *authored by* a local-account pubkey. (The
                        // *addressed-to* gift-wrap inbox is slice B.)
                        let conn = state.db.conn().await;
                        let acct = db::get_account_by_pubkey(&conn, &event.pubkey).ok().flatten();
                        if acct.is_none() {
                            drop(conn);
                            let ok = RelayMessage::Ok {
                                event_id: event.id.clone(),
                                accepted: false,
                                message: "restricted: pubkey not linked to a local account".into(),
                            };
                            let _ = ws_tx.send(Message::Text(ok.to_json().into())).await;
                            continue;
                        }

                        // Persist per NIP-01 lifecycle (regular / replaceable /
                        // parameterized-replaceable / ephemeral / duplicate).
                        let outcome = store::store_event(&conn, &event, false);

                        // NIP-09: a stored kind-5 deletion request removes the
                        // events/coordinates its e/a tags name, author-scoped
                        // (`store::apply_deletion`). Runs on any Ok outcome,
                        // including Duplicate — a resent deletion re-applying
                        // is a harmless no-op, and skipping it on Duplicate
                        // would silently drop a legitimate retry.
                        if event.kind == 5
                            && outcome.is_ok()
                            && let Err(e) = store::apply_deletion(&conn, &event)
                        {
                            tracing::warn!("nostr relay: apply deletion failed: {e}");
                        }
                        drop(conn);

                        let (accepted, message) = match outcome {
                            Ok(store::StoreOutcome::Expired) => {
                                (false, "invalid: event is expired".to_string())
                            }
                            Ok(store::StoreOutcome::CapExceeded) => {
                                (false, "rate-limited: store capacity reached".to_string())
                            }
                            Ok(o) => {
                                // Broadcast newly-stored and ephemeral events to
                                // live subscribers; a duplicate/superseded event
                                // changed no state, so it is not re-broadcast.
                                if o.is_newly_stored()
                                    || o == store::StoreOutcome::Ephemeral
                                {
                                    let _ = state.nostr.relay_tx.send(NostrRelayEvent {
                                        event_json: wire,
                                        author_pubkey: event.pubkey.clone(),
                                    });
                                }
                                tracing::debug!(
                                    "nostr relay: event {} -> {:?}",
                                    &event.id, o
                                );
                                let msg = match o {
                                    store::StoreOutcome::Duplicate
                                    | store::StoreOutcome::Superseded => "duplicate:",
                                    _ => "",
                                };
                                (true, msg.to_string())
                            }
                            Err(e) => {
                                tracing::warn!("nostr relay: store event failed: {e}");
                                (false, "error: could not store event".to_string())
                            }
                        };

                        let ok = RelayMessage::Ok {
                            event_id: event.id.clone(),
                            accepted,
                            message,
                        };
                        let _ = ws_tx.send(Message::Text(ok.to_json().into())).await;
                    }
                    Ok(ClientMessage::Req { subscription_id, filters }) => {
                        // Filter/search caps — refused (CLOSED) before any
                        // store query builds SQL or an FTS MATCH.
                        if let Some(reason) = req_reject_reason(&filters) {
                            let closed = RelayMessage::Closed { subscription_id, message: reason };
                            let _ = ws_tx.send(Message::Text(closed.to_json().into())).await;
                            continue;
                        }

                        // NIP-42's documented on-demand flow: an unauthed REQ
                        // that *explicitly asks for* kind-1059 gift wraps gets
                        // `CLOSED auth-required:` so an on-demand-auth client
                        // knows to AUTH and retry (an eager-auth client like
                        // nostr-sdk never sees this). Fires on the client's own
                        // filter shape before any store read — it reveals
                        // nothing about inbox contents. Kindless filters keep
                        // the silent recipient gate (they're normal public
                        // reads; gating them would break anonymous replay).
                        if authed_pubkey.is_none() && req_asks_for_gift_wraps(&filters) {
                            let closed = RelayMessage::Closed {
                                subscription_id,
                                message: "auth-required: gift wraps are served only to their NIP-42-authenticated recipient".into(),
                            };
                            let _ = ws_tx.send(Message::Text(closed.to_json().into())).await;
                            continue;
                        }

                        // Per-connection subscription cap (advertised via
                        // NIP-11 `max_subscriptions`) — every live broadcast
                        // re-evaluates every subscription, so the map must be
                        // bounded. Replacing an existing subscription_id is
                        // always allowed, even at the cap.
                        if !subscriptions.contains_key(&subscription_id)
                            && subscriptions.len()
                                >= fauna_bridge_nostr::nip11::MAX_SUBSCRIPTIONS_PER_CONN
                        {
                            let closed = RelayMessage::Closed {
                                subscription_id,
                                message: "rate-limited: too many subscriptions".into(),
                            };
                            let _ = ws_tx.send(Message::Text(closed.to_json().into())).await;
                            continue;
                        }

                        subscriptions.insert(subscription_id.clone(), filters.clone());

                        // Serve from the persistent store with real filter SQL.
                        // Exposed Fauna posts are materialized (signed once) into
                        // the same store, so there is no translate-on-read and no
                        // empty-`sig` fallback — the relay never emits an unsigned
                        // event (`nostr.md` § The relay event store).
                        let conn = state.db.conn().await;
                        let events = store::query_events(
                            &conn,
                            &filters,
                            store::MAX_QUERY_LIMIT,
                        )
                        .unwrap_or_default();
                        drop(conn);

                        for event in events {
                            // Final authoritative match (covers prefix ids/authors
                            // and any non-indexed multi-letter tag filter), then the
                            // kind-1059 recipient gate — a gift wrap is served only
                            // to its NIP-42-authed `p`-tag recipient (slice B); a
                            // missed gate would leak DM ciphertext + metadata to any
                            // REQ reader.
                            if matches_any(&filters, &event)
                                && store::gift_wrap_visible_to(&event, authed_pubkey.as_deref())
                            {
                                let msg = RelayMessage::Event {
                                    subscription_id: subscription_id.clone(),
                                    event,
                                };
                                if ws_tx.send(Message::Text(msg.to_json().into())).await.is_err() {
                                    return;
                                }
                            }
                        }

                        let eose = RelayMessage::Eose(subscription_id);
                        let _ = ws_tx.send(Message::Text(eose.to_json().into())).await;
                    }
                    Ok(ClientMessage::Close(sub_id)) => {
                        subscriptions.remove(&sub_id);
                    }
                    Ok(ClientMessage::Count { subscription_id, filters }) => {
                        // Same filter/search caps as REQ — COUNT shares the
                        // query path, so it shares the pre-query gate.
                        if let Some(reason) = req_reject_reason(&filters) {
                            let closed = RelayMessage::Closed { subscription_id, message: reason };
                            let _ = ws_tx.send(Message::Text(closed.to_json().into())).await;
                            continue;
                        }
                        // Same NIP-42 on-demand-auth signal as REQ (see there).
                        if authed_pubkey.is_none() && req_asks_for_gift_wraps(&filters) {
                            let closed = RelayMessage::Closed {
                                subscription_id,
                                message: "auth-required: gift wraps are served only to their NIP-42-authenticated recipient".into(),
                            };
                            let _ = ws_tx.send(Message::Text(closed.to_json().into())).await;
                            continue;
                        }
                        // NIP-45: a one-shot count, not a live subscription —
                        // no `subscriptions` entry, no EOSE. Same recipient
                        // gate as REQ (`store::count_events` applies
                        // `gift_wrap_visible_to` internally).
                        let conn = state.db.conn().await;
                        let result = store::count_events(&conn, &filters, authed_pubkey.as_deref())
                            .unwrap_or(store::CountResult { count: 0, approximate: false });
                        drop(conn);
                        let msg = RelayMessage::Count {
                            subscription_id,
                            count: result.count,
                            approximate: result.approximate,
                        };
                        let _ = ws_tx.send(Message::Text(msg.to_json().into())).await;
                    }
                    Err(e) => {
                        let notice = RelayMessage::Notice(format!("error: {e}"));
                        let _ = ws_tx.send(Message::Text(notice.to_json().into())).await;
                    }
                }
            }

            // Live event from broadcast channel
            event = live_rx.recv() => {
                if let Ok(relay_event) = event
                    && let Ok(parsed) = serde_json::from_str::<fauna_bridge_nostr::types::Event>(&relay_event.event_json) {
                        for (sub_id, filters) in &subscriptions {
                            // The same kind-1059 recipient gate on the live path: an
                            // accepted gift wrap is broadcast to every subscriber, so
                            // this is what keeps it from reaching anyone but its
                            // NIP-42-authed `p`-tag recipient.
                            if matches_any(filters, &parsed)
                                && store::gift_wrap_visible_to(&parsed, authed_pubkey.as_deref())
                            {
                                let msg = RelayMessage::Event {
                                    subscription_id: sub_id.clone(),
                                    event: parsed.clone(),
                                };
                                if ws_tx.send(Message::Text(msg.to_json().into())).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn over_cap_search_is_rejected_before_any_query() {
        // The red-first pin:
        // a REQ/COUNT whose search exceeds the relay caps must be refused
        // (NIP-01 CLOSED) before any store query builds an FTS MATCH.
        let ok = Filter {
            search: Some("picnic photos".into()),
            ..Default::default()
        };
        assert!(req_reject_reason(std::slice::from_ref(&ok)).is_none());

        let over = Filter {
            search: Some("x".repeat(1024 * 1024)),
            ..Default::default()
        };
        let reason = req_reject_reason(&[ok, over]).expect("over-cap search must be rejected");
        assert!(reason.starts_with("invalid:"), "{reason}");
    }

    #[test]
    fn too_many_filters_is_rejected() {
        use fauna_bridge_nostr::nip11::MAX_FILTERS_PER_REQ;
        let filters = vec![Filter::default(); MAX_FILTERS_PER_REQ + 1];
        let reason = req_reject_reason(&filters).expect("over-cap filter list must be rejected");
        assert!(reason.starts_with("invalid:"), "{reason}");
        assert!(req_reject_reason(&filters[..MAX_FILTERS_PER_REQ]).is_none());
    }
}
