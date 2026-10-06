//! Probe functions for the handle-first onboarding flow.
//!
//! Granular probes (handle-check wizard, Plan 1+):
//!   - `dns_ns_lookup`             — DoH NS lookup with TLD validation
//!   - `dns_a_lookup`              — DoH `A` read (the retire view's attribution check)
//!   - `nest_health_probe`         — /api/v1/health reachability check
//!   - `domain_search_proxy_price` — registrar price proxy
//!
//! The challenge/verify identity probe (`nest_challenge`/`nest_challenge_at`)
//! moved to WS-RPC — the wizard now calls
//! `fauna_onboarding_machine::nest_api::NestApi::silent_challenge`, which drives
//! the shared `fauna_protocol::auth::run_silent_challenge` ceremony.
//!
//! The setup-status / domain-status probes (`probe_setup_status[_at]`,
//! `probe_domain_status`) were retired
//! when `GET /api/v1/setup-status` was removed: the claimed/unclaimed bit now
//! rides the anonymous WS-RPC `fauna.setup.status` kind (consumed in
//! `fauna-onboarding-machine` / `fauna-launch-machine`), and the legacy
//! three-state domain probe had no remaining caller. The `DomainStatus` enum
//! is kept — the onboarding machine still carries it as wizard state.
//!
//! DoH endpoint: Cloudflare's `https://cloudflare-dns.com/dns-query`.
//! Has open CORS, returns JSON, works in WASM and native.

use std::net::IpAddr;

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::ProvisionError;
use crate::registrar::TldPriceQuote;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum DomainStatus {
    Unregistered,
    RegisteredNoNest,
    RegisteredWithNest,
}

#[derive(Deserialize)]
struct DohResponse {
    #[serde(rename = "Status")]
    status: u32,
    #[serde(rename = "Answer", default)]
    answer: Vec<DohAnswer>,
    #[serde(rename = "Authority", default)]
    authority: Vec<DohAuthority>,
}

#[derive(Deserialize)]
struct DohAnswer {
    #[serde(rename = "type")]
    record_type: u32,
    // ignoring data, name, TTL — we only need presence.
}

/// An Authority-section record: on a name with no delegation the resolver
/// returns the enclosing zone's SOA here, its owner `name` being the zone cut.
#[derive(Deserialize)]
struct DohAuthority {
    #[serde(rename = "type")]
    record_type: u32,
    #[serde(default)]
    name: String,
}

const DOH_BASE_URL: &str = "https://cloudflare-dns.com";
const A_RECORD_TYPE: u32 = 1;
const NS_RECORD_TYPE: u32 = 2;
const SOA_RECORD_TYPE: u32 = 6;
const SRV_RECORD_TYPE: u32 = 33;

// ── Granular probes for the handle-check wizard (Plan 1+) ─────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DnsLookupResult {
    pub has_ns: bool,
    pub tld_valid: bool,
}

const REGISTERABLE_TLD_INVALID_LIST: &[&str] =
    &["invalid", "localhost", "test", "example", "local", "onion"];

fn parse_tld(domain: &str) -> Option<&str> {
    domain.rsplit('.').next()
}

fn tld_is_valid_for_registration(domain: &str) -> bool {
    let Some(tld) = parse_tld(domain) else {
        return false;
    };
    if tld.is_empty() || tld == domain {
        return false; // no dot OR all-dot
    }
    !REGISTERABLE_TLD_INVALID_LIST
        .iter()
        .any(|x| x.eq_ignore_ascii_case(tld))
}

/// Where a handle's domain points for the nest probe. Internal to the
/// handle-check flow (not crossing the FFI boundary — the onboarding machine
/// consumes it in Rust), so no `uniffi::Record` derive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandleDomainTarget {
    /// Base URL (`scheme://host[:port]`, no trailing slash) the nest health /
    /// challenge / setup-status probes should hit.
    pub base_url: String,
    /// True for a registerable-form **public DNS name** — the caller runs the
    /// DNS NS lookup, the TLD-registration check, and the price probe. False
    /// for `localhost` / `*.localhost` / IP-literal / `.local` mDNS targets
    /// (with or without an explicit port) — none of those probes apply to a
    /// self-hosted loopback / LAN-IP / `.local` nest, so the caller skips them
    /// and probes `base_url` directly; such a target is served with a
    /// self-signed cert authenticated via the channel-binding trust model
    /// (`docs/goal/architecture/security.md` § Transport trust), not a CA.
    pub is_public_dns_name: bool,
}

/// Split a handle domain into `(host, Option<port>)`. Handles bare hosts
/// (`localhost`), `host:port` (`localhost:3000`), IPv4(`:port`), bare IPv6
/// (`::1`, no port — distinguished by having ≥2 colons), bracketed IPv6
/// (`[::1]`, `[::1]:3000`), a `userinfo@` prefix (dropped), and a trailing
/// path/query/fragment past a WHATWG authority terminator (`/ \ ? #`,
/// dropped — a handle domain isn't a URL, but nothing upstream guarantees
/// one wasn't pasted in whole).
///
/// Public: the onboarding machine's NAT-mode private-ward refinement strips
/// the port with the same splitter the probe uses before classifying the
/// bare host via `fauna_core::resolve::is_private_network_target`
/// (`docs/goal/behavior/onboarding.md` § 3b-bis Defaulting note) — one
/// definition of "the host part of a handle domain".
pub fn split_host_port(domain: &str) -> (&str, Option<u16>) {
    // Cut at the first WHATWG authority terminator BEFORE stripping
    // userinfo — mirroring `fauna_core::web::authority_of`'s order (cut,
    // then strip) rather than the reverse. Cutting first, not second,
    // matters: a terminator that appears *before* the last `@` would
    // otherwise be swallowed into the "userinfo" half by
    // `rsplit_once('@')`, so `"x.example?@evil"` stripped-then-cut reads
    // userinfo `"x.example?"` / host `"evil"` — the opposite of the host a
    // URL parser dials. `generic_authority` is `authority_of`'s any-scheme,
    // scheme-less-pass-through sibling: it also used to be the *whole*
    // fix here, when this function read to the end of the raw string with
    // no terminator cut at all, letting a `.local`-suffixed tail past `#`/
    // `?`/`/` masquerade as part of the host
    // (`security.md` § Transport trust, the host-extractor family).
    let authority = fauna_core::web::generic_authority(domain);
    // Strip any `userinfo@` next — `fauna_core::web::split_host_port`'s own
    // doc names this the caller's obligation, and this classifies the same
    // authority `parse_node_address` does (`resolve.rs::parse_node_address`).
    // Skipping it let a domain like `"nest.example.com@attacker.example"`
    // classify on the whole string instead of the host a URL parser would
    // actually dial (`security.md` § Transport trust).
    let authority = fauna_core::web::strip_userinfo(authority);
    // The canonical split, then the canonical bracket-strip — this caller
    // classifies the bare host as an `IpAddr`, so `[::1]` must come back as
    // `::1`. See `fauna_core::web::split_host_port` for why the strip is a
    // separate primitive.
    let (host, port) = fauna_core::web::split_host_port(authority);
    (fauna_core::resolve::strip_ipv6_brackets(host), port)
}

/// Resolve a handle's domain part (from `parse_handle_domain`) into a nest
/// probe target, distinguishing local "trying out the app" nests from
/// registerable domains.
///
/// - `localhost`, `*.localhost`, and loopback IPs (`127.0.0.0/8`, `::1`): with
///   no explicit port → `https://…` (port hidden, `:443`), identical to a LAN IP
///   / public domain — a nest listens on one client-facing port regardless of
///   client origin (`docs/goal/architecture/installers/windows.md`
///   § Network-reachable nest). An explicit port is honored, still `https://…:port`
///   — the scheme no longer depends on host-class. `is_public_dns_name = false`
///   either way (the caller skips the DNS / TLD / price probes).
/// - any other IP literal (e.g. a LAN/private-nest address) → `https://…`
///   (an explicit port is honored); `is_public_dns_name = false`.
/// - a `.local` mDNS hostname (`pi.local`) → `https://…` (explicit port
///   honored, else 443 implied); `is_public_dns_name = false`. The host
///   resolves to an address via OS-level multicast DNS at connect time; this
///   only classifies it so the caller skips the unicast-DNS registration
///   probes.
/// - everything else (a registerable domain) → `https://{domain}` and
///   `is_public_dns_name = true`, so the caller still runs the DNS / TLD /
///   price probes.
///
/// See `docs/goal/behavior/onboarding.md` §2 "Local / loopback targets".
pub fn resolve_handle_domain(domain: &str) -> HandleDomainTarget {
    resolve_handle_domain_with_local_port(domain, LOCAL_NEST_DEFAULT_PORT)
}

/// The port a loopback nest is assumed to listen on when the handle gives none —
/// `443`, matching the network-reachable nest (`installers/windows.md`
/// § Network-reachable nest). It is the default the plain [`resolve_handle_domain`]
/// uses; [`resolve_handle_domain_with_local_port`] lets a same-box client (or an
/// e2e test) inject a different one. A loopback nest on `443` keeps the port
/// hidden, exactly like a LAN IP / public domain.
pub const LOCAL_NEST_DEFAULT_PORT: u16 = 443;

/// Like [`resolve_handle_domain`], but the caller supplies the port a *loopback*
/// nest listens on (default `443` via [`resolve_handle_domain`]). The onboarding
/// machine threads its injectable `local_nest_port` here so a same-box client (the
/// Option-A "the app knows its local nest" path) — or an e2e test that boots a
/// nest on a random free port — can reach a local nest that is not on `443`,
/// **without** changing how LAN / `.local` / public-domain handles resolve. A
/// `443` value keeps the port hidden (the clean default), so existing behavior is
/// unchanged for every non-injecting caller.
pub fn resolve_handle_domain_with_local_port(
    domain: &str,
    local_nest_port: u16,
) -> HandleDomainTarget {
    let (host, port) = split_host_port(domain);
    let ip = host.parse::<IpAddr>().ok();
    let is_loopback = host.eq_ignore_ascii_case("localhost")
        || host.to_ascii_lowercase().ends_with(".localhost")
        || ip.is_some_and(|a| a.is_loopback());
    // The public-DNS-name classification (false for IP literal /
    // `localhost`/`*.localhost` / `.local` mDNS) is the shared
    // `fauna_core::resolve::is_public_dns_name` — one source of truth with
    // client MUA-endpoint display (`fauna_client_mail_settings`). A
    // non-public (local) target keeps the `https` default below (unless
    // loopback), authenticated via the channel-binding trust model
    // (`docs/goal/architecture/security.md` § Transport trust), not a public
    // CA, and the caller skips the unicast-DNS / registrar / price probes (a
    // `.local` name has no registrable TLD).
    let is_public_dns_name = fauna_core::resolve::is_public_dns_name(host);

    if is_public_dns_name {
        // Registerable domain — compose from the same `host`/`port` just
        // classified, not the raw `domain` string. Composing from `domain`
        // let a userinfo/junk prefix ride along into the URL even though the
        // classifier had already read past it (`strip_userinfo` above), so
        // the string classified and the string composed could name two
        // different authorities (`security.md` § Transport trust) — the caller still runs the DNS-registration probes.
        let base_url = match port {
            Some(p) => format!("https://{host}:{p}"),
            None => format!("https://{host}"),
        };
        return HandleDomainTarget {
            base_url,
            is_public_dns_name: true,
        };
    }

    // IPv6 literals must be bracketed in a URL authority.
    let host_in_url = if matches!(ip, Some(IpAddr::V6(_))) {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let base_url = match port {
        // An explicit port is honored for EVERY host-class — uniform https. A
        // real nest serves TLS on every entry path (the self-signed floor);
        // plain HTTP rides only `FAUNA_INSECURE_DISABLE_TLS`, the
        // test/diagnostic escape no deployment path sets. So a loopback
        // `host:port` resolves to https exactly like a LAN IP or a public
        // domain — the host-class no longer picks the scheme. The tier_3 e2e
        // harness reaches a plain-HTTP test nest through the
        // `provider_base_urls["nest"]` override seam (the onboarding machine
        // probes the override while `state.nest_url` records this resolved
        // https URL), not through a loopback-`http` special case.
        Some(p) => format!("https://{host_in_url}:{p}"),
        // No explicit port, a loopback host, and the caller injected a non-default
        // local nest port (a same-box app that knows its nest, or an e2e test on a
        // random free port) → https on that port. Still https, like every origin.
        None if is_loopback && local_nest_port != LOCAL_NEST_DEFAULT_PORT => {
            format!("https://{host_in_url}:{local_nest_port}")
        }
        // No explicit port → https with the port hidden (:443), identical for
        // loopback, LAN IP, `.local`, and public domains. A nest listens on one
        // client-facing port (:443) regardless of client origin
        // (installers/windows.md § Network-reachable nest); the old loopback-only
        // http:3000 default was the standalone dev binary's `--bind` port
        // (bins/fauna-nest/src/main.rs) leaking into client onboarding.
        None => format!("https://{host_in_url}"),
    };
    HandleDomainTarget {
        base_url,
        is_public_dns_name,
    }
}

/// Map a peer's handle domain to the peer nest's **base URL** — the one
/// derivation every cross-nest caller uses to turn "the domain part of
/// `bob@other.test`" into a URL it can hand to the wire. `None` (same-nest)
/// passes through as `None`.
///
/// The rule is [`resolve_handle_domain`]'s, one line below it so the two can
/// never drift: uniform `https://…` for every host-class — loopback and
/// IP-literal authorities included (Pillar C; a nest serves TLS on every entry
/// path, the self-signed floor).
///
/// Three callers share it today, which is why it lives here rather than in any
/// one of them (priorities #1/#2/#4):
///
/// - the conversations recipient picker's federation relay `nest_url`
///   (`fauna_client_conversations`' `keypackage_fetch` / `welcome_deliver`, on
///   the native **and** wasm seam arms alike — the derivation is identical,
///   only the transport differs), where the home nest signs + forwards;
/// - the contacts knock send's `recipient_nest_url` on `fauna.inbox.send`
///   (`bins/fauna-nest/src/inbox_handlers.rs` branches on it: `None` delivers
///   locally, `Some` originates `fauna.federation.inbox.deliver`);
/// - the same page's anonymous **discovery** hop, which dials this URL directly
///   to run `fauna.actor.by_handle` against the peer — the nest-proxied
///   `fauna.nest.resolve` cannot stand in, because it refuses loopback / IP /
///   `.local` authorities by design (`discovery_core.rs`), which is exactly the
///   shape a two-nest test topology has.
///
/// Pair it with [`fauna_core::resolve::is_foreign_handle_domain`], which owns
/// the *decision* this function then acts on.
pub fn peer_nest_url(peer_domain: Option<String>) -> Option<String> {
    peer_domain.map(|d| resolve_handle_domain(&d).base_url)
}

/// The Axis-2 DNS classification for a nest authority (`host[:port]`): `Some(bare
/// host)` when it is a registrable **public** domain whose `_fauna.{host}` TXT
/// `self=` identity root should be consulted, `None` for loopback / IP-literal /
/// `.local` (no DNS authority → TOFU-on-host).
///
/// Shares [`resolve_handle_domain`]'s `is_public_dns_name` classification
/// rather than re-deriving it, so "what counts as local" has one source of
/// truth. The
/// caller (the channel-binding trust path,
/// `docs/goal/architecture/security.md` § Transport trust, Axis 2) only reaches
/// for the DNS root on a self-signed (non-WebPKI) cert; a public domain serving
/// a self-signed cert is the rare case this exists for.
pub fn public_dns_host(authority: &str) -> Option<&str> {
    if !resolve_handle_domain(authority).is_public_dns_name {
        return None;
    }
    Some(split_host_port(authority).0)
}

pub async fn dns_ns_lookup(
    client: &Client,
    domain: &str,
) -> Result<DnsLookupResult, ProvisionError> {
    dns_ns_lookup_with_base_url(client, domain, DOH_BASE_URL).await
}

#[doc(hidden)]
pub async fn dns_ns_lookup_with_base_url(
    client: &Client,
    domain: &str,
    base_url: &str,
) -> Result<DnsLookupResult, ProvisionError> {
    Ok(dns_ns_probe_with_base_url(client, domain, base_url)
        .await?
        .lookup)
}

/// The handle check's NS probe: the [`DnsLookupResult`] plus the **zone cut**
/// the resolver reported for a name with no delegation of its own.
///
/// Rust-only (no UniFFI derive): [`DnsLookupResult`] is a bound record, and the
/// zone is consumed by the onboarding machine in Rust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NsProbe {
    pub lookup: DnsLookupResult,
    /// The zone a name without a delegation sits inside — the owner of the SOA
    /// in the response's Authority section, lowercased, no trailing dot — when
    /// that zone is a strict ancestor of the name with more than one label
    /// (`example.com` for `dev.example.com`). `None` when the name is delegated,
    /// when the SOA is a bare TLD (`com` — where every unregistered name lands),
    /// or when no usable SOA came back. A multi-label public suffix (`co.uk`
    /// for `foo.co.uk`) is reported too: without public-suffix knowledge the
    /// probe cannot tell it from a held domain
    /// (`docs/goal/behavior/onboarding-provisioning.md` § 4).
    pub enclosing_zone: Option<String>,
    /// The in-zone name already resolves: it has an `A` record of its own. Only
    /// looked up when [`Self::enclosing_zone`] is set (always `false`
    /// otherwise). A name someone has pointed at a host is not free to set up —
    /// most often a nest already serves it — so the handle check goes on to
    /// probe that host instead of offering the name as available
    /// (`docs/goal/behavior/onboarding-provisioning.md` § 4).
    pub in_zone_address: bool,
}

pub async fn dns_ns_probe(client: &Client, domain: &str) -> Result<NsProbe, ProvisionError> {
    dns_ns_probe_with_base_url(client, domain, DOH_BASE_URL).await
}

#[doc(hidden)]
pub async fn dns_ns_probe_with_base_url(
    client: &Client,
    domain: &str,
    base_url: &str,
) -> Result<NsProbe, ProvisionError> {
    let tld_valid = tld_is_valid_for_registration(domain);
    if !tld_valid {
        return Ok(NsProbe {
            lookup: DnsLookupResult {
                has_ns: false,
                tld_valid: false,
            },
            enclosing_zone: None,
            in_zone_address: false,
        });
    }
    let resp = client
        .get(format!("{base_url}/dns-query"))
        .query(&[("name", domain), ("type", "NS")])
        .header("Accept", "application/dns-json")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(ProvisionError::provider(
            resp.status().as_u16(),
            "DoH lookup failed",
        ));
    }
    let data: DohResponse = resp.json().await?;
    let has_ns = data.status != 3 && data.answer.iter().any(|a| a.record_type == NS_RECORD_TYPE);
    let enclosing_zone = if has_ns {
        None
    } else {
        data.authority
            .iter()
            .filter(|a| a.record_type == SOA_RECORD_TYPE)
            .find_map(|a| held_ancestor_zone(domain, &a.name))
    };
    let in_zone_address = enclosing_zone.is_some()
        && !dns_a_lookup_with_base_url(client, domain, base_url)
            .await
            .is_empty();
    Ok(NsProbe {
        lookup: DnsLookupResult {
            has_ns,
            tld_valid: true,
        },
        enclosing_zone,
        in_zone_address,
    })
}

/// `zone` (an SOA owner name) normalized, when it is a strict ancestor of
/// `domain` with more than one label — see [`NsProbe::enclosing_zone`].
fn held_ancestor_zone(domain: &str, zone: &str) -> Option<String> {
    let zone = zone.trim_end_matches('.').to_ascii_lowercase();
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    (zone.contains('.') && domain.ends_with(&format!(".{zone}"))).then_some(zone)
}

/// A DoH answer that also carries the record `data` (rdata). The NS probe's
/// [`DohAnswer`] deliberately drops it — it only needs record *presence* — but an
/// SRV lookup must read the port out of the rdata, and an `A` lookup the
/// address, so this variant keeps it.
#[derive(Deserialize)]
struct DohDataAnswer {
    #[serde(rename = "type")]
    record_type: u32,
    #[serde(rename = "data", default)]
    data: String,
}

#[derive(Deserialize)]
struct DohDataResponse {
    #[serde(rename = "Status")]
    status: u32,
    #[serde(rename = "Answer", default)]
    answer: Vec<DohDataAnswer>,
}

/// Parse the port (3rd field) out of an SRV record's rdata string,
/// `"{priority} {weight} {port} {target}"` (e.g. `"0 0 8443 nest.example.com."`).
fn parse_srv_port(rdata: &str) -> Option<u16> {
    rdata.split_whitespace().nth(2)?.parse::<u16>().ok()
}

/// Look up the `_fauna._tcp.<domain>` SRV record via DoH and return the
/// client-facing API port it advertises, if any.
///
/// **Pillar B (handle-reach) of the serving-ports plan** — a registered *public*
/// domain may serve its client-facing API on a non-standard port; this lets a
/// clean `alice@domain` handle stay **port-hidden** (the port is *discovered*
/// here, never typed). Returns `Some(port)` when an `_fauna._tcp` SRV record
/// exists (including a literal `443`, which the caller treats as "no rewrite"),
/// and `None` on NXDOMAIN, no SRV answer, or any DoH / parse error — the caller
/// then keeps the port-hidden `https://{domain}` (`:443`) default.
///
/// This is the **cross-platform** twin of
/// [`fauna_core::resolve::resolve_node_url`], which uses hickory's
/// `TokioResolver` and is native-only: the onboarding machine compiles to wasm
/// (where hickory does not build), so the handle-reach path uses the same DoH
/// transport as [`dns_ns_lookup`] above (`reqwest` over HTTPS works on wasm +
/// native). Mirrors `resolve_node_url`/`resolve_full_url` semantics: SRV is used
/// for the *port* only; the handle's host is preserved by the caller.
pub async fn fauna_srv_port(client: &Client, domain: &str) -> Option<u16> {
    fauna_srv_port_with_base_url(client, domain, DOH_BASE_URL).await
}

#[doc(hidden)]
pub async fn fauna_srv_port_with_base_url(
    client: &Client,
    domain: &str,
    base_url: &str,
) -> Option<u16> {
    let srv_name = format!("_fauna._tcp.{domain}");
    let resp = client
        .get(format!("{base_url}/dns-query"))
        .query(&[("name", srv_name.as_str()), ("type", "SRV")])
        .header("Accept", "application/dns-json")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let data: DohDataResponse = resp.json().await.ok()?;
    if data.status != 0 {
        return None; // NXDOMAIN (3) or any other DNS error → port-hidden default
    }
    data.answer
        .iter()
        .find(|a| a.record_type == SRV_RECORD_TYPE)
        .and_then(|a| parse_srv_port(&a.data))
}

/// Read `name`'s `A` records from **public DNS** via DoH and return their
/// addresses (dotted-quad strings, in answer order).
///
/// The client-side public-DNS read of `nest-retirement.md` § Box → domain
/// attribution: the retire view verifies *"this domain's apex `A` equals this
/// box's IPv4"* with **no reachable nest** and, for a VPS-only provider, no DNS
/// credential either — so the only witness left is what the public resolver
/// answers. Same transport as [`dns_ns_lookup`] / [`fauna_srv_port`] (`reqwest`
/// over HTTPS, wasm + native), so it adds no new third party.
///
/// Returns an **empty list** on NXDOMAIN, no `A` answer, or any DoH / parse
/// error: every caller treats "could not read" as "did not verify", which is
/// the safe direction. A `CNAME` chain in the answer is skipped over — only the
/// type-1 records at its end are addresses.
pub async fn dns_a_lookup(client: &Client, name: &str) -> Vec<String> {
    dns_a_lookup_with_base_url(client, name, DOH_BASE_URL).await
}

#[doc(hidden)]
pub async fn dns_a_lookup_with_base_url(
    client: &Client,
    name: &str,
    base_url: &str,
) -> Vec<String> {
    let Ok(resp) = client
        .get(format!("{base_url}/dns-query"))
        .query(&[("name", name), ("type", "A")])
        .header("Accept", "application/dns-json")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    else {
        return Vec::new();
    };
    if !resp.status().is_success() {
        return Vec::new();
    }
    let Ok(data) = resp.json::<DohDataResponse>().await else {
        return Vec::new();
    };
    if data.status != 0 {
        return Vec::new();
    }
    data.answer
        .into_iter()
        .filter(|a| a.record_type == A_RECORD_TYPE)
        .map(|a| a.data.trim().to_string())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum NestHealthState {
    Reachable,
    ConnectionRefused,
    Timeout,
    Misbehaving { status: u16 },
    MalformedBody,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct NestHealthResult {
    pub state: NestHealthState,
}

pub async fn nest_health_probe(client: &Client, domain: &str) -> NestHealthResult {
    nest_health_probe_at(client, &format!("https://{domain}")).await
}

#[doc(hidden)]
pub async fn nest_health_probe_at(client: &Client, base: &str) -> NestHealthResult {
    let url = format!("{base}/api/v1/health");
    let result = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;
    let state = match result {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if resp.status().is_success() {
                match resp.json::<serde_json::Value>().await {
                    Ok(_) => NestHealthState::Reachable,
                    Err(_) => NestHealthState::MalformedBody,
                }
            } else {
                NestHealthState::Misbehaving { status }
            }
        }
        Err(e) if e.is_timeout() => NestHealthState::Timeout,
        #[cfg(not(target_arch = "wasm32"))]
        Err(e) if e.is_connect() => NestHealthState::ConnectionRefused,
        Err(_) => NestHealthState::ConnectionRefused, // best-effort classification
    };
    NestHealthResult { state }
}

// The handle-check silent-sign-in (`nest_challenge`/`nest_challenge_at` +
// `ChallengeState`/`ChallengeResult`) was removed:
// the wizard now runs the ceremony over WS-RPC via
// `fauna_onboarding_machine::nest_api::NestApi::silent_challenge`, which delegates
// to the shared `fauna_protocol::auth::run_silent_challenge`. The HTTP twin
// `POST /api/v1/auth/{challenge,verify}` survives only for apple's
// `APIClient.silentSignIn` until that migrates.

const DOMAIN_SEARCH_PROXY_URL: Option<&'static str> = option_env!("FAUNA_DOMAIN_SEARCH_PROXY_URL");

pub async fn domain_search_proxy_price(client: &Client, domain: &str) -> Option<TldPriceQuote> {
    domain_search_proxy_price_with_url(client, domain, DOMAIN_SEARCH_PROXY_URL).await
}

#[doc(hidden)]
pub async fn domain_search_proxy_price_with_url(
    client: &Client,
    domain: &str,
    base_url: Option<&str>,
) -> Option<TldPriceQuote> {
    let base = base_url?;
    let resp = client
        .get(format!("{base}/price"))
        .query(&[("domain", domain)])
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<TldPriceQuote>().await.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_doh_with_ns_records() {
        let json = r#"{
            "Status": 0,
            "Answer": [
                { "name": "example.com.", "type": 2, "TTL": 86400, "data": "ns1.example.com." }
            ]
        }"#;
        let parsed: DohResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.status, 0);
        assert_eq!(parsed.answer.len(), 1);
        assert_eq!(parsed.answer[0].record_type, NS_RECORD_TYPE);
    }

    #[test]
    fn parses_doh_nxdomain() {
        let json = r#"{ "Status": 3 }"#;
        let parsed: DohResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.status, 3);
        assert!(parsed.answer.is_empty());
    }

    #[test]
    fn parses_doh_no_records() {
        // NOERROR but no Answer array — domain is in DNS root but has no
        // NS at the queried name (rare; usually means we got NXDOMAIN
        // instead, but defend against it).
        let json = r#"{ "Status": 0 }"#;
        let parsed: DohResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.status, 0);
        assert!(parsed.answer.is_empty());
    }

    #[tokio::test]
    async fn dns_ns_lookup_returns_has_ns_when_doh_returns_ns_record() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dns-query"))
            .and(query_param("name", "example.com"))
            .and(query_param("type", "NS"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"Status":0,"Answer":[{"name":"example.com.","type":2,"TTL":3600,"data":"ns1.example.com."}]}"#,
            ))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = dns_ns_lookup_with_base_url(&client, "example.com", &server.uri()).await;

        assert!(result.is_ok());
        let r = result.unwrap();
        assert!(r.has_ns);
        assert!(r.tld_valid);
    }

    #[tokio::test]
    async fn dns_ns_lookup_returns_no_ns_for_nxdomain() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dns-query"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"Status":3}"#))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let r = dns_ns_lookup_with_base_url(&client, "missing.com", &server.uri())
            .await
            .unwrap();
        assert!(!r.has_ns);
        assert!(r.tld_valid);
    }

    #[tokio::test]
    async fn dns_ns_lookup_marks_tld_invalid_for_localhost() {
        let client = reqwest::Client::new();
        let r = dns_ns_lookup(&client, "alice.localhost").await.unwrap();
        assert!(!r.has_ns);
        assert!(!r.tld_valid);
    }

    #[tokio::test]
    async fn dns_ns_lookup_marks_tld_invalid_for_test() {
        let client = reqwest::Client::new();
        let r = dns_ns_lookup(&client, "alice.test").await.unwrap();
        assert!(!r.tld_valid);
    }

    // ── dns_ns_probe: the zone cut behind a name with no delegation ─────────

    async fn ns_probe_against(domain: &str, body: &'static str) -> NsProbe {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dns-query"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        dns_ns_probe_with_base_url(&client, domain, &server.uri())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn ns_probe_reports_the_zone_a_subdomain_sits_inside() {
        // `dev.example.com` under a registered `example.com`: no delegation of
        // its own, and the resolver's SOA names the zone that holds it.
        let p = ns_probe_against(
            "dev.example.com",
            r#"{"Status":3,"Authority":[{"name":"Example.COM.","type":6,"TTL":1800,"data":"ns. host. 1 1 1 1 1"}]}"#,
        )
        .await;
        assert!(!p.lookup.has_ns);
        assert!(p.lookup.tld_valid);
        assert_eq!(p.enclosing_zone.as_deref(), Some("example.com"));
    }

    #[tokio::test]
    async fn ns_probe_reports_no_zone_when_the_name_sits_directly_under_a_tld() {
        // An unregistered `foo.com`: the SOA is the registry's `com` — where
        // every unregistered name lands, not a zone anyone holds.
        let p = ns_probe_against(
            "foo.com",
            r#"{"Status":3,"Authority":[{"name":"com","type":6,"TTL":900,"data":"a. b. 1 1 1 1 1"}]}"#,
        )
        .await;
        assert!(!p.lookup.has_ns);
        assert_eq!(p.enclosing_zone, None);
    }

    #[tokio::test]
    async fn ns_probe_reports_a_multi_label_suffix_zone_too() {
        // The documented ambiguity: with no public-suffix knowledge, `co.uk`
        // reads exactly like a held `example.com` — the caller's copy and
        // `buy_domain` default are shaped for both readings.
        let p = ns_probe_against(
            "foo.co.uk",
            r#"{"Status":3,"Authority":[{"name":"co.uk","type":6,"TTL":900,"data":"a. b. 1 1 1 1 1"}]}"#,
        )
        .await;
        assert_eq!(p.enclosing_zone.as_deref(), Some("co.uk"));
    }

    #[tokio::test]
    async fn ns_probe_ignores_an_soa_that_is_not_an_ancestor_or_not_an_soa() {
        let unrelated = ns_probe_against(
            "dev.example.com",
            r#"{"Status":3,"Authority":[{"name":"other.org","type":6,"TTL":1,"data":"a. b. 1 1 1 1 1"}]}"#,
        )
        .await;
        assert_eq!(unrelated.enclosing_zone, None);
        let not_soa = ns_probe_against(
            "dev.example.com",
            r#"{"Status":3,"Authority":[{"name":"example.com","type":2,"TTL":1,"data":"ns1.example.com."}]}"#,
        )
        .await;
        assert_eq!(not_soa.enclosing_zone, None);
        // A name that HAS a delegation is a zone itself: no enclosing zone.
        let delegated = ns_probe_against(
            "example.com",
            r#"{"Status":0,"Answer":[{"name":"example.com.","type":2,"TTL":1,"data":"ns1.example.com."}]}"#,
        )
        .await;
        assert!(delegated.lookup.has_ns);
        assert_eq!(delegated.enclosing_zone, None);
    }

    /// A resolver that answers the NS query with `ns_body` and the A query with
    /// `a_body` — the shape of a real in-zone name, whose two answers differ.
    async fn ns_probe_against_split(
        domain: &str,
        ns_body: &'static str,
        a_body: &'static str,
    ) -> NsProbe {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dns-query"))
            .and(query_param("type", "NS"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ns_body))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/dns-query"))
            .and(query_param("type", "A"))
            .respond_with(ResponseTemplate::new(200).set_body_string(a_body))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        dns_ns_probe_with_base_url(&client, domain, &server.uri())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn ns_probe_reports_an_in_zone_name_that_already_has_an_address() {
        // A nest already serving `dev.example.com` inside a held `example.com`:
        // no delegation of its own, but the name resolves — the handle check must
        // go on to probe the nest there, not offer the name as available.
        let p = ns_probe_against_split(
            "dev.example.com",
            r#"{"Status":0,"Authority":[{"name":"example.com","type":6,"TTL":1,"data":"a. b. 1 1 1 1 1"}]}"#,
            r#"{"Status":0,"Answer":[{"name":"dev.example.com","type":1,"TTL":1,"data":"192.0.2.7"}]}"#,
        )
        .await;
        assert!(!p.lookup.has_ns);
        assert_eq!(p.enclosing_zone.as_deref(), Some("example.com"));
        assert!(p.in_zone_address);
    }

    #[tokio::test]
    async fn ns_probe_reports_no_address_for_an_unset_in_zone_name() {
        // A fresh `dev.example.com` nobody has pointed anywhere yet: the
        // available-inside-a-zone path stays.
        let p = ns_probe_against_split(
            "dev.example.com",
            r#"{"Status":3,"Authority":[{"name":"example.com","type":6,"TTL":1,"data":"a. b. 1 1 1 1 1"}]}"#,
            r#"{"Status":3,"Authority":[{"name":"example.com","type":6,"TTL":1,"data":"a. b. 1 1 1 1 1"}]}"#,
        )
        .await;
        assert_eq!(p.enclosing_zone.as_deref(), Some("example.com"));
        assert!(!p.in_zone_address);
    }

    // ── fauna_srv_port: _fauna._tcp SRV port discovery (Pillar B reach) ──────

    #[test]
    fn parse_srv_port_reads_third_field() {
        assert_eq!(parse_srv_port("0 0 8443 nest.example.com."), Some(8443));
        assert_eq!(parse_srv_port("10 5 443 example.com."), Some(443));
        // Malformed rdata → None (caller keeps the port-hidden default).
        assert_eq!(parse_srv_port(""), None);
        assert_eq!(parse_srv_port("0 0"), None);
        assert_eq!(parse_srv_port("0 0 notaport host."), None);
    }

    #[tokio::test]
    async fn fauna_srv_port_returns_port_from_srv_record() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dns-query"))
            .and(query_param("name", "_fauna._tcp.example.com"))
            .and(query_param("type", "SRV"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"Status":0,"Answer":[{"name":"_fauna._tcp.example.com.","type":33,"TTL":300,"data":"0 0 8443 nest.example.com."}]}"#,
            ))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let port = fauna_srv_port_with_base_url(&client, "example.com", &server.uri()).await;
        assert_eq!(port, Some(8443));
    }

    #[tokio::test]
    async fn fauna_srv_port_returns_443_when_advertised() {
        // A literal :443 SRV is still returned — the caller treats it as "no
        // rewrite" (port-hidden), but the lookup itself is faithful.
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dns-query"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"Status":0,"Answer":[{"name":"_fauna._tcp.example.com.","type":33,"TTL":300,"data":"0 0 443 example.com."}]}"#,
            ))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let port = fauna_srv_port_with_base_url(&client, "example.com", &server.uri()).await;
        assert_eq!(port, Some(443));
    }

    #[tokio::test]
    async fn fauna_srv_port_none_on_nxdomain() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dns-query"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"Status":3}"#))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let port = fauna_srv_port_with_base_url(&client, "no-srv.example", &server.uri()).await;
        assert_eq!(port, None);
    }

    #[tokio::test]
    async fn fauna_srv_port_none_when_no_srv_answer() {
        // NOERROR but the Answer carries no SRV (type 33) record → None.
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dns-query"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"Status":0,"Answer":[{"name":"_fauna._tcp.example.com.","type":5,"TTL":300,"data":"alias.example.com."}]}"#,
            ))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let port = fauna_srv_port_with_base_url(&client, "example.com", &server.uri()).await;
        assert_eq!(port, None);
    }

    #[tokio::test]
    async fn fauna_srv_port_none_on_doh_5xx() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dns-query"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let port = fauna_srv_port_with_base_url(&client, "example.com", &server.uri()).await;
        assert_eq!(port, None);
    }

    // ── dns_a_lookup: the retire view's public-DNS attribution read ────────

    #[tokio::test]
    async fn dns_a_lookup_returns_the_a_records() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dns-query"))
            .and(query_param("name", "example.com"))
            .and(query_param("type", "A"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"Status":0,"Answer":[{"name":"example.com.","type":1,"TTL":300,"data":"203.0.113.5"},{"name":"example.com.","type":1,"TTL":300,"data":"203.0.113.6"}]}"#,
            ))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let got = dns_a_lookup_with_base_url(&client, "example.com", &server.uri()).await;
        assert_eq!(
            got,
            vec!["203.0.113.5".to_string(), "203.0.113.6".to_string()]
        );
    }

    #[tokio::test]
    async fn dns_a_lookup_skips_the_cname_chain_and_keeps_the_addresses() {
        // A chained name answers CNAME (type 5) rows ahead of the `A` rows;
        // only the type-1 rdata is an address.
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dns-query"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"Status":0,"Answer":[{"name":"www.example.com.","type":5,"TTL":300,"data":"example.com."},{"name":"example.com.","type":1,"TTL":300,"data":"203.0.113.5"}]}"#,
            ))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let got = dns_a_lookup_with_base_url(&client, "www.example.com", &server.uri()).await;
        assert_eq!(got, vec!["203.0.113.5".to_string()]);
    }

    #[tokio::test]
    async fn dns_a_lookup_is_empty_on_nxdomain_no_answer_and_doh_failure() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};
        let client = reqwest::Client::new();
        for (status, body) in [
            (200, r#"{"Status":3}"#),
            (200, r#"{"Status":0}"#),
            (200, "not json"),
            (503, ""),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/dns-query"))
                .respond_with(ResponseTemplate::new(status).set_body_string(body))
                .mount(&server)
                .await;
            let got = dns_a_lookup_with_base_url(&client, "example.com", &server.uri()).await;
            assert!(got.is_empty(), "{status} {body:?} must read as no address");
        }
    }

    // ── resolve_handle_domain: local "trying out the app" targets ──────────
    // `dns_ns_lookup` (above) still rejects `localhost`/`*.test` as
    // unregisterable — that is correct. The handle-check machine no longer
    // *calls* it for local targets; instead it resolves them here and probes
    // the nest directly. See `docs/goal/behavior/onboarding.md` §2.

    #[test]
    fn resolve_localhost_no_port_defaults_to_https_443() {
        // A bare loopback handle resolves like any other host: https, port
        // hidden (:443). A nest listens on one client-facing port regardless of
        // client origin (installers/windows.md § Network-reachable nest); the old
        // loopback-only http:3000 default was the standalone dev binary's port
        // leaking into client onboarding.
        let t = resolve_handle_domain("localhost");
        assert_eq!(t.base_url, "https://localhost");
        assert!(!t.is_public_dns_name);
    }

    #[test]
    fn resolve_localhost_honors_explicit_port() {
        // Uniform https: an explicit loopback port is honored on https, not the
        // old plain-`http` special case — the host-class no longer picks scheme.
        let t = resolve_handle_domain("localhost:9000");
        assert_eq!(t.base_url, "https://localhost:9000");
        assert!(!t.is_public_dns_name);
    }

    #[test]
    fn resolve_localhost_subdomain_no_port_is_https_443() {
        let t = resolve_handle_domain("alice.localhost");
        assert_eq!(t.base_url, "https://alice.localhost");
        assert!(!t.is_public_dns_name);
    }

    #[test]
    fn resolve_loopback_ipv4_no_port_is_https_443() {
        let t = resolve_handle_domain("127.0.0.1");
        assert_eq!(t.base_url, "https://127.0.0.1");
        assert!(!t.is_public_dns_name);
        // Any 127.0.0.0/8 address is loopback.
        assert_eq!(
            resolve_handle_domain("127.0.0.5").base_url,
            "https://127.0.0.5"
        );
    }

    #[test]
    fn resolve_loopback_ipv4_honors_explicit_port() {
        // Uniform https — an explicit loopback port no longer downgrades to http.
        let t = resolve_handle_domain("127.0.0.1:8080");
        assert_eq!(t.base_url, "https://127.0.0.1:8080");
        assert!(!t.is_public_dns_name);
    }

    #[test]
    fn resolve_ipv6_loopback_brackets() {
        // No port → https, bracketed, port hidden (:443) — like any host.
        let t = resolve_handle_domain("::1");
        assert_eq!(t.base_url, "https://[::1]");
        assert!(!t.is_public_dns_name);
        // An explicit loopback port is honored on https (uniform scheme).
        let t2 = resolve_handle_domain("[::1]:8443");
        assert_eq!(t2.base_url, "https://[::1]:8443");
        assert!(!t2.is_public_dns_name);
    }

    #[test]
    fn no_port_resolution_is_origin_independent() {
        // The user-facing principle: a nest is reached the SAME way regardless of
        // client origin — one scheme (https) with the port hidden (:443) —
        // whether the handle's host is loopback, a LAN IP, an mDNS name, or a
        // public domain. The old loopback `http://…:3000` special-case violated
        // this and broke `test@localhost` against a desktop nest serving `:443`
        // (`docs/goal/architecture/installers/windows.md` § Network-reachable nest).
        assert_eq!(
            resolve_handle_domain("localhost").base_url,
            "https://localhost"
        );
        assert_eq!(
            resolve_handle_domain("192.168.1.5").base_url,
            "https://192.168.1.5"
        );
        assert_eq!(
            resolve_handle_domain("pi.local").base_url,
            "https://pi.local"
        );
        assert_eq!(
            resolve_handle_domain("example.com").base_url,
            "https://example.com"
        );
    }

    #[test]
    fn resolve_with_local_port_points_a_loopback_handle_off_443() {
        // The injectable seam (Option A / e2e on a random port): a bare loopback
        // handle resolves to the caller-supplied local nest port. Default 443
        // keeps the port hidden; an injected port is honored. Only loopback is
        // affected — a real remote (LAN / public) nest ignores the local-nest port.
        assert_eq!(
            resolve_handle_domain_with_local_port("localhost", 443).base_url,
            "https://localhost"
        );
        assert_eq!(
            resolve_handle_domain_with_local_port("localhost", 8765).base_url,
            "https://localhost:8765"
        );
        assert_eq!(
            resolve_handle_domain_with_local_port("127.0.0.1", 9001).base_url,
            "https://127.0.0.1:9001"
        );
        assert_eq!(
            resolve_handle_domain_with_local_port("192.168.1.5", 8765).base_url,
            "https://192.168.1.5"
        );
    }

    #[test]
    fn resolve_private_ip_is_not_public_name_but_https() {
        // A non-loopback IP literal still skips DNS (not a public DNS name)
        // but defaults to https, since a LAN/private nest typically
        // terminates TLS.
        let t = resolve_handle_domain("192.168.1.5");
        assert_eq!(t.base_url, "https://192.168.1.5");
        assert!(!t.is_public_dns_name);
        let t2 = resolve_handle_domain("192.168.1.5:3000");
        assert_eq!(t2.base_url, "https://192.168.1.5:3000");
        assert!(!t2.is_public_dns_name);
    }

    #[test]
    fn resolve_registerable_domain_is_https_not_local() {
        let t = resolve_handle_domain("example.com");
        assert_eq!(t.base_url, "https://example.com");
        assert!(t.is_public_dns_name);
    }

    #[test]
    fn resolve_mdns_local_is_not_public_name_https_no_default_port() {
        // A `.local` mDNS hostname is a self-hosted direct-connect target (LAN
        // Pi): it skips the unicast-DNS registration probes (its TLD has no
        // registrar) and defaults to https (self-signed, authenticated via the
        // channel-binding trust model, not a public CA). It resolves to an
        // address via OS-level mDNS at connect time, not here. Like a bare LAN
        // IP, no port is defaulted (443 implied).
        let t = resolve_handle_domain("pi.local");
        assert_eq!(t.base_url, "https://pi.local");
        assert!(!t.is_public_dns_name);
    }

    #[test]
    fn resolve_mdns_local_honors_explicit_port() {
        let t = resolve_handle_domain("raspberrypi.local:8443");
        assert_eq!(t.base_url, "https://raspberrypi.local:8443");
        assert!(!t.is_public_dns_name);
    }

    #[test]
    fn resolve_mdns_local_is_case_insensitive() {
        let t = resolve_handle_domain("Pi.Local");
        assert_eq!(t.base_url, "https://Pi.Local");
        assert!(!t.is_public_dns_name);
    }

    #[test]
    fn resolve_bare_local_label_is_not_mdns_target() {
        // The bare TLD `local` (no host label before the dot) is not a `.local`
        // mDNS name — it stays a (non-registerable) domain, not a local target.
        let t = resolve_handle_domain("local");
        assert_eq!(t.base_url, "https://local");
        assert!(t.is_public_dns_name);
    }

    #[test]
    fn resolve_registerable_domain_with_port_is_not_local() {
        // A real domain keeps its port and stays non-local (still runs DNS).
        let t = resolve_handle_domain("example.com:8443");
        assert_eq!(t.base_url, "https://example.com:8443");
        assert!(t.is_public_dns_name);
    }

    #[test]
    fn resolve_strips_userinfo_before_classifying_and_composing() {
        // The matrix from `security.md` § Transport trust:
        // the classifier used to read `host` (unstripped) but compose
        // `base_url` from the whole `domain`, so a userinfo prefix rode along
        // into the URL even for a target the classifier itself read past.
        // A loopback IP hidden behind userinfo must classify as loopback, not
        // "public" — this was the actual authority-mismatch vector.
        let t = resolve_handle_domain("alice@127.0.0.1");
        assert!(!t.is_public_dns_name);
        assert_eq!(t.base_url, "https://127.0.0.1");

        let t = resolve_handle_domain("alice@localhost");
        assert!(!t.is_public_dns_name);
        assert_eq!(t.base_url, "https://localhost");

        let t = resolve_handle_domain("x@[::1]");
        assert!(!t.is_public_dns_name);
        assert_eq!(t.base_url, "https://[::1]");

        let t = resolve_handle_domain("user:pass@127.0.0.1:8080");
        assert!(!t.is_public_dns_name);
        assert_eq!(t.base_url, "https://127.0.0.1:8080");

        let t = resolve_handle_domain("a@192.168.1.5");
        assert!(!t.is_public_dns_name);
        assert_eq!(t.base_url, "https://192.168.1.5");

        // `.local` must classify false genuinely now (stripped host ends
        // with `.local`), not by the pre-fix accident (a bare `ends_with`
        // over the whole unstripped string).
        let t = resolve_handle_domain("a@nest.local");
        assert!(!t.is_public_dns_name);
        assert_eq!(t.base_url, "https://nest.local");

        // A public host behind userinfo: the class and the compose must now
        // name the SAME authority — the stripped host, never the userinfo.
        let t = resolve_handle_domain("nest.example.com@attacker.example");
        assert!(t.is_public_dns_name);
        assert_eq!(t.base_url, "https://attacker.example");
    }

    #[test]
    fn a_terminator_suffixed_local_string_names_one_authority() {
        // Fixes the residual graded at a security-review verify-back row
        // , filed: `split_host_port` used to
        // hand `is_public_dns_name` and
        // `resolve_handle_domain`'s composer the WHOLE stripped string, reading
        // past where a URL parser would end the authority — at the first of
        // `/ \ ? #`, the same terminator set a sibling finding names for
        // `fauna_core::web::authority_of`. So
        // `"x.example#.local"` used to classify `is_public_dns_name: false` (it
        // ends in `.local`) while composing `base_url =
        // "https://x.example#.local"` — whose real authority, per any WHATWG URL
        // parser, is `x.example`, not a `.local` mDNS name.
        //
        // Now `split_host_port` cuts at the terminator before either side ever
        // sees the tail, so classify and compose both name `x.example`: a public
        // DNS name, base URL `https://x.example` — agreeing with each other AND
        // with what the composed URL actually dials.
        let t = resolve_handle_domain("x.example#.local");
        assert!(t.is_public_dns_name);
        assert_eq!(t.base_url, "https://x.example");

        let t = resolve_handle_domain("x.example?.local");
        assert!(t.is_public_dns_name);
        assert_eq!(t.base_url, "https://x.example");

        let t = resolve_handle_domain("x.example/.local");
        assert!(t.is_public_dns_name);
        assert_eq!(t.base_url, "https://x.example");
    }

    #[test]
    fn split_host_port_stops_at_every_authority_terminator() {
        // The same shared cases `authority_of`, `generic_authority`'s other
        // four callers, and `parse_node_address` pin
        // (`fauna_core::web::AUTHORITY_TERMINATOR_CASES`) — `split_host_port`
        // joined the family fixing,
        // and a narrower `authority_len` should red this alongside every
        // other caller.
        for &(url, expected_host) in fauna_core::web::AUTHORITY_TERMINATOR_CASES {
            let (host, port) = split_host_port(url);
            assert_eq!(host, expected_host, "{url}");
            assert_eq!(port, None, "{url}");
        }
    }

    #[test]
    fn public_dns_host_only_for_registrable_domains() {
        // A registrable public domain → its bare host is returned for the
        // `_fauna self=` lookup (port stripped).
        assert_eq!(public_dns_host("example.com"), Some("example.com"));
        assert_eq!(public_dns_host("example.com:8443"), Some("example.com"));
        // Loopback / IP-literal / `.local` have no DNS authority → None (TOFU).
        assert_eq!(public_dns_host("localhost:3000"), None);
        assert_eq!(public_dns_host("127.0.0.1"), None);
        assert_eq!(public_dns_host("192.168.1.57"), None);
        assert_eq!(public_dns_host("pi.local"), None);
        assert_eq!(public_dns_host("raspberrypi.local:8443"), None);
    }

    #[tokio::test]
    async fn nest_health_probe_returns_reachable_on_200() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/health"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let r = nest_health_probe_at(&client, &server.uri()).await;
        assert_eq!(r.state, NestHealthState::Reachable);
    }

    #[tokio::test]
    async fn nest_health_probe_returns_connection_refused_on_unreachable() {
        let client = reqwest::Client::new();
        // Use a port that's almost certainly closed.
        let r = nest_health_probe_at(&client, "http://127.0.0.1:1").await;
        assert_eq!(r.state, NestHealthState::ConnectionRefused);
    }

    #[tokio::test]
    async fn nest_health_probe_returns_misbehaving_on_5xx() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/health"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let r = nest_health_probe_at(&client, &server.uri()).await;
        assert_eq!(r.state, NestHealthState::Misbehaving { status: 503 });
    }

    // The `nest_challenge_at` wiremock tests were removed alongside the function.
    // The challenge/verify ceremony's behavior
    // is now unit-tested in `fauna-protocol::auth` (`run_silent_challenge`) and
    // proven end-to-end over WS-RPC in `bins/fauna-nest/tests/*roundtrip.rs`.

    #[tokio::test]
    async fn proxy_price_returns_none_when_url_is_none() {
        let client = reqwest::Client::new();
        let r = domain_search_proxy_price_with_url(&client, "example.com", None).await;
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn proxy_price_returns_some_on_proxy_200() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/price"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"tld":"com","registration_cents":1500,"renewal_cents":1500,"currency":"USD"}"#,
            ))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let r =
            domain_search_proxy_price_with_url(&client, "example.com", Some(&server.uri())).await;
        assert!(r.is_some());
        let q = r.unwrap();
        assert_eq!(q.tld, "com");
        assert_eq!(q.registration_cents, 1500);
    }

    #[tokio::test]
    async fn proxy_price_returns_none_on_proxy_5xx() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/price"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let r =
            domain_search_proxy_price_with_url(&client, "example.com", Some(&server.uri())).await;
        assert!(r.is_none());
    }

    #[test]
    fn peer_nest_url_derives_relay_target() {
        // Same-nest (`None`) passes through untouched.
        assert_eq!(peer_nest_url(None), None);
        // A real registerable domain → `https://…` (the home nest relays there).
        assert_eq!(
            peer_nest_url(Some("nest.example.com".into())),
            Some("https://nest.example.com".into()),
        );
        // A loopback authority (the cross-nest *test* topology — F's
        // `handle_domain` is its own `127.0.0.1:<port>`) → `https://…:port`,
        // like every host-class: Pillar C (`730303718`) made the shared resolver
        // uniform-https (a nest serves TLS on every entry path — the self-signed
        // floor), so the test topology serves floor TLS rather than the resolver
        // special-casing loopback to `http`. The end-to-end proof is the native
        // conformance test `conformance_cross_nest_conversations_client.rs`
        // (floor-TLS two-nest fixture) + the browser e2e.
        assert_eq!(
            peer_nest_url(Some("127.0.0.1:8099".into())),
            Some("https://127.0.0.1:8099".into()),
        );
    }
}
