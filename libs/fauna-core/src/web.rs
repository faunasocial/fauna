//! Shared, WASM-safe helpers for web-content hosting addressing.
//!
//! The nest owns serving + the cert lifecycle; the apps own the authoring UI
//! (`docs/goal/behavior/web-content-hosting.md` § Architectural rules #9: shared
//! addressing logic lifts here once a second consumer appears). The reserved-
//! label check + the apex/subdomain URL shapes are the second-consumer logic:
//! the nest skips reserved-label handles at registration/cert issuance, and the
//! client `web-settings` UI shows the user the live `<handle>.<domain>` URL and
//! the reason a reserved-label / handle-less user can't opt in. One source of
//! truth so the UI hint never drifts from the nest's actual routing.

/// Reserved subdomain labels that must never serve as a user-handle subdomain —
/// `mail`/`mta-sts`/`app` have their own routes/SNI, `relay` is the iroh P2P
/// relay's SNI-routed host (`dns-management.md` § Records covered), `pds` is the
/// ATProto PDS bridge's SNI-routed host (service identity `did:web:pds.<domain>`
/// — `atproto-pds-full.md` § Wire & process topology), and `www`
/// is reserved for a future apex alias. Exported as a list (not just the
/// predicate) so `fauna_protocol::handle::RESERVED_HANDLES` can assert it stays
/// a subset — every reserved label must also be an unregistrable handle, or a
/// handle could be minted that later derives a reserved web host. The
/// same list gates ATProto handle derivation (`fauna_protocol::atproto`), which
/// shares the `<handle>.<domain>` namespace.
pub const RESERVED_SUBDOMAIN_LABELS: &[&str] = &["mail", "mta-sts", "app", "www", "relay", "pds"];

/// `true` when `label` is one of [`RESERVED_SUBDOMAIN_LABELS`]. An opted-in user
/// whose handle equals one of these is skipped at registration + cert issuance
/// on the nest, and the client surfaces it as a disabled reason. The
/// `_`-prefixed `_acme-challenge` host cannot collide — handles cannot start
/// with `_`. Case-insensitive.
pub fn is_reserved_subdomain_label(label: &str) -> bool {
    RESERVED_SUBDOMAIN_LABELS
        .iter()
        .any(|r| r.eq_ignore_ascii_case(label))
}

/// The `<handle>.<domain>` host a user's `web` content serves on, or `None` when
/// it cannot serve a subdomain (empty handle/domain, or a reserved-label handle).
pub fn subdomain_host(handle: &str, domain: &str) -> Option<String> {
    if handle.is_empty() || domain.is_empty() || is_reserved_subdomain_label(handle) {
        return None;
    }
    Some(format!("{handle}.{domain}"))
}

/// The `https://<handle>.<domain>/` URL a user's site is reachable at, or `None`
/// (see [`subdomain_host`]).
pub fn subdomain_url(handle: &str, domain: &str) -> Option<String> {
    subdomain_host(handle, domain).map(|host| format!("https://{host}/"))
}

/// The `https://<domain>/` apex URL the admin-designated apex actor's site serves
/// at.
pub fn apex_url(domain: &str) -> String {
    format!("https://{domain}/")
}

/// Rewrites an `http://`/`https://` URL to its `ws://`/`wss://` equivalent by
/// swapping the scheme only — no path/query/trailing-slash normalization (each
/// caller does its own, since the WS-RPC path segment differs per endpoint).
/// Input with no recognized scheme (already `ws(s)://`, or scheme-less) passes
/// through unchanged. Every WS-RPC dialer needs this swap to turn its
/// HTTP-shaped nest address into a dial target.
///
/// **Only the leading scheme is rewritten.** A `http(s)://` substring anywhere
/// else — a path segment, a `?next=http://…` query parameter — is left alone,
/// so a URL carrying an embedded scheme survives intact. The anchoring is the
/// property callers may rely on; do not relax it back to an unanchored
/// `str::replace`, which corrupts exactly those inputs.
pub fn http_to_ws(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        url.to_string()
    }
}

/// Split an RFC 3986 authority into its host and optional port.
///
/// ```text
/// "[::1]:443"  → ("[::1]", Some(443))   brackets KEPT
/// "[::1]"      → ("[::1]", None)
/// "host:443"   → ("host",  Some(443))
/// "host"       → ("host",  None)
/// "::1"        → ("::1",   None)        bare IPv6 — ≥2 colons, so no port
/// "host:junk"  → ("host:junk", None)    not a port, so nothing is split off
/// "[::1]:junk" → ("[::1]:junk", None)   likewise, brackets or not
/// ```
///
/// The input is a bare authority — strip any `userinfo@` with
/// [`strip_userinfo`] first if it may have come from a full URL.
///
/// ## Why the brackets stay on
///
/// This replaced five hand-rolled splitters that had drifted into two
/// incompatible camps: three kept the brackets (they reassemble a URL or a
/// `Host` header, where a bare `::1` would be unparseable) and two stripped
/// them (they hand the result to `parse::<IpAddr>()` or compare hosts bare).
/// That is a real split in what callers need, not copy-paste drift — but it
/// does **not** need a `keep_brackets` flag, because stripping is already its
/// own primitive. This function does the syntactic split and keeps the host
/// exactly as the authority spelled it; a caller that wants a bare host
/// composes:
///
/// ```
/// # use fauna_core::{resolve::strip_ipv6_brackets, web::split_host_port};
/// let (host, port) = split_host_port("[::1]:443");
/// assert_eq!((strip_ipv6_brackets(host), port), ("::1", Some(443)));
/// ```
///
/// ## Malformed input is left whole, deliberately
///
/// A suffix that is not a valid `u16` port is **not** split off — `host:junk`
/// is all host. Every replaced site either already did this or wanted it: the
/// two that split eagerly were `is_loopback_authority` (where `localhost:junk`
/// collapsing to `localhost` meant an unparseable authority got same-box
/// self-signed-cert trust, contradicting that function's own documented
/// "unparseable authorities are non-loopback") and the OAuth loopback
/// redirect-URI comparison (whose doc promises it "can only ever fail to
/// match — never match something it should not"). Leaving malformed input
/// whole is the fail-closed direction for both.
///
/// **Both branches agree on this.** The bracketed branch used to discard an
/// unparseable suffix and hand back a clean `[::1]`, so `[::1]:junk` reached
/// `is_loopback_authority`'s loopback branch — the exact opposite direction
/// from its unbracketed twin `localhost:junk`, in the same function, for the
/// same input class.
///
/// Requiring the *leading* `[` — rather than searching for a `]` anywhere —
/// is what keeps a stray bracket in a DNS name from being read as an IPv6
/// literal.
pub fn split_host_port(authority: &str) -> (&str, Option<u16>) {
    // Bracketed IPv6 literal: `[host]` or `[host]:port`. The `]` sits at
    // `end + 1` in `authority`, so the host slice includes both brackets.
    if let Some(rest) = authority.strip_prefix('[')
        && let Some(end) = rest.find(']')
    {
        let suffix = &authority[end + 2..];
        let port = match suffix.strip_prefix(':').map(str::parse::<u16>) {
            // `[host]` — no suffix at all.
            None if suffix.is_empty() => None,
            Some(Ok(port)) => Some(port),
            // Anything else after the `]` is malformed (`[::1]:junk`,
            // `[::1]:99999`, `[::1]junk`): leave the authority whole, so a
            // caller that parses the host gets a parse failure rather than a
            // silently-cleaned-up literal.
            _ => return (authority, None),
        };
        return (&authority[..=end + 1], port);
    }
    // Unbracketed: only exactly one colon can be a port separator — a bare
    // IPv6 literal (`::1`) has more, and is all host.
    if authority.matches(':').count() == 1
        && let Some((host, port)) = authority.rsplit_once(':')
        && let Ok(port) = port.parse::<u16>()
    {
        return (host, Some(port));
    }
    (authority, None)
}

/// Drop an RFC 3986 `userinfo@` prefix from an authority, leaving `host[:port]`.
///
/// ```text
/// "[::1]@nest.example.com" → "nest.example.com"
/// "user:pass@host:8443"    → "host:8443"
/// "host:8443"              → "host:8443"     no `@`, unchanged
/// "[::1]:8443"             → "[::1]:8443"    colons are not a userinfo mark
/// ```
///
/// **Userinfo names a credential, never a host — and a security decision keyed
/// on the wrong half of an `@` is a trust bypass.** WHATWG resolves the host of
/// `https://[::1]@nest.example.com/` to `nest.example.com`; a hand-rolled
/// extractor that stopped at the first `/` and never looked for an `@` read the
/// same URL's host as the loopback literal `::1`, which bought a *remote*
/// connection the same-box self-signed-cert carve-out
/// (`security.md` § Transport trust — the `is_loopback_authority` /
/// `NestCertTrust.ShouldTrust` path). It also keyed the TOFU identity pin under
/// a host-that-isn't, so adding `user@` to a nest URL defeated the
/// `known_hosts` change warning for the real host.
///
/// Splitting on the **last** `@` matches WHATWG: a raw `@` is not legal inside
/// userinfo (it must be percent-encoded), so the final one is the delimiter.
/// Callers wanting a bare host compose this with [`split_host_port`] and
/// [`crate::resolve::strip_ipv6_brackets`] — three syntactic steps, each its
/// own primitive, rather than one parser per call site.
pub fn strip_userinfo(authority: &str) -> &str {
    match authority.rsplit_once('@') {
        Some((_userinfo, hostport)) => hostport,
        None => authority,
    }
}

/// `true` when `s` contains only the bytes a bare hostname may use — ASCII
/// letters, digits, `.`, and `-` — and is non-empty.
///
/// Rejects userinfo (`@`), a scheme/port colon, a path or escaped-path
/// separator (`/` `\`), a query/fragment marker (`?` `#`), and whitespace:
/// the metacharacters that mean the string names more than one authority, or
/// something other than an authority at all. A **syntax** check only — it
/// does not require an FQDN shape (a bare `localhost` or a single-label name
/// passes) and does not reject an IP literal (`127.0.0.1` passes too); see
/// [`normalize_custom_domain`] for that stricter, registration-time contract.
///
/// **Why this exists, and why it is narrower than [`normalize_custom_domain`].**
/// `resolve_handle_domain`'s `is_public_dns_name` is a negative test — `true`
/// for anything that is not `localhost`/`.local`/an IP literal, so a domain
/// string carrying userinfo, a path, or whitespace still classifies as
/// "public" and a caller composing a URL from it (or persisting it as this
/// deployment's identity domain) inherits the mismatch
/// (`security.md` § Transport trust). A caller that must
/// confirm its candidate names exactly one host — before trusting that
/// classifier, or before storing the string — calls this first. It stays
/// permissive on FQDN-shape/IP-literal so a caller whose domain may
/// legitimately be `localhost` or a bare IP (identity-domain doors, whose
/// own `is_public_dns_name` check already excludes those) is not
/// double-restricted by a rule meant for the *different* custom-web-domain
/// feature.
pub fn is_hostname_syntax(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

/// `true` when `s` is a `host[:port]` authority: the host half passes
/// [`is_hostname_syntax`] and is at most [`MAX_HOSTNAME_BYTES`], and an
/// optional trailing `:port` parses as a `u16` ([`split_host_port`]).
///
/// **Built on `is_hostname_syntax`, not [`normalize_custom_domain`].** A
/// federation peer's announced handle domain may legitimately be a bare
/// loopback authority with a port (`127.0.0.1:PORT` — the shape every
/// in-process federation test fixture announces), an IP literal
/// `normalize_custom_domain`'s registration-time FQDN contract would refuse.
/// `is_hostname_syntax` alone is also not enough: it rejects any `:`
/// (`host:443` fails its own test), so a caller that must accept an optional
/// port composes the split here rather than loosening that function for
/// every other caller that must NOT accept one (a scheme/port colon is
/// exactly the metacharacter `is_hostname_syntax` exists to reject).
///
/// A malformed port suffix (`host:junk`, `host:99999`) is left whole by
/// [`split_host_port`], so the leftover `:` fails `is_hostname_syntax` and
/// this returns `false` — fail-closed, never a silently-cleaned-up host.
pub fn is_domain_authority_syntax(s: &str) -> bool {
    let (host, _port) = split_host_port(s);
    host.len() <= MAX_HOSTNAME_BYTES && is_hostname_syntax(host)
}

/// The byte length of the authority at the start of `s` — `s` already has its
/// scheme stripped by the caller. A WHATWG authority ends at the first of
/// `/ \ ? #`: `/` and `\` for the special schemes (the
/// two this crate's callers ever see — `https`/`http`/`ws`/`wss`), plus `?`
/// (query) and `#` (fragment), which every scheme's authority ends at. Shared
/// so the terminator set can't drift per-extractor again — it already has
/// twice: userinfo, then `\` alone,
/// each fixed in one extractor while the others kept the narrower set.
fn authority_len(s: &str) -> usize {
    s.find(['/', '\\', '?', '#']).unwrap_or(s.len())
}

/// Extract the `host[:port]` authority from a nest URL, for keying trust
/// state (a TOFU pin-store key, native or browser). `fauna-anon-client` and
/// `fauna-wasm` each hand-copied this exact body; both already called
/// [`strip_userinfo`] here, making this its natural shared home.
///
/// Any `userinfo@` is dropped ([`strip_userinfo`]), so the key names the host
/// a URL parser would actually connect to — keying a pin under `user@host`
/// would silently exempt `host` from a `known_hosts`-style change warning.
///
/// **The authority ends at the first of `/ \ ? #`**,
/// via [`authority_len`]: every WHATWG parser — the `url` crate reqwest dials
/// through — ends the special-scheme authorities this function strips at all
/// four, so stopping short on any one of them made a pin-store key disagree
/// with the host actually connected to (`https://host\x` keyed `host\x` while
/// the dial went to `host`; `https://host?@evil` keyed `evil` while the dial
/// went to `host`) — silently missing the stored pin for the real host, or
/// keying it under an attacker-chosen one. The contract is that the key names
/// the dialed host; only recognizes its own fixed scheme set — see
/// [`generic_authority`] for the any-scheme sibling the display/resolve
/// extractors need.
pub fn authority_of(nest_url: &str) -> String {
    let s = nest_url
        .strip_prefix("https://")
        .or_else(|| nest_url.strip_prefix("http://"))
        .or_else(|| nest_url.strip_prefix("wss://"))
        .or_else(|| nest_url.strip_prefix("ws://"))
        .unwrap_or(nest_url);
    let end = authority_len(s);
    strip_userinfo(&s[..end]).to_string()
}

/// Strip a leading `scheme://` — **any** scheme, unlike [`authority_of`]'s
/// fixed `http(s)`/`ws(s)` set — and return the authority: the substring up to
/// the first WHATWG authority terminator ([`authority_len`]: `/ \ ? #`). Input
/// with no `://` (already scheme-less, or malformed) passes through whole.
///
/// Userinfo is **not** stripped here — the authority is returned as spelled,
/// `userinfo@host[:port]` included. Callers that want a bare host compose
/// [`strip_userinfo`] themselves; a caller that also wants the port composes
/// [`strip_userinfo`] + [`split_host_port`] instead. The current caller list
/// and why each one needs the any-scheme, four-terminator cut is
/// `security.md` § Transport trust's job to keep current, not this doc's —
/// restating it here already drifted stale once, when
/// `fauna_launch_machine::auth::hint_authority_url` joined the family
/// without this comment naming it.
pub fn generic_authority(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    &after_scheme[..authority_len(after_scheme)]
}

/// One `(url, expected_host)` case per [`authority_len`] terminator (`/ \ ? #`),
/// shared so `generic_authority`'s six callers — `format::url_host_opt`,
/// `resolve::parse_node_address`, `data::url_host`,
/// `fauna_onboarding_machine::helpers::nest_host`,
/// `fauna_launch_machine::auth::hint_authority_url` and
/// `fauna_provisioning::probe::split_host_port` — pin the same terminator
/// set from their own test modules instead of each keeping a narrower private
/// copy (`security.md` § Transport trust ). Every URL
/// names a different, attacker-controlled host right after its terminator, the
/// way `authority_of_ends_at_query_and_fragment_not_just_slash_and_backslash`
/// above already does for `authority_of`: narrowing `authority_len` to drop
/// that terminator would let the text after it be read as (part of) the host
/// instead of `expected_host`.
///
/// `#[cfg(any(test, feature = "test-helpers"))]`, not `#[cfg(test)]` alone,
/// because the onboarding-machine and launch-machine callers need it from
/// their own crates' test builds, where `cfg(test)` is never set on this one
/// (mirrors `authoritative_dns::spawn_responder`'s reason for the same gate).
#[cfg(any(test, feature = "test-helpers"))]
pub const AUTHORITY_TERMINATOR_CASES: &[(&str, &str)] = &[
    ("https://nest.example.com/@evil.example", "nest.example.com"),
    (
        "https://nest.example.com\\@evil.example",
        "nest.example.com",
    ),
    ("https://nest.example.com?@evil.example", "nest.example.com"),
    ("https://nest.example.com#@evil.example", "nest.example.com"),
];

/// Reserved sub-hosts the nest serves *itself* (never user web content) on its
/// apex: `mail.`/`mta-sts.`/`app.<apex>`. Single source of truth shared with the
/// nest's `HostResolver::is_reserved_host` (so the host-routing exclusion and the
/// custom-domain registration guard agree on what's nest-owned) and the
/// subdomain-label list above (`mail`/`mta-sts`/`app`). The `_acme-challenge.*`
/// challenge host is a wildcard prefix the nest checks separately.
pub fn is_reserved_nest_host(host: &str, apex_domain: &str) -> bool {
    ["mail.", "mta-sts.", "app."]
        .iter()
        .any(|prefix| host == format!("{prefix}{apex_domain}"))
}

/// True if `host` is a name this nest already owns: its apex domain itself or a
/// reserved sub-host ([`is_reserved_nest_host`]). A user-registered custom web
/// domain must never be one of these — it would shadow the admin-designated apex
/// actor or a nest service host — so the nest rejects it at registration,
/// defense-in-depth behind DNS verification. `apex_domain` empty ⇒ a domainless
/// box owns no web host, so nothing is nest-owned.
pub fn is_nest_owned_host(host: &str, apex_domain: &str) -> bool {
    !apex_domain.is_empty() && (host == apex_domain || is_reserved_nest_host(host, apex_domain))
}

/// RFC 1035's ceiling on a whole hostname, in bytes. Shared rather than
/// re-spelled: [`normalize_custom_domain`] enforces it for an admin-typed
/// custom domain, and the peer-profile harvest (`fauna.state.peer-anchors`) enforces it for a
/// host it takes from a *peer's* signed profile — a value no human reviews.
/// One literal, two doors.
pub const MAX_HOSTNAME_BYTES: usize = 253;

/// RFC 1035's ceiling on one dot-separated label, in bytes. Same sharing
/// rationale as [`MAX_HOSTNAME_BYTES`].
pub const MAX_DNS_LABEL_BYTES: usize = 63;

/// Why a user-supplied custom web domain was rejected at registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomDomainError {
    /// Empty (or whitespace / trailing-dot only).
    Empty,
    /// Contains a metacharacter that isn't part of a hostname — a scheme/port
    /// (`:`), path (`/` `\`), wildcard (`*`), userinfo (`@`), whitespace, or any
    /// non-`[a-z0-9.-]` byte (covers Unicode / control chars; IDNs must be
    /// pre-punycoded to `xn--`).
    InvalidChars,
    /// A bare IP literal (v4 dotted-quad; v6 is already caught by `:` above).
    IpLiteral,
    /// Not a well-formed FQDN: fewer than two labels, an empty label, a label
    /// over [`MAX_DNS_LABEL_BYTES`], a label with a leading/trailing `-`, or a
    /// whole host over [`MAX_HOSTNAME_BYTES`].
    NotFqdn,
}

impl std::fmt::Display for CustomDomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Empty => "domain must not be empty",
            Self::InvalidChars => {
                "domain contains invalid characters (no scheme/port/path/wildcard; ASCII only)"
            }
            Self::IpLiteral => "domain must be a hostname, not an IP address",
            Self::NotFqdn => "domain must be a fully-qualified domain name",
        };
        f.write_str(s)
    }
}

/// Normalize a DNS name for case/trailing-dot-insensitive comparison: strips
/// ALL trailing dots (`trim_end_matches`, not `strip_suffix` — `"a.b.."` and
/// `"a.b."` normalize alike; only degenerate non-DNS inputs can collide this
/// way), then lowercases. No validation — a bare match-key
/// normalization for comparing two DNS names, unlike [`normalize_custom_domain`]
/// which additionally validates the input is a well-formed hostname worth
/// persisting.
pub fn normalize_dns_name(s: &str) -> String {
    s.trim_end_matches('.').to_ascii_lowercase()
}

/// Normalize + validate a user-supplied custom web domain for registration
/// (`web-content-hosting.md` § Custom domains; rule #9 flags this as the
/// shareable validation). Lowercases, strips surrounding whitespace + a trailing
/// dot, then rejects empties, hostname metacharacters, IP literals, and
/// malformed FQDNs. Pure + WASM-safe; the nest additionally rejects its own
/// apex + reserved sub-hosts ([`is_nest_owned_host`]) at the call site (runtime
/// knowledge). Returns the normalized domain to persist.
pub fn normalize_custom_domain(input: &str) -> Result<String, CustomDomainError> {
    let trimmed = input.trim().trim_end_matches('.').trim();
    if trimmed.is_empty() {
        return Err(CustomDomainError::Empty);
    }
    let lower = trimmed.to_ascii_lowercase();
    // Hostname charset only — a `:`/`/`/`*`/`@`/space/Unicode byte means it isn't
    // a bare hostname (a scheme, port, path, wildcard, or IDN), so reject it.
    if !lower
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
    {
        return Err(CustomDomainError::InvalidChars);
    }
    // A dotted-quad passes the charset check — reject IP literals explicitly so a
    // custom "domain" can't be an address.
    if lower.parse::<std::net::Ipv4Addr>().is_ok() {
        return Err(CustomDomainError::IpLiteral);
    }
    if lower.len() > MAX_HOSTNAME_BYTES {
        return Err(CustomDomainError::NotFqdn);
    }
    let labels: Vec<&str> = lower.split('.').collect();
    if labels.len() < 2 {
        return Err(CustomDomainError::NotFqdn);
    }
    for label in &labels {
        if label.is_empty()
            || label.len() > MAX_DNS_LABEL_BYTES
            || label.starts_with('-')
            || label.ends_with('-')
        {
            return Err(CustomDomainError::NotFqdn);
        }
    }
    Ok(lower)
}

/// Minimal percent-decoding: `%XX` hex escapes to bytes, with an optional
/// `application/x-www-form-urlencoded` `+`-as-space pass. An incomplete or
/// invalid `%` escape passes through unchanged (including a bare trailing
/// `%`). Decoded bytes are lossily reassembled as UTF-8. Pure and
/// dependency-free — no URL crate. The canonical form of the same decode
/// loop independently hand-rolled for a scope-clause `aud=` decode and a
/// `application/x-www-form-urlencoded` query-string parse.
pub fn percent_decode(s: &str, plus_as_space: bool) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' if plus_as_space => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_labels_case_insensitive() {
        for reserved in ["mail", "MTA-STS", "App", "www"] {
            assert!(is_reserved_subdomain_label(reserved), "{reserved} reserved");
        }
        for ok in ["alice", "bob", "blog"] {
            assert!(!is_reserved_subdomain_label(ok), "{ok} allowed");
        }
    }

    #[test]
    fn subdomain_url_shape() {
        assert_eq!(
            subdomain_url("alice", "example.com").as_deref(),
            Some("https://alice.example.com/")
        );
        assert_eq!(subdomain_url("", "example.com"), None);
        assert_eq!(subdomain_url("alice", ""), None);
        assert_eq!(subdomain_url("www", "example.com"), None);
    }

    #[test]
    fn apex_url_shape() {
        assert_eq!(apex_url("example.com"), "https://example.com/");
    }

    #[test]
    fn http_to_ws_swaps_scheme_only() {
        assert_eq!(
            http_to_ws("https://nest.example.com:8080/path"),
            "wss://nest.example.com:8080/path"
        );
        assert_eq!(http_to_ws("http://localhost:3000/"), "ws://localhost:3000/");
    }

    #[test]
    fn http_to_ws_passes_through_unrecognized_scheme() {
        assert_eq!(
            http_to_ws("wss://already.example.com"),
            "wss://already.example.com"
        );
        assert_eq!(http_to_ws("example.com"), "example.com");
        assert_eq!(http_to_ws(""), "");
    }

    #[test]
    fn http_to_ws_rewrites_only_the_leading_scheme() {
        // An embedded scheme in the path/query is data, not a scheme to swap.
        assert_eq!(
            http_to_ws("https://nest.example.com/ws?next=http://foo"),
            "wss://nest.example.com/ws?next=http://foo"
        );
        assert_eq!(
            http_to_ws("http://nest.example.com/p/https://bar"),
            "ws://nest.example.com/p/https://bar"
        );
        // A scheme that is present but not leading is left entirely alone.
        assert_eq!(http_to_ws("//relative/https://x"), "//relative/https://x");
    }

    #[test]
    fn split_host_port_keeps_ipv6_brackets_on_the_host() {
        assert_eq!(split_host_port("[::1]:443"), ("[::1]", Some(443)));
        assert_eq!(split_host_port("[::1]"), ("[::1]", None));
        assert_eq!(
            split_host_port("[2001:db8::1]:8080"),
            ("[2001:db8::1]", Some(8080))
        );
    }

    #[test]
    fn split_host_port_handles_names_and_v4() {
        assert_eq!(split_host_port("host:443"), ("host", Some(443)));
        assert_eq!(split_host_port("host"), ("host", None));
        assert_eq!(split_host_port("127.0.0.1:8080"), ("127.0.0.1", Some(8080)));
        assert_eq!(split_host_port("127.0.0.1"), ("127.0.0.1", None));
        assert_eq!(split_host_port(""), ("", None));
    }

    #[test]
    fn split_host_port_leaves_a_bare_ipv6_literal_whole() {
        // Unbracketed IPv6 has ≥2 colons — none of them a port separator.
        // The hand-rolled splitters this replaced got these wrong in three
        // different ways (`""`, `"::"`, and `("::", 1)`).
        assert_eq!(split_host_port("::1"), ("::1", None));
        assert_eq!(split_host_port("::1:8080"), ("::1:8080", None));
        assert_eq!(split_host_port("2001:db8::1"), ("2001:db8::1", None));
    }

    #[test]
    fn split_host_port_leaves_a_non_port_suffix_attached() {
        // Fail-closed: a suffix that is not a u16 is not a port, so nothing
        // splits off and the caller sees the authority it was actually given.
        assert_eq!(split_host_port("host:junk"), ("host:junk", None));
        assert_eq!(split_host_port("host:99999"), ("host:99999", None));
        assert_eq!(split_host_port("host:"), ("host:", None));
        // The bracketed camp agrees. This line used to assert
        // `("[::1]", None)` — a *clean* literal handed back from malformed
        // input, contradicting the name of the very test it sat in, and the
        // reason `[::1]:junk` reached `is_loopback_authority`'s loopback
        // branch while its twin `localhost:junk` did not.
        assert_eq!(split_host_port("[::1]:junk"), ("[::1]:junk", None));
        assert_eq!(split_host_port("[::1]:99999"), ("[::1]:99999", None));
        assert_eq!(
            split_host_port("[127.0.0.1]:junk"),
            ("[127.0.0.1]:junk", None)
        );
        // A suffix with no colon at all is malformed the same way.
        assert_eq!(split_host_port("[::1]junk"), ("[::1]junk", None));
        // ...but a well-formed bracketed authority is untouched, so this is
        // not a blanket "brackets are suspicious" rule.
        assert_eq!(split_host_port("[::1]"), ("[::1]", None));
        assert_eq!(split_host_port("[::1]:443"), ("[::1]", Some(443)));
    }

    #[test]
    fn authority_of_ends_at_query_and_fragment_not_just_slash_and_backslash() {
        // PROBE-613: a `?`/`#`-bearing nest URL read the *last* `@`'s
        // right-hand side as the authority, because `authority_of` only ever
        // stopped at `/` or `\`. Every WHATWG parser ends the authority at
        // `?`/`#` too, so these four used to key the TOFU pin and the
        // same-box loopback carve-out under an attacker-chosen host while the
        // real dial went to `nest.example.com`.
        assert_eq!(
            authority_of("https://nest.example.com?@[::1]"),
            "nest.example.com"
        );
        assert_eq!(
            authority_of("https://nest.example.com#@[::1]"),
            "nest.example.com"
        );
        assert_eq!(
            authority_of("https://nest.example.com?x=@127.0.0.1:443/"),
            "nest.example.com"
        );
        assert_eq!(
            authority_of("wss://nest.example.com#@localhost"),
            "nest.example.com"
        );
    }

    #[test]
    fn generic_authority_accepts_any_scheme_and_shares_the_terminator_set() {
        assert_eq!(
            generic_authority("gemini://nest.example.com:1965/x"),
            "nest.example.com:1965"
        );
        // No `://` at all → scheme-less input passes through whole.
        assert_eq!(generic_authority("bare-host"), "bare-host");
        assert_eq!(generic_authority(""), "");
        // Same four terminators as `authority_of`, for a scheme it doesn't
        // recognize.
        assert_eq!(
            generic_authority("ftp://nest.example.com?@evil"),
            "nest.example.com"
        );
        assert_eq!(
            generic_authority("ftp://nest.example.com#@evil"),
            "nest.example.com"
        );
        // Userinfo is left in place — composing `strip_userinfo` is the
        // caller's job.
        assert_eq!(
            generic_authority("https://user@nest.example.com:8443/x"),
            "user@nest.example.com:8443"
        );
    }

    #[test]
    fn strip_userinfo_keeps_the_host_after_the_last_at() {
        // Userinfo is a credential, not a host. WHATWG resolves the host of
        // `https://[::1]@nest.example.com/` to `nest.example.com`; reading the
        // `[::1]` as the host is what let a remote nest claim the same-box
        // self-signed-cert carve-out (`security.md` § Transport trust).
        assert_eq!(strip_userinfo("[::1]@nest.example.com"), "nest.example.com");
        assert_eq!(strip_userinfo("user:pass@host:8443"), "host:8443");
        assert_eq!(strip_userinfo("alice@127.0.0.1:443"), "127.0.0.1:443");
        // A raw `@` is illegal inside userinfo, so the LAST one delimits.
        assert_eq!(strip_userinfo("a@b@host"), "host");
        // No userinfo: unchanged. Colons are not a userinfo mark, so a bare
        // or bracketed IPv6 literal must survive whole.
        assert_eq!(strip_userinfo("host:8443"), "host:8443");
        assert_eq!(strip_userinfo("[::1]:8443"), "[::1]:8443");
        assert_eq!(strip_userinfo("::1"), "::1");
        assert_eq!(strip_userinfo(""), "");
        // Empty host after the `@` stays empty rather than falling back to
        // the userinfo — fail-closed for every classifier downstream.
        assert_eq!(strip_userinfo("user@"), "");
    }

    #[test]
    fn is_hostname_syntax_accepts_bare_hostnames() {
        assert!(is_hostname_syntax("example.com"));
        assert!(is_hostname_syntax("nest.example.com"));
        assert!(is_hostname_syntax("localhost"));
        assert!(is_hostname_syntax("local"));
        assert!(is_hostname_syntax("127.0.0.1"));
        assert!(is_hostname_syntax("EXAMPLE.COM"));
        assert!(is_hostname_syntax("xn--fsq.example")); // an IDN punycode label is plain ASCII; reserved TLD
    }

    #[test]
    fn is_hostname_syntax_rejects_userinfo_path_query_fragment_and_whitespace() {
        // The matrix from `security.md` § Transport trust:
        // each of these classified `is_public_dns_name: true` under the old
        // negative test while composing something other than a bare host.
        assert!(!is_hostname_syntax("nest.example.com@attacker.example"));
        assert!(!is_hostname_syntax("nest.example.com/../../evil"));
        assert!(!is_hostname_syntax(" "));
        assert!(!is_hostname_syntax("nest.example.com?x=1"));
        assert!(!is_hostname_syntax("nest.example.com#frag"));
        assert!(!is_hostname_syntax("nest.example.com\\evil"));
        assert!(!is_hostname_syntax("host:443"));
        assert!(!is_hostname_syntax(""));
        assert!(!is_hostname_syntax("user:pass@127.0.0.1:8080"));
    }

    #[test]
    fn is_domain_authority_syntax_accepts_a_host_with_an_optional_port() {
        // The `127.0.0.1:PORT` shape every in-process federation fixture
        // announces — `is_hostname_syntax` alone rejects this (`host:443`
        // is one of its own negative cases above).
        assert!(is_domain_authority_syntax("127.0.0.1:52341"));
        assert!(is_domain_authority_syntax("nest.example.com:443"));
        assert!(is_domain_authority_syntax("nest.example.com"));
        assert!(is_domain_authority_syntax("127.0.0.1"));
        assert!(is_domain_authority_syntax("localhost"));
    }

    #[test]
    fn is_domain_authority_syntax_rejects_oversized_and_malformed_authorities() {
        // A 300-byte host is over MAX_HOSTNAME_BYTES (253) even though every
        // byte is a legal hostname character — the ceiling
        // `is_hostname_syntax` alone does not enforce.
        let oversized = format!("{}.example", "a".repeat(300));
        assert!(!is_domain_authority_syntax(&oversized));
        assert_eq!(split_host_port(&oversized).0.len(), oversized.len());
        assert!(oversized.len() > MAX_HOSTNAME_BYTES);

        // A malformed port suffix is left attached to the host by
        // `split_host_port`, so its `:` fails `is_hostname_syntax`.
        assert!(!is_domain_authority_syntax("host:junk"));
        assert!(!is_domain_authority_syntax("host:99999"));

        // The ordinary metacharacter matrix still applies to the host half.
        assert!(!is_domain_authority_syntax(
            "nest.example.com@attacker.example"
        ));
        assert!(!is_domain_authority_syntax("nest.example.com/../../evil"));
        assert!(!is_domain_authority_syntax(""));
        assert!(!is_domain_authority_syntax(" "));
        assert!(!is_domain_authority_syntax("user:pass@127.0.0.1:8080"));
    }

    #[test]
    fn strip_userinfo_composes_into_a_bare_host() {
        // The full three-step composition the trust classifier uses:
        // userinfo-strip → split → bracket-strip.
        fn bare(a: &str) -> &str {
            crate::resolve::strip_ipv6_brackets(split_host_port(strip_userinfo(a)).0)
        }
        assert_eq!(bare("[::1]@nest.example.com"), "nest.example.com");
        assert_eq!(bare("alice@127.0.0.1:443"), "127.0.0.1");
        assert_eq!(bare("[::1]:443"), "::1");
        // Malformed stays malformed all the way through, so an `IpAddr` parse
        // downstream fails instead of succeeding on a cleaned-up literal.
        assert_eq!(bare("[::1]:junk"), "[::1]:junk");
    }

    #[test]
    fn split_host_port_requires_a_leading_bracket_to_read_ipv6() {
        // A `]` in the middle of a name is not an IPv6 literal.
        assert_eq!(split_host_port("a]b:80"), ("a]b", Some(80)));
        // An unterminated bracket is not one either — left whole.
        assert_eq!(split_host_port("[::1"), ("[::1", None));
    }

    #[test]
    fn split_host_port_composes_with_strip_ipv6_brackets_for_a_bare_host() {
        // The documented pairing for callers that parse the host as an IpAddr.
        fn bare(a: &str) -> &str {
            crate::resolve::strip_ipv6_brackets(split_host_port(a).0)
        }
        assert_eq!(bare("[::1]:443"), "::1");
        assert_eq!(bare("::1"), "::1");
        assert_eq!(bare("127.0.0.1:443"), "127.0.0.1");
        assert_eq!(bare("nest.example.com:443"), "nest.example.com");
    }

    #[test]
    fn nest_owned_host_covers_apex_and_reserved_subhosts() {
        let apex = "example.com";
        // The apex itself and the reserved sub-hosts are nest-owned.
        assert!(is_nest_owned_host("example.com", apex));
        assert!(is_nest_owned_host("mail.example.com", apex));
        assert!(is_nest_owned_host("mta-sts.example.com", apex));
        assert!(is_nest_owned_host("app.example.com", apex));
        // A user's own subdomain or a third-party domain is not.
        assert!(!is_nest_owned_host("alice.example.com", apex));
        // ⚠ The third-party domain must NOT be `example.com`: the publish scrub
        // rules rewrite `example.com` -> `example.com`, so an `example.com` here
        // collapses into the apex above and this assertion contradicts its own
        // first line in the public tree. Found by the local public-CI replay
        // 2026-08-24; `example.net` is the codebase's established contrast
        // domain and the scrub leaves it alone.
        assert!(!is_nest_owned_host("example.net", apex));
        // A domainless box owns no web host.
        assert!(!is_nest_owned_host("example.com", ""));
    }

    #[test]
    fn normalize_dns_name_strips_trailing_dot_and_lowercases() {
        assert_eq!(normalize_dns_name("Example.COM."), "example.com");
        assert_eq!(normalize_dns_name("example.com"), "example.com");
        assert_eq!(normalize_dns_name(""), "");
    }

    #[test]
    fn normalize_custom_domain_accepts_and_lowercases() {
        assert_eq!(
            normalize_custom_domain("Example.COM").unwrap(),
            "example.com"
        );
        // Surrounding whitespace + a trailing FQDN dot are stripped.
        assert_eq!(
            normalize_custom_domain("  blog.example.com.  ").unwrap(),
            "blog.example.com"
        );
        // Punycode IDN labels are allowed (already ASCII).
        assert_eq!(
            normalize_custom_domain("xn--bcher-kva.example").unwrap(),
            "xn--bcher-kva.example"
        );
    }

    #[test]
    fn normalize_custom_domain_rejects_malformed() {
        use CustomDomainError::*;
        assert_eq!(normalize_custom_domain(""), Err(Empty));
        assert_eq!(normalize_custom_domain("   "), Err(Empty));
        assert_eq!(normalize_custom_domain("."), Err(Empty));
        // Metacharacters: scheme/port, path, wildcard, userinfo, space, Unicode.
        assert_eq!(
            normalize_custom_domain("https://example.com"),
            Err(InvalidChars)
        );
        assert_eq!(
            normalize_custom_domain("example.com:8443"),
            Err(InvalidChars)
        );
        assert_eq!(
            normalize_custom_domain("example.com/evil"),
            Err(InvalidChars)
        );
        assert_eq!(normalize_custom_domain("*.example.com"), Err(InvalidChars));
        assert_eq!(normalize_custom_domain("a@example.com"), Err(InvalidChars));
        assert_eq!(normalize_custom_domain("ex ample.com"), Err(InvalidChars));
        assert_eq!(normalize_custom_domain("café.example"), Err(InvalidChars));
        // IP literals.
        assert_eq!(normalize_custom_domain("192.168.1.1"), Err(IpLiteral));
        assert_eq!(normalize_custom_domain("[::1]"), Err(InvalidChars));
        // Not an FQDN.
        assert_eq!(normalize_custom_domain("localhost"), Err(NotFqdn));
        assert_eq!(normalize_custom_domain("com"), Err(NotFqdn));
        assert_eq!(normalize_custom_domain("a..b.com"), Err(NotFqdn));
        assert_eq!(normalize_custom_domain("-bad.example.com"), Err(NotFqdn));
        assert_eq!(normalize_custom_domain("bad-.example.com"), Err(NotFqdn));
    }

    #[test]
    fn percent_decode_decodes_hex_escapes() {
        assert_eq!(percent_decode("a%23b", false), "a#b");
        assert_eq!(
            percent_decode("did:web:api.bsky.app%23bsky_appview", false),
            "did:web:api.bsky.app#bsky_appview"
        );
    }

    #[test]
    fn percent_decode_plus_as_space_is_opt_in() {
        assert_eq!(percent_decode("a+b", true), "a b");
        assert_eq!(percent_decode("a+b", false), "a+b");
    }

    #[test]
    fn percent_decode_passes_through_incomplete_or_invalid_escapes() {
        assert_eq!(percent_decode("100%", false), "100%");
        assert_eq!(percent_decode("100%2", false), "100%2");
        assert_eq!(percent_decode("100%zz", false), "100%zz");
    }

    #[test]
    fn percent_decode_scoped_ids() {
        assert_eq!(percent_decode("post-card%5B2%5D", true), "post-card[2]");
    }
}
