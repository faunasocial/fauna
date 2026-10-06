//! The federation channel pool (Spec Y2 slice 4 §4.D) and the typed
//! `fauna.federation.*` originator wrappers (§6).
//!
//! A nest-wide [`FederationChannelPool`] (held in `AppState`) keyed by the peer
//! `nest_id` holds one live, peer-symmetric [`FederationConnection`] per peer.
//! The first nest-side originator that needs peer `P` and finds no channel dials
//! it (TLS-only, via [`crate::federation_channel::dial`]), pools it, and
//! proceeds; subsequent originators reuse it. The channel is the **sole**
//! Fauna↔Fauna carrier (Spec Y2 slice 5 retired the HTTP interim), so a peer
//! that is unreachable or offers no channel surfaces a [`PoolError`].
//!
//! **Peer `nest_id` resolution.** `dial()` needs the peer's `nest_id` up front
//! (§4.B mutual handshake), but an originator holds only the peer's base URL.
//! The pool resolves URL→`nest_id` via the anonymous `fauna.nest.info` kind
//! over a short-lived [`AnonymousNestClient`] connection (the HTTP
//! `/api/v1/node-info` twin was deleted in the WS-RPC-everywhere rip),
//! caching the result so channel reuse never re-resolves.
//!
//! **Deferred (spec-sanctioned) refinements:** symmetric reuse of an *inbound*
//! (listener-side) channel for origination, and the supervisor-driven
//! *proactive* keepalive reconnect. §4.D permits two channels to briefly
//! coexist ("both valid and harmless"); this pool dials its own outbound channel
//! and re-dials lazily on a stale-channel disconnect. The `nest_id` keying keeps
//! it forward-compatible with adding the inbound index later.

use std::collections::HashMap;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::future::{Either, select};
use tokio::sync::Mutex;

use fauna_protocol::{RpcError, Value};

use crate::federation_channel::{DialError, FederationConnection, dial, to_value, value_to};
use crate::federation_handlers::{
    FedBackupChangesRecordReply, FedBackupChangesRecordRequest, FedBackupWriteTokenMintReply,
    FedBackupWriteTokenMintRequest, FedChannelActorsReply, FedChannelActorsRequest,
    FedChannelAppendReply, FedChannelAppendRequest, FedChannelFetchMessage, FedChannelFetchReply,
    FedChannelFetchRequest, FedChannelLeaveReply, FedChannelLeaveRequest,
    FedConversationWriteTokenMintReply, FedConversationWriteTokenMintRequest,
    FedFolderActorsFetchReply, FedFolderActorsFetchRequest, FedFolderChangesFetchReply,
    FedFolderChangesFetchRequest, FedFolderChangesRecordReply, FedFolderChangesRecordRequest,
    FedFolderContentKeyFetchReply, FedFolderContentKeyFetchRequest, FedFolderPublicFetchReply,
    FedFolderPublicFetchRequest, FedFolderReadTokenMintReply, FedFolderReadTokenMintRequest,
    FedFolderWriteTokenMintReply, FedFolderWriteTokenMintRequest, FedInboxDeliverReply,
    FedInboxDeliverRequest, FedKeypackageFetchReply, FedKeypackageFetchRequest, FedMailAckReply,
    FedMailAckRequest, FedMailPullReply, FedMailPullRequest, FedPostDeleteRequest,
    FedPostForwardRequest, FedPostGetReply, FedPostGetRequest, FedReportsExchangeReply,
    FedReportsExchangeRequest, FedReportsExportReply, FedRoomAcceptReply, FedRoomAcceptRequest,
    FedRoomGenerationsReply, FedRoomGenerationsRequest, FedRoomInviteIssueReply,
    FedRoomInviteIssueRequest, FedRoomInviteReply, FedRoomInviteRequest, FedRoomLabelsReply,
    FedRoomLabelsRequest, FedRoomLeaveReply, FedRoomLeaveRequest, FedRoomRosterReply,
    FedRoomRosterReportReply, FedRoomRosterReportRequest, FedRoomRosterRequest,
    FedSuccessionPushReply, FedSuccessionPushRequest, FedSyncPullReply, FedSyncPullRequest,
    FedSyncPushEntry, FedSyncPushReply, FedSyncPushRequest, FedTrendsExchangeReply,
    FedTrendsExchangeRequest, FedTrendsExportReply, FedWelcomeDeliverReply,
    FedWelcomeDeliverRequest,
};
use crate::feed_routes::{RemoteQueryRequest, RemoteQueryResponse};
use crate::routes::AppState;

/// Default per-request deadline for an originated federation call (mirrors the
/// serving handlers' `default_deadline`).
const ORIGINATE_DEADLINE: Duration = Duration::from_secs(30);

/// How long a negative `resolve_domain_nest_id` result, or an unchanged
/// announced `(handle, domain)` assertion, is treated as still current before
/// the next distinct-assertion check runs a fresh discovery
/// (`federation_handlers::record_announced_handle`,
/// ). One window bounds both
/// halves of the same cost: a peer alternating between domains that never
/// resolve buys at most one discovery **per domain** per window, never one
/// per fetch (the domain-keyed negative cache below); and a domain that was
/// only transiently unreachable is not stuck on its first failure forever —
/// the identical assertion is re-verified once the window passes, because the
/// per-binding record ages out too. The window alone bounds neither cache by
/// *size*, only by the peer's own send rate times the window — see
/// [`MAX_DOMAIN_FAILURE_ENTRIES`] for the fixed ceiling that makes the
/// resident set a function of the cap, never of the peer
/// ().
const ANNOUNCE_VERIFY_TTL: Duration = Duration::from_secs(5 * 60);

/// Hard ceiling on [`FederationChannelPool::domain_resolve_failures`]'s size,
/// independent of [`ANNOUNCE_VERIFY_TTL`] — a TTL sweep alone still bounds
/// memory by the peer's send rate times the window, not by a fixed number, so
/// a peer sending distinct (but syntactically valid, ≤ [`fauna_core::web::
/// MAX_HOSTNAME_BYTES`]) domains fast enough still grows the map without
/// limit inside one window. With the cap, the resident set is `O(cap)`
/// regardless of the peer's rate (). Expired entries are swept before the cap is checked
/// ([`FederationChannelPool::record_domain_failure`]), so a cap hit always
/// means the map genuinely holds this many domains still inside
/// [`ANNOUNCE_VERIFY_TTL`], never stale ones nobody removed.
const MAX_DOMAIN_FAILURE_ENTRIES: usize = 1024;

/// Hard ceiling on concurrently in-flight announce verifications
/// (`FederationChannelPool::in_flight_verification_count`) across every
/// peer — the twin of [`MAX_DOMAIN_FAILURE_ENTRIES`] for the OTHER resource
/// an unbounded announce can spend: an anonymous client connection held open
/// for up to `fauna-anon-client`'s 30 s connect deadline. A small constant
/// (); checked in
/// `federation_handlers::record_announced_handle` **before**
/// [`FederationChannelPool::begin_verification`] records the assertion, so a
/// drop at the cap is never mistaken for a completed (positive or negative)
/// verification — the same assertion is free to try again on the member's
/// next drain instead of being suppressed for the rest of
/// [`ANNOUNCE_VERIFY_TTL`].
pub(crate) const MAX_CONCURRENT_ANNOUNCE_VERIFICATIONS: u64 = 32;

/// Errors establishing or using a pooled federation channel. Since the channel
/// is the **sole** Fauna↔Fauna carrier (the HTTP interim was retired in Spec Y2
/// slice 5), every delivery failure surfaces here — there is no fallback.
#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    /// Could not resolve the peer's `nest_id` from its URL (nest.info failed).
    #[error("resolve peer nest_id: {0}")]
    Resolve(String),
    /// The nest answering the discovery dial could not be authenticated as
    /// the resolved domain: its served cert is not WebPKI-valid and no
    /// carve-out of the discovery trust rule covers the dial
    /// ([`discovery_dial_admits`]; `federation.md` § Peer-auth model →
    /// *Discovery trust rule*). Nothing was sent past the TLS handshake and
    /// nothing was cached — the same connection-teardown posture the client's
    /// first contact keeps (`security.md` § Transport trust).
    #[error("discovery peer not authenticated: {0}")]
    Unauthenticated(String),
    /// [`FederationChannelPool::resolve_domain_nest_id`] found a discovery
    /// for this exact domain already in flight from a concurrent caller.
    /// This call attempted nothing and wrote nothing to the negative cache —
    /// the caller has no answer for this round, but must not treat it as a
    /// confirmed failure the way an ordinary [`Self::Resolve`] is.
    #[error("domain resolution already in flight: {0}")]
    ResolveInFlight(String),
    /// The peer offers the channel but the dial/handshake failed, or the request
    /// could not be delivered (`federation_channel::DialError::Handshake`, or a
    /// dead link that re-dialing didn't recover).
    #[error("dial: {0}")]
    Dial(String),
    /// The peer offers no `/api/v1/federation/ws` (capability negotiation found
    /// no channel). Pre-production every peer serves the channel, so this is a
    /// hard error, not a fallback trigger.
    #[error("peer offers no federation channel")]
    Unsupported,
    /// The nest answering at `peer_url` is not the one the caller pinned.
    ///
    /// Only [`FederationChannelPool::originate_expecting`] can produce this.
    /// The ordinary dial verifies the peer against the id it *discovered from
    /// that same URL*, which proves the peer holds the key it advertises but
    /// says nothing about it being the peer the caller meant — so a URL takeover
    /// with a valid cert silently redirects the call. A caller holding a stored
    /// pin (a registered backup destination) turns that silent redirect into
    /// this loud refusal.
    #[error("peer nest_id mismatch: pinned {expected}, {url} answered as {resolved}")]
    PeerMismatch {
        url: String,
        expected: String,
        resolved: String,
    },
}

/// A nest-wide pool of live federation channels, keyed by verified peer
/// `nest_id` **and the URL that proved it**. See the module docs.
#[derive(Default)]
pub struct FederationChannelPool {
    /// Live dialed channels, keyed by `(verified peer nest_id, peer URL)` (§4.D).
    ///
    /// **The URL is part of the key deliberately**. Keyed on
    /// the `nest_id` alone, a pooled identity made the URL irrelevant: a box
    /// that merely *claimed* a pooled id in its self-declared `nest.info` was
    /// never dialed, so it proved nothing — yet its URL was recorded as that
    /// identity's address. An attacker could arrange the pooling themselves with
    /// one prior honest-URL call, which is what made the address plant
    /// free. With the URL in the key, an address this nest has not dialed is
    /// always dialed, and the handshake is what decides whether it is that
    /// identity. §4.D already declares two channels to one peer harmless, so the
    /// extra entry costs only a connection.
    channels: Mutex<HashMap<([u8; 32], String), Arc<FederationConnection>>>,
    /// Cache of normalized-peer-URL → resolved `nest_id`, so channel reuse
    /// doesn't re-run nest.info on every origination.
    url_nest_ids: Mutex<HashMap<String, [u8; 32]>>,
    /// Cache of handle-domain → the `nest_id` the ordinary discovery chain
    /// lands on for it ([`Self::resolve_domain_nest_id`]), so a foreign
    /// member's announced `handle@domain` is bound to its home nest's key
    /// once per domain, not once per drain.
    domain_nest_ids: Mutex<HashMap<String, [u8; 32]>>,
    /// Negative twin of [`Self::domain_nest_ids`]: a domain that just failed
    /// to resolve (SRV/DNS/connect, wrong peer, or no channel), timestamped.
    /// An entry younger than [`ANNOUNCE_VERIFY_TTL`] short-circuits
    /// [`Self::resolve_domain_nest_id`] to the cached failure without
    /// touching the network, so a peer alternating between domains that
    /// never resolve costs one discovery per domain per window rather than
    /// one per fetch; past the window the domain is retried, so a domain
    /// that was only transiently down is not stuck failing forever. Bounded
    /// at [`MAX_DOMAIN_FAILURE_ENTRIES`] regardless of the window, so the
    /// resident set is a function of the cap, never of the peer's send rate
    /// (;
    /// [`Self::record_domain_failure`]).
    domain_resolve_failures: Mutex<HashMap<String, Instant>>,
    /// Domains with a [`Self::resolve_domain_nest_id`] discovery attempt in
    /// flight right now — singleflight per domain
    /// (): a concurrent call naming
    /// a domain found here short-circuits ([`PoolError::ResolveInFlight`])
    /// rather than starting a second network attempt, so *K* fetches naming
    /// *K* distinct handles at one hanging domain spend at most one
    /// discovery, not *K* concurrent ones each alive for the anonymous
    /// client's 30 s connect deadline. Deliberately **not**
    /// [`Self::domain_resolve_failures`] — an in-flight domain is not a
    /// failure, and folding it into that map would cache a "failure" nobody
    /// actually observed, wrongly suppressing a concurrent binding's
    /// identical assertion for the rest of [`ANNOUNCE_VERIFY_TTL`] even
    /// though the domain may resolve moments later.
    domain_resolve_inflight: Mutex<std::collections::HashSet<String>>,
    /// Count of `resolve_domain_nest_id` calls that actually attempted a
    /// discovery (cache miss, on either the positive or negative cache) —
    /// test-observable so a witness can assert the TTL bound without timing
    /// (`e2e-conventions.md` convention 14).
    resolve_attempts: AtomicU64,
    /// Count of announce verifications currently spawned and not yet
    /// finished — test-observable so a witness can wait for quiescence
    /// before reading [`Self::resolve_attempts`] or DB state, without a
    /// fixed sleep.
    in_flight_verifications: AtomicU64,
    /// The last `(handle, domain)` each foreign member's home nest announced
    /// on that member's drain, keyed by `(channel_id, actor_id)` — recorded
    /// whatever the verification's eventual outcome, and generation-numbered.
    /// An announce identical to the last one, inside [`ANNOUNCE_VERIFY_TTL`],
    /// is a no-op before any lookup; the generation lets a verification that
    /// finishes after a newer assertion superseded it discard its own
    /// (stale) result instead of overwriting the newer one
    /// (`federation_handlers::record_announced_handle`).
    announced_handles: Mutex<HashMap<([u8; 32], [u8; 32]), AnnouncedHandle>>,
    /// The normalized `nest_url` of every local pairing row, each with what
    /// the rows say about it ([`PairingTargetTrust`]): whether an admin's row
    /// names it (exempt from the SSRF guard's global-address arm —
    /// [`crate::federation_channel::PeerUrlOrigin::Configured`]) and the
    /// `nest_id` it is pinned to. Rebuilt wholesale from the rows and the
    /// admin roster by [`Self::set_pairing_targets`] — never a stored field.
    pairing_targets: std::sync::RwLock<HashMap<String, PairingTargetTrust>>,
}

/// What this nest's pairing rows say about one peer URL
/// ([`FederationChannelPool::set_pairing_targets`], built by
/// `nest_sync_worker::refresh_pairing_targets`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PairingTargetTrust {
    /// A pairing row whose actor is an admin of this nest names the URL: it is
    /// the deployment's own topology, chosen by the one human who chooses it,
    /// so dialing it is validated as
    /// [`crate::federation_channel::PeerUrlOrigin::Configured`]
    /// (`private-mode.md` § Pairing Flow, re-decided 2026-10-01).
    pub exempt: bool,
    /// The `nest_id` the rows naming the URL record, when they agree on one.
    pub pin: Option<[u8; 32]>,
}

/// One binding's last-seen announce — see
/// [`FederationChannelPool::announced_handles`].
struct AnnouncedHandle {
    handle: String,
    domain: String,
    recorded_at: Instant,
    generation: u64,
}

/// The address class of a peer URL's host — with the URL's
/// [`PeerUrlOrigin`](crate::federation_channel::PeerUrlOrigin), what decides which arm of the discovery trust rule
/// ([`discovery_dial_admits`]) a dial falls under. Established by
/// [`classify_peer_host_resolving`] on the production path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerHostClass {
    /// A loopback literal (`127.0.0.1`, `localhost`, `::1`) — the in-process
    /// fixture, the one host class `validate_peer_url` admits over plaintext.
    Loopback,
    /// A private / link-local / CGNAT / ULA address, or a name **every**
    /// address of which is one — the LAN, a VPN, the docker bridge network of
    /// the two-box witness. Reachable only as the deployment's own configured
    /// pull target (`validate_peer_url`'s SSRF arm refuses it for a
    /// request-named URL).
    NonGlobal,
    /// A globally routable address, a name resolving to at least one such
    /// (a mixed answer set is judged by its strictest member), or a name that
    /// does not resolve — every production federation peer.
    Global,
}

/// A peer host's class together with the address the discovery dial must
/// connect to — established from **one** resolution, so the class judges the
/// very address that is dialed (a second lookup at dial
/// time could otherwise answer differently, a DNS-rebinding pair or a mixed
/// set turning a public pull target into a `NonGlobal` one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassifiedPeerHost {
    pub class: PeerHostClass,
    /// The resolved address to dial, one the class was judged from. `None`
    /// for a literal host (the literal *is* the dial target, no DNS) and for a
    /// name that did not resolve (classed `Global`, the strictest).
    pub dial_addr: Option<std::net::SocketAddr>,
}

/// Classify one resolution's answer set: `NonGlobal` only when **every**
/// address is non-global (dialing the first of them); any global address makes
/// the set `Global` and the dial goes to a global address, so a mixed answer
/// can never reach the configured-private carve-out. An empty set is `Global`
/// with nothing to dial.
pub fn classify_resolved_addrs(addrs: &[std::net::SocketAddr]) -> ClassifiedPeerHost {
    use fauna_core::resolve::is_global_ip;
    match addrs.iter().find(|a| is_global_ip(a.ip())) {
        Some(global) => ClassifiedPeerHost {
            class: PeerHostClass::Global,
            dial_addr: Some(*global),
        },
        None => ClassifiedPeerHost {
            class: if addrs.is_empty() {
                PeerHostClass::Global
            } else {
                PeerHostClass::NonGlobal
            },
            dial_addr: addrs.first().copied(),
        },
    }
}

/// Why a discovery dial was admitted — the arm of [`discovery_dial_admits`]
/// that fired, so a log line and a test can name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryTrust {
    /// The served cert chains to the public WebPKI roots for the dialed
    /// authority — the rule's one production arm.
    WebPki,
    /// A plaintext `http://` dial: no cert to judge. `validate_peer_url`
    /// bounds plaintext to loopback, so this is the legacy in-process fixture.
    Plaintext,
    /// A self-signed cert on a loopback literal — the in-process floor-TLS
    /// fixture, under the same test-only carve-out `validate_peer_url`
    /// documents (and with the same consciously-accepted production residual:
    /// a loopback service is inside the box's own trust boundary).
    LoopbackFixture,
    /// A self-signed (or hostname-mismatched) cert on the deployment's own
    /// pull target (an admin's pairing row) at a private address: a home box reaching its
    /// public relay over the LAN, a VPN or the docker bridge, where the
    /// relay's public-domain cert cannot match the private name it is dialed
    /// by (`deployment-home-with-public-relay.md`; the address carve-out
    /// ruled). **Pinned:** once the private
    /// nest's pairing rows name the public nest's `nest_id`
    /// ([`FederationChannelPool::set_pairing_targets`]), the answer admitted
    /// here is refused as [`PoolError::PeerMismatch`] unless it is that id —
    /// and the federation `hello` then binds the id to the served SPKI, so an
    /// impostor on the private network cannot pass as the relay. Before the
    /// first pairing row is seeded there is nothing to expect, and the private
    /// network is the trust boundary for that window.
    ConfiguredPrivate,
}

/// **The discovery trust rule** (`federation.md` § Peer-auth model →
/// *Discovery trust rule*, ruled 2026-09-25): whether the nest answering a
/// discovery dial is authenticated as the authority that was dialed, judged
/// from the cert the capturing verifier saw. `Err` carries the reason;
/// [`FederationChannelPool::resolve_peer_nest_id_in_class`] turns it into
/// [`PoolError::Unauthenticated`] before any request is sent.
///
/// The arms, in order: no TLS → nothing to judge ([`DiscoveryTrust::Plaintext`]);
/// WebPKI-valid → authenticated ([`DiscoveryTrust::WebPki`]); a loopback
/// literal → the fixture carve-out ([`DiscoveryTrust::LoopbackFixture`]); the
/// deployment's own [`PeerUrlOrigin::Configured`](crate::federation_channel::PeerUrlOrigin::Configured) target at a
/// [`PeerHostClass::NonGlobal`] address → the topology carve-out
/// ([`DiscoveryTrust::ConfiguredPrivate`]); anything else — a request-named
/// URL, a handle domain, or a configured target on the public internet,
/// serving a cert the WebPKI does not trust for that authority — is refused.
/// **There is deliberately no trust-on-first-use arm**: the client's TOFU rung
/// exists for a home nest with no DNS authority (LAN, `.local`), and a
/// federation peer has one by construction (a domain, or a public IP the IP
/// bridge cert covers), so a peer the WebPKI cannot vouch for is a
/// misconfigured or impersonated peer, never a benign one. DNS `self=` /
/// DNSSEC remain the hardening the owner doc names for later.
pub fn discovery_dial_admits(
    tls: bool,
    class: PeerHostClass,
    origin: crate::federation_channel::PeerUrlOrigin,
    captured: &fauna_ws_substrate::tls_verify::CapturedCert,
) -> Result<DiscoveryTrust, &'static str> {
    use crate::federation_channel::PeerUrlOrigin;
    if !tls {
        return Ok(DiscoveryTrust::Plaintext);
    }
    if captured.webpki_valid {
        return Ok(DiscoveryTrust::WebPki);
    }
    match (class, origin) {
        (PeerHostClass::Loopback, _) => Ok(DiscoveryTrust::LoopbackFixture),
        (PeerHostClass::NonGlobal, PeerUrlOrigin::Configured) => {
            Ok(DiscoveryTrust::ConfiguredPrivate)
        }
        (PeerHostClass::NonGlobal, PeerUrlOrigin::Supplied) | (PeerHostClass::Global, _) => Err(
            "served cert is not WebPKI-valid for the dialed authority (federation.md \
             § Peer-auth model → Discovery trust rule)",
        ),
    }
}

/// Establish a peer URL's [`PeerHostClass`] for [`discovery_dial_admits`],
/// with the address the dial must use: a loopback literal by text, an IP
/// literal by [`fauna_core::resolve::is_global_ip`] (no DNS, nothing to pin),
/// a name by resolving it **once** and classifying that answer set
/// ([`classify_resolved_addrs`] — `NonGlobal` only when every address is).
/// A name that does not resolve is classed `Global` — the strictest class, so
/// an unresolvable name can only ever be *more* refused, never admitted
/// through a carve-out. Runs after `validate_peer_url`, which has already
/// refused what is undialable.
pub async fn classify_peer_host_resolving(peer_url: &str) -> Result<ClassifiedPeerHost, PoolError> {
    let url = url::Url::parse(peer_url.trim_end_matches('/'))
        .map_err(|_| PoolError::Resolve(format!("invalid peer URL: {peer_url}")))?;
    let host = url
        .host()
        .ok_or_else(|| PoolError::Resolve(format!("peer URL has no host: {peer_url}")))?;
    let literal = |class| ClassifiedPeerHost {
        class,
        dial_addr: None,
    };
    let ip_class = |ip: std::net::IpAddr| {
        // Exactly the literals the text test used to name — not all of
        // 127/8, which stays `NonGlobal` as before.
        if ip == std::net::IpAddr::from([127, 0, 0, 1]) || ip == std::net::Ipv6Addr::LOCALHOST {
            PeerHostClass::Loopback
        } else if fauna_core::resolve::is_global_ip(ip) {
            PeerHostClass::Global
        } else {
            PeerHostClass::NonGlobal
        }
    };
    let name = match host {
        url::Host::Ipv4(ip) => return Ok(literal(ip_class(ip.into()))),
        url::Host::Ipv6(ip) => return Ok(literal(ip_class(ip.into()))),
        url::Host::Domain("localhost") => {
            return Ok(literal(PeerHostClass::Loopback));
        }
        url::Host::Domain(name) => name,
    };
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs: Vec<std::net::SocketAddr> = match tokio::net::lookup_host((name, port)).await {
        Ok(addrs) => addrs.collect(),
        Err(_) => Vec::new(),
    };
    Ok(classify_resolved_addrs(&addrs))
}

/// [`classify_peer_host_resolving`]'s class alone.
pub async fn classify_peer_host(peer_url: &str) -> Result<PeerHostClass, PoolError> {
    Ok(classify_peer_host_resolving(peer_url).await?.class)
}

impl FederationChannelPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the pairing-target table with `targets` (keys are normalized
    /// here). Called by `nest_sync_worker::refresh_pairing_targets` before
    /// every worker pass, after every pairing write and after every admin
    /// roster change, so the exemption follows the admin role as it stands
    /// when the dial is made. From then on every discovery of a pinned URL —
    /// fresh or cached, whichever worker asks — is refused as
    /// [`PoolError::PeerMismatch`] unless the answering nest is the pinned id
    /// (`federation.md` § Peer-auth model → *Discovery trust rule*, carve-out
    /// (2)).
    pub fn set_pairing_targets(&self, targets: HashMap<String, PairingTargetTrust>) {
        let normalized = targets
            .into_iter()
            .map(|(url, trust)| (url.trim_end_matches('/').to_string(), trust))
            .collect();
        if let Ok(mut map) = self.pairing_targets.write() {
            *map = normalized;
        }
    }

    /// What the pairing rows say about `peer_url`, if any row names it.
    pub fn pairing_target(&self, peer_url: &str) -> Option<PairingTargetTrust> {
        let key = peer_url.trim_end_matches('/');
        self.pairing_targets
            .read()
            .ok()
            .and_then(|map| map.get(key).copied())
    }

    /// Refuse `resolved` for `key` unless it is the pin the pairing rows hold
    /// for that URL (no pin → nothing to expect). The one decision point
    /// every discovery of a paired URL passes, cached or fresh.
    fn check_target_pin(&self, key: &str, resolved: &[u8; 32]) -> Result<(), PoolError> {
        match self.pairing_target(key).and_then(|t| t.pin) {
            Some(pin) if pin != *resolved => Err(PoolError::PeerMismatch {
                url: key.to_string(),
                expected: hex::encode(pin),
                resolved: hex::encode(resolved),
            }),
            _ => Ok(()),
        }
    }

    /// Whether `peer_url` is the deployment's own topology (an admin's pairing
    /// row names it) or was named by a request, a peer or any other user.
    pub fn peer_url_origin(&self, peer_url: &str) -> crate::federation_channel::PeerUrlOrigin {
        if self.pairing_target(peer_url).is_some_and(|t| t.exempt) {
            crate::federation_channel::PeerUrlOrigin::Configured
        } else {
            crate::federation_channel::PeerUrlOrigin::Supplied
        }
    }

    /// Resolve a peer's `nest_id` from its base URL via the anonymous
    /// `fauna.nest.info` kind (cached). The TLS that authenticates the peer's
    /// domain is the trust anchor — and it authenticates only when the
    /// **discovery trust rule** ([`discovery_dial_admits`]) admits the served
    /// cert: WebPKI-valid for the dialed authority, or one of the rule's two
    /// documented carve-outs (the loopback fixture; the deployment's own
    /// private pull target). Until that check passes the dial is
    /// encrypt-only (`AnonymousNestClient::connect` rides the capturing
    /// verifier's `AcceptProvisional`), so the `nest.info` answer of a peer
    /// the rule refuses is never requested, let alone believed — an attacker
    /// who owns the domain's resolution or sits on-path needs a CA-issued
    /// cert for that domain, which is exactly the bar `federation.md`
    /// § Peer-auth model sets (ruled 2026-09-25). The federation `hello` then re-binds the
    /// resolved id to the served SPKI, so the id the dial mutually verifies is
    /// the one the authenticated domain served.
    ///
    /// **A paired URL is pinned.** For a URL a local pairing row names, the
    /// id — fresh or cached — must also be the one the rows record
    /// ([`Self::set_pairing_targets`]), else [`PoolError::PeerMismatch`]; a
    /// cached answer that disagrees with a pin set after it was cached is
    /// evicted, so the next call re-dials.
    ///
    /// The host is resolved **once** ([`classify_peer_host_resolving`]) and the
    /// dial goes to an address from that same answer, so the class the rule
    /// judges is the class of the address actually dialed.
    ///
    /// `pub` so the conversations welcome relay can record a foreign channel
    /// member's **home** `nest_id` (resolved from the relay `peer_url`) to later
    /// authorize that member's `fauna.federation.channel.fetch` — the resolution is
    /// cached, so it costs nothing beyond the welcome relay's own dial.
    pub async fn resolve_peer_nest_id(&self, peer_url: &str) -> Result<[u8; 32], PoolError> {
        let key = peer_url.trim_end_matches('/').to_string();
        {
            let mut cache = self.url_nest_ids.lock().await;
            if let Some(id) = cache.get(&key).copied() {
                if let Err(e) = self.check_target_pin(&key, &id) {
                    cache.remove(&key);
                    return Err(e);
                }
                return Ok(id);
            }
        }
        crate::federation_channel::validate_peer_url(&key, self.peer_url_origin(&key))
            .await
            .map_err(|e| PoolError::Resolve(e.to_string()))?;
        let host = classify_peer_host_resolving(&key).await?;
        self.discover_nest_id(&key, host).await
    }

    /// The seam beneath [`Self::resolve_peer_nest_id`]: the dial, the
    /// discovery trust rule and the cache write, for a peer URL whose host
    /// class the caller has already established. **Production goes through
    /// `resolve_peer_nest_id`**, which validates the URL and classifies the
    /// host itself; this is `pub` so the conformance suite can put an
    /// in-process floor-TLS fixture — which only ever listens on loopback — in
    /// the [`PeerHostClass::Global`] class and watch the rule refuse it on the
    /// real wire, with the real captured cert (the loopback carve-out would
    /// otherwise admit every fixture and leave the refusal arm untested;
    /// `conformance_discovery_tls_root.rs`). Same shape as
    /// `validate_peer_url`'s explicit
    /// [`PeerUrlOrigin`](crate::federation_channel::PeerUrlOrigin) parameter.
    /// The dial resolves the host itself (no pinned address).
    pub async fn resolve_peer_nest_id_in_class(
        &self,
        peer_url: &str,
        class: PeerHostClass,
    ) -> Result<[u8; 32], PoolError> {
        let key = peer_url.trim_end_matches('/').to_string();
        let host = ClassifiedPeerHost {
            class,
            dial_addr: None,
        };
        self.discover_nest_id(&key, host).await
    }

    /// The dial, the discovery trust rule, the paired URL's pin and the
    /// cache write, for a normalized `key` whose host is already classified —
    /// dialing `host.dial_addr` when the classification resolved one (SNI and
    /// cert identity still come from `key`).
    async fn discover_nest_id(
        &self,
        key: &str,
        host: ClassifiedPeerHost,
    ) -> Result<[u8; 32], PoolError> {
        let key = key.to_string();
        let class = host.class;
        let origin = self.peer_url_origin(&key);
        let anon = fauna_anon_client::AnonymousNestClient::connect_resolving(&key, host.dial_addr)
            .await
            .map_err(|e| PoolError::Resolve(format!("anon connect: {e}")))?;
        // The rule runs between the TLS handshake and the first request, so a
        // refused peer is told nothing (the connection-teardown rule).
        let tls = key.starts_with("https://");
        let admitted = discovery_dial_admits(tls, class, origin, &anon.captured_cert())
            .map_err(|why| PoolError::Unauthenticated(format!("{key}: {why}")))?;
        tracing::debug!(peer_url = %key, ?admitted, "discovery dial admitted");
        let reply: fauna_protocol::discovery::NestInfoReply = anon
            .request(
                "fauna.nest.info",
                fauna_protocol::discovery::NestInfoRequest::default(),
            )
            .await
            .map_err(|e| PoolError::Resolve(format!("nest.info: {e}")))?;

        let id: [u8; 32] = fauna_core::hex32::decode(&reply.nest_id)
            .map_err(|e| PoolError::Resolve(format!("nest_id: {e}")))?;
        // A pinned paired URL answered by another nest: refused before the
        // answer is cached, so it is refused again on the next call.
        self.check_target_pin(&key, &id)?;
        self.url_nest_ids.lock().await.insert(key, id);
        Ok(id)
    }

    /// The `nest_id` a **handle domain** resolves to — the ordinary anonymous
    /// discovery chain of `federation.md` § Peer-auth model run from the
    /// domain end (`https://{domain}` → the shared SRV/port resolver →
    /// `fauna.nest.info` over authenticated TLS), cached per domain.
    ///
    /// This is the one honest way to bind a domain to a key: the TLS
    /// certificate for the *resolved* domain vouches for the nest answering
    /// there (§ Security → *Domain↔key binding rides TLS*) — a vouch
    /// [`Self::resolve_peer_nest_id`]'s discovery trust rule makes real by
    /// refusing a served cert that is not WebPKI-valid for the resolved
    /// authority (a handle domain is always a request-named, globally-routable
    /// target, so neither carve-out reaches it). A nest's own
    /// `nest.info.domain` at some URL is only that nest's claim — any nest may
    /// advertise any domain string — which is why a foreign member's
    /// announced `handle@domain` is verified here, from the domain, and never
    /// by reading the announcing nest's self-description
    /// (`federation_handlers::record_announced_handle`).
    ///
    /// The same loopback carve-out `validate_peer_url` documents applies: an
    /// in-process fixture's `127.0.0.1:PORT` authority resolves to itself, as
    /// it does for a client resolving `bob@127.0.0.1:PORT`.
    ///
    /// **Syntax-gated.** A domain that is not a well-formed `host[:port]`
    /// authority ([`fauna_core::web::is_domain_authority_syntax`] — accepts
    /// the `127.0.0.1:PORT` loopback shape every in-process fixture
    /// announces, unlike the stricter registration-time FQDN contract) is
    /// refused before touching either cache or the network
    /// ().
    ///
    /// **Bounded even against a domain that never resolves.** A failure is
    /// cached too (negative twin of the positive cache above), timestamped;
    /// an entry inside [`ANNOUNCE_VERIFY_TTL`] short-circuits to the cached
    /// failure without touching the network, so a peer alternating between
    /// two non-resolving domains costs at most one discovery **per domain**
    /// per window, never one per fetch — and past the window the domain gets
    /// a fresh attempt, so one that was only transiently unreachable is not
    /// stuck failing forever ().
    /// The negative cache is itself capped at [`MAX_DOMAIN_FAILURE_ENTRIES`]
    /// ().
    ///
    /// **Singleflight per domain.** A domain already being resolved by a
    /// concurrent caller short-circuits to [`PoolError::ResolveInFlight`]
    /// instead of starting a second network attempt, so *K* fetches naming
    /// *K* distinct handles at one hanging domain cost one discovery, not
    /// *K* concurrent ones ().
    pub async fn resolve_domain_nest_id(&self, domain: &str) -> Result<[u8; 32], PoolError> {
        let key = domain.trim().trim_end_matches('/').to_ascii_lowercase();
        if !fauna_core::web::is_domain_authority_syntax(&key) {
            return Err(PoolError::Resolve(format!(
                "not a handle domain: {domain:?}"
            )));
        }
        if let Some(id) = self.domain_nest_ids.lock().await.get(&key) {
            return Ok(*id);
        }
        if let Some(failed_at) = self.domain_resolve_failures.lock().await.get(&key)
            && failed_at.elapsed() < ANNOUNCE_VERIFY_TTL
        {
            return Err(PoolError::Resolve(format!(
                "domain did not resolve within the last {:?} (cached failure)",
                ANNOUNCE_VERIFY_TTL
            )));
        }
        {
            let mut inflight = self.domain_resolve_inflight.lock().await;
            if !inflight.insert(key.clone()) {
                return Err(PoolError::ResolveInFlight(key));
            }
        }
        self.resolve_attempts.fetch_add(1, Ordering::Relaxed);
        let url = fauna_core::resolve::resolve_full_url(&format!("https://{key}")).await;
        let result = self.resolve_peer_nest_id(&url).await;
        self.domain_resolve_inflight.lock().await.remove(&key);
        match result {
            Ok(id) => {
                self.domain_nest_ids.lock().await.insert(key.clone(), id);
                self.domain_resolve_failures.lock().await.remove(&key);
                Ok(id)
            }
            Err(e) => {
                self.record_domain_failure(key).await;
                Err(e)
            }
        }
    }

    /// The `nest_id` a previous [`Self::resolve_domain_nest_id`] bound `domain`
    /// to, **without dialing** — `None` on a cold (or failed) domain. The
    /// polled paths that must never be delayed by a name read this, and spawn
    /// the full binding on a miss so the next poll finds it warm
    /// (`federation.md` § Cross-nest shared folders + channel append → *The
    /// cross-nest owner label*, *When the binding runs*). Same key
    /// normalisation as the resolver.
    pub async fn cached_domain_nest_id(&self, domain: &str) -> Option<[u8; 32]> {
        let key = domain.trim().trim_end_matches('/').to_ascii_lowercase();
        self.domain_nest_ids.lock().await.get(&key).copied()
    }

    /// Test-only: seed the two resolve caches as a completed discovery would
    /// — `peer_url` → `nest_id`, and optionally `domain` → `domain_id`.
    #[cfg(test)]
    pub(crate) async fn seed_bindings_for_test(
        &self,
        peer_url: &str,
        nest_id: [u8; 32],
        domain: Option<(&str, [u8; 32])>,
    ) {
        self.url_nest_ids
            .lock()
            .await
            .insert(peer_url.trim_end_matches('/').to_string(), nest_id);
        if let Some((domain, id)) = domain {
            self.domain_nest_ids
                .lock()
                .await
                .insert(domain.to_ascii_lowercase(), id);
        }
    }

    /// Insert a negative-cache entry for `key`, bounded at
    /// [`MAX_DOMAIN_FAILURE_ENTRIES`] — a TTL sweep alone still bounds
    /// memory by the peer's send rate times [`ANNOUNCE_VERIFY_TTL`], not by
    /// a fixed number (). Expired
    /// entries are swept before the cap is checked, so a cap hit means the
    /// map genuinely holds `MAX_DOMAIN_FAILURE_ENTRIES` domains still inside
    /// the window, not stale ones nobody removed; past the cap the newest
    /// domain is simply not cached, and gets a fresh attempt on its very
    /// next assertion instead of a cached failure — the resident set stays a
    /// function of the cap, never of the peer.
    async fn record_domain_failure(&self, key: String) {
        let mut failures = self.domain_resolve_failures.lock().await;
        let now = Instant::now();
        failures.retain(|_, at| now.duration_since(*at) < ANNOUNCE_VERIFY_TTL);
        if failures.len() < MAX_DOMAIN_FAILURE_ENTRIES || failures.contains_key(&key) {
            failures.insert(key, now);
        }
    }

    /// Count of live entries in [`Self::domain_resolve_failures`] — test-only
    /// observability for [`MAX_DOMAIN_FAILURE_ENTRIES`].
    pub async fn domain_resolve_failure_count(&self) -> usize {
        self.domain_resolve_failures.lock().await.len()
    }

    /// Count of [`Self::resolve_domain_nest_id`] calls that actually
    /// attempted a discovery (never a cache hit, positive or negative) —
    /// test-only observability, see the field doc.
    pub fn domain_resolve_attempt_count(&self) -> u64 {
        self.resolve_attempts.load(Ordering::Relaxed)
    }

    /// Count of announce verifications spawned and not yet finished —
    /// test-only observability, see the field doc.
    pub fn in_flight_verification_count(&self) -> u64 {
        self.in_flight_verifications.load(Ordering::Relaxed)
    }

    /// Mark one verification as started; pairs with
    /// [`Self::note_verification_finished`], which the spawned task must
    /// call exactly once on every exit path (an RAII guard at the call site
    /// makes that automatic).
    pub(crate) fn note_verification_started(&self) {
        self.in_flight_verifications.fetch_add(1, Ordering::Relaxed);
    }

    /// Retire one verification, whatever its outcome — see
    /// [`Self::note_verification_started`].
    pub(crate) fn note_verification_finished(&self) {
        self.in_flight_verifications.fetch_sub(1, Ordering::Relaxed);
    }

    /// Decide whether `(handle, domain)` for `(channel_id, actor_id)` is
    /// worth a fresh verification: distinct from the last recorded
    /// assertion, or the last one aged out of [`ANNOUNCE_VERIFY_TTL`].
    ///
    /// When it is, records the new assertion — whatever the eventual
    /// verification outcome, exactly like the field doc's predecessor
    /// promised, now TTL-bound rather than for the process lifetime — and
    /// returns this binding's new generation. The caller's spawned
    /// verification must present that generation back to
    /// [`Self::is_current_verification`] before writing anything: a slower,
    /// now-superseded verification then discards its own result instead of
    /// clobbering a newer one. `None` means: nothing to do, don't spawn.
    pub async fn begin_verification(
        &self,
        channel_id: [u8; 32],
        actor_id: [u8; 32],
        handle: &str,
        domain: &str,
    ) -> Option<u64> {
        let mut seen = self.announced_handles.lock().await;
        let key = (channel_id, actor_id);
        let now = Instant::now();
        let generation = match seen.get(&key) {
            Some(existing)
                if existing.handle == handle
                    && existing.domain == domain
                    && now.duration_since(existing.recorded_at) < ANNOUNCE_VERIFY_TTL =>
            {
                return None;
            }
            Some(existing) => existing.generation + 1,
            None => 1,
        };
        seen.insert(
            key,
            AnnouncedHandle {
                handle: handle.to_string(),
                domain: domain.to_string(),
                recorded_at: now,
                generation,
            },
        );
        Some(generation)
    }

    /// `true` iff `generation` is still `(channel_id, actor_id)`'s latest —
    /// the caller may store its verified result. `false` means a newer
    /// assertion has already superseded it, and this (now stale) result must
    /// be discarded rather than overwrite the newer one.
    pub async fn is_current_verification(
        &self,
        channel_id: [u8; 32],
        actor_id: [u8; 32],
        generation: u64,
    ) -> bool {
        self.announced_handles
            .lock()
            .await
            .get(&(channel_id, actor_id))
            .is_some_and(|e| e.generation == generation)
    }

    /// Undo [`Self::begin_verification`]'s recorded assertion for
    /// `(channel_id, actor_id)` — but ONLY if it is still at `generation`, so
    /// a newer assertion that has already superseded it is never clobbered
    /// (same generation guard as [`Self::is_current_verification`]).
    ///
    /// For when the verification that would have confirmed or refuted the
    /// assertion never actually ran — a concurrent
    /// [`PoolError::ResolveInFlight`] collision on the same domain
    /// (). Without this, the
    /// binding's assertion stays marked "seen" for the rest of
    /// [`ANNOUNCE_VERIFY_TTL`] even though nothing was ever checked, so an
    /// honest member racing another on first contact with a shared home
    /// domain could go nameless for the whole window. Discarding lets the
    /// member's very next drain — carrying the identical assertion — start a
    /// fresh verification instead of being suppressed by a check that never
    /// happened.
    pub async fn discard_verification(
        &self,
        channel_id: [u8; 32],
        actor_id: [u8; 32],
        generation: u64,
    ) {
        let mut seen = self.announced_handles.lock().await;
        if seen
            .get(&(channel_id, actor_id))
            .is_some_and(|e| e.generation == generation)
        {
            seen.remove(&(channel_id, actor_id));
        }
    }

    /// Reuse a pooled channel to `peer_nest_id` **at `peer_url`**, or dial it.
    ///
    /// Reuse requires both halves to match: a URL this nest has never dialed is
    /// dialed now, so it must complete the signed `fauna.federation.hello`
    /// handshake as `peer_nest_id` before anything treats it as that identity
    /// (see the [`Self::channels`] field docs).
    ///
    /// A successful dial IS that possession proof, so it is also the honest
    /// place to stamp the address directory: `proven_at` is written here, at the
    /// moment the proof happens, never inferred by a downstream caller.
    async fn get_or_dial(
        &self,
        state: &Arc<AppState>,
        peer_url: &str,
        peer_nest_id: [u8; 32],
    ) -> Result<Arc<FederationConnection>, DialError> {
        let key = (peer_nest_id, peer_url.trim_end_matches('/').to_string());
        if let Some(c) = self.channels.lock().await.get(&key) {
            return Ok(Arc::clone(c));
        }
        let conn = dial(state, peer_url, &hex::encode(peer_nest_id)).await?;
        // The handshake bound this URL to this identity: record the proof.
        // Best-effort — a directory write must never fail an established dial.
        if let Err(e) = state
            .db
            .record_nest_address(&peer_nest_id, &key.1, true)
            .await
        {
            tracing::warn!("record proven nest address: {e}");
        }
        let mut map = self.channels.lock().await;
        // A concurrent dialer may have inserted while we were dialing; §4.D says
        // two channels coexisting briefly is harmless, so keep whichever is
        // already pooled and let ours drop (its serve task ends with the socket).
        Ok(Arc::clone(map.entry(key).or_insert(conn)))
    }

    /// Drop the pooled channels for a peer (after a stale-channel disconnect) so
    /// the next originator re-dials.
    ///
    /// Evicts **every** address entry for the identity: the dead-link policy
    /// (§4.D) is about the peer being unreachable, and the caller only knows
    /// which `nest_id` went dead, not which of its addresses.
    async fn evict(&self, peer_nest_id: &[u8; 32]) {
        self.channels
            .lock()
            .await
            .retain(|(id, _), _| id != peer_nest_id);
    }

    /// Originate a federation request to the peer at `peer_url`, returning the
    /// channel's reply (`Ok(value)` from the peer's handler, or a handler-level
    /// `RpcError`).
    ///
    /// The federation channel is the **sole** Fauna↔Fauna carrier (Spec Y2 slice
    /// 5 retired the HTTP interim), so a peer that is unreachable
    /// ([`PoolError::Resolve`]), offers no channel ([`PoolError::Unsupported`]),
    /// or whose handshake/link fails ([`PoolError::Dial`]) surfaces an error —
    /// there is no HTTP fallback.
    ///
    /// The stale-channel dead-link policy (§4.D): when the channel drops with a
    /// request **in flight** (the op MAY have run on the peer), the entry is
    /// evicted, and a kind whose handler is naturally idempotent is re-dialed +
    /// re-sent once with the SAME `idempotency_key`. Whether a kind qualifies
    /// is **not a per-call-site opinion**: it is
    /// [`FederationRouter::retry_safe`](crate::federation_router::FederationRouter::retry_safe)
    /// — `!forbid_replay` from the one serving table, with each kind's
    /// rationale at its declaration site in `federation_handlers.rs`. (The
    /// serving idempotency cache is per-connection and does not survive the
    /// redial, so semantic idempotence is the only thing the re-send can rest
    /// on.) A forbid-replay kind (`keypackage.fetch`, `welcome.deliver`,
    /// `inbox.deliver`) is NOT re-sent — re-running would consume a second
    /// one-time resource or append + charge twice — and the disconnect is
    /// surfaced to the caller (which retries the user action).
    ///
    /// **A local reply-wait timeout is a distinct outcome from §4.D's
    /// disconnect** — the channel never dropped, a well-behaved peer just
    /// hasn't answered within `ORIGINATE_DEADLINE`. It surfaces as `Ok(Err(fauna.protocol.timeout))`, with no
    /// eviction and no re-send: the channel may still be perfectly live, and
    /// re-sending would re-grant the budget this attempt already spent
    /// (`docs/goal/architecture/transport.md` § Request lifecycle, "never
    /// re-granted at each stage").
    pub async fn originate(
        &self,
        state: &Arc<AppState>,
        peer_url: &str,
        kind: &str,
        idempotency_key: [u8; 16],
        payload: Value,
    ) -> Result<Result<Value, RpcError>, PoolError> {
        let retry_safe = state.federation_router.retry_safe(kind);
        let peer_nest_id = self.resolve_peer_nest_id(peer_url).await?;
        let mut redialed = false;
        loop {
            let conn = match self.get_or_dial(state, peer_url, peer_nest_id).await {
                Ok(c) => c,
                Err(DialError::ChannelUnsupported) => return Err(PoolError::Unsupported),
                // Peer offers the channel but the handshake failed — a hard error.
                Err(e) => return Err(PoolError::Dial(e.to_string())),
            };

            // One budget, pinned once per attempt, spent across *both* races
            // below — the enqueue and then the reply wait — mirroring
            // `request_encoded`'s "one budget covers the whole call" shape
            // (`libs/fauna-protocol/src/dispatcher.rs:563-600`). A fresh timer
            // for the reply wait would grant the call two deadlines, breaking
            // the "never re-granted at each stage" guarantee
            // (`docs/goal/architecture/transport.md:274-275`).
            let mut budget = pin!(tokio::time::sleep(ORIGINATE_DEADLINE));

            match conn
                .dispatcher
                .request_raw_bounded(
                    kind,
                    idempotency_key,
                    payload.clone(),
                    Some(ORIGINATE_DEADLINE),
                    budget.as_mut(),
                )
                .await
            {
                // The send never went out — either the channel was dead before
                // send, or the outbound queue stayed full for the whole
                // deadline (a peer withholding its receive window; `was_in_flight
                // = false` either way): evict +
                // re-dial once; if the re-dial also can't send, surface a
                // dead-link error.
                Err(_send_err) => {
                    self.evict(&peer_nest_id).await;
                    if !redialed {
                        redialed = true;
                        continue;
                    }
                    return Err(PoolError::Dial(
                        "dead link: send failed after re-dial".to_string(),
                    ));
                }
                Ok(call) => {
                    // Race the reply against what is left of this attempt's
                    // budget, not a fresh timer — the enqueue above may
                    // already have spent part of it. Without this, a peer
                    // that accepts the request and keeps the link alive
                    // (e.g. answering Pings) but never calls `send_reply`
                    // races nothing local at all.
                    let reply = pin!(call.await_reply());
                    let reply = match select(reply, budget).await {
                        Either::Left((reply, _)) => reply,
                        Either::Right(((), _)) => {
                            // A local timeout is not a channel drop: §4.D's
                            // evict + retry-safe re-send covers only a
                            // channel that *drops* with a request in flight
                            // (`docs/goal/architecture/transport.md:274-275`),
                            // and re-sending here would re-grant the budget
                            // this attempt just spent. Map onto the same
                            // `fauna.protocol.timeout` code the nest's own
                            // shared serving path already uses for a local
                            // deadline.
                            return Ok(Err(RpcError::new(
                                "fauna.protocol.timeout",
                                "error.protocol.timeout",
                            )));
                        }
                    };
                    // Dropped with the request in flight (`was_in_flight = true`).
                    if is_disconnect(&reply) {
                        self.evict(&peer_nest_id).await;
                        if retry_safe && !redialed {
                            redialed = true;
                            continue;
                        }
                    }
                    return Ok(reply);
                }
            }
        }
    }

    /// [`Self::originate`], refusing before the dial unless the nest answering at
    /// `peer_url` is the one the caller pinned.
    ///
    /// **Why the ordinary dial is not enough.** `originate` resolves the peer id
    /// from the URL's own `fauna.nest.info` and the handshake then verifies the
    /// peer against *that* — which proves the peer holds the key it advertises,
    /// not that it is the peer the caller intended. An attacker who takes over a
    /// destination's URL resolution **and holds a WebPKI-valid cert for its
    /// domain** satisfies every check in that chain (the discovery trust rule,
    /// [`discovery_dial_admits`], is what makes the cert a requirement rather
    /// than a courtesy; below it the takeover needs no CA at all). For a
    /// caller that has no stored
    /// expectation (most origination is client-directed, with the URL supplied
    /// per call) there is nothing better to do. For a caller that *does* hold a
    /// pin — a registered backup destination the owner named once — spending it
    /// converts a silent redirect into a refusal the admin can see.
    ///
    /// Costs nothing beyond `originate`: the resolution is cached, so the second
    /// lookup inside is a map hit.
    pub async fn originate_expecting(
        &self,
        state: &Arc<AppState>,
        peer_url: &str,
        expected_nest_id: &[u8],
        kind: &str,
        idempotency_key: [u8; 16],
        payload: Value,
    ) -> Result<Result<Value, RpcError>, PoolError> {
        let resolved = self.resolve_peer_nest_id(peer_url).await?;
        if expected_nest_id != resolved.as_slice() {
            return Err(PoolError::PeerMismatch {
                url: peer_url.to_string(),
                expected: hex::encode(expected_nest_id),
                resolved: hex::encode(resolved),
            });
        }
        self.originate(state, peer_url, kind, idempotency_key, payload)
            .await
    }
}

/// `true` if the reply is the L3 disconnected sentinel.
fn is_disconnect(result: &Result<Value, RpcError>) -> bool {
    matches!(result, Err(e) if e.code == "fauna.protocol.disconnected")
}

/// A fresh random 16-byte idempotency key for an originated federation call.
fn fresh_idempotency_key() -> [u8; 16] {
    let mut k = [0u8; 16];
    getrandom::fill(&mut k).expect("getrandom for federation idempotency key");
    k
}

// ── Typed originator wrappers (§6) ─────────────────────────────────────────────
//
// These reuse the `Fed*Request`/`Fed*Reply` structs the serving handlers define
// in `federation_handlers` (a peer's serving side and our originating side share
// the wire types). The federation channel is the sole carrier (slice 5), so each
// returns the decoded result or a [`PoolError`].

/// Originate `fauna.federation.keypackage.fetch` (destructive; forbid-replay).
/// `Ok(kp)` ⇒ the (possibly-absent) consumed key package.
pub async fn originate_keypackage_fetch(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    target_actor_hex: &str,
) -> Result<Option<Vec<u8>>, PoolError> {
    let req = FedKeypackageFetchRequest {
        target_actor_id: target_actor_hex.to_string(),
    };
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.keypackage.fetch",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?
    {
        Ok(v) => {
            let reply: FedKeypackageFetchReply = value_to(&v)
                .map_err(|()| PoolError::Dial("decode keypackage.fetch reply".to_string()))?;
            Ok(reply.key_package)
        }
        Err(e) => Err(PoolError::Dial(format!(
            "peer keypackage.fetch: {}",
            e.code
        ))),
    }
}

/// Originate `fauna.federation.channel.fetch` (read-only). The home
/// nest of `channel_id` returns its application-message log entries with
/// `seq > after` (capped at `limit`) for `requesting_actor` — the cross-nest
/// message pull for unpaired nests (`direct-messages.md` § Technical Flow —
/// Cross-Nest, step 3). The peer authorizes the pull by the requester's recorded
/// channel membership bound to this nest's `nest_id`.
///
/// `announce` is the requesting member's `(handle, domain)` as THIS nest — the
/// member's home, the authority for handles at its domain — joins it from its
/// own `users` row: the id→handle ruling's one mechanism (`federation.md`
/// § Cross-nest shared folders + channel append, the id→handle bullet). It
/// rides every drain so a rename lands on the next one; `None` when the
/// member has no usable handle, which announces nothing rather than an empty
/// name.
///
/// Returns the peer's entries as served — each carrying, beside its envelope,
/// the takedown marker and a community room's verdicts the home nest filled
/// for `requesting_actor` ([`FedChannelFetchMessage`]).
#[allow(clippy::too_many_arguments)]
pub async fn originate_channel_fetch(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    channel_id_hex: &str,
    after: i64,
    limit: i64,
    announce: Option<(String, String)>,
) -> Result<Vec<FedChannelFetchMessage>, PoolError> {
    let (requesting_handle, requesting_domain) = match announce {
        Some((h, d)) => (Some(h), Some(d)),
        None => (None, None),
    };
    let req = FedChannelFetchRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        channel_id: channel_id_hex.to_string(),
        after,
        limit,
        requesting_handle,
        requesting_domain,
    };
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.channel.fetch",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?
    {
        Ok(v) => {
            let reply: FedChannelFetchReply = value_to(&v)
                .map_err(|()| PoolError::Dial("decode channel.fetch reply".to_string()))?;
            Ok(reply.messages)
        }
        Err(e) => Err(PoolError::Dial(format!("peer channel.fetch: {}", e.code))),
    }
}

// ── Phase 2: cross-nest shared folders + channel append ─────────────────────
//
// These four preserve the peer's **typed** `RpcError` (nested
// `Result<Result<_, RpcError>, PoolError>`) instead of flattening it into
// `PoolError::Dial` like the older wrappers: the relaying nest must map a
// peer-side `fauna.protocol.unauthenticated` (an old, kind-unaware peer) to a
// client-visible "the home nest needs an update" — never a spurious auth
// failure — and the append's `fauna.conversations.channel.stale` /
// `permission_denied` refusals must ride back to the client untouched (S5;
// `federation.md` § Cross-nest shared folders + channel append).

/// Decode a peer reply `Value` into a typed reply, preserving a typed peer
/// error untouched; a reply that fails to decode surfaces as `malformed`.
fn decode_peer_reply<T: serde::de::DeserializeOwned>(
    reply: Result<Value, RpcError>,
) -> Result<T, RpcError> {
    match reply {
        Ok(v) => value_to::<T>(&v)
            .map_err(|()| RpcError::new("fauna.protocol.malformed", "error.protocol.malformed")),
        Err(e) => Err(e),
    }
}

/// Originate `fauna.federation.channel.append` (redelivery-safe: a
/// same-key re-send replays from the peer's idempotency cache; past that cache
/// MLS consumers quiet-skip duplicates — S3). `Ok(Ok(seq))` ⇒ appended.
pub async fn originate_channel_append(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    channel_id_hex: &str,
    envelope: Vec<u8>,
    expect_no_commit_since: Option<i64>,
    attachment_refs: Vec<String>,
) -> Result<Result<i64, RpcError>, PoolError> {
    let req = FedChannelAppendRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        channel_id: channel_id_hex.to_string(),
        envelope,
        expect_no_commit_since,
        attachment_refs,
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.channel.append",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(decode_peer_reply::<FedChannelAppendReply>(reply).map(|r| r.seq))
}

/// Originate `fauna.federation.channel.leave` (idempotent).
/// `Ok(Ok(removed))` ⇒ the peer deleted the row (`false` = already absent).
pub async fn originate_channel_leave(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    channel_id_hex: &str,
) -> Result<Result<bool, RpcError>, PoolError> {
    let req = FedChannelLeaveRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        channel_id: channel_id_hex.to_string(),
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.channel.leave",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(decode_peer_reply::<FedChannelLeaveReply>(reply).map(|r| r.removed))
}

/// Originate `fauna.federation.channel.actors` (idempotent pure read). `Ok(Ok(actors))` ⇒ the channel home's authoritative roster
/// union as hex actor ids. The peer's typed error is preserved (the relay
/// handler maps an old peer's allowlist-first `unauthenticated` to the typed
/// `peer_nest_outdated` — S5, `federation.md` § old-peer error shapes).
pub async fn originate_channel_actors(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    channel_id_hex: &str,
) -> Result<Result<Vec<String>, RpcError>, PoolError> {
    let req = FedChannelActorsRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        channel_id: channel_id_hex.to_string(),
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.channel.actors",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(decode_peer_reply::<FedChannelActorsReply>(reply).map(|r| r.actors))
}

/// Originate `fauna.federation.conversation.generations.fetch` (idempotent
/// pure read). `Ok(Ok(generations))` ⇒ the room home's wraps **for this
/// member**, in the same wire shape the same-nest `room.generations` serves,
/// so the relay forwards them unchanged.
///
/// This nest keeps none of them: a relaying nest carries ciphertext and holds
/// no wrap (`../behavior/conversation-rooms.md` § The home nest), and the
/// wraps it passes through are sealed to the member's own reception key
/// anyway. The request names no roster entry, so the room home resolves the
/// recipient from `requesting_actor_hex` and this nest cannot substitute
/// itself.
pub async fn originate_room_generations(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    room_id_hex: &str,
) -> Result<Result<Vec<fauna_protocol::conversations::RoomGenerationWire>, RpcError>, PoolError> {
    let req = FedRoomGenerationsRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        room_id: room_id_hex.to_string(),
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.conversation.generations.fetch",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(decode_peer_reply::<FedRoomGenerationsReply>(reply).map(|r| r.generations))
}

/// Originate `fauna.federation.conversation.roster.fetch` (idempotent pure
/// read). `Ok(Ok(reply))` ⇒ the room home's floor roster in the same wire
/// shape the same-nest `room.list_roster` serves, so the relay forwards it
/// unchanged.
///
/// This nest stores none of it: the roster is the room's membership record,
/// read under the room home's own gate, and a relay that cached it would be
/// answering a membership question it holds no authority over. The request
/// names only the requesting actor, whom the room home binds to this nest's
/// verified identity, so a nest can ask only on behalf of its own members.
pub async fn originate_room_roster(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    room_id_hex: &str,
    at_policy_version: Option<u64>,
) -> Result<Result<fauna_protocol::conversations::RoomListRosterReply, RpcError>, PoolError> {
    let req = FedRoomRosterRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        room_id: room_id_hex.to_string(),
        at_policy_version,
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.conversation.roster.fetch",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(decode_peer_reply::<FedRoomRosterReply>(reply).map(|r| r.roster))
}

/// Originate `fauna.federation.conversation.room_labels.fetch` (idempotent
/// pure read). `Ok(Ok(reply))` ⇒ the room home's verdicts **for this member**,
/// in the same wire shape the same-nest `posts.room_labels` serves, so the
/// relay forwards them unchanged.
///
/// This nest stores none of them: a verdict is the room's, derived from
/// plaintext this nest never holds, and a relay that cached one would be
/// answering a floor question it has no authority over. The request names the
/// room as well as the posts, because the room home's first act is the
/// structural foreign-member gate on that room's channel id — and naming it
/// also keeps the answer inside the one room this nest is bound in.
pub async fn originate_room_labels(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    room_id_hex: &str,
    post_ids_hex: &[String],
) -> Result<Result<fauna_protocol::posts::PostRoomLabelsReply, RpcError>, PoolError> {
    let req = FedRoomLabelsRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        room_id: room_id_hex.to_string(),
        post_ids: post_ids_hex.to_vec(),
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.conversation.room_labels.fetch",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(decode_peer_reply::<FedRoomLabelsReply>(reply).map(|r| r.labels))
}

/// Originate `fauna.federation.conversation.roster.report` (idempotent
/// wholesale replace). `Ok(Ok(ack))` ⇒ the room home stored the report, in the
/// same wire shape the same-nest `room.roster_report` acks, so the relay
/// forwards it unchanged.
///
/// This nest stores none of it: the roster is the room's membership record,
/// admitted under the room home's own gates, and a relay that mirrored it
/// would be answering a membership question it holds no authority over. The
/// request names the reporting actor, whom the room home binds to this nest's
/// verified identity, so a nest can report only on behalf of its own members.
pub async fn originate_room_roster_report(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    room_id_hex: &str,
    members: Vec<fauna_protocol::conversations::RoomRosterEntryWire>,
    policy_version: Option<u64>,
    commit_seq: Option<i64>,
) -> Result<Result<fauna_protocol::conversations::RoomRosterReportReply, RpcError>, PoolError> {
    let req = FedRoomRosterReportRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        room_id: room_id_hex.to_string(),
        members,
        policy_version,
        commit_seq,
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.conversation.roster.report",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(decode_peer_reply::<FedRoomRosterReportReply>(reply).map(|r| r.ack))
}

/// Originate `fauna.federation.conversation.room.leave` (idempotent
/// severance). `Ok(Ok(ack))` ⇒ the room home took the member off its floor and
/// purged the binding — or found both already gone, which is the same
/// converged state and the same success. The ack is the wire shape the
/// same-nest `room.leave` returns, so the relay forwards it unchanged.
///
/// This nest stores nothing and decides nothing: the seat is the room home's
/// record, and the request names the departing actor, whom the home binds to
/// this nest's verified identity — so a nest can retire only its own members'
/// seats, never a member it does not carry.
pub async fn originate_room_leave(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    room_id_hex: &str,
) -> Result<Result<fauna_protocol::conversations::RoomLeaveReply, RpcError>, PoolError> {
    let req = FedRoomLeaveRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        room_id: room_id_hex.to_string(),
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.conversation.room.leave",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(decode_peer_reply::<FedRoomLeaveReply>(reply).map(|r| r.ack))
}

/// Originate `fauna.federation.conversation.room.invite` (forbid-replay:
/// appends an inbox row + charges quota on the invitee's nest per call). The
/// room home's delivery of a community-room invitation to an invitee homed on
/// `peer_url` (`conversation-rooms.md` § Join rules and invites → *A
/// cross-nest invitation*). `Ok(Ok(inbox_id))` ⇒ delivered; `Ok(Err(e))` is
/// the invitee's nest's own refusal (its member's reach policy, an
/// unregistered recipient, a record that does not verify), which the invite
/// door hands the inviter as the answer.
pub async fn originate_room_invite_deliver(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    req: FedRoomInviteRequest,
) -> Result<Result<i64, RpcError>, PoolError> {
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.conversation.room.invite",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(decode_peer_reply::<FedRoomInviteReply>(reply).map(|r| r.inbox_id))
}

/// Originate `fauna.federation.conversation.room.accept` (idempotent on the
/// home — `room_accept_relayed` answers a member its recorded invitation
/// already seated from this nest with its role, so the §4.D re-send reports
/// the seating that landed). `Ok(Ok(ack))` ⇒ the room home's
/// `RoomAcceptInviteReply`, forwarded to the member unchanged.
///
/// This nest stores none of it: it holds no room record and no invitation
/// row for a room homed elsewhere, and a seat it mirrored would be a
/// membership question it has no authority over. The request names only the
/// requesting actor, whom the home binds to this nest's verified identity
/// through the invitation it recorded, so a nest can seat only its own
/// members.
pub async fn originate_room_accept(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    room_id_hex: &str,
    reception_pubkey: Vec<u8>,
) -> Result<Result<fauna_protocol::conversations::RoomAcceptInviteReply, RpcError>, PoolError> {
    let req = FedRoomAcceptRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        room_id: room_id_hex.to_string(),
        reception_pubkey,
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.conversation.room.accept",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(decode_peer_reply::<FedRoomAcceptReply>(reply).map(|r| r.ack))
}

/// Originate `fauna.federation.conversation.room.invite_issue` (forbid-replay:
/// the room home records a row and delivers a knock per call). A foreign
/// member's invitation, issued through this nest — its own home — to the
/// room's home at `peer_url` (`conversation-rooms.md` § Join rules and
/// invites → *A cross-nest invitation*, the foreign-inviter leg).
/// `Ok(Ok(ack))` ⇒ the home's `RoomInviteReply`, forwarded to the member
/// unchanged; `Ok(Err(e))` is the home's own refusal (the join rule, an
/// invitee already seated, the invitee's nest's reach refusal relayed on),
/// which the relay hands the inviter as the answer.
///
/// This nest stores none of it: it holds no room record and no invitation
/// row for a room homed elsewhere. The request names the requesting actor,
/// whom the home binds to this nest's verified identity through the
/// foreign-member binding, and carries `invitee_node` as the inviter spelled
/// it — empty for an invitee homed HERE, which the home resolves from this
/// nest's verified identity; the claim-refreshed domain rides along as the
/// Welcome relay's own `nest_url` does, honoured there only against that
/// identity.
pub async fn originate_room_invite_issue(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    signed_invite: Vec<u8>,
    invitee_node: String,
) -> Result<Result<fauna_protocol::conversations::RoomInviteReply, RpcError>, PoolError> {
    let req = FedRoomInviteIssueRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        signed_invite,
        invitee_node,
        origin_nest_url: state.handle_domain_if_set().map(|d| format!("https://{d}")),
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.conversation.room.invite_issue",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(decode_peer_reply::<FedRoomInviteIssueReply>(reply).map(|r| r.ack))
}

/// Originate `fauna.federation.folder.changes.fetch` (idempotent read). `Ok(Ok(reply))` ⇒ the home nest's wire rows
/// (`seq > after`, oldest first, frame-budgeted — same `SyncChange` shape as a
/// same-nest `changes.list`), its `signer_certs` side table, and the home
/// nest's stamps of the requester's current grant and the folder's residency,
/// threaded on to the client so it can refresh its stored copy
/// (`federation_handlers::caller_access_stamp`, `residency_stamp`).
pub async fn originate_folder_changes_fetch(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    channel_id_hex: &str,
    after: i64,
    limit: i64,
) -> Result<Result<FedFolderChangesFetchReply, RpcError>, PoolError> {
    let req = FedFolderChangesFetchRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        channel_id: channel_id_hex.to_string(),
        after,
        limit,
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.folder.changes.fetch",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(decode_peer_reply::<FedFolderChangesFetchReply>(reply))
}

/// Originate `fauna.federation.folder.public.fetch` (idempotent read) — the
/// follower's own nest relaying a public-folder read to the folder's home nest
/// (`federation.md` § The public folder read plane).
///
/// ⚠ Note what is **not** in this signature: no requesting actor. There is no
/// membership to check on the far side, so the follower's identity never leaves
/// this nest — the home nest sees only the requesting nest and source IP.
pub async fn originate_folder_public_fetch(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    owner_actor_hex: Option<&str>,
    folder_name: Option<&str>,
    folder_id: Option<i64>,
    since: i64,
    limit: i64,
) -> Result<Result<FedFolderPublicFetchReply, RpcError>, PoolError> {
    let req = FedFolderPublicFetchRequest {
        owner_actor_id: owner_actor_hex.map(str::to_string),
        folder_name: folder_name.map(str::to_string),
        folder_id,
        since,
        limit,
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.folder.public.fetch",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(decode_peer_reply::<FedFolderPublicFetchReply>(reply))
}

/// Originate `fauna.federation.folder.content_key.fetch` (idempotent read).
/// `Ok(Ok(reply))` ⇒ the sealed envelope (opaque to both nests) plus the home
/// nest's stamps the member's client refreshes from: the requester's current
/// grant (`caller_access_stamp`) and the home nest's deployment identity (the
/// byte-plane SPKI-pin trust root). This is the federated read a foreign
/// member's client actually runs in production, so it is what makes both
/// refreshes live.
pub async fn originate_folder_content_key_fetch(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    channel_id_hex: &str,
) -> Result<Result<FedFolderContentKeyFetchReply, RpcError>, PoolError> {
    let req = FedFolderContentKeyFetchRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        channel_id: channel_id_hex.to_string(),
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.folder.content_key.fetch",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(decode_peer_reply::<FedFolderContentKeyFetchReply>(reply))
}

/// Originate `fauna.federation.folder.actors.fetch` (idempotent pure read) —
/// the cross-nest writer roster read, the
/// [`originate_folder_content_key_fetch`] twin. `Ok(Ok(reply))` ⇒ the home
/// nest's actor-roster projection (ids-only) plus its `caller_access` stamp.
pub async fn originate_folder_actors_fetch(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    channel_id_hex: &str,
) -> Result<Result<FedFolderActorsFetchReply, RpcError>, PoolError> {
    let req = FedFolderActorsFetchRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        channel_id: channel_id_hex.to_string(),
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.folder.actors.fetch",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(decode_peer_reply::<FedFolderActorsFetchReply>(reply))
}

/// Originate `fauna.federation.folder.changes.record` (a WRITE, but
/// **content-idempotent** on the home nest — a re-relayed record with identical
/// content returns the original seq and charges nothing). `Ok(Ok(seq))` ⇒ the assigned (or existing, on replay) sequence.
#[allow(clippy::too_many_arguments)]
pub async fn originate_folder_changes_record(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    channel_id_hex: &str,
    device_id_hex: &str,
    path: &str,
    manifest_hash: Option<String>,
    size_bytes: i64,
    change_type: &str,
    content_key_version: Option<u64>,
    thumbnail_hash: Option<String>,
    path_sealed: Option<serde_bytes::ByteBuf>,
    derived_through: Option<i64>,
    is_resolution: Option<bool>,
    signature: Option<serde_bytes::ByteBuf>,
    signer_key: Option<serde_bytes::ByteBuf>,
    signer_cert: Option<fauna_core::encoding::EmbedAsBytes>,
) -> Result<Result<i64, RpcError>, PoolError> {
    let req = FedFolderChangesRecordRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        channel_id: channel_id_hex.to_string(),
        device_id: device_id_hex.to_string(),
        path: path.to_string(),
        manifest_hash,
        size_bytes,
        change_type: change_type.to_string(),
        content_key_version,
        thumbnail_hash,
        path_sealed,
        derived_through,
        is_resolution,
        signature,
        signer_key,
        signer_cert,
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.folder.changes.record",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(decode_peer_reply::<FedFolderChangesRecordReply>(reply).map(|r| r.seq))
}

/// Originate `fauna.federation.folder.write_token.mint` (a mint — a duplicate
/// yields a second valid short-TTL token, both harmless).
/// `Ok(Ok((token, expires_at)))` ⇒ a write-only bulk-byte token for direct
/// chunk/manifest POSTs to the home nest.
pub async fn originate_folder_write_token_mint(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    channel_id_hex: &str,
) -> Result<Result<(String, u64), RpcError>, PoolError> {
    let req = FedFolderWriteTokenMintRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        channel_id: channel_id_hex.to_string(),
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.folder.write_token.mint",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(decode_peer_reply::<FedFolderWriteTokenMintReply>(reply).map(|r| (r.token, r.expires_at)))
}

/// Originate `fauna.federation.folder.read_token.mint` — the read-scoped twin
/// of [`originate_folder_write_token_mint`] (a mint; a duplicate yields a
/// second valid short-TTL token, both harmless). `Ok(Ok((token, expires_at)))`
/// ⇒ a read-only bulk-byte token for the home nest's relay read arm.
pub async fn originate_folder_read_token_mint(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    channel_id_hex: &str,
) -> Result<Result<(String, u64), RpcError>, PoolError> {
    let req = FedFolderReadTokenMintRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        channel_id: channel_id_hex.to_string(),
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.folder.read_token.mint",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(decode_peer_reply::<FedFolderReadTokenMintReply>(reply).map(|r| (r.token, r.expires_at)))
}

/// Originate `fauna.federation.folder.serve.announce` — a member's seat of a
/// folder homed at `peer_url` serves (`serving`) or no longer serves it
/// (`file-sync.md` § Relay serving → *A member on another nest*, step (2)).
/// `Ok(Ok((lease_secs, home_nest_id)))` ⇒ the home nest admitted it, for that
/// many seconds, and `home_nest_id` is the `nest_id` the channel verified —
/// the one nest whose ask this seat is pushed for.
pub async fn originate_folder_serve_announce(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    channel_id_hex: &str,
    device_id_hex: &str,
    serving: bool,
) -> Result<Result<(u64, [u8; 32]), RpcError>, PoolError> {
    let req = crate::federation_handlers::FedFolderServeAnnounceRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        channel_id: channel_id_hex.to_string(),
        device_id: device_id_hex.to_string(),
        serving,
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            crate::federation_handlers::KIND_FED_FOLDER_SERVE_ANNOUNCE,
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    // `originate` dialed the channel pinned to exactly this resolution, so it
    // is the verified identity of the nest that just answered.
    let home_nest_id = pool.resolve_peer_nest_id(peer_url).await?;
    Ok(
        decode_peer_reply::<crate::federation_handlers::FedFolderServeAnnounceReply>(reply)
            .map(|r| (r.lease_secs, home_nest_id)),
    )
}

/// Originate `fauna.federation.folder.chunk.wanted` to the member's nest that
/// leased `seat` — and only to the `nest_id` that leased it
/// ([`FederationChannelPool::originate_expecting`]). Resolves to whether that
/// nest pushed the ask; every other outcome — a refusal, a nest that predates
/// the kind, a transport error — is `false`, which passes the seat over at once
/// (`file-sync.md` § Relay serving → *A member on another nest*, step (3)).
pub async fn originate_folder_chunk_wanted(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    seat: &crate::chunk_relay::ForeignSeat,
    request_id: u64,
    store_key_hex: String,
) -> bool {
    let req = crate::federation_handlers::FedFolderChunkWantedRequest {
        requesting_actor_id: hex::encode(seat.member),
        channel_id: hex::encode(seat.channel_id),
        device_id: hex::encode(seat.device),
        request_id,
        store_key: store_key_hex,
    };
    match pool
        .originate_expecting(
            state,
            &seat.nest_url,
            &seat.origin_nest_id,
            crate::federation_handlers::KIND_FED_FOLDER_CHUNK_WANTED,
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await
    {
        Ok(reply) => {
            decode_peer_reply::<crate::federation_handlers::FedFolderChunkWantedReply>(reply)
                .is_ok_and(|r| r.pushed)
        }
        Err(e) => {
            tracing::debug!("chunk relay: a foreign seat's ask did not reach its nest: {e}");
            false
        }
    }
}

/// Originate `fauna.federation.conversation.write_token.mint` — the
/// cross-nest attachment upload's mint, relayed for a foreign conversation
/// member by their own nest (`fauna.conversations.blob.write_token.get`).
/// `Ok(Ok((token, expires_at)))` on success.
pub async fn originate_conversation_write_token_mint(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    requesting_actor_hex: &str,
    channel_id_hex: &str,
) -> Result<Result<(String, u64), RpcError>, PoolError> {
    let req = FedConversationWriteTokenMintRequest {
        requesting_actor_id: requesting_actor_hex.to_string(),
        channel_id: channel_id_hex.to_string(),
    };
    let reply = pool
        .originate(
            state,
            peer_url,
            "fauna.federation.conversation.write_token.mint",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?;
    Ok(
        decode_peer_reply::<FedConversationWriteTokenMintReply>(reply)
            .map(|r| (r.token, r.expires_at)),
    )
}

/// Originate `fauna.federation.welcome.deliver` (forbid-replay: appends an
/// inbox row + charges quota per call — rationale at the declaration site in
/// `federation_handlers.rs`).
/// `Ok(inbox_id)` ⇒ delivered (the peer's own inbox row id).
///
/// Takes the built [`FedWelcomeDeliverRequest`] rather than its fields: the
/// envelope has grown three consecutive `Option<String>`s (`origin_nest_url`,
/// `set_name`, `access`) that a positional signature would let a caller
/// transpose with no compile error — relaying a set's *name* as its access
/// *grant*. Named struct fields make that unrepresentable, and the next
/// additive field touches one struct instead of every call site.
pub async fn originate_welcome_deliver(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    req: FedWelcomeDeliverRequest,
) -> Result<i64, PoolError> {
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.welcome.deliver",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?
    {
        Ok(v) => {
            let reply: FedWelcomeDeliverReply = value_to(&v)
                .map_err(|()| PoolError::Dial("decode welcome.deliver reply".to_string()))?;
            Ok(reply.inbox_id)
        }
        Err(e) => Err(PoolError::Dial(format!("peer welcome.deliver: {}", e.code))),
    }
}

/// Originate `fauna.federation.inbox.deliver` (forbid-replay: appends an inbox
/// row + charges quota per call — rationale at the declaration site in
/// `federation_handlers.rs`). Hands the
/// recipient's home nest the verbatim signed `(ContactRequest, Post)` tuple; the
/// peer runs `InboxMode` routing and returns `Ok(Some(inbox_id))` on delivery,
/// `Ok(None)` for a stored knock. A policy rejection (closed/contacts-only/
/// blocked) surfaces as a `PoolError::Dial` carrying the peer error code.
pub async fn originate_inbox_deliver(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    recipient_actor_hex: &str,
    payload_bytes: &[u8],
) -> Result<Option<i64>, PoolError> {
    let req = FedInboxDeliverRequest {
        recipient_actor_id: recipient_actor_hex.to_string(),
        payload_bytes: payload_bytes.to_vec(),
    };
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.inbox.deliver",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?
    {
        Ok(v) => {
            let reply: FedInboxDeliverReply = value_to(&v)
                .map_err(|()| PoolError::Dial("decode inbox.deliver reply".to_string()))?;
            Ok(reply.inbox_id)
        }
        Err(e) => Err(PoolError::Dial(format!("peer inbox.deliver: {}", e.code))),
    }
}

/// Originate `fauna.federation.sync.pull` (idempotent read).
/// `Ok(reply)` ⇒ the pulled namespace entries (raw bytes, CBOR-native) and the
/// new high-water `up_to`.
pub async fn originate_sync_pull(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    actor_id_hex: &str,
    namespace: &[u8],
    since: i64,
) -> Result<FedSyncPullReply, PoolError> {
    let req = FedSyncPullRequest {
        actor_id: actor_id_hex.to_string(),
        namespace: hex::encode(namespace),
        since,
    };
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.sync.pull",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?
    {
        Ok(v) => {
            let reply: FedSyncPullReply =
                value_to(&v).map_err(|()| PoolError::Dial("decode sync.pull reply".to_string()))?;
            Ok(reply)
        }
        Err(e) => Err(PoolError::Dial(format!("peer sync.pull: {}", e.code))),
    }
}

/// Originate `fauna.federation.sync.push` (idempotent; the peer
/// dedups by `entry_id`). `Ok(up_to)` ⇒ pushed, with the peer's new high-water
/// sequence.
pub async fn originate_sync_push(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    actor_id_hex: &str,
    namespace: &[u8],
    entries: &[crate::db::NamespaceEntry],
) -> Result<i64, PoolError> {
    let req = FedSyncPushRequest {
        actor_id: actor_id_hex.to_string(),
        namespace: hex::encode(namespace),
        entries: entries
            .iter()
            .map(|e| FedSyncPushEntry {
                entry_id: e.entry_id.clone(),
                ciphertext: e.ciphertext.clone(),
                actor_sig: e.actor_sig.clone(),
            })
            .collect(),
    };
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.sync.push",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?
    {
        Ok(v) => {
            let reply: FedSyncPushReply =
                value_to(&v).map_err(|()| PoolError::Dial("decode sync.push reply".to_string()))?;
            Ok(reply.up_to)
        }
        Err(e) => Err(PoolError::Dial(format!("peer sync.push: {}", e.code))),
    }
}

/// Originate `fauna.federation.sync.mail_pull` (idempotent read).
/// The private nest's relay pull: returns up to 500 sealed `__mail` records
/// after `since_seq`. Channel-only (slice 5 made the channel the sole carrier —
/// there is no HTTP fallback); a `PoolError` is a transient transport failure
/// the caller retries next cycle.
pub async fn originate_mail_pull(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    actor_id_hex: &str,
    since_seq: i64,
) -> Result<FedMailPullReply, PoolError> {
    let req = FedMailPullRequest {
        actor_id: actor_id_hex.to_string(),
        since_seq,
    };
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.sync.mail_pull",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?
    {
        Ok(v) => {
            let reply: FedMailPullReply =
                value_to(&v).map_err(|()| PoolError::Dial("decode mail_pull reply".to_string()))?;
            Ok(reply)
        }
        Err(e) => Err(PoolError::Dial(format!("peer mail_pull: {}", e.code))),
    }
}

/// Originate `fauna.federation.sync.mail_ack` (idempotent; the
/// peer's tombstone is idempotent on re-ack). Returns the count purged.
pub async fn originate_mail_ack(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    actor_id_hex: &str,
    up_to_seq: i64,
) -> Result<u64, PoolError> {
    let req = FedMailAckRequest {
        actor_id: actor_id_hex.to_string(),
        up_to_seq,
    };
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.sync.mail_ack",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?
    {
        Ok(v) => {
            let reply: FedMailAckReply =
                value_to(&v).map_err(|()| PoolError::Dial("decode mail_ack reply".to_string()))?;
            Ok(reply.purged)
        }
        Err(e) => Err(PoolError::Dial(format!("peer mail_ack: {}", e.code))),
    }
}

/// Originate `fauna.federation.sync.nostr_push` (spec P2.4). The head pushes its
/// `origin='ingest'` rows public-ward; the reply is the ack (the head advances
/// its push cursor on a successful reply). Idempotent: the serving
/// side stores under a content-addressed event `id` and dedups, so a redelivered
/// batch stores nothing new.
#[cfg(feature = "nostr")]
pub async fn originate_nostr_push(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    req: crate::federation_handlers::FedNostrPushRequest,
) -> Result<crate::federation_handlers::FedNostrPushReply, PoolError> {
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.sync.nostr_push",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?
    {
        Ok(v) => {
            let reply: crate::federation_handlers::FedNostrPushReply = value_to(&v)
                .map_err(|()| PoolError::Dial("decode nostr_push reply".to_string()))?;
            Ok(reply)
        }
        Err(e) => Err(PoolError::Dial(format!("peer nostr_push: {}", e.code))),
    }
}

/// Originate `fauna.federation.sync.nostr_pull` (spec P2.4, idempotent read). The head fetches externally-deposited `origin='ingest'` rows
/// head-ward, cursor-parameterized by the compound `(stored_at, id)` it persists.
#[cfg(feature = "nostr")]
pub async fn originate_nostr_pull(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    req: crate::federation_handlers::FedNostrPullRequest,
) -> Result<crate::federation_handlers::FedNostrPullReply, PoolError> {
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.sync.nostr_pull",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?
    {
        Ok(v) => {
            let reply: crate::federation_handlers::FedNostrPullReply = value_to(&v)
                .map_err(|()| PoolError::Dial("decode nostr_pull reply".to_string()))?;
            Ok(reply)
        }
        Err(e) => Err(PoolError::Dial(format!("peer nostr_pull: {}", e.code))),
    }
}

/// Originate `fauna.federation.post.forward` (idempotent: the
/// serving handler stores under a content-addressed `post_id` and tolerates the
/// `UNIQUE` collision, so a redelivered post is a no-op).
///
/// The private nest's post-forwarding leg (`private-mode.md` § Post Forwarding).
/// Only the post's **own** author sign-over-CID envelope rides; the retired HTTP
/// twin's extra nest-signature over the forward body is dropped, because the
/// channel already authenticates this nest to the peer at handshake.
pub async fn originate_post_forward(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    post_bytes: Vec<u8>,
    post_envelope: Vec<u8>,
    signer_auth: Option<Vec<u8>>,
) -> Result<(), PoolError> {
    let req = FedPostForwardRequest {
        post_bytes,
        post_envelope,
        signer_auth,
    };
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.post.forward",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?
    {
        Ok(_) => Ok(()),
        Err(e) => Err(PoolError::Dial(format!("peer post.forward: {}", e.code))),
    }
}

/// Originate `fauna.federation.post.delete` — the delete twin of
/// [`originate_post_forward`]. Relays an author-signed post deletion to the
/// paired public nest so a forwarded copy there does not outlive the original
/// (`feed.md` § Post deletion → Propagation). Idempotent: the
/// serving handler's `delete_post_core` returns `AlreadyGone` on a re-delivered
/// tombstone (a crash between peer-accept and `outbox_mark_sent`), so a re-drain
/// is a no-op, not an error. Only the tombstone's **own** author sign-over-CID
/// envelope rides; the channel authenticates this nest to the peer at handshake.
pub async fn originate_post_delete(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    tombstone_body: Vec<u8>,
) -> Result<(), PoolError> {
    let req = FedPostDeleteRequest { tombstone_body };
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.post.delete",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?
    {
        Ok(_) => Ok(()),
        Err(e) => Err(PoolError::Dial(format!("peer post.delete: {}", e.code))),
    }
}

/// Originate `fauna.federation.reports.exchange` (idempotent: the
/// peer's import is a latest-epoch-wins upsert per `(hash, factor, peer)`).
/// Pushes this nest's local ≥k report aggregates (`report-sharing.md`
/// § Federation exchange) — the caller supplies exactly the
/// `export_report_aggregates()` view, so nothing below k ever rides.
pub async fn originate_reports_exchange(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    req: &FedReportsExchangeRequest,
) -> Result<usize, PoolError> {
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.reports.exchange",
            fresh_idempotency_key(),
            to_value(req),
        )
        .await?
    {
        Ok(v) => {
            let reply: FedReportsExchangeReply = value_to(&v)
                .map_err(|()| PoolError::Dial("decode reports.exchange reply".to_string()))?;
            Ok(reply.imported)
        }
        Err(e) => Err(PoolError::Dial(format!(
            "peer reports.exchange: {}",
            e.code
        ))),
    }
}

/// Originate one leg of the abuse-report triad
/// (`fauna.federation.abuse_report.{deliver,withdraw,outcome}` — each
/// idempotent on the receiver, so the §4.D re-send is safe). `payload` is the
/// request as the durable queue holds it; `expected_nest_id` pins the dial to
/// the peer the report already went to (a withdrawal, an outcome). The reply
/// is the empty ack or the peer's refusal, which the caller classifies.
pub async fn originate_abuse_report_call(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    expected_nest_id: Option<&[u8]>,
    kind: &str,
    payload: Value,
) -> Result<Result<Value, RpcError>, PoolError> {
    match expected_nest_id {
        Some(expected) => {
            pool.originate_expecting(
                state,
                peer_url,
                expected,
                kind,
                fresh_idempotency_key(),
                payload,
            )
            .await
        }
        None => {
            pool.originate(state, peer_url, kind, fresh_idempotency_key(), payload)
                .await
        }
    }
}

/// Originate `fauna.federation.reports.export` (idempotent read).
/// Pulls the peer's local ≥k report aggregates; the caller imports them through
/// the same k-gate + flat-bucket path the serving exchange handler uses.
pub async fn originate_reports_export(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
) -> Result<FedReportsExportReply, PoolError> {
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.reports.export",
            fresh_idempotency_key(),
            Value::Null, // the serving handler ignores the payload
        )
        .await?
    {
        Ok(v) => {
            let reply: FedReportsExportReply = value_to(&v)
                .map_err(|()| PoolError::Dial("decode reports.export reply".to_string()))?;
            Ok(reply)
        }
        Err(e) => Err(PoolError::Dial(format!("peer reports.export: {}", e.code))),
    }
}

/// Originate `fauna.federation.trends.exchange` (idempotent: the
/// peer's import is a latest-epoch-wins upsert per `(content_id, peer)`). Pushes
/// this nest's local k-gate-passed trend head (`trending.md` § Federation
/// exchange) — the caller supplies exactly the `export_trend_entries()` view, so
/// nothing below k ever rides. `trends.exchange` is an **additive** kind (Slice
/// 3); any error here (an unsupported-kind answer or a transport fault) is
/// swallowed by the caller (the reports legs still complete).
pub async fn originate_trends_exchange(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    req: &FedTrendsExchangeRequest,
) -> Result<usize, PoolError> {
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.trends.exchange",
            fresh_idempotency_key(),
            to_value(req),
        )
        .await?
    {
        Ok(v) => {
            let reply: FedTrendsExchangeReply = value_to(&v)
                .map_err(|()| PoolError::Dial("decode trends.exchange reply".to_string()))?;
            Ok(reply.imported)
        }
        Err(e) => Err(PoolError::Dial(format!("peer trends.exchange: {}", e.code))),
    }
}

/// Originate `fauna.federation.trends.export` (idempotent read).
/// Pulls the peer's local k-gate-passed trend head; the caller imports the
/// presence bits through the same k-gate the serving exchange handler uses, then
/// triggers the bounded `post.get` fetch of the unseen ids. Additive kind — any
/// error here (unsupported kind, transport fault) is swallowed by the caller.
pub async fn originate_trends_export(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
) -> Result<FedTrendsExportReply, PoolError> {
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.trends.export",
            fresh_idempotency_key(),
            Value::Null, // the serving handler ignores the payload
        )
        .await?
    {
        Ok(v) => {
            let reply: FedTrendsExportReply = value_to(&v)
                .map_err(|()| PoolError::Dial("decode trends.export reply".to_string()))?;
            Ok(reply)
        }
        Err(e) => Err(PoolError::Dial(format!("peer trends.export: {}", e.code))),
    }
}

/// Originate `fauna.federation.post.get` (idempotent read). Fetches
/// a single post's raw stored bytes from `peer_url` by content-id — the
/// import-triggered fetch (`trending.md` § Import-triggered fetch) that surfaces
/// a peer-only trending post so its ramp can land. The caller verifies the
/// returned bytes (signature + content-address binding to `post_id`) before
/// ingesting; a missing/withheld post surfaces as an error / empty body the
/// caller drops.
pub async fn originate_post_get(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    post_id: String,
) -> Result<FedPostGetReply, PoolError> {
    let req = FedPostGetRequest { post_id };
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.post.get",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?
    {
        Ok(v) => {
            let reply: FedPostGetReply =
                value_to(&v).map_err(|()| PoolError::Dial("decode post.get reply".to_string()))?;
            Ok(reply)
        }
        Err(e) => Err(PoolError::Dial(format!("peer post.get: {}", e.code))),
    }
}

/// Originate `fauna.federation.feed.query` (idempotent read).
/// `Ok(resp)` ⇒ the peer's scored candidates in the **shared**
/// [`RemoteQueryResponse`] the serving handler emits, carrying the channel's
/// mutual nest-key auth + per-nest throttle. The caller
/// (`peer_query::query_peer_channel_first`) converts it to `ScoredCandidate`.
pub async fn originate_feed_query(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    req: &RemoteQueryRequest,
) -> Result<RemoteQueryResponse, PoolError> {
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.feed.query",
            fresh_idempotency_key(),
            to_value(req),
        )
        .await?
    {
        Ok(v) => {
            let reply: RemoteQueryResponse = value_to(&v)
                .map_err(|()| PoolError::Dial("decode feed.query reply".to_string()))?;
            Ok(reply)
        }
        Err(e) => Err(PoolError::Dial(format!("peer feed.query: {}", e.code))),
    }
}

// ── Nest-writer backup plane originators (nest-side segment backup, slice 3) ────
//
// The source nest's in-process backup coordinator drives these to write an
// owner's segment-backup custody at a destination it holds a user-minted
// nest-writer grant on. The channel authenticates the source *as itself* (the
// `fauna.federation.hello` handshake signs with `nest_identity`); the destination
// gates on the grant row it wrote at the owner's direction (`require_backup_writer`).
// Both are idempotent (mint dedups by idempotency key; record is exactly-once by
// content), so both stay replay-permitted in the serving table. Owner: `federation.md` § Nest-writer backup
// plane; `message-segment-store.md` § Cross-location backup protocol.

/// Originate `fauna.federation.backup.write_token.mint` (idempotent).
/// `Ok(reply)` ⇒ a short-TTL write-only bulk-byte token the source nest presents
/// to the destination's by-hash chunk routes; bulk bytes never ride the channel.
/// `expected_nest_id` is the destination row's stored pin
/// (`db/backup_destinations.rs` — "the 32-byte nest id it must pin as the
/// handshake's expected peer"). Required, not optional: every caller of this
/// function reaches it from a registered destination, so there is no legitimate
/// unpinned backup write, and making it a parameter is what stops a future
/// caller from quietly omitting it.
pub async fn originate_backup_write_token_mint(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    expected_nest_id: &[u8],
    owner_actor_hex: &str,
) -> Result<FedBackupWriteTokenMintReply, PoolError> {
    let req = FedBackupWriteTokenMintRequest {
        owner_actor_id: owner_actor_hex.to_string(),
    };
    match pool
        .originate_expecting(
            state,
            peer_url,
            expected_nest_id,
            "fauna.federation.backup.write_token.mint",
            fresh_idempotency_key(),
            to_value(&req),
        )
        .await?
    {
        Ok(v) => value_to::<FedBackupWriteTokenMintReply>(&v)
            .map_err(|()| PoolError::Dial("decode backup.write_token.mint reply".to_string())),
        Err(e) => Err(PoolError::Dial(format!(
            "peer backup.write_token.mint: {}",
            e.code
        ))),
    }
}

/// Push a succession statement to one peer (`identity-succession.md:81`).
///
/// Idempotent: the receiver's `record_peer_succession` is keyed on
/// `old_actor_id PRIMARY KEY`, so a re-delivered push is a no-op that replies
/// `recorded: false` rather than a second application. A failed push is not
/// escalated anywhere — an unreachable peer is the residual
/// `identity-succession.md:103` declares, and the pull leg is what closes it.
pub async fn originate_succession_push(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    req: &FedSuccessionPushRequest,
) -> Result<FedSuccessionPushReply, PoolError> {
    match pool
        .originate(
            state,
            peer_url,
            "fauna.federation.succession.push",
            fresh_idempotency_key(),
            to_value(req),
        )
        .await?
    {
        Ok(v) => value_to::<FedSuccessionPushReply>(&v)
            .map_err(|()| PoolError::Dial("decode succession.push reply".to_string())),
        Err(e) => Err(PoolError::Dial(format!("peer succession.push: {}", e.code))),
    }
}

/// Originate `fauna.federation.backup.changes.record` (exactly-once by
/// content). Records one uploaded segment/manifest path as custody on the
/// destination's owner-owned custody-copy reserved set. `Ok(reply)` ⇒ the
/// custody `seq` (identical on a content-identical replay).
///
/// `expected_nest_id`: see [`originate_backup_write_token_mint`].
pub async fn originate_backup_changes_record(
    pool: &FederationChannelPool,
    state: &Arc<AppState>,
    peer_url: &str,
    expected_nest_id: &[u8],
    req: &FedBackupChangesRecordRequest,
) -> Result<FedBackupChangesRecordReply, PoolError> {
    match pool
        .originate_expecting(
            state,
            peer_url,
            expected_nest_id,
            "fauna.federation.backup.changes.record",
            fresh_idempotency_key(),
            to_value(req),
        )
        .await?
    {
        Ok(v) => value_to::<FedBackupChangesRecordReply>(&v)
            .map_err(|()| PoolError::Dial("decode backup.changes.record reply".to_string())),
        Err(e) => Err(PoolError::Dial(format!(
            "peer backup.changes.record: {}",
            e.code
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The discovery trust rule's pure table (`federation.md` § Peer-auth
    /// model → *Discovery trust rule*): WebPKI admits everywhere; below it
    /// only the two carve-outs admit, and every production shape is refused.
    #[test]
    fn discovery_trust_rule_table() {
        use crate::federation_channel::PeerUrlOrigin::{Configured, Supplied};
        use fauna_ws_substrate::tls_verify::CapturedCert;
        let webpki = CapturedCert {
            spki: Some([1u8; 32]),
            webpki_valid: true,
        };
        let floor = CapturedCert {
            spki: Some([2u8; 32]),
            webpki_valid: false,
        };
        let none = CapturedCert::default();

        // WebPKI-valid: authenticated the boring way, whatever the class/origin.
        for class in [
            PeerHostClass::Loopback,
            PeerHostClass::NonGlobal,
            PeerHostClass::Global,
        ] {
            for origin in [Supplied, Configured] {
                assert_eq!(
                    discovery_dial_admits(true, class, origin, &webpki),
                    Ok(DiscoveryTrust::WebPki),
                    "{class:?}/{origin:?}"
                );
            }
        }
        // Plaintext: no cert to judge (validate_peer_url bounds it to loopback).
        assert_eq!(
            discovery_dial_admits(false, PeerHostClass::Loopback, Supplied, &none),
            Ok(DiscoveryTrust::Plaintext)
        );
        // The loopback fixture carve-out.
        assert_eq!(
            discovery_dial_admits(true, PeerHostClass::Loopback, Supplied, &floor),
            Ok(DiscoveryTrust::LoopbackFixture)
        );
        // The configured-private-target carve-out — and ONLY that pairing.
        assert_eq!(
            discovery_dial_admits(true, PeerHostClass::NonGlobal, Configured, &floor),
            Ok(DiscoveryTrust::ConfiguredPrivate)
        );
        assert!(discovery_dial_admits(true, PeerHostClass::NonGlobal, Supplied, &floor).is_err());
        // A production peer on an untrusted cert: refused, configured or not.
        assert!(discovery_dial_admits(true, PeerHostClass::Global, Supplied, &floor).is_err());
        assert!(discovery_dial_admits(true, PeerHostClass::Global, Configured, &floor).is_err());
        // A cert whose leaf failed to parse is no better than the floor.
        assert!(discovery_dial_admits(true, PeerHostClass::Global, Supplied, &none).is_err());
    }

    /// The host classifier on literals (no DNS): loopback by text, private
    /// and public IPs by the SSRF guard's own classifier, `http` and `https`
    /// alike.
    #[tokio::test]
    async fn classify_peer_host_literals() {
        for url in [
            "https://127.0.0.1:1",
            "http://localhost:1/",
            "https://[::1]:1",
        ] {
            assert_eq!(
                classify_peer_host(url).await.unwrap(),
                PeerHostClass::Loopback,
                "{url}"
            );
        }
        for url in [
            "https://172.18.0.2:3000",
            "https://10.0.0.5/",
            "https://100.64.1.2/",
        ] {
            assert_eq!(
                classify_peer_host(url).await.unwrap(),
                PeerHostClass::NonGlobal,
                "{url}"
            );
        }
        assert_eq!(
            classify_peer_host("https://1.1.1.1/").await.unwrap(),
            PeerHostClass::Global
        );
        assert!(matches!(
            classify_peer_host("not a url").await,
            Err(PoolError::Resolve(_))
        ));
    }

    /// One resolution's answer set decides both the class and the address
    /// dialed: `NonGlobal` only when every address is
    /// private, a mixed set is `Global` (strict) and dials a global member, so
    /// a private answer planted beside the real one can never reach the
    /// configured-private carve-out; an empty set is `Global` with no address.
    #[test]
    fn classify_resolved_addrs_judges_the_dialed_address() {
        use std::net::SocketAddr;
        let private: SocketAddr = "10.0.0.5:443".parse().unwrap();
        let private2: SocketAddr = "192.168.1.9:443".parse().unwrap();
        let public: SocketAddr = "1.1.1.1:443".parse().unwrap();

        let all_private = classify_resolved_addrs(&[private, private2]);
        assert_eq!(all_private.class, PeerHostClass::NonGlobal);
        assert_eq!(all_private.dial_addr, Some(private));

        for mixed in [[private, public], [public, private]] {
            let got = classify_resolved_addrs(&mixed);
            assert_eq!(got.class, PeerHostClass::Global, "{mixed:?}");
            assert_eq!(got.dial_addr, Some(public), "{mixed:?}");
        }

        let none = classify_resolved_addrs(&[]);
        assert_eq!(none.class, PeerHostClass::Global);
        assert_eq!(none.dial_addr, None);
    }

    /// The pin binds a paired URL whoever's row names it; the exemption is
    /// granted only to an admin's; and the table is replaced wholesale, so a
    /// URL no row names any longer is neither pinned nor exempt.
    #[test]
    fn pairing_targets_pin_every_row_url_and_exempt_only_an_admins() {
        use crate::federation_channel::PeerUrlOrigin;
        let pool = FederationChannelPool::new();
        assert!(
            pool.check_target_pin("https://relay.example", &[2; 32])
                .is_ok()
        );
        assert_eq!(
            pool.peer_url_origin("https://relay.example"),
            PeerUrlOrigin::Supplied
        );

        pool.set_pairing_targets(HashMap::from([
            (
                "https://relay.example/".to_string(),
                PairingTargetTrust {
                    exempt: true,
                    pin: Some([1; 32]),
                },
            ),
            (
                "https://friend.example".to_string(),
                PairingTargetTrust {
                    exempt: false,
                    pin: Some([3; 32]),
                },
            ),
        ]));
        assert!(
            pool.check_target_pin("https://relay.example", &[1; 32])
                .is_ok()
        );
        assert!(matches!(
            pool.check_target_pin("https://relay.example", &[2; 32]),
            Err(PoolError::PeerMismatch { .. })
        ));
        assert!(matches!(
            pool.check_target_pin("https://friend.example", &[2; 32]),
            Err(PoolError::PeerMismatch { .. })
        ));
        assert_eq!(
            pool.peer_url_origin("https://relay.example/"),
            PeerUrlOrigin::Configured
        );
        assert_eq!(
            pool.peer_url_origin("https://friend.example"),
            PeerUrlOrigin::Supplied,
            "a non-admin's row is pinned but never exempt"
        );

        pool.set_pairing_targets(HashMap::new());
        assert!(
            pool.check_target_pin("https://relay.example", &[2; 32])
                .is_ok()
        );
        assert_eq!(
            pool.peer_url_origin("https://relay.example"),
            PeerUrlOrigin::Supplied
        );
    }
    use fauna_protocol::RpcDispatcher;
    use fauna_protocol::test_transport::{delayed_sink_pair, stalled_sink_transport};

    fn test_state() -> Arc<AppState> {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let mut b = crate::federation_router::FederationRouter::builder();
        crate::federation_handlers::register_federation_handlers(&mut b);
        Arc::new(AppState {
            federation_router: Arc::new(b.build()),
            ..AppState::for_test(Arc::new(db))
        })
    }

    /// Fill a dispatcher's outbound queue so the next enqueue has nowhere to
    /// go. The pool-level twin of `fauna_protocol::dispatcher`'s own
    /// `saturate_outbound` (private to that crate, so its probe shape is
    /// replicated here rather than reused): keep sending until one enqueue
    /// doesn't resolve within a short window, proving the queue is full
    /// without needing to know `OUTBOUND_CAPACITY`'s value.
    async fn saturate_outbound(dispatcher: &RpcDispatcher) -> Vec<fauna_protocol::RpcCall> {
        let mut held = Vec::new();
        while let Ok(Ok(call)) = tokio::time::timeout(
            Duration::from_millis(50),
            dispatcher.request_raw("fauna.protocol.echo", [0u8; 16], Value::Null, None),
        )
        .await
        {
            held.push(call);
        }
        held
    }

    /// **A peer that accepts the federation connection and then stops reading
    /// cannot delay eviction past `ORIGINATE_DEADLINE`** (`docs/goal/architecture/transport.md` § Request lifecycle, step
    /// 4).
    ///
    /// Before the fix, `originate` awaited plain `request_raw` — an
    /// **unbounded** enqueue — so a full outbound queue parked
    /// until the substrate's own dead-link detection fired, on the
    /// `KEEPALIVE_TIMEOUT` clock (~60s), not the `ORIGINATE_DEADLINE` (30s)
    /// this path declares to the peer. Against this test's synthetic stalled
    /// sink — which has no dead-link detection of its own at all — the
    /// pre-fix shape would hang forever; the outer timeout below is what
    /// turns that into a failing test rather than an actual hang.
    #[tokio::test(start_paused = true)]
    async fn a_peer_that_never_reads_is_evicted_on_originate_deadline_not_keepalive() {
        let state = test_state();
        let pool = FederationChannelPool::new();

        let peer_nest_id = [7u8; 32];
        // Not a loopback literal and not `https`, so the redial after
        // eviction fails at `validate_peer_url`'s synchronous scheme check —
        // no real connect attempt, no real network round trip at all. A live
        // "nothing listening" loopback port was tried here first, but its
        // connection-refused is real I/O: this environment's redial over it
        // measured a real, reproducible multi-second cost that has nothing to
        // do with `ORIGINATE_DEADLINE` or `KEEPALIVE_TIMEOUT`, which would
        // have forced a loose, less meaningful timing tolerance below. Only
        // `get_or_dial`'s cache hit (pre-seeded below) needs `peer_url` to
        // resolve anything on the FIRST attempt; the redial's own validity
        // is irrelevant to what this test is about.
        let peer_url = "http://example.invalid:1".to_string();

        // Pre-seed the resolved-id cache so `originate` never touches the
        // network for `fauna.nest.info` — this test is entirely about the
        // enqueue bound on an ALREADY-established channel.
        pool.url_nest_ids
            .lock()
            .await
            .insert(peer_url.clone(), peer_nest_id);

        let (transport, _feed) = stalled_sink_transport();
        let (dispatcher, driver) = RpcDispatcher::new(transport);
        // spawn-ok(test): the dispatcher's driver for this one test's stalled
        // transport. It holds no `AppState`, so no generation teardown has to
        // reach it, and it dies with this test's own (paused-clock) runtime.
        tokio::spawn(driver);
        let _held = saturate_outbound(&dispatcher).await;

        let conn = Arc::new(crate::federation_channel::FederationConnection::new(
            peer_nest_id,
            Arc::new(dispatcher),
        ));
        pool.channels
            .lock()
            .await
            .insert((peer_nest_id, peer_url.clone()), conn);

        // A generous outer SAFETY ceiling, not the assertion — the
        // elapsed-time check below is that. (`ORIGINATE_DEADLINE * 3` is 90s,
        // actually ABOVE `KEEPALIVE_TIMEOUT`'s 60s, not "well under" it as a
        // prior version of this comment claimed; this bound only has to be
        // large enough that a hang fails the test instead of parking forever,
        // against a double with no dead-link detection of its own at all.)
        let started = tokio::time::Instant::now();
        let outcome = tokio::time::timeout(
            ORIGINATE_DEADLINE * 3,
            pool.originate(
                &state,
                &peer_url,
                "fauna.federation.channel.fetch",
                [1u8; 16],
                Value::Null,
            ),
        )
        .await
        .expect(
            "the call must resolve at ORIGINATE_DEADLINE, not park at the enqueue \
             until KEEPALIVE_TIMEOUT or forever",
        );
        let elapsed = started.elapsed();

        // The stalled channel's enqueue times out (evicted); the retry then
        // re-dials `peer_url`, which fails `validate_peer_url` synchronously
        // (not https, not a loopback literal) — the overall call surfaces
        // that failure, not a hang. The exact error variant isn't this
        // test's concern, only that it resolved.
        assert!(
            outcome.is_err(),
            "expected the redial's invalid peer_url to fail validation, got {outcome:?}"
        );
        // A lower ceiling alone proves only that the wait ends, not that it
        // ends AT the deadline: a regression to a longer bound (e.g.
        // `KEEPALIVE_TIMEOUT`) would still resolve well inside the 90s outer
        // ceiling and pass. The redial's own failure is synchronous (no real
        // I/O, no timer of its own), so the elapsed paused time should land
        // right at `ORIGINATE_DEADLINE`.
        assert!(
            elapsed >= ORIGINATE_DEADLINE
                && elapsed < ORIGINATE_DEADLINE + Duration::from_millis(20),
            "expected the enqueue to time out at ORIGINATE_DEADLINE ({ORIGINATE_DEADLINE:?}), \
             not linger toward KEEPALIVE_TIMEOUT or beyond: got {elapsed:?}"
        );
    }

    /// **A peer that accepts a federation request — the enqueue succeeds,
    /// after spending part of the shared budget getting there — but never
    /// sends a reply cannot hang `originate` past `ORIGINATE_DEADLINE`, and
    /// the reply wait races only what's LEFT of that one budget, never a
    /// fresh one**
    /// (`docs/goal/architecture/transport.md` § Request lifecycle, "one
    /// budget covers the whole call, never re-granted at each stage").
    ///
    /// The dialer's sink stays not-ready for a deliberate slice of
    /// `ORIGINATE_DEADLINE` (like the enqueue-bound test above, but
    /// releasing partway through instead of never), so the enqueue itself
    /// spends real budget before the request reaches the peer — who then
    /// never calls `send_reply`, exactly the shape
    /// `dialer_handshake_reply_wait_is_bounded_by_handshake_deadline`
    /// (`federation_channel.rs`) uses for the handshake's own reply wait. A
    /// plain two-sided `make_pair` duplex, with no enqueue stall at all,
    /// would only prove the reply wait is independently bounded — it can't
    /// tell a shared budget from a bug that re-grants the reply wait a
    /// FRESH `ORIGINATE_DEADLINE` once the enqueue clears. Before the fix,
    /// `call.await_reply()` raced nothing local, so only the substrate's
    /// dead-link timer (re-armed by any inbound frame, e.g. a Pong) could
    /// end the wait; against an in-memory duplex with no dead-link
    /// detection at all, a pre-fix run would hang forever.
    #[tokio::test(start_paused = true)]
    async fn a_peer_that_never_replies_times_out_at_originate_deadline() {
        let state = test_state();
        let pool = FederationChannelPool::new();

        let peer_nest_id = [9u8; 32];
        let peer_url = "http://127.0.0.1:1".to_string();
        pool.url_nest_ids
            .lock()
            .await
            .insert(peer_url.clone(), peer_nest_id);

        let (dialer_t, listener_t, release_enqueue) = delayed_sink_pair();
        let (dispatcher, drv_d) = RpcDispatcher::new(dialer_t);
        let (disp_listener, drv_l) = RpcDispatcher::new(listener_t);
        // spawn-ok(test)
        tokio::spawn(drv_d);
        tokio::spawn(drv_l);

        // Fill the dialer's outbound queue to capacity while its sink is
        // still stalled, so the real request below must itself wait in
        // `out_tx.send` for the enqueue stage, exactly like the sibling
        // enqueue-eviction witness above.
        let _held = saturate_outbound(&dispatcher).await;

        // The "peer" takes whichever request lands first and withholds its
        // reply indefinitely — never calling `send_reply`. It may end up
        // consuming one of the queue-filling junk requests instead of the
        // real one below; immaterial, since it never replies to anything
        // either way.
        let mut inbound = disp_listener.inbound_requests().unwrap();
        // spawn-ok(test)
        let peer_task = tokio::spawn(async move {
            let _req = inbound.recv().await.unwrap();
            std::future::pending::<()>().await
        });

        let conn = Arc::new(crate::federation_channel::FederationConnection::new(
            peer_nest_id,
            Arc::new(dispatcher),
        ));
        pool.channels
            .lock()
            .await
            .insert((peer_nest_id, peer_url.clone()), conn);

        // Release the stalled sink partway through the shared budget — well
        // before ORIGINATE_DEADLINE — so the real request's enqueue
        // completes and the reply wait races whatever is LEFT of the same
        // budget, not a fresh one.
        let enqueue_stall = Duration::from_secs(10);
        // spawn-ok(test): releases the delayed sink once the paused clock
        // advances past `enqueue_stall`.
        tokio::spawn(async move {
            tokio::time::sleep(enqueue_stall).await;
            let _ = release_enqueue.send(());
        });

        // A generous outer SAFETY ceiling, not the assertion — the
        // elapsed-time check below is that.
        let started = tokio::time::Instant::now();
        let outcome = tokio::time::timeout(
            ORIGINATE_DEADLINE * 3,
            pool.originate(
                &state,
                &peer_url,
                "fauna.federation.channel.fetch",
                [1u8; 16],
                Value::Null,
            ),
        )
        .await
        .expect("the call must resolve at ORIGINATE_DEADLINE, not hang on a withheld reply");
        let elapsed = started.elapsed();

        peer_task.abort();

        let reply =
            outcome.expect("a withheld reply must not surface a PoolError (no dial happened)");
        let err = reply.expect_err("a withheld reply must not be treated as success");
        assert_eq!(
            err.code, "fauna.protocol.timeout",
            "expected a local timeout, not eviction/redial — a re-send would re-grant the \
             budget this attempt already spent"
        );
        // The whole call must resolve at ORIGINATE_DEADLINE — the ONE shared
        // budget, spent partly stalled in the enqueue and the rest in the
        // reply wait. A bug re-granting a fresh budget to the reply wait
        // would instead resolve at `enqueue_stall + ORIGINATE_DEADLINE` (40s).
        assert!(
            elapsed >= ORIGINATE_DEADLINE
                && elapsed < ORIGINATE_DEADLINE + Duration::from_millis(20),
            "expected the call to resolve at ORIGINATE_DEADLINE ({ORIGINATE_DEADLINE:?}) — the \
             shared budget spent partly in the {enqueue_stall:?} enqueue stall and the rest in \
             the reply wait — not a fresh per-stage timer: got {elapsed:?}"
        );
    }

    /// **Syntax-gated before it ever counts as an attempt** — . Every byte of this domain is a
    /// legal hostname character; only the length ceiling
    /// (`fauna_core::web::MAX_HOSTNAME_BYTES`) catches it, so a validator
    /// that merely re-checked metacharacters would let it through.
    #[tokio::test]
    async fn resolve_domain_nest_id_rejects_an_oversized_domain_before_touching_the_network() {
        let pool = FederationChannelPool::new();
        let oversized = format!("{}.example", "a".repeat(300));

        let before = pool.domain_resolve_attempt_count();
        let err = pool
            .resolve_domain_nest_id(&oversized)
            .await
            .expect_err("an oversized domain is not a handle domain");
        assert!(matches!(err, PoolError::Resolve(_)), "got {err:?}");
        assert_eq!(
            pool.domain_resolve_attempt_count(),
            before,
            "a syntactically invalid domain must never count as an attempted \
             discovery"
        );
    }

    /// **Singleflight per domain** — : two distinct handles asserting the same hanging domain at once
    /// must cost exactly one discovery attempt, not two. `127.0.0.1:1` is
    /// loopback with nothing listening, so the connect refuses fast and
    /// deterministically (no real DNS or outbound network) — the same
    /// dead-loopback shape the conformance suite's own alternating-announce
    /// test uses — but the async TCP connect still yields at least once
    /// before it resolves, which is what gives the first caller a genuine
    /// window to mark itself in flight before the second is ever polled.
    #[tokio::test]
    async fn concurrent_resolves_of_the_same_domain_singleflight_to_one_attempt() {
        let pool = FederationChannelPool::new();
        let domain = "127.0.0.1:1";

        let (a, b) = tokio::join!(
            pool.resolve_domain_nest_id(domain),
            pool.resolve_domain_nest_id(domain),
        );
        let in_flight_hits = [&a, &b]
            .into_iter()
            .filter(|r| matches!(r, Err(PoolError::ResolveInFlight(_))))
            .count();
        assert_eq!(
            in_flight_hits, 1,
            "exactly one of two concurrent callers for the same domain must \
             see the in-flight marker: got {a:?} / {b:?}"
        );
        assert_eq!(
            pool.domain_resolve_attempt_count(),
            1,
            "two distinct handles at one hanging domain must make exactly one \
             discovery attempt, not two"
        );
    }

    /// [`FederationChannelPool::discard_verification`] must remove the
    /// current generation's assertion but never a newer one that has already
    /// superseded it — the same guard [`FederationChannelPool::is_current_verification`]
    /// uses ().
    #[tokio::test]
    async fn discard_verification_only_removes_the_still_current_generation() {
        let pool = FederationChannelPool::new();
        let channel = [9u8; 32];
        let actor = [10u8; 32];

        let gen1 = pool
            .begin_verification(channel, actor, "bob", "example.com")
            .await
            .expect("first assertion always starts a verification");

        // Discarding a STALE generation (one already superseded) must be a
        // no-op against the newer one.
        let gen2 = pool
            .begin_verification(channel, actor, "bob", "other.example")
            .await
            .expect("a distinct domain is a new assertion");
        assert_ne!(gen1, gen2);
        pool.discard_verification(channel, actor, gen1).await;
        assert!(
            pool.is_current_verification(channel, actor, gen2).await,
            "discarding a stale generation must not touch the current one"
        );

        // Discarding the CURRENT generation removes it entirely, so the
        // identical assertion is treated as new rather than suppressed for
        // the rest of ANNOUNCE_VERIFY_TTL.
        pool.discard_verification(channel, actor, gen2).await;
        let gen3 = pool
            .begin_verification(channel, actor, "bob", "other.example")
            .await;
        assert!(
            gen3.is_some(),
            "discarding the current generation must let an identical \
             assertion be retried, not suppressed for the rest of the window"
        );
    }

    /// **A fixed cap, not just the TTL window** — : a TTL sweep alone still bounds the negative
    /// cache by the peer's own send rate times `ANNOUNCE_VERIFY_TTL`, not by
    /// a fixed number, so a peer sending distinct (but syntactically valid)
    /// domains fast enough grows the map without limit inside one window.
    /// Exercises [`FederationChannelPool::record_domain_failure`] directly
    /// rather than through real network fetches — the mechanism under test
    /// is the cap arithmetic, not the resolver (convention 14: no re-timing
    /// a real discovery just to observe a counter).
    #[tokio::test]
    async fn record_domain_failure_is_bounded_by_a_fixed_cap_not_only_the_ttl_window() {
        let pool = FederationChannelPool::new();
        for i in 0..(MAX_DOMAIN_FAILURE_ENTRIES + 200) {
            pool.record_domain_failure(format!("dead-{i}.example"))
                .await;
        }
        assert_eq!(
            pool.domain_resolve_failure_count().await,
            MAX_DOMAIN_FAILURE_ENTRIES,
            "the negative cache must never grow past its fixed cap, however \
             many distinct domains fail inside one window"
        );

        // A domain already tracked may still refresh past the cap — the cap
        // bounds distinct domains tracked, not re-failures of one already
        // in the map.
        pool.record_domain_failure("dead-0.example".to_string())
            .await;
        assert_eq!(
            pool.domain_resolve_failure_count().await,
            MAX_DOMAIN_FAILURE_ENTRIES,
            "refreshing an already-cached domain must not grow the map"
        );
    }

    /// **A full cache does not evict — the doc-declared full-cache behaviour**
    /// (`federation.md:465-472`, ): past [`MAX_DOMAIN_FAILURE_ENTRIES`] a newly-failing domain is
    /// not cached at all, no eviction makes room for it. So the
    /// one-discovery-per-distinct-domain-per-window bound holds only while a
    /// peer's distinct-domain footprint inside one window stays at or under
    /// the cap; past it, the new domain pays a fresh discovery on every
    /// subsequent assertion instead of being cached for the rest of the
    /// window.
    #[tokio::test]
    async fn a_full_cache_does_not_cache_a_new_domains_failure() {
        let pool = FederationChannelPool::new();
        for i in 0..MAX_DOMAIN_FAILURE_ENTRIES {
            pool.record_domain_failure(format!("dead-{i}.example"))
                .await;
        }
        assert_eq!(
            pool.domain_resolve_failure_count().await,
            MAX_DOMAIN_FAILURE_ENTRIES
        );

        pool.record_domain_failure("dead-overflow.example".to_string())
            .await;
        assert_eq!(
            pool.domain_resolve_failure_count().await,
            MAX_DOMAIN_FAILURE_ENTRIES,
            "the map must stay at the cap, not grow past it"
        );
        assert!(
            !pool
                .domain_resolve_failures
                .lock()
                .await
                .contains_key("dead-overflow.example"),
            "past the cap, the NEW domain must not be cached at all — with \
             no eviction it pays a fresh discovery on every assertion \
             instead of being cached for the rest of the window"
        );
    }

    /// **The expiry sweep, not just the cap** — remove the `retain` sweep at
    /// the top of [`FederationChannelPool::record_domain_failure`], or move
    /// it after the cap check, and this reds. Backdating only a handful of
    /// entries can't catch a sweep-after-cap reorder — with the cap far
    /// from full, the cap check passes either way and both orders leave the
    /// same count. Filling to [`MAX_DOMAIN_FAILURE_ENTRIES`] before
    /// backdating puts the cap on the boundary: a sweep run after the cap
    /// check finds the map still full of (unswept) stale entries, so the
    /// insert is refused, and the map ends up empty instead of holding the
    /// fresh entry ().
    /// Backdates entries directly rather than waiting out a real
    /// `ANNOUNCE_VERIFY_TTL` (convention 14: no re-timing a real window just
    /// to observe a counter).
    #[tokio::test]
    async fn record_domain_failure_sweeps_entries_older_than_the_ttl() {
        let pool = FederationChannelPool::new();
        let expired_at = Instant::now()
            .checked_sub(ANNOUNCE_VERIFY_TTL + Duration::from_secs(1))
            .expect("test host has been up longer than one ANNOUNCE_VERIFY_TTL");
        {
            let mut failures = pool.domain_resolve_failures.lock().await;
            for i in 0..MAX_DOMAIN_FAILURE_ENTRIES {
                failures.insert(format!("stale-{i}.example"), expired_at);
            }
        }
        assert_eq!(
            pool.domain_resolve_failure_count().await,
            MAX_DOMAIN_FAILURE_ENTRIES
        );

        pool.record_domain_failure("fresh.example".to_string())
            .await;

        assert_eq!(
            pool.domain_resolve_failure_count().await,
            1,
            "every entry older than ANNOUNCE_VERIFY_TTL must be swept before \
             the cap check runs, leaving only the fresh insert — even when \
             the cache was full of nothing but expired entries"
        );
    }
}
