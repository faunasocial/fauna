//! The media proxy's **playback ticket** — a URL-borne credential for
//! `GET /api/v1/media/proxy`, and `fauna.media.playback_ticket`, the WS-RPC kind
//! that mints it (`docs/goal/architecture/render-model.md` § D6c → *Inline
//! playback*, answer 4).
//!
//! A `<video src>` (and every native player the other apps use) cannot carry a
//! bearer header, so a bridged video playing through the proxy needs a
//! credential in the URL. The client's one authenticated channel asks for a
//! ticket on the proxied path a `ProxiedVideo` block carries; the nest appends
//!
//! ```text
//! exp = <unix second, MEDIA_TICKET_TTL ahead>
//! sig = base64url(HMAC-SHA-256(secret, "fauna media ticket v1\0" ‖ url ‖ "\0" ‖ exp))
//! ```
//!
//! where `url` is the remote url the proxy will dial (the decoded `url` query
//! value) and `exp` is its decimal digits as they ride the query. The proxy
//! route verifies the pair as its second credential beside the bearer
//! ([`crate::media_proxy_routes`]); a forged, expired or absent ticket is the
//! same 401 a missing bearer gets, so the unauthenticated surface is exactly as
//! wide as before. The key is [`crate::media_ticket_secret`], read under the
//! deployment seed and never seated here.

use std::sync::Arc;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use fauna_protocol::{
    decode_strict as decode,
    media_ticket::{KIND_MEDIA_PLAYBACK_TICKET, PlaybackTicketReply, PlaybackTicketRequest},
};

use crate::bridge_method_allowlist::require_permission_default as require_permission;
use crate::routes::AppState;
use crate::rpc_errors::{encode_reply, internal, malformed};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// How long a ticket fetches: an hour covers a feature-length pause, and a
/// leaked ticket fetches one public video through this nest for that hour and
/// nothing else. A constant — nobody would choose it (render-model.md § D6c,
/// answer 4).
pub const MEDIA_TICKET_TTL: Duration = Duration::from_secs(60 * 60);

/// The one path a ticket covers — the media proxy route.
pub const MEDIA_PROXY_PATH: &str = "/api/v1/media/proxy";

/// Domain separation for the MAC input: the purpose and its version, so a MAC
/// over the same bytes for any other reason never verifies as a ticket.
const TICKET_DOMAIN: &[u8] = b"fauna media ticket v1\0";

type HmacSha256 = Hmac<Sha256>;

fn mac(secret: &[u8; 32], url: &str, exp: i64) -> HmacSha256 {
    let mut m = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    m.update(TICKET_DOMAIN);
    m.update(url.as_bytes());
    m.update(b"\0");
    m.update(exp.to_string().as_bytes());
    m
}

/// The base64url `sig` over (`url`, `exp`).
pub fn sign(secret: &[u8; 32], url: &str, exp: i64) -> String {
    URL_SAFE_NO_PAD.encode(mac(secret, url, exp).finalize().into_bytes())
}

/// Whether (`exp`, `sig`) is a live ticket for `url` at `now`: the MAC verifies
/// (constant-time), `exp` has not passed, and `exp` lies no further ahead than
/// one [`MEDIA_TICKET_TTL`] — the most a mint ever grants.
pub fn verify(secret: &[u8; 32], url: &str, exp: i64, sig: &str, now: i64) -> bool {
    if exp < now || exp - now > MEDIA_TICKET_TTL.as_secs() as i64 {
        return false;
    }
    let Ok(presented) = URL_SAFE_NO_PAD.decode(sig) else {
        return false;
    };
    mac(secret, url, exp).verify_slice(&presented).is_ok()
}

/// The remote url a proxied `path` carries, if `path` is exactly this nest's
/// media-proxy form: [`MEDIA_PROXY_PATH`] with one `url` query parameter and
/// nothing else (a path already carrying a ticket is not re-ticketed).
fn proxied_url(path: &str) -> Option<String> {
    let (route, query) = path.split_once('?')?;
    if route != MEDIA_PROXY_PATH {
        return None;
    }
    let mut pairs = url::form_urlencoded::parse(query.as_bytes());
    let (key, url) = pairs.next()?;
    if key != "url" || url.is_empty() || pairs.next().is_some() {
        return None;
    }
    Some(url.into_owned())
}

/// `path` with a ticket good until `exp` appended.
fn ticketed_path(secret: &[u8; 32], url: &str, exp: i64) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("url", url)
        .append_pair("exp", &exp.to_string())
        .append_pair("sig", &sign(secret, url, exp))
        .finish();
    format!("{MEDIA_PROXY_PATH}?{query}")
}

/// The ticket secret, read under the deployment seed this generation holds.
/// `None` when the nest holds no deployment key or the row is absent or will
/// not open — the boot step seats it; a read never does.
pub(crate) async fn load_secret(state: &Arc<AppState>) -> Option<[u8; 32]> {
    let seed = state.nest_signing_key.as_ref()?.to_bytes();
    let db = state.db.clone();
    let loaded = tokio::task::spawn_blocking(move || {
        crate::media_ticket_secret::ticket_secret(&db.conn_blocking(), &seed)
    })
    .await;
    match loaded {
        Ok(Ok(secret)) => Some(secret),
        Ok(Err(e)) => {
            tracing::error!(error = %format!("{e:#}"), "media ticket: secret unavailable");
            None
        }
        Err(e) => {
            tracing::error!(error = %e, "media ticket: secret task failed");
            None
        }
    }
}

// ── fauna.media.playback_ticket ──────────────────────────────────────────────

fn playback_ticket_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_MEDIA_PLAYBACK_TICKET).await?;
            let req: PlaybackTicketRequest = decode(&payload).map_err(malformed)?;
            let url = proxied_url(&req.path).ok_or_else(|| {
                malformed(format!(
                    "not a media-proxy path: expected {MEDIA_PROXY_PATH}?url=<remote url>"
                ))
            })?;
            // A url the proxy would refuse on its text alone gets no ticket —
            // the dial-time guard still decides everything else.
            crate::ssrf::parse_https_url(&url).map_err(|e| malformed(format!("url: {e}")))?;
            let secret = load_secret(&state)
                .await
                .ok_or_else(|| internal("the media playback-ticket secret is unavailable"))?;
            let exp =
                fauna_core::data::Timestamp::now_secs_or_zero() + MEDIA_TICKET_TTL.as_secs() as i64;
            encode_reply(&PlaybackTicketReply {
                path: ticketed_path(&secret, &url, exp),
                ..Default::default()
            })
        })
    })
}

// ── Registration entry point ─────────────────────────────────────────────────

pub fn register_media_ticket_handlers(b: &mut RpcRouterBuilder) {
    // Replay-safe @5s, the protocol-crate metadata (`register_media_kinds`):
    // the mint writes nothing.
    b.add(
        KIND_MEDIA_PLAYBACK_TICKET,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: playback_ticket_handler(),
        },
    );
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db::CacheDb;

    const USER: [u8; 32] = [0x21u8; 32];
    const URL: &str = "https://cdn.example/v.mp4";

    /// A serving generation over `db`, built from the deployment seed it holds.
    pub(crate) async fn generation(db: &Arc<CacheDb>) -> Arc<AppState> {
        let db2 = db.clone();
        let seed = tokio::task::spawn_blocking(move || {
            crate::nest_kek::require_deployment_seed(&db2.conn_blocking()).expect("seed")
        })
        .await
        .expect("seed task");
        Arc::new(AppState {
            nest_signing_key: Some(ed25519_dalek::SigningKey::from_bytes(&seed)),
            ..AppState::for_test(db.clone())
        })
    }

    /// A booted nest (its satellites seated) with one registered user.
    pub(crate) async fn booted_state() -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
        crate::test_support::boot_mint(&db).await;
        db.create_user(&USER, "free", "viewer").await.expect("user");
        generation(&db).await
    }

    /// Mint a ticket for `path` through the real handler.
    pub(crate) async fn mint(state: &Arc<AppState>, path: &str) -> Result<String, String> {
        let req = fauna_protocol::encode_canonical(&PlaybackTicketRequest {
            path: path.into(),
            ..Default::default()
        })
        .expect("encode");
        match playback_ticket_handler()(state.clone(), USER, req).await {
            Ok(bytes) => Ok(decode::<PlaybackTicketReply>(&bytes).expect("decode").path),
            Err(e) => Err(e.code.to_string()),
        }
    }

    fn proxy_path(url: &str) -> String {
        let q = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("url", url)
            .finish();
        format!("{MEDIA_PROXY_PATH}?{q}")
    }

    #[test]
    fn a_signed_ticket_verifies_for_its_url_until_it_expires() {
        let secret = [7u8; 32];
        let now = 1_700_000_000;
        let exp = now + 60;
        let sig = sign(&secret, URL, exp);
        assert!(verify(&secret, URL, exp, &sig, now));
        assert!(verify(&secret, URL, exp, &sig, exp), "valid through exp");
        assert!(!verify(&secret, URL, exp, &sig, exp + 1), "expired");
        assert!(
            !verify(&secret, "https://cdn.example/other.mp4", exp, &sig, now),
            "another url"
        );
        assert!(!verify(&secret, URL, exp + 1, &sig, now), "another exp");
        assert!(!verify(&[8u8; 32], URL, exp, &sig, now), "another key");
        assert!(
            !verify(&secret, URL, exp, "not base64!", now),
            "garbage sig"
        );
    }

    /// No mint grants more than one TTL, so a validly signed `exp` further
    /// ahead than that is not one this nest issued — refused.
    #[test]
    fn an_exp_beyond_one_ttl_is_refused() {
        let secret = [7u8; 32];
        let now = 1_700_000_000;
        let exp = now + MEDIA_TICKET_TTL.as_secs() as i64 + 1;
        assert!(!verify(&secret, URL, exp, &sign(&secret, URL, exp), now));
    }

    /// The handler appends `exp` + `sig` that verify for the proxied url, and
    /// keeps the path's form so the proxy's query extractor reads it back.
    #[tokio::test]
    async fn the_mint_appends_a_ticket_that_verifies() {
        let state = booted_state().await;
        let ticketed = mint(&state, &proxy_path(URL)).await.expect("minted");
        let (route, query) = ticketed.split_once('?').expect("query");
        assert_eq!(route, MEDIA_PROXY_PATH);
        let pairs: std::collections::BTreeMap<String, String> =
            url::form_urlencoded::parse(query.as_bytes())
                .into_owned()
                .collect();
        assert_eq!(pairs["url"], URL);
        let exp: i64 = pairs["exp"].parse().expect("exp is a unix second");
        let now = fauna_core::data::Timestamp::now_secs_or_zero();
        assert!(exp > now && exp - now <= MEDIA_TICKET_TTL.as_secs() as i64);
        let secret = load_secret(&state).await.expect("secret");
        assert!(verify(&secret, URL, exp, &pairs["sig"], now));
    }

    /// Only this nest's proxy form is ticketed — anything else is a typed
    /// refusal, never a ticket over some other route or a re-ticket.
    #[tokio::test]
    async fn the_mint_refuses_a_path_that_is_not_the_proxy_form() {
        let state = booted_state().await;
        for path in [
            "/api/v1/blob/abcd".to_string(),
            format!("{MEDIA_PROXY_PATH}?url="),
            format!("{MEDIA_PROXY_PATH}?u={URL}"),
            format!("{}&exp=1&sig=AA", proxy_path(URL)),
            "https://evil.example/api/v1/media/proxy?url=x".to_string(),
            proxy_path("http://cdn.example/v.mp4"),
            proxy_path("https://169.254.169.254/latest/meta-data/"),
        ] {
            assert_eq!(
                mint(&state, &path).await,
                Err("fauna.protocol.malformed".to_string()),
                "{path}"
            );
        }
    }

    /// The handler reads the secret and never seats it: answered by a serving
    /// generation inside a deployment-seed rotation's hand-off window (the
    /// database holds the successor seed, the generation the retired one) on a
    /// nest whose boot step has not seated the row, it refuses and writes
    /// nothing — so the next rotation commits (`key-material-hierarchy.md`
    /// § Audience: deployment infrastructure → *Room-read keypair* → *When it
    /// mints*; the `oauth_issuer_handlers` `rotation_window` twin).
    #[tokio::test]
    async fn a_mint_in_the_rotation_window_seats_nothing() {
        use zeroize::Zeroizing;
        let (a, b, c) = (
            Zeroizing::new([0xa1u8; 32]),
            Zeroizing::new([0xb2u8; 32]),
            Zeroizing::new([0xc3u8; 32]),
        );
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
        let public = ed25519_dalek::SigningKey::from_bytes(&a)
            .verifying_key()
            .to_bytes();
        db.set_nest_keypair(&a[..], &public)
            .await
            .expect("seat seed");
        db.create_user(&USER, "free", "viewer").await.expect("user");
        let outgoing = generation(&db).await;

        db.rotate_deployment_seed(&a, &b)
            .await
            .expect("the ceremony runs")
            .expect("and commits");

        assert_eq!(
            mint(&outgoing, &proxy_path(URL)).await,
            Err("fauna.protocol.internal".to_string()),
            "no secret seated, so no ticket"
        );
        let db2 = db.clone();
        let rows: i64 = tokio::task::spawn_blocking(move || {
            db2.conn_blocking()
                .query_row("SELECT COUNT(*) FROM media_ticket_secret", [], |r| r.get(0))
                .expect("count")
        })
        .await
        .expect("count task");
        assert_eq!(
            rows, 0,
            "a mint inside the rotation window seated the secret under the retired seed"
        );
        db.rotate_deployment_seed(&b, &c)
            .await
            .expect("the next rotation commits")
            .expect("and is no rule refusal");
    }
}
