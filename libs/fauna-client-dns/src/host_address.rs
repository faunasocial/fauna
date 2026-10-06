//! Client host-address reporting — the admin client tells the nest its **public
//! IP** so the nest can gate ACME HTTP-01 on the *strong* resolve-check and
//! assemble the apex/`mail.` `A`/`AAAA` rows.
//!
//! Authority: `docs/goal/architecture/nest/domains-and-tls-bootstrap.md`
//! § "Host-address acquisition: the client reports the nest's public IP". The
//! admin client is the authority (it holds the DNS credential, already sets the
//! nest's public DNS, and reliably knows the public IP; a NAT'd nest cannot
//! self-detect). The call is made at onboarding/claim success and again,
//! idempotently, on every later admin-client connect (so an existing box or an
//! IP change converges without re-onboarding). It is an **IPC call, not a UI
//! element** — uniform across all 7 apps (priority #1), no ui.yaml ID.
//!
//! ## The safety invariant — never publish a private/LAN address
//!
//! The client must report the nest's **PUBLIC** address, never its observed
//! connect-address. An admin frequently onboards from the nest's **own LAN**
//! (reaching it at a private IP or an mDNS `.local` name); publishing that as the
//! apex/`mail` `A` record poisons public DNS and guarantees an HTTP-01 failure (a
//! CA cannot reach a private IP). So [`classify_dial_host`] splits the dial-address:
//!
//! - **Public IP literal** → that IP *is* the nest's public address; report it.
//! - **Private / CGNAT / link-local / ULA / `.local` / `localhost`** → the client
//!   shares the nest's NAT, so its WAN IP must come from an **external** reflector
//!   (STUN) — the nest's own STUN server, being *inside* the LAN, would reflect the
//!   client's LAN address, so it is useless here. See below.
//! - **Registrable public name** → resolve it to the IP the connection used; report
//!   it if global, else (split-horizon / hosts-file → private) fall through to STUN.
//!
//! A private/LAN/`.local` address is **never** placed in `nest_ipv4` / `mail_ipv4`.
//!
//! ## LAN active-discovery is deferred
//!
//! Determining the WAN IP in the LAN case needs an external STUN/echo reflector,
//! and none is wired (all `stun_servers` lists are empty; there is no hardcoded
//! public STUN server — the self-hosted invariant; the nest's own STUN server
//! does not work from inside the same LAN). Picking a reflector is a real
//! self-hosted-invariant decision, deferred by the user (2026-07-07). So the STUN
//! step is an **injected capability** ([`HostAddressProbe::stun_public_ipv4`]) that
//! the shipped native + web probes return `None` from today: a pure-LAN box (no
//! public IP determinable) reports **nothing** and keeps the self-signed floor —
//! exactly the spec'd safe fallback (§ "No public IP determinable"). The seam is
//! ready for a reflector to be dropped in without touching this decision tree.

use std::net::{IpAddr, Ipv4Addr};

use fauna_core::resolve::{
    is_global_ip, is_public_dns_name, parse_node_address, strip_ipv6_brackets,
};
use fauna_protocol::RpcRequester;
use fauna_protocol::dns::SetHostAddressRequest;

use crate::DnsAdminClient;

/// How the dial-address the client used to reach the nest classifies for
/// host-address reporting. Pure ([`classify_dial_host`]); the async resolve/STUN
/// follow-ups live in [`report_host_address`] behind [`HostAddressProbe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialClassification {
    /// A publicly-routable IP literal — that IP *is* the nest's public address;
    /// report it directly (no resolve, no STUN).
    PublicIp(IpAddr),
    /// A private/LAN/CGNAT/link-local/`.local`/`localhost` target — the client is
    /// on the nest's LAN, so the public IP comes from an external STUN reflector.
    Lan,
    /// A registrable public *name* — resolve it, then report the resolved global
    /// address (or fall through to STUN if it resolves to a private address).
    Name(String),
}

/// Classify a dial **host** (no scheme/port — pass the host from
/// [`parse_node_address`]). Pure; the IP taxonomy is [`is_global_ip`] and the
/// name/`.local` taxonomy is [`is_public_dns_name`], both shared from
/// `fauna-core`.
pub fn classify_dial_host(host: &str) -> DialClassification {
    // An IP literal (optionally bracketed for IPv6) is classified directly.
    let unbracketed = strip_ipv6_brackets(host);
    if let Ok(ip) = unbracketed.parse::<IpAddr>() {
        return if is_global_ip(ip) {
            DialClassification::PublicIp(ip)
        } else {
            DialClassification::Lan
        };
    }
    // A name past this point. `localhost` / `*.localhost` / `.local` mDNS are LAN
    // targets with no registrable public name (`is_public_dns_name` is also false
    // for IP literals, already handled above).
    if !is_public_dns_name(host) {
        return DialClassification::Lan;
    }
    DialClassification::Name(host.to_string())
}

/// The platform-injected side of host-address determination: DNS resolution and
/// external STUN. Native supplies real implementations; the web (wasm) client —
/// which has neither a DNS resolver nor UDP sockets in the browser — supplies
/// stubs (`resolve_host` → empty, `stun_public_ipv4` → `None`), so web reports
/// only a public IP-literal dial-address and skips otherwise. Plain `async fn in
/// trait` (static-dispatch only, per-impl `Send` inference), mirroring
/// [`fauna_protocol::RpcRequester`].
#[allow(async_fn_in_trait)]
pub trait HostAddressProbe {
    /// Resolve a registrable host name to the IP addresses a connection would
    /// use. Native: a real DNS lookup. Web: `vec![]` (no resolver in-browser).
    async fn resolve_host(&self, host: &str) -> Vec<IpAddr>;

    /// Discover the deployment's public IPv4 via an **external** STUN reflector —
    /// valid only when the client shares the nest's NAT (the LAN case). `None`
    /// when no reflector is configured/reachable. Active LAN WAN-discovery is
    /// deferred, so the shipped native + web probes return `None` here today (a
    /// home-LAN box reports nothing — the spec'd safe fallback).
    async fn stun_public_ipv4(&self) -> Option<Ipv4Addr>;
}

/// The native [`HostAddressProbe`]: real DNS resolution (tokio), and — until an
/// external reflector is wired — a `None` STUN (LAN active-discovery deferred, so
/// a home-LAN box reports nothing: the spec'd safe fallback). Shared by the
/// `fauna-ffi` free fn (windows/apple/android) and the linux app (priority #2);
/// the web (wasm) client supplies its own stub probe (no in-browser resolver / no
/// UDP). `#[cfg(not(wasm32))]` so the wasm build never references `tokio::net`
/// (whose module tokio itself compiles out on wasm).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeHostAddressProbe;

#[cfg(not(target_arch = "wasm32"))]
impl HostAddressProbe for NativeHostAddressProbe {
    async fn resolve_host(&self, host: &str) -> Vec<IpAddr> {
        // The port is irrelevant to the resolved IPs; use the https default so
        // `lookup_host` has a `SocketAddr` shape to return.
        match tokio::net::lookup_host((host, 443)).await {
            Ok(addrs) => addrs.map(|sa| sa.ip()).collect(),
            Err(_) => vec![],
        }
    }
    async fn stun_public_ipv4(&self) -> Option<Ipv4Addr> {
        // Active LAN WAN-discovery is deferred — no external STUN reflector is
        // wired (self-hosted invariant; the nest's own STUN server is inside the
        // LAN and useless here). A home-LAN box reports nothing (safe floor); the
        // seam is ready for a reflector to be dropped in.
        None
    }
}

/// The result of a host-address report attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostAddressOutcome {
    /// Reported this request to the nest (RPC succeeded). `nest_ipv4 == mail_ipv4`
    /// is the deployment's public IPv4.
    Reported(SetHostAddressRequest),
    /// No public IP was determinable (a pure-LAN box with no reflector, or a
    /// name that would not resolve to a global address) → reported **nothing**;
    /// the nest keeps the weak resolve-gate + self-signed floor. Spec'd safe path.
    SkippedNoPublicIp,
    /// The `set_host_address` RPC failed (transport fault or nest rejection).
    /// Non-fatal to onboarding — a later reconnect call retries idempotently.
    Failed(String),
}

/// Determine the nest's public IP from the `dial_url` the client used to reach it,
/// and — when one is determinable — report it via `fauna.dns.set_host_address`.
/// Enforces the safety invariant (never reports a private/LAN address). See the
/// module docs for the decision tree; `probe` injects the platform resolve/STUN.
///
/// Call at onboarding/claim success and idempotently on every admin-client
/// connect (last-writer-wins on the nest, so repeats are harmless).
pub async fn report_host_address<R, P>(
    dns: &DnsAdminClient<R>,
    dial_url: &str,
    probe: &P,
) -> HostAddressOutcome
where
    R: RpcRequester,
    R::Error: core::fmt::Display,
    P: HostAddressProbe,
{
    let (host, _port) = parse_node_address(dial_url);

    // Determine the *public* address to report (or `None` → nothing determinable).
    let public: Option<IpAddr> = match classify_dial_host(&host) {
        DialClassification::PublicIp(ip) => Some(ip),
        // LAN: the WAN IP can only come from an external reflector. Deferred, so
        // this is `None` today → the box reports nothing (safe floor).
        DialClassification::Lan => probe.stun_public_ipv4().await.map(IpAddr::V4),
        DialClassification::Name(name) => {
            let resolved = probe.resolve_host(&name).await;
            match resolved.iter().copied().find(|ip| is_global_ip(*ip)) {
                // Resolves to a global address → that is the public IP; report it.
                Some(global) => Some(global),
                // Resolves only to private addresses (split-horizon / hosts file),
                // or does not resolve → the client may be on the nest's LAN, so
                // fall through to STUN (deferred → `None` today).
                None => probe.stun_public_ipv4().await.map(IpAddr::V4),
            }
        }
    };

    let Some(public) = public else {
        tracing::debug!(dial = %dial_url, "host-address: no public IP determinable — reporting nothing");
        return HostAddressOutcome::SkippedNoPublicIp;
    };

    // `nest_ipv4` / `mail_ipv4` are REQUIRED, so a v4 must exist. STUN is v4-only,
    // and a public-IP dial gives whichever family was used. A v6-only public
    // address cannot fill the required v4 field (an IPv6-only deployment is out of
    // scope for this wire today), so treat it as "nothing determinable".
    let IpAddr::V4(v4) = public else {
        tracing::debug!(dial = %dial_url, "host-address: only a public IPv6 known, but nest_ipv4 is required — reporting nothing");
        return HostAddressOutcome::SkippedNoPublicIp;
    };

    // Single-box default: `mail_ipv4 == nest_ipv4` (one public IP for both roles).
    // A split MX host is a separate advanced admin action that sets `mail_*`.
    let s = v4.to_string();
    let req = SetHostAddressRequest {
        nest_ipv4: s.clone(),
        nest_ipv6: None,
        mail_ipv4: s,
        mail_ipv6: None,
        extra: Default::default(),
    };

    match dns.set_host_address(req.clone()).await {
        Ok(_) => {
            tracing::info!(nest_ipv4 = %req.nest_ipv4, "host-address: reported nest public IP");
            HostAddressOutcome::Reported(req)
        }
        Err(e) => {
            let msg = e.to_string();
            tracing::warn!(error = %msg, "host-address: set_host_address failed (will retry on reconnect)");
            HostAddressOutcome::Failed(msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::block_on;
    use std::sync::Mutex as StdMutex;

    // ── classify_dial_host (pure) ───────────────────────────────────────

    #[test]
    fn public_ipv4_literal_is_public() {
        assert_eq!(
            classify_dial_host("1.1.1.1"),
            DialClassification::PublicIp("1.1.1.1".parse().unwrap())
        );
    }

    #[test]
    fn private_and_cgnat_and_linklocal_literals_are_lan() {
        for s in [
            "10.0.0.5",
            "192.168.1.10",
            "172.16.9.9",
            "100.64.0.1",
            "169.254.1.1",
        ] {
            assert_eq!(classify_dial_host(s), DialClassification::Lan, "{s}");
        }
    }

    #[test]
    fn ipv6_literals_classify_by_globalness() {
        assert_eq!(classify_dial_host("[::1]"), DialClassification::Lan); // loopback
        assert_eq!(classify_dial_host("[fd00::1]"), DialClassification::Lan); // ULA
        assert_eq!(classify_dial_host("[fe80::1]"), DialClassification::Lan); // link-local
        assert_eq!(
            classify_dial_host("[2606:4700:4700::1111]"),
            DialClassification::PublicIp("2606:4700:4700::1111".parse().unwrap())
        );
    }

    #[test]
    fn localhost_and_mdns_names_are_lan() {
        assert_eq!(classify_dial_host("localhost"), DialClassification::Lan);
        assert_eq!(classify_dial_host("pi.local"), DialClassification::Lan);
        assert_eq!(
            classify_dial_host("raspberrypi.local"),
            DialClassification::Lan
        );
    }

    #[test]
    fn registrable_public_name_needs_resolve() {
        assert_eq!(
            classify_dial_host("nest.example.com"),
            DialClassification::Name("nest.example.com".into())
        );
    }

    // ── report_host_address (decision table) ────────────────────────────

    /// Records the last (kind, encoded-payload); answers a `SetHostAddressReply`.
    /// `Mutex` so `Arc<FakeNest>` stays `Send + Sync`. Mirrors the crate's
    /// `RecordingRequester`.
    #[derive(Default)]
    struct FakeNest {
        last: StdMutex<Option<(&'static str, Vec<u8>)>>,
    }

    impl RpcRequester for FakeNest {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            *self.last.lock().unwrap() = Some((kind, bytes.to_vec()));
            assert_eq!(kind, "fauna.dns.set_host_address", "unexpected kind");
            let reply =
                fauna_protocol::encode_canonical(&fauna_protocol::dns::SetHostAddressReply {})
                    .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    /// Injected resolve/STUN outcomes for a test.
    struct FakeProbe {
        resolved: Vec<IpAddr>,
        stun: Option<Ipv4Addr>,
    }
    impl FakeProbe {
        fn none() -> Self {
            Self {
                resolved: vec![],
                stun: None,
            }
        }
    }
    impl HostAddressProbe for FakeProbe {
        async fn resolve_host(&self, _host: &str) -> Vec<IpAddr> {
            self.resolved.clone()
        }
        async fn stun_public_ipv4(&self) -> Option<Ipv4Addr> {
            self.stun
        }
    }

    fn run(dial: &str, probe: FakeProbe) -> (HostAddressOutcome, Option<(&'static str, Vec<u8>)>) {
        let nest = std::sync::Arc::new(FakeNest::default());
        let client = DnsAdminClient::new(nest.clone());
        let outcome = block_on(report_host_address(&client, dial, &probe));
        let recorded = nest.last.lock().unwrap().clone();
        (outcome, recorded)
    }

    fn reported_req(rec: &Option<(&'static str, Vec<u8>)>) -> SetHostAddressRequest {
        let (kind, payload) = rec.as_ref().expect("an RPC was sent");
        assert_eq!(*kind, "fauna.dns.set_host_address");
        fauna_protocol::decode_strict(payload).expect("decodes")
    }

    #[test]
    fn public_ip_dial_reports_that_ip_for_both_roles() {
        let (outcome, rec) = run("https://1.1.1.1:8443", FakeProbe::none());
        assert!(matches!(outcome, HostAddressOutcome::Reported(_)));
        let req = reported_req(&rec);
        assert_eq!(req.nest_ipv4, "1.1.1.1");
        assert_eq!(req.mail_ipv4, "1.1.1.1"); // single-box: mail == nest
        assert_eq!(req.nest_ipv6, None);
        assert_eq!(req.mail_ipv6, None);
    }

    #[test]
    fn private_dial_no_reflector_reports_nothing() {
        // The crux safety case: a same-LAN admin must NOT publish the LAN address.
        let (outcome, rec) = run("https://192.168.1.10:8443", FakeProbe::none());
        assert_eq!(outcome, HostAddressOutcome::SkippedNoPublicIp);
        assert!(
            rec.is_none(),
            "no RPC may be sent for a LAN dial with no reflector"
        );
    }

    #[test]
    fn port_hidden_ipv6_loopback_dial_reports_nothing() {
        // `report_host_address` reaches `classify_dial_host` through
        // `parse_node_address`, so the URL-level form is what matters here. A
        // port-*hidden* `[::1]` used to arrive as the fragment `"[:"`, which is
        // neither parseable as an `IpAddr` nor a `.local`/`localhost` name — so
        // a loopback dial classified as a registrable public `Name` and went to
        // the resolver.
        //
        // The probe below deliberately *does* answer with a global IP: in
        // production `resolve_host("[:")` returns nothing, so the safety
        // invariant ("never report a private/LAN address") held only by the
        // accident that the mangled fragment is unresolvable. This pins the
        // classification itself as what protects it.
        let probe = FakeProbe {
            resolved: vec!["93.184.216.34".parse().unwrap()],
            stun: None,
        };
        let (outcome, rec) = run("https://[::1]", probe);
        assert_eq!(outcome, HostAddressOutcome::SkippedNoPublicIp);
        assert!(
            rec.is_none(),
            "a loopback dial must never publish a host address"
        );
    }

    #[test]
    fn mdns_local_dial_no_reflector_reports_nothing() {
        let (outcome, rec) = run("https://raspberrypi.local:8443", FakeProbe::none());
        assert_eq!(outcome, HostAddressOutcome::SkippedNoPublicIp);
        assert!(rec.is_none());
    }

    #[test]
    fn public_name_resolving_to_global_ip_is_reported() {
        let probe = FakeProbe {
            resolved: vec!["93.184.216.34".parse().unwrap()],
            stun: None,
        };
        let (outcome, rec) = run("https://nest.example.com", probe);
        assert!(matches!(outcome, HostAddressOutcome::Reported(_)));
        assert_eq!(reported_req(&rec).nest_ipv4, "93.184.216.34");
    }

    #[test]
    fn public_name_resolving_to_private_ip_reports_nothing() {
        // Split-horizon DNS resolves the name to a LAN address; with no reflector
        // there is nothing safe to report.
        let probe = FakeProbe {
            resolved: vec!["10.1.2.3".parse().unwrap()],
            stun: None,
        };
        let (outcome, rec) = run("https://nest.example.com", probe);
        assert_eq!(outcome, HostAddressOutcome::SkippedNoPublicIp);
        assert!(rec.is_none());
    }

    #[test]
    fn lan_dial_with_reflector_reports_stun_wan_ip() {
        // Proves the injected-STUN seam works the moment a reflector is wired: a
        // LAN dial + a STUN-discovered WAN IPv4 reports that WAN IP, never the LAN.
        let probe = FakeProbe {
            resolved: vec![],
            stun: Some("203.0.113.50".parse().unwrap()),
        };
        let (outcome, rec) = run("https://192.168.1.10:8443", probe);
        assert!(matches!(outcome, HostAddressOutcome::Reported(_)));
        let req = reported_req(&rec);
        assert_eq!(req.nest_ipv4, "203.0.113.50");
        assert_eq!(req.mail_ipv4, "203.0.113.50");
    }

    #[test]
    fn public_ipv6_only_dial_reports_nothing() {
        // Required nest_ipv4 cannot be filled from a v6-only public dial.
        let (outcome, rec) = run("https://[2606:4700:4700::1111]:8443", FakeProbe::none());
        assert_eq!(outcome, HostAddressOutcome::SkippedNoPublicIp);
        assert!(rec.is_none());
    }
}
