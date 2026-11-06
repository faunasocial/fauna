//! The dial guard of the **consume-side** atproto HTTP client
//! ([`crate::oauth::ReqwestHttpClient`]) — `docs/goal/architecture/nest/network-exposure.md`
//! § Rulings F9.
//!
//! Every host that client dials is named by something a local user chose: the
//! handle they typed (`/.well-known/atproto-did` on the handle's host), the DID
//! it resolves to (a `did:web` host, or the PLC directory's answer), the PDS
//! and authorization server that DID document names, and every XRPC call the
//! DM, feed and notification workers make to that PDS. Unguarded, a user who
//! links an account whose DID document names an internal address — or a public
//! host that redirects to one — makes the nest dial its own network and hands
//! the reply back to that user's Bluesky surfaces.
//!
//! The rule is the nest's own SSRF guard's (`bins/fauna-nest/src/ssrf.rs`,
//! F1), applied to a long-lived client rather than a per-request one:
//!
//! * **https only**, and an address-literal host must be globally routable, and
//!   never a `localhost` name ([`dial_refusal`] — what the URL text alone
//!   decides);
//! * **every address a name resolves to must be global**, and the connection is
//!   made to exactly the addresses verified ([`GlobalOnlyResolver`], through
//!   the shared [`fauna_core::resolve::resolve_permitted_addrs`] under
//!   [`fauna_core::resolve::is_global_ip`] — the resolver's answer *is* the
//!   dial target, so nothing re-resolves past the check);
//! * **every redirect hop is judged under the same rule** ([`redirect_policy`])
//!   before it is followed; its name is then resolved by the same resolver.
//!
//! Redirects are followed (re-checked) rather than refused, unlike the nest's
//! per-request guard: a handle's `/.well-known/atproto-did` legitimately sits
//! behind an apex-to-`www` or a hosting provider's redirect, and the hop is
//! held to the first dial's rule.

use std::net::IpAddr;
use std::sync::Arc;

use fauna_core::resolve::{is_global_ip, resolve_permitted_addrs};

/// The text every refusal carries, so a caller (and a test) can tell a guard
/// refusal from a far end that failed on its own.
pub const DIAL_REFUSED: &str = "atproto dial refused";

/// The most redirect hops one request follows — reqwest's own default.
pub const MAX_REDIRECTS: usize = 10;

/// Which targets the client may dial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialPolicy {
    /// Production: https to globally-routable addresses only.
    PublicOnly,
    /// Test seam: [`Self::PublicOnly`], plus a loopback target over `http` or
    /// `https` — the in-process far ends of this crate's tests and the e2e
    /// harness's fake atproto far end the origin override reaches. Compiled
    /// only for tests and under `test-helpers`, which no production build
    /// enables.
    #[cfg(any(test, feature = "test-helpers"))]
    PublicOrLoopback,
}

impl DialPolicy {
    fn admits_loopback(self) -> bool {
        match self {
            DialPolicy::PublicOnly => false,
            #[cfg(any(test, feature = "test-helpers"))]
            DialPolicy::PublicOrLoopback => true,
        }
    }

    /// Whether the policy admits a connection to `ip`.
    pub fn permits(self, ip: IpAddr) -> bool {
        is_global_ip(ip) || (self.admits_loopback() && ip.is_loopback())
    }
}

/// Why `url` may not be dialed under `policy`, decided from its text alone, or
/// `None` when the text admits it (a name's addresses are then judged by
/// [`GlobalOnlyResolver`] at the dial).
pub fn dial_refusal(url: &url::Url, policy: DialPolicy) -> Option<String> {
    let host = url.host()?;
    let loopback_host = match &host {
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
        url::Host::Domain(d) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost")
        }
    };
    let loopback_admitted = loopback_host && policy.admits_loopback();
    match url.scheme() {
        "https" => {}
        "http" if loopback_admitted => {}
        other => return Some(format!("{DIAL_REFUSED}: scheme {other} is not https")),
    }
    let permitted = match host {
        url::Host::Ipv4(ip) => policy.permits(IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => policy.permits(IpAddr::V6(ip)),
        url::Host::Domain(_) => !loopback_host || loopback_admitted,
    };
    if permitted {
        None
    } else {
        Some(format!("{DIAL_REFUSED}: {host} is not globally routable"))
    }
}

/// The client's resolver: a name resolves only when **every** address it
/// answers is permitted, and the verified addresses are what the connection is
/// made to (the pin — the connector dials the resolver's answer and nothing
/// else). An address-literal host never reaches a resolver; [`dial_refusal`]
/// judges it before the request and at every redirect.
pub struct GlobalOnlyResolver {
    policy: DialPolicy,
}

impl GlobalOnlyResolver {
    pub fn new(policy: DialPolicy) -> Arc<Self> {
        Arc::new(Self { policy })
    }
}

impl reqwest::dns::Resolve for GlobalOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let policy = self.policy;
        Box::pin(async move {
            let host = name.as_str().to_string();
            // Port 0: the connector puts the URL's own port on each address.
            match resolve_permitted_addrs(&host, 0, |ip| policy.permits(ip)).await {
                Ok(addrs) => Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs),
                Err(e) => Err(Box::new(std::io::Error::other(format!(
                    "{DIAL_REFUSED}: {host}: {e}"
                )))
                    as Box<dyn std::error::Error + Send + Sync>),
            }
        })
    }
}

/// The redirect policy: at most [`MAX_REDIRECTS`] hops, each judged by
/// [`dial_refusal`] before it is followed.
pub fn redirect_policy(policy: DialPolicy) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error(format!("{DIAL_REFUSED}: too many redirects"));
        }
        match dial_refusal(attempt.url(), policy) {
            Some(reason) => attempt.error(format!("{reason} (redirect)")),
            None => attempt.follow(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(url: &str, policy: DialPolicy) -> bool {
        dial_refusal(&url::Url::parse(url).unwrap(), policy).is_some()
    }

    #[test]
    fn the_production_policy_admits_only_https_to_public_targets() {
        for ok in [
            "https://bsky.social/xrpc/x",
            "https://plc.directory/did:plc:abc",
            "https://93.184.216.34/",
            "https://[2606:2800:220:1:248:1893:25c8:1946]/",
        ] {
            assert!(!refused(ok, DialPolicy::PublicOnly), "{ok}");
        }
        for bad in [
            "http://bsky.social/",
            "ftp://bsky.social/",
            "https://127.0.0.1/",
            "https://[::1]/",
            "https://localhost/",
            "https://pds.localhost/",
            "https://LOCALHOST./",
            "https://10.0.0.5/",
            "https://172.16.0.1/",
            "https://192.168.1.1/",
            "https://169.254.169.254/",
            "https://100.64.0.1/",
            "https://[fd00::1]/",
            "https://[fe80::1]/",
            "https://[::ffff:127.0.0.1]/",
        ] {
            assert!(refused(bad, DialPolicy::PublicOnly), "{bad}");
        }
    }

    #[test]
    fn the_test_policy_adds_loopback_and_nothing_else() {
        for ok in [
            "http://127.0.0.1:4711/",
            "http://localhost:4711/",
            "http://[::1]:4711/",
        ] {
            assert!(!refused(ok, DialPolicy::PublicOrLoopback), "{ok}");
        }
        for bad in [
            "http://bsky.social/",
            "https://10.0.0.5/",
            "https://169.254.169.254/",
            "http://192.168.1.1/",
        ] {
            assert!(refused(bad, DialPolicy::PublicOrLoopback), "{bad}");
        }
    }

    /// The resolver refuses a name that answers an internal address.
    #[tokio::test]
    async fn the_resolver_refuses_a_name_resolving_inward() {
        use reqwest::dns::Resolve;
        let resolver = GlobalOnlyResolver::new(DialPolicy::PublicOnly);
        let name: reqwest::dns::Name = "localhost".parse().unwrap();
        let err = match resolver.resolve(name).await {
            Ok(_) => panic!("localhost resolves to loopback and is refused"),
            Err(e) => e,
        };
        assert!(err.to_string().contains(DIAL_REFUSED), "{err}");
    }
}
