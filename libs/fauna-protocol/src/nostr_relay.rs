//! Nostr relay-URL format validation and list maintenance — the shared
//! predicates behind the "Add relay" control in the Nostr bridge settings
//! surface (`ui.yaml` `nostr-add-relay`).
//!
//! Every app that lets a user grow their relay list (web, android, apple,
//! windows, linux, tui) independently hand-rolled the identical sequence:
//! trim the input, do nothing on empty, validate the `wss://`/`ws://` scheme
//! (priority #1/#2, the relay analog of [`crate::handle::validate_handle`]),
//! silently no-op on an exact-string duplicate, else append and persist.
//! [`trimmed_relay_input`] and [`relay_list_appending`] now own the
//! trim/empty and dedup/append halves of that rule (2026-08-23 — the
//! original lift's doc comment called these "just call-site glue", but a
//! comparison across all six implementations found them byte-identical, the
//! same duplicated-decision shape [`relay_url_error`] was lifted for).
//! Each app keeps only its own already-localized invalid-url error copy.
//!
//! The same predicate is the **store-time half of the outbound relay SSRF
//! guard** (`nest/network-exposure.md` § Rulings F7): [`relay_url_refusal`]
//! refuses, from the text alone, a relay no dial could ever be allowed to
//! reach — a non-`ws`/`wss` scheme, no host, an IP-literal host that is not
//! globally routable, or a `localhost` / `*.localhost` name — so the user is
//! told when they add it instead of the nest warning in its log at the next
//! dial. A DNS *name* is never looked up here; its addresses stay the dial's to
//! judge. The nest re-runs the check under its own dial policy
//! (`fauna_bridge_nostr::relay_client::RelayDialPolicy::permits`) on every
//! relay URL it stores; apps run it under the production posture
//! ([`is_global_ip`]).
//!
//! WASM-safe and UniFFI-exportable: pure `&str`/`Vec<String>` in, `bool`/
//! `Option<_>` out, no transport/runtime deps. Clients reach these natively
//! (linux, tui), via the UniFFI face in `fauna-ffi` (apple/android/windows),
//! or the wasm wrapper in `fauna-wasm` (web).

use std::net::{IpAddr, Ipv4Addr};

use fauna_core::resolve::is_global_ip;

/// Why a relay URL is refused before it is stored — the text-decidable half of
/// the relay dial guard (module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayUrlRefusal {
    /// Not a `ws://` / `wss://` URL with a host (unparseable, another scheme,
    /// or no host at all).
    Malformed,
    /// Well-formed, but its host is an IP literal (or a `localhost` name) no
    /// relay dial is permitted to reach — loopback, private, link-local,
    /// unique-local, CGNAT, cloud metadata, … (`nest/network-exposure.md`
    /// § Rulings F7).
    PrivateAddress,
}

/// Refuse `url` if its text alone shows it can never be dialed as a relay
/// under `permits` — the address predicate of the caller's dial policy (apps:
/// [`is_global_ip`]; the nest: `RelayDialPolicy::permits`, so its loopback
/// test affordance holds here too). A `localhost` / `*.localhost` name is
/// judged as the loopback address it always names (RFC 6761); any other name
/// passes — no DNS lookup is performed. Callers are expected to trim first.
pub fn relay_url_refusal(url: &str, permits: impl Fn(IpAddr) -> bool) -> Option<RelayUrlRefusal> {
    let Ok(parsed) = url::Url::parse(url) else {
        return Some(RelayUrlRefusal::Malformed);
    };
    if !matches!(parsed.scheme(), "ws" | "wss") {
        return Some(RelayUrlRefusal::Malformed);
    }
    let ip = match parsed.host() {
        None | Some(url::Host::Domain("")) => return Some(RelayUrlRefusal::Malformed),
        Some(url::Host::Ipv4(v4)) => IpAddr::V4(v4),
        Some(url::Host::Ipv6(v6)) => IpAddr::V6(v6),
        Some(url::Host::Domain(name)) => {
            let name = name.trim_end_matches('.');
            if name == "localhost" || name.ends_with(".localhost") {
                IpAddr::V4(Ipv4Addr::LOCALHOST)
            } else {
                return None;
            }
        }
    };
    (!permits(ip)).then_some(RelayUrlRefusal::PrivateAddress)
}

/// The message an app shows when the user tries to add `url` to their relay
/// list, or `None` when [`relay_url_refusal`] under the production posture
/// ([`is_global_ip`]) accepts it — the refusal →
/// copy map lives here once so every app names the same rule
/// (`nostr.relays.invalid_url` for a malformed URL, `nostr.relays.private_address`
/// for F7's private-network refusal); each app resolves it through its own
/// i18n pipeline.
pub fn relay_url_error(url: &str) -> Option<fauna_core::localized::LocalizedText> {
    use fauna_core::localized::LocalizedText;
    relay_url_refusal(url, is_global_ip).map(|refusal| match refusal {
        RelayUrlRefusal::Malformed => LocalizedText::key("nostr.relays.invalid_url"),
        RelayUrlRefusal::PrivateAddress => LocalizedText::key("nostr.relays.private_address"),
    })
}

/// `input.trim()`, or `None` when that leaves nothing — the identical
/// "empty input, nothing to do" rule every app's relay text field applies
/// before validating or adding.
pub fn trimmed_relay_input(input: &str) -> Option<String> {
    let trimmed = input.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// `existing` with `url` appended, or `None` when `url` is already present
/// (exact match) — the identical dedup-then-append rule every app's "Add
/// relay" control applies once [`relay_url_error`] has returned `None`. Callers
/// persist the returned list; a `None` means no change, no error (matches
/// every app's silent no-op on a duplicate).
pub fn relay_list_appending(existing: &[String], url: &str) -> Option<Vec<String>> {
    if existing.iter().any(|r| r == url) {
        return None;
    }
    let mut next = existing.to_vec();
    next.push(url.to_string());
    Some(next)
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use fauna_core::resolve::is_global_ip;

    use super::{RelayUrlRefusal, relay_list_appending, relay_url_refusal, trimmed_relay_input};

    /// Whether `url` passes the store-time refusal under the production posture.
    fn relay_accepted(url: &str) -> bool {
        relay_url_refusal(url, is_global_ip).is_none()
    }

    #[test]
    fn accepts_wss_and_ws_schemes() {
        assert!(relay_accepted("wss://relay.damus.io"));
        assert!(relay_accepted("ws://relay.example.com:7777"));
    }

    #[test]
    fn refuses_a_private_literal_or_localhost_relay() {
        // `nest/network-exposure.md` § Rulings F7 — no private-network relay,
        // decided from the text alone.
        for url in [
            "ws://127.0.0.1:7777",
            "wss://10.0.0.7",
            "ws://[fd00::1]",
            "ws://169.254.169.254",
            "ws://localhost",
            "ws://localhost:7777",
            "wss://relay.localhost",
            "wss://localhost./",
            "ws://[::1]:7777",
            "ws://[::ffff:192.168.1.1]",
            "ws://100.64.0.1",
            "ws://0x7f.1",
            "wss://user@127.0.0.1/",
        ] {
            assert_eq!(
                relay_url_refusal(url, is_global_ip),
                Some(RelayUrlRefusal::PrivateAddress),
                "{url} must be refused as a private address"
            );
            assert!(!relay_accepted(url), "{url}");
        }
    }

    #[test]
    fn accepts_a_public_literal_and_any_name_without_a_lookup() {
        assert!(relay_accepted("wss://relay.example.com"));
        assert!(relay_accepted("wss://8.8.8.8"));
        assert!(relay_accepted("wss://[2606:4700:4700::1111]"));
        // A name is never resolved here, so the predicate is never consulted.
        assert_eq!(
            relay_url_refusal("wss://relay.invalid", |_: IpAddr| panic!("no lookup")),
            None
        );
    }

    #[test]
    fn the_caller_policy_decides_loopback() {
        // The nest passes its dial policy's predicate: a loopback-permitting
        // test policy admits a loopback relay, still never a private range.
        let loopback_ok = |ip: IpAddr| is_global_ip(ip) || ip.is_loopback();
        assert_eq!(relay_url_refusal("ws://127.0.0.1:1", loopback_ok), None);
        assert_eq!(relay_url_refusal("ws://localhost:7777", loopback_ok), None);
        assert_eq!(
            relay_url_refusal("ws://10.0.0.7", loopback_ok),
            Some(RelayUrlRefusal::PrivateAddress)
        );
    }

    #[test]
    fn rejects_other_schemes() {
        assert!(!relay_accepted("https://relay.damus.io"));
        assert!(!relay_accepted("relay.damus.io"));
        assert!(!relay_accepted("ftp://relay.damus.io"));
    }

    #[test]
    fn rejects_empty_and_whitespace() {
        assert!(!relay_accepted(""));
        assert!(!relay_accepted("   "));
    }

    #[test]
    fn the_error_names_the_rule_that_refused() {
        assert_eq!(super::relay_url_error("wss://relay.example.com"), None);
        assert_eq!(
            super::relay_url_error("http://relay.example.com").map(|t| t.key),
            Some("nostr.relays.invalid_url".to_string())
        );
        assert_eq!(
            super::relay_url_error("ws://192.168.1.10").map(|t| t.key),
            Some("nostr.relays.private_address".to_string())
        );
    }

    #[test]
    fn a_url_with_no_host_is_malformed() {
        // Once a prefix check (the rule every app hand-rolled); F7's
        // store-time refusal made it a real URL parse, so a hostless URL is
        // refused rather than stored.
        for url in ["wss://", "ws://", "wss://:7777"] {
            assert_eq!(
                relay_url_refusal(url, is_global_ip),
                Some(RelayUrlRefusal::Malformed),
                "{url}"
            );
        }
    }

    #[test]
    fn trims_and_rejects_empty_or_whitespace() {
        assert_eq!(
            trimmed_relay_input("  wss://relay.damus.io  "),
            Some("wss://relay.damus.io".to_string())
        );
        assert_eq!(trimmed_relay_input(""), None);
        assert_eq!(trimmed_relay_input("   "), None);
    }

    #[test]
    fn appends_a_new_relay() {
        let existing = vec!["wss://a".to_string()];
        assert_eq!(
            relay_list_appending(&existing, "wss://b"),
            Some(vec!["wss://a".to_string(), "wss://b".to_string()])
        );
    }

    #[test]
    fn no_op_on_exact_duplicate() {
        let existing = vec!["wss://a".to_string(), "wss://b".to_string()];
        assert_eq!(relay_list_appending(&existing, "wss://a"), None);
    }

    #[test]
    fn empty_existing_list_still_appends() {
        assert_eq!(
            relay_list_appending(&[], "wss://a"),
            Some(vec!["wss://a".to_string()])
        );
    }
}
