//! The consent starts' user half — `fauna.oauth.consent.{lookup_code,
//! open_handoff,block_client,list_blocked_clients}` (`authorization-server.md`
//! § Consent).
//!
//! All four are User-class and self-scoped: the caller acts on its own
//! account's consent rows and blocks and nobody else's. The rows themselves
//! are opened by the one owner every start shares
//! ([`crate::bridge_atproto_handlers::open_consent_request`]) and answered by
//! the built card's `fauna.bridges.atproto.resolve_consent`; what lives here is
//! only what the newer starts added on the user's side.

use std::sync::Arc;
use std::time::Duration;

use fauna_protocol::{
    decode_strict as decode,
    oauth_consent::{
        BlockClientReply, BlockClientRequest, BlockedClient, ListBlockedClientsReply,
        ListBlockedClientsRequest, LookupConsentCodeReply, LookupConsentCodeRequest,
        OpenHandoffReply, OpenHandoffRequest,
    },
};

use crate::bridge_atproto_handlers::ConsentStart;
use crate::bridge_method_allowlist::require_permission_default as require_permission;
use crate::db::atproto_pds::ConsentRequestRow;
use crate::oauth_as_ceremony::{BackchannelFlow, BackchannelStart};
use crate::routes::AppState;
use crate::rpc_errors::{encode_reply, internal, malformed};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// `fauna.oauth.consent.lookup_code` (User) — the typed-code start's door.
///
/// Claims the live typed-code row the user's code names to the caller's
/// account and answers the row the card renders; from then on it is an
/// ordinary pending row of that account — listed by `list_pending_consents`
/// and answered by `resolve_consent`. A miss of any kind is `None`, never an
/// error, so the reply says nothing about codes the caller does not hold.
fn lookup_code_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.oauth.consent.lookup_code").await?;
            let req: LookupConsentCodeRequest = decode(&payload).map_err(malformed)?;
            let claimed = state
                .db
                .claim_typed_consent_code(&req.user_code, &actor_id)
                .await
                .map_err(internal)?;
            encode_reply(&LookupConsentCodeReply {
                consent: claimed
                    .as_ref()
                    .map(crate::bridge_atproto_handlers::pending_consent_row),
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.oauth.consent.open_handoff` (User) — the same-device handoff's door
/// (`authorization-server.md` § Consent → *How the same-device handoff is
/// built*).
///
/// Spends the pushed request a `fauna://consent/<request_uri>` route carried
/// and answers the row the card renders, opened assigned to the caller; from
/// then on it is an ordinary pending row of that account. A miss of any kind
/// is `None`, never an error — the `lookup_code` rule.
fn open_handoff_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.oauth.consent.open_handoff").await?;
            let req: OpenHandoffRequest = decode(&payload).map_err(malformed)?;
            let opened = open_handoff(&state, actor_id, &req.request_uri)
                .await
                .map_err(internal)?;
            encode_reply(&OpenHandoffReply {
                consent: opened
                    .as_ref()
                    .map(crate::bridge_atproto_handlers::pending_consent_row),
                extra: Default::default(),
            })
        })
    })
}

/// Open the handoff the caller's app was routed to — the kind's whole act,
/// apart from decoding.
///
/// ⚠ **The PAR is consumed BEFORE anything else is judged**, exactly as
/// `/oauth/authorize` consumes it: whichever door reaches the handle first
/// spends it, so an authenticated caller probing handles can at worst spend
/// one. A `login_hint` the pushed request carried must name the caller — across
/// both doors the hint means the account this request is for.
///
/// ⚠ **The poll is bridged across the open.** From the instant the PAR store
/// stops holding the handle, the polled-start store must, or a device poll
/// landing while the row is written would read the handle unknown and be told
/// `invalid_grant`. A flow with no consent row polls `authorization_pending`,
/// which is the truth at that instant; a miss removes it again, so the handle
/// then reads as the spent handle it is.
pub(crate) async fn open_handoff(
    state: &Arc<AppState>,
    actor: [u8; 32],
    request_uri: &str,
) -> anyhow::Result<Option<ConsentRequestRow>> {
    // Any shape the route grammar would not mint is a miss before it touches
    // the store — the same rule the app applied when it parsed the route.
    if !fauna_core::app_route::is_par_handle(request_uri) {
        return Ok(None);
    }
    let runtime = &state.oauth_as;
    let now = fauna_core::data::Timestamp::now_secs_or_zero();
    let Some(par) = runtime.par.take(request_uri, now) else {
        return Ok(None);
    };
    let flow = |consent_id: Option<Vec<u8>>, expires: i64| {
        BackchannelFlow::new(
            BackchannelStart::Handoff,
            consent_id,
            par.request.client_id.clone(),
            par.dpop_jkt.clone(),
            par.attested,
            expires,
        )
        .with_code_challenge(par.request.code_challenge.clone())
    };
    runtime
        .backchannel
        .put(request_uri.to_string(), flow(None, par.expires));

    let opened = async {
        if let Some(hint) = par.request.login_hint.as_deref()
            && crate::bridge_atproto_handlers::resolve_identifier(state, hint).await? != Some(actor)
        {
            return Ok(None);
        }
        crate::bridge_atproto_handlers::open_consent_request(
            state,
            ConsentStart::Handoff { actor },
            &par.request.client_id,
            crate::oauth_as_routes::consent_client_name(&par.client),
            &par.request.scopes,
            &crate::oauth_as_routes::consent_sets(&par.request),
            &crate::oauth_as_routes::consent_binding(&par.client, par.attested),
        )
        .await
    }
    .await;
    match opened {
        Ok(Some(row)) => {
            runtime.backchannel.put(
                request_uri.to_string(),
                flow(Some(row.consent_id.clone()), row.expires_at / 1000),
            );
            Ok(Some(row))
        }
        missed => {
            runtime.backchannel.take(request_uri);
            missed
        }
    }
}

/// `fauna.oauth.consent.block_client` (User) — rule (c)'s "never show requests
/// from this app", and its lifting. Nothing else changes when it is set: a
/// request already on the card stays answerable, and a blocked client's next
/// quiet push opens nothing.
fn block_client_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.oauth.consent.block_client").await?;
            let req: BlockClientRequest = decode(&payload).map_err(malformed)?;
            if req.client_id.is_empty()
                || req.client_id.len() > crate::db::atproto_pds::MAX_CONSENT_FIELD_LEN
            {
                return Err(malformed(
                    "client_id must be non-empty and within the consent field cap",
                ));
            }
            let blocked = state
                .db
                .set_oauth_client_block(&actor_id, &req.client_id, req.blocked)
                .await
                .map_err(internal)?;
            encode_reply(&BlockClientReply {
                blocked,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.oauth.consent.list_blocked_clients` (User) — the block's read half,
/// so a block set from a card can be seen and lifted from the user's own app.
fn list_blocked_clients_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(
                &state,
                &actor_id,
                "fauna.oauth.consent.list_blocked_clients",
            )
            .await?;
            let _req: ListBlockedClientsRequest = decode(&payload).map_err(malformed)?;
            let rows = state
                .db
                .list_oauth_client_blocks(&actor_id)
                .await
                .map_err(internal)?;
            encode_reply(&ListBlockedClientsReply {
                clients: rows
                    .into_iter()
                    .map(|(client_id, blocked_at)| BlockedClient {
                        client_id,
                        blocked_at,
                        extra: Default::default(),
                    })
                    .collect(),
                extra: Default::default(),
            })
        })
    })
}

/// Register the `fauna.oauth.consent.*` kinds with the WS-RPC router.
pub fn register_oauth_consent_handlers(b: &mut RpcRouterBuilder) {
    for (kind, handler) in [
        ("fauna.oauth.consent.lookup_code", lookup_code_handler()),
        ("fauna.oauth.consent.open_handoff", open_handoff_handler()),
        ("fauna.oauth.consent.block_client", block_client_handler()),
        (
            "fauna.oauth.consent.list_blocked_clients",
            list_blocked_clients_handler(),
        ),
    ] {
        b.add(
            kind,
            RpcKindMeta {
                forbid_replay: false,
                default_deadline: Duration::from_secs(5),
                handler,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth_as_ceremony::BackchannelLookup;
    use crate::oauth_as_state::StoredParRequest;
    use bytes::Bytes;
    use fauna_bridge_atproto::oauth_client::ResolvedClient;
    use fauna_bridge_atproto::oauth_par::AcceptedParRequest;

    const USER: [u8; 32] = [42u8; 32];
    const OTHER: [u8; 32] = [43u8; 32];
    const HANDLE: &str = "urn:ietf:params:oauth:request_uri:open-handoff-test";
    const CLIENT: &str = "https://app.example/client-metadata.json";

    fn par(login_hint: Option<&str>) -> StoredParRequest {
        StoredParRequest {
            request: AcceptedParRequest {
                client_id: CLIENT.into(),
                redirect_uri: "https://app.example/cb".into(),
                scopes: vec!["atproto".into()],
                sets: vec![],
                state: "csrf".into(),
                code_challenge: "challenge".into(),
                login_hint: login_hint.map(str::to_string),
                nonce: None,
            },
            client: ResolvedClient {
                client_id: CLIENT.into(),
                client_name: Some("Example App".into()),
                client_uri: None,
                logo_uri: None,
                tos_uri: None,
                policy_uri: None,
                redirect_uris: vec!["https://app.example/cb".into()],
                declared_scopes: vec!["atproto".into()],
                confidential: false,
                jwks: vec![],
                jwks_uri: None,
                loopback: false,
                fauna_manifest: None,
            },
            dpop_jkt: "thumbprint".into(),
            attested: crate::db::third_party_principals::AttestedKeys {
                holder_x25519: Some([7u8; 32]),
                writer_ed25519: None,
            },
            expires: i64::MAX,
        }
    }

    async fn seeded() -> Arc<AppState> {
        let state = crate::test_support::fixture_state();
        state.db.create_user(&USER, "free", "test").await.unwrap();
        state.db.set_handle(&USER, "alice").await.unwrap();
        state.db.create_user(&OTHER, "free", "test").await.unwrap();
        state.db.set_handle(&OTHER, "bob").await.unwrap();
        state
    }

    /// Through the registered handler, as an app calls it.
    async fn call(state: &Arc<AppState>, actor: [u8; 32], request_uri: &str) -> OpenHandoffReply {
        let req = OpenHandoffRequest {
            request_uri: request_uri.into(),
            extra: Default::default(),
        };
        let payload = Bytes::from(fauna_protocol::encode_canonical(&req).unwrap().to_vec());
        let out = open_handoff_handler()(state.clone(), actor, payload)
            .await
            .expect("open_handoff answers");
        decode(&out).unwrap()
    }

    /// The kind spends the pushed request — a second open and the browser
    /// door both miss — and answers the card of a row assigned to the caller
    /// alone, which the device's poll is now bound to.
    #[tokio::test]
    async fn opening_spends_the_par_and_assigns_the_row_to_the_caller() {
        let state = seeded().await;
        let (_conn, mut rx) = state.ws.subscribe(USER);
        state.oauth_as.par.put(HANDLE.into(), par(None), 0);

        let card = call(&state, USER, HANDLE).await.consent.expect("the card");
        assert_eq!(card.client_id, CLIENT);
        assert_eq!(card.client_name.as_deref(), Some("Example App"));
        assert!(
            rx.try_recv().is_ok(),
            "the user asked for this card — it notifies"
        );

        assert!(call(&state, USER, HANDLE).await.consent.is_none());
        assert!(
            state.oauth_as.par.take(HANDLE, 0).is_none(),
            "the browser door finds it spent"
        );

        let mine = state
            .db
            .list_pending_atproto_consent_requests(&USER)
            .await
            .unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(
            mine[0].start,
            crate::db::atproto_pds::ConsentStartKind::Handoff
        );
        assert!(
            state
                .db
                .list_pending_atproto_consent_requests(&OTHER)
                .await
                .unwrap()
                .is_empty(),
            "listed to the opening account only"
        );

        let BackchannelLookup::Live(flow) =
            state
                .oauth_as
                .backchannel
                .get(HANDLE, BackchannelStart::Handoff, 0)
        else {
            panic!("the poll's flow is in place")
        };
        assert_eq!(flow.consent_id.as_deref(), Some(&card.consent_id[..]));
        assert_eq!(flow.code_challenge.as_deref(), Some("challenge"));
        assert_eq!(flow.attested.holder_x25519, Some([7u8; 32]));
        assert_eq!(flow.dpop_jkt, "thumbprint");
    }

    /// Every miss is the one empty answer: a hint naming another account
    /// (which still spends the handle), an unknown handle, a malformed one.
    #[tokio::test]
    async fn every_miss_is_one_empty_answer() {
        let state = seeded().await;
        state.oauth_as.par.put(HANDLE.into(), par(Some("bob")), 0);
        let miss = call(&state, USER, HANDLE).await;
        assert_eq!(miss, OpenHandoffReply::default());
        assert!(
            state.oauth_as.par.take(HANDLE, 0).is_none(),
            "spent by the try"
        );
        assert!(matches!(
            state
                .oauth_as
                .backchannel
                .get(HANDLE, BackchannelStart::Handoff, 0),
            BackchannelLookup::Unknown
        ));
        assert!(
            state
                .db
                .list_pending_atproto_consent_requests(&USER)
                .await
                .unwrap()
                .is_empty()
        );

        assert_eq!(
            call(&state, USER, HANDLE).await,
            OpenHandoffReply::default()
        );
        for malformed in ["", "not-a-handle", "urn:ietf:params:oauth:request_uri:a/b"] {
            assert_eq!(
                call(&state, USER, malformed).await,
                OpenHandoffReply::default(),
                "{malformed:?}"
            );
        }

        // The hint naming the caller opens as usual.
        state.oauth_as.par.put(HANDLE.into(), par(Some("alice")), 0);
        assert!(call(&state, USER, HANDLE).await.consent.is_some());
    }
}
