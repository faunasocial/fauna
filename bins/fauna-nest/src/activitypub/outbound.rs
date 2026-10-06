//! The single SSRF-guarded dialer for every ActivityPub outbound request.
//!
//! AP dials URLs that an untrusted remote chose, at **two** sinks — and both are
//! reachable from the anonymous inbox:
//!
//!   * **the remote-actor fetch** (`inbox_routes::fetch_remote_actor`) — the
//!     `actor` URI lifted straight out of an unverified inbox POST and dialed
//!     *before* HTTP-Signature verification, which cannot run until we hold the
//!     actor's key. Also reached by `bridge_provider` when a local user follows
//!     a remote actor by URI.
//!   * **the delivery POST** (`sync_worker::deliver`) — `target_inbox`, taken
//!     verbatim from the `inbox` / `endpoints.sharedInbox` of the actor document
//!     that same remote served us (`inbox_routes` Follow → `enqueue_delivery`).
//!
//! Guarding only the fetch would leave the POST wide open: a remote serves its
//! actor document from a perfectly global address and *names* a loopback inbox,
//! and the delivery worker dials it. So both route through here, exactly as
//! `media_proxy_routes::proxy_remote_media` funnels every bridge media fetch
//! through one place.
//!
//! `crate::ssrf`'s module doc makes this mandatory for any caller-supplied-URL
//! fetch: https-only, every resolved address globally routable, redirects
//! disabled, DNS pinned against rebinding. Ruling:
//! `docs/goal/architecture/nest/network-exposure.md` § Rulings F1.

use std::time::Duration;

/// Outbound AP dials are bounded. The general-purpose `state.http_client` this
/// replaced carried no timeout, so a hostile remote could hold a connection open
/// indefinitely.
const AP_OUTBOUND_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest remote actor document we read. The fetch runs before any
/// signature is checked, on a URI the unverified activity named, so the reply
/// is read through `crate::ssrf::read_capped` at this bound — the same 1 MiB
/// the inbox POST itself is held to (`inbox_routes::MAX_BODY_BYTES`). Real
/// actor documents are a few KiB.
pub(crate) const AP_ACTOR_DOCUMENT_MAX_BYTES: usize = 1_048_576;

/// How much of a remote's error body we keep for a diagnostic.
const AP_ERROR_BODY_MAX_BYTES: usize = 4 * 1024;

/// The diagnostic prefix of an error reply from an AP dial: at most
/// [`AP_ERROR_BODY_MAX_BYTES`] read, the rest never pulled off the socket.
pub(crate) async fn error_body_snippet(resp: reqwest::Response) -> String {
    String::from_utf8_lossy(&crate::ssrf::read_prefix(resp, AP_ERROR_BODY_MAX_BYTES).await)
        .into_owned()
}

/// The `User-Agent` every outbound AP request carries.
///
/// **Not cosmetic — a UA-less AP request is unroutable on part of the
/// fediverse.** GoToSocial answers a request with no `User-Agent` with
/// `418 I'm a teapot` (`{"error": "I'm a teapot: no user-agent sent with
/// request"}`), and since the *first* thing an inbound activity makes us do is
/// fetch the sending actor's document to get the key we verify it with, a
/// missing UA means no key, no verified `Follow`, no `Accept` — federation with
/// every GoToSocial instance simply does not happen. Mastodon tolerates the
/// omission, which is exactly why this survived until a second implementation
/// was put in front of it (`tests/e2e-unified/tests/platform/fediverse/`).
///
/// Deliberately static, carrying **no instance domain**. The conventional
/// fediverse form appends `(+https://<instance>/)`, but every AP request we make
/// already identifies this deployment precisely — signed GETs carry
/// `keyId=https://<domain>/ap/instance#main-key`, deliveries carry the actor —
/// so threading the domain here would add a parameter to the single dialer and
/// tell a remote nothing it was not already told.
const AP_USER_AGENT: &str = concat!(
    "fauna-nest/",
    env!("CARGO_PKG_VERSION"),
    " (+https://fauna.social/)"
);

/// The guarded dial with **no test hook in front of it** — byte-for-byte the
/// body `ap_outbound_client` compiles down to under `not(feature = "test-hooks")`,
/// i.e. in every artifact we ship.
///
/// It exists as its own function so the production posture is assertable from a
/// build the merge gate actually runs. `nest-lib-test-check`'s two arms are
/// all-features-off and all-features-on, so a test gated
/// `#[cfg(not(feature = "test-hooks"))]` *inside* the activitypub tree needs a
/// third shape (activitypub ON, test-hooks OFF) that neither arm has — it was
/// executed by nothing but the Docker build until 2026-08-06. Being cfg-free,
/// this entry point is reachable from the union arm, and it is also independent
/// of the ambient environment: no hook env var can change its answer.
/// Pinned by `test_no_nest_test_is_dark_to_the_two_arm_lib_gate`.
async fn ap_outbound_client_strict(
    url_str: &str,
) -> Result<(reqwest::Client, url::Url), crate::ssrf::SsrfError> {
    crate::ssrf::ssrf_safe_https_client(url_str, AP_OUTBOUND_TIMEOUT, Some(AP_USER_AGENT)).await
}

/// Build an SSRF-guarded client + parsed URL for an AP dial to `url_str`.
///
/// The returned client has redirects disabled and DNS pinned to the verified
/// addresses, so a permitted public host can neither `302` nor re-resolve to an
/// internal address.
pub(crate) async fn ap_outbound_client(
    url_str: &str,
) -> Result<(reqwest::Client, url::Url), crate::ssrf::SsrfError> {
    #[cfg(feature = "test-hooks")]
    if test_loopback_allowed(url_str) {
        let url = url::Url::parse(url_str).map_err(|_| crate::ssrf::SsrfError::InvalidUrl)?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(AP_OUTBOUND_TIMEOUT)
            .user_agent(AP_USER_AGENT)
            .build()
            .map_err(|_| crate::ssrf::SsrfError::Resolve)?;
        return Ok((client, url));
    }

    // The real-Mastodon interop harness dials `https://mastodon.test/...` — a
    // real hostname with a real (test-CA) cert, published on a loopback port.
    // The literal-loopback exemption above cannot cover it (the host is not
    // loopback), so a second, equally narrow hook resolve-overrides the mapped
    // host to its loopback port and trusts the test CA. Only consulted when
    // `FAUNA_TEST_AP_RESOLVE_JSON` is set, so a normal test build never reads
    // the CA file or touches this path.
    #[cfg(feature = "test-hooks")]
    if let Ok(resolve_json) = std::env::var("FAUNA_TEST_AP_RESOLVE_JSON") {
        let extra_ca = match std::env::var_os("FAUNA_TEST_AP_EXTRA_CA_PEM") {
            Some(path) => Some(std::fs::read(path).map_err(|_| crate::ssrf::SsrfError::Resolve)?),
            None => None,
        };
        if let Some(client_url) = test_resolve_client(url_str, &resolve_json, extra_ca.as_deref())?
        {
            return Ok(client_url);
        }
    }

    ap_outbound_client_strict(url_str).await
}

/// The `(request-target)` value an HTTP signature is computed over: path **and**
/// query.
///
/// The remote rebuilds the request-target from the line it received, so signing
/// the bare path would mismatch for any URL carrying a query and the signature
/// would be rejected — a failure that only shows up against the peers whose
/// actor or inbox URLs happen to be parameterised, which is exactly the kind of
/// interop bug that hides until production.
pub(crate) fn signing_path(url: &url::Url) -> String {
    match url.query() {
        Some(query) => format!("{}?{}", url.path(), query),
        None => url.path().to_string(),
    }
}

/// The AP dial's resolve-override (`crate::ssrf::test_resolve_client` owns the
/// two invariants), with the AP client's timeout and user agent.
#[cfg(feature = "test-hooks")]
fn test_resolve_client(
    url_str: &str,
    resolve_json: &str,
    extra_ca_pem: Option<&[u8]>,
) -> Result<Option<(reqwest::Client, url::Url)>, crate::ssrf::SsrfError> {
    crate::ssrf::test_resolve_client(
        url_str,
        resolve_json,
        extra_ca_pem,
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(AP_OUTBOUND_TIMEOUT)
            .user_agent(AP_USER_AGENT),
    )
}

/// Under `test-hooks` **only**, and only when `FAUNA_TEST_AP_ALLOW_LOOPBACK` is
/// set, permit a dial to a **loopback** host so the AP e2e can federate two
/// nests and an in-test fediverse server over `http://127.0.0.1:<port>`.
///
/// Deliberately narrow: loopback alone is exempted, so a private-range, ULA,
/// CGNAT or cloud-metadata target (`169.254.169.254`) is still rejected even in
/// a test build — which is why the rejection tests below hold under either
/// feature set. Production is built without `test-hooks` (see `Cargo.toml`), so
/// none of this is compiled into the shipping nest.
#[cfg(feature = "test-hooks")]
fn test_loopback_allowed(url_str: &str) -> bool {
    if std::env::var_os("FAUNA_TEST_AP_ALLOW_LOOPBACK").is_none() {
        return false;
    }
    url::Url::parse(url_str)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .is_some_and(|h| {
            h == "localhost"
                || h.parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssrf::SsrfError;

    /// The F1 gate: an `actor` naming the cloud metadata service must be
    /// rejected before any socket is opened. IMDS is not loopback, so this holds
    /// with or without `test-hooks`.
    #[tokio::test]
    async fn rejects_imds_actor() {
        assert_eq!(
            ap_outbound_client("https://169.254.169.254/latest/meta-data/")
                .await
                .map(|_| ()),
            Err(SsrfError::NonGlobal)
        );
    }

    /// A private-range inbox — the shape a hostile actor document names to aim
    /// the delivery POST at an internal service.
    #[tokio::test]
    async fn rejects_private_range_inbox() {
        assert_eq!(
            ap_outbound_client("https://192.168.1.10/inbox")
                .await
                .map(|_| ()),
            Err(SsrfError::NonGlobal)
        );
    }

    /// Plain http is refused outright: AP over cleartext would also strip the
    /// guard's rebinding protection.
    #[tokio::test]
    async fn rejects_http_scheme() {
        assert_eq!(
            ap_outbound_client("http://example.com/users/bob")
                .await
                .map(|_| ()),
            Err(SsrfError::BadScheme)
        );
    }

    /// Loopback — the nest↔MDA↔SNI-router split (CalDAV `127.0.0.1:8444`, nest
    /// `:3000`). Rejected in the shipped build; permitted only under
    /// `test-hooks` + `FAUNA_TEST_AP_ALLOW_LOOPBACK`, which no production binary
    /// compiles.
    ///
    /// Asserted against `ap_outbound_client_strict` — the exact body
    /// `ap_outbound_client` becomes under `not(feature = "test-hooks")` — rather
    /// than gating the test itself `#[cfg(not(feature = "test-hooks"))]`, which
    /// is what made it the one lib test no merge-gate arm executed: it needed
    /// activitypub ON *and* test-hooks OFF, a shape only the Docker build has.
    /// The hook's own closure is pinned separately by
    /// `test_hook_is_closed_without_the_env_var`.
    #[tokio::test]
    async fn rejects_loopback_inbox() {
        assert_eq!(
            ap_outbound_client_strict("https://127.0.0.1:8444/inbox")
                .await
                .map(|_| ()),
            Err(SsrfError::NonGlobal)
        );
    }

    /// The test hook stays shut unless the env var is set, so merely building
    /// with `test-hooks` never loosens the guard.
    #[cfg(feature = "test-hooks")]
    #[test]
    fn test_hook_is_closed_without_the_env_var() {
        assert!(!test_loopback_allowed("http://127.0.0.1:9999/inbox"));
    }

    /// The resolve-override hook (`FAUNA_TEST_AP_RESOLVE_JSON` +
    /// `FAUNA_TEST_AP_EXTRA_CA_PEM`) that lets the real-Mastodon harness dial a
    /// real-hostname HTTPS peer published on a loopback port. Every assertion is
    /// on the pure `test_resolve_client`, so no process-global env is touched.
    #[cfg(feature = "test-hooks")]
    mod resolve_override {
        use super::*;

        const MAP: &str = r#"{"mastodon.test":"127.0.0.1:8443"}"#;

        /// A self-signed test CA (Ed25519, CN=fauna-test-ca) — a well-formed PEM
        /// so the happy path builds. The harness mints its own per-run CA; this
        /// is only to prove `Certificate::from_pem` + `add_root_certificate`
        /// wire against our pinned reqwest/rustls (design risk #1).
        const TEST_CA_PEM: &[u8] = b"-----BEGIN CERTIFICATE-----\n\
MIIBRDCB96ADAgECAhQtGTdrtyYlAqJPo3DDJJ1c5hqEPDAFBgMrZXAwGDEWMBQG\n\
A1UEAwwNZmF1bmEtdGVzdC1jYTAeFw0yNjA3MTkyMzI0MTlaFw0zNjA3MTYyMzI0\n\
MTlaMBgxFjAUBgNVBAMMDWZhdW5hLXRlc3QtY2EwKjAFBgMrZXADIQAHCAPb5hry\n\
F76zXZdxh+BgdvOdu+fSkd7dA21etT9vvKNTMFEwHQYDVR0OBBYEFEc34722LVcq\n\
cGi57AP5WP7OX/xcMB8GA1UdIwQYMBaAFEc34722LVcqcGi57AP5WP7OX/xcMA8G\n\
A1UdEwEB/wQFMAMBAf8wBQYDK2VwA0EAIVr0BX+BPVdmXWL6BYbkJakVJxXvfD0Z\n\
FyjvH0MlX+wAXSsX+j/m3qCuDBK/kwxu/sjKnepxVAT29PuRjB4UAw==\n\
-----END CERTIFICATE-----\n";

        /// A mapped host with a loopback target is exempted: the override client
        /// builds and the URL's host is preserved unchanged, so SNI + the `Host`
        /// header stay `mastodon.test` (the cert + actor-id hostname).
        #[test]
        fn mapped_loopback_host_is_exempted() {
            let (_, url) = test_resolve_client("https://mastodon.test/users/bob", MAP, None)
                .expect("valid map")
                .expect("host is mapped → exemption fires");
            assert_eq!(url.host_str(), Some("mastodon.test"));
        }

        /// An UNmapped host must NOT get the exemption — it falls through to the
        /// full SSRF guard (`Ok(None)`), so a real remote stays guarded even
        /// while the harness runs.
        #[test]
        fn unmapped_host_falls_through_to_ssrf() {
            let out = test_resolve_client("https://evil.example/users/bob", MAP, None)
                .expect("valid map");
            assert!(out.is_none(), "unmapped host must not be exempted");
        }

        /// Loopback-ONLY: a resolve target that is not loopback is refused, so
        /// the hook can never be bent into a private-range SSRF exemption.
        #[test]
        fn non_loopback_resolve_target_is_refused() {
            let map = r#"{"mastodon.test":"10.0.0.5:8443"}"#;
            assert_eq!(
                test_resolve_client("https://mastodon.test/x", map, None).map(|_| ()),
                Err(SsrfError::NonGlobal)
            );
        }

        /// The cloud-metadata address — the canonical SSRF aim — is refused as a
        /// resolve target like any other non-loopback address.
        #[test]
        fn imds_resolve_target_is_refused() {
            let map = r#"{"mastodon.test":"169.254.169.254:80"}"#;
            assert_eq!(
                test_resolve_client("https://mastodon.test/x", map, None).map(|_| ()),
                Err(SsrfError::NonGlobal)
            );
        }

        /// Malformed JSON fails loud rather than silently disabling the override
        /// (which would surface as a confusing SSRF rejection downstream).
        #[test]
        fn malformed_json_is_an_error() {
            assert_eq!(
                test_resolve_client("https://mastodon.test/x", "not json", None).map(|_| ()),
                Err(SsrfError::InvalidUrl)
            );
        }

        /// A malformed extra-CA PEM fails loud — a silently-dropped CA would
        /// surface later only as an opaque TLS handshake failure.
        #[test]
        fn malformed_extra_ca_is_an_error() {
            assert_eq!(
                test_resolve_client("https://mastodon.test/x", MAP, Some(b"not a pem")).map(|_| ()),
                Err(SsrfError::Resolve)
            );
        }

        /// A well-formed extra-CA PEM is accepted and the override client builds
        /// — the risk-#1 wiring (`Certificate::from_pem` + `add_root_certificate`
        /// against pinned reqwest/rustls) compiles and runs.
        #[test]
        fn valid_extra_ca_builds() {
            let (_, url) = test_resolve_client("https://mastodon.test/x", MAP, Some(TEST_CA_PEM))
                .expect("valid ca")
                .expect("mapped host");
            assert_eq!(url.host_str(), Some("mastodon.test"));
        }
    }
}
