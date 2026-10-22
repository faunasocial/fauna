//! The production MTA-STS policy GET (RFC 8461 §3.3) behind
//! `fauna_mail::outbound::mta_sts::LiveMtaStsFetcher`.
//!
//! The URL is `https://mta-sts.<recipient domain>/.well-known/mta-sts.txt`, and
//! the recipient domain — any domain this nest delivers mail to — sets where
//! `mta-sts.<domain>` resolves: a remote-chosen target, so the GET goes through
//! the nest's SSRF guard and its body through a cap
//! (`deployment-threat-model.md` § Concern 1, 1a). `fauna-mail` cannot reach
//! [`crate::ssrf`], so it takes this through its `MtaStsPolicyHttp` seam, as the
//! TLS-RPT report POST takes [`crate::outbound_tlsrpt::GuardedTlsrptPoster`].
//!
//! docs/goal/behavior/smtp-server.md § MX resolution (item 3).

use fauna_mail::outbound::mta_sts::{
    MTA_STS_FETCH_TIMEOUT, MTA_STS_POLICY_MAX_BYTES, MtaStsPolicyHttp,
};

/// The production [`MtaStsPolicyHttp`]: [`crate::ssrf::ssrf_safe_https_client`]
/// (https only, every resolved address global and pinned, no redirect followed),
/// no `User-Agent`, and the body read under [`MTA_STS_POLICY_MAX_BYTES`]. A
/// refused target is an `Err`, which the fetcher records as a policy fetch
/// error — delivery proceeds as if no policy (RFC 8461 §5).
pub struct GuardedMtaStsPolicyHttp;

#[async_trait::async_trait]
impl MtaStsPolicyHttp for GuardedMtaStsPolicyHttp {
    async fn get_policy(&self, url: &str) -> anyhow::Result<String> {
        let (client, url) = crate::ssrf::ssrf_safe_https_client(url, MTA_STS_FETCH_TIMEOUT, None)
            .await
            .map_err(|e| anyhow::anyhow!("MTA-STS policy target refused ({e}): {url}"))?;
        read_policy_body(client.get(url).send().await?).await
    }
}

/// A policy response's body: 2xx only, read under [`MTA_STS_POLICY_MAX_BYTES`].
async fn read_policy_body(resp: reqwest::Response) -> anyhow::Result<String> {
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("MTA-STS policy GET answered {status}");
    }
    let body = crate::ssrf::read_capped(resp, MTA_STS_POLICY_MAX_BYTES).await?;
    // A non-UTF-8 byte cannot be part of a valid policy; keep it for the parser
    // to refuse (`sts-policy-invalid`) rather than call it a fetch error.
    Ok(String::from_utf8_lossy(&body).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssrf::test_server::{
        get, head_with_length, head_without_length, serve_once, within,
    };

    /// A recipient domain cannot aim the policy GET at this box's loopback, a
    /// private address or the cloud metadata service: refused before any dial.
    #[tokio::test]
    async fn guarded_get_refuses_loopback_private_and_imds() {
        for url in [
            "https://127.0.0.1/.well-known/mta-sts.txt",
            "https://169.254.169.254/.well-known/mta-sts.txt",
            "https://10.0.0.7/.well-known/mta-sts.txt",
            "https://mta-sts.localhost/.well-known/mta-sts.txt",
            "http://8.8.8.8/.well-known/mta-sts.txt",
        ] {
            let err = GuardedMtaStsPolicyHttp
                .get_policy(url)
                .await
                .expect_err("a non-global policy target is refused");
            assert!(err.to_string().contains("refused"), "{url}: {err}");
        }
    }

    #[tokio::test]
    async fn policy_body_is_read_up_to_the_cap() {
        let body = b"version: STSv1\nmode: testing\n".to_vec();
        let url = serve_once(head_without_length("200 OK"), body.clone(), false).await;
        assert_eq!(
            read_policy_body(get(&url).await).await.unwrap().as_bytes(),
            body
        );
    }

    #[tokio::test]
    async fn policy_body_over_the_cap_is_refused() {
        let url = serve_once(
            head_with_length("200 OK", MTA_STS_POLICY_MAX_BYTES + 1),
            vec![b'x'; 8],
            false,
        )
        .await;
        assert!(read_policy_body(get(&url).await).await.is_err());
        let url = serve_once(head_without_length("200 OK"), vec![b'x'; 8192], true).await;
        assert!(within(read_policy_body(get(&url).await)).await.is_err());
    }

    #[tokio::test]
    async fn non_2xx_policy_answer_is_an_error() {
        let url = serve_once(
            head_without_length("404 Not Found"),
            b"nope".to_vec(),
            false,
        )
        .await;
        assert!(read_policy_body(get(&url).await).await.is_err());
    }
}
