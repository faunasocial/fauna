//! CORS-proxy aware API base URL computation.
//!
//! Browser-context (wasm32) builds of the provisioning crate cannot reach
//! a provider's API directly when the provider doesn't serve permissive
//! CORS headers. The wizard routes such calls through the stateless
//! `services/fauna-cors-proxy` (deployed at `proxy.fauna.social` by
//! default; overridable via `FAUNA_PROXY_URL` at compile time).
//!
//! Native builds always call providers directly — no proxy involved.
//!
//! Per-provider impls call `default_api_base(direct_url, proxy_prefix)`
//! from their `new()` constructor. The `proxy_prefix` is the path the
//! cors proxy prepends to forward to the provider (e.g. `"gandi/v5"`
//! for Gandi's v5 API). See `services/fauna-cors-proxy/README.md` for
//! the prefix convention.
//!
//! `with_base_url(...)` constructors bypass this entirely so wiremock
//! tests can point adapters at arbitrary mock servers.

/// Build target environment for proxy decisions.
///
/// Distinguishes "browser/WASM" from "native" without leaking
/// `cfg(target_arch = "wasm32")` checks throughout the codebase, and
/// keeps both branches testable on a native test runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildEnv {
    /// Native build — direct API calls work.
    Native,
    /// Browser/WASM build — provider calls subject to CORS, must be
    /// proxied for `cors_policy: Proxy` providers.
    Web,
}

/// Default proxy root URL. Set at compile time via `FAUNA_PROXY_URL`;
/// falls back to `https://proxy.fauna.social` if unset.
pub const DEFAULT_PROXY_ROOT: &str = match option_env!("FAUNA_PROXY_URL") {
    Some(s) => s,
    None => "https://proxy.fauna.social",
};

/// Returns the build env the current binary was compiled for.
pub fn current_build_env() -> BuildEnv {
    if cfg!(target_arch = "wasm32") {
        BuildEnv::Web
    } else {
        BuildEnv::Native
    }
}

/// Pure function: pick the right base URL for a given env + proxy root.
/// Exposed for tests; production callers use `default_api_base`.
pub fn compute_api_base(
    direct_url: &str,
    proxy_prefix: &str,
    env: BuildEnv,
    proxy_root: &str,
) -> String {
    match env {
        BuildEnv::Native => direct_url.to_string(),
        BuildEnv::Web => {
            let root = proxy_root.trim_end_matches('/');
            let prefix = proxy_prefix.trim_start_matches('/');
            format!("{root}/{prefix}")
        }
    }
}

/// Compose the api_base a provider impl should use for its default
/// `new()` constructor: direct URL on native, proxied URL on web.
pub fn default_api_base(direct_url: &str, proxy_prefix: &str) -> String {
    default_api_base_for_env(direct_url, proxy_prefix, current_build_env())
}

/// [`default_api_base`] with the build env supplied explicitly.
///
/// Exists so a **native** test runner can assert what a `cors_policy: proxy`
/// adapter would do in a browser. Without this seam `current_build_env()` is
/// always `Native` under `cargo test`, so an adapter that ignores the proxy
/// entirely is indistinguishable from one that honours it — which is exactly
/// how cloudflare/namecheap/vultr shipped hard-coded direct bases while the
/// registry declared them proxied. Every `cors_policy: proxy` adapter is
/// pinned against this by `tests/cors_policy_bijection.rs`.
pub fn default_api_base_for_env(direct_url: &str, proxy_prefix: &str, env: BuildEnv) -> String {
    compute_api_base(direct_url, proxy_prefix, env, DEFAULT_PROXY_ROOT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_env_returns_direct_url_unchanged() {
        let result = compute_api_base(
            "https://api.gandi.net/v5",
            "gandi/v5",
            BuildEnv::Native,
            "https://proxy.fauna.social",
        );
        assert_eq!(result, "https://api.gandi.net/v5");
    }

    #[test]
    fn web_env_routes_through_proxy_root_with_prefix() {
        let result = compute_api_base(
            "https://api.gandi.net/v5",
            "gandi/v5",
            BuildEnv::Web,
            "https://proxy.fauna.social",
        );
        assert_eq!(result, "https://proxy.fauna.social/gandi/v5");
    }

    #[test]
    fn web_env_handles_trailing_slash_on_proxy_root() {
        let result = compute_api_base(
            "https://api.gandi.net/v5",
            "gandi/v5",
            BuildEnv::Web,
            "https://proxy.fauna.social/",
        );
        assert_eq!(result, "https://proxy.fauna.social/gandi/v5");
    }

    #[test]
    fn web_env_handles_leading_slash_on_prefix() {
        let result = compute_api_base(
            "https://api.gandi.net/v5",
            "/gandi/v5",
            BuildEnv::Web,
            "https://proxy.fauna.social",
        );
        assert_eq!(result, "https://proxy.fauna.social/gandi/v5");
    }

    #[test]
    fn web_env_supports_self_hosted_proxy_via_override() {
        // Self-hosters set FAUNA_PROXY_URL to their own host. The function
        // doesn't care about the scheme/host as long as it's a valid root.
        let result = compute_api_base(
            "https://api.gandi.net/v5",
            "gandi/v5",
            BuildEnv::Web,
            "http://localhost:8080",
        );
        assert_eq!(result, "http://localhost:8080/gandi/v5");
    }
}
