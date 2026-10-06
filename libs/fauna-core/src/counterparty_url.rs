//! The counterparty-supplied nest-URL policy — what a user's device, or their
//! nest, may be made to dial on someone else's say-so.
//!
//! Ruling owner: `docs/goal/architecture/account-data-plane.md` § Replica
//! posture → *The custody grant + ceremony* → **What a custodian will dial**.
//! The class: any URL that reaches a dial loop from a
//! COUNTERPARTY's hand (`CustodyOffer.owner_nest_url` today;
//! `CustodyAccept.custodian_nest_url` inherits the same policy at its first
//! dial site; the nest's custody-hosting row carries one too). The policy is
//! deliberately NOT an address-class block — a home-LAN nest on an RFC-1918
//! address is a first-class deployment — it is *identity before bytes*:
//!
//! - **Origin-only shape.** Scheme + host [+ port], nothing else: no
//!   userinfo, no path, no query, no fragment. The dial appends the fixed
//!   fauna route itself, so a counterparty can never steer request bytes at
//!   an arbitrary endpoint of a host.
//! - **TLS schemes (`https`/`wss`) anywhere.** The existing transport-trust
//!   stack (WebPKI, or the graduated SPKI pin — `security.md` § Transport
//!   trust) means nothing beyond the TLS hello is ever sent to an endpoint
//!   that cannot prove the named identity.
//! - **Plaintext schemes (`http`/`ws`) only for loopback hosts.** The dev /
//!   e2e posture (a loopback nest is the fleet's test norm). The residual —
//!   fixed-shape, payload-free connect attempts at the device's own loopback
//!   ports on a counterparty's say-so — is accepted and recorded in the
//!   ruling. ⚠ This is the ONE clause [`DialScope`] changes: it is a
//!   *device*-scoped acceptance, and a public nest does not get it.
//!
//! WASM-safe, string-shape only (no resolution, no I/O). FIVE doors consume it —
//! three at [`DialScope::Device`]: `begin_offer` (a well-meaning owner learns at
//! offer time), the ceremony's `build_accept` ingest (the adversarial path — a
//! crafted, validly-signed offer is refused at acceptance) and the client
//! custody dial loop (the enforcement backstop — a bad URL already at rest, or
//! rewritten later by the deliver ingest's LWW, never reaches `connect()`); and
//! two at [`DialScope::Nest`]: the custody-hosting register door and the nest's
//! hosting pump, which stand in the same learn-early / backstop relation.
//!
//! ⚠ The shape check lives in a WASM-safe crate with no URL parser, while the
//! dial composes `format!("{nest_url}/api/v1/…")` and hands the result to
//! reqwest's WHATWG parser. Two parsers over one string is a differential by
//! construction, so the agreement is PINNED, not assumed:
//! `libs/fauna-client/tests/counterparty_url_parser_agreement.rs`.

/// **Whose** dial loop is being protected — the one clause of the policy that
/// is not the same for a personal device and a nest.
///
/// Everything else above is one ruling and must not fork: the origin-only
/// shape, the allowlisted `host[:port]` charset and TLS-anywhere hold
/// identically for both. Only the plaintext-loopback carve-out changes, because
/// the acceptance behind it was taken for a *device's* blast radius:
/// *"fixed-shape, payload-free connect attempts at **the device's** own
/// loopback ports"*. Two premises of that acceptance change at a nest — the
/// loopback whose ports get knocked on is the **nest's**, where nest-private
/// surfaces live, and the counterparty is no longer someone the victim chose (a
/// hosting depositor mints both ends of the ceremony themselves).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialScope {
    /// A user's own device dialling a counterparty (the ceremony's three doors:
    /// `begin_offer`, `build_accept`, and the client custody dial loop). The
    /// acceptance applies as ratified — plaintext loopback admitted.
    Device,
    /// A nest dialling on an account holder's say-so (the custody-hosting
    /// register door and its pump).
    Nest {
        /// Whether this nest is a **public deployment**. The nest's own uniform
        /// test is `resolve_handle_domain(d).is_public_dns_name` — the same
        /// predicate `refuse_plain_http_for_public_domain` and the enable-email
        /// default use, so "public" means one thing across the nest.
        ///
        /// ⚠ Deliberately **not** "is TLS enabled": TLS terminates in the
        /// `fauna-sni-router` on a fronted deployment, so `tls_enabled` is
        /// false on plenty of real public nests and would leave the carve-out
        /// standing exactly where it must not be.
        ///
        /// `true` withdraws the plaintext-loopback carve-out — an account
        /// holder cannot make a publicly reachable nest knock at its own
        /// loopback ports. `false` keeps it, which is what makes the local /
        /// tier_3 posture (two nests on `127.0.0.1`, plaintext) work with no
        /// runtime knob and no build-profile split.
        public_deployment: bool,
    },
}

/// Validate a counterparty-supplied nest URL against the dial policy above, for
/// a **device's** dial loop ([`DialScope::Device`]). The nest legs call
/// [`validate_counterparty_nest_url_scoped`].
pub fn validate_counterparty_nest_url(url: &str) -> Result<(), &'static str> {
    validate_counterparty_nest_url_scoped(url, DialScope::Device)
}

/// Validate a counterparty-supplied nest URL against the dial policy above.
/// `Err` carries the human-readable refusal reason (stable enough to log,
/// never parsed). See [`DialScope`] for the single clause `scope` changes.
pub fn validate_counterparty_nest_url_scoped(
    url: &str,
    scope: DialScope,
) -> Result<(), &'static str> {
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err("no scheme — an absolute https:// or wss:// origin is required");
    };
    let scheme = scheme.to_ascii_lowercase();
    let plaintext = match scheme.as_str() {
        "https" | "wss" => false,
        "http" | "ws" => true,
        _ => return Err("unsupported scheme — https, wss, or (loopback-only) http/ws"),
    };
    // Origin-only: at most one trailing slash, then nothing but authority.
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty() {
        return Err("empty host");
    }
    if authority.contains(['/', '?', '#']) {
        return Err("origin-only: no path, query, or fragment");
    }
    if authority.contains('@') {
        return Err("userinfo is refused");
    }
    // The origin-only clause is held by an ALLOWLIST, not a denylist. A
    // denylist already failed here once: the first cut
    // rejected `/ ? # @` and missed `\`, which every WHATWG parser — the
    // `url` crate reqwest dials through — treats as a path separator for
    // the special schemes, so `https://host\..\admin` passed this check and
    // dialed `host` at path `/admin/…`. Whatever a parser may yet make of a
    // byte, a legal `host[:port]` does not contain it.
    if authority
        .chars()
        .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '[' | ']')))
    {
        return Err("origin-only: illegal character in host[:port]");
    }
    let host = crate::resolve::strip_ipv6_brackets(crate::web::split_host_port(authority).0);
    if host.is_empty() {
        return Err("empty host");
    }
    if plaintext {
        // The one scope-dependent clause: a public nest has no
        // plaintext carve-out at all, because the loopback it would knock on is
        // its own. Checked before the loopback test so the refusal names the
        // real reason rather than "not loopback".
        if scope
            == (DialScope::Nest {
                public_deployment: true,
            })
        {
            return Err(
                "plaintext http/ws is refused on a public nest deployment — the loopback \
                 carve-out is a device-scoped acceptance, not a nest one",
            );
        }
        let loopback = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if !loopback {
            return Err("plaintext http/ws is loopback-only — a reachable nest speaks https/wss");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_counterparty_nest_url as validate;
    use super::{DialScope, validate_counterparty_nest_url_scoped as validate_scoped};

    const PUBLIC_NEST: DialScope = DialScope::Nest {
        public_deployment: true,
    };
    const LOCAL_NEST: DialScope = DialScope::Nest {
        public_deployment: false,
    };

    /// The plaintext-loopback carve-out was accepted for a
    /// *device's* own loopback. A publicly reachable nest inherited it, so any
    /// account holder could point a hosting row at the nest's own loopback and
    /// have the pump knock there every pass. The carve-out is withdrawn there —
    /// and nowhere else, so the local/tier_3 posture is untouched.
    #[test]
    fn a_public_nest_has_no_plaintext_loopback_carve_out() {
        for url in [
            "http://127.0.0.1:8080",
            "ws://localhost:3000",
            "http://[::1]:9",
            "http://127.1.2.3",
        ] {
            assert!(
                validate_scoped(url, PUBLIC_NEST).is_err(),
                "{url} must refuse on a public nest — the acceptance was a device's"
            );
            assert!(
                validate_scoped(url, LOCAL_NEST).is_ok(),
                "{url} must still pass on a local nest (the tier_3 rig's whole posture)"
            );
            assert!(
                validate_scoped(url, DialScope::Device).is_ok(),
                "{url} must still pass for a device — the acceptance is unchanged"
            );
        }
    }

    /// Only the plaintext clause is scope-dependent. Everything else is one
    /// ruling, and a fork would be the drift the scope argument exists to
    /// prevent.
    #[test]
    fn every_other_clause_is_identical_across_scopes() {
        for url in [
            // admitted everywhere
            "https://nest.example.com",
            "wss://nest.example.com:8443",
            "https://192.168.1.40:4443",
            // refused everywhere
            "",
            "nest.example.com",
            "ftp://nest.example.com",
            "https://",
            "https://nest.example.com/admin",
            "https://user:pw@nest.example.com",
            r"https://internal.corp.example:8443\..\..\admin",
            "https://nest.example.com%2fadmin",
            // plaintext to a NON-loopback host: refused for every scope, and on
            // a public nest for the scope reason rather than the loopback one
            "http://192.168.1.1",
            "ws://nest.example.com",
        ] {
            let device = validate_scoped(url, DialScope::Device).is_ok();
            assert_eq!(
                device,
                validate_scoped(url, LOCAL_NEST).is_ok(),
                "{url}: a local nest must agree with a device"
            );
            assert_eq!(
                device,
                validate_scoped(url, PUBLIC_NEST).is_ok(),
                "{url}: a public nest must agree with a device outside the plaintext \
                 carve-out"
            );
        }
    }

    #[test]
    fn the_unscoped_alias_is_the_device_scope() {
        for url in ["http://127.0.0.1:8080", "https://nest.example.com", "bad"] {
            assert_eq!(
                validate(url).is_ok(),
                validate_scoped(url, DialScope::Device).is_ok(),
                "{url}: the alias the three device doors call must not drift"
            );
        }
    }

    #[test]
    fn tls_origins_pass_anywhere() {
        for ok in [
            "https://nest.example.com",
            "https://nest.example.com/",
            "wss://nest.example.com:8443",
            "https://192.168.1.40:4443", // the home-LAN nest — first-class
            "https://[2001:db8::1]:4443",
            "HTTPS://nest.example.com", // scheme case is not identity
        ] {
            assert!(validate(ok).is_ok(), "{ok} must pass");
        }
    }

    #[test]
    fn plaintext_is_loopback_only() {
        for ok in [
            "http://127.0.0.1:8080",
            "ws://localhost:3000",
            "http://[::1]:9",
            "http://127.1.2.3", // whole /8
        ] {
            assert!(validate(ok).is_ok(), "{ok} must pass (dev/e2e posture)");
        }
        for bad in [
            "http://192.168.1.1",       // the finding's canonical target
            "ws://nest.example.com",    // plaintext to the internet
            "http://10.0.0.5:6379",     // internal service
            "http://localhost.evil.io", // a NAME that merely contains localhost
        ] {
            assert!(validate(bad).is_err(), "{bad} must refuse");
        }
    }

    #[test]
    fn shape_violations_refuse() {
        for bad in [
            "",
            "nest.example.com",                        // no scheme
            "ftp://nest.example.com",                  // unsupported scheme
            "https://",                                // empty host
            "https:///",                               // still empty
            "https://nest.example.com/admin",          // path
            "https://nest.example.com/api?x=1",        // query
            "https://nest.example.com#frag",           // fragment
            "https://user:pw@nest.example.com",        // userinfo
            "https://nest.example.com/../.well-known", // path however spelled
        ] {
            assert!(validate(bad).is_err(), "{bad} must refuse");
        }
    }

    /// The origin-only clause must hold against the
    /// separators a WHATWG parser recognises but a `/ ? # @` denylist does
    /// not. `\` is the one that shipped: for the special schemes the `url`
    /// crate reqwest dials through treats it exactly as `/`, so every
    /// candidate below dialed a real host at an attacker-chosen path prefix
    /// while passing the shape check. Held by an allowlist now, so the list
    /// is illustrative rather than exhaustive — that is the point of it.
    #[test]
    fn authority_separators_a_denylist_would_miss_are_refused() {
        for bad in [
            r"https://internal.corp.example:8443\..\..\admin", // the probe's own case
            r"https://nest.example.com\x",                     // enough to strip the SPKI pin
            r"https://nest.example.com\/admin",
            "https://nest.example.com\u{0000}.evil.io", // NUL
            "https://nest.example.com\t.evil.io",       // tab: WHATWG strips it
            "https://nest.example.com\n.evil.io",       // newline: likewise
            "https://nest.example.com .evil.io",        // space
            "https://nest.example.com%2fadmin",         // percent-encoded separator
            "https://nеst.example.com",                 // Cyrillic е — not ASCII, not this host
        ] {
            assert!(
                validate(bad).is_err(),
                "{bad:?} must refuse — a legal host[:port] contains none of these"
            );
        }
    }
}
