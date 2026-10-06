//! What the embedder provides behind the plugin's imports — the nest's side
//! of every capability, as one object-safe trait — and the pure outbound-host
//! policy the host applies before any dial.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;

/// A boxed, sendable future — the return shape every async service method
/// takes, so the trait stays object-safe.
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A WS-RPC refusal or fault as the nest answers it (the WIT `rpc-error`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcRefusal {
    pub code: String,
    pub message: String,
}

/// One outbound HTTP request the plugin asked for (the WIT `http.request`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// The reply the embedder fetched (the WIT `http.response`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// The WIT `log.level`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// The embedder's half of the plugin's imports. One implementation per
/// hosted principal: every method already knows which principal it serves.
pub trait HostServices: Send + Sync + 'static {
    /// `nest-api.call`: dispatch `kind` as this principal, for `account`
    /// (a bound account) or the install row (`None`). The embedder runs the
    /// principal chokepoint; this crate never decides reach.
    fn nest_call(
        &self,
        account: Option<[u8; 32]>,
        kind: String,
        payload: Vec<u8>,
    ) -> BoxFut<'_, Result<Vec<u8>, RpcRefusal>>;

    /// `nest-api.bindings`: the accounts bound to this plugin.
    fn bindings(&self) -> BoxFut<'_, Vec<[u8; 32]>>;

    /// `state.get`.
    fn state_get(&self, key: String) -> BoxFut<'_, anyhow::Result<Option<Vec<u8>>>>;
    /// `state.put`.
    fn state_put(&self, key: String, value: Vec<u8>) -> BoxFut<'_, anyhow::Result<()>>;
    /// `state.delete`.
    fn state_delete(&self, key: String) -> BoxFut<'_, anyhow::Result<()>>;

    /// `holder.public-key` / `holder.open-grant` read this key. Returned by
    /// reference so the secret is never copied into a lowered value.
    fn holder_key(&self) -> &crate::HolderKey;

    /// `http.fetch`, called ONLY after [`OutboundPolicy`] admitted the URL's
    /// host. The embedder dials through its own guarded fetcher.
    fn http_fetch(&self, req: HttpRequest) -> BoxFut<'_, Result<HttpResponse, String>>;

    /// `clock.now-millis`.
    fn now_millis(&self) -> u64;

    /// `log.log`, attributed to the principal by the embedder.
    fn log(&self, level: LogLevel, message: String);
}

/// The hosts a plugin's document declared (`third-party.md` § The manifest,
/// the `execution.hosts` member) — the whole outbound surface. Host equality,
/// lowercase, no port: the same rule the kind grammar applies to a publisher.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutboundPolicy {
    hosts: BTreeSet<String>,
}

impl OutboundPolicy {
    /// A policy over exactly `hosts` (each lowercased).
    pub fn new<I, S>(hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            hosts: hosts
                .into_iter()
                .map(|h| h.as_ref().trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty())
                .collect(),
        }
    }

    /// No host at all — a plugin that declared none reaches nothing.
    pub fn none() -> Self {
        Self::default()
    }

    /// The declared hosts, sorted.
    pub fn hosts(&self) -> impl Iterator<Item = &str> {
        self.hosts.iter().map(String::as_str)
    }

    /// Is `url` an `https` URL whose host is declared? Refuses any other
    /// scheme (a plugin never speaks plaintext to the internet), a
    /// userinfo-bearing URL, an IP literal, and a host not in the set.
    pub fn allows(&self, url: &str) -> Result<(), String> {
        let rest = url
            .strip_prefix("https://")
            .ok_or_else(|| "only https URLs are reachable".to_string())?;
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        if authority.contains('@') {
            return Err("a URL with userinfo is refused".into());
        }
        let host = authority
            .rsplit_once(':')
            .filter(|(_, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
            .map_or(authority, |(h, _)| h);
        let host = host.to_ascii_lowercase();
        if host.is_empty() || host.starts_with('[') || host.parse::<std::net::Ipv4Addr>().is_ok() {
            return Err("an IP-literal or empty host is refused".into());
        }
        if self.hosts.contains(&host) {
            Ok(())
        } else {
            Err(format!(
                "host {host:?} is not declared by the plugin's document"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_declared_https_hosts_pass() {
        let p = OutboundPolicy::new(["API.Example.com", " bridge.example "]);
        assert_eq!(p.allows("https://api.example.com/v1?x=1"), Ok(()));
        assert_eq!(p.allows("https://API.example.com:8443/"), Ok(()));
        assert_eq!(p.allows("https://bridge.example"), Ok(()));
        assert!(p.allows("http://api.example.com/").is_err());
        assert!(p.allows("https://other.example/").is_err());
        assert!(p.allows("https://user@api.example.com/").is_err());
        assert!(p.allows("https://10.0.0.1/").is_err());
        assert!(p.allows("https://[::1]/").is_err());
        assert!(
            OutboundPolicy::none()
                .allows("https://api.example.com/")
                .is_err()
        );
    }
}
