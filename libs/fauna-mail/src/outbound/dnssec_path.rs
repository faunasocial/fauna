//! The DNSSEC-capable resolver path: one per-process decision both outbound
//! DANE legs consult.
//!
//! Implements `docs/goal/behavior/smtp-server.md` § MX resolution +
//! IPv4/IPv6 mixed handling, item 1 — the resolver-path paragraph. RFC 7672
//! §2.1.1 requires a security-aware resolver; a nest works out of the box on
//! whatever resolver its box has, and some strip DNSSEC records (a container
//! or VPS forwarder, the unsigned e2e fake DNS). On such a path every
//! validating lookup fails, so the Bogus / Indeterminate rule alone — a
//! validating lookup that fails delays delivery — would defer every delivery
//! for ever.
//!
//! So the nest decides, by a probe that depends on no recipient and no third
//! party, whether its path carries DNSSEC: a validating lookup of the root
//! zone's own apex SOA, which proves `Secure` against the built-in root trust
//! anchor exactly when the path passes the DNSSEC records through.
//!
//! - **Capable** → both DANE legs run validating and a failed validation is a
//!   lookup failure ([`crate::outbound::mx::lookup_mx_on_path`],
//!   [`crate::outbound::dane::lookup_tlsa_on_path`]).
//! - **Incapable** → the nest is, honestly, a pre-DANE opportunistic-TLS MTA:
//!   MX is read without validation at `secure: false` and TLSA answers no
//!   records without looking.
//!
//! The posture moves ONE way while the process lives: an incapable path may
//! be re-probed (on a later lookup, at most once per
//! [`INCAPABLE_REPROBE_INTERVAL`]) and promoted; a capable path is never
//! demoted — a runtime demotion would hand the attacker who can make the probe
//! fail the very downgrade the rule refuses.
//!
//! **The transport is part of what "capable" means** (§ MX resolution item 1,
//! *The validating resolver speaks TCP*): the probe and both DANE legs build
//! their resolver from [`validating_resolver`], which queries the box's
//! configured name servers over TCP only ([`tcp_only`]). A forwarder's
//! truncated UDP answer to a large DNSSEC RRset is not reliably well-formed —
//! systemd-resolved's stub cuts the message at the client's payload, which the
//! validator cannot decode and does not retry over TCP — so a path that cannot
//! carry a validated answer whole is incapable, honestly, never capable at the
//! root and broken below it. The incapable path's non-validating MX read keeps
//! the default transports.
//!
//! Native-only (`outbound-net`): the live probe is a hickory lookup.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

/// How long an incapable verdict stands before the next lookup re-probes.
pub const INCAPABLE_REPROBE_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// The resolver path's DNSSEC posture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolverPath {
    /// The root apex SOA proved `Secure`: DNSSEC records reach the validator.
    Capable,
    /// Anything else: the path does not carry DNSSEC (or could not be read).
    Incapable,
}

/// One probe of the resolver path. `true` = the root apex SOA proved `Secure`.
#[async_trait]
pub trait ResolverPathProber: Send + Sync {
    async fn probe(&self) -> bool;
}

/// A prober with a fixed answer — tests, and builds that model no DNS.
pub struct FixedPathProber(pub bool);

#[async_trait]
impl ResolverPathProber for FixedPathProber {
    async fn probe(&self) -> bool {
        self.0
    }
}

#[derive(Debug, Clone, Copy)]
enum State {
    Unprobed,
    Capable,
    Incapable { probed_at: Instant },
}

/// The per-process posture, shared (`Arc`) by the MX and TLSA resolvers and
/// the deliverability diagnostics.
pub struct DnssecPath {
    prober: Box<dyn ResolverPathProber>,
    // A tokio mutex held across the probe, so concurrent first lookups wait
    // for one probe instead of each running their own.
    state: tokio::sync::Mutex<State>,
    reprobe_interval: Duration,
}

impl DnssecPath {
    pub fn new(prober: Box<dyn ResolverPathProber>) -> Self {
        Self::with_reprobe_interval(prober, INCAPABLE_REPROBE_INTERVAL)
    }

    pub fn with_reprobe_interval(
        prober: Box<dyn ResolverPathProber>,
        reprobe_interval: Duration,
    ) -> Self {
        Self {
            prober,
            state: tokio::sync::Mutex::new(State::Unprobed),
            reprobe_interval,
        }
    }

    /// A path with a fixed posture (tests).
    pub fn fixed(path: ResolverPath) -> Arc<Self> {
        Arc::new(Self::new(Box::new(FixedPathProber(
            path == ResolverPath::Capable,
        ))))
    }

    /// The live path over the box's resolver (the root apex SOA probe).
    pub fn live() -> Arc<Self> {
        Arc::new(Self::new(Box::new(LiveRootSoaProber)))
    }

    /// The standing posture, probing when none is held yet or when an
    /// incapable verdict is older than the re-probe interval. A capable
    /// verdict is final for the life of the process.
    pub async fn posture(&self) -> ResolverPath {
        let mut state = self.state.lock().await;
        match *state {
            State::Capable => return ResolverPath::Capable,
            State::Incapable { probed_at } if probed_at.elapsed() < self.reprobe_interval => {
                return ResolverPath::Incapable;
            }
            State::Unprobed | State::Incapable { .. } => {}
        }
        let first = matches!(*state, State::Unprobed);
        if self.prober.probe().await {
            *state = State::Capable;
            tracing::warn!(
                target: "outbound_dane",
                "resolver path carries DNSSEC: outbound DANE active; a DNSSEC validation \
                 failure on either DANE leg now delays delivery (RFC 7672 §2.1)"
            );
            ResolverPath::Capable
        } else {
            *state = State::Incapable {
                probed_at: Instant::now(),
            };
            if first {
                tracing::warn!(
                    target: "outbound_dane",
                    "resolver path does not carry DNSSEC (the root zone's SOA did not prove \
                     Secure): outbound DANE unavailable, delivering over MTA-STS / \
                     opportunistic TLS; the fix is the box's resolver"
                );
            }
            ResolverPath::Incapable
        }
    }
}

impl DnssecPath {
    /// Re-run the probe now, whatever the held posture — the deliverability
    /// diagnostics' re-run (`mail-deliverability.md` § Symptom diagnostics →
    /// *Checks*, the DNSSEC resolver path row). `true` = the root apex SOA
    /// proved `Secure` on this run. A capable answer promotes the held
    /// posture; an incapable one never demotes it (the one-way rule).
    pub async fn reprobe(&self) -> bool {
        let mut state = self.state.lock().await;
        let capable = self.prober.probe().await;
        if capable {
            if !matches!(*state, State::Capable) {
                tracing::warn!(
                    target: "outbound_dane",
                    "resolver path carries DNSSEC: outbound DANE active (promoted by a re-probe)"
                );
            }
            *state = State::Capable;
        } else if !matches!(*state, State::Capable) {
            *state = State::Incapable {
                probed_at: Instant::now(),
            };
        }
        capable
    }
}

/// `cfg` with every name server rewritten to a single TCP connection — the
/// validating resolver's transport (module doc). Each server keeps its
/// address, its negative-response trust and the port (and bind address) of
/// its existing TCP connection, else of its first connection; the domain and
/// the search list are kept.
pub fn tcp_only(
    cfg: &hickory_resolver::config::ResolverConfig,
) -> hickory_resolver::config::ResolverConfig {
    use hickory_resolver::config::{
        ConnectionConfig, NameServerConfig, ProtocolConfig, ResolverConfig,
    };

    let servers = cfg
        .name_servers()
        .iter()
        .map(|ns| {
            let tcp = ns
                .connections
                .iter()
                .find(|c| matches!(c.protocol, ProtocolConfig::Tcp))
                .cloned()
                .unwrap_or_else(|| {
                    let mut tcp = ConnectionConfig::tcp();
                    if let Some(first) = ns.connections.first() {
                        tcp.port = first.port;
                        tcp.bind_addr = first.bind_addr;
                    }
                    tcp
                });
            NameServerConfig::new(ns.ip, ns.trust_negative_responses, vec![tcp])
        })
        .collect();
    ResolverConfig::from_parts(cfg.domain().cloned(), cfg.search().to_vec(), servers)
}

/// The one validating resolver the probe and both DANE legs use: the box's
/// system resolver configuration, rewritten by [`tcp_only`], with DNSSEC
/// validation on.
pub fn validating_resolver() -> anyhow::Result<hickory_resolver::TokioResolver> {
    let (cfg, mut opts) = hickory_resolver::system_conf::read_system_conf()
        .map_err(|e| anyhow::anyhow!("read system resolver config: {e}"))?;
    opts.validate = true;
    hickory_resolver::TokioResolver::builder_with_config(tcp_only(&cfg), Default::default())
        .with_options(opts)
        .build()
        .map_err(|e| anyhow::anyhow!("build resolver: {e}"))
}

/// The live probe: a validating lookup of `.` SOA through the box's resolver,
/// over TCP ([`validating_resolver`]).
pub struct LiveRootSoaProber;

#[async_trait]
impl ResolverPathProber for LiveRootSoaProber {
    async fn probe(&self) -> bool {
        use hickory_resolver::proto::dnssec::Proof;
        use hickory_resolver::proto::rr::{Name, RData, RecordType};

        let resolver = match validating_resolver() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(target: "outbound_dane", "resolver-path probe: {e}");
                return false;
            }
        };
        match resolver.lookup(Name::root(), RecordType::SOA).await {
            Ok(lookup) => {
                let soas: Vec<_> = lookup
                    .answers()
                    .iter()
                    .filter(|r| matches!(r.data, RData::SOA(_)))
                    .collect();
                !soas.is_empty() && soas.iter().all(|r| r.proof == Proof::Secure)
            }
            Err(e) => {
                tracing::debug!(target: "outbound_dane", "resolver-path probe: {e}");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Answers from a script, counting probes.
    struct Scripted {
        answers: std::sync::Mutex<Vec<bool>>,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ResolverPathProber for Scripted {
        async fn probe(&self) -> bool {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut a = self.answers.lock().unwrap();
            if a.len() > 1 { a.remove(0) } else { a[0] }
        }
    }

    fn scripted(answers: &[bool], interval: Duration) -> (DnssecPath, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let path = DnssecPath::with_reprobe_interval(
            Box::new(Scripted {
                answers: std::sync::Mutex::new(answers.to_vec()),
                calls: calls.clone(),
            }),
            interval,
        );
        (path, calls)
    }

    #[tokio::test]
    async fn a_capable_path_is_probed_once_and_never_demoted() {
        let (path, calls) = scripted(&[true, false], Duration::ZERO);
        assert_eq!(path.posture().await, ResolverPath::Capable);
        assert_eq!(path.posture().await, ResolverPath::Capable);
        assert_eq!(path.posture().await, ResolverPath::Capable);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "capable is final");
    }

    #[tokio::test]
    async fn an_incapable_verdict_stands_inside_the_interval() {
        let (path, calls) = scripted(&[false, true], Duration::from_secs(3600));
        assert_eq!(path.posture().await, ResolverPath::Incapable);
        assert_eq!(path.posture().await, ResolverPath::Incapable);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_incapable_path_is_reprobed_and_promoted() {
        let (path, calls) = scripted(&[false, true, false], Duration::ZERO);
        assert_eq!(path.posture().await, ResolverPath::Incapable);
        assert_eq!(path.posture().await, ResolverPath::Capable);
        assert_eq!(path.posture().await, ResolverPath::Capable);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_reprobe_promotes_but_never_demotes() {
        let (path, _) = scripted(&[false, true, false], Duration::from_secs(3600));
        assert_eq!(path.posture().await, ResolverPath::Incapable);
        assert!(path.reprobe().await);
        assert_eq!(path.posture().await, ResolverPath::Capable);
        assert!(!path.reprobe().await, "the re-run reports what it saw");
        assert_eq!(path.posture().await, ResolverPath::Capable, "never demoted");
    }

    #[tokio::test]
    async fn fixed_paths_answer_their_posture() {
        assert_eq!(
            DnssecPath::fixed(ResolverPath::Capable).posture().await,
            ResolverPath::Capable
        );
        assert_eq!(
            DnssecPath::fixed(ResolverPath::Incapable).posture().await,
            ResolverPath::Incapable
        );
    }

    // ── The TCP transport (smtp-server.md § MX resolution item 1, the TCP
    // paragraph) ─────────────────────────────────────────────────────────────

    use hickory_resolver::config::{
        ConnectionConfig, NameServerConfig, ProtocolConfig, ResolverConfig, ResolverOpts,
    };
    use std::net::{IpAddr, Ipv4Addr};

    fn udp_then_tcp(ip: IpAddr, port: u16) -> NameServerConfig {
        let mut udp = ConnectionConfig::udp();
        udp.port = port;
        let mut tcp = ConnectionConfig::tcp();
        tcp.port = port;
        NameServerConfig::new(ip, true, vec![udp, tcp])
    }

    #[test]
    fn tcp_only_keeps_one_tcp_connection_per_server_and_the_search_list() {
        use hickory_resolver::proto::rr::Name;
        use std::str::FromStr;

        let domain = Name::from_str("example.test.").unwrap();
        let search = vec![Name::from_str("corp.example.test.").unwrap()];
        let cfg = ResolverConfig::from_parts(
            Some(domain.clone()),
            search.clone(),
            vec![
                udp_then_tcp(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 53),
                udp_then_tcp(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2)), 5353),
            ],
        );
        let out = tcp_only(&cfg);
        assert_eq!(out.domain(), Some(&domain));
        assert_eq!(out.search(), search.as_slice());
        let servers = out.name_servers();
        assert_eq!(servers.len(), 2);
        for (ns, (ip, port)) in servers.iter().zip([
            (Ipv4Addr::new(192, 0, 2, 1), 53),
            (Ipv4Addr::new(192, 0, 2, 2), 5353),
        ]) {
            assert_eq!(ns.ip, IpAddr::V4(ip));
            assert_eq!(ns.connections.len(), 1, "exactly one connection");
            assert!(matches!(ns.connections[0].protocol, ProtocolConfig::Tcp));
            assert_eq!(ns.connections[0].port, port, "the server's port is kept");
        }
    }

    #[test]
    fn tcp_only_builds_a_tcp_connection_on_the_port_of_a_udp_only_server() {
        let mut udp = ConnectionConfig::udp();
        udp.port = 5300;
        let cfg = ResolverConfig::from_parts(
            None,
            vec![],
            vec![NameServerConfig::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                true,
                vec![udp],
            )],
        );
        let out = tcp_only(&cfg);
        let conns = &out.name_servers()[0].connections;
        assert_eq!(conns.len(), 1);
        assert!(matches!(conns[0].protocol, ProtocolConfig::Tcp));
        assert_eq!(conns[0].port, 5300);
    }

    /// The answer both responders give: one A record for the queried name,
    /// echoing the query.
    fn a_answer(
        query: &hickory_resolver::proto::op::Message,
    ) -> hickory_resolver::proto::op::Message {
        use hickory_resolver::proto::op::{Message, MessageType, OpCode};
        use hickory_resolver::proto::rr::rdata::A;
        use hickory_resolver::proto::rr::{RData, Record};

        let mut resp = Message::new(query.metadata.id, MessageType::Response, OpCode::Query);
        resp.metadata.recursion_desired = query.metadata.recursion_desired;
        resp.metadata.recursion_available = true;
        for q in &query.queries {
            resp.add_query(q.clone());
            resp.add_answer(Record::from_rdata(
                q.name().clone(),
                60,
                RData::A(A::new(192, 0, 2, 25)),
            ));
        }
        resp
    }

    /// A UDP responder that answers like systemd-resolved's stub answers a
    /// too-large query — the message cut inside its last record, TC set, the
    /// answer count unrewritten — beside a TCP responder on the same port that
    /// answers the whole message. Returns the shared port.
    async fn spawn_hard_cut_pair() -> u16 {
        use hickory_resolver::proto::op::Message;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::{TcpListener, UdpSocket};

        let (listener, socket) = loop {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind tcp");
            let port = listener.local_addr().expect("local addr").port();
            if let Ok(socket) = UdpSocket::bind(("127.0.0.1", port)).await {
                break (listener, socket);
            }
        };
        let port = listener.local_addr().expect("local addr").port();
        tokio::spawn(async move {
            let mut buf = [0u8; 1500];
            loop {
                let Ok((len, peer)) = socket.recv_from(&mut buf).await else {
                    return;
                };
                let Ok(query) = Message::from_vec(&buf[..len]) else {
                    continue;
                };
                let mut resp = a_answer(&query);
                resp.metadata.truncation = true;
                let Ok(mut bytes) = resp.to_vec() else {
                    continue;
                };
                // Cut inside the A record's RDATA: the header still counts
                // one answer, whose bytes are not all there.
                bytes.truncate(bytes.len() - 2);
                let _ = socket.send_to(&bytes, peer).await;
            }
        });
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    loop {
                        let mut len = [0u8; 2];
                        if stream.read_exact(&mut len).await.is_err() {
                            return;
                        }
                        let mut buf = vec![0u8; u16::from_be_bytes(len) as usize];
                        if stream.read_exact(&mut buf).await.is_err() {
                            return;
                        }
                        let Ok(query) = Message::from_vec(&buf) else {
                            return;
                        };
                        let Ok(bytes) = a_answer(&query).to_vec() else {
                            return;
                        };
                        let mut framed = (bytes.len() as u16).to_be_bytes().to_vec();
                        framed.extend_from_slice(&bytes);
                        if stream.write_all(&framed).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        port
    }

    async fn lookup_a(cfg: ResolverConfig) -> anyhow::Result<()> {
        let mut opts = ResolverOpts::default();
        opts.validate = false; // the transport is under test, not the chain
        opts.attempts = 1;
        opts.cache_size = 0;
        opts.timeout = Duration::from_secs(5);
        let resolver =
            hickory_resolver::TokioResolver::builder_with_config(cfg, Default::default())
                .with_options(opts)
                .build()
                .map_err(|e| anyhow::anyhow!("build resolver: {e}"))?;
        let lookup = resolver
            .lookup(
                "mx.example.test.",
                hickory_resolver::proto::rr::RecordType::A,
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        anyhow::ensure!(!lookup.answers().is_empty(), "no answer");
        Ok(())
    }

    /// The mechanism the TCP rule rests on, pinned: a hard-cut truncated UDP
    /// answer fails the default transports (no fall-back to TCP from a decode
    /// error), while the same server through `tcp_only` resolves.
    #[tokio::test]
    async fn a_hard_cut_udp_answer_fails_the_default_transports_but_not_tcp_only() {
        let port = spawn_hard_cut_pair().await;
        let cfg = ResolverConfig::from_parts(
            None,
            vec![],
            vec![udp_then_tcp(IpAddr::V4(Ipv4Addr::LOCALHOST), port)],
        );
        lookup_a(tcp_only(&cfg))
            .await
            .expect("tcp_only resolves through the hard-cutting server");
        let err = lookup_a(cfg)
            .await
            .expect_err("the default transports read the cut UDP message");
        assert!(!err.to_string().is_empty());
    }
}
