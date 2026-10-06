//! Shared SSRF-safe outbound-fetch guard.
//!
//! Any nest endpoint that dials a **caller-supplied** URL (the media privacy
//! proxy `media_proxy_routes`, the outbound federation dialer
//! `federation_channel::validate_peer_url` reached via `fauna.inbox.send`, the
//! ActivityPub dialer `activitypub/outbound.rs`, and every outbound Nostr relay
//! dial — the sync worker's pool, the bunker drain, the interaction fan-out and
//! the NIP-46 remote-link handshake, whose seat is the `RelayDialPolicy` that
//! `fauna_bridge_nostr::relay_client::RelayClient::connect` requires)
//! must ensure the target can never be a loopback / private / link-local / ULA /
//! cloud-metadata / CGNAT address. Otherwise an attacker turns the nest into a
//! confused deputy that reads cloud IMDS credentials (`169.254.169.254`) or
//! internal services from the public internet (SSRF — security-review
//! findings, tracked internally). The relay rule, including the refusal of a
//! private-network relay on a user's own list, is
//! `nest/network-exposure.md` § Rulings F7.
//!
//! Two defenses live here:
//!   * [`fauna_core::resolve::is_global_ip`] — a self-contained
//!     "globally-routable?" classifier (does **not** use the unstable std
//!     `IpAddr::is_global`, so it survives toolchain-pin bumps). Lifted into
//!     `fauna-core` so the client host-address reporter and the relay dialer
//!     share it; this guard consumes it, and
//!   * [`resolve_global_addrs`] — resolve a host and reject unless *every*
//!     resolved address is global, returning the verified addresses so the
//!     caller can **pin** them for the actual connection (defeating
//!     DNS-rebinding, where a name resolves "public" for the check then
//!     "internal" for the connection). The resolve-and-pin mechanism itself is
//!     [`fauna_core::resolve::resolve_permitted_addrs`], shared with the relay
//!     dialer; this is that resolver under the `is_global_ip` predicate.
//!
//! A third bounds what such a URL can send back: [`read_capped`] /
//! [`read_prefix`] / [`CappedBody`] (the media proxy's streamed relay) are the
//! only readers of a body it served, since the guarded client limits the dial
//! and its duration, not the bytes.

use std::net::{IpAddr, SocketAddr};

// The globally-routable classifier and the resolve-and-pin resolver live in
// `fauna-core` (shared with the client host-address reporter
// `fauna_client_dns::host_address` and the Nostr relay dialer); this guard
// consumes them.
use fauna_core::resolve::{AddrGuardError, is_global_ip, resolve_permitted_addrs};

/// Why an outbound URL was rejected.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SsrfError {
    #[error("invalid URL")]
    InvalidUrl,
    #[error("only https URLs allowed")]
    BadScheme,
    #[error("no host in URL")]
    NoHost,
    #[error("could not resolve host")]
    Resolve,
    #[error("address is not globally routable")]
    NonGlobal,
}

/// Resolve `host:port` and reject unless **every** resolved address is global.
/// Returns the verified [`SocketAddr`]s so the caller can pin them for the
/// connection (rebinding-safe). An IP-literal host is verified directly (no DNS,
/// so no rebinding window).
pub async fn resolve_global_addrs(host: &str, port: u16) -> Result<Vec<SocketAddr>, SsrfError> {
    // The shared resolver rejects if ANY resolved address fails the predicate
    // (a name that resolves to a mix of public + internal addresses must not be
    // dialable) and verifies an IP literal directly.
    resolve_permitted_addrs(host, port, is_global_ip)
        .await
        .map_err(|e| match e {
            AddrGuardError::Resolve => SsrfError::Resolve,
            AddrGuardError::NotPermitted => SsrfError::NonGlobal,
        })
}

/// The **deterministic** half of the guard: everything decidable from the URL
/// text alone, with no DNS lookup — https only, a host present, an IP-literal
/// host globally routable, and never the reserved `localhost` names (RFC 6761).
///
/// [`ssrf_safe_https_client`] runs this first, so the two can never disagree.
/// It is public in its own right for a caller that *stores* a caller-supplied
/// URL now and dials it later (`fauna.push.subscribe`): such a caller refuses
/// what is determinably undialable at store time, with an answer that depends
/// on nothing but the request, and leaves the resolving half — which can change
/// between the two moments — to the dial itself.
pub fn parse_https_url(url_str: &str) -> Result<url::Url, SsrfError> {
    let url = url::Url::parse(url_str).map_err(|_| SsrfError::InvalidUrl)?;
    if url.scheme() != "https" {
        return Err(SsrfError::BadScheme);
    }
    let global = match url.host().ok_or(SsrfError::NoHost)? {
        url::Host::Ipv4(ip) => is_global_ip(IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => is_global_ip(IpAddr::V6(ip)),
        url::Host::Domain(name) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            name != "localhost" && !name.ends_with(".localhost")
        }
    };
    if !global {
        return Err(SsrfError::NonGlobal);
    }
    Ok(url)
}

/// Validate a caller-supplied **https** URL for SSRF safety and return a
/// reqwest client with redirects disabled and DNS pinned to the verified
/// addresses, plus the parsed URL. The client follows **no** redirects so a
/// permitted public host cannot `302` to an internal address; the pinned DNS
/// override defeats rebinding.
///
/// `user_agent` sets the client's default `User-Agent`. It is `Option` rather
/// than a default because sending one is a **per-subsystem** decision, not a
/// blanket one: ActivityPub must send it (GoToSocial answers a UA-less request
/// with `418 I'm a teapot`, so federation with it is impossible without one —
/// see `activitypub::outbound::AP_USER_AGENT`), whereas the media proxy and
/// link-preview fetchers deliberately stay unattributed, since a UA naming this
/// nest would tell every fetched origin which deployment is looking. Passing
/// `None` preserves reqwest's default of sending no `User-Agent` at all.
pub async fn ssrf_safe_https_client(
    url_str: &str,
    timeout: std::time::Duration,
    user_agent: Option<&str>,
) -> Result<(reqwest::Client, url::Url), SsrfError> {
    let (builder, url) = guarded_builder(url_str, user_agent).await?;
    let client = builder
        .timeout(timeout)
        .build()
        .map_err(|_| SsrfError::Resolve)?;
    Ok((client, url))
}

/// [`ssrf_safe_https_client`] for a body the caller **streams** onward rather
/// than reads whole — the media proxy relaying a video. The same guard (https
/// only, every resolved address global, redirects off, resolution pinned), but
/// no whole-request deadline, which would cut a long stream over a slow link:
/// `connect_timeout` bounds the dial and `read_timeout` each wait for the next
/// read (the head included), so a stalled upstream still ends.
pub async fn ssrf_safe_https_streaming_client(
    url_str: &str,
    connect_timeout: std::time::Duration,
    read_timeout: std::time::Duration,
    user_agent: Option<&str>,
) -> Result<(reqwest::Client, url::Url), SsrfError> {
    let (builder, url) = guarded_builder(url_str, user_agent).await?;
    let client = builder
        .connect_timeout(connect_timeout)
        .read_timeout(read_timeout)
        .build()
        .map_err(|_| SsrfError::Resolve)?;
    Ok((client, url))
}

/// The guard both clients above share: validate `url_str`, resolve and verify
/// its host, and return a builder with redirects disabled and DNS pinned to the
/// verified addresses — the caller adds only its timeouts.
async fn guarded_builder(
    url_str: &str,
    user_agent: Option<&str>,
) -> Result<(reqwest::ClientBuilder, url::Url), SsrfError> {
    let url = parse_https_url(url_str)?;
    let host = url.host_str().ok_or(SsrfError::NoHost)?;
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs = resolve_global_addrs(host, port).await?;

    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        // Pin resolution for this host to the addresses we just verified, so the
        // connection cannot re-resolve to a different (internal) address.
        .resolve_to_addrs(host, &addrs);
    if let Some(ua) = user_agent {
        builder = builder.user_agent(ua);
    }
    Ok((builder, url))
}

/// Why [`guarded_dial`] built no client.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GuardedDialError {
    #[error("url did not parse")]
    InvalidUrl,
    #[error("target refused by the fetch guard: {0}")]
    Refused(String),
    #[error("network error")]
    Network,
}

/// The dial an **attacker-directed** fetch makes — a third party's metadata
/// document, its plugin module, a hosted plugin's own outbound request — with
/// both seats of the nest's SSRF policy over **one** parse of the URL:
/// [`resolve_global_addrs`] (every address global, kept to pin the connection
/// to) and [`fauna_bridge_atproto::fetch_guard::check_fetch_target`] (the pure
/// policy: a public, registrable DNS name over those addresses). The client
/// follows no redirect — a permitted host must not `302` to a target neither
/// seat saw — and dials only the verified addresses. The caller reads the
/// body through [`read_capped`]; `builder` carries its own timeout.
///
/// The URL is parsed here, by the component that dials, and handed back: the
/// caller sends the request to the returned [`url::Url`], never to the string
/// it passed in, so the check and the dial cannot be two different parses.
pub async fn guarded_dial(
    url_str: &str,
    builder: reqwest::ClientBuilder,
) -> Result<(reqwest::Client, url::Url), GuardedDialError> {
    use fauna_bridge_atproto::fetch_guard::{FetchTarget, FetchTargetVerdict, check_fetch_target};
    let parsed = url::Url::parse(url_str).map_err(|_| GuardedDialError::InvalidUrl)?;
    let host = parsed
        .host_str()
        .ok_or_else(|| {
            GuardedDialError::Refused(
                fauna_bridge_atproto::fetch_guard::DENY_NOT_PUBLIC_NAME.to_string(),
            )
        })?
        .to_string();
    let port = parsed.port_or_known_default().unwrap_or(443);

    // Seat one: resolve, refuse any non-global address, keep the verified
    // set to pin the connection to.
    let addrs = resolve_global_addrs(&host, port)
        .await
        .map_err(|e| GuardedDialError::Refused(e.to_string()))?;

    // Seat two: the pure policy over the components just parsed and the
    // addresses about to be dialled.
    let verdict = check_fetch_target(FetchTarget {
        scheme: parsed.scheme().to_string(),
        host: host.clone(),
        resolved_ips: addrs.iter().map(|a| a.ip().to_string()).collect(),
    });
    if let FetchTargetVerdict::Deny { reason } = verdict {
        return Err(GuardedDialError::Refused(reason));
    }

    let client = builder
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(&host, &addrs)
        .build()
        .map_err(|_| GuardedDialError::Network)?;
    Ok((client, parsed))
}

/// The capped body reads — lifted to shared Rust (`fauna_nest_http::capped`)
/// so the sync engine's segment pull bounds its downloads with the same
/// helper. Every body a caller-supplied URL served is read through
/// [`read_capped`], [`read_prefix`] or — streamed onward, never collected —
/// [`CappedBody`]: the guarded client above bounds *where* such a URL can dial
/// and for how long, but not how much it may send back.
pub use fauna_nest_http::capped::{CappedBody, CappedReadError, read_capped, read_prefix};

/// A loopback HTTP server that answers one request with a canned head and body,
/// for the capped-read pins here and at their callers. `endless` streams `body`
/// forever after the head — a remote that never stops sending — which only a
/// read that stops at its cap survives.
/// Under `test-hooks` **only**, a resolve-override + extra-root path so the nest
/// can dial a real-hostname HTTPS peer (e.g. `https://mastodon.test`, or a
/// third-party client's metadata document) that is
/// published on a **loopback** port with a test-CA cert. Same double-gate as the
/// literal-loopback exemption (feature **and** env); production compiles neither.
///
/// `resolve_json` maps hostnames to `ip:port` connect targets. Two invariants
/// keep this from ever widening SSRF:
///
///   * **Loopback-ONLY** — every resolve target must be a loopback address, or
///     the dial is refused (`NonGlobal`). A private-range, ULA, CGNAT or
///     cloud-metadata target is rejected exactly as in the guarded path, so the
///     hook can never be bent into a private-range exemption.
///   * **Mapped-host-ONLY** — the exemption fires *only* when the URL's host is
///     a key in the map. Any other host returns `Ok(None)` and falls through to
///     the full SSRF guard, so a real remote stays guarded even under the
///     harness.
///
/// `extra_ca_pem`, when present, adds one root to the outbound rustls store —
/// required because `rustls-tls` bakes the webpki roots and ignores
/// `SSL_CERT_FILE` (workspace `Cargo.toml`). A malformed PEM fails loud rather
/// than surfacing later as an opaque handshake failure.
///
/// Pure in its environment: the caller reads the env vars and the CA file, so
/// this is unit-testable without touching process-global state.
/// `builder` is the caller's own client shape (redirects, timeout, user
/// agent); the override adds only the resolve map and the extra root.
#[cfg(feature = "test-hooks")]
pub(crate) fn test_resolve_client(
    url_str: &str,
    resolve_json: &str,
    extra_ca_pem: Option<&[u8]>,
    builder: reqwest::ClientBuilder,
) -> Result<Option<(reqwest::Client, url::Url)>, SsrfError> {
    let map: std::collections::BTreeMap<String, String> =
        serde_json::from_str(resolve_json).map_err(|_| SsrfError::InvalidUrl)?;

    let url = url::Url::parse(url_str).map_err(|_| SsrfError::InvalidUrl)?;
    let host = url.host_str().ok_or(SsrfError::InvalidUrl)?;

    // Mapped-host-ONLY: an unmapped host is not exempted — it must fall through
    // to the full SSRF guard, so we return before building any override client.
    if !map.contains_key(host) {
        return Ok(None);
    }

    let mut builder = builder;

    for (mapped_host, target) in &map {
        let addr: std::net::SocketAddr = target.parse().map_err(|_| SsrfError::InvalidUrl)?;
        // Loopback-ONLY: the resolve map never widens SSRF beyond loopback.
        if !addr.ip().is_loopback() {
            return Err(SsrfError::NonGlobal);
        }
        builder = builder.resolve(mapped_host, addr);
    }

    if let Some(pem) = extra_ca_pem {
        // `from_pem_bundle` (not `from_pem`) so we can *count* the parsed roots:
        // reqwest defers parse errors and rustls silently drops unparseable
        // certs at `build()`, so a broken/empty CA file would otherwise add zero
        // roots with no error and surface only as an opaque handshake failure at
        // the first dial. A zero-cert result is that broken file — fail loud.
        let certs = reqwest::Certificate::from_pem_bundle(pem).map_err(|_| SsrfError::Resolve)?;
        if certs.is_empty() {
            return Err(SsrfError::Resolve);
        }
        for cert in certs {
            builder = builder.add_root_certificate(cert);
        }
    }

    let client = builder.build().map_err(|_| SsrfError::Resolve)?;
    Ok(Some((client, url)))
}

#[cfg(test)]
pub(crate) mod test_server {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serve one response; returns the `http://127.0.0.1:<port>/` URL.
    pub(crate) async fn serve_once(head: String, body: Vec<u8>, endless: bool) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // spawn-ok(test): a one-shot in-test HTTP responder.
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = [0u8; 4096];
            let _ = sock.read(&mut req).await;
            if sock.write_all(head.as_bytes()).await.is_err() {
                return;
            }
            while sock.write_all(&body).await.is_ok() && endless {}
        });
        format!("http://{addr}/")
    }

    /// A `Connection: close` head with no `Content-Length`, so only the
    /// streaming guard stands between the reader and the body.
    pub(crate) fn head_without_length(status: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/activity+json\r\n\
             Connection: close\r\n\r\n"
        )
    }

    /// A head that declares `len` bytes up front.
    pub(crate) fn head_with_length(status: &str, len: usize) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/activity+json\r\n\
             Content-Length: {len}\r\nConnection: close\r\n\r\n"
        )
    }

    pub(crate) async fn get(url: &str) -> reqwest::Response {
        reqwest::Client::new().get(url).send().await.unwrap()
    }

    /// The pins below would HANG, not fail, if a read ran to the end of an
    /// endless body; bound them so a regression reports instead of wedging.
    pub(crate) async fn within<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(std::time::Duration::from_secs(10), fut)
            .await
            .expect("the read ran past its cap into an endless body")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> IpAddr {
        IpAddr::V4(s.parse().unwrap())
    }

    // The `is_global_ip` classifier's own unit tests moved with it to
    // `fauna-core` (`fauna_core::resolve` tests). These exercise the nest-side
    // `resolve_global_addrs` / `ssrf_safe_https_client` guards that consume it.

    #[tokio::test]
    async fn resolve_rejects_ip_literal_imds() {
        assert_eq!(
            resolve_global_addrs("169.254.169.254", 443).await,
            Err(SsrfError::NonGlobal)
        );
        assert_eq!(
            resolve_global_addrs("[::1]", 443).await,
            Err(SsrfError::NonGlobal)
        );
    }

    #[tokio::test]
    async fn resolve_accepts_public_literal() {
        let addrs = resolve_global_addrs("8.8.8.8", 443).await.unwrap();
        assert_eq!(addrs, vec![SocketAddr::new(v4("8.8.8.8"), 443)]);
    }

    /// The store-time half answers from the URL text alone — so it must refuse
    /// every determinably undialable shape without a resolver, and must NOT
    /// refuse a name it would need DNS to judge (the dial settles that).
    #[test]
    fn parse_refuses_what_the_text_alone_decides() {
        for (url, want) in [
            ("not a url", SsrfError::InvalidUrl),
            ("http://push.example.com/x", SsrfError::BadScheme),
            ("https://127.0.0.1:8080/x", SsrfError::NonGlobal),
            ("https://10.0.0.7/x", SsrfError::NonGlobal),
            (
                "https://169.254.169.254/latest/meta-data/",
                SsrfError::NonGlobal,
            ),
            ("https://[::1]/x", SsrfError::NonGlobal),
            ("https://[fd00::1]/x", SsrfError::NonGlobal),
            // Encoded-decimal literal: the URL parser normalizes it to 127.0.0.1.
            ("https://2130706433/x", SsrfError::NonGlobal),
            ("https://localhost/x", SsrfError::NonGlobal),
            ("https://LOCALHOST./x", SsrfError::NonGlobal),
            ("https://relay.localhost/x", SsrfError::NonGlobal),
        ] {
            assert_eq!(parse_https_url(url).unwrap_err(), want, "{url}");
        }
        assert!(parse_https_url("https://8.8.8.8/x").is_ok());
        // A name is the dial's to judge: no resolver is consulted here.
        assert!(parse_https_url("https://updates.push.services.mozilla.com/wpush/v2/x").is_ok());
        assert!(parse_https_url("https://no-such-host.invalid/x").is_ok());
    }

    #[tokio::test]
    async fn client_rejects_non_https_and_imds() {
        let t = std::time::Duration::from_secs(5);
        assert_eq!(
            ssrf_safe_https_client("http://example.com/", t, None)
                .await
                .unwrap_err(),
            SsrfError::BadScheme
        );
        assert_eq!(
            ssrf_safe_https_client("https://169.254.169.254/latest/meta-data/", t, None)
                .await
                .unwrap_err(),
            SsrfError::NonGlobal
        );
        // Encoded-decimal IP literal that resolves to loopback (2130706433 = 127.0.0.1).
        assert_eq!(
            ssrf_safe_https_client("https://2130706433/", t, None)
                .await
                .unwrap_err(),
            SsrfError::NonGlobal
        );
    }

    use super::test_server::{get, head_with_length, head_without_length, serve_once, within};

    #[tokio::test]
    async fn read_capped_returns_a_body_at_the_cap() {
        let url = serve_once(head_without_length("200 OK"), vec![b'x'; 64], false).await;
        assert_eq!(read_capped(get(&url).await, 64).await, Ok(vec![b'x'; 64]));
    }

    #[tokio::test]
    async fn read_capped_refuses_an_unlengthed_body_one_byte_over() {
        let url = serve_once(head_without_length("200 OK"), vec![b'x'; 65], false).await;
        assert_eq!(
            read_capped(get(&url).await, 64).await,
            Err(CappedReadError::TooLarge)
        );
    }

    /// A declared length over the cap is refused before a byte of body is read.
    #[tokio::test]
    async fn read_capped_refuses_a_declared_length_over_the_cap() {
        let url = serve_once(head_with_length("200 OK", 1 << 30), vec![b'x'; 8], false).await;
        assert_eq!(
            read_capped(get(&url).await, 64).await,
            Err(CappedReadError::TooLarge)
        );
    }

    #[tokio::test]
    async fn read_capped_stops_on_an_endless_body() {
        let url = serve_once(head_without_length("200 OK"), vec![b'x'; 8192], true).await;
        assert_eq!(
            within(read_capped(get(&url).await, 1 << 20)).await,
            Err(CappedReadError::TooLarge)
        );
    }

    #[tokio::test]
    async fn read_prefix_keeps_a_short_body_whole() {
        let url = serve_once(head_without_length("500 Oops"), b"nope".to_vec(), false).await;
        assert_eq!(read_prefix(get(&url).await, 64).await, b"nope".to_vec());
    }

    #[tokio::test]
    async fn read_prefix_truncates_an_endless_body_at_the_cap() {
        let url = serve_once(head_without_length("500 Oops"), vec![b'e'; 8192], true).await;
        assert_eq!(
            within(read_prefix(get(&url).await, 100)).await,
            vec![b'e'; 100]
        );
    }
}
