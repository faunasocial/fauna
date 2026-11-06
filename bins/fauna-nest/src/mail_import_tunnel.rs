//! The web mail-import tunnel — a blind WebSocket-to-TCP byte tunnel for the
//! web app's source transport, because a browser has no raw TCP.
//!
//! Owner: `docs/goal/behavior/mailbox-migration.md` § Where the IMAP client
//! runs → *The web tunnel*; its entry among the outbound primitives a user can
//! aim is `docs/goal/architecture/deployment-threat-model.md` § Concern 1b
//! item 3. The wire is `fauna_protocol::mail_import_tunnel`.
//!
//! **What it carries: TLS to a public host, and nothing else.** One
//! destination per tunnel, named by the client's first frame; 993 (implicit
//! TLS) or 143 (STARTTLS) to any globally-routable host, or 443 to the
//! Microsoft identity authority alone; resolved and pinned through the shared
//! SSRF guard ([`crate::ssrf::resolve_global_addrs`]). The client's first
//! bytes must be a TLS ClientHello record — on 143 after at most one
//! `<tag> STARTTLS` line ([`ShapeGate`]) — so the nest relays no plaintext
//! protocol. TLS ends in the client: the nest sees ciphertext only, never the
//! source credential, the OAuth bearer or the mail.
//!
//! **Who opens it:** the signed-in actor's bearer, offered in
//! `Sec-WebSocket-Protocol` exactly as the WS-RPC upgrade offers it, through
//! the same standing-aware validator ([`crate::auth::check_bearer_session`]).
//! It is validated once, so the live tunnel registers in the actor's teardown
//! ([`crate::ws::WsState::register_tunnel`]): every revocation that closes the
//! actor's — or that session's, or that device's — RPC sockets closes its
//! tunnels too, `4401 revoked` (`transport-connection.md` § *Revocation
//! teardown*).
//!
//! **Bounds** — Rust constants (nobody chooses them): [`MAX_TUNNELS_PER_ACTOR`]
//! and [`MAX_TUNNELS_PER_NEST`] concurrent tunnels, opens metered per actor by
//! [`OPEN_LIMITER_CONFIG`], the protocol crate's idle and frame bounds; no
//! byte-rate and no lifetime bound (an import may take hours).
//!
//! **The e2e seam** ([`LoopbackAdmission`]): the harness's IMAP source and
//! identity fake listen on loopback, which the SSRF guard refuses. A
//! `test-hooks` nest reads the launch environment's seeds and admits exactly
//! the loopback ports they name (`e2e-gated-seeds.md` § The source-IMAP trust
//! seed → *The tunnel's reading*); a shipped build compiles none of it.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::response::{IntoResponse, Response};
use fauna_protocol::codec::{decode_strict, encode_canonical};
use fauna_protocol::mail_import_tunnel::{
    IDLE_SECS, MAX_FRAME_BYTES, MICROSOFT_AUTHORITY_HOST, PORT_HTTPS, PORT_IMAP_STARTTLS,
    PORT_IMAPS, REFUSAL_CLOSE_CODE, SUBPROTOCOL, TunnelAnswer, TunnelClose, TunnelOpen,
    TunnelRefusal,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::bridge_rate_limit::{Limiter, LimiterConfig};
use crate::routes::AppState;

/// Concurrent tunnels one actor may hold: an IMAP session, an authority
/// sign-in, a reconnect overlapping a close, one spare.
pub const MAX_TUNNELS_PER_ACTOR: usize = 4;

/// Concurrent tunnels the whole nest holds.
pub const MAX_TUNNELS_PER_NEST: usize = 64;

/// The per-actor open meter: 30 opens a minute sustained with a burst of 60,
/// as one sliding window of 60 opens over two minutes. Sized for the
/// device-code poll (RFC 8628's 5 s `interval`), so an actor scans neither
/// ports nor hosts at speed. Every open spends, refused or not.
pub const OPEN_LIMITER_CONFIG: LimiterConfig = LimiterConfig {
    window: Duration::from_secs(120),
    max_events: 60,
};

/// How long the TCP dial may take before the open is refused `dial_failed`.
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest `<tag> STARTTLS\r\n` line the gate reads before refusing.
const MAX_STARTTLS_LINE: usize = 80;

/// The open meter's bucket key (the limiter's credential axis).
const OPEN_BUCKET: &str = "mail_import_tunnel";

/// The tunnel's share of `AppState`: the open meter, the live-tunnel counts,
/// and — in a test build only — the loopback admission.
pub struct TunnelState {
    opens: Limiter,
    live: Arc<Mutex<LiveCounts>>,
    #[cfg(any(
        test,
        debug_assertions,
        feature = "test-hooks",
        feature = "test-helpers"
    ))]
    loopback: LoopbackAdmission,
}

#[derive(Default)]
struct LiveCounts {
    total: usize,
    per_actor: HashMap<[u8; 32], usize>,
}

impl Default for TunnelState {
    fn default() -> Self {
        Self::new()
    }
}

impl TunnelState {
    /// The production state; under `test-hooks` it also reads the launch
    /// environment's seeds ([`LoopbackAdmission::from_env`]).
    pub fn new() -> Self {
        Self {
            opens: Limiter::with_config(OPEN_LIMITER_CONFIG),
            live: Arc::default(),
            #[cfg(any(
                test,
                debug_assertions,
                feature = "test-hooks",
                feature = "test-helpers"
            ))]
            loopback: LoopbackAdmission::from_env(),
        }
    }

    /// A state admitting `loopback` — for a test that dials a fixture on
    /// loopback without the environment.
    #[cfg(any(
        test,
        debug_assertions,
        feature = "test-hooks",
        feature = "test-helpers"
    ))]
    pub fn with_loopback_admission(loopback: LoopbackAdmission) -> Self {
        Self {
            loopback,
            ..Self::new()
        }
    }

    /// Take a live-tunnel slot for `actor`, or `None` when the actor's or the
    /// nest's bound is full. The slot frees on drop.
    fn take_slot(&self, actor: [u8; 32]) -> Option<TunnelSlot> {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        let mine = live.per_actor.get(&actor).copied().unwrap_or(0);
        if mine >= MAX_TUNNELS_PER_ACTOR || live.total >= MAX_TUNNELS_PER_NEST {
            return None;
        }
        live.total += 1;
        live.per_actor.insert(actor, mine + 1);
        Some(TunnelSlot {
            live: Arc::clone(&self.live),
            actor,
        })
    }

    /// How many tunnels are live — nest-wide and for `actor`.
    pub fn live_counts(&self, actor: &[u8; 32]) -> (usize, usize) {
        let live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        (live.total, live.per_actor.get(actor).copied().unwrap_or(0))
    }
}

struct TunnelSlot {
    live: Arc<Mutex<LiveCounts>>,
    actor: [u8; 32],
}

impl Drop for TunnelSlot {
    fn drop(&mut self) {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        live.total = live.total.saturating_sub(1);
        if let Some(n) = live.per_actor.get_mut(&self.actor) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                live.per_actor.remove(&self.actor);
            }
        }
    }
}

/// The e2e harness's loopback destinations: hosts the preset-host redirect
/// seed maps to a loopback address, and the loopback ports its seeds name.
/// Compiled into test builds only; populated from the environment only under
/// `test-hooks`.
#[cfg(any(
    test,
    debug_assertions,
    feature = "test-hooks",
    feature = "test-helpers"
))]
#[derive(Debug, Clone, Default)]
pub struct LoopbackAdmission {
    /// `(host, loopback address)` — dial a mapped host there instead.
    pub host_map: Vec<(String, SocketAddr)>,
    /// Loopback ports a `localhost` or loopback-literal destination may name.
    pub ports: Vec<u16>,
}

/// The preset-host redirect seed — the same variable and grammar
/// `fauna_mail::imap_client::native` reads (a test pins the spelling).
#[cfg(any(
    test,
    debug_assertions,
    feature = "test-hooks",
    feature = "test-helpers"
))]
pub const E2E_HOST_MAP_ENV: &str = "FAUNA_E2E_IMAP_HOST_MAP";

/// The Microsoft authority seed — the same variable
/// `fauna_mail::imap_client::oauth_device` reads.
#[cfg(any(
    test,
    debug_assertions,
    feature = "test-hooks",
    feature = "test-helpers"
))]
pub const E2E_AUTHORITY_ENV: &str = "FAUNA_E2E_MICROSOFT_AUTHORITY";

#[cfg(any(
    test,
    debug_assertions,
    feature = "test-hooks",
    feature = "test-helpers"
))]
impl LoopbackAdmission {
    /// Read the seeds — under `test-hooks` only; any other build admits
    /// nothing, whatever its environment holds.
    pub fn from_env() -> Self {
        #[cfg(feature = "test-hooks")]
        {
            Self::from_values(
                std::env::var(E2E_HOST_MAP_ENV).ok().as_deref(),
                std::env::var(E2E_AUTHORITY_ENV).ok().as_deref(),
            )
        }
        #[cfg(not(feature = "test-hooks"))]
        {
            Self::default()
        }
    }

    /// Build from the two seeds' values. A host-map entry that is malformed
    /// or not loopback panics, as the app-side reader does: only the harness
    /// sets these, so a bad value is a harness bug.
    pub fn from_values(host_map: Option<&str>, authority: Option<&str>) -> Self {
        let mut admission = Self::default();
        for entry in host_map
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|e| !e.is_empty())
        {
            let parsed = entry.split_once('=').and_then(|(host, addr)| {
                let addr = addr.trim().parse::<SocketAddr>().ok()?;
                let host = host.trim();
                (!host.is_empty()).then(|| (host.to_string(), addr))
            });
            let Some((host, addr)) = parsed else {
                panic!("{E2E_HOST_MAP_ENV} entry {entry:?} is not `<host>=<ip>:<port>`");
            };
            assert!(
                addr.ip().is_loopback(),
                "{E2E_HOST_MAP_ENV} maps {host} to {addr}, which is not loopback"
            );
            admission.ports.push(addr.port());
            admission.host_map.push((host, addr));
        }
        if let Some(authority) = authority.map(str::trim).filter(|a| !a.is_empty()) {
            let url = url::Url::parse(authority)
                .unwrap_or_else(|e| panic!("{E2E_AUTHORITY_ENV} is not a URL: {e}"));
            if let Some(port) = url.port_or_known_default() {
                admission.ports.push(port);
            }
        }
        admission
    }

    /// Where the seam sends `open`, if it admits it.
    fn dial_for(&self, open: &TunnelOpen) -> Option<SocketAddr> {
        let host = normalize_host(&open.host);
        if let Some((_, addr)) = self
            .host_map
            .iter()
            .find(|(mapped, _)| mapped.eq_ignore_ascii_case(&host))
        {
            return Some(*addr);
        }
        if !self.ports.contains(&open.port) {
            return None;
        }
        let ip = if host == "localhost" {
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        } else {
            host.parse::<std::net::IpAddr>().ok()?
        };
        ip.is_loopback().then(|| SocketAddr::new(ip, open.port))
    }
}

/// The host as the policy compares it: trimmed, unbracketed, without a
/// trailing dot, lower-case.
fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

/// The deterministic half of the destination policy — everything decidable
/// from the open alone, before any DNS: the port and its TLS mode, the
/// Microsoft-only rule on 443, and a host that is a plausible DNS name or an
/// IP literal. Address classification is the resolver's
/// ([`crate::ssrf::resolve_global_addrs`]).
pub fn check_open(open: &TunnelOpen) -> Result<String, TunnelRefusal> {
    let host = normalize_host(&open.host);
    let literal = host.parse::<std::net::IpAddr>().is_ok();
    let dns_name = !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    if !literal && !dns_name {
        return Err(TunnelRefusal::Destination);
    }
    match (open.port, open.starttls) {
        (PORT_IMAPS, false) | (PORT_IMAP_STARTTLS, true) => Ok(host),
        (PORT_HTTPS, false) if host == MICROSOFT_AUTHORITY_HOST => Ok(host),
        (PORT_HTTPS, false) => Err(TunnelRefusal::Destination),
        _ => Err(TunnelRefusal::Port),
    }
}

/// The TLS-shape check on the client's first bytes, sans-io: [`Self::feed`]
/// takes each client frame and answers the bytes that may be forwarded now —
/// nothing until the shape is proven, then everything.
#[derive(Debug)]
pub struct ShapeGate {
    phase: GatePhase,
    held: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatePhase {
    /// 143 only: one `<tag> STARTTLS\r\n` line may come before the record.
    LineOrRecord,
    /// The first five bytes of a TLS handshake record, then a ClientHello.
    Record,
    /// Proven: bytes pass through.
    Open,
}

/// The client's bytes were not TLS-shaped.
#[derive(Debug, PartialEq, Eq)]
pub struct NotTls;

impl ShapeGate {
    pub fn new(starttls: bool) -> Self {
        Self {
            phase: if starttls {
                GatePhase::LineOrRecord
            } else {
                GatePhase::Record
            },
            held: Vec::new(),
        }
    }

    /// Feed one frame's bytes; answer what may be forwarded now.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<u8>, NotTls> {
        if self.phase == GatePhase::Open {
            return Ok(bytes.to_vec());
        }
        self.held.extend_from_slice(bytes);
        let mut out = Vec::new();
        if self.phase == GatePhase::LineOrRecord {
            if self.held.first() == Some(&0x16) {
                self.phase = GatePhase::Record;
            } else {
                let Some(end) = self.held.windows(2).position(|w| w == b"\r\n") else {
                    return if self.held.len() >= MAX_STARTTLS_LINE {
                        Err(NotTls)
                    } else {
                        Ok(out)
                    };
                };
                if end + 2 > MAX_STARTTLS_LINE || !is_starttls_line(&self.held[..end]) {
                    return Err(NotTls);
                }
                out.extend(self.held.drain(..end + 2));
                self.phase = GatePhase::Record;
            }
        }
        // A TLS record header: content type 22 (handshake), major version 3,
        // a minor version, a two-byte length; then handshake type 1.
        let h = &self.held;
        let bad = h.first().is_some_and(|&b| b != 0x16)
            || h.get(1).is_some_and(|&b| b != 0x03)
            || h.get(5).is_some_and(|&b| b != 0x01);
        if bad {
            return Err(NotTls);
        }
        if h.len() >= 6 {
            out.append(&mut self.held);
            self.phase = GatePhase::Open;
        }
        Ok(out)
    }
}

/// `<tag> STARTTLS`, the command case-insensitive, the tag an IMAP atom.
fn is_starttls_line(line: &[u8]) -> bool {
    let Some(space) = line.iter().position(|&b| b == b' ') else {
        return false;
    };
    let (tag, rest) = (&line[..space], &line[space + 1..]);
    !tag.is_empty()
        && tag
            .iter()
            .all(|&b| b.is_ascii_graphic() && !b"(){%*\"\\]+".contains(&b))
        && rest.eq_ignore_ascii_case(b"STARTTLS")
}

/// `GET /api/v1/mail/import/tunnel` — the upgrade.
pub async fn tunnel_handler(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let Some((subprotocol, token)) = crate::routes::parse_subprotocol(&headers) else {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            "subprotocol header required",
        )
            .into_response();
    };
    if subprotocol != SUBPROTOCOL {
        return (
            axum::http::StatusCode::UPGRADE_REQUIRED,
            format!("subprotocol {SUBPROTOCOL} required"),
        )
            .into_response();
    }
    let session = match crate::auth::check_bearer_session(&state, &token).await {
        Ok(session) => session,
        Err(_) => {
            return (axum::http::StatusCode::UNAUTHORIZED, "invalid token").into_response();
        }
    };
    // The frame bound is enforced in `serve` so an oversize frame gets its
    // typed 1009; the transport limit sits above it.
    ws.max_message_size(2 * MAX_FRAME_BYTES)
        .max_frame_size(2 * MAX_FRAME_BYTES)
        .protocols([SUBPROTOCOL])
        // The whole connection future is generation-scoped: axum's
        // per-connection task is detached, so teardown's `server_handle.abort()`
        // never reaches an established tunnel (same reason as
        // `federation_channel`'s upgrade).
        .on_upgrade(move |socket| {
            let scope = Arc::clone(&state);
            async move {
                let _ = scope.spawn_scoped(serve(state, session, socket)).await;
            }
        })
}

async fn close(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

async fn refuse(socket: &mut WebSocket, reason: TunnelRefusal) {
    if let Ok(bytes) = encode_canonical(&TunnelAnswer::Refused { reason }) {
        let _ = socket.send(Message::Binary(bytes)).await;
    }
    close(socket, REFUSAL_CLOSE_CODE, reason.as_str()).await;
}

async fn closed(socket: &mut WebSocket, why: TunnelClose) {
    close(socket, why.code(), why.as_str()).await;
}

/// How a tunnel ended, and what (if anything) the client is told.
enum Ending {
    Refused(TunnelRefusal),
    Closed(TunnelClose),
}

/// Register the tunnel in its actor's teardown, then run it until it ends or
/// a revocation selects it — whichever is first.
async fn serve(
    state: Arc<AppState>,
    session: crate::token_store::ValidatedSession,
    mut socket: WebSocket,
) {
    let actor = session.actor_id.0;
    // Test-only rendezvous inside the upgrade window (validated, not yet
    // registered), the RPC upgrade's barrier (`routes::upgrade_race`), so a
    // test can run a whole revoke here and pin the re-read below.
    // `#[cfg(test)]` alone: no artifact and no integration test reaches it.
    #[cfg(test)]
    if let Some(barrier) = crate::routes::upgrade_race::take(&session.token_id) {
        barrier.validated.notify_one();
        barrier.may_register.notified().await;
    }
    let lease = state.ws.register_tunnel(
        actor,
        session.token_id.clone(),
        crate::ws::bound_device_key_for(&actor, session.minted_by_device),
    );
    // The upgrade window: the bearer was validated before the 101, and a
    // revoke that ran its sweep between then and the registration above found
    // nothing to close. Its row delete precedes its sweep, so one re-read
    // after joining the registry is total — the RPC upgrade's rule
    // (`AppState::register_upgraded_connection`).
    if !state
        .auth
        .token_store
        .has_session(&session.actor_id, &session.token_id)
        .await
    {
        lease.revoke();
    }
    let ending = tokio::select! {
        biased;
        () = lease.revoked() => Some(Ending::Closed(TunnelClose::Revoked)),
        ending = run(&state, actor, &mut socket) => ending,
    };
    // The destination stream and the live slot were dropped with `run`.
    match ending {
        Some(Ending::Refused(reason)) => refuse(&mut socket, reason).await,
        Some(Ending::Closed(why)) => closed(&mut socket, why).await,
        None => {}
    }
}

/// Read the open, admit it, dial, then relay until either side ends.
async fn run(state: &AppState, actor: [u8; 32], socket: &mut WebSocket) -> Option<Ending> {
    let idle = Duration::from_secs(IDLE_SECS);
    let open = match tokio::time::timeout(idle, socket.recv()).await {
        Ok(Some(Ok(Message::Binary(bytes)))) if bytes.len() <= MAX_FRAME_BYTES => {
            match decode_strict::<TunnelOpen>(&bytes) {
                Ok(open) => open,
                Err(_) => return Some(Ending::Closed(TunnelClose::BadFrame)),
            }
        }
        Ok(Some(Ok(Message::Binary(_)))) => {
            return Some(Ending::Closed(TunnelClose::FrameTooLarge));
        }
        Ok(Some(Ok(Message::Close(_)) | Err(_)) | None) => return None,
        Ok(Some(Ok(_))) => return Some(Ending::Closed(TunnelClose::BadFrame)),
        Err(_) => return Some(Ending::Closed(TunnelClose::Idle)),
    };
    let tunnel = &state.mail_import_tunnel;
    if !tunnel.opens.check(&actor, &actor, OPEN_BUCKET) {
        return Some(Ending::Refused(TunnelRefusal::RateLimited));
    }
    let Some(_slot) = tunnel.take_slot(actor) else {
        return Some(Ending::Refused(TunnelRefusal::Busy));
    };
    let addrs = match admit(tunnel, &open).await {
        Ok(addrs) => addrs,
        Err(reason) => return Some(Ending::Refused(reason)),
    };
    let tcp = match tokio::time::timeout(DIAL_TIMEOUT, tokio::net::TcpStream::connect(&addrs[..]))
        .await
    {
        Ok(Ok(tcp)) => tcp,
        _ => return Some(Ending::Refused(TunnelRefusal::DialFailed)),
    };
    let Ok(opened) = encode_canonical(&TunnelAnswer::Opened {}) else {
        return Some(Ending::Closed(TunnelClose::Fault));
    };
    if socket.send(Message::Binary(opened)).await.is_err() {
        return None;
    }
    relay(socket, tcp, ShapeGate::new(open.starttls), idle)
        .await
        .map(Ending::Closed)
}

/// The destination's pinned addresses, or the refusal.
async fn admit(tunnel: &TunnelState, open: &TunnelOpen) -> Result<Vec<SocketAddr>, TunnelRefusal> {
    #[cfg(any(
        test,
        debug_assertions,
        feature = "test-hooks",
        feature = "test-helpers"
    ))]
    if let Some(addr) = tunnel.loopback.dial_for(open) {
        return Ok(vec![addr]);
    }
    #[cfg(not(any(
        test,
        debug_assertions,
        feature = "test-hooks",
        feature = "test-helpers"
    )))]
    let _ = tunnel;
    let host = check_open(open)?;
    crate::ssrf::resolve_global_addrs(&host, open.port)
        .await
        .map_err(|e| match e {
            crate::ssrf::SsrfError::Resolve => TunnelRefusal::Resolve,
            _ => TunnelRefusal::Destination,
        })
}

/// Pump bytes both ways. Answers the close to send, or `None` when the client
/// went away and there is no one to tell.
async fn relay(
    socket: &mut WebSocket,
    tcp: tokio::net::TcpStream,
    mut gate: ShapeGate,
    idle: Duration,
) -> Option<TunnelClose> {
    let (mut from_dest, mut to_dest) = tcp.into_split();
    let mut buf = vec![0u8; 16 * 1024];
    let deadline = tokio::time::sleep(idle);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => return Some(TunnelClose::Idle),
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(bytes))) => {
                    if bytes.len() > MAX_FRAME_BYTES {
                        return Some(TunnelClose::FrameTooLarge);
                    }
                    deadline.as_mut().reset(tokio::time::Instant::now() + idle);
                    let Ok(out) = gate.feed(&bytes) else {
                        return Some(TunnelClose::NotTls);
                    };
                    if !out.is_empty() && to_dest.write_all(&out).await.is_err() {
                        return Some(TunnelClose::Fault);
                    }
                }
                Some(Ok(Message::Text(_))) => return Some(TunnelClose::BadFrame),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_)) | Err(_)) | None => {
                    let _ = to_dest.shutdown().await;
                    return None;
                }
            },
            n = from_dest.read(&mut buf) => match n {
                Ok(0) => return Some(TunnelClose::Eof),
                Ok(n) => {
                    deadline.as_mut().reset(tokio::time::Instant::now() + idle);
                    if socket
                        .send(Message::Binary(bytes::Bytes::copy_from_slice(&buf[..n])))
                        .await
                        .is_err()
                    {
                        return None;
                    }
                }
                Err(_) => return Some(TunnelClose::Fault),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(host: &str, port: u16, starttls: bool) -> TunnelOpen {
        TunnelOpen {
            host: host.into(),
            port,
            starttls,
            ..Default::default()
        }
    }

    #[test]
    fn the_policy_admits_imap_to_any_host_and_https_to_the_authority_alone() {
        assert_eq!(
            check_open(&open("imap.Example.org.", 993, false)),
            Ok("imap.example.org".into())
        );
        assert!(check_open(&open("imap.example.org", 143, true)).is_ok());
        assert!(check_open(&open("login.microsoftonline.com", 443, false)).is_ok());
        assert!(check_open(&open("203.0.113.7", 993, false)).is_ok());
        assert_eq!(
            check_open(&open("example.org", 443, false)),
            Err(TunnelRefusal::Destination)
        );
        for (port, starttls) in [
            (25, false),
            (80, false),
            (993, true),
            (143, false),
            (443, true),
            (587, true),
        ] {
            assert_eq!(
                check_open(&open("imap.example.org", port, starttls)),
                Err(TunnelRefusal::Port),
                "{port}/{starttls}"
            );
        }
        for host in ["", "a b", "http://x", "x..y", "x/y"] {
            assert_eq!(
                check_open(&open(host, 993, false)),
                Err(TunnelRefusal::Destination),
                "{host:?}"
            );
        }
    }

    const HELLO: &[u8] = &[0x16, 0x03, 0x01, 0x00, 0x40, 0x01, 0xaa, 0xbb];

    #[test]
    fn the_gate_passes_a_client_hello_whole_and_then_everything() {
        let mut g = ShapeGate::new(false);
        assert_eq!(g.feed(&HELLO[..3]).unwrap(), b"");
        assert_eq!(g.feed(&HELLO[3..]).unwrap(), HELLO[..].to_vec());
        assert_eq!(g.feed(b"anything").unwrap(), b"anything".to_vec());
    }

    #[test]
    fn the_gate_refuses_plaintext_and_a_non_hello_handshake() {
        assert_eq!(
            ShapeGate::new(false).feed(b"GET / HTTP/1.1\r\n"),
            Err(NotTls)
        );
        assert_eq!(ShapeGate::new(false).feed(b"a1 LOGIN u p\r\n"), Err(NotTls));
        assert_eq!(
            ShapeGate::new(false).feed(&[0x16, 0x03, 0x01, 0x00, 0x40, 0x02]),
            Err(NotTls)
        );
        assert_eq!(ShapeGate::new(false).feed(&[0x17, 0x03]), Err(NotTls));
    }

    #[test]
    fn on_143_one_starttls_line_may_come_first_and_nothing_else() {
        let mut g = ShapeGate::new(true);
        assert_eq!(g.feed(b"a1 star").unwrap(), b"");
        assert_eq!(g.feed(b"ttls\r\n").unwrap(), b"a1 starttls\r\n".to_vec());
        assert_eq!(g.feed(HELLO).unwrap(), HELLO.to_vec());

        let mut g = ShapeGate::new(true);
        assert_eq!(g.feed(HELLO).unwrap(), HELLO.to_vec());

        assert_eq!(ShapeGate::new(true).feed(b"a1 LOGIN u p\r\n"), Err(NotTls));
        assert_eq!(ShapeGate::new(true).feed(b"STARTTLS\r\n"), Err(NotTls));
        let mut g = ShapeGate::new(true);
        g.feed(b"a1 STARTTLS\r\n").unwrap();
        assert_eq!(g.feed(b"a2 STARTTLS\r\n"), Err(NotTls));
        assert_eq!(
            ShapeGate::new(true).feed(&[b'x'; MAX_STARTTLS_LINE]),
            Err(NotTls)
        );
    }

    #[test]
    fn the_seam_admits_the_seeded_loopback_ports_and_mapped_hosts_only() {
        let a = LoopbackAdmission::from_values(
            Some("outlook.office365.com=127.0.0.1:40993"),
            Some("https://127.0.0.1:40443"),
        );
        assert_eq!(
            a.dial_for(&open("Outlook.Office365.com", 993, false)),
            Some("127.0.0.1:40993".parse().unwrap())
        );
        assert_eq!(
            a.dial_for(&open("localhost", 40993, false)),
            Some("127.0.0.1:40993".parse().unwrap())
        );
        assert!(a.dial_for(&open("127.0.0.1", 40443, false)).is_some());
        assert_eq!(a.dial_for(&open("127.0.0.1", 22, false)), None);
        assert_eq!(a.dial_for(&open("10.0.0.1", 40993, false)), None);
        assert_eq!(a.dial_for(&open("imap.example.org", 993, false)), None);
        assert!(
            LoopbackAdmission::from_values(None, None)
                .dial_for(&open("localhost", 40993, false))
                .is_none()
        );
    }

    #[test]
    #[should_panic(expected = "not loopback")]
    fn a_host_map_off_the_box_is_a_harness_bug() {
        LoopbackAdmission::from_values(Some("x=192.0.2.1:993"), None);
    }

    #[test]
    fn the_seed_variables_are_spelled_as_the_app_reads_them() {
        assert_eq!(
            E2E_HOST_MAP_ENV,
            fauna_mail::imap_client::native::E2E_HOST_MAP_ENV
        );
        assert_eq!(
            E2E_AUTHORITY_ENV,
            fauna_mail::imap_client::oauth_device::E2E_AUTHORITY_ENV
        );
    }

    #[test]
    fn the_slots_bound_per_actor_and_free_on_drop() {
        let s = TunnelState::new();
        let a = [1u8; 32];
        let held: Vec<_> = (0..MAX_TUNNELS_PER_ACTOR)
            .map(|_| s.take_slot(a).unwrap())
            .collect();
        assert!(s.take_slot(a).is_none());
        assert!(s.take_slot([2u8; 32]).is_some());
        drop(held);
        assert_eq!(s.live_counts(&a), (0, 0));
    }

    #[test]
    fn the_nest_bound_holds_across_actors() {
        let s = TunnelState::new();
        let held: Vec<_> = (0..MAX_TUNNELS_PER_NEST)
            .map(|i| {
                s.take_slot([(i / MAX_TUNNELS_PER_ACTOR) as u8; 32])
                    .unwrap()
            })
            .collect();
        assert!(s.take_slot([255u8; 32]).is_none());
        drop(held);
    }

    /// The upgrade window: a session
    /// revoked after the tunnel's bearer was validated and before the tunnel
    /// joined the teardown registry is swept while there is nothing to close.
    /// `serve`'s one `has_session` re-read after `register_tunnel` is what
    /// closes it (`transport-connection.md` § Revocation teardown) — without
    /// it the tunnel waits for its open frame and would relay on a revoked
    /// bearer. The upgrade is parked in the window (`routes::upgrade_race`),
    /// the whole revoke runs there, then the registration goes on.
    #[tokio::test]
    async fn a_session_revoked_inside_the_upgrade_window_closes_the_tunnel() {
        use fauna_protocol::mail_import_tunnel::{ROUTE, SUBPROTOCOL};
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState {
            auth: crate::state::AuthState {
                token_store: Arc::new(crate::token_store::TokenStore::new()),
                ..Default::default()
            },
            ..AppState::for_test(db)
        });
        let kp = fauna_core::identity::ActorKeypair::generate();
        let actor = kp.actor_id().0;
        state
            .db
            .create_user_with_handle(&actor, "personal", "importer", None)
            .await
            .unwrap();
        let token = state.auth.token_store.insert(kp.actor_id(), 3600).await;
        let token_id = state
            .auth
            .token_store
            .validate_with_session(&token)
            .await
            .unwrap()
            .token_id;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = crate::build_router(Arc::clone(&state));
        // spawn-ok(test): the server under test, dropped with the runtime.
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let barrier = crate::routes::upgrade_race::arm(&token_id);
        let mut req = format!("ws://{addr}{ROUTE}").into_client_request().unwrap();
        req.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            format!("{SUBPROTOCOL}, bearer.{token}").parse().unwrap(),
        );
        let (mut ws, _) = tokio_tungstenite::connect_async(req)
            .await
            .expect("a valid bearer upgrades");
        barrier.validated.notified().await;

        state.revoke_session_authority(&actor, &token_id).await;
        assert_eq!(
            state.ws.tunnel_count(&actor),
            0,
            "precondition: the revoke swept before the tunnel registered"
        );
        barrier.may_register.notify_one();

        let close = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(msg) = ws.next().await {
                if let Message::Close(Some(frame)) = msg.unwrap() {
                    return (u16::from(frame.code), frame.reason.to_string());
                }
            }
            panic!("the tunnel ended without a close frame");
        })
        .await
        .expect(
            "a tunnel registered after its session's revoke stayed open — \
             nothing re-read the session after `register_tunnel`, so it would \
             relay on a revoked bearer until its idle timeout",
        );
        assert_eq!(close, (4401, "revoked".into()));
    }
}
