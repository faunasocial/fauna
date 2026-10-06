//! TOML configuration for fauna-router — **wiring only**.
//!
//! Where the backends are, what to bind, timeouts: the things whatever stood
//! the deployment up already knows. No *policy* lives here, because policy is
//! never artifact-set — a nest's registration posture, capacity and handle
//! domain are client-set nest state the router learns from every
//! `/internal/router-status` poll, and the reserved-handle list is a hard-coded
//! shared constant (`fauna_protocol::handle::RESERVED_HANDLES`).
//!
//! A whole `[registration]` section used to live here and every key of it was
//! config theatre: the posture trio (`open`/`invite_required`/`max_free_users`)
//! was a second authority that could only disagree with the nest; a
//! `rate_limit_per_hour` had **no reader at all** — a knob that silently did
//! nothing (the then-module-wide `#![allow(dead_code)]` for "schema parity" is
//! what let it sit unnoticed; that allow is gone — **a key earns its keep by
//! having a reader**); `reserved_handles` shadowed what is a correctness
//! constant; and `handle_domain` shadowed what the backends report on the poll.
//! All deleted (the section fully retired 2026-07-17); stale configs still
//! parse, their keys inert — see `a_stale_configs_registration_keys_are_inert`.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ProxyConfig {
    pub proxy: ProxySection,
    #[serde(default)]
    pub backend: Vec<BackendConfig>,
    // `[acme]` was REMOVED 2026-10-01 (the 2026-10-01 removal of dead shapes): the
    // binary serves plain HTTP behind external TLS termination and never read
    // it — the same fate as the `[wireguard]` section, deleted 2026-08-23. A
    // stale file still carrying it parses, inert.
}

#[derive(Debug, Deserialize, Clone)]
pub struct ProxySection {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_db_path")]
    pub db_path: String,
    pub domain: String,
    #[serde(default = "default_health_interval")]
    pub health_check_interval_secs: u64,
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
}

/// Where a backend nest is and how to reach it — pure artifact wiring, set by
/// whatever stood the deployment up.
///
/// Deliberately carries **no capacity or policy**: a nest's user ceiling is
/// derived from its admin's client-set storage cap and reported on every
/// `/internal/router-status` poll (`crate::backends::RouterStatus`). A
/// `max_users` here would be a hand-edited second opinion the nest never
/// agreed to.
#[derive(Debug, Deserialize, Clone)]
pub struct BackendConfig {
    pub name: String,
    pub nest_id: String,
    pub tunnel_ip: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub labels: Vec<String>,
}

fn default_listen() -> String {
    "0.0.0.0:443".into()
}
fn default_db_path() -> String {
    "proxy.db".into()
}
fn default_health_interval() -> u64 {
    30
}
fn default_request_timeout() -> u64 {
    30
}
fn default_port() -> u16 {
    3000
}

impl ProxyConfig {
    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read config {path}: {e}"))?;
        toml::from_str(&contents).map_err(|e| anyhow::anyhow!("failed to parse config {path}: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let toml = r#"
[proxy]
domain = "fauna.social"
"#;
        let cfg: ProxyConfig = toml::from_str(toml).expect("parse failed");
        assert_eq!(cfg.proxy.domain, "fauna.social");
        assert_eq!(cfg.proxy.listen, "0.0.0.0:443");
        assert_eq!(cfg.proxy.db_path, "proxy.db");
        assert_eq!(cfg.proxy.health_check_interval_secs, 30);
        assert_eq!(cfg.proxy.request_timeout_secs, 30);
        assert!(cfg.backend.is_empty());
    }

    #[test]
    fn parse_full_config() {
        let toml = r#"
[proxy]
domain = "fauna.social"
listen = "0.0.0.0:8443"
db_path = "/var/lib/fauna-router/proxy.db"
health_check_interval_secs = 60
request_timeout_secs = 15

[[backend]]
name = "nest-a"
nest_id = "abc123"
tunnel_ip = "10.0.0.1"
port = 3001

[[backend]]
name = "nest-b"
nest_id = "def456"
tunnel_ip = "10.0.0.2"

# The retired `[acme]` section: still parses, inert.
[acme]
enabled = true
email = "admin@fauna.social"
staging = false
"#;
        let cfg: ProxyConfig = toml::from_str(toml).expect("parse failed");
        assert_eq!(cfg.proxy.domain, "fauna.social");
        assert_eq!(cfg.proxy.listen, "0.0.0.0:8443");
        assert_eq!(cfg.proxy.health_check_interval_secs, 60);
        assert_eq!(cfg.backend.len(), 2);
        assert_eq!(cfg.backend[0].name, "nest-a");
        assert_eq!(cfg.backend[0].port, 3001);
        assert_eq!(cfg.backend[1].name, "nest-b");
        assert_eq!(cfg.backend[1].port, 3000); // default
    }

    /// A config file left over from when a `[registration]` section existed
    /// still boots — and grants nothing. The whole section is inert: there is
    /// no field left for any of its keys to land in, so no edit to this file
    /// can change the posture, the reserved list, or the reported handle
    /// domain. The nest decides (client-set state via the poll); the reserved
    /// list is the shared constant.
    #[test]
    fn a_stale_configs_registration_keys_are_inert() {
        let toml = r#"
[proxy]
domain = "fauna.social"

[[backend]]
name = "nest-a"
nest_id = "abc123"
tunnel_ip = "10.0.0.1"
max_users = 9999

[registration]
open = true
invite_required = true
max_free_users = 100
handle_domain = "operator.example"
reserved_handles = ["admin-pick"]
rate_limit_per_hour = 5
"#;
        let cfg: ProxyConfig = toml::from_str(toml).expect("a stale config must still boot");
        assert_eq!(cfg.backend.len(), 1);
        // Neither a `registration` field nor any key inside it exists to
        // deserialize into; capacity, posture and handle domain come from the
        // nest's poll report, and the reserved list is
        // `fauna_protocol::handle::RESERVED_HANDLES`.
    }
}
