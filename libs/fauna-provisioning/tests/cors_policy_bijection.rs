//! `cors_policy: proxy` is a promise the adapters have to keep.
//!
//! `i18n/providers.yaml` declares, per provider, whether a browser can reach
//! its API directly (`open`) or has to go through `services/fauna-cors-proxy`
//! (`proxy`). Nothing used to *enforce* the `proxy` half: the declaration lived
//! in the registry, and honouring it was a per-adapter convention — call
//! `proxy::default_api_base(DIRECT_API, PROXY_PREFIX)` from `new()` instead of
//! hard-coding `API_BASE`.
//!
//! Three of the four `proxy` providers silently didn't (cloudflare DNS,
//! namecheap DNS, vultr VPS — only gandi did), so the web wizard's default
//! constructors pointed straight at provider APIs that answer browsers with a
//! CORS rejection. A symbol-existence check can't see this: every adapter has
//! an `api_base`, a `new()`, and a plausible constant. Only the end-to-end
//! assertion below — *the base a web build would actually use* — catches it.
//!
//! Two properties are pinned:
//!
//! 1. **Bijection, per (provider, capability).** Every capability of every
//!    `CorsPolicy::Proxy` provider in the generated registry has a row in
//!    `PROXY_ADAPTERS`, and every row is declared `proxy` in the registry. A
//!    new proxied provider, a policy flipped to `proxy`, *or a new capability
//!    on a provider already listed* fails here until its adapter is wired —
//!    the last of those is why the check is per-capability rather than per
//!    provider (Namecheap gained a registrar while its DNS row already sat in
//!    the table, which a provider-level check would have waved through).
//! 2. **Routing.** Each of those adapters, constructed for `BuildEnv::Web`,
//!    yields a proxy-rooted base — and, constructed for `BuildEnv::Native`,
//!    still calls the provider directly.
//!
//! Note this pins the *client* half only. `proxy.fauna.social` is not deployed
//! (`docs/goal/architecture/provisioning/registry.md` § Implementation status
//! today), so web provisioning through these providers stays degraded in
//! production until it is — this test keeps the code half honest meanwhile.

use fauna_provisioning::proxy::{BuildEnv, DEFAULT_PROXY_ROOT};
use fauna_provisioning::{Capability, CorsPolicy, PROVIDERS, ProviderId};

/// One row per adapter that must route through the CORS proxy on web.
///
/// `base_for` returns the base URL the adapter's *default* constructor
/// produces for the given build env — i.e. what production would use, not a
/// recomputation of the proxy formula. That distinction is the whole point:
/// re-deriving `compute_api_base` here would pass even for an adapter that
/// never calls it.
struct ProxyAdapter {
    provider: ProviderId,
    /// Kept for failure messages — names which capability's adapter is wired.
    what: &'static str,
    direct_host: &'static str,
    base_for: fn(BuildEnv) -> String,
}

const PROXY_ADAPTERS: &[ProxyAdapter] = &[
    ProxyAdapter {
        provider: ProviderId::Cloudflare,
        what: "dns",
        direct_host: "https://api.cloudflare.com",
        base_for: |env| {
            fauna_provisioning::dns::cloudflare::Cloudflare::new_for_env("t".into(), env)
                .api_base()
                .to_string()
        },
    },
    ProxyAdapter {
        provider: ProviderId::Namecheap,
        what: "dns",
        direct_host: "https://api.namecheap.com",
        base_for: |env| {
            fauna_provisioning::dns::namecheap::Namecheap::new_for_env("u".into(), "k".into(), env)
                .api_base()
                .to_string()
        },
    },
    ProxyAdapter {
        provider: ProviderId::Vultr,
        what: "vps",
        direct_host: "https://api.vultr.com",
        base_for: |env| {
            fauna_provisioning::vps::vultr::Vultr::new_for_env("t".into(), env)
                .api_base()
                .to_string()
        },
    },
    ProxyAdapter {
        provider: ProviderId::Gandi,
        what: "dns",
        direct_host: "https://api.gandi.net",
        base_for: |env| {
            fauna_provisioning::dns::gandi::Gandi::new_for_env("t".into(), env)
                .api_base()
                .to_string()
        },
    },
    ProxyAdapter {
        provider: ProviderId::Gandi,
        what: "registrar",
        direct_host: "https://api.gandi.net",
        base_for: |env| {
            fauna_provisioning::registrar::gandi::GandiRegistrar::new_for_env("t".into(), env)
                .api_base()
                .to_string()
        },
    },
    ProxyAdapter {
        provider: ProviderId::Namecheap,
        what: "registrar",
        direct_host: "https://api.namecheap.com",
        base_for: |env| {
            fauna_provisioning::registrar::namecheap::NamecheapRegistrar::new_for_env(
                "u".into(),
                "k".into(),
                env,
            )
            .api_base()
            .to_string()
        },
    },
    ProxyAdapter {
        provider: ProviderId::Cloudflare,
        what: "registrar",
        direct_host: "https://api.cloudflare.com",
        base_for: |env| {
            fauna_provisioning::registrar::cloudflare::CloudflareRegistrar::new_for_env(
                "t".into(),
                "acct".into(),
                env,
            )
            .api_base()
            .to_string()
        },
    },
];

/// The `what` label a `Capability` must be wired under in [`PROXY_ADAPTERS`].
fn adapter_label(capability: Capability) -> &'static str {
    match capability {
        Capability::Dns => "dns",
        Capability::Vps => "vps",
        Capability::Registrar => "registrar",
    }
}

#[test]
fn every_proxy_policy_capability_has_a_wired_adapter() {
    // Per **capability**, not per provider: a provider already in the table for
    // one capability would otherwise smuggle a second, unwired adapter past
    // this guard — which is exactly what happened when Namecheap gained its
    // registrar (its DNS row was already here).
    for p in PROVIDERS
        .iter()
        .filter(|p| p.cors_policy == CorsPolicy::Proxy)
    {
        for capability in p.capabilities {
            let what = adapter_label(*capability);
            assert!(
                PROXY_ADAPTERS
                    .iter()
                    .any(|a| a.provider == p.id && a.what == what),
                "provider {:?} declares cors_policy: proxy in i18n/providers.yaml and has the {} \
                 capability, but no {} row in PROXY_ADAPTERS — that adapter is not known to route \
                 through the CORS proxy. Wire it (see dns/gandi.rs for the pattern) and add the \
                 row.",
                p.id,
                what,
                what,
            );
        }
    }
}

#[test]
fn every_wired_adapter_is_declared_proxy_in_the_registry() {
    for a in PROXY_ADAPTERS {
        let p = PROVIDERS
            .iter()
            .find(|p| p.id == a.provider)
            .unwrap_or_else(|| panic!("{:?} missing from the generated registry", a.provider));
        assert_eq!(
            p.cors_policy,
            CorsPolicy::Proxy,
            "{:?} ({}) routes through the CORS proxy but the registry declares {:?} — the \
             adapter and i18n/providers.yaml disagree about who needs the proxy.",
            a.provider,
            a.what,
            p.cors_policy,
        );
    }
}

#[test]
fn proxy_adapters_route_through_the_proxy_on_web() {
    for a in PROXY_ADAPTERS {
        let base = (a.base_for)(BuildEnv::Web);
        assert!(
            base.starts_with(DEFAULT_PROXY_ROOT),
            "{:?} ({}) built for the browser must call the CORS proxy, got {base:?}. A browser \
             cannot reach {} directly — this provider declares cors_policy: proxy.",
            a.provider,
            a.what,
            a.direct_host,
        );
    }
}

#[test]
fn proxy_adapters_still_call_the_provider_directly_on_native() {
    for a in PROXY_ADAPTERS {
        let base = (a.base_for)(BuildEnv::Native);
        assert!(
            base.starts_with(a.direct_host),
            "{:?} ({}) on native must call {} directly (no proxy hop), got {base:?}.",
            a.provider,
            a.what,
            a.direct_host,
        );
    }
}
