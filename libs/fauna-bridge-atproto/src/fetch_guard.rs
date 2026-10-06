//! The SSRF guard for **attacker-directed** outbound fetches from the hosted
//! PDS (`docs/goal/behavior/atproto-pds-full.md` § F3 detail, *Service
//! proxying*; § F4 detail, *Client metadata resolution*).
//!
//! Two callers, one guard — deliberately, because they are the same hazard:
//!
//! * **F3 service proxying** dials the service endpoint named by an
//!   `atproto-proxy: did#fragment` header. The DID, its document, and hence
//!   the endpoint URL are all attacker-chosen.
//! * **F4 client-metadata resolution** fetches the OAuth `client_id` URL,
//!   which is likewise whatever the requesting client says it is.
//!
//! Without a guard either one turns the bridge into a confused deputy that
//! reads cloud IMDS credentials (`169.254.169.254`) or internal services from
//! the public internet. The nest's own guard (`bins/fauna-nest/src/ssrf.rs`)
//! defends the same boundary for caller-supplied URLs there; this module is
//! the bridge-side seat of the same policy, and both consume the *same*
//! globally-routable classifier ([`fauna_core::resolve::is_global_ip`]) so the
//! two can never drift apart on what "internal" means.
//!
//! # Why the caller hands over parsed components, not a URL string
//!
//! [`check_fetch_target`] takes a scheme, a host, and the addresses the caller
//! resolved — **not** the URL. That is not an ergonomic accident; it closes a
//! **parser-differential bypass**. Go must parse the URL anyway to make the
//! request. If this module parsed it a *second* time, the two parsers could
//! disagree on which host the URL names — the classic SSRF bypass where the
//! check inspects host A and the connection goes to host B. One parse, by the
//! component that dials, is the only shape with no gap between them.
//!
//! The same reasoning governs `resolved_ips`: the caller passes the addresses
//! it is **about to connect to**, and must then pin them for the connection.
//! Re-resolving after the check re-opens the DNS-rebinding window this guard
//! exists to close (a name that answers "public" for the check and "internal"
//! for the connection). `bins/fauna-nest/src/ssrf.rs::resolve_global_addrs`
//! returns its verified addresses for exactly this reason.
//!
//! # Purity
//!
//! Like [`crate::authz`], this is a **pure function of its inputs** — no I/O,
//! no DNS, no clock, wasm-clean — so the policy is table-driven-testable in
//! Rust and Go keeps only the resolution, the dial, and the caps. Response
//! size and time caps are the caller's half of the guard (they govern the
//! transfer, not the target) and live with the fetch in Go.
//!
//! # Closed world
//!
//! Default-deny, like D8. An unparseable address, an empty address set, an
//! unrecognized scheme — every one of them denies. There is no fall-through.

use serde::{Deserialize, Serialize};

use fauna_core::resolve::{is_global_ip, is_public_dns_name, strip_ipv6_brackets};

// ── Deny reasons (stable strings — callers map them to their own surface) ────

/// Scheme was not `https`.
pub const DENY_SCHEME: &str = "target scheme must be https";
/// Host was an IP literal, a loopback/`.localhost` name, or an mDNS `.local`
/// name — never a registrable public name.
pub const DENY_NOT_PUBLIC_NAME: &str = "target host is not a public DNS name";
/// Host was a single label with no dot (`intranet`), which a search-domain
/// suffix can turn into an internal target.
pub const DENY_SINGLE_LABEL: &str = "target host is a single label";
/// The caller resolved the host to nothing, so there is no address set to
/// verify — refusing to make a decision we cannot make.
pub const DENY_NO_ADDRESSES: &str = "target host resolved to no addresses";
/// An address the caller handed over did not parse as an IP.
pub const DENY_UNPARSEABLE_ADDRESS: &str = "target address did not parse";
/// An address the caller handed over is not globally routable.
pub const DENY_NON_GLOBAL: &str = "target address is not globally routable";

/// The target of an attacker-directed outbound fetch, as the component that
/// will dial it parsed and resolved it.
///
/// See the module docs for why this is components + addresses rather than a
/// URL string — it is a security property, not a convenience.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FetchTarget {
    /// URL scheme as the caller's own parser produced it. Compared
    /// case-insensitively; only `https` is allowed.
    pub scheme: String,
    /// Host component, with no port. A bracketed IPv6 literal (`[::1]`) is
    /// recognized and rejected like any other IP literal.
    pub host: String,
    /// Every address the caller resolved `host` to **and will connect to**.
    /// The caller must pin these for the connection; re-resolving afterwards
    /// re-opens the rebinding window (module docs).
    pub resolved_ips: Vec<String>,
}

/// The decision. `Deny` carries a stable reason string; each caller maps it to
/// its own surface (an XRPC error for the proxy path, an OAuth error for
/// client-metadata resolution) — this module does not know about either.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FetchTargetVerdict {
    Allow,
    Deny { reason: String },
}

impl FetchTargetVerdict {
    fn deny(reason: &str) -> Self {
        FetchTargetVerdict::Deny {
            reason: reason.to_string(),
        }
    }
}

/// Decide whether an attacker-directed outbound fetch may proceed.
///
/// Exported on this crate rather than through a `fauna-ffi` wrapper for the
/// same reason [`crate::authz::authorize`] is: uniffi-bindgen-go emits one Go
/// package per namespace and cannot resolve a type across two, so a function
/// and the types it takes must share a crate.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn check_fetch_target(target: FetchTarget) -> FetchTargetVerdict {
    // HTTPS only. A plaintext hop would expose the minted service JWT (F3) to
    // anyone on the path, and the JWT authenticates as the user.
    if !target.scheme.eq_ignore_ascii_case("https") {
        return FetchTargetVerdict::deny(DENY_SCHEME);
    }

    // A registrable public name — never an IP literal, `localhost`, or an
    // mDNS `.local` name. This is the *name*-shaped half of the check; the
    // address half below is what actually stops a public name pointing
    // inward.
    if !is_public_dns_name(&target.host) {
        return FetchTargetVerdict::deny(DENY_NOT_PUBLIC_NAME);
    }

    // Stricter than `is_public_dns_name` on purpose: a single-label host
    // (`intranet`) is a public DNS name by that classifier's axis, but a
    // resolver search domain can expand it into an internal target. These
    // fetches are attacker-*directed*, so the posture here is deliberately
    // tighter than the accepted consume-side ones.
    if !target.host.contains('.') {
        return FetchTargetVerdict::deny(DENY_SINGLE_LABEL);
    }

    // Closed world: no addresses means no decision to make, so refuse.
    if target.resolved_ips.is_empty() {
        return FetchTargetVerdict::deny(DENY_NO_ADDRESSES);
    }

    // *Every* resolved address must be global. Rejecting on any single
    // non-global answer is what stops a name that returns one public and one
    // internal address from being dialled at the internal one.
    for raw in &target.resolved_ips {
        let unbracketed = strip_ipv6_brackets(raw);
        let Ok(ip) = unbracketed.parse::<std::net::IpAddr>() else {
            return FetchTargetVerdict::deny(DENY_UNPARSEABLE_ADDRESS);
        };
        if !is_global_ip(ip) {
            return FetchTargetVerdict::deny(DENY_NON_GLOBAL);
        }
    }

    FetchTargetVerdict::Allow
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A target that passes, so each test can vary exactly one axis.
    fn ok() -> FetchTarget {
        FetchTarget {
            scheme: "https".into(),
            host: "api.bsky.app".into(),
            resolved_ips: vec!["93.184.216.34".into()],
        }
    }

    fn denied_for(t: FetchTarget) -> String {
        match check_fetch_target(t) {
            FetchTargetVerdict::Allow => panic!("expected a deny"),
            FetchTargetVerdict::Deny { reason } => reason,
        }
    }

    #[test]
    fn a_public_https_target_is_allowed() {
        assert_eq!(check_fetch_target(ok()), FetchTargetVerdict::Allow);
    }

    #[test]
    fn only_https_is_allowed() {
        for scheme in ["http", "file", "gopher", "ftp", "ws", ""] {
            let t = FetchTarget {
                scheme: scheme.into(),
                ..ok()
            };
            assert_eq!(denied_for(t), DENY_SCHEME, "scheme {scheme:?}");
        }
        // Case-insensitive: the caller's parser may not have lowercased.
        let t = FetchTarget {
            scheme: "HTTPS".into(),
            ..ok()
        };
        assert_eq!(check_fetch_target(t), FetchTargetVerdict::Allow);
    }

    #[test]
    fn ip_literal_and_local_name_hosts_are_refused() {
        for host in [
            "127.0.0.1",
            "169.254.169.254",
            "93.184.216.34",
            "[::1]",
            "::1",
            "localhost",
            "foo.localhost",
            "pi.local",
        ] {
            let t = FetchTarget {
                host: host.into(),
                ..ok()
            };
            assert_eq!(denied_for(t), DENY_NOT_PUBLIC_NAME, "host {host:?}");
        }
    }

    #[test]
    fn a_single_label_host_is_refused() {
        let t = FetchTarget {
            host: "intranet".into(),
            ..ok()
        };
        assert_eq!(denied_for(t), DENY_SINGLE_LABEL);
    }

    #[test]
    fn an_empty_address_set_is_refused() {
        let t = FetchTarget {
            resolved_ips: vec![],
            ..ok()
        };
        assert_eq!(denied_for(t), DENY_NO_ADDRESSES);
    }

    #[test]
    fn an_unparseable_address_is_refused() {
        for raw in ["", "not-an-ip", "999.1.1.1", "93.184.216.34:443"] {
            let t = FetchTarget {
                resolved_ips: vec![raw.into()],
                ..ok()
            };
            assert_eq!(denied_for(t), DENY_UNPARSEABLE_ADDRESS, "addr {raw:?}");
        }
    }

    /// The whole point of the guard: a perfectly ordinary public *name* whose
    /// resolution points inward.
    #[test]
    fn a_public_name_resolving_to_an_internal_address_is_refused() {
        for raw in [
            "127.0.0.1",
            "169.254.169.254", // cloud IMDS — the canonical SSRF prize
            "10.0.0.5",
            "192.168.1.1",
            "172.16.0.1",
            "100.64.0.1", // CGNAT
            "0.0.0.0",
            "::1",
            "fd00::1",          // ULA
            "fe80::1",          // link-local
            "[fd00::1]",        // bracketed form
            "::ffff:127.0.0.1", // IPv4-mapped loopback
        ] {
            let t = FetchTarget {
                resolved_ips: vec![raw.into()],
                ..ok()
            };
            assert_eq!(denied_for(t), DENY_NON_GLOBAL, "addr {raw:?}");
        }
    }

    /// A split answer must not be dialled at its internal half.
    #[test]
    fn one_internal_address_among_public_ones_refuses_the_whole_target() {
        let t = FetchTarget {
            resolved_ips: vec![
                "93.184.216.34".into(),
                "169.254.169.254".into(),
                "93.184.216.35".into(),
            ],
            ..ok()
        };
        assert_eq!(denied_for(t), DENY_NON_GLOBAL);
    }

    #[test]
    fn several_public_addresses_all_pass() {
        let t = FetchTarget {
            resolved_ips: vec![
                "93.184.216.34".into(),
                "2606:2800:220:1:248:1893:25c8:1946".into(),
            ],
            ..ok()
        };
        assert_eq!(check_fetch_target(t), FetchTargetVerdict::Allow);
    }

    /// The two ecosystem services F3 proxies to must survive the guard — a
    /// guard that refuses the AppView is a guard nobody can ship.
    #[test]
    fn the_ecosystem_service_hosts_pass_the_name_checks() {
        for host in ["api.bsky.app", "api.bsky.chat"] {
            let t = FetchTarget {
                host: host.into(),
                ..ok()
            };
            assert_eq!(
                check_fetch_target(t),
                FetchTargetVerdict::Allow,
                "host {host:?}"
            );
        }
    }
}
