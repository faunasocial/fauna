// bins/fauna-nest/src/bluesky/bridge_provider.rs

use async_trait::async_trait;
use serde::Deserialize;

use fauna_bridge_atproto::oauth::{AuthorizeOptions, bluesky_scopes};
use fauna_protocol::Value;

use crate::bluesky::db_helpers;
use crate::bridge_management::*;
use crate::routes::AppState;

pub struct BlueskyProvider;

// ── Per-mode link params + per-bridge settings (typed) ─────────────
//
// The wire ships `params` / `settings` as free-form `Value` maps;
// each provider deserializes into the typed shape it cares about. Bluesky
// has one link mode (OAuth) and one setting key (write_through).

#[derive(Deserialize)]
struct OauthLinkParams {
    handle: String,
    #[serde(default)]
    return_url: Option<String>,
}

#[derive(Deserialize)]
struct BlueskySettings {
    #[serde(default)]
    write_through: Option<i64>,
}

#[async_trait]
impl BridgeProvider for BlueskyProvider {
    fn id(&self) -> &str {
        "bluesky"
    }
    fn name(&self) -> &str {
        "Bluesky"
    }

    async fn available(&self, state: &AppState) -> bool {
        state.bluesky_oauth().is_some()
    }

    async fn unavailable_reason(&self, state: &AppState) -> Option<String> {
        // Only one thing can make this bridge unavailable on a nest that
        // compiled it in: the deployment has no public identity domain, so the
        // `client_id` Bluesky's authorization server must fetch by name does
        // not derive (`bluesky::oauth_public_url`). Say that, and say what
        // would change it — a card that is disabled for an unexplained reason
        // is the shape `bridges.md` § A bridge that cannot be linked right now
        // exists to forbid.
        (!self.available(state).await).then(|| {
            "Linking Bluesky needs this nest to have a public domain name — \
             Bluesky's login service has to reach this nest by name to \
             complete the sign-in. Give the nest a domain and this becomes \
             available."
                .to_string()
        })
    }

    fn supports_follows(&self) -> bool {
        false
    }

    fn link_modes(&self) -> Vec<BridgeLinkMode> {
        vec![BridgeLinkMode {
            mode: "oauth".into(),
            label: "Link via OAuth".into(),
            client_action: Some("oauth_redirect".into()),
            platform: None,
            fields: vec![BridgeLinkField {
                key: "handle".into(),
                label: "Bluesky handle".into(),
                field_type: "text".into(),
                placeholder: Some("yourname.bsky.social".into()),
                extra: Default::default(),
            }],
            extra: Default::default(),
        }]
    }

    async fn status(&self, state: &AppState, actor_id: &str) -> Result<BridgeStatus, BridgeError> {
        let conn = state.db.conn().await;
        let acct = db_helpers::get_linked_account(&conn, actor_id)
            .map_err(|e| BridgeError::provider_error(&e.to_string()))?;
        drop(conn);

        let available = self.available(state).await;
        match acct {
            Some(a) => {
                let conn = state.db.conn().await;
                let wt = db_helpers::get_write_through(&conn, actor_id).unwrap_or(0);
                drop(conn);

                Ok(BridgeStatus {
                    linked: true,
                    identity: Some(BridgeIdentity {
                        label: "Handle".into(),
                        value: a.bluesky_did,
                        display: format!("@{}", a.bluesky_handle),
                        extra: Default::default(),
                    }),
                    mode: Some("oauth".into()),
                    settings: vec![BridgeSetting {
                        key: "write_through".into(),
                        label: "Cross-post to Bluesky".into(),
                        setting_type: "select".into(),
                        value: Value::Integer(i128::from(wt)),
                        options: Some(vec![
                            BridgeSettingOption {
                                value: Value::Integer(i128::from(0i64)),
                                label: "Disabled".into(),
                                extra: Default::default(),
                            },
                            BridgeSettingOption {
                                value: Value::Integer(i128::from(1i64)),
                                label: "Auto".into(),
                                extra: Default::default(),
                            },
                            BridgeSettingOption {
                                value: Value::Integer(i128::from(2i64)),
                                label: "Manual".into(),
                                extra: Default::default(),
                            },
                        ]),
                        extra: Default::default(),
                    }],
                    link_modes: None,
                })
            }
            None => Ok(BridgeStatus {
                linked: false,
                identity: None,
                mode: None,
                settings: vec![],
                link_modes: if available {
                    Some(self.link_modes())
                } else {
                    None
                },
            }),
        }
    }

    async fn link(
        &self,
        state: &AppState,
        actor_id: &str,
        mode: &str,
        params: Value,
    ) -> Result<LinkReply, BridgeError> {
        if mode != "oauth" {
            return Err(BridgeError::invalid_mode(mode));
        }
        let conn = state.db.conn().await;
        if db_helpers::get_linked_account(&conn, actor_id)
            .map_err(|e| BridgeError::provider_error(&e.to_string()))?
            .is_some()
        {
            return Err(BridgeError::already_linked());
        }
        drop(conn);

        let oauth = state.bluesky_oauth().ok_or_else(BridgeError::unavailable)?;
        let p: OauthLinkParams = deserialize_value(&params)
            .map_err(|e| BridgeError::invalid_params(&format!("handle required: {e}")))?;
        // Clamp the client-supplied return_url to a same-origin relative path
        // (open-redirect guard, defense-in-depth — the load-bearing check is
        // at the callback use-site, since `state` is attacker-controllable there).
        let return_url =
            super::auth_routes::sanitize_return_url(p.return_url.as_deref().unwrap_or("/bridges"));

        let oauth_state = format!("{}|{}", actor_id, return_url);
        let options = AuthorizeOptions {
            scopes: bluesky_scopes(),
            state: Some(oauth_state),
            ..Default::default()
        };

        let url = oauth
            .authorize(&p.handle, options)
            .await
            .map_err(|e| BridgeError::provider_error(&format!("authorization failed: {e}")))?;

        Ok(LinkReply {
            linked: false,
            identity: None,
            redirect_url: Some(url),
            extra: Default::default(),
        })
    }

    async fn unlink(&self, state: &AppState, actor_id: &str) -> Result<(), BridgeError> {
        let conn = state.db.conn().await;
        db_helpers::delete_linked_account(&conn, actor_id)
            .map_err(|e| BridgeError::provider_error(&e.to_string()))
    }

    async fn update_settings(
        &self,
        state: &AppState,
        actor_id: &str,
        settings: Value,
    ) -> Result<(), BridgeError> {
        let s: BlueskySettings = deserialize_value(&settings)
            .map_err(|e| BridgeError::invalid_params(&format!("settings: {e}")))?;
        if let Some(wt) = s.write_through {
            if ![0, 1, 2].contains(&wt) {
                return Err(BridgeError::invalid_params(
                    "write_through must be 0, 1, or 2",
                ));
            }
            let conn = state.db.conn().await;
            db_helpers::set_write_through(&conn, actor_id, wt)
                .map_err(|e| BridgeError::provider_error(&e.to_string()))?;
        }
        Ok(())
    }

    async fn list_follows(
        &self,
        _state: &AppState,
        _actor_id: &str,
    ) -> Result<Vec<BridgeFollow>, BridgeError> {
        Ok(vec![])
    }

    async fn add_follow(
        &self,
        _state: &AppState,
        _actor_id: &str,
        _id: &str,
        _petname: Option<&str>,
        _extra: Option<Value>,
    ) -> Result<(), BridgeError> {
        Err(BridgeError::invalid_params(
            "Bluesky does not support bridge follows",
        ))
    }

    async fn remove_follow(
        &self,
        _state: &AppState,
        _actor_id: &str,
        _follow_id: &str,
    ) -> Result<(), BridgeError> {
        Err(BridgeError::invalid_params(
            "Bluesky does not support bridge follows",
        ))
    }
}

#[cfg(test)]
mod availability_tests {
    //! The derivation, at the provider boundary the wire actually reads.
    //!
    //! `bluesky::oauth_public_url`'s own tests pin the URL rule; these pin what
    //! the *bridge* does with it — that a box with no public identity domain
    //! reports itself unavailable **with a reason** rather than silently
    //! (`bridges.md` § A bridge that cannot be linked right now, rule 2), and
    //! that a claimed one reports available. Headless: no nest boots, no
    //! network, no OAuth round.
    use super::*;
    use crate::bridge_management::BridgeProvider;
    use crate::db::CacheDb;
    use std::sync::Arc;

    async fn state_claimed_onto(domain: Option<&str>) -> AppState {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        let state = AppState::for_test(Arc::clone(&db));
        if let Some(d) = domain {
            state.identity_domain.store(Some(Arc::new(d.to_string())));
        }
        state
    }

    #[tokio::test]
    async fn a_domainless_nest_reports_bluesky_unavailable_with_a_reason() {
        let state = state_claimed_onto(None).await;
        assert!(
            !BlueskyProvider.available(&state).await,
            "a box with no identity domain cannot present a client_id Bluesky \
             can resolve, so the bridge must not claim to be linkable"
        );
        let reason = BlueskyProvider
            .unavailable_reason(&state)
            .await
            .expect("an unavailable Bluesky bridge owes the user a reason");
        assert!(
            reason.contains("domain"),
            "the reason must name the missing domain — it is rendered verbatim \
             beside the disabled Link control; got {reason:?}"
        );
    }

    #[tokio::test]
    async fn a_localhost_nest_is_unavailable_too() {
        // `handle_domain()`'s placeholder is not a real domain, and this is the
        // shape every un-domained dev/e2e box runs in.
        let state = state_claimed_onto(Some("localhost")).await;
        assert!(!BlueskyProvider.available(&state).await);
    }

    #[tokio::test]
    async fn an_unlinked_actor_on_a_domainless_nest_gets_no_link_modes() {
        // `link_block_of` blocks on the mode count, and renders the reason only
        // because it is zero here. If `status()` ever advertised modes on an
        // unavailable box, the user would get a live button that cannot work.
        let state = state_claimed_onto(None).await;
        crate::bluesky::init_db(&state.db).await.expect("schema");
        let status = BlueskyProvider
            .status(&state, &"11".repeat(32))
            .await
            .expect("status on an unlinked actor");
        assert!(!status.linked);
        assert!(
            status.link_modes.is_none(),
            "an unavailable bridge advertises no link mode; got {:?}",
            status.link_modes
        );
    }

    #[tokio::test]
    async fn a_domained_nest_derives_a_client_and_offers_the_oauth_mode() {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        crate::bluesky::init_db(&db).await.expect("schema");
        let state = AppState::for_test(Arc::clone(&db));
        state
            .identity_domain
            .store(Some(Arc::new("nest.example.com".to_string())));
        // A real deployment's keypair path; `load_or_generate` mints one on
        // first use exactly as it does beside a live nest's database.
        let dir = tempfile::tempdir().expect("tempdir");
        let seeded = crate::state::BlueskyState::new(dir.path().join("key.json"), Arc::clone(&db));
        let state = AppState {
            bluesky: seeded,
            ..state
        };

        assert!(
            BlueskyProvider.available(&state).await,
            "a nest claimed onto a public domain derives its OAuth client with \
             no flag, no env and no config file"
        );
        assert!(
            BlueskyProvider.unavailable_reason(&state).await.is_none(),
            "an available bridge has nothing to explain"
        );
        let client = state.bluesky_oauth().expect("derived client");
        assert_eq!(
            client.client_metadata.client_id,
            "https://nest.example.com/.well-known/atproto-oauth-client",
            "the client_id is the deployment's own identity domain"
        );

        let status = BlueskyProvider
            .status(&state, &"22".repeat(32))
            .await
            .expect("status on an unlinked actor");
        let modes = status.link_modes.expect("an available bridge offers modes");
        assert!(modes.iter().any(|m| m.mode == "oauth"));
    }

    #[tokio::test]
    async fn the_client_is_rebuilt_when_the_identity_domain_moves() {
        // The reason this is derived per read rather than built once: the
        // domain is learned at claim and can change afterwards, and a stale
        // client_id is one Bluesky's authorization server can no longer fetch.
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        let dir = tempfile::tempdir().expect("tempdir");
        let state = AppState {
            bluesky: crate::state::BlueskyState::new(dir.path().join("key.json"), Arc::clone(&db)),
            ..AppState::for_test(Arc::clone(&db))
        };

        state
            .identity_domain
            .store(Some(Arc::new("first.example.com".to_string())));
        let first = state.bluesky_oauth().expect("derived for the first domain");
        assert_eq!(
            first.client_metadata.client_id,
            "https://first.example.com/.well-known/atproto-oauth-client"
        );

        state
            .identity_domain
            .store(Some(Arc::new("second.example.com".to_string())));
        let second = state
            .bluesky_oauth()
            .expect("re-derived for the new domain");
        assert_eq!(
            second.client_metadata.client_id,
            "https://second.example.com/.well-known/atproto-oauth-client",
            "a domain change must re-mint the client_id, not serve the old one"
        );
    }
}
