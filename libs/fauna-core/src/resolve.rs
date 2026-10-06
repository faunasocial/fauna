use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[cfg(feature = "network")]
use hickory_resolver::TokioResolver;
// hickory 0.26: the typed `TxtLookup`/`SrvLookup` wrappers are gone — convenience
// lookups return a generic `Lookup`, so records come via `answers()` and the rdata
// is extracted by matching the `RData` enum.
#[cfg(feature = "network")]
use hickory_resolver::proto::rr::RData;

/// Parsed form of a handle input.
#[derive(Debug, PartialEq, Eq)]
pub enum HandleForm {
    /// `@domain.tld` — bare-domain handle (exactly 2 labels after @)
    BareDomain(String),
    /// `@user.domain.tld` — might be subdomain handle (3+ labels after @)
    PossibleSubdomain {
        user: String,
        parent: String,
        full_domain: String,
    },
    /// `user@domain` — email-style named handle
    Named { user: String, domain: String },
    /// Unparseable
    Invalid,
}

/// Resolved handle result.
#[derive(Debug, PartialEq, Eq)]
pub struct ResolvedHandle {
    pub user: Option<String>,
    pub domain: String,
    pub actor_id: String,
}

/// Parse any handle format into a `HandleForm`.
///
/// - `@domain.tld` (2 labels) → `BareDomain`
/// - `@user.domain.tld` (3+ labels) → `PossibleSubdomain`
/// - `user@domain` → `Named`
/// - Everything else → `Invalid`
///
/// A parsed **domain never contains `@`**. A handle carrying a second one
/// (`a@b@c`) is `Invalid`, not silently re-split: splitting on the first `@`
/// would yield the domain `b@c`, which a URL parser reads as userinfo `b@` plus
/// host `c` — so the user sees `a@b@c` and the client dials `c`. That is the
/// display-vs-dial divergence `docs/goal/architecture/security.md` § Transport
/// trust forbids ("the authority is the host a URL parser would dial — userinfo
/// is never part of it"; [`crate::web::strip_userinfo`] splits on the *last*
/// `@` per WHATWG, which is what makes the mismatch exploitable). Splitting on
/// the first `@` is otherwise correct here and stays: a handle's `user@domain`
/// boundary is genuinely the first one.
pub fn parse_handle(input: &str) -> HandleForm {
    if let Some(at_domain) = input.strip_prefix('@') {
        // @-prefixed form. A residual `@` puts one inside the domain — reject.
        if at_domain.contains('@') {
            return HandleForm::Invalid;
        }
        let labels: Vec<&str> = at_domain.split('.').collect();
        match labels.len() {
            0 | 1 => HandleForm::Invalid,
            2 => HandleForm::BareDomain(at_domain.to_string()),
            _ => {
                let user = labels[0].to_string();
                let parent = labels[1..].join(".");
                let full_domain = at_domain.to_string();
                HandleForm::PossibleSubdomain {
                    user,
                    parent,
                    full_domain,
                }
            }
        }
    } else if let Some(at_pos) = input.find('@') {
        // email-style: user@domain
        let user = input[..at_pos].to_string();
        let domain = input[at_pos + 1..].to_string();
        // `domain.contains('@')` ⇒ a second `@` — see the display-vs-dial note
        // on this fn.
        if user.is_empty() || domain.is_empty() || domain.contains('@') {
            HandleForm::Invalid
        } else {
            HandleForm::Named { user, domain }
        }
    } else {
        HandleForm::Invalid
    }
}

/// Whether `input` looks like a raw 64-character hex actor ID (the wire form an
/// actor is addressed by directly, no DNS resolution needed). Case-insensitive.
pub fn is_actor_id(input: &str) -> bool {
    crate::hex32::is_hex64(input)
}

/// A "compose / find-user" recipient input, classified for the apps' recipient
/// fields. The single shared shape web (`resolve.ts`), android (`ResolveService`),
/// and linux (`contacts/find.rs`) render from, instead of each re-deriving the
/// actor-id check + handle split (priority #2/#4).
///
/// The *classification* is shared; the subsequent resolution of a [`RecipientInput::Handle`]
/// to an actor (DNS TXT natively, or the nest's `/api/v1/resolve-node` + `/by-handle`
/// HTTP proxy on web) stays platform-specific I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipientInput {
    /// A raw 64-hex actor ID, normalized to lowercase — addressable directly.
    ActorId(String),
    /// An email-style `user@domain` handle that needs resolution.
    Handle { user: String, domain: String },
    /// Not a recognizable recipient (empty, malformed, or a bare-domain/subdomain
    /// `@…` form that the compose path doesn't resolve today).
    Invalid,
}

impl RecipientInput {
    /// Flat `[kind, actor_id, user, domain]` projection for the wasm + UniFFI faces,
    /// which return built-in types rather than this enum (web/android/apple/windows
    /// each re-key the slots into a named object in their thin wrapper; native linux
    /// matches the enum directly). `kind` is `"actor_id"` | `"handle"` | `"invalid"`;
    /// the slots not carried by that kind are empty strings. The positional contract
    /// lives here so both FFI crates project it identically.
    pub fn into_parts(self) -> Vec<String> {
        match self {
            RecipientInput::ActorId(id) => {
                vec!["actor_id".into(), id, String::new(), String::new()]
            }
            RecipientInput::Handle { user, domain } => {
                vec!["handle".into(), String::new(), user, domain]
            }
            RecipientInput::Invalid => vec![
                "invalid".into(),
                String::new(),
                String::new(),
                String::new(),
            ],
        }
    }
}

/// Classify a recipient input string typed into a "compose / find-user" field.
///
/// - A 64-hex actor ID → [`RecipientInput::ActorId`] (lowercased).
/// - An email-style `user@domain` ([`HandleForm::Named`], via [`parse_handle`] — so the
///   LAN-IP / mDNS / no-dot cases the canonical parser accepts all classify uniformly) →
///   [`RecipientInput::Handle`].
/// - Everything else (empty, bare-domain `@domain`, subdomain `@user.domain`, malformed)
///   → [`RecipientInput::Invalid`]. The `@…`-prefixed forms aren't resolvable by the
///   compose path on any client yet, so they're rejected here rather than half-supported.
pub fn classify_recipient(input: &str) -> RecipientInput {
    let trimmed = input.trim();
    if is_actor_id(trimmed) {
        return RecipientInput::ActorId(trimmed.to_ascii_lowercase());
    }
    match parse_handle(trimmed) {
        HandleForm::Named { user, domain } => RecipientInput::Handle { user, domain },
        _ => RecipientInput::Invalid,
    }
}

/// Parse a typed address as a **Fauna handle**: a bare localpart (`alice`) or a
/// `localpart@domain` (`alice@nest.test`). Returns `(localpart, domain)` with
/// `domain == None` for the bare form. `None` for the shapes that belong to
/// other rails — ActivityPub (`@user@instance`), Nostr (`npub1…`), Bluesky
/// (`did:…`) — and for empty / whitespace-bearing junk.
///
/// The one parser behind every surface that takes a Fauna handle and then
/// decides same-nest-vs-cross-nest with [`is_foreign_handle_domain`]: the
/// conversations recipient picker (`FaunaMlsBackend::resolve_address`) and the
/// public-folder follow (`fauna_client_folders::follow_ops`). It differs from
/// [`classify_recipient`] in accepting the bare form — a bare handle is by
/// definition addressed on the nest the client is logged into, which is exactly
/// the same-nest arm those surfaces need to keep.
pub fn parse_fauna_handle(raw: &str) -> Option<(String, Option<String>)> {
    let t = raw.trim();
    if t.is_empty()
        || t.contains(char::is_whitespace)
        || t.starts_with('@')
        || t.starts_with("npub1")
        || t.starts_with("did:")
    {
        return None;
    }
    match t.split_once('@') {
        None => Some((t.to_string(), None)),
        Some((local, domain))
            if !local.is_empty() && !domain.is_empty() && !domain.contains('@') =>
        {
            Some((local.to_string(), Some(domain.to_string())))
        }
        _ => None,
    }
}

/// Is a typed recipient handle's `@domain` **foreign** — served by a nest other
/// than the caller's own home nest?
///
/// The one owner of the same-nest-vs-cross-nest decision every addressing
/// surface makes (priorities #1/#3): the conversations recipient picker
/// (`fauna_conversations::backends::fauna_mls::FaunaMlsBackend::resolve_address`,
/// which routes a `true` to `resolve_foreign`) and the contacts Find User /
/// knock send (which routes a `true` to a peer `recipient_nest_url` on
/// `fauna.inbox.send`) both read the verdict here rather than each re-deriving
/// it — a per-surface copy is how `bob@other.test` comes to mean two different
/// actors on two pages of the same app.
///
/// `typed_domain` is the `@domain` the user actually typed (the
/// [`RecipientInput::Handle`] `domain` slot); `home_domain` is the handle
/// domain the caller's OWN nest reports for itself — in practice the `domain`
/// field echoed by a same-nest `fauna.actor.by_handle` reply, which is why the
/// decision is normally made with that reply already in hand.
///
/// The three cases, in the order they bite:
///
/// - **No typed domain** (a bare `alice`, or a 64-hex actor id that never got
///   here) → never foreign. A bare handle is by definition addressed on the
///   nest the client is logged into.
/// - **Typed domain, home domain known** → foreign iff they differ, compared
///   ASCII-case-insensitively (DNS labels are case-insensitive, and the two
///   strings come from different sources — one typed by a human, one echoed by
///   a nest — so a case difference is noise, not a distinction).
/// - **Typed domain, home domain unknown** (the same-nest probe failed, or
///   answered "no such handle" and so volunteered no domain) → foreign. The
///   peer may well be up while our own answer is missing, so the typed domain
///   gets its own probe rather than a silent same-nest fallback that would
///   resolve `bob@other.test` to a *local* `bob`. A typed domain that turns out
///   to be our own costs one redundant hop to ourselves and reports the same
///   answer — the cheap side of the trade.
///
/// Note the asymmetry this rule encodes deliberately: a local handle of the
/// same localpart under a different domain is a **distinct actor**
/// (`bob@other.test` ≠ the local `bob`), never a synonym.
pub fn is_foreign_handle_domain(typed_domain: Option<&str>, home_domain: Option<&str>) -> bool {
    match (typed_domain, home_domain) {
        (Some(typed), Some(home)) => !typed.eq_ignore_ascii_case(home),
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// Find `key=value` in a slice of TXT record strings and return the value part.
pub fn extract_txt_value<'a>(values: &'a [String], key: &str) -> Option<&'a str> {
    let prefix = format!("{}=", key);
    for v in values {
        if let Some(val) = v.strip_prefix(&prefix) {
            return Some(val);
        }
    }
    None
}

/// Check for the exact `key=expected` pair in a slice of TXT record strings.
pub fn has_txt_flag(values: &[String], key: &str, expected: &str) -> bool {
    let target = format!("{}={}", key, expected);
    values.iter().any(|v| v == &target)
}

/// Query `_fauna.{domain}` TXT records.
///
/// Returns an empty vec on any error (including DNS NXDOMAIN or resolver init failure).
#[cfg(feature = "network")]
pub async fn lookup_fauna_txt(domain: &str) -> Vec<String> {
    let txt_name = format!("_fauna.{}", domain);

    let resolver = match TokioResolver::builder_tokio().and_then(|b| b.build()) {
        Ok(resolver) => resolver,
        Err(_) => return vec![],
    };

    match resolver.txt_lookup(&txt_name).await {
        Ok(lookup) => lookup
            .answers()
            .iter()
            .filter_map(|record| match &record.data {
                RData::TXT(txt) => Some(txt),
                _ => None,
            })
            .flat_map(|txt| txt.txt_data.iter())
            .filter_map(|bytes| {
                std::str::from_utf8(bytes.as_ref())
                    .ok()
                    .map(|s| s.to_string())
            })
            .collect(),
        Err(_) => vec![],
    }
}

/// Resolve a handle string to a `ResolvedHandle`.
///
/// Resolution strategy per form:
/// - `BareDomain`: lookup `_fauna.{domain}`, require `self=` or `cache=`, extract `self=` as actor_id.
/// - `PossibleSubdomain`: try `_fauna.{full_domain}` first — if has `self=` or `cache=`, treat as
///   bare-domain. Otherwise check `_fauna.{parent}` for `subhandles=true`, then lookup
///   `_fauna.{user}.{parent}` for `id=`.
/// - `Named`: lookup `_fauna.{user}.{domain}` for `id=`.
/// - `Invalid`: return `None`.
#[cfg(feature = "network")]
pub async fn resolve_handle(input: &str) -> Option<ResolvedHandle> {
    match parse_handle(input) {
        HandleForm::BareDomain(domain) => {
            let txt = lookup_fauna_txt(&domain).await;
            if extract_txt_value(&txt, "self").is_none()
                && extract_txt_value(&txt, "cache").is_none()
            {
                return None;
            }
            let actor_id = extract_txt_value(&txt, "self")?.to_string();
            Some(ResolvedHandle {
                user: None,
                domain,
                actor_id,
            })
        }
        HandleForm::PossibleSubdomain {
            user,
            parent,
            full_domain,
        } => {
            // Try full_domain first — if it looks like a bare-domain node, handle it that way.
            let full_txt = lookup_fauna_txt(&full_domain).await;
            if extract_txt_value(&full_txt, "self").is_some()
                || extract_txt_value(&full_txt, "cache").is_some()
            {
                let actor_id = extract_txt_value(&full_txt, "self")?.to_string();
                return Some(ResolvedHandle {
                    user: None,
                    domain: full_domain,
                    actor_id,
                });
            }
            // Check parent for subhandles=true.
            let parent_txt = lookup_fauna_txt(&parent).await;
            if !has_txt_flag(&parent_txt, "subhandles", "true") {
                return None;
            }
            // Lookup the user's personal TXT record.
            let user_domain = format!("{}.{}", user, parent);
            let user_txt = lookup_fauna_txt(&user_domain).await;
            let actor_id = extract_txt_value(&user_txt, "id")?.to_string();
            Some(ResolvedHandle {
                user: Some(user),
                domain: parent,
                actor_id,
            })
        }
        HandleForm::Named { user, domain } => {
            let user_domain = format!("{}.{}", user, domain);
            let txt = lookup_fauna_txt(&user_domain).await;
            let actor_id = extract_txt_value(&txt, "id")?.to_string();
            Some(ResolvedHandle {
                user: Some(user),
                domain,
                actor_id,
            })
        }
        HandleForm::Invalid => None,
    }
}

/// Resolve a fauna node domain to (host, port) using SRV records.
///
/// Looks up `_fauna._tcp.<domain>` SRV record. If found, uses the port
/// from the SRV record. If not found, falls back to port 443.
#[cfg(feature = "network")]
pub async fn resolve_node_url(domain: &str) -> (String, u16) {
    let srv_name = format!("_fauna._tcp.{}", domain);

    let resolver = match TokioResolver::builder_tokio().and_then(|b| b.build()) {
        Ok(resolver) => resolver,
        Err(_) => return (domain.to_string(), 443),
    };

    match resolver.srv_lookup(&srv_name).await {
        Ok(lookup) => {
            if let Some(srv) = lookup
                .answers()
                .iter()
                .find_map(|record| match &record.data {
                    RData::SRV(srv) => Some(srv),
                    _ => None,
                })
            {
                let target = srv.target.to_string();
                let host = target.trim_end_matches('.');
                return (host.to_string(), srv.port);
            }
            (domain.to_string(), 443)
        }
        Err(_) => (domain.to_string(), 443),
    }
}

/// Parse a node URL into its `(host, port)`, where `None` means the URL carries
/// no explicit port — the signal that an SRV lookup supplies it.
///
/// The port was a `u16` with `0` standing in for "absent" until 2026-08-01. The
/// sentinel was never a real port (a caller reaching for `:0` cannot dial it),
/// but it forced every consumer to remember the convention, and it made the
/// absent case indistinguishable from a literal `:0` — so a `https://host:0`
/// silently took the SRV path as if no port had been typed. `Option<u16>` is
/// what [`crate::web::split_host_port`] already returns, so the sentinel only
/// existed to be re-encoded here and decoded at each call site.
///
/// The authority is split by the shared string family, in order:
/// [`crate::web::generic_authority`] — any `scheme://`, ending at the first of
/// the four WHATWG authority terminators `/ \ ? #` — then
/// [`crate::web::strip_userinfo`] — userinfo names a credential, never a host
/// (`docs/goal/architecture/security.md` § Transport trust) — then
/// [`crate::web::split_host_port`], where only a valid `u16` suffix counts as a
/// port and a bare IPv6 literal's own colons never do.
///
/// **Joined the `generic_authority` family 2026-09-13**:
/// this parser used to strip `https`/`http` only and end the authority at `/`
/// alone, so a `?`/`#`-bearing nest URL fed the SRV reconnect self-heal,
/// `fauna_client_dns`'s dial classification, the IMAP/SMTP/CalDAV endpoint
/// display, and `report_host_address`'s public-IP report a host the dialer
/// never actually reached.
///
/// **The IPv6 brackets are kept** (`[::1]`, not `::1`): the family's third
/// member, [`strip_ipv6_brackets`], is deliberately *not* applied here. Every
/// consumer either reassembles a URL authority, where the brackets are required
/// ([`resolve_full_url`], [`srv_recovered_url`], the onboarding SRV probe, and
/// `fauna_client_mail_settings`'s WebDAV URL), or strips them itself before an
/// `IpAddr` parse ([`is_public_dns_name`],
/// `fauna_client_dns::host_address::classify_dial_host`). Stripping here would
/// corrupt the first group to serve a second group that already composes the
/// primitive — the same split `split_host_port` resolves by keeping brackets.
///
/// Before this routed through the family it split the raw authority on its last
/// `:`, which read `https://user:pass@host` as host `user` and left a
/// port-hidden `https://[::1]` as the fragment `"[:"` — both of which then
/// classified as *public DNS names*, so a loopback nest issued public SRV
/// queries and every app advertised `mail.[:` as the user's IMAP host.
pub fn parse_node_address(url: &str) -> (String, Option<u16>) {
    let authority = crate::web::generic_authority(url);
    let (host, port) = crate::web::split_host_port(crate::web::strip_userinfo(authority));
    (host.to_string(), port)
}

/// Qualify a signed-in session's handle with the nest it is homed on:
/// `("alice", "https://fauna.social")` → `"alice@fauna.social"`.
///
/// Apps hold the handle as the **bare local part** — that is what the nest
/// stores and what every `handle`-taking kind expects — which is fine right up
/// until something has to be readable *off* the device: a recovery kit's
/// `handle=` payload ([`crate::recovery::RecoveryKitQr::to_uri`]), where the
/// `@domain` is the only thing that can later locate the home nest, since the
/// restore that reads it is pre-identity and has no session to ask.
///
/// A non-default port is kept (`"alice@localhost:8443"`) — the exact form the
/// onboarding handle field accepts, so the value round-trips through the same
/// resolution path a typed handle takes. A handle that already carries an `@`
/// passes through unchanged; an empty handle or an unparseable URL yields
/// `None` rather than a half-formed address.
pub fn qualify_handle(handle: &str, node_url: &str) -> Option<String> {
    let handle = handle.trim();
    if handle.is_empty() {
        return None;
    }
    if handle.contains('@') {
        return Some(handle.to_string());
    }
    let (host, port) = parse_node_address(node_url);
    if host.is_empty() {
        return None;
    }
    Some(match port {
        Some(p) => format!("{handle}@{host}:{p}"),
        None => format!("{handle}@{host}"),
    })
}

/// Strip the brackets off a bracketed IPv6 literal (`[::1]` → `::1`); any
/// other input (a bare IPv4/IPv6 literal, a DNS name) passes through
/// unchanged. The shared first step before parsing a `host` string that may
/// carry RFC 3986 IPv6 bracketing as `IpAddr`.
pub fn strip_ipv6_brackets(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
}

/// Returns true iff `host` is a name resolved via **public unicast DNS** —
/// i.e. NOT a loopback name (`localhost`/`*.localhost`), NOT an mDNS `.local`
/// name, and NOT an IP literal (bracketed or bare). This is the form test the
/// onboarding probes, ACME orderability, mail-enable defaults, and MUA
/// endpoint display all branch on: a host failing it has no registrable
/// public name, no CA-orderable certificate, and no unicast-DNS records to
/// check — it is reached by its raw locator, served with the nest's
/// self-signed floor cert (authenticated by channel-binding for Fauna
/// apps, TOFU-on-cert for third-party MUAs), and is **not**
/// auto-registered as a mail domain (so `mail.<host>` is nonsense that breaks
/// mail + CalDAV host routing).
///
/// It says nothing about *registrability* of the TLD (`foo.example` is a
/// public DNS name with an unregistrable TLD — `tld_is_valid_for_registration`
/// owns that axis) and nothing about IP routability ([`is_global_ip`] owns
/// that axis).
///
/// This is the **single source of truth** for the local-target carve-out
/// shared by nest provisioning (`fauna_provisioning::probe::
/// resolve_handle_domain`'s `is_public_dns_name`) and client MUA-endpoint
/// display (`fauna_client_mail_settings`'s `MuaInstructions::for_node_url`).
/// `host` is a bare host with no port; a bracketed IPv6 literal (`[::1]`) is
/// accepted. Authority: `docs/goal/architecture/nest/domains-and-tls-bootstrap.md`
/// § "by handle, no domain auto-registered" (the local-target carve-out).
pub fn is_public_dns_name(host: &str) -> bool {
    let unbracketed = strip_ipv6_brackets(host);
    let lower = host.to_ascii_lowercase();
    let is_loopback_name = host.eq_ignore_ascii_case("localhost") || lower.ends_with(".localhost");
    // A `.local` mDNS name (`pi.local`) is a self-hosted LAN target resolved by
    // OS-level multicast DNS at connect time — not a registrable domain. The
    // bare label `local` (no dot) is *not* an mDNS name.
    let is_mdns_local = lower.ends_with(".local");
    let is_ip_literal = unbracketed.parse::<std::net::IpAddr>().is_ok();
    !(is_loopback_name || is_mdns_local || is_ip_literal)
}

/// Returns true iff `host` looks like a **private-network** (not
/// internet-reachable) target: a loopback/`.localhost` name, an mDNS `.local`
/// name, or an IP literal that is not globally routable (`!`[`is_global_ip`] —
/// RFC 1918, loopback, link-local, CGNAT, ULA, …). Unlike
/// `!is_public_dns_name`, a *public* bare IP does NOT qualify — this is the
/// reachability-flavored classifier, used to refine the NAT-mode
/// pre-selection private-ward during onboarding
/// (`docs/goal/behavior/onboarding.md` § 3b-bis Defaulting note): a `public`
/// seed + a private-network target pre-selects `private`. Advisory only — the
/// human on the page decides.
pub fn is_private_network_target(host: &str) -> bool {
    let unbracketed = strip_ipv6_brackets(host);
    if let Ok(ip) = unbracketed.parse::<std::net::IpAddr>() {
        return !is_global_ip(ip);
    }
    let lower = host.to_ascii_lowercase();
    host.eq_ignore_ascii_case("localhost")
        || lower.ends_with(".localhost")
        || lower.ends_with(".local")
}

/// Returns `true` iff `ip` is a *globally-routable* public address — i.e. NOT
/// loopback, private (RFC1918), link-local (incl. `169.254.0.0/16` cloud IMDS),
/// CGNAT (`100.64.0.0/10`), unspecified, "this network" (`0.0.0.0/8`),
/// documentation, benchmarking, reserved, broadcast, multicast, IPv6
/// unique-local (`fc00::/7`) / link-local (`fe80::/10`), or an
/// IPv4-mapped / IPv4-compatible / NAT64 IPv6 wrapper of any of the above.
///
/// Pure (no `network` feature, no unstable std `IpAddr::is_global`, so it
/// survives toolchain-pin bumps and compiles on wasm). Three consumers share
/// this one classifier: the nest's SSRF outbound-fetch guard (`bins/fauna-nest`'s
/// `ssrf::resolve_global_addrs`, which rejects a non-global target), the Nostr
/// relay dialer (`fauna_bridge_nostr::relay_client::RelayDialPolicy`, the same
/// refusal over every outbound relay dial — `nest/network-exposure.md`
/// § Rulings F7), and the client host-address reporter
/// (`fauna_client_dns::host_address`, which reports a *global* dial-address as
/// the nest's public IP and never publishes a non-global one — see
/// `docs/goal/architecture/nest/domains-and-tls-bootstrap.md`
/// § Host-address acquisition). It is the exact inverse of "is this a LAN /
/// non-routable address", complementing [`is_public_dns_name`] (whose *false*
/// side also covers the `.local` / `localhost` *name* forms an IP classifier
/// can't see).
pub fn is_global_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_global_ipv4(v4),
        IpAddr::V6(v6) => is_global_ipv6(v6),
    }
}

fn is_global_ipv4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    // CGNAT 100.64.0.0/10 (100.64.0.0 .. 100.127.255.255).
    let is_cgnat = o[0] == 100 && (o[1] & 0xc0) == 0x40;
    // Benchmarking 198.18.0.0/15.
    let is_benchmarking = o[0] == 198 && (o[1] == 18 || o[1] == 19);
    // IETF protocol assignments 192.0.0.0/24 (includes 192.0.0.0/29 etc.).
    let is_ietf_protocol = o[0] == 192 && o[1] == 0 && o[2] == 0;
    // "This network" 0.0.0.0/8.
    let is_this_network = o[0] == 0;
    // Reserved 240.0.0.0/4 (covers 255.255.255.255 too, but be explicit).
    let is_reserved = o[0] >= 240;

    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || is_cgnat
        || is_benchmarking
        || is_ietf_protocol
        || is_this_network
        || is_reserved)
}

fn is_global_ipv6(ip: Ipv6Addr) -> bool {
    // IPv4-mapped `::ffff:a.b.c.d` — the canonical SSRF wrapper.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_global_ipv4(v4);
    }
    let seg = ip.segments();
    // IPv4-compatible `::a.b.c.d` (deprecated) — also embeds a v4 dest, and
    // covers `::` (0.0.0.0) and `::1` (loopback) which fail the v4 check.
    if seg[..6].iter().all(|&s| s == 0) {
        let v4 = Ipv4Addr::new(
            (seg[6] >> 8) as u8,
            (seg[6] & 0xff) as u8,
            (seg[7] >> 8) as u8,
            (seg[7] & 0xff) as u8,
        );
        return is_global_ipv4(v4);
    }
    // NAT64 well-known prefix 64:ff9b::/96 embeds an IPv4 destination.
    if seg[0] == 0x0064 && seg[1] == 0xff9b && seg[2..6].iter().all(|&s| s == 0) {
        let v4 = Ipv4Addr::new(
            (seg[6] >> 8) as u8,
            (seg[6] & 0xff) as u8,
            (seg[7] >> 8) as u8,
            (seg[7] & 0xff) as u8,
        );
        return is_global_ipv4(v4);
    }

    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (seg[0] & 0xfe00) == 0xfc00            // fc00::/7  unique local
        || (seg[0] & 0xffc0) == 0xfe80            // fe80::/10 link-local unicast
        || (seg[0] == 0x2001 && seg[1] == 0x0db8) // 2001:db8::/32 documentation
        || (seg[0] == 0x2001 && seg[1] == 0x0002 && seg[2] == 0)) // 2001:2::/48 benchmarking
}

/// Why [`resolve_permitted_addrs`] refused a host.
#[cfg(feature = "network")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AddrGuardError {
    /// The name did not resolve, or resolved to nothing.
    #[error("could not resolve host")]
    Resolve,
    /// At least one address the host names failed the caller's predicate.
    #[error("address is not permitted")]
    NotPermitted,
}

/// The **resolving half** of every outbound-dial guard: resolve `host:port`
/// and refuse unless `permit` accepts **every** address the name answers with,
/// returning the verified [`std::net::SocketAddr`]s so the caller can **pin**
/// them for the connection. Re-resolving after the check would re-open the
/// DNS-rebinding window (a name that answers "public" for the check and
/// "internal" for the connection), so a caller connects to what this returned
/// and nothing else. An IP literal — bare or bracketed IPv6 — is verified
/// directly: the literal *is* the connection target, so there is no DNS and no
/// rebinding window. A name answering a mix of permitted and refused addresses
/// is refused whole.
///
/// The predicate is the caller's policy; the resolver is shared so the nest's
/// SSRF guard (`bins/fauna-nest`'s `ssrf::resolve_global_addrs`, predicate
/// [`is_global_ip`]) and the Nostr relay dialer
/// (`fauna_bridge_nostr::relay_client::RelayDialPolicy`, the same predicate
/// plus a test-only loopback allowance) cannot drift on the mechanism that
/// pins what they verified.
#[cfg(feature = "network")]
pub async fn resolve_permitted_addrs(
    host: &str,
    port: u16,
    permit: impl Fn(IpAddr) -> bool,
) -> Result<Vec<std::net::SocketAddr>, AddrGuardError> {
    use std::net::SocketAddr;
    if let Ok(ip) = strip_ipv6_brackets(host).parse::<IpAddr>() {
        if !permit(ip) {
            return Err(AddrGuardError::NotPermitted);
        }
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| AddrGuardError::Resolve)?
        .collect();
    if addrs.is_empty() {
        return Err(AddrGuardError::Resolve);
    }
    if addrs.iter().any(|a| !permit(a.ip())) {
        return Err(AddrGuardError::NotPermitted);
    }
    Ok(addrs)
}

/// Resolve a full node URL, using SRV lookup if no explicit port is present.
/// Returns a URL with the resolved port.
#[cfg(feature = "network")]
pub async fn resolve_full_url(node_url: &str) -> String {
    let (host, port) = parse_node_address(node_url);
    if port.is_some() {
        return node_url.to_string();
    }

    let (_resolved_host, resolved_port) = resolve_node_url(&host).await;
    if resolved_port == 443 {
        node_url.to_string()
    } else {
        let scheme = if node_url.starts_with("http://") {
            "http"
        } else {
            "https"
        };
        let path = node_url
            .strip_prefix(&format!("{}://{}", scheme, host))
            .unwrap_or("");
        format!("{}://{}:{}{}", scheme, host, resolved_port, path)
    }
}

/// The host whose `_fauna._tcp` SRV a client **reconnect** loop should
/// re-resolve to recover a `current_url` that just failed to connect, or
/// `None` when the host is a **local target** (loopback / IP literal /
/// `.local`) with no public SRV zone — in which case the loop must NOT issue a
/// pointless DNS / DoH query and falls through to its normal backoff (a local
/// nest is reached by same-box injection / manual re-entry, never SRV; see
/// `docs/goal/architecture/nest/common.md` § Serving ports). The caller passes
/// the returned host to its platform resolver ([`resolve_node_url`] on native /
/// `fauna_provisioning::probe::fauna_srv_port` DoH on wasm), then feeds the
/// looked-up port to [`srv_recovered_url`].
///
/// Pure (no `network` feature) so both the native and the wasm reconnect loops
/// share one source of truth for *which* endpoints are SRV-recoverable.
pub fn srv_recovery_host(current_url: &str) -> Option<String> {
    let (host, _port) = parse_node_address(current_url);
    if !is_public_dns_name(&host) {
        None
    } else {
        Some(host)
    }
}

/// Given the `current_url` that just failed to connect and the port its
/// `_fauna._tcp` SRV advertises — `Some(port)` when a record is present,
/// **`None` when the record is absent or the lookup errored** — return the
/// recovered `https://host:port` URL, or `None` when there is nothing to heal.
///
/// The `None` srv-port case maps to "no change": a reconnect must NOT rewrite a
/// working explicit-port URL just because an SRV lookup transiently failed or
/// the record is gone (we have no new port to move to). Only a *present* SRV
/// port that *differs* from the current one heals the URL. This is why the
/// resolver twins ([`fauna_srv_port`] native / `fauna_provisioning::probe::
/// fauna_srv_port` DoH) return `Option<u16>` rather than the 443-on-absent
/// [`resolve_node_url`].
///
/// This is the **reconnect** twin of [`resolve_full_url`], and differs from it
/// in one load-bearing way: `resolve_full_url` treats an explicit port in the
/// URL as the user's own override and returns it untouched (onboarding
/// semantics), so it can never heal a *second* serving-port change once a URL
/// already carries an explicit port. A reconnect's current port is, by
/// definition, the last-known port that just *failed* — never a user override —
/// so this helper always treats it as stale and honors the freshly-resolved SRV
/// port. A port-hidden URL (`https://host`, no port) counts as `443`. The host
/// is preserved (SRV supplies only the port, mirroring `resolve_full_url`); the
/// scheme is always `https` (a public nest serves TLS on every entry path).
/// Pure (no `network`) — shared by the native + wasm reconnect loops.
pub fn srv_recovered_url(current_url: &str, srv_port: Option<u16>) -> Option<String> {
    let srv_port = srv_port?;
    let (host, current_port) = parse_node_address(current_url);
    let effective_current = current_port.unwrap_or(443);
    if srv_port == effective_current {
        return None;
    }
    Some(format!("https://{host}:{srv_port}"))
}

/// Look up `_fauna._tcp.<domain>` SRV and return the advertised client-facing
/// port, or `None` on NXDOMAIN / no SRV answer / resolver error. The native
/// (hickory) twin of the DoH `fauna_provisioning::probe::fauna_srv_port` (which
/// the wasm web app uses) — used by the **reconnect** self-heal
/// ([`crate::resolve::srv_recovered_url`]), which must distinguish "no record →
/// don't touch the URL" from a present port, unlike [`resolve_node_url`] (which
/// flattens absent → `443`). Returns only the port; the caller keeps the
/// handle's host.
#[cfg(feature = "network")]
pub async fn fauna_srv_port(domain: &str) -> Option<u16> {
    let srv_name = format!("_fauna._tcp.{}", domain);
    let resolver = TokioResolver::builder_tokio().ok()?.build().ok()?;
    let lookup = resolver.srv_lookup(&srv_name).await.ok()?;
    lookup
        .answers()
        .iter()
        .find_map(|record| match &record.data {
            RData::SRV(srv) => Some(srv.port),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_url_with_port() {
        let (host, port) = parse_node_address("https://example.com:8443/api");
        assert_eq!(host, "example.com");
        assert_eq!(port, Some(8443));
    }

    #[test]
    fn parse_url_without_port() {
        let (host, port) = parse_node_address("https://example.com/api");
        assert_eq!(host, "example.com");
        assert_eq!(port, None);
    }

    #[test]
    fn parse_url_no_scheme() {
        let (host, port) = parse_node_address("example.com:9000");
        assert_eq!(host, "example.com");
        assert_eq!(port, Some(9000));
    }

    #[test]
    fn parse_url_no_scheme_no_port() {
        let (host, port) = parse_node_address("example.com");
        assert_eq!(host, "example.com");
        assert_eq!(port, None);
    }

    #[test]
    fn parse_node_address_reads_the_host_not_the_userinfo() {
        // Userinfo names a credential, never a host — `web::strip_userinfo`.
        // Splitting the raw authority on its last `:` read `user` as the host
        // and the rest as an unparseable port, so a reconnect queried
        // `_fauna._tcp.user` and the MUA block advertised `mail.user`.
        let (host, port) = parse_node_address("https://user:pass@nest.example.com");
        assert_eq!(host, "nest.example.com");
        assert_eq!(port, None);

        let (host, port) = parse_node_address("https://user:pass@nest.example.com:8443/api");
        assert_eq!(host, "nest.example.com");
        assert_eq!(port, Some(8443));
    }

    #[test]
    fn parse_node_address_keeps_a_port_hidden_ipv6_literal_whole() {
        // The port-*hidden* form is the one that broke: the last `:` of a bare
        // `[::1]` sits *inside* the literal, so the host came back as `"[:"`.
        // (`[::1]:8443` happened to survive — its last `:` is the port
        // separator, after the `]`.) Brackets are kept: every consumer either
        // reassembles a URL authority (which requires them) or strips them
        // itself via `strip_ipv6_brackets`.
        let (host, port) = parse_node_address("https://[::1]");
        assert_eq!(host, "[::1]");
        assert_eq!(port, None);

        let (host, port) = parse_node_address("https://[fd00::1]/api");
        assert_eq!(host, "[fd00::1]");
        assert_eq!(port, None);

        // The already-working explicit-port form must stay working.
        let (host, port) = parse_node_address("https://[::1]:8443");
        assert_eq!(host, "[::1]");
        assert_eq!(port, Some(8443));
    }

    #[test]
    fn parse_node_address_stops_at_every_authority_terminator() {
        // PROBE-613: before joining the `generic_authority` family this
        // parser stopped at `/` alone, so a `?`/`#`-bearing nest URL read
        // past the real host — the same class of bug `authority_of` had
        // . `report_host_address` feeds `dial_url`
        // straight through this, so the mis-parsed host would have been
        // reported as the nest's public address. Shared cases so a
        // narrower `authority_len` reds this alongside the other five
        // callers .
        for &(url, expected_host) in crate::web::AUTHORITY_TERMINATOR_CASES {
            let (host, port) = parse_node_address(url);
            assert_eq!(host, expected_host, "{url}");
            assert_eq!(port, None, "{url}");
        }

        let (host, port) = parse_node_address("https://nest.example.com?x=@127.0.0.1:443/");
        assert_eq!(host, "nest.example.com");
        assert_eq!(port, None);
    }

    #[test]
    fn srv_recovery_host_skips_a_port_hidden_ipv6_loopback() {
        // The consumer-level witness for the case above: a loopback nest has no
        // public `_fauna._tcp` zone, so the reconnect loop must not query. The
        // mangled `"[:"` classified as a *public DNS name*, so it did.
        assert_eq!(srv_recovery_host("https://[::1]"), None);
        assert_eq!(srv_recovery_host("https://[fd00::1]"), None);
    }

    #[test]
    fn srv_recovery_host_queries_the_host_not_the_userinfo() {
        // A userinfo-bearing nest URL must re-resolve the *host*; querying
        // `_fauna._tcp.user` leaked the credential name to the DNS/DoH resolver
        // and could never recover the connection.
        assert_eq!(
            srv_recovery_host("https://user:pass@example.com").as_deref(),
            Some("example.com")
        );
    }

    #[test]
    fn srv_recovered_url_rebuilds_on_the_real_host() {
        // `srv_recovered_url` reassembles `https://{host}:{port}`, so a mangled
        // host is *persisted* as the client's new nest URL.
        assert_eq!(
            srv_recovered_url("https://user:pass@example.com", Some(8443)).as_deref(),
            Some("https://example.com:8443")
        );
        // Brackets must survive the round-trip — an unbracketed `::1` is not a
        // legal URL authority.
        assert_eq!(
            srv_recovered_url("https://[fd00::1]", Some(8443)).as_deref(),
            Some("https://[fd00::1]:8443")
        );
    }

    #[test]
    fn strip_ipv6_brackets_only_strips_bracketed_literals() {
        assert_eq!(strip_ipv6_brackets("[::1]"), "::1");
        assert_eq!(strip_ipv6_brackets("[fd00::1]"), "fd00::1");
        // Unbracketed / non-IPv6 inputs pass through unchanged.
        assert_eq!(strip_ipv6_brackets("::1"), "::1");
        assert_eq!(strip_ipv6_brackets("127.0.0.1"), "127.0.0.1");
        assert_eq!(strip_ipv6_brackets("example.com"), "example.com");
        // A lone bracket on one side only isn't a match — passes through whole.
        assert_eq!(strip_ipv6_brackets("[::1"), "[::1");
        assert_eq!(strip_ipv6_brackets("::1]"), "::1]");
    }

    #[test]
    fn is_public_dns_name_classifies_locators() {
        // IP literals (loopback, private/LAN, public) — never public DNS names.
        assert!(!is_public_dns_name("127.0.0.1"));
        assert!(!is_public_dns_name("192.168.1.50"));
        assert!(!is_public_dns_name("10.1.8.51"));
        assert!(!is_public_dns_name("::1"));
        assert!(!is_public_dns_name("[::1]"));
        assert!(!is_public_dns_name("fd00::1"));
        // localhost / *.localhost / .local mDNS — not public DNS names.
        assert!(!is_public_dns_name("localhost"));
        assert!(!is_public_dns_name("LocalHost"));
        assert!(!is_public_dns_name("foo.localhost"));
        assert!(!is_public_dns_name("pi.local"));
        assert!(!is_public_dns_name("raspberrypi.local"));
        // Registrable public domains ARE public DNS names.
        assert!(is_public_dns_name("example.com"));
        assert!(is_public_dns_name("nest.example.com"));
        // A second, unrelated registrable domain. ⚠ Not `example.com`: the
        // publish scrub rules rewrite it to `example.com`, which would make this
        // a duplicate of the first line in the shipped tree — a silently weaker
        // test rather than a failing one; the `verify_no_scrub_collisions` gate
        // in transform.py enforces this.
        assert!(is_public_dns_name("example.net"));
        // The bare label `local` (no dot) is not an mDNS name.
        assert!(is_public_dns_name("local"));
    }

    #[test]
    fn is_private_network_target_classifies_locators() {
        // Loopback / *.localhost / .local mDNS names.
        assert!(is_private_network_target("localhost"));
        assert!(is_private_network_target("foo.localhost"));
        assert!(is_private_network_target("pi.local"));
        // Non-global IP literals (loopback, RFC 1918, ULA, link-local).
        assert!(is_private_network_target("127.0.0.1"));
        assert!(is_private_network_target("192.168.1.50"));
        assert!(is_private_network_target("10.1.8.51"));
        assert!(is_private_network_target("[::1]"));
        assert!(is_private_network_target("::1"));
        assert!(is_private_network_target("fd00::1"));
        assert!(is_private_network_target("169.254.10.10"));
        // Globally-routable IP literals do NOT qualify.
        assert!(!is_private_network_target("8.8.8.8"));
        assert!(!is_private_network_target("93.184.216.34"));
        assert!(!is_private_network_target("2600::1"));
        // Public DNS names do NOT qualify.
        assert!(!is_private_network_target("example.com"));
        assert!(!is_private_network_target("nest.example.com"));
        // The bare label `local` (no dot) is not an mDNS name.
        assert!(!is_private_network_target("local"));
    }

    // ── globally-routable classifier (shared by nest SSRF guard + client
    //    host-address reporter). Lifted from `bins/fauna-nest/src/ssrf.rs`. ──

    fn v4(s: &str) -> IpAddr {
        IpAddr::V4(s.parse().unwrap())
    }
    fn v6(s: &str) -> IpAddr {
        IpAddr::V6(s.parse().unwrap())
    }

    #[test]
    fn public_ipv4_is_global() {
        assert!(is_global_ip(v4("8.8.8.8")));
        assert!(is_global_ip(v4("1.1.1.1")));
        assert!(is_global_ip(v4("93.184.216.34"))); // example.com
    }

    #[test]
    fn private_and_special_ipv4_rejected() {
        for s in [
            "127.0.0.1",       // loopback
            "10.0.0.5",        // RFC1918
            "172.16.0.1",      // RFC1918
            "192.168.1.1",     // RFC1918
            "169.254.169.254", // cloud IMDS link-local
            "100.64.0.1",      // CGNAT
            "100.127.255.255", // CGNAT upper
            "0.0.0.0",         // this network
            "198.18.0.1",      // benchmarking
            "192.0.0.1",       // IETF protocol
            "192.0.2.5",       // documentation
            "255.255.255.255", // broadcast
            "240.0.0.1",       // reserved
            "224.0.0.1",       // multicast
        ] {
            assert!(!is_global_ip(v4(s)), "{s} must NOT be global");
        }
    }

    #[test]
    fn ipv6_special_rejected() {
        for s in [
            "::1",                      // loopback
            "::",                       // unspecified
            "fc00::1",                  // unique local
            "fd12:3456::1",             // unique local
            "fe80::1",                  // link-local
            "ff02::1",                  // multicast
            "2001:db8::1",              // documentation
            "::ffff:127.0.0.1",         // IPv4-mapped loopback (rebinding wrapper)
            "::ffff:169.254.169.254",   // IPv4-mapped IMDS
            "::ffff:10.0.0.1",          // IPv4-mapped RFC1918
            "::7f00:1",                 // IPv4-compatible ::127.0.0.1
            "64:ff9b::169.254.169.254", // NAT64 -> IMDS
        ] {
            assert!(!is_global_ip(v6(s)), "{s} must NOT be global");
        }
    }

    #[test]
    fn public_ipv6_is_global() {
        assert!(is_global_ip(v6("2606:4700:4700::1111"))); // cloudflare
        assert!(is_global_ip(v6("::ffff:8.8.8.8"))); // mapped public
    }

    #[cfg(feature = "network")]
    #[tokio::test]
    async fn resolve_falls_back_to_443() {
        // example.com has no _fauna._tcp SRV record, so we expect fallback
        let (host, port) = resolve_node_url("example.com").await;
        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
    }

    #[cfg(feature = "network")]
    #[tokio::test]
    async fn resolve_full_url_with_explicit_port() {
        let result = resolve_full_url("https://example.com:8443/api/v1").await;
        assert_eq!(result, "https://example.com:8443/api/v1");
    }

    #[cfg(feature = "network")]
    #[tokio::test]
    async fn resolve_full_url_without_port_falls_back() {
        // No SRV record → port 443 → return original URL unchanged
        let result = resolve_full_url("https://example.com/api/v1").await;
        assert_eq!(result, "https://example.com/api/v1");
    }

    #[test]
    fn parse_handle_at_domain() {
        assert_eq!(
            parse_handle("@fauna.example"),
            HandleForm::BareDomain("fauna.example".into())
        );
    }

    #[test]
    fn parse_handle_at_user_domain() {
        assert_eq!(
            parse_handle("@alice.fauna.example"),
            HandleForm::PossibleSubdomain {
                user: "alice".into(),
                parent: "fauna.example".into(),
                full_domain: "alice.fauna.example".into(),
            }
        );
    }

    #[test]
    fn parse_handle_email_style() {
        assert_eq!(
            parse_handle("alice@fauna.example"),
            HandleForm::Named {
                user: "alice".into(),
                domain: "fauna.example".into(),
            }
        );
    }

    #[test]
    fn parse_handle_bare_tld_rejected() {
        assert_eq!(parse_handle("@example"), HandleForm::Invalid);
    }

    #[test]
    fn parse_handle_email_style_lan_ip() {
        // A self-hosted nest's location IS the handle's @-part. An email-style
        // handle whose domain is a LAN IP literal (optionally with a port) is a
        // valid `Named` handle; the IP is classified at probe time by
        // `fauna_provisioning::probe::resolve_handle_domain`, not here.
        assert_eq!(
            parse_handle("alice@192.168.1.57"),
            HandleForm::Named {
                user: "alice".into(),
                domain: "192.168.1.57".into(),
            }
        );
        assert_eq!(
            parse_handle("alice@192.168.1.57:8443"),
            HandleForm::Named {
                user: "alice".into(),
                domain: "192.168.1.57:8443".into(),
            }
        );
    }

    #[test]
    fn parse_handle_email_style_mdns_local() {
        assert_eq!(
            parse_handle("alice@raspberrypi.local"),
            HandleForm::Named {
                user: "alice".into(),
                domain: "raspberrypi.local".into(),
            }
        );
    }

    #[test]
    fn test_extract_txt_value() {
        let values = vec![
            "cache=https://nest.fauna.example/api/v1".to_string(),
            "self=abcdef".to_string(),
            "subhandles=true".to_string(),
        ];
        assert_eq!(
            extract_txt_value(&values, "cache"),
            Some("https://nest.fauna.example/api/v1")
        );
        assert_eq!(extract_txt_value(&values, "self"), Some("abcdef"));
        assert!(has_txt_flag(&values, "subhandles", "true"));
        assert!(!has_txt_flag(&values, "subhandles", "false"));
        assert_eq!(extract_txt_value(&values, "id"), None);
    }

    #[test]
    fn parse_handle_empty_user() {
        assert_eq!(
            parse_handle("@fauna.example"),
            HandleForm::BareDomain("fauna.example".into())
        );
    }

    #[test]
    fn parse_handle_no_at() {
        assert_eq!(parse_handle("justtext"), HandleForm::Invalid);
    }

    /// A domain can never contain `@`, and accepting one is a **display-vs-dial
    /// split**: `a@b@c` split on the first `@` yields the domain `b@c`, which a
    /// URL parser reads as userinfo `b@` plus host `c` (`web::strip_userinfo`
    /// splits on the *last* `@` per WHATWG). The user is shown `a@b@c` and the
    /// client dials `c` — the exact divergence `security.md` § Transport trust
    /// forbids: "the authority is the host a URL parser would dial — userinfo is
    /// never part of it". Rejected outright rather than silently re-split.
    #[test]
    fn parse_handle_rejects_a_second_at() {
        assert_eq!(parse_handle("a@b@c"), HandleForm::Invalid);
        assert_eq!(
            parse_handle("alice@evil.example@real.example"),
            HandleForm::Invalid
        );
        // Same hazard through the `@`-prefixed form: `@a@b.com` would otherwise
        // parse as the two-label bare domain `a@b.com`.
        assert_eq!(parse_handle("@a@b.com"), HandleForm::Invalid);
    }

    /// The classifier inherits the rule — it is the compose/find-user entry
    /// point on web, android and linux.
    #[test]
    fn classify_recipient_rejects_a_second_at() {
        assert_eq!(classify_recipient("a@b@c"), RecipientInput::Invalid);
    }

    // ── recipient input classification (shared by web / android / linux) ──

    #[test]
    fn is_actor_id_accepts_64_hex_any_case() {
        let lower = "a".repeat(64);
        let upper = "A".repeat(64);
        let mixed = format!("{}{}", "aB".repeat(31), "cD");
        assert!(is_actor_id(&lower));
        assert!(is_actor_id(&upper));
        assert!(is_actor_id(&mixed));
    }

    #[test]
    fn is_actor_id_rejects_wrong_length_or_nonhex() {
        assert!(!is_actor_id(&"a".repeat(63)));
        assert!(!is_actor_id(&"a".repeat(65)));
        assert!(!is_actor_id(&format!("{}g", "a".repeat(63)))); // 64 chars, one non-hex
        assert!(!is_actor_id(""));
    }

    #[test]
    fn classify_actor_id_is_lowercased() {
        let upper = "A".repeat(64);
        assert_eq!(
            classify_recipient(&upper),
            RecipientInput::ActorId("a".repeat(64))
        );
    }

    #[test]
    fn classify_actor_id_trims_whitespace() {
        let padded = format!("  {}\n", "f".repeat(64));
        assert_eq!(
            classify_recipient(&padded),
            RecipientInput::ActorId("f".repeat(64))
        );
    }

    #[test]
    fn classify_named_handle() {
        assert_eq!(
            classify_recipient("alice@fauna.social"),
            RecipientInput::Handle {
                user: "alice".into(),
                domain: "fauna.social".into(),
            }
        );
    }

    #[test]
    fn classify_named_handle_trims() {
        assert_eq!(
            classify_recipient("  bob@example.com  "),
            RecipientInput::Handle {
                user: "bob".into(),
                domain: "example.com".into(),
            }
        );
    }

    #[test]
    fn classify_handle_no_dot_domain_is_accepted() {
        // Drift resolution: linux's old `split_handle_domain` required a `.` in the
        // domain and rejected this; the canonical `parse_handle` (LAN/local-aware)
        // accepts it, so the unified classifier does too.
        assert_eq!(
            classify_recipient("alice@localhost"),
            RecipientInput::Handle {
                user: "alice".into(),
                domain: "localhost".into(),
            }
        );
    }

    #[test]
    fn classify_handle_lan_ip_and_mdns() {
        assert_eq!(
            classify_recipient("alice@192.168.1.57:8443"),
            RecipientInput::Handle {
                user: "alice".into(),
                domain: "192.168.1.57:8443".into(),
            }
        );
        assert_eq!(
            classify_recipient("alice@raspberrypi.local"),
            RecipientInput::Handle {
                user: "alice".into(),
                domain: "raspberrypi.local".into(),
            }
        );
    }

    #[test]
    fn classify_bare_domain_and_subdomain_are_invalid() {
        // `@domain` / `@user.domain` forms aren't resolvable by the compose path on
        // any client today, so the classifier rejects them rather than half-support.
        assert_eq!(classify_recipient("@fauna.social"), RecipientInput::Invalid);
        assert_eq!(
            classify_recipient("@alice.fauna.social"),
            RecipientInput::Invalid
        );
    }

    #[test]
    fn classify_malformed_inputs_are_invalid() {
        assert_eq!(classify_recipient(""), RecipientInput::Invalid);
        assert_eq!(classify_recipient("   "), RecipientInput::Invalid);
        assert_eq!(classify_recipient("justtext"), RecipientInput::Invalid);
        assert_eq!(classify_recipient("alice@"), RecipientInput::Invalid);
        assert_eq!(classify_recipient("@"), RecipientInput::Invalid);
    }

    /// The kit payload's `handle=` has to be resolvable months later on a
    /// device that has never seen this account, so a bare local part is exactly
    /// the wrong thing to write there.
    #[test]
    fn qualify_handle_makes_a_session_handle_resolvable_off_device() {
        assert_eq!(
            qualify_handle("alice", "https://fauna.social"),
            Some("alice@fauna.social".to_string())
        );
        // A non-default port is part of the address, and is the form the
        // onboarding handle field accepts — so the value round-trips.
        assert_eq!(
            qualify_handle("alice", "http://localhost:8443"),
            Some("alice@localhost:8443".to_string())
        );
        // Already qualified: pass through, never double-qualify.
        assert_eq!(
            qualify_handle("alice@other.test", "https://fauna.social"),
            Some("alice@other.test".to_string())
        );
        // Nothing to qualify, or nothing to qualify it with → no half-formed
        // address (which would send a restore at a nest that isn't there).
        assert_eq!(qualify_handle("", "https://fauna.social"), None);
        assert_eq!(qualify_handle("   ", "https://fauna.social"), None);
        assert_eq!(qualify_handle("alice", ""), None);
    }

    #[test]
    fn into_parts_positional_contract() {
        assert_eq!(
            RecipientInput::ActorId("ab".into()).into_parts(),
            vec!["actor_id", "ab", "", ""]
        );
        assert_eq!(
            RecipientInput::Handle {
                user: "alice".into(),
                domain: "fauna.social".into(),
            }
            .into_parts(),
            vec!["handle", "", "alice", "fauna.social"]
        );
        assert_eq!(
            RecipientInput::Invalid.into_parts(),
            vec!["invalid", "", "", ""]
        );
    }

    // --- SRV-aware reconnect recovery (serving-ports Pillar B) ---

    #[test]
    fn srv_recovery_host_skips_local_targets() {
        // Loopback / IP literal / `.local` have no public `_fauna._tcp` zone —
        // the reconnect loop must not issue a DNS/DoH query for them.
        assert_eq!(srv_recovery_host("https://127.0.0.1:3000"), None);
        assert_eq!(srv_recovery_host("https://localhost"), None);
        assert_eq!(srv_recovery_host("https://[::1]:8443"), None);
        assert_eq!(srv_recovery_host("https://pi.local:8443"), None);
        assert_eq!(srv_recovery_host("https://192.168.1.5:443"), None);
    }

    #[test]
    fn srv_recovery_host_returns_bare_host_for_public_domains() {
        // Port-hidden and explicit-port public domains both re-resolve; only the
        // bare host is handed to the resolver.
        assert_eq!(
            srv_recovery_host("https://example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            srv_recovery_host("https://example.com:8443").as_deref(),
            Some("example.com")
        );
    }

    #[test]
    fn srv_recovered_url_rewrites_port_hidden_to_discovered_port() {
        // A port-hidden URL (current == 443) heals to the SRV port.
        assert_eq!(
            srv_recovered_url("https://example.com", Some(8443)).as_deref(),
            Some("https://example.com:8443")
        );
    }

    #[test]
    fn srv_recovered_url_no_change_when_srv_matches_current() {
        // Port-hidden URL + SRV 443 ⇒ unchanged.
        assert_eq!(srv_recovered_url("https://example.com", Some(443)), None);
        // Explicit port equal to SRV ⇒ unchanged.
        assert_eq!(
            srv_recovered_url("https://example.com:8443", Some(8443)),
            None
        );
    }

    #[test]
    fn srv_recovered_url_no_change_when_srv_absent() {
        // The correctness guard: an absent / errored SRV lookup (`None`) must NOT
        // rewrite a working explicit-port URL to the 443 default — we have no new
        // port to move to. (A transient DNS failure must not break a `:8443` nest.)
        assert_eq!(srv_recovered_url("https://example.com:8443", None), None);
        assert_eq!(srv_recovered_url("https://example.com", None), None);
    }

    #[test]
    fn srv_recovered_url_heals_a_second_port_change() {
        // The case `resolve_full_url` CANNOT do: a URL already carrying an
        // explicit port (`:8443`, from a prior resolution) re-heals when the
        // admin moves the port again (`:9000`). resolve_full_url would short-
        // circuit on the explicit port and never re-resolve.
        assert_eq!(
            srv_recovered_url("https://example.com:8443", Some(9000)).as_deref(),
            Some("https://example.com:9000")
        );
        // ...including a move back to the default 443 (SRV explicitly says 443).
        assert_eq!(
            srv_recovered_url("https://example.com:8443", Some(443)).as_deref(),
            Some("https://example.com:443")
        );
    }

    #[test]
    fn bare_handle_is_never_foreign() {
        // No typed `@domain` ⇒ the nest the client is logged into owns the
        // handle, whatever that nest's own domain happens to be.
        assert!(!is_foreign_handle_domain(None, Some("nest.test")));
        assert!(!is_foreign_handle_domain(None, None));
    }

    #[test]
    fn typed_domain_matching_home_is_local_case_insensitively() {
        assert!(!is_foreign_handle_domain(
            Some("nest.test"),
            Some("nest.test")
        ));
        // The typed string is human-entered and the home string is nest-echoed:
        // a case difference between the two is noise, not a distinction.
        assert!(!is_foreign_handle_domain(
            Some("Nest.TEST"),
            Some("nest.test")
        ));
        assert!(!is_foreign_handle_domain(
            Some("nest.test"),
            Some("NEST.test")
        ));
    }

    #[test]
    fn typed_domain_differing_from_home_is_foreign() {
        assert!(is_foreign_handle_domain(
            Some("other.test"),
            Some("nest.test")
        ));
        // A local handle of the same localpart under a different domain is a
        // DISTINCT actor — the asymmetry the rule encodes on purpose.
        assert!(is_foreign_handle_domain(
            Some("127.0.0.1:8443"),
            Some("nest.test")
        ));
    }

    #[test]
    fn typed_domain_with_unknown_home_is_foreign() {
        // The same-nest probe failed or volunteered no domain. Probing the typed
        // domain is the safe answer: falling back to same-nest would resolve
        // `bob@other.test` to a local `bob`, a different person.
        assert!(is_foreign_handle_domain(Some("other.test"), None));
    }
}
