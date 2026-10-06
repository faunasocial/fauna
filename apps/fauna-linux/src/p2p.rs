//! P2P service module wrapping fauna-peer contact storage plus the
//! substrate-agnostic P2P transport lifecycle.
//!
//! `P2pService` is designed to be initialized once at app startup and held in
//! an `Arc`.  The P2P node (an iroh [`IrohTransport`] behind the
//! `fauna-transport` seam, driven by a [`PeerNode`]) is started on demand (not
//! at construction time) via [`P2pService::start_tunnel`].
//!
//! **Substrate: iroh, and only iroh.** `start_tunnel` builds an
//! [`IrohTransport`] from this actor's Ed25519 secret (so the iroh `NodeId`
//! *is* the actor key, PT-1b) and hosts a [`PeerNode`] on it. The bespoke
//! WireGuard stack this module once kept a parked keypair and signaling
//! sessions for was deleted 2026-08-23 (user-directed;
//! `docs/goal/behavior/p2p.md`). The P2P data plane is still dormant
//! (§ Transport seam) — the node listens + serves the base `fauna.peer.*`
//! kinds; no live chunk/manifest traffic rides it yet.

use std::net::{SocketAddr, UdpSocket};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use fauna_iroh::IrohTransport;
use fauna_peer::connection_quality::{ConnectionQuality, QualityDb};
use fauna_peer::contact::{PeerContact, PeerDb};
use fauna_peer_channel::PeerNode;
use fauna_transport::PeerTransport;

/// Manages P2P connectivity: peer contacts, connection quality, and the P2P
/// transport node (iroh behind the `fauna-transport` seam).
pub struct P2pService {
    /// `rusqlite::Connection` is `Send` but not `Sync`, and this service is
    /// held in an `Arc` and reached from multiple threads (the GTK main
    /// thread and background tokio tasks) — the `Mutex` is load-bearing, not
    /// a style choice.
    contact_db: Mutex<PeerDb>,
    quality_db: Mutex<QualityDb>,
    /// The running P2P node (iroh [`PeerNode`]) when the tunnel is active. `None`
    /// until [`start_tunnel`](Self::start_tunnel) brings it up.
    node: Mutex<Option<PeerNode>>,
    actor_id_hex: String,
    /// This actor's Ed25519 secret key — the iroh node identity (`NodeId` = actor
    /// key, PT-1b). Sourced from the client's `secret_hex()` at construction.
    actor_secret: [u8; 32],
    /// The app's long-lived tokio runtime. The [`PeerNode`]'s accept loop is a
    /// background task that must outlive `start_tunnel`, so it is spawned on this
    /// shared runtime, never a per-call throwaway one.
    runtime: tokio::runtime::Handle,
    /// Test-only bind-conflict fixture (e2e-conventions.md convention 8's
    /// carve-out (b) — arranges a REAL precondition, never a simulated
    /// error). When set, `start_tunnel` binds at this exact address instead
    /// of the OS-assigned one; the held [`UdpSocket`] keeps it occupied, so
    /// the iroh endpoint's own bind fails for a genuine reason. The field is
    /// always compiled (`start_tunnel` reads it, and it stays `None` in a
    /// release build); its only writers,
    /// `force_bind_conflict_for_test`/`clear_bind_conflict_for_test`, carry
    /// the same `#[cfg(any(debug_assertions, feature = "e2e-agent"))]` as
    /// `main.rs`'s test-command handler that calls them.
    test_bind_conflict: Mutex<Option<(UdpSocket, SocketAddr)>>,
}

impl P2pService {
    /// Initialize the P2P service.
    ///
    /// `actor_secret_hex` is this actor's Ed25519 secret key (the client's
    /// `secret_hex()`); it becomes the iroh node identity (the `NodeId` = the
    /// actor's Ed25519 public key). `runtime` is the app's long-lived tokio
    /// runtime, on which the P2P node's background accept loop is spawned.
    ///
    /// Opens (or creates) SQLite databases for peer contacts and connection
    /// quality under `{state_dir}/`.
    ///
    /// `state_dir` is this actor's already-scoped state directory (account-scoping.md
    /// § Serialized switching — the caller resolves + adopts it, e.g.
    /// `<xdg-config>/fauna/<actor-id-hex>/`); this constructor does no further
    /// per-actor namespacing of its own.
    pub fn new(
        actor_id_hex: &str,
        actor_secret_hex: &str,
        state_dir: &Path,
        runtime: tokio::runtime::Handle,
    ) -> Result<Arc<Self>> {
        let actor_secret = fauna_core::hex32::decode(actor_secret_hex)
            .context("decode actor Ed25519 secret key")?;

        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("create state dir: {}", state_dir.display()))?;

        // Open peer contact database
        let contact_db_path = state_dir.join("p2p_contacts.db");
        let contact_db = PeerDb::open(&contact_db_path).context("open peer contact database")?;

        // Open connection quality database
        let quality_db_path = state_dir.join("p2p_quality.db");
        let quality_db =
            QualityDb::open(&quality_db_path).context("open connection quality database")?;

        Ok(Arc::new(Self {
            contact_db: Mutex::new(contact_db),
            quality_db: Mutex::new(quality_db),
            node: Mutex::new(None),
            actor_id_hex: actor_id_hex.to_string(),
            actor_secret,
            runtime,
            test_bind_conflict: Mutex::new(None),
        }))
    }

    /// Start the P2P node: bind an iroh endpoint from this actor's Ed25519 secret
    /// and host a [`PeerNode`] on it (listening + serving the base `fauna.peer.*`
    /// kinds). Synchronous — it drives the async build on the app runtime so the
    /// node's background accept loop lives on that long-lived runtime, not a
    /// throwaway one (the caller must not `block_on` this on a per-call runtime).
    ///
    /// The peer data plane is dormant, so contacts are not dialed here — the
    /// node comes up listening; a future P2P feature drives outbound
    /// [`PeerNode::dial`].
    pub fn start_tunnel(&self) -> Result<()> {
        // Don't start if already running
        if self.is_tunnel_active() {
            anyhow::bail!("tunnel is already active");
        }

        let secret = self.actor_secret;
        // Test-only: a held-open socket at this address forces the real bind
        // below to fail (see `test_bind_conflict`'s doc comment).
        let bind_addr = self
            .test_bind_conflict
            .lock()
            .unwrap()
            .as_ref()
            .map(|(_, addr)| *addr);
        let node = self.runtime.block_on(async move {
            let mut builder = IrohTransport::builder(secret);
            if let Some(addr) = bind_addr {
                builder = builder.bind_addr(addr);
            }
            let transport = builder
                .build()
                .await
                .map_err(|e| anyhow::anyhow!("build iroh transport: {e}"))?;
            let transport: Arc<dyn PeerTransport> = Arc::new(transport);
            // The display name is not surfaced by the dormant P2P UI — leave it empty.
            Ok::<PeerNode, anyhow::Error>(PeerNode::start(transport, String::new()).await)
        })?;

        *self.node.lock().unwrap() = Some(node);
        Ok(())
    }

    /// Force the next [`start_tunnel`](Self::start_tunnel) call to fail with
    /// a REAL bind conflict — e2e-conventions.md convention 8's carve-out
    /// (b): this arranges the precondition, the UI click still drives the
    /// actual failure. Binds a raw UDP socket on an OS-assigned loopback
    /// port and holds it open; `start_tunnel` then points the iroh endpoint
    /// at that exact address, whose bind fails for the same reason a real
    /// port collision would. Idempotent: a second call replaces (drops) the
    /// previous held socket.
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    pub fn force_bind_conflict_for_test(&self) -> std::io::Result<()> {
        let held = UdpSocket::bind("127.0.0.1:0")?;
        let addr = held.local_addr()?;
        *self.test_bind_conflict.lock().unwrap() = Some((held, addr));
        Ok(())
    }

    /// Release the fixture above: drops the held socket, so the next
    /// `start_tunnel` binds normally again.
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    pub fn clear_bind_conflict_for_test(&self) {
        *self.test_bind_conflict.lock().unwrap() = None;
    }

    /// Stop the P2P node if it is running (drops it → aborts its accept loop +
    /// drops its inbound serving channels).
    pub fn stop_tunnel(&self) {
        let node = self.node.lock().unwrap().take();
        drop(node);
    }

    /// Returns `true` if the P2P node's accept loop is currently active.
    pub fn is_tunnel_active(&self) -> bool {
        self.node
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|n| n.is_active())
    }

    /// List all P2P-enabled contacts.
    pub fn list_contacts(&self) -> Result<Vec<PeerContact>> {
        self.contact_db
            .lock()
            .unwrap()
            .list_p2p_enabled()
            .context("list P2P contacts")
    }

    /// Add or update a peer contact.
    pub fn add_contact(&self, contact: &PeerContact) -> Result<()> {
        self.contact_db
            .lock()
            .unwrap()
            .upsert_contact(contact)
            .context("upsert peer contact")
    }

    /// Remove a peer contact by actor ID (hex-encoded).
    ///
    /// Uncalled as of 2026-08-02 — linux renders no removal
    /// affordance, and neither does any other app. It correctly wraps the
    /// survivor `delete_contact` (`p2p.md` § Implementation status today); the
    /// affordance itself is a 7-app job, not a
    /// linux-only one.
    pub fn remove_contact(&self, actor_id_hex: &str) -> Result<()> {
        let actor_id = fauna_core::hex32::decode(actor_id_hex)?;
        self.contact_db
            .lock()
            .unwrap()
            .delete_contact(&actor_id)
            .context("delete peer contact")
    }

    /// Get P2P node status for the UI: the local node id (hex) if active, `None`
    /// otherwise. That id is the node's own identity — its `NodeId`, which *is*
    /// its dialable address, so there is no separate host/port to report.
    pub fn tunnel_info(&self) -> Option<String> {
        let guard = self.node.lock().unwrap();
        guard.as_ref().and_then(|n| {
            if n.is_active() {
                Some(fauna_core::format::hex_full(n.local_identity().as_bytes()))
            } else {
                None
            }
        })
    }

    /// Get the latest connection quality measurement for a peer.
    pub fn get_quality(&self, actor_id_hex: &str) -> Result<Option<ConnectionQuality>> {
        let actor_id = fauna_core::hex32::decode(actor_id_hex)?;
        self.quality_db
            .lock()
            .unwrap()
            .latest(&actor_id)
            .context("query connection quality")
    }

    /// Access the actor ID hex string.
    pub fn actor_id_hex(&self) -> &str {
        &self.actor_id_hex
    }
}
