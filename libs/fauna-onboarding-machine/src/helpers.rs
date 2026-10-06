//! Formatting helpers exposed to all apps via UniFFI / WASM.

use fauna_provisioning::vps::ServerTypeInfo;

/// Render a price in cents as a localized-feeling string. Currency is the
/// ISO-4217 code (e.g. "USD"). The output is `"{whole}.{cents:02} {ccy}"` —
/// callers that want full localization should ignore this and pass the raw
/// (cents, currency) through their platform's number formatter.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn format_price(cents: u64, currency: String) -> String {
    let whole = cents / 100;
    let frac = cents % 100;
    format!("{whole}.{frac:02} {currency}")
}

/// Display label for a server-type radio button:
/// "{id} — {vcpu} vCPU / {mem_gb} GB / {disk_gb} GB disk / {price/100} {ccy}/mo".
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn server_type_label(st: ServerTypeInfo) -> String {
    let price = format_price(st.price_monthly_cents, st.currency.clone());
    format!(
        "{} — {} vCPU / {:.1} GB / {} GB disk / {}/mo",
        st.id, st.vcpu, st.mem_gb, st.disk_gb, price
    )
}

/// Minimum RAM (GB) a mail box needs: the clamd signature DB alone holds ~1.5 GB
/// resident, so a box that runs the content-scan sidecars wants ≥ 2 GB
/// (`docs/goal/architecture/installers/vps.md` § Minimum VPS Requirements).
const MAIL_MIN_MEM_GB: f32 = 2.0;

/// RAM gate for the `vps-config-mail-mode-toggle`: whether `st` may be selected
/// given the chosen mail mode. When mail is ON every plan must have
/// `mem_gb ≥ 2.0` (the scanner sidecars need the RAM); when mail is OFF the
/// social-only box runs lean, so every plan — including the 1 GB tier — is
/// allowed. Clients filter/disable the `vps-server-type-radio` through this
/// single predicate so the gate stays uniform across all six (priority #2). Per
/// `docs/goal/behavior/onboarding.md` §5 (RAM gate).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn server_type_allowed_for_mail(st: ServerTypeInfo, enable_mail: bool) -> bool {
    !enable_mail || st.mem_gb >= MAIL_MIN_MEM_GB
}

/// Re-qualify a bare-localpart admin `handle` with its mail domain for a
/// **factory-reset re-claim** (`mail-bridge-lifecycle.md` § Factory reset →
/// *re-claim handle sourcing*). The nest stores **bare** localparts but
/// auto-registers the primary mail domain from the handle's `@domain` at claim,
/// so the re-onboard after a wipe must carry `localpart@domain` — otherwise the
/// re-claimed nest comes back with no primary mail domain and the bridge idles
/// (mail silently breaks) on any box the `ensure_primary_mail_domain` safety net
/// doesn't cover (notably a custom-domain admin whose handle domain ≠ the nest's
/// `handle_domain`).
///
/// A `handle` that is empty or already contains `@` is returned unchanged.
/// Otherwise the domain is the supplied `domain` (the cached mail domain, if
/// non-empty), else the host parsed from `nest_url`; if neither yields a host
/// the bare localpart is returned (the safety net then covers the common case).
/// The caller decides where the bare handle comes from — linux sources it
/// authoritatively from the live admin session before the wipe (`fauna.account.get`)
/// with a cache fallback, others from the cache — but the qualification itself
/// (and the nest-URL-host parse) lives here once for all seven apps rather than
/// being re-derived per client.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn qualify_reclaim_handle(handle: String, domain: Option<String>, nest_url: String) -> String {
    if handle.is_empty() || handle.contains('@') {
        return handle;
    }
    match domain
        .filter(|d| !d.is_empty())
        .or_else(|| nest_host(&nest_url))
    {
        Some(d) => format!("{handle}@{d}"),
        None => handle,
    }
}

/// Extract the bare host (no scheme / port / userinfo) from a nest base URL —
/// e.g. `https://example.com` → `example.com`, `http://localhost:3000` → `localhost`,
/// bare `example.com` → `example.com`. Returns `None` for an empty/hostless URL.
/// Plain string parsing (the one dependency, `fauna_core::web`, is WASM-safe)
/// so the crate stays wasm-safe.
/// `pub(crate)`: also keys the claim-URI first-contact identity hold
/// (`machine::wizard_submit_claim_code`) — the same bare-host shape
/// `WsNestApi::host_of` matches connections against.
///
/// The userinfo strip is [`fauna_core::web::strip_userinfo`], shared with
/// `fauna_anon_client::trust::authority_of` — two extractors reading the same
/// nest URL must not disagree about which side of an `@` is the host, because
/// one of them keys a TLS-trust decision (`security.md` § Transport trust).
///
/// The authority itself comes from [`fauna_core::web::generic_authority`] —
/// shared with `fauna_core::format::url_host_opt` and
/// `fauna_core::resolve::parse_node_address` — so it ends at the same four
/// WHATWG terminators (`/ \ ? #`) those extractors do, not `/` alone
/// .
pub(crate) fn nest_host(url: &str) -> Option<String> {
    let authority = fauna_core::web::generic_authority(url);
    let hostport = fauna_core::web::strip_userinfo(authority);
    let host = hostport.split(':').next()?.trim(); // strip port
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// The top-level domain (TLD) of a wizard `handle`'s mail domain — e.g.
/// `alice@example.xyz` → `Some("xyz")`. Drives the onboarding "no supported
/// registrar carries .{tld}" message (`dns-no-provider-message`,
/// `docs/goal/behavior/onboarding.md` § dns_config :176): the client shows it
/// with `.{tld}` only when there is a real TLD, so all seven apps agree on
/// what counts as one instead of each re-deriving it.
///
/// Returns `None` (— i.e. "no TLD, hide the `.{tld}` message") when the handle
/// has no `@`, an empty local part or empty domain (`@x` / `x@`), or a domain
/// with no `.` (still being typed, or a bare hostname). The domain extraction
/// mirrors `machine::parse_handle_domain`; the TLD is the substring after the
/// domain's last `.`. This is the canonical shape: web + iOS already matched it,
/// and macOS (previously returned the whole domain on a dot-less domain) is now
/// migrated onto this helper via the UniFFI export below. The web (already
/// correct — uniformity) and android (`substringAfterLast('.')` on the whole
/// handle leaks a local-part dot — bug-fix) legs are the cross-area remainder.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn handle_tld(handle: String) -> Option<String> {
    let at = handle.find('@')?;
    if at == 0 || at == handle.len() - 1 {
        return None;
    }
    let domain = &handle[at + 1..];
    let dot = domain.rfind('.')?;
    let tld = &domain[dot + 1..];
    if tld.is_empty() {
        None
    } else {
        Some(tld.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_price_basic() {
        assert_eq!(format_price(999, "USD".into()), "9.99 USD");
        assert_eq!(format_price(450, "EUR".into()), "4.50 EUR");
        assert_eq!(format_price(0, "USD".into()), "0.00 USD");
    }

    #[test]
    fn format_price_zero_pads_fractional_cents() {
        // The `{frac:02}` pad is a cross-app wire contract: a regression to
        // `{frac}` would render these as "0.1 USD" / "9.5 USD" / "1.0 USD".
        assert_eq!(format_price(1, "USD".into()), "0.01 USD"); // whole == 0, single-digit frac
        assert_eq!(format_price(905, "USD".into()), "9.05 USD"); // nonzero whole, single-digit frac
        assert_eq!(format_price(100, "USD".into()), "1.00 USD"); // exact-dollar carry boundary
    }

    #[test]
    fn format_price_no_thousands_separator() {
        // The shared formatter is deliberately separator-free (callers wanting
        // grouped digits localize the raw cents themselves) — pin it so no
        // client diverges to "5,999.50".
        assert_eq!(format_price(599950, "USD".into()), "5999.50 USD");
        assert_eq!(format_price(1234567, "EUR".into()), "12345.67 EUR");
    }

    #[test]
    fn server_type_label_exact_canonical_format() {
        // The canonical 6-client label shape (web/windows/apple all consume this
        // verbatim — apple's onboarding views route onto it as of the disk-segment
        // unification). Exact equality pins the separators (" — ", " / ", "/mo"),
        // the "{disk_gb} GB disk" segment, and the cents-then-currency price so a
        // layout regression fails here, not silently in a client's UI.
        let st = ServerTypeInfo {
            id: "cx22".into(),
            vcpu: 2,
            mem_gb: 4.0,
            disk_gb: 40,
            price_monthly_cents: 595,
            currency: "EUR".into(),
        };
        assert_eq!(
            server_type_label(st),
            "cx22 — 2 vCPU / 4.0 GB / 40 GB disk / 5.95 EUR/mo"
        );
    }

    #[test]
    fn server_type_label_fractional_gb_and_free_tier() {
        // Fractional RAM renders one decimal (`{:.1}` → "0.5 GB", not "0.5..." or
        // "0 GB"); a zero-cost tier still shows the padded "0.00" price.
        let st = ServerTypeInfo {
            id: "free1".into(),
            vcpu: 1,
            mem_gb: 0.5,
            disk_gb: 10,
            price_monthly_cents: 0,
            currency: "USD".into(),
        };
        assert_eq!(
            server_type_label(st),
            "free1 — 1 vCPU / 0.5 GB / 10 GB disk / 0.00 USD/mo"
        );
    }

    #[test]
    fn server_type_allowed_for_mail_ram_gate() {
        let st = |mem_gb: f32| ServerTypeInfo {
            id: "t".into(),
            vcpu: 1,
            mem_gb,
            disk_gb: 25,
            price_monthly_cents: 500,
            currency: "USD".into(),
        };
        // Mail OFF (social-only box): every plan is allowed, including the 1 GB tier.
        assert!(server_type_allowed_for_mail(st(1.0), false));
        assert!(server_type_allowed_for_mail(st(0.5), false));
        assert!(server_type_allowed_for_mail(st(4.0), false));
        // Mail ON: only plans with mem_gb ≥ 2 (the scanner sidecars need the RAM).
        assert!(!server_type_allowed_for_mail(st(1.0), true));
        assert!(!server_type_allowed_for_mail(st(1.5), true));
        assert!(server_type_allowed_for_mail(st(2.0), true)); // exactly the floor is allowed
        assert!(server_type_allowed_for_mail(st(4.0), true));
    }

    #[test]
    fn qualify_reclaim_handle_passthrough() {
        // Empty stays empty; an already-qualified handle is untouched.
        assert_eq!(
            qualify_reclaim_handle(
                String::new(),
                Some("example.com".into()),
                "https://example.com".into()
            ),
            ""
        );
        assert_eq!(
            qualify_reclaim_handle(
                "alice@example.com".into(),
                Some("other.example".into()),
                "https://x".into()
            ),
            "alice@example.com"
        );
    }

    #[test]
    fn qualify_reclaim_handle_prefers_cached_domain() {
        // A non-empty cached domain wins over the nest-URL host.
        assert_eq!(
            qualify_reclaim_handle(
                "alice".into(),
                Some("custom.example".into()),
                "https://example.com".into()
            ),
            "alice@custom.example"
        );
        // An empty cached domain falls through to the nest-URL host.
        assert_eq!(
            qualify_reclaim_handle(
                "alice".into(),
                Some(String::new()),
                "https://example.com".into()
            ),
            "alice@example.com"
        );
    }

    #[test]
    fn qualify_reclaim_handle_falls_back_to_nest_host() {
        // None domain → parse the host out of the nest URL (scheme/port/userinfo stripped).
        assert_eq!(
            qualify_reclaim_handle("bob".into(), None, "http://localhost:3000".into()),
            "bob@localhost"
        );
        assert_eq!(
            qualify_reclaim_handle("bob".into(), None, "example.com".into()),
            "bob@example.com"
        );
        assert_eq!(
            qualify_reclaim_handle(
                "bob".into(),
                None,
                "https://user@example.com:8443/path".into()
            ),
            "bob@example.com"
        );
        // Hostless URL → can't qualify → return the bare localpart (safety net covers it).
        assert_eq!(
            qualify_reclaim_handle("bob".into(), None, String::new()),
            "bob"
        );
    }

    #[test]
    fn nest_host_variants() {
        assert_eq!(
            nest_host("https://example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            nest_host("http://localhost:3000").as_deref(),
            Some("localhost")
        );
        assert_eq!(nest_host("example.com").as_deref(), Some("example.com"));
        assert_eq!(
            nest_host("https://user@example.com:8443/x").as_deref(),
            Some("example.com")
        );
        assert_eq!(nest_host(""), None);
        assert_eq!(nest_host("https://"), None);
        // PROBE-613: a `?`/`#`-bearing nest URL used to read past the
        // authority and return the userinfo-lookalike host instead of the
        // one actually dialed . Shared cases so a
        // narrower `authority_len` reds this alongside the other five
        // callers .
        for &(url, expected_host) in fauna_core::web::AUTHORITY_TERMINATOR_CASES {
            assert_eq!(nest_host(url).as_deref(), Some(expected_host), "{url}");
        }
    }

    #[test]
    fn handle_tld_extracts_after_last_dot() {
        // The TLD is the substring after the domain's LAST dot — a multi-label
        // domain yields the final label, not the eTLD+1.
        assert_eq!(
            handle_tld("alice@example.xyz".into()).as_deref(),
            Some("xyz")
        );
        assert_eq!(
            handle_tld("alice@a.b.example".into()).as_deref(),
            Some("example")
        );
        assert_eq!(
            handle_tld("alice@example.co.uk".into()).as_deref(),
            Some("uk")
        );
    }

    #[test]
    fn handle_tld_none_when_no_real_tld() {
        // No `@`, empty local part / empty domain, or a dot-less domain all mean
        // "no TLD to show" → None (so the `.{tld}` no-provider message hides).
        // These are exactly the inputs macOS (whole-domain) and android
        // (whole-handle) currently mis-handle; pin the correct web/iOS shape so
        // the migration routes them onto it without re-introducing the drift.
        assert_eq!(handle_tld("alice".into()), None); // no @ (handle still being typed)
        assert_eq!(handle_tld("alice@example".into()), None); // dot-less domain
        assert_eq!(handle_tld("@example.com".into()), None); // empty local part
        assert_eq!(handle_tld("alice@".into()), None); // empty domain
        assert_eq!(handle_tld("alice@example.".into()), None); // trailing dot, empty TLD
        assert_eq!(handle_tld(String::new()), None); // empty handle
    }

    #[test]
    fn handle_tld_ignores_local_part_dots() {
        // A dot in the LOCAL part must not be mistaken for the TLD delimiter —
        // the regression android exhibits (it runs `substringAfterLast('.')` on
        // the whole handle, so `alice.b@example` leaks `b@example`). Domain-first
        // extraction here yields the real TLD / None instead.
        assert_eq!(
            handle_tld("alice.b@example.com".into()).as_deref(),
            Some("com")
        );
        assert_eq!(handle_tld("alice.b@example".into()), None);
    }
}
