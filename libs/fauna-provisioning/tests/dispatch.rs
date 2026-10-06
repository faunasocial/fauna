use fauna_provisioning::dns::DnsProvider;
use fauna_provisioning::vps::VpsProvider;
use fauna_provisioning::{ProviderId, dispatch};

/// Build a `Credentials` bag from `(field-id, secret-value)` pairs. The secret
/// values are wrapped in `SecretString` here (one place) so the per-test bodies
/// stay plain string literals.
fn creds(pairs: &[(&str, &str)]) -> dispatch::Credentials {
    dispatch::Credentials {
        entries: pairs
            .iter()
            .map(|(k, v)| (k.to_string(), (*v).into()))
            .collect(),
    }
}

#[test]
fn dns_dispatch_returns_some_for_dns_capable() {
    // Cloudflare has capability=dns → dispatch returns Some.
    let creds = creds(&[("api-token", "test-token")]);
    assert!(dispatch::dns_provider(ProviderId::Cloudflare, creds, None).is_some());
}

#[test]
fn dns_dispatch_returns_none_for_non_dns_providers() {
    // Hetzner has capability=vps only (no dns in providers.yaml).
    assert!(dispatch::dns_provider(ProviderId::Hetzner, Default::default(), None).is_none());
}

/// `record_names_relative_to_zone` forwards through the `DnsDispatch` wrapper to
/// the inner provider. The trait default is `true`, so relying on it (a missing
/// forward) would wrongly relativize Cloudflare's fully-qualified owner names —
/// Cloudflare is the lone `false`; Hetzner/Gandi/Namecheap/Porkbun relativize.
#[test]
fn dns_dispatch_forwards_record_names_relative_to_zone() {
    let cf = dispatch::dns_provider(ProviderId::Cloudflare, creds(&[("api-token", "t")]), None)
        .expect("cloudflare dns dispatch");
    assert!(
        !cf.record_names_relative_to_zone(),
        "Cloudflare wants fully-qualified owner names",
    );

    for (label, built) in [
        (
            "hetzner",
            dispatch::dns_provider(ProviderId::Hetzner, creds(&[("api-token", "t")]), None),
        ),
        (
            "gandi",
            dispatch::dns_provider(
                ProviderId::Gandi,
                creds(&[("personal-access-token", "t")]),
                None,
            ),
        ),
        (
            "namecheap",
            dispatch::dns_provider(
                ProviderId::Namecheap,
                creds(&[("api-user", "u"), ("api-key", "k")]),
                None,
            ),
        ),
        (
            "porkbun",
            dispatch::dns_provider(
                ProviderId::Porkbun,
                creds(&[("api-key", "k"), ("secret-api-key", "s")]),
                None,
            ),
        ),
    ] {
        let p = built.unwrap_or_else(|| panic!("{label} dns dispatch should construct"));
        assert!(
            p.record_names_relative_to_zone(),
            "{label} owners are zone-relative",
        );
    }
}

#[test]
fn registrar_dispatch_returns_some_for_porkbun() {
    let creds = creds(&[("api-key", "test-key"), ("secret-api-key", "test-secret")]);
    assert!(dispatch::registrar(ProviderId::Porkbun, creds).is_some());
}

#[test]
fn registrar_dispatch_returns_none_for_non_registrar() {
    // Hetzner has capability=vps,dns only (no registrar in providers.yaml).
    assert!(dispatch::registrar(ProviderId::Hetzner, Default::default()).is_none());
}

#[test]
fn vps_dispatch_returns_some_for_hetzner() {
    let creds = creds(&[("api-token", "test-token")]);
    assert!(dispatch::vps_provider(ProviderId::Hetzner, creds, None).is_some());
}

#[test]
fn missing_credential_returns_none() {
    // Porkbun needs two creds; passing only one should return None.
    let creds = creds(&[("api-key", "test-key")]);
    assert!(dispatch::registrar(ProviderId::Porkbun, creds).is_none());
}

// --- Namecheap ---

#[test]
fn dns_dispatch_namecheap_some() {
    let creds = creds(&[("api-user", "testuser"), ("api-key", "testkey")]);
    assert!(dispatch::dns_provider(ProviderId::Namecheap, creds, None).is_some());
}

#[test]
fn dns_dispatch_namecheap_wrong_capability() {
    // Namecheap has DNS capability; VPS dispatch returns None.
    let creds = creds(&[("api-user", "testuser"), ("api-key", "testkey")]);
    assert!(dispatch::vps_provider(ProviderId::Namecheap, creds, None).is_none());
}

// --- Gandi ---

#[test]
fn dns_dispatch_gandi_some() {
    let creds = creds(&[("personal-access-token", "test-token")]);
    assert!(dispatch::dns_provider(ProviderId::Gandi, creds, None).is_some());
}

#[test]
fn dns_dispatch_gandi_wrong_capability() {
    // Gandi has DNS capability; VPS dispatch returns None.
    let creds = creds(&[("personal-access-token", "test-token")]);
    assert!(dispatch::vps_provider(ProviderId::Gandi, creds, None).is_none());
}

// --- DigitalOcean ---

#[test]
fn vps_dispatch_digitalocean_some() {
    let creds = creds(&[("api-token", "test-token")]);
    assert!(dispatch::vps_provider(ProviderId::Digitalocean, creds, None).is_some());
}

#[test]
fn vps_dispatch_digitalocean_wrong_capability() {
    // DigitalOcean has VPS capability; DNS dispatch returns None.
    let creds = creds(&[("api-token", "test-token")]);
    assert!(dispatch::dns_provider(ProviderId::Digitalocean, creds, None).is_none());
}

// --- Vultr ---

#[test]
fn vps_dispatch_vultr_some() {
    let creds = creds(&[("api-key", "test-key")]);
    assert!(dispatch::vps_provider(ProviderId::Vultr, creds, None).is_some());
}

#[test]
fn vps_dispatch_vultr_wrong_capability() {
    // Vultr has VPS capability; DNS dispatch returns None.
    let creds = creds(&[("api-key", "test-key")]);
    assert!(dispatch::dns_provider(ProviderId::Vultr, creds, None).is_none());
}

// --- OVH ---

#[test]
fn vps_dispatch_ovh_some() {
    let creds = creds(&[
        ("app-key", "test-app-key"),
        ("app-secret", "test-app-secret"),
        ("consumer-key", "test-consumer-key"),
    ]);
    assert!(dispatch::vps_provider(ProviderId::Ovh, creds, None).is_some());
}

#[test]
fn vps_dispatch_ovh_wrong_capability() {
    // OVH has VPS capability; DNS dispatch returns None.
    let creds = creds(&[
        ("app-key", "test-app-key"),
        ("app-secret", "test-app-secret"),
        ("consumer-key", "test-consumer-key"),
    ]);
    assert!(dispatch::dns_provider(ProviderId::Ovh, creds, None).is_none());
}

// --- Linode ---

#[test]
fn vps_dispatch_linode_some() {
    let creds = creds(&[("api-token", "test-token")]);
    assert!(dispatch::vps_provider(ProviderId::Linode, creds, None).is_some());
}

#[test]
fn vps_dispatch_linode_wrong_capability() {
    // Linode has VPS capability; DNS dispatch returns None.
    let creds = creds(&[("api-token", "test-token")]);
    assert!(dispatch::dns_provider(ProviderId::Linode, creds, None).is_none());
}

// --- Bundled (registry.md § Bundled provider) ---

/// The generic bundled entry carries all three capabilities off ONE credential
/// pair — the user-typed `base-url` plus the hosted-auth `api-token` — so all
/// three dispatchers construct from the same bag, against the same base.
#[test]
fn bundled_dispatch_serves_all_three_capabilities_from_the_typed_base_url() {
    use fauna_provisioning::registrar::Registrar as _;
    let bag = || {
        creds(&[
            ("base-url", "https://bundle.example/"),
            ("api-token", "tok"),
        ])
    };
    let dns = dispatch::dns_provider(ProviderId::Bundled, bag(), None).expect("bundled dns");
    let vps = dispatch::vps_provider(ProviderId::Bundled, bag(), None).expect("bundled vps");
    let reg = dispatch::registrar(ProviderId::Bundled, bag()).expect("bundled registrar");
    // Trailing slash normalized away so `{base}/v1/...` is well-formed.
    assert_eq!(dns.base_url(), "https://bundle.example");
    assert_eq!(vps.base_url(), "https://bundle.example");
    assert!(
        dns.record_names_relative_to_zone(),
        "spec: owner names are zone-relative"
    );
    assert!(
        reg.requires_contact(),
        "spec § Exit: the registrant is the user"
    );
}

/// No canonical host exists for the generic entry: a missing or blank
/// `base-url` is a missing credential, not a fall-through to some default.
#[test]
fn bundled_dispatch_needs_both_the_base_url_and_the_token() {
    assert!(
        dispatch::dns_provider(ProviderId::Bundled, creds(&[("api-token", "tok")]), None).is_none()
    );
    assert!(
        dispatch::vps_provider(
            ProviderId::Bundled,
            creds(&[("base-url", "https://b.example")]),
            None
        )
        .is_none()
    );
    assert!(
        dispatch::registrar(
            ProviderId::Bundled,
            creds(&[("base-url", "  "), ("api-token", "tok")])
        )
        .is_none()
    );
}

/// The E2E `override_base_url` still wins for the DNS/VPS seams, as it does
/// for every other adapter.
#[test]
fn bundled_dispatch_honours_the_e2e_base_url_override() {
    let dns = dispatch::dns_provider(
        ProviderId::Bundled,
        creds(&[("base-url", "https://bundle.example"), ("api-token", "tok")]),
        Some("http://127.0.0.1:9999/bundled".into()),
    )
    .expect("bundled dns");
    assert_eq!(dns.base_url(), "http://127.0.0.1:9999/bundled");
}

#[test]
fn vps_provider_with_override_uses_override_base_url() {
    let mut creds = std::collections::HashMap::new();
    creds.insert("api-token".into(), "tok".into());
    let creds = dispatch::Credentials::from_map(creds);
    let dispatch = dispatch::vps_provider(
        ProviderId::Hetzner,
        creds,
        Some("http://127.0.0.1:9999/hetzner".into()),
    )
    .expect("hetzner vps construction");
    assert_eq!(dispatch.base_url(), "http://127.0.0.1:9999/hetzner");
}

#[test]
fn vps_provider_without_override_uses_default() {
    let mut creds = std::collections::HashMap::new();
    creds.insert("api-token".into(), "tok".into());
    let creds = dispatch::Credentials::from_map(creds);
    let dispatch =
        dispatch::vps_provider(ProviderId::Hetzner, creds, None).expect("hetzner vps construction");
    assert_eq!(dispatch.base_url(), "https://api.hetzner.cloud/v1");
}

#[test]
fn dns_provider_with_override_uses_override_base_url() {
    let mut creds = std::collections::HashMap::new();
    creds.insert("api-token".into(), "tok".into());
    let creds = dispatch::Credentials::from_map(creds);
    let dispatch = dispatch::dns_provider(
        ProviderId::Cloudflare,
        creds,
        Some("http://127.0.0.1:9999/cf".into()),
    )
    .expect("cloudflare dns construction");
    assert_eq!(dispatch.base_url(), "http://127.0.0.1:9999/cf");
}

#[test]
fn dns_provider_without_override_uses_default() {
    let mut creds = std::collections::HashMap::new();
    creds.insert("api-token".into(), "tok".into());
    let creds = dispatch::Credentials::from_map(creds);
    let dispatch = dispatch::dns_provider(ProviderId::Cloudflare, creds, None)
        .expect("cloudflare dns construction");
    assert_eq!(dispatch.base_url(), "https://api.cloudflare.com/client/v4");
}

#[test]
fn dns_provider_gandi_api_base_cred_still_honored_when_override_none() {
    // Regression guard: the existing Gandi `api-base` cred-injection
    // legacy path (used by verify-time registrar tests) is preserved
    // when override_base_url is None.
    let mut creds = std::collections::HashMap::new();
    creds.insert("personal-access-token".into(), "tok".into());
    creds.insert("api-base".into(), "http://gandi-cred.example/api".into());
    let creds = dispatch::Credentials::from_map(creds);
    let dispatch =
        dispatch::dns_provider(ProviderId::Gandi, creds, None).expect("gandi dns construction");
    assert_eq!(dispatch.base_url(), "http://gandi-cred.example/api");
}

#[test]
fn dns_provider_override_wins_over_gandi_api_base_cred() {
    // When both are set, override_base_url wins (test-helper layer
    // is the more specific intent).
    let mut creds = std::collections::HashMap::new();
    creds.insert("personal-access-token".into(), "tok".into());
    creds.insert("api-base".into(), "http://gandi-cred.example/api".into());
    let creds = dispatch::Credentials::from_map(creds);
    let dispatch = dispatch::dns_provider(
        ProviderId::Gandi,
        creds,
        Some("http://override.example/api".into()),
    )
    .expect("gandi dns construction");
    assert_eq!(dispatch.base_url(), "http://override.example/api");
}
