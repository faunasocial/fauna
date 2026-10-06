//! Nostr interaction routing for the unified interact endpoint.
//!
//! Maps unified actions to Nostr event kinds:
//! - like/react  -> kind 7 reaction (NIP-25)
//! - repost      -> kind 6 repost (NIP-18)
//! - reply        -> kind 1 with `e` and `p` tags
//! - quote        -> kind 1 with `q` tag (NIP-18)
//!
//! `reply`/`quote` here are the eligibility + target-info door only: the reply is the
//! replier's own signed post and the create-side nostr arm derives + pushes the event —
//! `ui/nostr.md` § Replying to and quoting a nostr note. The door publishes nothing.

use axum::response::Json;
use serde_json::json;

use fauna_bridge_nostr::relay_client::{RelayClient, RelayDialPolicy};
use fauna_bridge_nostr::signing::Keypair;
use fauna_bridge_nostr::types::{Event, Filter, UnsignedEvent};
use fauna_bridge_nostr::{nip18, nip25};

use crate::api_error::ApiError;
use crate::nostr::key_crypto;
use crate::nostr::relays::resolve_relay_urls;
use crate::nostr::store;
use crate::routes::AppState;

/// Route a unified interaction to the Nostr network.
///
/// Looks up the Nostr event ID from `nostr_event_map`, constructs the
/// appropriate event kind, signs it, and publishes to the user's relay list.
pub async fn route_unified_interaction(
    state: &AppState,
    actor_hex: &str,
    post_id_hex: &str,
    action: &str,
) -> Result<Json<serde_json::Value>, ApiError> {
    let conn = state.db.conn().await;

    // Look up the Nostr event ID for this post
    let event_entry = super::db::get_event_by_fauna_id(&conn, post_id_hex)
        .map_err(|e| {
            tracing::error!("nostr event map lookup error: {e}");
            ApiError::internal("failed to look up Nostr event")
        })?
        .ok_or_else(|| ApiError::not_found("no Nostr event mapping found for this post"))?;

    // Look up the user's Nostr account
    let account = super::db::get_account(&conn, actor_hex)
        .map_err(|e| {
            tracing::error!("nostr account lookup error: {e}");
            ApiError::internal("failed to look up Nostr account")
        })?
        .ok_or_else(|| {
            ApiError::bad_request(
                "no linked Nostr account; link one first from the Nostr settings page",
            )
        })?;

    drop(conn);

    let nostr_event_id = &event_entry.nostr_event_id;
    let target_pubkey = &event_entry.nostr_pubkey;

    // Resolved once for every publishing arm below. An explicit empty list —
    // the user removed every relay — is respected verbatim (the explicit-empty
    // semantic flip, `relays.rs` module doc) and refused loudly here rather
    // than handed to `spawn_publish`, whose fire-and-forget task has no way to
    // report a zero-relay publish back to this response.
    let relay_urls = resolve_relay_urls(account.relay_list.as_deref());

    match action {
        "like" => {
            if relay_urls.is_empty() {
                return Err(ApiError::bad_request(
                    "no relays configured; add a relay first",
                ));
            }

            // NIP-25: kind 7 reaction with "+" content
            // Tags: ["e", <event_id>], ["p", <pubkey>]
            tracing::info!(
                actor = actor_hex,
                nostr_event = nostr_event_id,
                "nostr: publishing kind 7 reaction"
            );

            let keypair = decrypt_keypair(state, &account)?;
            let now = crate::db::now_epoch_secs() as u64;

            let unsigned = build_like_event(
                nostr_event_id,
                target_pubkey,
                keypair.public_key_bytes(),
                now,
            );

            let signed = keypair.sign_event(unsigned);
            spawn_publish(signed.clone(), relay_urls, state.nostr.relay_dial_policy);

            Ok(Json(json!({
                "ok": true,
                "protocol": "nostr",
                "event_kind": 7,
                "event_id": signed.id,
                "target_event": nostr_event_id,
                "target_pubkey": target_pubkey,
                "status": "published",
            })))
        }
        "repost" => {
            if relay_urls.is_empty() {
                return Err(ApiError::bad_request(
                    "no relays configured; add a relay first",
                ));
            }

            // NIP-18: kind 6 repost wrapping the full reposted event JSON.
            tracing::info!(
                actor = actor_hex,
                nostr_event = nostr_event_id,
                "nostr: publishing kind 6 repost"
            );

            let keypair = decrypt_keypair(state, &account)?;
            let now = crate::db::now_epoch_secs() as u64;

            let reposted_event = fetch_stored_event(state, nostr_event_id).await?;
            let unsigned = build_repost_event(&reposted_event, keypair.public_key_bytes(), now);

            let signed = keypair.sign_event(unsigned);
            spawn_publish(signed.clone(), relay_urls, state.nostr.relay_dial_policy);

            Ok(Json(json!({
                "ok": true,
                "protocol": "nostr",
                "event_kind": 6,
                "event_id": signed.id,
                "target_event": nostr_event_id,
                "target_pubkey": target_pubkey,
                "status": "published",
            })))
        }
        // The eligibility door: every reason the create-side arm could not
        // publish the composed post is refused here, before the app composes
        // it; the native arm's ack verbatim otherwise (the dispatcher adds the
        // swept note's counters — it is a local row).
        "reply" | "quote" => {
            if relay_urls.is_empty() {
                return Err(ApiError::bad_request(
                    "no relays configured; add a relay first",
                ));
            }
            decrypt_keypair(state, &account)?;
            if action == "reply" && !account.publish_replies {
                return Err(ApiError::bad_request(
                    "replies to Nostr are switched off; turn on Publish replies on the Nostr page",
                ));
            }
            Ok(Json(json!({
                "action": action,
                "target_post_id": post_id_hex,
                "source": "nostr",
            })))
        }
        _ => Err(ApiError::bad_request(format!(
            "action '{action}' is not supported on Nostr"
        ))),
    }
}

/// Build the NIP-25 kind-7 reaction event for a unified "like" interaction.
/// Pure wiring over the shared [`nip25::build_reaction_event`] builder —
/// unified "like" always reacts with the default `"+"` emoji.
fn build_like_event(
    target_id: &str,
    target_pubkey: &str,
    reactor_pubkey: [u8; 32],
    created_at: u64,
) -> UnsignedEvent {
    nip25::build_reaction_event(target_id, target_pubkey, "+", &reactor_pubkey, created_at)
}

/// Build the NIP-18 kind-6 repost event for a unified "repost" interaction.
/// Pure wiring over the shared [`nip18::build_repost_event`] builder, which
/// wraps the full reposted event as the repost's content (NIP-18).
fn build_repost_event(
    reposted_event: &Event,
    reposter_pubkey: [u8; 32],
    created_at: u64,
) -> UnsignedEvent {
    nip18::build_repost_event(reposted_event, &reposter_pubkey, created_at)
}

/// Fetch the full stored Nostr event for `nostr_event_id` from the relay
/// event store — needed to build a NIP-18 repost, which wraps the complete
/// reposted event as its content. The store always holds this row: every
/// `nostr_event_map` entry (which is how the caller got `nostr_event_id`) is
/// written alongside a `store::store_event` call, inbound or outbound.
async fn fetch_stored_event(state: &AppState, nostr_event_id: &str) -> Result<Event, ApiError> {
    let conn = state.db.conn().await;
    let filter = Filter {
        ids: Some(vec![nostr_event_id.to_string()]),
        ..Default::default()
    };
    let events = store::query_events(&conn, std::slice::from_ref(&filter), 1).map_err(|e| {
        tracing::error!("nostr repost: event store lookup error: {e}");
        ApiError::internal("failed to look up the reposted Nostr event")
    })?;
    events
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::not_found("reposted Nostr event not found in the relay store"))
}

/// Decrypt the user's Nostr private key and build a signing keypair.
fn decrypt_keypair(
    state: &AppState,
    account: &super::db::NostrAccount,
) -> Result<Keypair, ApiError> {
    let nest_key = state
        .nest_signing_key
        .as_ref()
        .ok_or_else(|| ApiError::internal("nest signing key not configured"))?;
    let nest_key_bytes = nest_key.to_bytes();

    let encrypted_privkey = account.encrypted_privkey.as_ref().ok_or_else(|| {
        ApiError::bad_request(
            "Nostr interactions require a locally-stored private key (generate or import mode)",
        )
    })?;

    let secret_bytes = key_crypto::decrypt_nostr_privkey(&nest_key_bytes, encrypted_privkey)
        .map_err(|e| {
            tracing::error!("nostr interact decrypt_privkey: {e}");
            ApiError::internal("key decryption error")
        })?;

    Keypair::from_secret_bytes(secret_bytes).map_err(|e| {
        tracing::error!("nostr interact build keypair: {e}");
        ApiError::internal("keypair error")
    })
}

/// Spawn a background task to publish the signed event to each relay
/// (best-effort). Callers check `relay_urls.is_empty()` before calling this —
/// [`resolve_relay_urls`] can now return an explicit empty vec (the
/// explicit-empty semantic flip) — because this task is fire-and-forget with
/// no way to report a zero-relay publish back to the caller's response.
fn spawn_publish(event: Event, relay_urls: Vec<String>, dial_policy: RelayDialPolicy) {
    // spawn-ok(request-scoped): holds no `AppState` and no nest key material —
    // the event arrives already signed, and the task only walks a fixed relay
    // list, each hop bounded by `RelayClient`'s own connect/publish timeouts.
    // Nothing it can do after a rotation depends on the superseded identity.
    tokio::spawn(async move {
        for url in &relay_urls {
            // The user's own publish list, dialed under the nest's relay
            // policy (`state.nostr.relay_dial_policy`) like every other
            // caller-supplied relay.
            match RelayClient::connect(url, dial_policy).await {
                Ok(mut client) => match client.publish(event.clone()).await {
                    Ok(true) => {
                        tracing::info!("nostr: published event {} to {url}", event.id);
                    }
                    Ok(false) => {
                        tracing::warn!("nostr: relay {url} rejected event {}", event.id);
                    }
                    Err(e) => {
                        tracing::warn!("nostr: publish to {url} failed: {e}");
                    }
                },
                Err(e) => {
                    tracing::warn!("nostr: connect to relay {url} failed: {e}");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use fauna_bridge_nostr::types::kind;
    use std::sync::Arc;

    /// **Source: the user's publish relay list.** The interaction fan-out
    /// dials whatever the list names; a loopback entry never makes the nest
    /// open a socket under the production policy — the listener
    /// behind the URL sees no connection.
    #[tokio::test]
    async fn a_listed_relay_at_loopback_is_refused_before_any_tcp_connect() {
        use fauna_bridge_nostr::relay_client::test_support::ProbeListener;
        let probe = ProbeListener::bind().await;
        let kp = Keypair::generate();
        let event = kp.sign_event(UnsignedEvent {
            pubkey: kp.public_key_bytes(),
            created_at: 1_700_000_000,
            kind: kind::REACTION,
            tags: vec![],
            content: "+".into(),
        });
        spawn_publish(event, vec![probe.url.clone()], RelayDialPolicy::PublicOnly);
        assert!(
            !probe
                .saw_a_connection_within(std::time::Duration::from_millis(500))
                .await,
            "the guard must refuse before opening a socket"
        );
    }

    /// Row 15 — the explicit-empty semantic flip: a "like" whose account has
    /// removed every relay (`relay_list: Some("[]")`) must be refused loudly
    /// via the response (never handed to the fire-and-forget `spawn_publish`,
    /// which has no way to report a zero-relay publish back to the caller).
    #[tokio::test]
    async fn like_with_explicit_empty_relay_list_is_refused() {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        crate::nostr::init_db(&db).await.expect("init_db");
        let state = AppState::for_test(db);

        let actor = [0x19u8; 32];
        let actor_hex = hex::encode(actor);
        let kp = fauna_bridge_nostr::signing::Keypair::generate();
        let fauna_post_id = "f".repeat(64);
        let nostr_event_id = "e".repeat(64);
        {
            let conn = state.db.conn().await;
            crate::nostr::db::link_account(
                &conn,
                &actor_hex,
                &kp.public_key_hex(),
                "generated",
                None,
                None,
                Some("[]"),
            )
            .unwrap();
            crate::nostr::db::insert_event_map(
                &conn,
                &fauna_post_id,
                &nostr_event_id,
                &kp.public_key_hex(),
                "out",
            )
            .unwrap();
        }

        let err = route_unified_interaction(&state, &actor_hex, &fauna_post_id, "like")
            .await
            .expect_err("an explicitly empty relay list must be refused, not silently defaulted");
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    }

    const NEST_SEED: [u8; 32] = [42u8; 32];
    const TARGET: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

    /// The door's fixture: a swept note mapped at `TARGET`, the caller linked
    /// in `mode` (custodial key sealed under the nest seed when `custodial`),
    /// with `relays` and the `publish_replies` toggle as given.
    async fn door_state(
        actor_hex: &str,
        custodial: bool,
        relays: Option<&str>,
        publish_replies: bool,
    ) -> AppState {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        crate::nostr::init_db(&db).await.expect("init_db");
        let mut state = AppState::for_test(db);
        state.nest_signing_key = Some(ed25519_dalek::SigningKey::from_bytes(&NEST_SEED));
        let kp = fauna_bridge_nostr::signing::Keypair::generate();
        let sealed = custodial
            .then(|| key_crypto::encrypt_nostr_privkey(&NEST_SEED, &kp.secret_bytes()).unwrap());
        let conn = state.db.conn().await;
        crate::nostr::db::link_account(
            &conn,
            actor_hex,
            &kp.public_key_hex(),
            if custodial { "generate" } else { "remote" },
            sealed.as_deref(),
            None,
            relays,
        )
        .unwrap();
        crate::nostr::db::update_settings(
            &conn,
            actor_hex,
            &crate::nostr::db::NostrSettings {
                publish_replies: Some(publish_replies),
                ..Default::default()
            },
        )
        .unwrap();
        crate::nostr::db::insert_event_map(
            &conn,
            TARGET,
            &"e".repeat(64),
            &"b".repeat(64),
            "inbound",
        )
        .unwrap();
        drop(conn);
        state
    }

    /// `nostr.md` § Replying to and quoting a nostr note → *The door*: the
    /// native ack verbatim when the caller may publish.
    #[tokio::test]
    async fn reply_and_quote_answer_the_native_ack() {
        let actor_hex = hex::encode([0x31u8; 32]);
        let state = door_state(&actor_hex, true, None, true).await;
        for action in ["reply", "quote"] {
            let Json(ack) = route_unified_interaction(&state, &actor_hex, TARGET, action)
                .await
                .unwrap_or_else(|e| panic!("{action} must ack: {}", e.message));
            assert_eq!(
                ack,
                json!({"action": action, "target_post_id": TARGET, "source": "nostr"})
            );
        }
    }

    /// Each refusal names its remedy and is a `400`: an explicitly empty relay
    /// list, a non-custodial signing mode, and — for a reply only —
    /// `publish_replies` off (a quote still acks).
    #[tokio::test]
    async fn reply_door_refusals_name_their_remedy() {
        let actor_hex = hex::encode([0x32u8; 32]);
        let cases: [(bool, Option<&str>, bool, &str); 3] = [
            (true, Some("[]"), true, "relay"),
            (false, None, true, "private key"),
            (true, None, false, "Publish replies"),
        ];
        for (custodial, relays, publish_replies, remedy) in cases {
            let state = door_state(&actor_hex, custodial, relays, publish_replies).await;
            let err = route_unified_interaction(&state, &actor_hex, TARGET, "reply")
                .await
                .expect_err(remedy);
            assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST, "{remedy}");
            assert!(err.message.contains(remedy), "{remedy}: {}", err.message);
        }
        let state = door_state(&actor_hex, true, None, false).await;
        let Json(ack) = route_unified_interaction(&state, &actor_hex, TARGET, "quote")
            .await
            .unwrap_or_else(|e| panic!("publish_replies gates replies only: {}", e.message));
        assert_eq!(ack["action"], "quote");
    }

    #[test]
    fn build_like_event_is_kind_7_with_e_and_p_tags() {
        let pubkey = [7u8; 32];
        let unsigned = build_like_event("target-event-id", "target-pubkey", pubkey, 1234);

        assert_eq!(unsigned.kind, kind::REACTION);
        assert_eq!(unsigned.pubkey, pubkey);
        assert_eq!(unsigned.created_at, 1234);
        assert_eq!(unsigned.content, "+");
        assert!(
            unsigned
                .tags
                .iter()
                .any(|t| t.name() == Some("e") && t.value() == Some("target-event-id"))
        );
        assert!(
            unsigned
                .tags
                .iter()
                .any(|t| t.name() == Some("p") && t.value() == Some("target-pubkey"))
        );
    }

    fn sample_reposted_event() -> Event {
        Event {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: 1000,
            kind: 1,
            tags: vec![],
            content: "the original post".into(),
            sig: "s".repeat(128),
        }
    }

    #[test]
    fn build_repost_event_is_kind_6_wrapping_full_reposted_event() {
        let original = sample_reposted_event();
        let reposter = [9u8; 32];
        let unsigned = build_repost_event(&original, reposter, 2000);

        assert_eq!(unsigned.kind, kind::REPOST);
        assert_eq!(unsigned.pubkey, reposter);
        assert_eq!(unsigned.created_at, 2000);

        // NIP-18: the `e` tag carries a third (empty relay-hint) element, and
        // the content is the full serialized reposted event, not empty.
        let e_tag = unsigned
            .tags
            .iter()
            .find(|t| t.name() == Some("e"))
            .expect("repost must carry an e tag");
        assert_eq!(e_tag.len(), 3, "NIP-18 e tag carries a relay-hint slot");
        assert_eq!(e_tag.value(), Some(original.id.as_str()));
        assert!(
            unsigned
                .tags
                .iter()
                .any(|t| t.name() == Some("p") && t.value() == Some(original.pubkey.as_str()))
        );
        assert!(
            unsigned.content.contains("the original post"),
            "repost content must embed the full reposted event JSON, got: {}",
            unsigned.content
        );
        assert_ne!(unsigned.content, "", "pre-fix behavior had empty content");
    }
}
