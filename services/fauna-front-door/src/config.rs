//! Door configuration. Per the one-configuration-surface invariant
//! (`docs/goal/principles.md`), nothing here is a human preference: every
//! value is a hard-coded constant or internal wiring the deployment artifact
//! sets (the systemd unit's environment) / the binary auto-detects. There is
//! no config file.

use std::net::SocketAddr;
use std::path::PathBuf;

/// The apex the door serves (also the certificate's primary name).
pub const APEX: &str = "fauna.social";
/// Redirect-only host: 308 to the same path on the apex.
pub const WWW: &str = "www.fauna.social";
/// Canonical hosted-SPA origin — baked into shipped code as the default
/// CORS origin; never rename (front-door.md § Public-vhost topology).
pub const APP: &str = "app.fauna.social";
/// The CORS proxy's public name; passes to the loopback unit.
pub const PROXY: &str = "proxy.fauna.social";

/// Where deploys land the site content (`current` is the atomic symlink the
/// rsync deploy flips — front-door.md § The box).
pub const SITE_ROOT: &str = "/srv/fauna/site/current";
/// Where deploys land the SPA build.
pub const APP_ROOT: &str = "/srv/fauna/app/current";
/// The loopback CORS-proxy upstream. Port is internal wiring shared with
/// `deploy/fauna-cors-proxy.service` (which sets the unit's `PORT` to match).
pub const PROXY_UPSTREAM: &str = "http://127.0.0.1:8402";
/// ACME contact for the door's Let's Encrypt account.
pub const CONTACT_EMAIL: &str = "hostmaster@fauna.social";

#[derive(Debug, Clone)]
pub struct DoorConfig {
    pub apex: String,
    pub www: String,
    pub app: String,
    pub proxy: String,
    pub site_root: PathBuf,
    pub app_root: PathBuf,
    pub proxy_upstream: String,
    /// ACME account, certificates, and the retry budget live here
    /// (`StateDirectory=` in the unit).
    pub state_dir: PathBuf,
    pub http_bind: SocketAddr,
    pub https_bind: SocketAddr,
    pub contact_email: String,
    /// Explicit ACME directory override (`FAUNA_ACME_DIRECTORY_URL`, the same
    /// artifact-set knob the nest honors — tls-certificates.md § C). Wins
    /// over `acme_staging`.
    pub acme_directory_url: Option<String>,
    /// `FAUNA_ACME_STAGING=1` orders against Let's Encrypt staging.
    pub acme_staging: bool,
}

impl DoorConfig {
    /// Production values: constants above + the artifact-set environment
    /// (systemd `StateDirectory` exports `STATE_DIRECTORY`).
    pub fn production() -> Self {
        let state_dir = std::env::var_os("STATE_DIRECTORY")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/var/lib/fauna-front-door"));
        Self {
            apex: APEX.into(),
            www: WWW.into(),
            app: APP.into(),
            proxy: PROXY.into(),
            site_root: SITE_ROOT.into(),
            app_root: APP_ROOT.into(),
            proxy_upstream: PROXY_UPSTREAM.into(),
            state_dir,
            http_bind: ([0, 0, 0, 0], 80).into(),
            https_bind: ([0, 0, 0, 0], 443).into(),
            contact_email: CONTACT_EMAIL.into(),
            acme_directory_url: std::env::var("FAUNA_ACME_DIRECTORY_URL").ok(),
            acme_staging: std::env::var("FAUNA_ACME_STAGING").is_ok_and(|v| v == "1"),
        }
    }

    /// The certificate SAN set, apex first (it becomes the primary name).
    pub fn sans(&self) -> Vec<String> {
        vec![
            self.apex.clone(),
            self.www.clone(),
            self.app.clone(),
            self.proxy.clone(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sans_lead_with_the_apex() {
        let cfg = DoorConfig::production();
        let sans = cfg.sans();
        assert_eq!(sans[0], "fauna.social");
        assert_eq!(sans.len(), 4);
        assert!(sans.contains(&"proxy.fauna.social".to_string()));
    }
}
