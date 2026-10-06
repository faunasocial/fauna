//! Self-hosted `iroh-relay` server — the nest's own P2P relay sidecar.
//!
//! This is the **server** half of the nest-side relay capability: a
//! self-hosted `iroh-relay` that forwards **E2E-encrypted QUIC packets it cannot
//! decrypt** between peers behind NAT — **no n0 third party** (relay sovereignty,
//! empirically de-risked via an internal PQ-transport prototype evaluation). It is the
//! counterpart to `fauna_iroh::IrohTransport`'s client-side relay wiring
//! (`RelayMode::Custom` + the `CaTlsConfig` trust hook): a nest that runs this
//! sidecar advertises the `relay` capability, and clients route through it when
//! no direct path establishes.
//!
//! **Cert sourcing — a channel sidecar, never `/data/acme` (`security.md` § UID
//! isolation).** The relay terminates TLS for `relay.<apex>`, but it runs under
//! its own UID with **zero `/data` access**: it obtains its cert as an
//! HPKE-sealed blob it opens with its own X25519, exactly how the mail bridge
//! gets `mail.<apex>`. It is a co-located sidecar — it dials nest's
//! loopback `/internal/relay/ws`, runs the `fauna.sidecar.hello` token handshake
//! **attesting its X25519 public key**, then originates `fauna.relay.fetch_tls_cert`
//! (`fauna_protocol::relay`). The nest seals the on-disk `relay.<apex>` cert to
//! that attested X25519; the relay opens it (`fauna_mls::wrapped_blob`) and serves
//! it. A compromised relay therefore cannot read the nest's TLS key material at
//! rest. The cert is hot-swapped the moment the nest says it changed
//! (`fauna.relay.cert_changed`), on every reconnect, and on a 12h timer and SIGHUP
//! as backstops, so a renewal never drops a live relay connection.
//!
//! **When it serves — only for a nest with a public name of its own**
//! (`behavior/p2p.md` § The relay). The image always starts this binary; the
//! nest decides whether it serves, by handing it a certificate or not. Until the
//! nest has a public name the relay *stands by*: it holds its channel, listens
//! (on loopback, and on its address-discovery port) with no certificate to
//! present, and serves nobody. The claim that
//! gives the nest its name reaches it as the same `fauna.relay.cert_changed`
//! push, and it starts serving with no restart and nothing switched on.
//!
//! **Who may use it — the nest's own members' devices, and nobody else**
//! (`behavior/p2p.md` § The relay). The relay holds its channel to the nest for
//! its whole life and asks it about every connecting endpoint
//! (`fauna.relay.admit`); only a key the nest knows is served. It fails closed:
//! with the channel down, or on any error, the endpoint is refused. The relay
//! itself holds no list and learns nothing about an account from the answer.
//!
//! The relay-serving code lives behind the non-default `relay` feature (this
//! whole module is `#![cfg(feature = "relay")]`), mirroring `fauna-iroh`'s `quic`
//! feature: the default workspace build never compiles iroh-relay's tree. The
//! released nest image builds this binary with the feature; building it and
//! running it are **artifact** decisions (`behavior/p2p.md` § The relay),
//! **never a human config knob** (the no-operator invariant). Its bind addresses,
//! the nest loopback URL, and the key dir are **artifact-set IPC** (CLI/env the
//! run-script provides); the sidecar token rides `FAUNA_SIDECAR_TOKEN`.
//!
//! The lean WS-RPC dial uses the shared [`fauna_sidecar_client`] crate (connect +
//! the `fauna.sidecar.hello` token handshake) — the ONE audited copy of the sidecar
//! dialer — plus
//! `fauna-protocol` (the `fauna.relay.*` wire types) and `fauna-mls` (unseal). It
//! does **not** depend on `fauna-nest`, so the binary stays small.
#![cfg(feature = "relay")]

/// This binary's compile-time event catalogue for the sidecar log plane — the
/// complete set of events it may report to nest's admin Logs surface.
mod log_catalogue;

use std::collections::BTreeMap;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use arc_swap::ArcSwapOption;
use clap::Parser;
use iroh_relay::server::{
    Access, CertConfig, ClientRequest, ConnectionId, DynAccessControl, QuicConfig, RelayConfig,
    Server, ServerConfig, TlsConfig,
};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;

use fauna_mls::wrapped_blob::{TlsCertBlob, generate_x25519_keypair, unseal_tls_cert};
use fauna_protocol::relay::{
    HELLO_EXTRA_X25519, KIND_RELAY_ADMIT, KIND_RELAY_CERT_CHANGED, KIND_RELAY_FETCH_TLS_CERT,
    RelayAdmitReply, RelayAdmitRequest, RelayCertChangedReply, RelayFetchTlsCertReply,
    RelayFetchTlsCertRequest,
};
use fauna_protocol::{Reply, RpcDispatcher, RpcError, Value};
use fauna_sidecar_client::{CredentialRefused, SidecarDialError};

/// Handshake-pubkey cache capacity for the relay. A fixed Rust constant (not a
/// human knob): the relay-protocol key cache trades a little memory for fewer
/// repeated key parses; 1024 matches iroh's own test/default sizing and the
/// relay-sovereignty prototype.
const KEY_CACHE_CAPACITY: usize = 1024;

/// The backstop cadence at which the relay re-fetches its TLS cert from nest and
/// hot-swaps it. The prompt path is nest's `fauna.relay.cert_changed` push and the
/// re-fetch on every reconnect; this timer (the mail bridge's `tls.go` cadence)
/// only covers a push that was lost. SIGHUP forces an immediate refresh.
const CERT_REFRESH_INTERVAL: Duration = Duration::from_secs(12 * 3600);

/// How often a relay that is standing by (the nest has handed it no certificate
/// yet) asks again over its open channel. The prompt path is the nest's push at
/// claim; this only covers a push that was lost.
const STANDBY_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Filename of the relay's persisted X25519 keypair under the key dir (own UID,
/// 0700 dir / 0600 file). Stored as 64 hex-encoded bytes = `secret ‖ public`.
const RELAY_KEY_FILENAME: &str = "relay.key";

/// The sidecar-hello scope string this binary declares — must equal
/// `fauna_nest::sidecar_tokens::SidecarScope::Relay::as_str()` (the nest verifies
/// it against the token map). Kept as a literal here (the binary does not depend
/// on `fauna-nest`); the wire agreement is covered by the tier_4 test.
const RELAY_SCOPE: &str = "relay";

/// How long the relay waits to enqueue a reply to nest before treating the
/// channel as dead (the value every serving loop in the tree uses,
/// `fauna_peer_channel::REPLY_ENQUEUE_BUDGET`; restated because this binary does
/// not depend on that crate).
const REPLY_ENQUEUE_BUDGET: Duration = Duration::from_secs(30);

/// The nest internal listener path the relay sidecar dials (the relay's
/// `fauna.sidecar.hello` channel). Passed to [`fauna_sidecar_client::dial_sidecar`].
const RELAY_WS_ROUTE: &str = "/internal/relay/ws";

/// Artifact-set IPC config for the relay binary. Every field is wiring the
/// deployment artifact provides (CLI/env), never a value a human chooses.
#[derive(Parser, Debug)]
#[command(
    name = "fauna-iroh-relay",
    about = "Self-hosted iroh-relay sidecar — the nest's own P2P relay (no n0 third party)"
)]
struct Args {
    /// HTTPS bind for the relay protocol — the address clients dial (SNI-routed at
    /// `relay.<domain>`). Artifact-set IPC. Required: there is no operator and no
    /// sensible default to guess.
    #[arg(long, env = "FAUNA_RELAY_HTTPS_BIND")]
    https_bind: SocketAddr,
    /// HTTP bind for the relay's plaintext HTTP services (incl. the captive-portal
    /// probe, which must run without TLS). Artifact-set IPC; the run-script keeps
    /// this **loopback-only** (it is not SNI-routed) so it is not a public
    /// exposure. Required.
    #[arg(long, env = "FAUNA_RELAY_HTTP_BIND")]
    http_bind: SocketAddr,
    /// nest's HTTP base (e.g. `http://127.0.0.1:3000`); the dialer derives the
    /// `ws(s)://…/internal/relay/ws` channel URL from it. Artifact-set IPC.
    #[arg(
        long,
        env = "FAUNA_RELAY_NEST_URL",
        default_value = "http://127.0.0.1:3000"
    )]
    nest_url: String,
    /// Directory holding the relay's own X25519 keypair (the seal recipient).
    /// Artifact-set IPC; the run-script points it at a relay-owned 0700 subdir.
    #[arg(long, env = "FAUNA_RELAY_KEY_DIR", default_value = "/data/keys/relay")]
    key_dir: PathBuf,
}

/// Resolved bind addresses for [`spawn_relay`]. Kept separate from the CLI so the
/// serving path is testable without going through `clap`.
pub struct RelayServerOptions {
    /// HTTPS bind — the relay protocol over TLS (what clients dial).
    pub https_bind: SocketAddr,
    /// HTTP bind — plaintext HTTP services (captive-portal probe, etc.).
    pub http_bind: SocketAddr,
    /// Handshake-pubkey cache capacity (`None` leaves it unset).
    pub key_cache_capacity: Option<usize>,
    /// UDP bind for the substrate's QUIC address-discovery server, served beside
    /// the relay protocol under the same certificate; `None` serves none. The
    /// production [`run`] binds [`DISCOVERY_PORT`] on every interface
    /// ([`production_options`]); a test relay may bind an ephemeral port or none.
    pub quic_bind: Option<SocketAddr>,
}

/// The UDP port the relay serves address discovery on — the substrate's default,
/// which every endpoint's one-URL relay map already queries, so the relay URL
/// stays the only fact a nest advertises (`behavior/p2p.md` § The relay →
/// *Address discovery*, ruling 2). A constant nobody chooses: the deployment
/// artifact publishes it and the internet-setup guide opens it.
pub const DISCOVERY_PORT: u16 = iroh_relay::defaults::DEFAULT_RELAY_QUIC_PORT;

/// The options [`run`] serves with: the relay protocol on the artifact-set
/// loopback binds (fronted by the SNI router), and address discovery on
/// [`DISCOVERY_PORT`] on every interface — the one port the relay process
/// itself exposes. Discovery takes its TLS from the relay's own hot-swap
/// resolver, so a relay standing by (no certificate yet) completes no discovery
/// handshake and tells no caller anything.
pub fn production_options(https_bind: SocketAddr, http_bind: SocketAddr) -> RelayServerOptions {
    RelayServerOptions {
        https_bind,
        http_bind,
        key_cache_capacity: Some(KEY_CACHE_CAPACITY),
        quic_bind: Some(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            DISCOVERY_PORT,
        )),
    }
}

// ── X25519 keypair (the seal recipient) ──────────────────────────────────────

/// The relay's X25519 keypair — the recipient the nest HPKE-seals the cert to.
#[derive(Clone)]
struct RelayKeypair {
    secret: [u8; 32],
    public: [u8; 32],
}

/// Load the relay's persisted X25519 keypair from `key_dir/relay.key`, or generate
/// a fresh one and persist it (load-if-exists-else-create-and-persist, like the
/// nest's `load_or_create_floor_key`). A stable key keeps the relay's seal
/// identity constant across restarts; a corrupt/malformed file is treated as
/// absent and regenerated (the nest just re-seals to the new key on next fetch).
fn load_or_create_relay_keypair(key_dir: &Path) -> Result<RelayKeypair> {
    let path = key_dir.join(RELAY_KEY_FILENAME);
    if let Ok(text) = std::fs::read_to_string(&path)
        && let Some(bytes) = fauna_core::format::hex_decode(text.trim())
        && bytes.len() == 64
    {
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&bytes[..32]);
        let mut public = [0u8; 32];
        public.copy_from_slice(&bytes[32..]);
        return Ok(RelayKeypair { secret, public });
    }

    let (secret, public) = generate_x25519_keypair();
    std::fs::create_dir_all(key_dir)
        .with_context(|| format!("create relay key dir {key_dir:?}"))?;
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(&secret);
    buf.extend_from_slice(&public);
    // Owner-only, crash-atomic write — the shared mint (`fauna_core::secret_file`,
    // lifted from the nest's `deployment_key.rs` for this exact site) unlinks any `.tmp` crash residue before an
    // exclusive create, so the mode binds on a genuinely fresh file rather than a
    // write-then-chmod on a path that could pre-exist at a wider mode.
    fauna_core::secret_file::write_secret_file_0600(
        &path,
        fauna_core::format::hex_full(&buf).as_bytes(),
    )
    .with_context(|| format!("write relay key {path:?}"))?;
    Ok(RelayKeypair { secret, public })
}

// ── The sealed cert fetch ────────────────────────────────────────────────────

/// Originate `fauna.relay.fetch_tls_cert` over the authenticated channel, unseal the
/// returned blob with our X25519 secret, and return the cert + key PEM.
/// `Ok(None)` ⇒ the nest handed us nothing: it has no public name of its own
/// (yet), so there is nothing for the relay to serve under — stand by.
async fn fetch_and_unseal(
    dispatcher: &Arc<RpcDispatcher>,
    keypair: &RelayKeypair,
) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
    let reply: RelayFetchTlsCertReply = fauna_sidecar_client::request_typed(
        dispatcher,
        KIND_RELAY_FETCH_TLS_CERT,
        &RelayFetchTlsCertRequest::default(),
    )
    .await
    .map_err(|e| anyhow!("relay cert fetch: {e}"))?;
    let Some(blob_bytes) = reply.blob else {
        return Ok(None);
    };

    // Unseal with our X25519 secret → the cert + key PEM.
    let blob = TlsCertBlob::from_canonical_bytes(blob_bytes.as_ref())
        .map_err(|e| anyhow!("decode sealed cert blob: {e}"))?;
    let bundle = unseal_tls_cert(&blob, &keypair.secret)
        .map_err(|e| anyhow!("unseal relay cert with our X25519: {e}"))?;
    Ok(Some((bundle.cert_chain.clone(), bundle.priv_key.clone())))
}

// ── Hot-swappable cert resolver ──────────────────────────────────────────────

/// A single-cert rustls resolver whose `CertifiedKey` can be atomically swapped at
/// runtime (the cert refresh) without restarting the relay — mirrors the nest's
/// `MultiDomainCertResolver` (`ArcSwapOption<CertifiedKey>`).
struct RelayCertResolver {
    current: ArcSwapOption<CertifiedKey>,
}

impl RelayCertResolver {
    /// A resolver with no cert: every TLS handshake fails until one is stored.
    /// The standing-by state — the relay listens but can present nothing.
    fn empty() -> Self {
        Self {
            current: ArcSwapOption::empty(),
        }
    }

    #[cfg(test)]
    fn new(certified: CertifiedKey) -> Self {
        Self {
            current: ArcSwapOption::from(Some(Arc::new(certified))),
        }
    }

    fn is_empty(&self) -> bool {
        self.current.load().is_none()
    }

    /// Atomically replace the served cert (in-flight handshakes keep their loaded
    /// `Arc`; new ones see the swap).
    fn store(&self, certified: CertifiedKey) {
        self.current.store(Some(Arc::new(certified)));
    }
}

impl std::fmt::Debug for RelayCertResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayCertResolver").finish()
    }
}

impl ResolvesServerCert for RelayCertResolver {
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.current.load_full()
    }
}

/// Parse an UNSEALED cert chain + private key PEM into a rustls [`CertifiedKey`]
/// (ring provider, matching the iroh tree). Mirrors the nest's
/// `acme::load_certified_key_from_pem`.
fn certified_key_from_pem(cert_pem: &[u8], key_pem: &[u8]) -> Result<CertifiedKey> {
    use rustls::pki_types::pem::{self, PemObject};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let certs = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| anyhow!("parse certificates from unsealed PEM: {e}"))?;
    anyhow::ensure!(!certs.is_empty(), "no certificates in unsealed PEM");
    let key_der = PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| match e {
        pem::Error::NoItemsFound => anyhow!("no private key in unsealed PEM"),
        e => anyhow!("parse private key from unsealed PEM: {e}"),
    })?;
    let signing_key = rustls::crypto::ring::sign::any_supported_type(&key_der)
        .map_err(|e| anyhow!("unsupported relay key type: {e}"))?;
    Ok(CertifiedKey::new(certs, signing_key))
}

/// Build the relay's HTTPS [`rustls::ServerConfig`] backed by `resolver` (ring
/// provider, no client auth). The resolver lets the cert hot-swap on refresh.
fn build_server_config(resolver: Arc<RelayCertResolver>) -> Result<rustls::ServerConfig> {
    Ok(rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .context("ring provider rejected the default protocol versions")?
    .with_no_client_auth()
    .with_cert_resolver(resolver))
}

// ── Admission: who may use the relay ─────────────────────────────────────────

/// The relay's admission rule: is this endpoint key one the relay serves?
///
/// A seam so the serving path has exactly one rule to apply and the tests can
/// state it directly. Production is [`NestLink`] — the nest answers, over the
/// sidecar channel; the relay holds no list of its own.
pub trait Admission: std::fmt::Debug + Send + Sync + 'static {
    /// `true` ⇒ serve this endpoint. Anything that is not a clear yes is `false`.
    fn admits<'a>(
        &'a self,
        endpoint_key: [u8; 32],
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;
}

/// Adapts an [`Admission`] to the relay server's access hook — the one place the
/// dependency's access types are named.
#[derive(Debug)]
struct AdmissionGate(Arc<dyn Admission>);

impl DynAccessControl for AdmissionGate {
    fn on_connect<'a>(
        &'a self,
        request: &'a ClientRequest,
    ) -> Pin<Box<dyn Future<Output = Access> + Send + 'a>> {
        Box::pin(async move {
            if self.0.admits(*request.endpoint_id().as_bytes()).await {
                Access::Allow
            } else {
                // Debug, not warn: an open port on the internet is probed
                // constantly, and a refused stranger is the rule working.
                tracing::debug!(endpoint = %request.endpoint_id(), "relay: endpoint refused");
                Access::Deny {
                    reason: Some("endpoint is not a member's device".to_string()),
                }
            }
        })
    }

    fn on_disconnect(&self, _endpoint: iroh_base::PublicKey, _connection: ConnectionId) {}
}

/// The relay's live channel to nest, when one is up — and, through it, the
/// production [`Admission`]: the nest is asked about every connecting endpoint.
///
/// **Fails closed.** No channel (nest restarting, the relay just started), a
/// request that errors or times out, a reply that is not `admitted: true` — each
/// is a refusal. The endpoint's own reconnect asks again.
#[derive(Default)]
pub struct NestLink {
    channel: ArcSwapOption<RpcDispatcher>,
}

impl std::fmt::Debug for NestLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NestLink")
            .field("connected", &self.channel.load().is_some())
            .finish()
    }
}

impl Admission for NestLink {
    fn admits<'a>(
        &'a self,
        endpoint_key: [u8; 32],
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            let Some(dispatcher) = self.channel.load_full() else {
                return false;
            };
            let request = RelayAdmitRequest {
                endpoint_key: fauna_core::hex32::encode(&endpoint_key),
                extra: Default::default(),
            };
            match fauna_sidecar_client::request_typed::<_, RelayAdmitReply>(
                &dispatcher,
                KIND_RELAY_ADMIT,
                &request,
            )
            .await
            {
                Ok(reply) => reply.admitted,
                Err(e) => {
                    tracing::warn!(error = %e, "relay: admission question to nest failed; refusing");
                    false
                }
            }
        })
    }
}

// ── Serving (the proven relay recipe) ────────────────────────────────────────

/// Spawn the self-hosted relay with the given bind addresses, HTTPS
/// [`rustls::ServerConfig`] and admission rule. Returns the running [`Server`];
/// drive it with [`Server::join`] and stop it with [`Server::shutdown`].
///
/// Lifts the proven relay-server recipe from the 2026-06-28 relay-sovereignty
/// prototype: a relay `ServerConfig` — the QUIC address-discovery server only
/// when `opts.quic_bind` names one — with TLS via [`CertConfig::Manual`] using the
/// given config as-is. `admission` is required —
/// there is no way to start this relay open to every endpoint.
#[allow(clippy::field_reassign_with_default)] // ServerConfig/RelayConfig are #[non_exhaustive] → can't struct-literal them
pub async fn spawn_relay(
    opts: RelayServerOptions,
    tls: rustls::ServerConfig,
    admission: Arc<dyn Admission>,
) -> Result<Server> {
    let tls_config = TlsConfig::new(opts.https_bind, CertConfig::Manual { server_config: tls });
    let mut relay = RelayConfig::new(opts.http_bind);
    relay.tls = Some(tls_config);
    relay.key_cache_capacity = opts.key_cache_capacity;
    relay.access = Arc::new(AdmissionGate(admission));

    let mut config = ServerConfig::default();
    config.relay = Some(relay);
    // With no `quic_bind`, `config.quic` stays `None`: only the relay protocol over
    // HTTPS is served. With one, the substrate's QUIC address-discovery server runs
    // beside it; its TLS (`server_config: None`) is taken from the relay's own.
    config.quic = opts.quic_bind.map(QuicConfig::new);

    Server::spawn(config)
        .await
        .context("Server::spawn (iroh-relay)")
}

// ── Entry point ──────────────────────────────────────────────────────────────

/// Entry point: load/mint the X25519 keypair, stand the relay up behind an empty
/// hot-swap resolver (standing by), then hold the channel to nest — which hands
/// it its cert, tells it when the cert changes and answers its admission
/// questions — until a shutdown signal (or the relay's own supervisor exits).
pub async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let token = std::env::var("FAUNA_SIDECAR_TOKEN")
        .map_err(|_| anyhow!("FAUNA_SIDECAR_TOKEN not set (the s6 run-script provides it)"))?;
    let keypair =
        load_or_create_relay_keypair(&args.key_dir).context("load/create relay X25519 keypair")?;

    let resolver = Arc::new(RelayCertResolver::empty());
    let config = build_server_config(resolver.clone())?;
    let opts = production_options(args.https_bind, args.http_bind);

    let link = Arc::new(NestLink::default());
    let mut server = spawn_relay(opts, config, link.clone()).await?;
    let https_port = server.https_addr().map(|a| a.port()).unwrap_or_default();
    tracing::info!(
        https_addr = ?server.https_addr(),
        http_addr = ?server.http_addr(),
        discovery_addr = ?server.quic_addr(),
        nest_url = %args.nest_url,
        "fauna-iroh-relay standing by — waiting for its cert from nest"
    );

    // The relay's long-lived channel to nest: admission questions go out over
    // it, the cert-changed push comes in over it, and it re-dials whenever it
    // drops. Until it is up the relay refuses every endpoint (fail closed).
    let mut refresh = tokio::spawn(nest_channel_loop(
        args.nest_url.clone(),
        token.clone(),
        keypair.clone(),
        resolver.clone(),
        link,
        https_port,
    ));

    // Race the relay's own supervisor against a shutdown signal — and against the
    // channel loop discovering nest has re-minted its tokens, which no longer
    // leaves us serving a cert we can never renew. The match runs *after* the
    // select drops the `join()` future, releasing its `&mut server` borrow, so the
    // shutdown arm can consume `server`.
    enum Outcome {
        Supervisor(Result<()>),
        Signal,
        CredentialRefused(CredentialRefused),
    }
    let outcome = tokio::select! {
        res = server.join() => Outcome::Supervisor(match res {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(anyhow::Error::from(e)),
            Err(e) => Err(anyhow::Error::from(e)),
        }),
        () = shutdown_signal() => Outcome::Signal,
        refused = &mut refresh => match refused {
            Ok(refused) => Outcome::CredentialRefused(refused),
            // The channel task can only end by returning; a join error is a panic.
            Err(e) => Outcome::Supervisor(Err(anyhow::Error::from(e))),
        },
    };
    refresh.abort();
    match outcome {
        Outcome::Supervisor(res) => {
            res.context("relay supervisor exited")?;
            tracing::info!("relay supervisor finished cleanly");
            Ok(())
        }
        Outcome::Signal => {
            tracing::info!("shutdown signal received — stopping relay");
            server.shutdown().await.context("relay shutdown")
        }
        Outcome::CredentialRefused(refused) => {
            // Stop serving cleanly first: the cert we hold is still valid, so
            // in-flight relay clients are dropped politely rather than at a socket
            // reset, and s6 brings us straight back with a fresh token.
            let _ = server.shutdown().await;
            Err(refused.into())
        }
    }
}

/// Hold the relay's channel to nest for the life of the process, re-dialing
/// whenever it drops (shared policy: [`fauna_sidecar_client::serve_while_reaching_nest`]).
///
/// Returns only when nest refuses our credential — it re-minted its sidecar tokens
/// while we were serving, so every future dial would fail the same way: the relay
/// could admit nobody and would keep serving a cert it can no longer renew. `run`
/// ends on it so the supervisor restarts us with the token file's current contents.
async fn nest_channel_loop(
    nest_url: String,
    token: String,
    keypair: RelayKeypair,
    resolver: Arc<RelayCertResolver>,
    link: Arc<NestLink>,
    https_port: u16,
) -> CredentialRefused {
    fauna_sidecar_client::serve_while_reaching_nest(
        || serve_nest_channel(&nest_url, &token, &keypair, &resolver, &link, https_port),
        |_e: &SidecarDialError, _delay| crate::log_catalogue::nest_unreachable(),
    )
    .await
}

/// One life of the channel: dial + handshake, publish it on `link` so admission
/// questions can go out, re-fetch the cert (a channel that was down may have
/// missed a push), then serve nest's `fauna.relay.cert_changed` push and the
/// backstop refreshes until the channel closes. `Ok(())` ⇒ it closed and the
/// caller re-dials; `Err` ⇒ the dial itself failed.
async fn serve_nest_channel(
    nest_url: &str,
    token: &str,
    keypair: &RelayKeypair,
    resolver: &RelayCertResolver,
    link: &NestLink,
    https_port: u16,
) -> Result<(), SidecarDialError> {
    let mut extra = BTreeMap::new();
    extra.insert(
        HELLO_EXTRA_X25519.to_string(),
        Value::String(fauna_core::hex32::encode(&keypair.public)),
    );
    let dial =
        fauna_sidecar_client::dial_sidecar(nest_url, RELAY_WS_ROUTE, token, RELAY_SCOPE, extra)
            .await?;
    let dispatcher = dial.dispatcher;
    let Some(mut inbound) = dispatcher.inbound_requests() else {
        dial.driver.abort();
        return Ok(());
    };
    link.channel.store(Some(dispatcher.clone()));
    tracing::info!("relay channel to nest established; admission is live");

    refresh_cert(&dispatcher, keypair, resolver, https_port).await;
    loop {
        fauna_sidecar_client::log_plane::flush(&dispatcher).await;
        // Standing by, ask again soon; serving, the timer is only a backstop.
        // Re-armed each turn of the loop — every other arm refreshes anyway.
        let next_refresh = if resolver.is_empty() {
            STANDBY_POLL_INTERVAL
        } else {
            CERT_REFRESH_INTERVAL
        };
        tokio::select! {
            request = inbound.recv() => {
                let Some(request) = request else { break };
                let (payload, ok) = if request.kind == KIND_RELAY_CERT_CHANGED {
                    (fauna_protocol::encode_payload(&RelayCertChangedReply::default()), true)
                } else {
                    tracing::debug!(kind = %request.kind, "relay: kind not served on this channel");
                    (
                        fauna_protocol::encode_payload(&RpcError::new(
                            "fauna.protocol.unauthenticated",
                            "error.protocol.unauthenticated",
                        )),
                        false,
                    )
                };
                let Ok(payload) = payload else { continue };
                let reply = Reply {
                    ty: Reply::TYPE,
                    correlation_id: request.correlation_id,
                    payload,
                    ok,
                };
                if dispatcher
                    .send_reply_bounded(reply, tokio::time::sleep(REPLY_ENQUEUE_BUDGET))
                    .await
                    .is_err()
                {
                    break;
                }
                if ok {
                    tracing::info!("nest reports a changed cert — refreshing now");
                    refresh_cert(&dispatcher, keypair, resolver, https_port).await;
                }
            }
            () = tokio::time::sleep(next_refresh) => {
                refresh_cert(&dispatcher, keypair, resolver, https_port).await;
            }
            () = sighup() => {
                tracing::info!("SIGHUP — refreshing relay cert now");
                refresh_cert(&dispatcher, keypair, resolver, https_port).await;
            }
        }
    }

    link.channel.store(None);
    dial.driver.abort();
    Ok(())
}

/// Fetch the relay cert over the live channel and hot-swap it into `resolver`.
/// The first cert to arrive is the moment the relay starts serving. A fetch the
/// nest answers with nothing leaves the relay as it was — standing by, or
/// serving the cert it holds. A failed refresh logs and keeps the current cert;
/// the next push, reconnect or timer retries.
async fn refresh_cert(
    dispatcher: &Arc<RpcDispatcher>,
    keypair: &RelayKeypair,
    resolver: &RelayCertResolver,
    https_port: u16,
) {
    match fetch_and_unseal(dispatcher, keypair).await {
        Ok(Some((cert_pem, key_pem))) => match certified_key_from_pem(&cert_pem, &key_pem) {
            Ok(ck) => {
                let first = resolver.is_empty();
                resolver.store(ck);
                if first {
                    tracing::info!(
                        https_port,
                        "fauna-iroh-relay serving — self-hosted P2P relay, no n0 third party"
                    );
                    crate::log_catalogue::serving(https_port);
                } else {
                    tracing::info!("relay TLS cert refreshed (hot-swapped, no connection drop)");
                    crate::log_catalogue::cert_refreshed();
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "relay cert from nest unparsable; keeping current");
                crate::log_catalogue::cert_unparsable();
            }
        },
        Ok(None) => {
            tracing::debug!("nest handed the relay no cert (no public name yet); standing by");
        }
        Err(e) => {
            tracing::warn!(error = %e, "relay cert fetch failed; keeping current");
            crate::log_catalogue::cert_refresh_failed();
        }
    }
}

/// Resolve on the next SIGHUP (used to force a cert refresh). On non-unix, pends
/// forever (no SIGHUP), so only the timer drives refresh there.
async fn sighup() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to install SIGHUP handler");
                std::future::pending::<()>().await;
            }
        }
    }
    #[cfg(not(unix))]
    std::future::pending::<()>().await;
}

/// Resolve when the process receives SIGTERM (s6 stop) or Ctrl-C (dev).
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use iroh::address_lookup::AddrFilter;
    use iroh::endpoint::presets;
    use iroh::tls::CaTlsConfig;
    use iroh::{Endpoint, EndpointAddr, RelayMap, RelayMode, RelayUrl, SecretKey};
    use rustls::pki_types::CertificateDer;

    const ALPN: &[u8] = b"fauna/iroh-relay-binary-test/0";

    fn ring_provider() -> Arc<rustls::crypto::CryptoProvider> {
        Arc::new(rustls::crypto::ring::default_provider())
    }

    /// Generate a self-signed cert + key as in-memory PEM — the shape the relay
    /// gets after unsealing the nest blob (`certified_key_from_pem`). Also returns
    /// the cert DER so the test client can trust it via `CaTlsConfig::custom_roots`
    /// (the production trust shape, not the test-only `insecure_skip_verify`).
    fn self_signed_pem() -> (String, String, Vec<CertificateDer<'static>>) {
        let cert = rcgen::generate_simple_self_signed(vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
        ])
        .expect("self-signed cert");
        let der = cert.cert.der().clone();
        (cert.cert.pem(), cert.signing_key.serialize_pem(), vec![der])
    }

    /// Build the relay's HTTPS config the way `run` does — from an unsealed PEM via
    /// `certified_key_from_pem` → `RelayCertResolver` → `build_server_config`.
    fn server_config_from_pem(cert_pem: &str, key_pem: &str) -> rustls::ServerConfig {
        let ck = certified_key_from_pem(cert_pem.as_bytes(), key_pem.as_bytes())
            .expect("certified key from unsealed PEM");
        build_server_config(Arc::new(RelayCertResolver::new(ck))).expect("server config")
    }

    /// A fixed-set [`Admission`] — the rule stated directly, standing in for the
    /// nest's answer (the nest's own lookup is tested in `fauna-nest`).
    #[derive(Debug)]
    struct Members(Vec<[u8; 32]>);

    impl Admission for Members {
        fn admits<'a>(
            &'a self,
            endpoint_key: [u8; 32],
        ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            Box::pin(async move { self.0.contains(&endpoint_key) })
        }
    }

    /// The endpoint id `relay_only_endpoint(_, seed, ..)` binds with.
    fn endpoint_key(seed: u8) -> [u8; 32] {
        let mut sk = [0u8; 32];
        sk[0] = seed;
        *SecretKey::from_bytes(&sk).public().as_bytes()
    }

    /// Stand up the relay the way `run` does, admitting exactly `members`.
    async fn spawn_test_relay(
        members: Vec<[u8; 32]>,
    ) -> (Server, RelayUrl, Vec<CertificateDer<'static>>) {
        let (cert_pem, key_pem, relay_roots) = self_signed_pem();
        let config = server_config_from_pem(&cert_pem, &key_pem);
        let opts = RelayServerOptions {
            https_bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            http_bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            key_cache_capacity: Some(KEY_CACHE_CAPACITY),
            quic_bind: None,
        };
        let server = spawn_relay(opts, config, Arc::new(Members(members)))
            .await
            .expect("spawn relay");
        let https_addr = server.https_addr().expect("relay https addr");
        let relay_url: RelayUrl = format!("https://{https_addr}").parse().expect("relay url");
        (server, relay_url, relay_roots)
    }

    /// A raw iroh endpoint that registers ONLY its relay path (`relay_only`) — no
    /// direct addresses — so it is reachable solely through the relay. Trusts the
    /// relay's self-signed cert via `custom_roots` (the production trust shape).
    async fn relay_only_endpoint(
        relay_url: RelayUrl,
        seed: u8,
        alpns: Option<Vec<Vec<u8>>>,
        relay_roots: Vec<CertificateDer<'static>>,
    ) -> Endpoint {
        let mut sk = [0u8; 32];
        sk[0] = seed;
        let mut builder = Endpoint::builder(presets::Empty)
            .secret_key(SecretKey::from_bytes(&sk))
            .relay_mode(RelayMode::Custom(RelayMap::from(relay_url)))
            .ca_tls_config(CaTlsConfig::custom_roots(relay_roots))
            .addr_filter(AddrFilter::relay_only())
            .crypto_provider(ring_provider());
        if let Some(alpns) = alpns {
            builder = builder.alpns(alpns);
        }
        builder
            .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .expect("bind_addr")
            .bind()
            .await
            .expect("bind relay-only endpoint")
    }

    /// The binary's resolver-backed serving path (`certified_key_from_pem` →
    /// `build_server_config` → `spawn_relay`) stands up a working self-hosted relay
    /// that carries a full iroh connection between two relay-only peers with zero
    /// n0 — the durable, binary-level relay-sovereignty proof. Both peers are
    /// members: the admission rule lets exactly them through. (The dial+fetch of
    /// the sealed cert is proven against a live nest by the tier_4 test.)
    #[tokio::test]
    async fn binary_relay_carries_relay_only_peers() {
        let (server, relay_url, relay_roots) =
            spawn_test_relay(vec![endpoint_key(0xa1), endpoint_key(0xb2)]).await;

        // Acceptor: relay-only, echoes the first bidi stream's bytes.
        let acceptor = relay_only_endpoint(
            relay_url.clone(),
            0xa1,
            Some(vec![ALPN.to_vec()]),
            relay_roots.clone(),
        )
        .await;
        let acceptor_addr = EndpointAddr::new(acceptor.id()).with_relay_url(relay_url.clone());
        let acceptor_task = tokio::spawn(async move {
            let incoming = acceptor.accept().await.expect("acceptor: inbound conn");
            let conn = incoming.await.expect("acceptor: incoming -> conn");
            let (mut send, mut recv) = conn.accept_bi().await.expect("acceptor: accept_bi");
            let mut buf = [0u8; 5];
            recv.read_exact(&mut buf).await.expect("acceptor: read");
            send.write_all(&buf).await.expect("acceptor: echo");
            send.finish().expect("acceptor: finish");
            conn.closed().await;
            drop(acceptor);
        });

        // Connector: relay-only, no direct candidates — the relay is the only path.
        let connector = relay_only_endpoint(relay_url, 0xb2, None, relay_roots).await;
        let conn = tokio::time::timeout(
            Duration::from_secs(20),
            connector.connect(acceptor_addr, ALPN),
        )
        .await
        .expect("connect timed out")
        .expect("connect via our self-hosted relay");

        let (mut send, mut recv) = conn.open_bi().await.expect("open_bi");
        send.write_all(b"relay").await.expect("write");
        send.finish().expect("finish");
        let mut back = [0u8; 5];
        recv.read_exact(&mut back).await.expect("read echo");
        assert_eq!(
            &back, b"relay",
            "payload round-tripped through the self-hosted relay"
        );

        conn.close(0u32.into(), b"done");
        connector.close().await;
        acceptor_task.await.expect("acceptor task");
        server.shutdown().await.expect("relay shutdown");
    }

    /// The admission rule refuses a stranger: with only the acceptor a member, a
    /// connector whose key the rule does not admit cannot reach it through the
    /// relay — the same dial that succeeds in
    /// `binary_relay_carries_relay_only_peers` when both are members.
    #[tokio::test]
    async fn relay_refuses_an_endpoint_that_is_not_a_member() {
        let (server, relay_url, relay_roots) = spawn_test_relay(vec![endpoint_key(0xa1)]).await;

        let acceptor = relay_only_endpoint(
            relay_url.clone(),
            0xa1,
            Some(vec![ALPN.to_vec()]),
            relay_roots.clone(),
        )
        .await;
        let acceptor_addr = EndpointAddr::new(acceptor.id()).with_relay_url(relay_url.clone());

        // 0xb2 is not in the member set: the relay is its only path, and the
        // relay will not serve it.
        let stranger = relay_only_endpoint(relay_url, 0xb2, None, relay_roots).await;
        let attempt = tokio::time::timeout(
            Duration::from_secs(8),
            stranger.connect(acceptor_addr, ALPN),
        )
        .await;
        assert!(
            !matches!(attempt, Ok(Ok(_))),
            "an endpoint the admission rule refuses must not connect through the relay"
        );

        stranger.close().await;
        acceptor.close().await;
        server.shutdown().await.expect("relay shutdown");
    }

    /// `quic_bind` alone decides whether the address-discovery server runs: with
    /// none, the relay serves only the relay protocol; given a bind, the QUIC
    /// server comes up beside it under the same certificate
    /// (`behavior/p2p.md` § The relay → *Address discovery*, ruling 1).
    #[tokio::test]
    async fn address_discovery_is_served_only_when_given_a_bind() {
        let (off, _, _) = spawn_test_relay(vec![]).await;
        assert!(
            off.quic_addr().is_none(),
            "no quic_bind must serve no discovery"
        );
        off.shutdown().await.expect("relay shutdown");

        let (cert_pem, key_pem, _) = self_signed_pem();
        let opts = RelayServerOptions {
            https_bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            http_bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            key_cache_capacity: Some(KEY_CACHE_CAPACITY),
            quic_bind: Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)),
        };
        let on = spawn_relay(
            opts,
            server_config_from_pem(&cert_pem, &key_pem),
            Arc::new(Members(vec![])),
        )
        .await
        .expect("spawn relay with discovery");
        assert!(on.quic_addr().is_some(), "a quic_bind must serve discovery");
        on.shutdown().await.expect("relay shutdown");
    }

    /// `run` serves address discovery with nothing switched on: on every
    /// interface, at the substrate's default port (the one every endpoint's
    /// one-URL relay map queries), while the relay protocol stays on the
    /// artifact-set loopback binds (`behavior/p2p.md` § The relay → *Address
    /// discovery*, ruling 2).
    #[test]
    fn production_serves_discovery_on_every_interface_at_the_default_port() {
        let https: SocketAddr = "127.0.0.1:8445".parse().unwrap();
        let http: SocketAddr = "127.0.0.1:8446".parse().unwrap();
        let opts = production_options(https, http);
        assert_eq!(opts.https_bind, https);
        assert_eq!(opts.http_bind, http);
        assert_eq!(
            opts.quic_bind,
            Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 7842)),
            "discovery binds 0.0.0.0 at the substrate default every endpoint queries"
        );
        assert_eq!(
            DISCOVERY_PORT, 7842,
            "the port the artifact and guide publish"
        );
    }

    /// [`NestLink`] fails closed: with no channel to nest it admits nobody.
    #[tokio::test]
    async fn nest_link_without_a_channel_admits_nobody() {
        let link = NestLink::default();
        assert!(!link.admits(endpoint_key(0xa1)).await);
    }

    /// `certified_key_from_pem` surfaces a clean error (not a panic) on non-PEM —
    /// the misconfiguration / corrupt-unseal failure mode.
    #[test]
    fn certified_key_from_pem_rejects_non_pem() {
        let err = certified_key_from_pem(b"not a certificate", b"not a key")
            .expect_err("must reject non-PEM");
        assert!(
            err.to_string().contains("no certificates"),
            "unexpected error: {err}"
        );
    }

    /// The relay keypair persists on first call and reloads to the SAME keypair on
    /// the next call (a stable seal identity across restarts); a corrupt file is
    /// regenerated.
    #[test]
    fn relay_keypair_persists_and_reloads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let k1 = load_or_create_relay_keypair(dir.path()).expect("create");
        assert!(
            dir.path().join(RELAY_KEY_FILENAME).exists(),
            "keypair persisted on first call"
        );
        let k2 = load_or_create_relay_keypair(dir.path()).expect("reload");
        assert_eq!(k1.secret, k2.secret, "stable secret across reloads");
        assert_eq!(k1.public, k2.public, "stable public across reloads");

        // A corrupt key file is treated as absent and regenerated (fresh identity).
        std::fs::write(dir.path().join(RELAY_KEY_FILENAME), "garbage").unwrap();
        let k3 = load_or_create_relay_keypair(dir.path()).expect("regen");
        assert_ne!(k3.secret, k1.secret, "corrupt key file is regenerated");
    }

    /// The cert resolver hot-swaps: after `store`, `resolve` returns the new cert.
    #[test]
    fn relay_cert_resolver_hot_swaps() {
        let (cert1, key1, _) = self_signed_pem();
        let (cert2, key2, _) = self_signed_pem();
        let ck1 = certified_key_from_pem(cert1.as_bytes(), key1.as_bytes()).unwrap();
        let resolver = RelayCertResolver::new(ck1);
        let first = resolver.current.load_full().expect("first cert");

        let ck2 = certified_key_from_pem(cert2.as_bytes(), key2.as_bytes()).unwrap();
        resolver.store(ck2);
        let second = resolver.current.load_full().expect("second cert");
        assert!(
            !Arc::ptr_eq(&first, &second),
            "store swaps the served CertifiedKey"
        );
    }
}
