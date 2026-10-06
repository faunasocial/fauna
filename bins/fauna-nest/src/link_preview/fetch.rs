//! SSRF-safe, redirect-following, size/time-capped outbound fetch for the D4
//! link-preview resolver.
//!
//! The existing [`crate::ssrf::ssrf_safe_https_client`] is https-only and
//! follows **no** redirects; D4 needs http(s) plus capped redirect-following
//! (`render-model.md` § D4: "cap redirects … re-check after redirects"). So this
//! reuses the audited [`crate::ssrf::resolve_global_addrs`] classifier (which
//! rejects loopback / private / link-local / CGNAT / cloud-metadata targets and
//! returns pinned addresses, defeating DNS-rebinding) and drives the redirect
//! loop here — re-validating **every** hop.
//!
//! There is deliberately **no loopback carve-out** (unlike `federation_channel`'s
//! admin-config peer URLs): link-preview URLs come from arbitrary user-posted
//! content, so `http://127.0.0.1:<internal-port>/` must never be reachable. The
//! e2e success path uses a `test-hooks`-gated fetcher override instead.

use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;

use crate::ssrf::{self, SsrfError};

/// Default per-request (per-hop) timeout. The kind deadline (`kind.rs`, 30 s)
/// envelopes the whole resolve; this bounds each individual hop.
pub const DEFAULT_HOP_TIMEOUT: Duration = Duration::from_secs(8);
/// Default redirect cap.
pub const DEFAULT_MAX_REDIRECTS: usize = 5;

/// A successful fetch: the final (post-redirect) URL, the raw `Content-Type`
/// header, and the size-capped body bytes.
#[derive(Debug)]
pub struct FetchedResource {
    /// Final URL after redirects — the base for resolving a relative `og:image`.
    pub url: url::Url,
    /// Raw `Content-Type` header (may be empty).
    pub content_type: String,
    pub body: Bytes,
}

/// Why a link-preview fetch failed. All collapse to a render `Failed`, but the
/// distinct variants make the SSRF/cap behaviour unit-testable.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FetchError {
    #[error("url rejected by ssrf guard: {0}")]
    Ssrf(#[from] SsrfError),
    #[error("unsupported scheme (http/https only)")]
    BadScheme,
    #[error("too many redirects")]
    TooManyRedirects,
    #[error("redirect without a usable Location")]
    BadRedirect,
    #[error("upstream status {0}")]
    Status(u16),
    #[error("response exceeded size cap")]
    TooLarge,
    #[error("network error")]
    Network,
}

/// The outbound-fetch seam (mirrors the `MtaStsFetcher` / `TlsrptPolicyFetcher`
/// `dyn` seams): production wires [`SsrfSafeFetcher`]; tests inject a fake; the
/// `test-hooks` build can override it so the e2e success path resolves a served
/// fixture without loosening production SSRF.
#[async_trait]
pub trait LinkPreviewFetcher: Send + Sync {
    /// Fetch `url` (http(s) only, SSRF-guarded, capped redirects), reading at
    /// most `max_bytes` of the body. Implementors MUST refuse private /
    /// link-local / loopback / metadata targets and re-validate every redirect.
    async fn fetch(&self, url: &str, max_bytes: usize) -> Result<FetchedResource, FetchError>;
}

/// Production fetcher: per-hop SSRF resolve + DNS-pinned reqwest client, manual
/// capped redirect following, per-hop timeout, capped streaming body read.
pub struct SsrfSafeFetcher {
    pub hop_timeout: Duration,
    pub max_redirects: usize,
}

impl Default for SsrfSafeFetcher {
    fn default() -> Self {
        Self {
            hop_timeout: DEFAULT_HOP_TIMEOUT,
            max_redirects: DEFAULT_MAX_REDIRECTS,
        }
    }
}

#[async_trait]
impl LinkPreviewFetcher for SsrfSafeFetcher {
    async fn fetch(&self, url: &str, max_bytes: usize) -> Result<FetchedResource, FetchError> {
        let mut current =
            url::Url::parse(url).map_err(|_| FetchError::Ssrf(SsrfError::InvalidUrl))?;

        // `0..=max_redirects` ⇒ one initial fetch + up to `max_redirects` hops.
        for _ in 0..=self.max_redirects {
            let scheme = current.scheme();
            if scheme != "http" && scheme != "https" {
                return Err(FetchError::BadScheme);
            }
            let host = current
                .host_str()
                .ok_or(FetchError::Ssrf(SsrfError::NoHost))?;
            let port = current
                .port_or_known_default()
                .unwrap_or(if scheme == "https" { 443 } else { 80 });

            // SSRF: resolve + reject any non-global address; pin the verified
            // addresses so the connection can't re-resolve to an internal host.
            let addrs = ssrf::resolve_global_addrs(host, port).await?;
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(self.hop_timeout)
                .resolve_to_addrs(host, &addrs)
                .build()
                .map_err(|_| FetchError::Network)?;

            let resp = client
                .get(current.clone())
                .send()
                .await
                .map_err(|_| FetchError::Network)?;
            let status = resp.status();

            if status.is_redirection() {
                let location = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or(FetchError::BadRedirect)?;
                // Resolve relative redirects against the current URL, then loop —
                // the next iteration re-runs the full SSRF check on the target.
                current = current
                    .join(location)
                    .map_err(|_| FetchError::BadRedirect)?;
                continue;
            }
            if !status.is_success() {
                return Err(FetchError::Status(status.as_u16()));
            }

            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let body = ssrf::read_capped(resp, max_bytes)
                .await
                .map_err(|e| match e {
                    ssrf::CappedReadError::TooLarge => FetchError::TooLarge,
                    ssrf::CappedReadError::Network => FetchError::Network,
                })?;
            return Ok(FetchedResource {
                url: current,
                content_type,
                body: Bytes::from(body),
            });
        }
        Err(FetchError::TooManyRedirects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Rejection paths need no network — the SSRF resolve / scheme check fail
    // before any connection. The success / redirect / oversize paths run against
    // a served fixture (tier_3) since every test-local address is non-global and
    // is correctly refused here.

    #[tokio::test]
    async fn rejects_non_http_scheme() {
        let f = SsrfSafeFetcher::default();
        assert_eq!(
            f.fetch("ftp://example.com/x", 1024).await.unwrap_err(),
            FetchError::BadScheme
        );
        assert_eq!(
            f.fetch("file:///etc/passwd", 1024).await.unwrap_err(),
            FetchError::BadScheme
        );
    }

    #[tokio::test]
    async fn rejects_loopback_and_metadata_literals() {
        let f = SsrfSafeFetcher::default();
        assert_eq!(
            f.fetch("http://127.0.0.1:8444/admin", 1024)
                .await
                .unwrap_err(),
            FetchError::Ssrf(SsrfError::NonGlobal)
        );
        assert_eq!(
            f.fetch("http://169.254.169.254/latest/meta-data/", 1024)
                .await
                .unwrap_err(),
            FetchError::Ssrf(SsrfError::NonGlobal)
        );
        // Decimal-encoded 127.0.0.1.
        assert_eq!(
            f.fetch("http://2130706433/", 1024).await.unwrap_err(),
            FetchError::Ssrf(SsrfError::NonGlobal)
        );
    }

    #[tokio::test]
    async fn rejects_private_rfc1918_literal() {
        let f = SsrfSafeFetcher::default();
        assert_eq!(
            f.fetch("https://10.0.0.5/", 1024).await.unwrap_err(),
            FetchError::Ssrf(SsrfError::NonGlobal)
        );
        assert_eq!(
            f.fetch("https://192.168.1.1/", 1024).await.unwrap_err(),
            FetchError::Ssrf(SsrfError::NonGlobal)
        );
    }

    #[tokio::test]
    async fn rejects_malformed_url() {
        let f = SsrfSafeFetcher::default();
        assert_eq!(
            f.fetch("not a url", 1024).await.unwrap_err(),
            FetchError::Ssrf(SsrfError::InvalidUrl)
        );
    }
}
