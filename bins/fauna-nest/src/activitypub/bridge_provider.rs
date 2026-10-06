//! ActivityPubProvider implementing BridgeProvider trait.

use async_trait::async_trait;
use serde::Deserialize;

use fauna_protocol::Value;

use crate::activitypub::{db_helpers, key_crypto};
use crate::bridge_management::*;
use crate::routes::AppState;

pub struct ActivityPubProvider;

/// Derive the AP username minted at enablement (`activitypub.md` § Identity &
/// keys — user-ratified 2026-07-16).
///
/// - **Handled account** → the Fauna handle **verbatim**: the handle charset
///   (3–63 lowercase alphanumeric + hyphens, no edge hyphens —
///   `public-mode.md` § User Registration) is already valid as a WebFinger
///   local-part and inside Mastodon's remote-username grammar.
/// - **Collision** (a released-and-reclaimed handle whose previous holder
///   froze the bare name, or any other occupant) → lowest free numeric
///   suffix: `alice-2`, `alice-3`, …
/// - **Handle-less account** (admin-created rows may carry no handle) → the
///   legacy first-16-hex-of-actor-id derivation, same suffix rule.
///
/// The result is **frozen at enablement**: a later Fauna handle change never
/// renames the AP actor (the username is baked into `actor_url`, every pushed
/// note id, and every remote server's cache + follow edge; there is no AP
/// rename/`Move` support).
fn derive_username(
    handle: Option<&str>,
    actor_id_hex: &str,
    is_taken: &dyn Fn(&str) -> bool,
) -> String {
    let base = match handle {
        Some(h) if !h.is_empty() => h.to_string(),
        _ => {
            if actor_id_hex.len() >= 16 {
                actor_id_hex[..16].to_string()
            } else {
                actor_id_hex.to_string()
            }
        }
    };
    if !is_taken(&base) {
        return base;
    }
    // The bare name belongs to whoever froze it first; later claimants of the
    // same (reclaimed) handle get a deterministic numeric suffix.
    let mut n: u32 = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if !is_taken(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

// `enable` mode takes no params; `update_settings` accepts the typed
// settings struct via Ipld deserialize. The wire `Value` lookup
// is whatever the client composed.

#[derive(Deserialize, Default)]
struct ApSettingsWire {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    backfill: Option<bool>,
    #[serde(default)]
    auto_accept_follows: Option<bool>,
    #[serde(default)]
    default_visibility: Option<String>,
}

#[async_trait]
impl BridgeProvider for ActivityPubProvider {
    fn id(&self) -> &str {
        "activitypub"
    }
    fn name(&self) -> &str {
        "ActivityPub"
    }

    async fn available(&self, _state: &AppState) -> bool {
        true
    }

    fn supports_follows(&self) -> bool {
        true
    }

    fn supports_follow_requests(&self) -> bool {
        true
    }

    fn link_modes(&self) -> Vec<BridgeLinkMode> {
        vec![BridgeLinkMode {
            mode: "enable".into(),
            label: "Enable federation".into(),
            client_action: None,
            platform: None,
            fields: vec![],
            extra: Default::default(),
        }]
    }

    async fn status(&self, state: &AppState, actor_id: &str) -> Result<BridgeStatus, BridgeError> {
        let conn = state.db.conn().await;
        let acct = db_helpers::get_account(&conn, actor_id)
            .map_err(|e| BridgeError::provider_error(&e.to_string()))?;
        drop(conn);

        let domain = state.handle_domain();

        match acct {
            Some(a) => Ok(BridgeStatus {
                linked: true,
                identity: Some(BridgeIdentity {
                    label: "Handle".into(),
                    value: a.actor_url.clone(),
                    display: format!("@{}@{}", a.username, domain),
                    extra: Default::default(),
                }),
                mode: Some("enabled".into()),
                settings: vec![
                    BridgeSetting {
                        key: "auto_accept_follows".into(),
                        label: "Auto-accept follows".into(),
                        setting_type: "boolean".into(),
                        value: Value::Bool(a.auto_accept_follows),
                        options: None,
                        extra: Default::default(),
                    },
                    BridgeSetting {
                        key: "default_visibility".into(),
                        label: "Default visibility".into(),
                        setting_type: "select".into(),
                        value: Value::String(a.default_visibility),
                        options: Some(vec![
                            BridgeSettingOption {
                                value: Value::String("public".into()),
                                label: "Public".into(),
                                extra: Default::default(),
                            },
                            BridgeSettingOption {
                                value: Value::String("unlisted".into()),
                                label: "Unlisted".into(),
                                extra: Default::default(),
                            },
                            BridgeSettingOption {
                                value: Value::String("followers_only".into()),
                                label: "Followers only".into(),
                                extra: Default::default(),
                            },
                        ]),
                        extra: Default::default(),
                    },
                    BridgeSetting {
                        key: "backfill".into(),
                        label: "Backfill posts".into(),
                        setting_type: "boolean".into(),
                        value: Value::Bool(a.backfill),
                        options: None,
                        extra: Default::default(),
                    },
                ],
                link_modes: None,
            }),
            None => Ok(BridgeStatus {
                linked: false,
                identity: None,
                mode: None,
                settings: vec![],
                link_modes: Some(self.link_modes()),
            }),
        }
    }

    async fn link(
        &self,
        state: &AppState,
        actor_id: &str,
        mode: &str,
        _params: Value,
    ) -> Result<LinkReply, BridgeError> {
        match mode {
            "enable" => {
                // Check not already linked
                let conn = state.db.conn().await;
                if db_helpers::get_account(&conn, actor_id)
                    .map_err(|e| BridgeError::provider_error(&e.to_string()))?
                    .is_some()
                {
                    return Err(BridgeError::already_linked());
                }
                drop(conn);

                // Generate RSA keypair
                let (privkey_der, public_key_pem) =
                    fauna_bridge_activitypub::identity::generate_rsa_keypair().map_err(|e| {
                        BridgeError::provider_error(&format!("key generation error: {e}"))
                    })?;

                // Mint the username: Fauna-handle-derived, frozen at
                // enablement; hex fallback for handle-less accounts
                // (`derive_username` above). The handle read takes its own
                // conn — never held across the derive/create conn below.
                let handle = match fauna_core::hex32::decode(actor_id) {
                    Ok(bytes) => state.db.get_handle(&bytes).await.ok().flatten(),
                    Err(_) => None,
                };

                let domain = state.handle_domain();

                let conn = state.db.conn().await;
                // Seal the private key under the deployment seed `nest_keypair`
                // holds, read on this connection so the read, the seal and the
                // insert below share one hold of the database guard — never a
                // serving generation's copy (`crate::nest_kek`'s module docs):
                // a generation not yet torn down after a deployment-seed
                // rotation still holds the retired seed.
                let encrypted_privkey = crate::nest_kek::require_deployment_seed(&conn)
                    .and_then(|seed| key_crypto::encrypt_rsa_privkey(&seed, &privkey_der))
                    .map_err(|e| BridgeError::provider_error(&format!("encryption error: {e}")))?;
                let username = derive_username(handle.as_deref(), actor_id, &|name| {
                    // DB error → treat as free: a wrong pick then fails
                    // create_account's UNIQUE and surfaces, instead of the
                    // suffix loop spinning forever on "taken".
                    db_helpers::get_account_by_username(&conn, name)
                        .map(|a| a.is_some())
                        .unwrap_or(false)
                });
                let actor_url = format!("https://{domain}/ap/users/{username}");

                db_helpers::create_account(
                    &conn,
                    actor_id,
                    &username,
                    &actor_url,
                    &encrypted_privkey,
                    &public_key_pem,
                )
                .map_err(|e| BridgeError::provider_error(&e.to_string()))?;
                drop(conn);

                Ok(LinkReply {
                    linked: true,
                    identity: Some(BridgeIdentity {
                        label: "Handle".into(),
                        value: actor_url,
                        display: format!("@{username}@{domain}"),
                        extra: Default::default(),
                    }),
                    redirect_url: None,
                    extra: Default::default(),
                })
            }
            other => Err(BridgeError::invalid_mode(other)),
        }
    }

    async fn unlink(&self, state: &AppState, actor_id: &str) -> Result<(), BridgeError> {
        let conn = state.db.conn().await;
        db_helpers::delete_account(&conn, actor_id)
            .map_err(|e| BridgeError::provider_error(&e.to_string()))
    }

    async fn update_settings(
        &self,
        state: &AppState,
        actor_id: &str,
        settings: Value,
    ) -> Result<(), BridgeError> {
        let wire: ApSettingsWire = deserialize_value(&settings)
            .map_err(|e| BridgeError::invalid_params(&format!("settings: {e}")))?;
        let ap_settings = db_helpers::ApSettings {
            enabled: wire.enabled,
            backfill: wire.backfill,
            auto_accept_follows: wire.auto_accept_follows,
            default_visibility: wire.default_visibility,
        };
        let account = {
            let conn = state.db.conn().await;
            db_helpers::update_settings(&conn, actor_id, &ap_settings)
                .map_err(|e| BridgeError::provider_error(&e.to_string()))?;
            db_helpers::get_account(&conn, actor_id)
                .map_err(|e| BridgeError::provider_error(&e.to_string()))?
        };

        // Turning *accept follows by itself* on answers everything waiting
        // (`activitypub.md` § Follow requests). The setting is written first,
        // so a `Follow` arriving meanwhile is accepted at the door; and the
        // sweep runs on every write that leaves the switch on, not only on
        // the off→on edge, so a sweep cut short is finished by saving again.
        if wire.auto_accept_follows == Some(true)
            && let Some(account) = account
        {
            crate::activitypub::inbox_routes::accept_all_pending_follows(state, &account)
                .await
                .map_err(|e| BridgeError::provider_error(&e.to_string()))?;
        }
        Ok(())
    }

    async fn list_follows(
        &self,
        state: &AppState,
        actor_id: &str,
    ) -> Result<Vec<BridgeFollow>, BridgeError> {
        let conn = state.db.conn().await;
        let follows = db_helpers::list_outbound_follows(&conn, actor_id)
            .map_err(|e| BridgeError::provider_error(&e.to_string()))?;
        drop(conn);
        Ok(follows
            .into_iter()
            .map(|f| BridgeFollow {
                id: f.remote_actor_uri,
                petname: None,
                created_at: Some(f.created_at),
                extra: None,
                unknown_keys: Default::default(),
            })
            .collect())
    }

    async fn list_follow_requests(
        &self,
        state: &AppState,
        actor_id: &str,
    ) -> Result<Vec<BridgeFollowRequest>, BridgeError> {
        // A request is the pending inbound `ap_follows` row, read beside the
        // cached remote actor for the requester's name and `@user@host`
        // address (`activitypub.md` § Follow requests).
        let conn = state.db.conn().await;
        let pending = db_helpers::list_pending_inbound_follows(&conn, actor_id)
            .map_err(|e| BridgeError::provider_error(&e.to_string()))?;
        Ok(pending
            .into_iter()
            .map(|f| {
                let remote = db_helpers::get_remote_actor(&conn, &f.remote_actor_uri)
                    .ok()
                    .flatten();
                let handle = remote
                    .as_ref()
                    .and_then(|r| db_helpers::bridge_author_of(r).handle);
                BridgeFollowRequest {
                    id: f.remote_actor_uri,
                    name: remote
                        .and_then(|r| r.display_name)
                        .filter(|n| !n.trim().is_empty()),
                    requested_at: Some(f.created_at),
                    extra: handle.map(|h| {
                        Value::Map(std::collections::BTreeMap::from([(
                            "handle".to_string(),
                            Value::String(h),
                        )]))
                    }),
                    unknown_keys: Default::default(),
                }
            })
            .collect())
    }

    async fn resolve_follow_request(
        &self,
        state: &AppState,
        actor_id: &str,
        id: &str,
        approve: bool,
    ) -> Result<(), BridgeError> {
        let account = {
            let conn = state.db.conn().await;
            db_helpers::get_account(&conn, actor_id)
                .map_err(|e| BridgeError::provider_error(&e.to_string()))?
                .ok_or_else(BridgeError::not_linked)?
        };
        // `id` only selects among this account's own pending rows; the
        // requester and the `Follow` id the answer names are the row's. A
        // request already gone is a success (idempotent).
        crate::activitypub::inbox_routes::resolve_follow_request(state, &account, id, approve)
            .await
            .map(|_| ())
            .map_err(|e| BridgeError::provider_error(&e.to_string()))
    }

    async fn add_follow(
        &self,
        state: &AppState,
        actor_id: &str,
        id: &str,
        _petname: Option<&str>,
        _extra: Option<Value>,
    ) -> Result<(), BridgeError> {
        // `id` is the remote AP actor URI. The local actor URL comes from the
        // stored account row — the username is minted at enablement
        // (handle-derived) and must never be re-derived here.
        let conn = state.db.conn().await;
        let account = db_helpers::get_account(&conn, actor_id)
            .map_err(|e| BridgeError::provider_error(&e.to_string()))?
            .ok_or_else(|| BridgeError::provider_error("activitypub not linked"))?;
        db_helpers::create_follow(&conn, actor_id, id, "outbound", None)
            .map_err(|e| BridgeError::provider_error(&e.to_string()))?;
        drop(conn);

        // Deliver the Follow to the remote actor's inbox.
        let domain = state.handle_domain();
        let local_actor_url = account.actor_url;
        let activity_id = format!(
            "https://{domain}/ap/activities/{}-follow-{}",
            actor_id,
            uuid_v4_hex()
        );
        let follow_json = serde_json::json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "type": "Follow",
            "id": activity_id,
            "actor": local_actor_url,
            "object": id,
        });
        if let Ok(activity_str) = serde_json::to_string(&follow_json) {
            deliver_to_actor_inbox(state, id, &activity_str).await;
        }

        Ok(())
    }

    async fn remove_follow(
        &self,
        state: &AppState,
        actor_id: &str,
        follow_id: &str,
    ) -> Result<(), BridgeError> {
        // The local actor URL comes from the stored account row (username is
        // minted at enablement — see `add_follow`).
        let conn = state.db.conn().await;
        let account = db_helpers::get_account(&conn, actor_id)
            .map_err(|e| BridgeError::provider_error(&e.to_string()))?
            .ok_or_else(|| BridgeError::provider_error("activitypub not linked"))?;
        db_helpers::delete_follow(&conn, actor_id, follow_id, "outbound")
            .map_err(|e| BridgeError::provider_error(&e.to_string()))?;
        drop(conn);

        // Deliver the Undo{Follow} to the remote actor's inbox.
        let domain = state.handle_domain();
        let local_actor_url = account.actor_url;
        let follow_activity_id = format!(
            "https://{domain}/ap/activities/{}-follow-{}",
            actor_id,
            uuid_v4_hex()
        );
        let undo_id = format!(
            "https://{domain}/ap/activities/{}-undo-{}",
            actor_id,
            uuid_v4_hex()
        );
        let undo_json = serde_json::json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "type": "Undo",
            "id": undo_id,
            "actor": local_actor_url,
            "object": {
                "type": "Follow",
                "id": follow_activity_id,
                "actor": local_actor_url,
                "object": follow_id,
            },
        });
        if let Ok(activity_str) = serde_json::to_string(&undo_json) {
            deliver_to_actor_inbox(state, follow_id, &activity_str).await;
        }

        Ok(())
    }
}

/// Resolve a remote actor's personal inbox (cached, else fetched + cached),
/// enqueue the activity durably, and nudge the worker to deliver it now.
/// Best-effort: an unresolvable actor is logged and skipped — the follow edge
/// itself already persisted.
///
/// This replaced an empty-`target_inboxes` delivery-event send whose comment
/// claimed the worker would resolve the inbox — it never did, so outbound
/// Follow/Undo activities were silently dropped (`activitypub.md`
/// § Implementation status).
async fn deliver_to_actor_inbox(state: &AppState, actor_uri: &str, activity_json: &str) {
    let cached = {
        let conn = state.db.conn().await;
        db_helpers::get_remote_actor(&conn, actor_uri)
            .ok()
            .flatten()
    };
    let remote = match cached {
        Some(r) => Some(r),
        None => {
            match crate::activitypub::inbox_routes::fetch_remote_actor(state, actor_uri).await {
                Ok(actor) => {
                    let conn = state.db.conn().await;
                    if let Err(e) = db_helpers::upsert_remote_actor(&conn, &actor) {
                        tracing::warn!(error = %e, "ap: caching fetched remote actor failed");
                    }
                    Some(actor)
                }
                Err(e) => {
                    tracing::warn!(actor = %actor_uri, error = %e, "ap: cannot resolve remote actor for delivery");
                    None
                }
            }
        }
    };
    let Some(remote) = remote else { return };
    // A Follow/Undo addresses one actor: their personal inbox first.
    let inbox = if remote.inbox.is_empty() {
        remote.shared_inbox.unwrap_or_default()
    } else {
        remote.inbox
    };
    if inbox.is_empty() {
        tracing::warn!(actor = %actor_uri, "ap: remote actor has no inbox");
        return;
    }
    let conn = state.db.conn().await;
    if let Err(e) = db_helpers::enqueue_delivery(&conn, activity_json, &inbox) {
        tracing::warn!(error = %e, target_inbox = %inbox, "ap: enqueue delivery failed");
        return;
    }
    drop(conn);
    state.activitypub.delivery_nudge.notify_one();
}

/// Generate a short random hex string for activity IDs.
fn uuid_v4_hex() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Simple non-crypto random using timestamp + thread ID for uniqueness.
    // A full UUID library isn't needed for opaque activity IDs.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tid = std::thread::current().id();
    format!("{nanos:08x}{tid:?}")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(16)
        .collect()
}

#[cfg(test)]
mod derive_username_tests {
    //! The enablement-time username mint (`activitypub.md` § Identity & keys,
    //! user-ratified 2026-07-16): handle verbatim, lowest-free numeric suffix
    //! on collision, legacy 16-hex fallback for handle-less accounts.
    use super::derive_username;

    const HEX: &str = "a3f9c2d41b7e8f05aabbccddeeff00112233445566778899aabbccddeeff0011";

    #[test]
    fn handled_account_gets_the_handle_verbatim() {
        assert_eq!(derive_username(Some("alice"), HEX, &|_| false), "alice");
    }

    #[test]
    fn collision_takes_the_lowest_free_numeric_suffix() {
        // The released-and-reclaimed-handle case: a previous holder froze the
        // bare name (and someone the -2); the new claimant gets -3.
        let taken = |name: &str| name == "alice" || name == "alice-2";
        assert_eq!(derive_username(Some("alice"), HEX, &taken), "alice-3");
    }

    #[test]
    fn handle_less_account_falls_back_to_hex16() {
        assert_eq!(derive_username(None, HEX, &|_| false), "a3f9c2d41b7e8f05");
        assert_eq!(
            derive_username(Some(""), HEX, &|_| false),
            "a3f9c2d41b7e8f05"
        );
    }

    #[test]
    fn hex_fallback_also_suffixes_on_collision() {
        let taken = |name: &str| name == "a3f9c2d41b7e8f05";
        assert_eq!(derive_username(None, HEX, &taken), "a3f9c2d41b7e8f05-2");
    }
}

/// The deployment-seed rotation's hand-off window for the per-account RSA key,
/// driven causally: the ceremony has committed while a serving generation not
/// yet torn down still holds the retired seed (`box-recovery.md`
/// § Deployment-seed rotation → *The bounded hand-off window*). Enabling
/// federation mints the account's key, and when that generation answers the
/// enable the key must still be sealed under the seed the database holds. A key
/// sealed under the retired copy never opens again, and the satellite walk
/// refuses it on every later rotation.
#[cfg(test)]
mod rotation_window {
    use std::sync::Arc;

    use fauna_protocol::Value;
    use zeroize::Zeroizing;

    use super::ActivityPubProvider;
    use crate::bridge_management::BridgeProvider;
    use crate::db::CacheDb;
    use crate::test_support::{every_row_opens_under, seat_deployment_seed, serving_generation};

    #[tokio::test(flavor = "multi_thread")]
    async fn an_enable_in_the_rotation_window_seals_the_key_under_the_successor_seed() {
        let (a, b, c) = (
            Zeroizing::new([0xa1u8; 32]),
            Zeroizing::new([0xb2u8; 32]),
            Zeroizing::new([0xc3u8; 32]),
        );
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
        crate::activitypub::init_db(&db).await.expect("AP schema");
        seat_deployment_seed(&db, &a).await;
        let outgoing = serving_generation(db.clone(), &a);

        db.rotate_deployment_seed(&a, &b)
            .await
            .expect("the ceremony runs")
            .expect("and commits");

        ActivityPubProvider
            .link(&outgoing, &hex::encode([0x44u8; 32]), "enable", Value::Null)
            .await
            .expect("the outgoing generation answers the enable");
        assert!(
            every_row_opens_under(
                &db,
                "ap_accounts",
                "encrypted_privkey",
                crate::nest_kek::ACTIVITYPUB_RSA_CONTEXT,
                &b,
            )
            .await,
            "an enable answered by the outgoing generation sealed the account key under the \
             retired deployment seed"
        );

        db.rotate_deployment_seed(&b, &c)
            .await
            .expect("the next rotation commits — the account key does not wedge the walk")
            .expect("and is no rule refusal");
    }
}
