//! The counterparty-URL dial policy must agree with the parser that actually
//! dials.
//!
//! `fauna_core::counterparty_url::validate_counterparty_nest_url` is a
//! string-shape check living in a WASM-safe crate with no URL parser; the dial
//! composes `format!("{nest_url}/api/v1/…")` and hands the result to reqwest,
//! i.e. the WHATWG `url` crate. Two independent parsers over one string is a
//! differential waiting to happen, and it happened: the shape check's `/ ? # @`
//! denylist missed `\`, which WHATWG treats as a path separator for the
//! special schemes.
//!
//! These pins live HERE rather than beside the validator because this is the
//! lowest crate that has both the policy and a real URL parser in scope. They
//! are the executable form of the two properties the ruling
//! (`account-data-plane.md` § The custody grant + ceremony → *What a custodian
//! will dial*) rests on.

/// Property 1 — **origin-only**: whatever the policy accepts, the dialing
/// parser must see as a bare origin. No path, no query, no fragment, and the
/// host it resolves to is the host the string names.
#[test]
fn an_accepted_counterparty_url_parses_as_a_bare_origin() {
    for accepted in [
        "https://nest.example.com",
        "https://nest.example.com/",
        "wss://nest.example.com:8443",
        "https://192.168.1.40:4443",
        "https://[2001:db8::1]:4443",
        "http://127.0.0.1:8080",
        "ws://localhost:3000",
    ] {
        assert!(
            fauna_core::counterparty_url::validate_counterparty_nest_url(accepted).is_ok(),
            "{accepted} is a policy-accepted origin"
        );
        // The dial's own composition, verbatim: `AuthClient` normalises the
        // trailing slash on the way in (`auth_client.rs`, "so the content-API
        // `format!(\"{nest_url}/api/v1/…\")` never double-slashes") and every
        // request is built by concatenation from there.
        let composed = format!("{}/api/v1/whoami", accepted.trim_end_matches('/'));
        let parsed = url::Url::parse(&composed).expect("the dial parses this");
        assert_eq!(
            parsed.path(),
            "/api/v1/whoami",
            "{accepted}: the counterparty must not steer the path"
        );
        assert!(parsed.query().is_none(), "{accepted}: no query");
        assert!(parsed.fragment().is_none(), "{accepted}: no fragment");
        assert_eq!(parsed.username(), "", "{accepted}: no userinfo");
    }
}

/// Property 2 — **the pin key names the dialed host**. `trust::authority_of`
/// keys the SPKI/TOFU pin store; if it and the dialer disagree about where the
/// authority ends, a counterparty can silently drop its own dial out of a
/// stored pin's scope. This is `authority_of`'s own documented contract.
#[test]
fn the_pin_lookup_key_names_the_host_the_dial_reaches() {
    for candidate in [
        "https://nest.example.com",
        "https://nest.example.com/",
        "https://pi.local:8443",
        "https://192.168.1.40:4443",
        // Malformed shapes the policy refuses, kept here because
        // `authority_of` is reached from URL sources the policy never sees.
        r"https://nest.example.com\x",
        r"https://nest.example.com\..\admin",
        // PROBE-613: `authority_of` used to end the authority at `/`/`\`
        // only, so a `?`/`#`-bearing string read the *last* `@`'s
        // right-hand side as the host while every WHATWG parser — the real
        // dialer included — ends the authority at `?`/`#` too and never
        // leaves `nest.example.com` .
        "https://nest.example.com?@[::1]",
        "https://nest.example.com#@[::1]",
        "https://nest.example.com?x=@127.0.0.1:443/",
        "wss://nest.example.com#@localhost",
    ] {
        let key = fauna_client::trust::authority_of(candidate);
        let parsed = url::Url::parse(&format!("{candidate}/api/v1/whoami")).expect("parses");
        let dialed = match parsed.port() {
            Some(p) => format!("{}:{p}", parsed.host_str().expect("host")),
            None => parsed.host_str().expect("host").to_string(),
        };
        assert_eq!(
            key, dialed,
            "{candidate}: the pin-lookup key must name the host actually dialed"
        );
    }
}
