//! The **principal session** — the token-bearing WS-RPC connection a
//! third-party principal opens on `GET /api/v1/principal/ws`
//! (`docs/goal/architecture/transport-connection.md` § Connection lifecycle →
//! *The principal session*).
//!
//! A principal holds no actor key and no nest bearer: it presents a DPoP-bound
//! access token from this nest's own issuer, carried in
//! `Sec-WebSocket-Protocol: fauna.v1, dpop.<access token>, dpop-proof.<proof>`
//! (a browser's WebSocket API can set nothing else). This module owns the
//! upgrade end to end:
//!
//! - **the gate** ([`admit`]) — the issuer's resource-server gate applied to
//!   the upgrade request, once, before the 101, in `/oauth/userinfo`'s order:
//!   the token, then the proof, then at least one built Fauna-family scope,
//!   then the principal row and the account's own standing;
//! - **the registration** ([`register_principal_connection`]) — the principal
//!   registry, never the account's subscription entry, with the one re-read
//!   after joining that makes a racing revoke total;
//! - **the deadline** ([`spawn_exp_deadline`]) — the socket closes `4401` at
//!   the token's `exp`.
//!
//! Dispatch on the connection is `principal_handlers`'; the revocation doors
//! are `WsState::disconnect_principal` (door (a), `fauna.principals.revoke`)
//! and the per-actor teardown's principal sweep (door (b)).

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use fauna_bridge_atproto::oauth_metadata::principal_ws_htu;

use crate::oauth_as_error::ERR_USE_DPOP_NONCE;
use crate::oauth_as_state::OAuthAsRuntime;
use crate::oauth_as_token::{AccessClaims, b64, split_scope, verify_access_token};
use crate::oauth_issuer_key::IssuerPublicKey;
use crate::principal_handlers::PrincipalBinding;
use crate::routes::AppState;
use crate::ws::RpcConnection;

/// How many sessions one principal may hold open at once. A correct client
/// needs one, plus a second for the overlap the token refresh allows (it may
/// open the new session before the old one's `exp` closes it); four leaves
/// room for a client running a few tabs or workers without letting one
/// principal hold an unbounded share of the nest's sockets. One over is
/// refused `429`. A Rust constant: no user or admin would choose it
/// (`principles.md` § One configuration surface).
///
/// Checked at the upgrade, before the 101, against the registry's count — so
/// concurrent upgrades of one principal racing that read can overshoot it by
/// their own number, which a principal cannot multiply beyond the throttles in
/// front of the gate.
pub const MAX_PRINCIPAL_SESSIONS: usize = 4;

const ERR_INVALID_TOKEN: &str = "invalid_token";
const ERR_INSUFFICIENT_SCOPE: &str = "insufficient_scope";

/// What an admitted upgrade carries into its connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admission {
    pub binding: PrincipalBinding,
    /// The access token's `exp`, epoch seconds — the session's deadline.
    pub exp: i64,
}

/// Why the gate refused, before it is rendered — the three shapes
/// `/oauth/userinfo` answers with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// `401 invalid_token` — every token, proof, principal and account failure
    /// alike, so the answer is never an oracle about which.
    InvalidToken,
    /// `401 use_dpop_nonce` — only ever after a verified token.
    UseDpopNonce,
    /// `403 insufficient_scope` — a sound token naming no built Fauna scope.
    InsufficientScope,
}

/// Read the principal carriage out of `Sec-WebSocket-Protocol`: `fauna.v1`
/// offered, exactly one `dpop.<token>`, and every `dpop-proof.<jwt>` element
/// (the proof gate refuses anything but exactly one, by its own rule). Both
/// wire forms the bearer carriage accepts — one comma-joined value, or
/// separate header values — are read alike.
pub fn parse_principal_subprotocol(headers: &HeaderMap) -> Option<(String, Vec<String>)> {
    let elements: Vec<&str> = headers
        .get_all(axum::http::header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .collect();
    if !elements.contains(&"fauna.v1") {
        return None;
    }
    let mut tokens = elements.iter().filter_map(|e| e.strip_prefix("dpop."));
    let token = tokens.next()?;
    if token.is_empty() || tokens.next().is_some() {
        return None;
    }
    let proofs = elements
        .iter()
        .filter_map(|e| e.strip_prefix("dpop-proof."))
        .map(str::to_string)
        .collect();
    Some((token.to_string(), proofs))
}

/// A principal's credential as one request presented it: the DPoP-bound
/// access token and every DPoP proof beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presentation {
    pub token: String,
    pub proofs: Vec<String>,
}

/// Read a principal's credential off a plain HTTP request — RFC 9449's
/// `Authorization: DPoP <token>` (exactly one) and its `DPoP` proof header(s)
/// — for the nest's resource-server doors a remote principal reaches without
/// a session (`api-layers.md` § HTTP residue: the deposit door).
pub fn parse_http_presentation(headers: &HeaderMap) -> Option<Presentation> {
    let mut values = headers.get_all(axum::http::header::AUTHORIZATION).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    if !scheme.eq_ignore_ascii_case("DPoP") || token.is_empty() {
        return None;
    }
    Some(Presentation {
        token: token.to_string(),
        proofs: crate::oauth_as_routes::dpop_proofs(headers),
    })
}

/// Render a principal call's refusal as HTTP for the resource-server doors (the
/// deposit door, the record door, the events door): the RPC error's code and
/// message as JSON, under the status its family means.
pub fn rpc_error_response(err: &fauna_protocol::RpcError) -> Response {
    let code = err.code.as_str();
    let status = if code.ends_with("permission_denied") {
        StatusCode::FORBIDDEN
    } else if code.ends_with("invalid_params")
        || code.ends_with("invalid_request")
        || code.ends_with("malformed")
    {
        StatusCode::BAD_REQUEST
    } else if code.ends_with("stale_writer_seq") || code.ends_with("cas_mismatch") {
        StatusCode::CONFLICT
    } else if code.ends_with("scope_full") {
        StatusCode::INSUFFICIENT_STORAGE
    } else if code.ends_with("unavailable") {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    let detail = match err.details.as_deref() {
        Some(fauna_protocol::Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    (
        status,
        axum::Json(serde_json::json!({ "error": err.code, "detail": detail })),
    )
        .into_response()
}

/// The presentation half of the gate, steps (1)–(3): the token, then the
/// proof strictly after it, then the scope. Pure over its inputs, so the order
/// is pinned without a running nest. `htm`/`htu` are the door's own: `GET`
/// and the upgrade URL for the session, `POST` and the folder's URL for the
/// deposit door.
#[allow(clippy::too_many_arguments)] // the gate's inputs, flat and pure
pub fn verify_presentation(
    runtime: &OAuthAsRuntime,
    keys: &[IssuerPublicKey],
    issuer: &str,
    htm: &str,
    htu: &str,
    token: &str,
    proofs: &[String],
    now: i64,
) -> Result<AccessClaims, Refusal> {
    // (1) Signed by a served key, `typ: at+jwt`, `iss` this issuer, this
    // issuer among the audiences, unexpired.
    let claims =
        verify_access_token(keys, token, issuer, &[issuer], now).ok_or(Refusal::InvalidToken)?;
    // (2) The proof, bound to THIS token by `ath` and to the token's key by
    // `cnf.jkt`.
    let ath = b64(&<sha2::Sha256 as sha2::Digest>::digest(token.as_bytes()));
    let jkt = match crate::oauth_as_gates::resource_dpop_gate(runtime, proofs, htm, htu, now, ath) {
        Ok(jkt) => jkt,
        Err(deny) if deny.error == ERR_USE_DPOP_NONCE => return Err(Refusal::UseDpopNonce),
        Err(_) => return Err(Refusal::InvalidToken),
    };
    if jkt != claims.cnf.jkt {
        return Err(Refusal::InvalidToken);
    }
    // (3) At least one BUILT Fauna-family scope: a token consented only for
    // the PDS, or only for sign-in, opens no session.
    if !split_scope(&claims.scope)
        .iter()
        .any(|s| fauna_bridge_atproto::fauna_scope::arm_of(s).is_some())
    {
        return Err(Refusal::InsufficientScope);
    }
    Ok(claims)
}

/// The `client_id` a token claims, read WITHOUT verifying it — the
/// failed-credential throttle's claimed identity, which is unproven by
/// definition. Empty for anything that does not decode.
fn unverified_client_id(token: &str) -> String {
    use base64::Engine as _;
    token
        .split('.')
        .nth(1)
        .and_then(|c| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(c)
                .ok()
        })
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|v| v.get("client_id")?.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The whole gate, steps (1)–(4), for the session's upgrade. `Err` is the
/// rendered refusal, its fresh `DPoP-Nonce` included (or the throttle's
/// `429`, or the domainless `503`).
pub async fn admit(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    source: Option<std::net::IpAddr>,
) -> Result<Admission, Response> {
    let parsed =
        parse_principal_subprotocol(headers).map(|(token, proofs)| Presentation { token, proofs });
    let htu = principal_ws_htu(&state.web_serving_domain());
    admit_presentation(state, parsed.as_ref(), "GET", &htu, source).await
}

/// The whole gate, steps (1)–(4), over a credential any door read — the
/// session's upgrade ([`admit`]) and the HTTP doors
/// ([`parse_http_presentation`]) alike, so a principal is admitted by one
/// rule wherever it knocks. `None` is a request that carried no well-formed
/// credential. The failed-credential throttle counts every door under the
/// principal surface: one principal, one budget.
pub async fn admit_presentation(
    state: &Arc<AppState>,
    parsed: Option<&Presentation>,
    htm: &str,
    htu: &str,
    source: Option<std::net::IpAddr>,
) -> Result<Admission, Response> {
    let Some(issuer) = crate::oauth_issuer_routes::issuer(state) else {
        return Err(crate::oauth_issuer_routes::no_issuer_yet());
    };
    let now = fauna_core::data::Timestamp::now_secs_or_zero();
    let runtime = &state.oauth_as;
    // Every answer from here on carries a fresh nonce — minted by the issuer's
    // one minter, so a client that just redeemed or refreshed already holds a
    // usable one.
    let nonce = runtime.nonces.mint(now);
    let refuse = |refusal: Refusal| -> Response {
        // Only a credential refusal spends the throttle; asking for a nonce is
        // the protocol, not a failure.
        if refusal != Refusal::UseDpopNonce {
            let client_id = parsed
                .map(|p| unverified_client_id(&p.token))
                .unwrap_or_default();
            if state.failed_credential_throttle.note_refusal(
                crate::failed_credential_throttle::Surface::PrincipalUpgrade,
                source,
                &crate::failed_credential_throttle::principal_claimed_identity(&client_id),
            ) {
                return crate::failed_credential_throttle::too_many_requests();
            }
        }
        let (status, error) = match refusal {
            Refusal::InvalidToken => (StatusCode::UNAUTHORIZED, ERR_INVALID_TOKEN),
            Refusal::UseDpopNonce => (StatusCode::UNAUTHORIZED, ERR_USE_DPOP_NONCE),
            Refusal::InsufficientScope => (StatusCode::FORBIDDEN, ERR_INSUFFICIENT_SCOPE),
        };
        crate::oauth_as_oidc::challenge(status, error, &nonce)
    };

    let Some(Presentation { token, proofs }) = parsed else {
        return Err(refuse(Refusal::InvalidToken));
    };
    let keys = match crate::oauth_as_routes::verifying_material(state, now).await {
        Ok((keys, _)) => keys,
        Err(deny) => {
            return Err(crate::oauth_as_routes::with_nonce(
                crate::oauth_as_error::oauth_error_response(&deny),
                &nonce,
            ));
        }
    };
    let claims = verify_presentation(runtime, &keys, &issuer, htm, htu, token, proofs, now)
        .map_err(&refuse)?;

    // (4) A principal row for `(fauna_actor, client_id)`, and an account whose
    // own authority is live: not deleted, suspended, locked out or superseded.
    let Some(account) = claims
        .actor_bytes()
        .and_then(|a| <[u8; 32]>::try_from(a).ok())
    else {
        return Err(refuse(Refusal::InvalidToken));
    };
    let standing = account_standing(state, &account, &claims.client_id).await;
    let principal_id = match standing {
        Ok(Some(principal_id)) => principal_id,
        Ok(None) => return Err(refuse(Refusal::InvalidToken)),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "principal upgrade: standing read failed");
            return Err(crate::oauth_as_routes::with_nonce(
                crate::oauth_as_error::oauth_error_response(
                    &crate::oauth_as_error::OAuthDeny::server("could not read this principal"),
                ),
                &nonce,
            ));
        }
    };
    Ok(Admission {
        binding: PrincipalBinding {
            account,
            principal_id,
            token_scopes: split_scope(&claims.scope),
        },
        exp: claims.exp,
    })
}

/// Step (4)'s reads: the principal's id when the row exists and the account
/// stands — `None` for a missing row, a missing, suspended or locked-out
/// account, a superseded identity, and an account whose external-apps switch
/// is OFF alike.
async fn account_standing(
    state: &AppState,
    account: &[u8; 32],
    client_id: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    let Some(principal_id) = state
        .db
        .get_third_party_principal_id(account, client_id)
        .await?
    else {
        return Ok(None);
    };
    match state.db.actor_authority(&account[..]).await? {
        Some(authority) if !authority.is_revoked_at(crate::db::now_epoch_secs()) => {}
        _ => return Ok(None),
    }
    // Supersession is a decision the nest stores, not a flag on the account
    // row (the ceremony keeps the retired `users` row unsuspended), so it is
    // its own read — once, here, as at every mint door. A session already open
    // when the ceremony runs is closed by its actor-wide teardown, which
    // sweeps the account's principals (door (b)).
    if state.db.succession_for(&account[..]).await?.is_some() {
        return Ok(None);
    }
    // The account's external-apps switch: OFF suspends every external app,
    // this principal included, with the same `invalid_token` a revoked row
    // gets (`atproto-pds-full.md` § F1 detail, the kill-switch bullet).
    if !state.db.get_atproto_external_apps_enabled(account).await? {
        return Ok(None);
    }
    Ok(Some(principal_id))
}

/// `GET /api/v1/principal/ws` — the principal session's upgrade.
pub async fn principal_ws_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    crate::registration::OptionalConnectInfo(peer_addr): crate::registration::OptionalConnectInfo,
    ws: WebSocketUpgrade,
) -> Response {
    let admission = match admit(&state, &headers, peer_addr.map(|a| a.ip())).await {
        Ok(admission) => admission,
        Err(response) => return response,
    };
    let binding = &admission.binding;
    if state
        .ws
        .principal_sessions(&binding.account, &binding.principal_id)
        >= MAX_PRINCIPAL_SESSIONS
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "too many concurrent sessions for this principal",
        )
            .into_response();
    }
    ws.max_message_size(crate::routes::MAX_WS_MESSAGE_SIZE)
        .max_frame_size(crate::routes::MAX_WS_MESSAGE_SIZE)
        .protocols(["fauna.v1"])
        .on_upgrade(move |socket| handle_principal_ws(state, admission, socket))
}

async fn handle_principal_ws(
    state: Arc<AppState>,
    admission: Admission,
    socket: axum::extract::ws::WebSocket,
) {
    let Admission { binding, exp } = admission;
    let (account, principal_id) = (binding.account, binding.principal_id.clone());
    let (conn, rx) = register_principal_connection(&state, binding).await;
    let now = fauna_core::data::Timestamp::now_secs_or_zero();
    let deadline = spawn_exp_deadline(Arc::clone(&conn), until_exp(exp, now));
    crate::routes::run_connection(Arc::clone(&state), Arc::clone(&conn), rx, socket).await;
    deadline.abort();
    state
        .ws
        .remove_principal(&account, &principal_id, conn.conn_id);
}

/// Register a principal session and close the upgrade window.
///
/// **The window.** The gate read the row before the 101; the connection joins
/// the registry only afterwards. A `fauna.principals.revoke` (or an account
/// teardown) landing in between sweeps a registry this connection has not
/// joined yet. Every revocation door deletes or strips first and sweeps
/// second, so one read AFTER joining is total: either the row is already gone
/// (or the account already stands no more), and this closes the connection
/// itself; or it is still there, the sweep has not run either, and when it
/// runs it finds this connection. ⚠ The read must stay after the subscribe —
/// the ordering argument is `AppState::register_upgraded_connection`'s.
///
/// A database fault here closes the connection: it is the read that decides
/// whether the session may exist at all, and every call the session could make
/// re-reads the same rows anyway.
pub async fn register_principal_connection(
    state: &AppState,
    binding: PrincipalBinding,
) -> (
    Arc<RpcConnection>,
    tokio::sync::mpsc::Receiver<bytes::Bytes>,
) {
    let (conn, rx) = state.ws.subscribe_principal(binding);
    let binding = conn
        .principal
        .as_ref()
        .expect("subscribe_principal binds the connection");
    match crate::principal_handlers::resolve_principal(state, binding).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            tracing::info!(
                target: "auth",
                "a principal upgrade raced its own revocation; the connection was closed"
            );
            conn.revoke();
        }
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "principal registration re-read failed");
            conn.revoke();
        }
    }
    (conn, rx)
}

/// How long until `exp` (epoch seconds) from `now` — zero once it has passed.
pub fn until_exp(exp: i64, now: i64) -> Duration {
    Duration::from_secs(u64::try_from(exp.saturating_sub(now)).unwrap_or(0))
}

/// Close `conn` **4401** once `remaining` elapses — the token's `exp`.
///
/// Closed, not merely refused at the next call: a Push is not an RPC, so an
/// idle socket would otherwise outlive its credential. `4401` keeps its one
/// meaning — the credential this connection was opened with is finished. The
/// caller aborts the task when the connection ends first.
pub fn spawn_exp_deadline(
    conn: Arc<RpcConnection>,
    remaining: Duration,
) -> tokio::task::JoinHandle<()> {
    // spawn-ok(connection-scoped): one sleep per principal session, aborted by
    // `handle_principal_ws` the moment the connection ends, and never longer
    // than an access token's lifetime.
    tokio::spawn(async move {
        tokio::time::sleep(remaining).await;
        conn.revoke();
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protocol_headers(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append(
                axum::http::header::SEC_WEBSOCKET_PROTOCOL,
                axum::http::HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn the_carriage_is_read_from_either_wire_form() {
        let joined = protocol_headers(&["fauna.v1, dpop.tok, dpop-proof.prf"]);
        assert_eq!(
            parse_principal_subprotocol(&joined),
            Some(("tok".into(), vec!["prf".into()]))
        );
        let split = protocol_headers(&["fauna.v1", "dpop.tok", "dpop-proof.prf"]);
        assert_eq!(
            parse_principal_subprotocol(&split),
            Some(("tok".into(), vec!["prf".into()]))
        );
    }

    #[test]
    fn a_carriage_without_the_version_or_with_two_tokens_is_unreadable() {
        assert!(
            parse_principal_subprotocol(&protocol_headers(&["dpop.t, dpop-proof.p"])).is_none()
        );
        assert!(
            parse_principal_subprotocol(&protocol_headers(&["fauna.v1, dpop.a, dpop.b"])).is_none()
        );
        assert!(parse_principal_subprotocol(&protocol_headers(&["fauna.v1, dpop."])).is_none());
        assert!(parse_principal_subprotocol(&protocol_headers(&["fauna.v1, bearer.x"])).is_none());
        // No proof is readable; the gate refuses it by its own rule.
        assert_eq!(
            parse_principal_subprotocol(&protocol_headers(&["fauna.v1, dpop.t"])),
            Some(("t".into(), vec![]))
        );
    }

    #[test]
    fn the_unverified_client_id_is_read_without_trusting_it() {
        use base64::Engine as _;
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"client_id":"https://app.example/c.json"}"#);
        assert_eq!(
            unverified_client_id(&format!("h.{claims}.s")),
            "https://app.example/c.json"
        );
        assert_eq!(unverified_client_id("garbage"), "");
    }

    // ── The presentation gate, over a real minted token and a real proof ──

    use crate::oauth_as_token::{OAuthGrant, mint_tokens};
    use crate::oauth_issuer_key::IssuerSigner;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
    use p256::ecdsa::signature::Signer as _;

    const ISS: &str = "https://nest.example";
    const HTU: &str = "https://nest.example/api/v1/principal/ws";
    const PDS: &str = "did:web:pds.nest.example";
    const NOW: i64 = 1_800_000_000;

    struct Rig {
        runtime: OAuthAsRuntime,
        signer: IssuerSigner,
        keys: Vec<IssuerPublicKey>,
        dpop: p256::ecdsa::SigningKey,
        x: String,
        y: String,
    }

    fn rig() -> Rig {
        struct NoFetch;
        #[async_trait::async_trait]
        impl crate::oauth_as_client::ClientMetadataFetcher for NoFetch {
            async fn fetch(
                &self,
                _url: &str,
            ) -> Result<String, crate::oauth_as_client::MetadataFetchError> {
                panic!("the gate never fetches")
            }
        }
        let minted = fauna_provisioning::oauth_issuer::mint_issuer_key().expect("mint");
        let dpop = p256::ecdsa::SigningKey::random(&mut rand::thread_rng());
        let (x, y) =
            fauna_provisioning::oauth_issuer::public_coordinates(dpop.verifying_key()).unwrap();
        Rig {
            runtime: OAuthAsRuntime::new(NOW, Arc::new(NoFetch)),
            signer: IssuerSigner {
                kid: minted.kid.clone(),
                secret_scalar: minted.secret_scalar,
            },
            keys: vec![IssuerPublicKey {
                kid: minted.kid,
                x: minted.x,
                y: minted.y,
                retired_at: None,
            }],
            dpop,
            x,
            y,
        }
    }

    impl Rig {
        fn jkt(&self) -> String {
            fauna_provisioning::oauth_issuer::rfc7638_p256_thumbprint(self.dpop.verifying_key())
        }

        fn token(&self, scopes: &[&str], jkt: &str) -> String {
            let grant = OAuthGrant {
                client_id: "https://app.example/client.json".to_string(),
                subject: "did:plc:abcdefghijklmnopqrstuvwx".to_string(),
                actor_id: vec![7u8; 32],
                scopes: scopes.iter().map(|s| s.to_string()).collect(),
                dpop_jkt: jkt.to_string(),
                session_deadline: 0,
            };
            mint_tokens(
                &self.signer,
                &[9u8; 32],
                &grant,
                ISS,
                PDS,
                b"sid",
                b"sid",
                NOW,
            )
            .expect("mint")
            .0
        }

        fn proof(&self, token: &str, htu: &str, nonce: &str, jti: &str) -> String {
            let header = serde_json::json!({
                "typ": "dpop+jwt", "alg": "ES256",
                "jwk": { "kty": "EC", "crv": "P-256", "x": self.x, "y": self.y },
            });
            let ath = b64(&<sha2::Sha256 as sha2::Digest>::digest(token.as_bytes()));
            let claims = serde_json::json!({
                "jti": jti, "htm": "GET", "htu": htu, "iat": NOW, "nonce": nonce, "ath": ath,
            });
            let input = format!(
                "{}.{}",
                B64.encode(serde_json::to_vec(&header).unwrap()),
                B64.encode(serde_json::to_vec(&claims).unwrap())
            );
            let sig: p256::ecdsa::Signature = self.dpop.sign(input.as_bytes());
            format!("{input}.{}", B64.encode(sig.to_bytes()))
        }

        fn check(&self, token: &str, proofs: &[String]) -> Result<AccessClaims, Refusal> {
            verify_presentation(
                &self.runtime,
                &self.keys,
                ISS,
                "GET",
                HTU,
                token,
                proofs,
                NOW + 1,
            )
        }

        /// The refusal alone (`AccessClaims` carries no `PartialEq`).
        fn refusal(&self, token: &str, proofs: &[String]) -> Option<Refusal> {
            self.check(token, proofs).err()
        }
    }

    /// A Fauna-scoped token under its own key, proved for this upgrade URL,
    /// is admitted — and the claims the binding is built from come back.
    #[test]
    fn a_fauna_scoped_token_with_its_proof_is_admitted() {
        let r = rig();
        let token = r.token(&["fauna:feed:read"], &r.jkt());
        let nonce = r.runtime.nonces.mint(NOW);
        let claims = r
            .check(&token, &[r.proof(&token, HTU, &nonce, "j1")])
            .unwrap();
        assert_eq!(claims.client_id, "https://app.example/client.json");
        assert_eq!(claims.scope, "fauna:feed:read");
    }

    /// Step (3): a sound token naming no BUILT Fauna scope opens no session —
    /// sign-in only, PDS only, and an unbuilt plane alike.
    #[test]
    fn a_token_without_a_built_fauna_scope_is_refused_insufficient_scope() {
        let r = rig();
        for scopes in [&["openid"][..], &["openid", "profile"]] {
            let token = r.token(scopes, &r.jkt());
            let nonce = r.runtime.nonces.mint(NOW);
            let proof = r.proof(&token, HTU, &nonce, &format!("s-{}", scopes.len()));
            assert_eq!(
                r.refusal(&token, &[proof]),
                Some(Refusal::InsufficientScope)
            );
        }
        // A PDS-only token does not name this issuer as a reader: refused at (1).
        let token = r.token(&["atproto"], &r.jkt());
        let nonce = r.runtime.nonces.mint(NOW);
        let proof = r.proof(&token, HTU, &nonce, "pds");
        assert_eq!(r.refusal(&token, &[proof]), Some(Refusal::InvalidToken));
    }

    /// Step (2): no proof, a proof for another URL, a proof under a key the
    /// token is not bound to, and a replayed proof are all `invalid_token`;
    /// a stale nonce asks for a fresh one.
    #[test]
    fn the_proof_must_bind_this_token_this_url_and_this_key() {
        let r = rig();
        let token = r.token(&["fauna:feed:read"], &r.jkt());
        assert_eq!(r.refusal(&token, &[]), Some(Refusal::InvalidToken));

        let nonce = r.runtime.nonces.mint(NOW);
        let elsewhere = r.proof(&token, "https://nest.example/oauth/userinfo", &nonce, "e");
        assert_eq!(r.refusal(&token, &[elsewhere]), Some(Refusal::InvalidToken));

        let other_key = r.token(&["fauna:feed:read"], "someone-elses-thumbprint");
        let proof = r.proof(&other_key, HTU, &nonce, "k");
        assert_eq!(r.refusal(&other_key, &[proof]), Some(Refusal::InvalidToken));

        let once = r.proof(&token, HTU, &nonce, "replayed");
        assert!(r.check(&token, std::slice::from_ref(&once)).is_ok());
        assert_eq!(r.refusal(&token, &[once]), Some(Refusal::InvalidToken));

        let stale = r.proof(&token, HTU, "not-a-nonce-we-minted", "n");
        assert_eq!(r.refusal(&token, &[stale]), Some(Refusal::UseDpopNonce));
    }

    /// The token is checked strictly before the proof: a forged token is
    /// `invalid_token` even when its proof's nonce is stale, so the retryable
    /// answer is never an oracle about a token.
    #[test]
    fn a_forged_token_never_learns_use_dpop_nonce() {
        let r = rig();
        let forged = "eyJhbGciOiJFUzI1NiJ9.e30.c2ln";
        let stale = r.proof(forged, HTU, "stale", "f");
        assert_eq!(r.refusal(forged, &[stale]), Some(Refusal::InvalidToken));
    }

    #[test]
    fn until_exp_never_goes_negative() {
        assert_eq!(until_exp(1_000, 400), Duration::from_secs(600));
        assert_eq!(until_exp(1_000, 1_000), Duration::ZERO);
        assert_eq!(until_exp(1_000, 2_000), Duration::ZERO);
    }

    // ── The registry and the three revocation doors ──

    use crate::db::CacheDb;
    use crate::db::third_party_principals::{AttestedKeys, ExecutionForm, PrincipalAttestation};

    const ACCOUNT: [u8; 32] = [0xA1; 32];
    const OTHER: [u8; 32] = [0xB2; 32];

    fn binding(account: [u8; 32], principal_id: &[u8]) -> PrincipalBinding {
        PrincipalBinding {
            account,
            principal_id: principal_id.to_vec(),
            token_scopes: vec!["fauna:feed:read".into()],
        }
    }

    /// A principal session never joins the account's subscription entry, so no
    /// Push, presence or roster walk reaches it — but a serving-generation
    /// drain counts it.
    #[test]
    fn a_principal_session_never_joins_the_accounts_subscriptions() {
        let ws = crate::ws::WsState::new();
        let (conn, _rx) = ws.subscribe_principal(binding(ACCOUNT, b"p1"));
        assert_eq!(
            conn.actor_id, [0u8; 32],
            "the actor slot is the placeholder"
        );
        assert!(!conn.anonymous);
        assert!(!ws.has_connections(&ACCOUNT));
        assert!(ws.connections_for(&ACCOUNT).is_empty());
        assert_eq!(ws.principal_sessions(&ACCOUNT, b"p1"), 1);
        assert_eq!(ws.connection_count(), 1);
        ws.remove_principal(&ACCOUNT, b"p1", conn.conn_id);
        assert_eq!(ws.principal_sessions(&ACCOUNT, b"p1"), 0);
        assert_eq!(ws.connection_count(), 0);
    }

    /// Door (a)'s sweep closes exactly the revoked principal's sessions — not
    /// the account's own sockets, and not its other principals'.
    #[test]
    fn revoking_one_principal_closes_only_its_sessions() {
        let ws = crate::ws::WsState::new();
        let (p1, _r1) = ws.subscribe_principal(binding(ACCOUNT, b"p1"));
        let (p1b, _r2) = ws.subscribe_principal(binding(ACCOUNT, b"p1"));
        let (p2, _r3) = ws.subscribe_principal(binding(ACCOUNT, b"p2"));
        let (own, _r4) = ws.subscribe(ACCOUNT);
        assert_eq!(ws.disconnect_principal(&ACCOUNT, b"p1"), 2);
        assert!(p1.is_revoked() && p1b.is_revoked());
        assert!(!p2.is_revoked(), "another principal of the account lives");
        assert!(!own.is_revoked(), "the account's own socket lives");
    }

    /// Door (b): every actor-wide teardown closes the account's principal
    /// sessions with its own sockets — and never another account's.
    #[test]
    fn an_actor_wide_teardown_closes_the_accounts_principal_sessions() {
        let ws = crate::ws::WsState::new();
        let (mine, _r1) = ws.subscribe_principal(binding(ACCOUNT, b"p1"));
        let (theirs, _r2) = ws.subscribe_principal(binding(OTHER, b"p1"));
        let (own, _r3) = ws.subscribe(ACCOUNT);
        assert_eq!(ws.disconnect_actor(&ACCOUNT), 2);
        assert!(own.is_revoked() && mine.is_revoked());
        assert!(!theirs.is_revoked());
    }

    /// An account with one principal consented `fauna:feed:read`; its binding.
    async fn consented() -> (Arc<AppState>, PrincipalBinding) {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        db.create_user(&ACCOUNT, "free", "test").await.unwrap();
        db.record_atproto_oauth_grant(
            &ACCOUNT,
            b"family-1",
            "https://app.example/client.json",
            Some("Example App"),
            "fauna:feed:read",
            &[],
            "jkt",
            i64::MAX,
            None,
            crate::db::atproto_pds::OAUTH_GRANT_ISSUER_NEST,
            &PrincipalAttestation {
                keys: AttestedKeys {
                    holder_x25519: None,
                    writer_ed25519: None,
                },
                execution_form: ExecutionForm::Device,
                manifest: None,
            },
        )
        .await
        .unwrap();
        let principal_id = db
            .get_third_party_principal_id(&ACCOUNT, "https://app.example/client.json")
            .await
            .unwrap()
            .expect("the consent minted the principal");
        (
            Arc::new(AppState::for_test(db)),
            binding(ACCOUNT, &principal_id),
        )
    }

    /// Door (a), through the real handler: `fauna.principals.revoke` closes the
    /// principal's live session after its transaction commits.
    #[tokio::test]
    async fn the_revoke_verb_closes_the_principals_live_session() {
        let (state, binding) = consented().await;
        let (conn, _rx) = register_principal_connection(&state, binding.clone()).await;
        assert!(!conn.is_revoked(), "a standing principal registers");

        let router = crate::build_rpc_router();
        let meta = router.kind_meta("fauna.principals.revoke").unwrap();
        let payload =
            fauna_protocol::encode_canonical(&fauna_protocol::principals::RevokePrincipalRequest {
                principal_id: binding.principal_id.clone(),
                ..Default::default()
            })
            .unwrap();
        (meta.handler)(Arc::clone(&state), ACCOUNT, payload)
            .await
            .expect("the revoke answers");
        assert!(conn.is_revoked(), "the live session closes 4401");
    }

    /// The upgrade window: a revoke that deleted the row and swept the registry
    /// BEFORE this connection joined it is caught by the re-read after joining.
    #[tokio::test]
    async fn a_principal_upgrade_racing_its_revoke_closes_at_registration() {
        let (state, binding) = consented().await;
        // The whole revoke runs inside the window: the gate already admitted,
        // the 101 is out, the connection has not registered.
        state
            .db
            .revoke_third_party_principal(&ACCOUNT, &binding.principal_id)
            .await
            .unwrap()
            .expect("the principal existed");
        assert_eq!(
            state
                .ws
                .disconnect_principal(&ACCOUNT, &binding.principal_id),
            0
        );
        let (conn, _rx) = register_principal_connection(&state, binding).await;
        assert!(conn.is_revoked(), "the re-read after joining closes it");
    }

    /// The same window for door (b): an account suspended while the upgrade
    /// was in flight.
    #[tokio::test]
    async fn a_principal_upgrade_racing_its_accounts_suspension_closes_at_registration() {
        let (state, binding) = consented().await;
        assert!(
            state
                .db
                .suspend_user_now(&ACCOUNT, "test", "test")
                .await
                .unwrap()
        );
        let (conn, _rx) = register_principal_connection(&state, binding).await;
        assert!(conn.is_revoked());
    }

    /// The external-apps switch, through the real handler: OFF closes the
    /// account's live principal sessions after its write (the kill-switch
    /// bullet's third leg, `atproto-pds-full.md` § F1 detail), an upgrade
    /// while OFF closes at registration as a revoked one does, and the gate's
    /// own standing read answers none.
    #[tokio::test]
    async fn turning_external_apps_off_closes_the_accounts_principal_sessions() {
        let (state, binding) = consented().await;
        let (conn, _rx) = register_principal_connection(&state, binding.clone()).await;
        assert!(!conn.is_revoked(), "a standing principal registers");

        let router = crate::build_rpc_router();
        let meta = router
            .kind_meta("fauna.bridges.atproto.set_external_apps_enabled")
            .unwrap();
        let payload = fauna_protocol::encode_canonical(
            &fauna_protocol::atproto_pds::SetExternalAppsEnabledRequest {
                enabled: false,
                extra: Default::default(),
            },
        )
        .unwrap();
        (meta.handler)(Arc::clone(&state), ACCOUNT, payload)
            .await
            .expect("the flip answers");
        assert!(
            conn.is_revoked(),
            "OFF closes the live socket (close code 4401)"
        );

        let (again, _rx2) = register_principal_connection(&state, binding).await;
        assert!(
            again.is_revoked(),
            "an upgrade while OFF closes at registration"
        );
        assert_eq!(
            account_standing(&state, &ACCOUNT, "https://app.example/client.json")
                .await
                .unwrap(),
            None,
            "the upgrade gate's standing read refuses while OFF"
        );
    }

    /// The `exp` deadline closes the socket (revokes the connection) when it
    /// passes, and not a moment before — on the injected clock.
    #[tokio::test(start_paused = true)]
    async fn the_session_closes_at_its_tokens_exp() {
        let ws = crate::ws::WsState::new();
        let (conn, _rx) = ws.subscribe_principal(PrincipalBinding {
            account: [1; 32],
            principal_id: vec![2],
            token_scopes: vec![],
        });
        let task = spawn_exp_deadline(Arc::clone(&conn), until_exp(NOW + 900, NOW));
        tokio::time::sleep(Duration::from_secs(899)).await;
        assert!(!conn.is_revoked(), "a second before exp the session lives");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(conn.is_revoked(), "past exp the connection closes 4401");
        task.await.unwrap();
    }
}
