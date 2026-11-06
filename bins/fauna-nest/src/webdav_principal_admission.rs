//! `fauna.bridges.webdav_admit_principal` — the WebDAV bearer door's
//! admission relay (`webdav-server.md` § Key model → *A principal's read*,
//! (6); `authorization-server.md` § Scope grammar → *The read arm*).
//!
//! The read arm `fauna:folder:read:<id>` is the family's first bridge-doored
//! arm: its token is honoured at the MDA's WebDAV server, not at the nest's
//! dispatch. The MDA does not verify the token itself — it hands the token,
//! the DPoP proof and the request's method and URL to this kind, and the nest
//! runs the ONE principal admission its own doors run
//! ([`crate::principal_session::admit_presentation`]: the issuer verifying its
//! own token, a revoked family refused), then the per-call reach resolution
//! every principal kind runs ([`crate::principal_handlers::resolve_principal`]:
//! the row, the account's authority, the external-apps switch). On top it
//! answers, per `folder:read` scope, whether a LIVE `content.read{folder, set}`
//! grant owned by the account and held by the principal's holder key exists —
//! the deposit door's re-resolution, kept where the grant rows are — and
//! whether the folder is served now, the door's backstop.
//!
//! A refusal is not an RPC error: the reply carries the challenge the nest's
//! own door would have answered (status, `WWW-Authenticate`, the fresh
//! `DPoP-Nonce`), which the MDA relays verbatim — a DAV client that sent no
//! nonce learns one exactly as at the deposit door.

use std::sync::Arc;
use std::time::Duration;

use axum::http::header::WWW_AUTHENTICATE;
use axum::response::Response;
use fauna_protocol::wrapped_blob::{
    WebdavAdmitPrincipalReply, WebdavAdmitPrincipalRequest, WebdavAdmittedFolder,
    WebdavAdmittedPrincipal,
};
use fauna_protocol::{RpcError, decode_strict as decode};

use crate::principal_session::{Presentation, admit_presentation};
use crate::routes::AppState;
use crate::rpc_errors::{encode_reply, internal, malformed};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// The kind — one string with the grammar's [`Door::Mda`] for the read arm.
///
/// [`Door::Mda`]: fauna_bridge_atproto::fauna_scope::Door::Mda
pub const KIND: &str = fauna_bridge_atproto::fauna_scope::WEBDAV_ADMIT_PRINCIPAL_KIND;

/// The only path a relayed proof may name: the DAV door's own tree. A proof
/// minted for any other door — the deposit door, the record door — is refused
/// before the admission runs, so the relay never vouches it.
const WEBDAV_PATH_PREFIX: &str = "/webdav/";

/// Does `htu` name the WebDAV tree over `https`? The path is the part after
/// the authority; the authority is checked against the nest's own DAV
/// listener by [`binds_dav_origin`].
fn is_webdav_htu(htu: &str) -> bool {
    let Some(rest) = htu.strip_prefix("https://") else {
        return false;
    };
    let path = rest.find('/').map_or("", |i| &rest[i..]);
    path.starts_with(WEBDAV_PATH_PREFIX)
}

/// The authority (`host` or `host:port`) of an `https` `htu`.
fn htu_authority(htu: &str) -> Option<&str> {
    let rest = htu.strip_prefix("https://")?;
    Some(rest.find('/').map_or(rest, |i| &rest[..i]))
}

/// Does the proof's authority name the nest's own DAV listener at `origin`
/// ([`crate::well_known_dav::dav_origin`], the one answer to *where the DAV
/// listener answers*)? The host is compared case-insensitively; the port is
/// the origin's, or `443` when the origin names none (`mail.<primary>` behind
/// the SNI router). The `Host` header the MDA builds `htu` from is the DAV
/// client's choice, so without this a proof would carry no binding to this
/// server — replay stays stopped by `ath`, the `jti` cache and the audience.
fn binds_dav_origin(authority: &str, origin: &str) -> bool {
    let parse = |s: &str| {
        s.parse::<axum::http::uri::Authority>()
            .ok()
            .filter(|a| !a.as_str().contains('@') && !a.host().is_empty())
    };
    let (Some(got), Some(want)) = (parse(authority), parse(origin)) else {
        return false;
    };
    got.host().eq_ignore_ascii_case(want.host())
        && got.port_u16().unwrap_or(443) == want.port_u16().unwrap_or(443)
}

/// Where this nest's DAV listener answers for a request that named
/// `authority`: `None` when the nest serves no DAV host (no primary mail
/// domain) or knows no name for it.
async fn dav_origin_for(state: &AppState, authority: &str) -> Result<Option<String>, RpcError> {
    let Some(primary) = state
        .db
        .lookup_primary_mail_domain()
        .await
        .map_err(internal)?
    else {
        return Ok(None);
    };
    let port = state
        .db
        .get_caldav_port()
        .await
        .map_err(internal)?
        .unwrap_or(fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT);
    Ok(crate::well_known_dav::dav_origin(
        &primary.domain_name,
        state.dav_direct_listener,
        Some(authority),
        port,
    ))
}

/// The challenge a refused admission answered with, lifted off the rendered
/// response so the MDA can answer the DAV client with the same one.
fn refusal(response: &Response) -> WebdavAdmitPrincipalReply {
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    WebdavAdmitPrincipalReply {
        admitted: None,
        status: response.status().as_u16(),
        www_authenticate: header(WWW_AUTHENTICATE.as_str()),
        dpop_nonce: header("dpop-nonce"),
        extra: Default::default(),
    }
}

/// The `invalid_token` answer for a principal the reach resolution refused —
/// the same opaque shape the admission gives a revoked row.
fn invalid_token(nonce: Option<String>) -> WebdavAdmitPrincipalReply {
    WebdavAdmitPrincipalReply {
        admitted: None,
        status: 401,
        www_authenticate: Some("DPoP error=\"invalid_token\"".to_string()),
        dpop_nonce: nonce,
        extra: Default::default(),
    }
}

/// Does any of `blobs` carry the folder read tuple over `name_hash` whose
/// window is open at `now`?
fn any_grant_admits_read<'a>(
    blobs: impl IntoIterator<Item = &'a [u8]>,
    name_hash: &[u8],
    now: i64,
) -> bool {
    blobs.into_iter().any(|blob| {
        fauna_mls::wrapped_blob::GrantBlob::from_canonical_bytes(blob).is_ok_and(|grant| {
            fauna_mls::wrapped_blob::grant_window_is_open(&grant, now)
                && grant.scope.iter().any(|t| t.is_folder_read_for(name_hash))
        })
    })
}

async fn admit(
    state: &Arc<AppState>,
    req: WebdavAdmitPrincipalRequest,
) -> Result<WebdavAdmitPrincipalReply, RpcError> {
    if !is_webdav_htu(&req.htu) {
        return Err(malformed("htu must name a path under /webdav/"));
    }
    let authority = htu_authority(&req.htu).unwrap_or_default();
    let bound = dav_origin_for(state, authority)
        .await?
        .is_some_and(|origin| binds_dav_origin(authority, &origin));
    if !bound {
        return Err(malformed("htu must name this nest's DAV host"));
    }
    // The failed-credential throttle keys on the DAV client's address as the
    // MDA saw it, beside the claimed client, as at the nest's own doors — so
    // forged tokens naming an app's `client_id` spend only their own source's
    // bucket. An older MDA sends none, and its refusals key on the claim alone.
    let source = req
        .source_ip
        .as_deref()
        .and_then(|s| s.trim().parse::<std::net::IpAddr>().ok());
    let presentation = Presentation {
        token: req.token,
        proofs: req.dpop_proofs,
    };
    let admission =
        match admit_presentation(state, Some(&presentation), &req.htm, &req.htu, source).await {
            Ok(admission) => admission,
            Err(response) => return Ok(refusal(&response)),
        };
    admitted_reply(state, &admission).await
}

/// The half after a sound token: the per-call reach resolution, then each
/// `folder:read` scope's grant liveness and served state.
async fn admitted_reply(
    state: &Arc<AppState>,
    admission: &crate::principal_session::Admission,
) -> Result<WebdavAdmitPrincipalReply, RpcError> {
    let Some(caller) = crate::principal_handlers::resolve_principal(state, &admission.binding)
        .await
        .map_err(internal)?
    else {
        return Ok(invalid_token(None));
    };
    let now = crate::db::now_epoch_secs();
    let grants = match caller.holder_x25519 {
        Some(holder) => state
            .db
            .fetch_capability_grants_for_holder_owned_by(&caller.account, &holder, now)
            .await
            .map_err(internal)?,
        None => Vec::new(),
    };
    let mut folders = Vec::new();
    for folder_id in caller
        .scopes
        .iter()
        .filter_map(|s| fauna_bridge_atproto::fauna_scope::folder_read_qualifier(s))
    {
        let row = state
            .db
            .get_folder_by_id(folder_id)
            .await
            .map_err(internal)?
            .filter(|f| f.actor_id.as_slice() == caller.account.as_slice());
        let (name_hash, served) = match row {
            Some(row) => {
                let served = row.is_webdav_served();
                (row.name_hash.unwrap_or_default(), served)
            }
            None => (Vec::new(), false),
        };
        let grant_live = !name_hash.is_empty()
            && any_grant_admits_read(grants.iter().map(Vec::as_slice), &name_hash, now);
        folders.push(WebdavAdmittedFolder {
            folder_id,
            name_hash,
            grant_live,
            served,
            extra: Default::default(),
        });
    }
    Ok(WebdavAdmitPrincipalReply {
        admitted: Some(WebdavAdmittedPrincipal {
            actor_id: caller.account.to_vec(),
            holder_x25519: caller.holder_x25519.map(|k| k.to_vec()).unwrap_or_default(),
            scopes: caller.scopes,
            exp: admission.exp,
            folders,
            extra: Default::default(),
        }),
        status: 0,
        www_authenticate: None,
        dpop_nonce: None,
        extra: Default::default(),
    })
}

fn handler() -> RpcHandler {
    Box::new(|state, bridge_actor, payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission(
                &state.db,
                &bridge_actor,
                KIND,
                internal,
            )
            .await?;
            let req: WebdavAdmitPrincipalRequest = decode(&payload).map_err(malformed)?;
            encode_reply(&admit(&state, req).await?)
        })
    })
}

pub fn register_webdav_principal_admission_handler(b: &mut RpcRouterBuilder) {
    b.add(
        KIND,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: handler(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_webdav_url_is_relayed() {
        for good in [
            "https://mail.example.com/webdav/alice/Photos/",
            "https://mail.example.com:443/webdav/x",
        ] {
            assert!(is_webdav_htu(good), "{good}");
        }
        for bad in [
            "http://mail.example.com/webdav/alice/",
            "https://example.com/api/v1/folders/4/deposit",
            "https://mail.example.com/caldav/alice/",
            "https://mail.example.com",
            "https://mail.example.com/webdavx/a",
            "",
        ] {
            assert!(!is_webdav_htu(bad), "{bad}");
        }
    }

    /// The proof's host is the nest's own DAV host: `mail.<primary>` on a
    /// routed or undeclared box (port 443), the request's host at the admin
    /// DAV port on a direct listener.
    #[test]
    fn the_htu_must_name_this_nests_dav_host() {
        for (authority, origin) in [
            ("mail.fauna.example", "mail.fauna.example"),
            ("MAIL.Fauna.Example:443", "mail.fauna.example"),
            ("192.168.1.9:9443", "192.168.1.9:9443"),
        ] {
            assert!(
                binds_dav_origin(authority, origin),
                "{authority} vs {origin}"
            );
        }
        for (authority, origin) in [
            ("evil.example", "mail.fauna.example"),
            ("mail.fauna.example:8443", "mail.fauna.example"),
            ("mail.fauna.example.evil.example", "mail.fauna.example"),
            ("user@mail.fauna.example", "mail.fauna.example"),
            ("192.168.1.9:443", "192.168.1.9:9443"),
            ("", "mail.fauna.example"),
        ] {
            assert!(
                !binds_dav_origin(authority, origin),
                "{authority} vs {origin}"
            );
        }
    }

    const DOMAIN: &str = "fauna.example";

    /// A nest that can run the admission: an issuer (the apex), its signing
    /// key, and `DOMAIN` as the primary mail domain the DAV host derives from.
    async fn admitting_state() -> Arc<AppState> {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let seed = [0x5Au8; 32];
        crate::test_support::seat_deployment_seed(&db, &seed).await;
        crate::test_support::boot_mint(&db).await;
        let state = crate::test_support::serving_generation(db, &seed);
        state
            .identity_domain
            .store(Some(Arc::new(DOMAIN.to_string())));
        state
            .db
            .add_mail_domain(DOMAIN, true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        Arc::new(state)
    }

    /// A token no issuer minted, claiming `client_id` in its payload.
    fn forged_token(client_id: &str) -> String {
        use base64::Engine as _;
        let b64 = |v: serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
        };
        format!(
            "{}.{}.c2ln",
            b64(serde_json::json!({"alg": "ES256"})),
            b64(serde_json::json!({ "client_id": client_id }))
        )
    }

    fn request(htu: &str, token: String, source: Option<&str>) -> WebdavAdmitPrincipalRequest {
        WebdavAdmitPrincipalRequest {
            token,
            dpop_proofs: vec!["not-a-proof".into()],
            htm: "PROPFIND".into(),
            htu: htu.into(),
            source_ip: source.map(str::to_string),
            ..Default::default()
        }
    }

    /// A proof naming another host is refused before the admission
    /// runs; the nest's own DAV host reaches it.
    #[tokio::test]
    async fn a_proof_for_another_host_is_refused_before_the_admission() {
        let state = admitting_state().await;
        let foreign = admit(
            &state,
            request("https://evil.example/webdav/u/", forged_token(CLIENT), None),
        )
        .await;
        assert!(
            foreign.is_err(),
            "a foreign htu host is refused: {foreign:?}"
        );
        let own = admit(
            &state,
            request(
                &format!("https://mail.{DOMAIN}/webdav/u/"),
                forged_token(CLIENT),
                None,
            ),
        )
        .await
        .expect("the nest's own DAV host reaches the admission");
        assert_eq!(own.status, 401, "the forged token itself is then refused");
    }

    /// Forged tokens claiming an app's `client_id` from one source
    /// spend that source's bucket only: an honest refusal from another source
    /// stays a `401`, never the throttle's `429`.
    #[tokio::test]
    async fn forged_refusals_from_one_source_never_throttle_another() {
        let state = admitting_state().await;
        let htu = format!("https://mail.{DOMAIN}/webdav/u/");
        let mut statuses = Vec::new();
        for _ in 0..=crate::failed_credential_throttle::FAILED_CREDENTIAL_MAX {
            let reply = admit(
                &state,
                request(&htu, forged_token(CLIENT), Some("198.51.100.7")),
            )
            .await
            .unwrap();
            statuses.push(reply.status);
        }
        assert_eq!(
            statuses.last(),
            Some(&429),
            "the flooding source is throttled: {statuses:?}"
        );
        let honest = admit(
            &state,
            request(&htu, forged_token(CLIENT), Some("203.0.113.20")),
        )
        .await
        .unwrap();
        assert_eq!(
            honest.status, 401,
            "another source's refusal is its own, not the flood's"
        );
    }

    fn grant_over(tuple: fauna_mls::wrapped_blob::ScopeTuple, not_after: i64) -> Vec<u8> {
        let blob = fauna_mls::wrapped_blob::build_grant_blob_with_epochs(
            &[1u8; 32],
            &[2u8; 16],
            &x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([3u8; 32])).to_bytes(),
            None,
            fauna_mls::wrapped_blob::GrantWindow(0, not_after as u64),
            &[(tuple, vec![(Some(1), vec![7u8; 32])])],
        )
        .expect("grant builds");
        blob.to_canonical_bytes().expect("encodes")
    }

    const ACCOUNT: [u8; 32] = [0xA1; 32];
    const CLIENT: &str = "https://app.example/client.json";
    const HOLDER: [u8; 32] = [0x77; 32];
    const NEVER: i64 = i64::MAX;

    struct Fixture {
        state: Arc<AppState>,
        admission: crate::principal_session::Admission,
        folder: i64,
        name_hash: Vec<u8>,
    }

    /// An account with a served folder and a device principal consented
    /// `fauna:folder:read:<that folder>`, holding the owner's read grant over
    /// it when `granted`.
    async fn fixture(granted: bool) -> Fixture {
        use crate::db::third_party_principals::{
            AttestedKeys, ExecutionForm, PrincipalAttestation,
        };
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        db.create_user(&ACCOUNT, "free", "test").await.unwrap();
        let folder = db.create_folder("Photos", &ACCOUNT).await.unwrap();
        db.update_folder_by_id(
            folder,
            crate::db::FolderUpdate {
                webdav_enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let scope = format!("fauna:folder:read:{folder}");
        db.record_atproto_oauth_grant(
            &ACCOUNT,
            b"family-1",
            CLIENT,
            Some("Example App"),
            &scope,
            &[],
            "jkt",
            NEVER,
            None,
            crate::db::atproto_pds::OAUTH_GRANT_ISSUER_NEST,
            &PrincipalAttestation {
                keys: AttestedKeys {
                    holder_x25519: Some(HOLDER),
                    writer_ed25519: None,
                },
                execution_form: ExecutionForm::Device,
                manifest: None,
            },
        )
        .await
        .unwrap();
        let principal_id = db.list_third_party_principals(&ACCOUNT).await.unwrap()[0]
            .principal_id
            .clone();
        let name_hash = db
            .get_folder_by_id(folder)
            .await
            .unwrap()
            .unwrap()
            .name_hash
            .unwrap();
        let state = Arc::new(AppState::for_test(db));
        if granted {
            let blob = grant_over(
                fauna_mls::wrapped_blob::ScopeTuple::folder_read(&name_hash),
                NEVER,
            );
            state
                .db
                .put_capability_grant(&ACCOUNT, &[1; 16], &HOLDER, NEVER, &blob)
                .await
                .unwrap();
        }
        Fixture {
            state,
            admission: crate::principal_session::Admission {
                binding: crate::principal_handlers::PrincipalBinding {
                    account: ACCOUNT,
                    principal_id,
                    token_scopes: vec![scope, "fauna:folder:read:999".into()],
                    family_id: None,
                },
                exp: 4_000_000_000,
            },
            folder,
            name_hash,
        }
    }

    fn admitted(reply: WebdavAdmitPrincipalReply) -> WebdavAdmittedPrincipal {
        assert_eq!(reply.status, 0);
        reply.admitted.expect("admitted")
    }

    /// The journey: the account, the holder key, `exp`, and the one folder
    /// the granted scopes name — live and served. A scope only the token
    /// names (never granted at consent) answers nothing.
    #[tokio::test]
    async fn an_admitted_principal_learns_its_live_served_folder() {
        let f = fixture(true).await;
        let a = admitted(admitted_reply(&f.state, &f.admission).await.unwrap());
        assert_eq!(a.actor_id, ACCOUNT.to_vec());
        assert_eq!(a.holder_x25519, HOLDER.to_vec());
        assert_eq!(a.exp, 4_000_000_000);
        assert_eq!(a.folders.len(), 1);
        let folder = &a.folders[0];
        assert_eq!(folder.folder_id, f.folder);
        assert_eq!(folder.name_hash, f.name_hash);
        assert!(folder.grant_live && folder.served);
    }

    /// No grant, or a revoked one, is `grant_live: false` — the door then
    /// serves nothing of the folder.
    #[tokio::test]
    async fn a_missing_or_revoked_grant_is_not_live() {
        let f = fixture(false).await;
        let a = admitted(admitted_reply(&f.state, &f.admission).await.unwrap());
        assert!(!a.folders[0].grant_live);

        let f = fixture(true).await;
        assert!(
            f.state
                .db
                .delete_capability_grant(&ACCOUNT, &[1; 16])
                .await
                .unwrap()
        );
        let a = admitted(admitted_reply(&f.state, &f.admission).await.unwrap());
        assert!(!a.folders[0].grant_live);
    }

    /// The served gate is answered beside the grant — the door's backstop.
    #[tokio::test]
    async fn an_unserved_folder_answers_served_false() {
        let f = fixture(true).await;
        f.state
            .db
            .update_folder_by_id(
                f.folder,
                crate::db::FolderUpdate {
                    webdav_enabled: Some(false),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let a = admitted(admitted_reply(&f.state, &f.admission).await.unwrap());
        assert!(a.folders[0].grant_live && !a.folders[0].served);
    }

    /// A revoked principal (its row gone) is refused with the opaque
    /// `invalid_token` the admission gives every credential failure.
    #[tokio::test]
    async fn a_revoked_principal_is_refused() {
        let f = fixture(true).await;
        let mut admission = f.admission;
        admission.binding.principal_id = vec![0xEE; 16];
        let reply = admitted_reply(&f.state, &admission).await.unwrap();
        assert!(reply.admitted.is_none());
        assert_eq!(reply.status, 401);
        assert!(
            reply
                .www_authenticate
                .is_some_and(|h| h.contains("invalid_token"))
        );
    }

    /// The challenge a refused admission rendered is relayed verbatim.
    #[test]
    fn a_refusal_relays_the_rendered_challenge() {
        let rendered = crate::oauth_as_oidc::challenge(
            axum::http::StatusCode::UNAUTHORIZED,
            "use_dpop_nonce",
            "fresh-nonce",
        );
        let reply = refusal(&rendered);
        assert!(reply.admitted.is_none());
        assert_eq!(reply.status, 401);
        assert_eq!(reply.dpop_nonce.as_deref(), Some("fresh-nonce"));
        assert!(
            reply
                .www_authenticate
                .is_some_and(|h| h.contains("use_dpop_nonce"))
        );
    }

    #[test]
    fn only_a_live_read_tuple_over_the_set_admits() {
        use fauna_mls::wrapped_blob::ScopeTuple;
        let hash = [9u8; 32];
        let live = grant_over(ScopeTuple::folder_read(&hash), 2_000);
        assert!(any_grant_admits_read([live.as_slice()], &hash, 1_000));
        // Another set, a lapsed window, a deposit tuple — none admits.
        assert!(!any_grant_admits_read([live.as_slice()], &[8u8; 32], 1_000));
        assert!(!any_grant_admits_read([live.as_slice()], &hash, 3_000));
        let deposit = grant_over(ScopeTuple::folder_deposit(4, &[5; 32]), 2_000);
        assert!(!any_grant_admits_read([deposit.as_slice()], &hash, 1_000));
        assert!(!any_grant_admits_read([b"junk".as_slice()], &hash, 1_000));
    }
}
