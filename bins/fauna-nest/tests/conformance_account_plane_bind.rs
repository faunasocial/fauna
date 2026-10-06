//! **The bind leg, holder half — a bound nest is made complete**, against real
//! nests and real account runtimes (`account-sync-plane.md` § The bind leg,
//! ruling 2 and its *Proof*; `account-data-taxonomy.md` § The generation
//! machinery → *A holder change re-receipts and never mints*;
//! `nest/box-recovery.md` § The plane-era recovery floor, (a)).
//!
//! Each case runs production [`AccountStoreRuntime`]s — the pump, its bind
//! check, its enrollment, publish diff, re-escrow and recovery passes, driven
//! through the handle only — over in-process nests built from the real
//! handlers. The runtime's requester is a [`Bound`] switch standing for "the
//! nest this device is bound to", which the test re-points under a running
//! runtime: at a second nest, at a box rebuilt with its identity and an empty
//! database, at a box that rotated its identity. The app's pin for that nest is
//! a [`Pin`] the test moves exactly as an accepted rotation or a fresh bind
//! would; the runtime re-reads it at every pass. Each case ends with a reader
//! holding only the identity seed.
//!
//! The last three cases are ruling 4's **secondary leg**: a nest linked to the
//! account with the `account_replica` capability, which no device binds to,
//! completed over a second connection the test's [`connector`] opens — the
//! cold read from it, the channel binding refused, and a shred reaching it.
//!
//! The case after them is the **custody leg's linked arm**
//! (`nest/box-recovery.md` § The plane-era recovery floor, *(c)*): a linked
//! nest the account administers has its own deployment seed custodied over
//! that same second connection, by a device never bound to it.
//!
//! The seven cases after it are the **seed-leg role** (`account-runtime.md`
//! § Multi-instance concurrency → *The seed-leg role*; `account-sync-plane.md`
//! § The bind leg, rulings 5 and 6): two runtimes on ONE store dir — a
//! seedless engine holder (the sync agent's shape, every connection of which
//! is the machine's store principal's) and a seed-holding app beside it that
//! never pumps — and the linked nest is still completed and custodied, by the
//! app's seed pass; the same over a real connection — the production
//! connector dialling a box that listens — read for how deep in the store
//! thread's stack that dial runs; a box rebuilt under the agent is registered at again by
//! that seed pass, then verified and settled by the agent; a retire the
//! seedless holder sent reaches the linked nest through the store's retire
//! record; a signed-out device is no member at the linked nest one leg run
//! later, and its removal row is gone from both nests; a retire whose record
//! entry was spent while the linked nest was away reaches it a round later;
//! and two seed holders on one store dir run one secondary leg between them.
//!
//! The three cases after them are **a removal across two bound nests**
//! (`account-sync-plane.md` § The bind leg, ruling 7): three devices of one
//! account on two nests each linked at the other, two bound to A and one to B;
//! one of the two at A signs out, or is removed by its sibling, and the device
//! bound to B reads it removed — also when the sibling at A cannot reach B —
//! and neither nest ends serving a device-set row of the departed device.
//!
//! The two cases after them are **the removal's nest half across two nests**
//! (`account-data-taxonomy.md` § The generation machinery → *Fleet-scope
//! reclamation*, clause (4) → *The nest half follows merged state*): the
//! device bound to B is lost and removed from a device bound to A, and B must
//! stop minting for its key and stop counting its walk mark (measured red
//! 2026-10-01, before the rule was built); and a lost device removed by key
//! stops minting at the nest it shares with its remover. Both nests keep the
//! row.
//!
//! The last three cases are **a removal on the fleet plane that the nest half
//! does not follow**, on one nest. Two are the guardian's enrolled device on a
//! supervised account (`behavior/family-safety.md` § Full visibility for young
//! children → *The device marker*): the ward's device writes its fleet
//! `Removed` row by key, the nest keeps the marked row and its grant, and the
//! guardian's device still reads and writes — then returns as a successor
//! principal by itself, on its own evidence
//! (`account-replica-posture.md` § The store device principal → *Principal
//! succession after a device delete*, decision 1, the third trigger). The
//! third is a removal the generation tip does not name, which nothing mints
//! past (`account-data-taxonomy.md` § The generation machinery → *The mint
//! protocol*, trigger (b)) — **measured red the same day and `#[ignore]`d
//! until that trigger is built.**
//!
//! The case after them is **the seed-only floor for every kind the `__config`
//! blob held** (`config-dissolution.md` § The `__config` dissolution schedule
//! → *The closure order*, step (5)): a device stores one record of each of the
//! twenty kinds and is lost; a fresh device holding only the identity seed
//! reads every one back.
//!
//! Tier: tier_3 (real nest handlers + real stores + real runtimes). Every
//! assertion is on latency-independent state (e2e convention 14): passes are
//! driven with `reconcile_now`, each one whole, and the only loop is a
//! bounded count of them.

mod common;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use ed25519_dalek::SigningKey;
use fauna_client::NestClient;
use fauna_core::crypto::BackupKey;
use fauna_core::data::DeploymentSeedEntry;
use fauna_core::encoding::canonical_decode;
use fauna_core::generation::EscrowReceiptRecord;
use fauna_core::generation::FleetView;
use fauna_core::generation::GenerationMintRecord;
use fauna_core::identity::ActorKeypair;
use fauna_credential_store::CredentialStore;
use fauna_nest::backup::service::BackupService;
use fauna_nest::routes::RegistrationConfig;
use fauna_nest::state::AuthState;
use fauna_nest::{
    db::CacheDb, folder_handlers, routes::AppState, rpc_router::RpcRouter, sync_handlers,
};
use fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE;
use fauna_protocol::admin::{AdminInviteCodeCreateReply, AdminInviteCodeCreateRequest};
use fauna_protocol::family::FamilyDeviceMarkRequest;
use fauna_protocol::merge_policy::{
    KIND_DEVICE_SET, KIND_ESCROW_RECEIPT, KIND_GENERATION_MINT, TipSealedKind,
};
use fauna_protocol::node_policy::RegistrationMode;
use fauna_protocol::sync::{SyncChangesListReply, SyncChangesListRequest};
use fauna_protocol::{RpcError, RpcErrorClass, RpcRequester, encode_canonical};
use fauna_sync_engine::account_runtime::{
    AccountRuntimeParams, AccountStoreHandle, AccountStoreRuntime, CRED_NAMESPACE, EnrollmentPass,
    LinkedNestConnector, PumpReport, RuntimePrincipal, STORE_THREAD_STACK_BUDGET, StoreRoot,
    store_thread_stack_depth,
};
use fauna_sync_engine::bind_leg::BindPass;
use fauna_sync_engine::cold_replica::{ColdFleetReplica, ColdKeySource};
use fauna_sync_engine::deployment_seed_recovery::cold_read_deployment_seeds;
use fauna_sync_engine::linked_leg::{LinkedConnection, LinkedNestTarget, LinkedOutcome};

// ── One account ───────────────────────────────────────────────────────────────

/// The identity seed — the one thing the seed-only reader at the end of every
/// case holds.
const IDENTITY_SEED: [u8; 32] = [0x44; 32];

fn account() -> ActorKeypair {
    ActorKeypair::from_secret(IDENTITY_SEED)
}

fn actor_bytes() -> [u8; 32] {
    account().actor_id().0
}

/// A box's deployment key from its seed byte — the identity a nest signs its
/// escrow receipts with, and the one an app pins for it.
fn deployment(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn id_of(key: &SigningKey) -> [u8; 32] {
    key.verifying_key().to_bytes()
}

// ── A nest: the real handlers, dispatched in-process ─────────────────────────

struct Nest {
    /// The identity seed of the account this box holds — [`IDENTITY_SEED`]
    /// unless the case boots its boxes under one of its own
    /// ([`Nest::boot_for`]).
    seed: [u8; 32],
    router: RpcRouter,
    state: Arc<AppState>,
    _blob: tempfile::TempDir,
    /// The deepest a request to this box was polled below an account store
    /// thread's entry, in bytes of stack ([`Nest::deepest_request`]).
    deepest_request: AtomicUsize,
    /// Every kind a handler of this box was dispatched for, in any session
    /// ([`Nest::served`]) — the box's own record of what reached it, so a
    /// test can say a kind never crossed the wire.
    served: Mutex<BTreeSet<&'static str>>,
}

#[derive(Debug)]
enum Refused {
    /// The nest answered the request with a refusal.
    Nest(RpcError),
    /// The connection never came up, so the request reached no handler: the
    /// machine's store principal was refused its bearer ([`MachinePrincipal`]).
    /// A transport failure, never a rejection of the request.
    NoConnection,
    /// A real connection ([`Bound::over_connection`]) failed under the
    /// request: a transport failure too, carrying the client's own words.
    Connection(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nest(e) => write!(f, "{}: {:?}", e.code, e.message),
            Self::NoConnection => write!(
                f,
                "no connection: the device handshake was refused (fauna.auth.not_registered)"
            ),
            Self::Connection(e) => write!(f, "connection: {e}"),
        }
    }
}

impl RpcErrorClass for Refused {
    fn is_rejection(&self) -> bool {
        matches!(self, Self::Nest(_))
    }
    fn as_rpc_error(&self) -> Option<&RpcError> {
        match self {
            Self::Nest(e) => Some(e),
            Self::NoConnection | Self::Connection(_) => None,
        }
    }
}

impl From<fauna_client::NestClientError> for Refused {
    fn from(e: fauna_client::NestClientError) -> Self {
        match e.as_rpc_error() {
            Some(refusal) if e.is_rejection() => Self::Nest(refusal.clone()),
            _ => Self::Connection(e.to_string()),
        }
    }
}

impl Nest {
    /// A box booted over an empty database with the deployment key `key`, the
    /// account created on it (its `users` row — what the sign-in creates).
    async fn boot(key: &SigningKey) -> Arc<Nest> {
        Self::boot_for(key, IDENTITY_SEED).await
    }

    /// [`Self::boot`], for the account of the identity seed `seed` — a case
    /// whose account must be its own in this process: the secondary leg's
    /// linked readings are one process-wide registry keyed by account
    /// (`fauna_client_core::recovery_pending`), which every other case's pass
    /// would rewrite under the shared one.
    async fn boot_for(key: &SigningKey, seed: [u8; 32]) -> Arc<Nest> {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        common::seed_dispatch_actor(&db, &ActorKeypair::from_secret(seed).actor_id().0).await;
        db.set_nest_keypair(key.as_bytes(), &id_of(key))
            .await
            .unwrap();
        Self::over(db, key, seed)
    }

    /// The same box's database served under the deployment key `key` — how a
    /// box comes back up after its identity rotated.
    fn over(db: Arc<CacheDb>, key: &SigningKey, seed: [u8; 32]) -> Arc<Nest> {
        Self::over_with(db, key, seed, |state| state)
    }

    /// The account this box holds.
    fn account(&self) -> ActorKeypair {
        ActorKeypair::from_secret(self.seed)
    }

    fn actor(&self) -> [u8; 32] {
        self.account().actor_id().0
    }

    /// [`Self::boot`], the account admitted **supervised** instead of seeded:
    /// an invite-gated box, a guardian account on it, an invite code carrying
    /// the guardian, and the account's own `fauna.account.register` redeeming
    /// it — the admission that writes the guardianship link. Answers the box
    /// and the guardian's actor id.
    async fn boot_supervised(key: &SigningKey) -> (Arc<Nest>, [u8; 32]) {
        const DOMAIN: &str = "test.fauna.social";
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        db.set_nest_keypair(key.as_bytes(), &id_of(key))
            .await
            .unwrap();
        let nest = Self::over_with(Arc::clone(&db), key, IDENTITY_SEED, |state| AppState {
            auth: AuthState {
                registration: RegistrationConfig {
                    handle_domain: Some(DOMAIN.to_string()),
                    reserved_handles: vec![],
                },
                ..Default::default()
            },
            registration_mode: Arc::new(tokio::sync::RwLock::new((
                RegistrationMode::InviteRequired,
                None,
            ))),
            ..state
        });
        let admin = common::admin_actor(&nest.state).await;
        let guardian = ActorKeypair::generate().actor_id().0;
        db.create_user_with_handle(&guardian, "personal", "parent", None)
            .await
            .unwrap();
        let minted: AdminInviteCodeCreateReply = nest
            .request_as(
                admin,
                "fauna.admin.invite_codes.create",
                AdminInviteCodeCreateRequest {
                    guardian_actor: Some(guardian.to_vec().into()),
                    ..Default::default()
                },
            )
            .await
            .expect("the admin mints a code carrying the guardian");
        nest.dispatch(
            [0u8; 32],
            "fauna.account.register",
            common::register_payload(&account(), "kid", DOMAIN, Some(&minted.code), None),
        )
        .await
        .expect("the account is admitted supervised");
        assert!(
            db.get_guardian_of(&actor_bytes()).await.unwrap().is_some(),
            "the admission wrote the guardianship link"
        );
        (nest, guardian)
    }

    /// [`Self::over`], the box's state shaped by `shape` before it serves.
    fn over_with(
        db: Arc<CacheDb>,
        key: &SigningKey,
        seed: [u8; 32],
        shape: impl FnOnce(AppState) -> AppState,
    ) -> Arc<Nest> {
        let blob = tempfile::tempdir().unwrap();
        let backup = Arc::new(
            BackupService::new(db.clone(), None, false, blob.path().into(), None).unwrap(),
        );
        Arc::new(Nest {
            seed,
            router: Self::handlers(),
            state: Arc::new(shape(AppState {
                backup_service: Some(backup),
                nest_signing_key: Some(key.clone()),
                ..AppState::for_test(db)
            })),
            _blob: blob,
            deepest_request: AtomicUsize::new(0),
            served: Mutex::new(BTreeSet::new()),
        })
    }

    /// Every handler family a box serves in this suite.
    fn handlers() -> RpcRouter {
        let mut b = RpcRouter::builder();
        folder_handlers::register_folders_handlers(&mut b);
        sync_handlers::register_sync_handlers(&mut b);
        fauna_nest::generation_escrow_handlers::register_generation_escrow_handlers(&mut b);
        // `fauna.auth.rotation_chain` — what the bind leg reads the ancestry from.
        fauna_nest::auth_handlers::register_auth_handlers(&mut b);
        // `fauna.pair.list` — what the secondary leg reads the linked replicas from.
        fauna_nest::pair_handlers::register_pair_handlers(&mut b);
        // What the custody leg's linked arm asks a linked nest: whether the
        // account administers it, its deployment seed, and its domain.
        fauna_nest::account_handlers::register_account_user_handlers(&mut b);
        fauna_nest::admin_ws_handlers::register_admin_handlers(&mut b);
        fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
        // The supervised admission and the guardian's device mark
        // ([`Self::boot_supervised`], the guardian-marked device's case).
        fauna_nest::invite_handlers::register_invite_handlers(&mut b);
        fauna_nest::account_handlers::register_account_handlers(&mut b);
        fauna_nest::family_handlers::register_family_handlers(&mut b);
        // The RecoveryKey registration chain the secondary leg carries to
        // every linked nest, and the kit ceremony that moves it.
        fauna_nest::recovery_handlers::register_recovery_handlers(&mut b);
        b.build()
    }

    /// [`Self::boot`], **listening**: the same box, also served by the real
    /// router over TLS on a loopback port with a self-signed certificate, the
    /// way a box with no public CA is served (`tls_channel_binding_roundtrip`'s
    /// idiom). Answers the box and its `https://` address. What a real
    /// [`NestClient`] dials — the login mint on an anonymous connection, the
    /// channel binding, the authenticated upgrade — is all real; the handlers
    /// behind it are the ones [`Self::dispatch`] reaches in-process.
    async fn boot_listening(key: &SigningKey) -> (Arc<Nest>, String) {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("self-signed cert");
        let cert_der: CertificateDer<'static> = cert.cert.der().clone();
        let spki = fauna_nest::acme::spki_sha256_of_cert_der(cert_der.as_ref())
            .expect("the served leaf's SPKI");
        let key_der: PrivateKeyDer<'static> =
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));

        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        common::seed_dispatch_actor(&db, &actor_bytes()).await;
        db.set_nest_keypair(key.as_bytes(), &id_of(key))
            .await
            .unwrap();
        let nest = Self::over_with(db, key, IDENTITY_SEED, |state| AppState {
            rpc_router: Arc::new(Self::handlers()),
            served_cert_spki: Some(Arc::new(common::FixedSpki(spki))),
            ..state
        });

        let server = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)
            .expect("server tls config");
        let (listener, addr) = common::nest_listener().await;
        tokio::spawn(fauna_nest::serve_tls(
            listener,
            tokio_rustls::TlsAcceptor::from(Arc::new(server)),
            fauna_nest::build_router(Arc::clone(&nest.state)).into_make_service(),
            fauna_conn_limit::PerIpConnLimit::new(fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP),
        ));
        (nest, format!("https://{addr}"))
    }

    /// How deep in an account store thread's stack this box's requests were
    /// polled, at the deepest: every runtime here polls its passes on its own
    /// store thread, and [`store_thread_stack_depth`] reads how far below
    /// that thread's entry a request is made. Requests a test makes itself,
    /// off any store thread, leave it alone. A box only a linked-nest
    /// connector reaches therefore reads the linked leg alone.
    fn deepest_request(&self) -> usize {
        self.deepest_request.load(Ordering::SeqCst)
    }

    /// Record a request to this box made from here, when here is a store
    /// thread.
    /// Whether any session ever asked this box for `kind`.
    fn served(&self, kind: &str) -> bool {
        self.served.lock().unwrap().contains(kind)
    }

    fn record_request_depth(&self) {
        if let Some(depth) = store_thread_stack_depth() {
            self.deepest_request.fetch_max(depth, Ordering::SeqCst);
        }
    }

    /// One request's bytes handed to the handler of `kind` as `actor`.
    async fn dispatch(
        &self,
        actor: [u8; 32],
        kind: &'static str,
        payload: Bytes,
    ) -> Result<Bytes, Refused> {
        self.served.lock().unwrap().insert(kind);
        let Some(meta) = self.router.kind_meta(kind) else {
            return Err(Refused::Nest(RpcError::new(
                "kind_not_served",
                "test.router.kind_not_served",
            )));
        };
        (meta.handler)(Arc::clone(&self.state), actor, payload)
            .await
            .map_err(Refused::Nest)
    }

    /// [`RpcRequester::request`] from another account's session — the admin's,
    /// the guardian's.
    async fn request_as<Req, Reply>(
        &self,
        actor: [u8; 32],
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Refused>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let bytes = Bytes::from(encode_canonical(&payload).unwrap().to_vec());
        let reply = self.dispatch(actor, kind, bytes).await?;
        Ok(fauna_protocol::decode_strict(&reply).expect("reply decodes"))
    }

    /// Rotate this box's deployment identity from `old` to `new` — the
    /// ceremony's atomic decision point (the chain statement, the keypair
    /// swap) — and serve the same database under the successor.
    async fn rotate(&self, old: &SigningKey, new: &SigningKey) -> Arc<Nest> {
        let outcome = self
            .state
            .db
            .rotate_deployment_seed(
                &zeroize::Zeroizing::new(old.to_bytes()),
                &zeroize::Zeroizing::new(new.to_bytes()),
            )
            .await
            .unwrap();
        assert!(outcome.is_ok(), "the rotation is refused: {outcome:?}");
        Self::over(Arc::clone(&self.state.db), new, self.seed)
    }

    /// Put the account on this box's admin roster — what claiming it does.
    async fn admit_admin(&self) {
        self.state.db.add_admin_actor(&actor_bytes()).await.unwrap();
    }

    /// Whether `fauna.auth.device_handshake` would mint for `device_key` here:
    /// the box holds that key's renewal grant and no revocation of it
    /// (`auth_core::device_auth_core`, steps 3 and 3b).
    async fn mints_for(&self, device_key: &[u8; 32]) -> bool {
        let db = &self.state.db;
        db.get_sync_device_grant(&actor_bytes(), device_key)
            .await
            .unwrap()
            .is_some()
            && !db
                .is_device_grant_revoked(&actor_bytes(), device_key)
                .await
                .unwrap()
    }

    async fn devices(&self) -> i64 {
        self.state
            .db
            .count_sync_devices(&actor_bytes())
            .await
            .unwrap()
    }

    async fn wraps(&self) -> usize {
        self.state
            .db
            .get_generation_escrow_wraps(&actor_bytes(), None)
            .await
            .unwrap()
            .len()
    }
}

impl RpcRequester for Nest {
    type Error = Refused;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply, Refused>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.record_request_depth();
        self.request_as(self.actor(), kind, payload).await
    }
}

/// The nest a device is bound to — the same URL, answered by whichever box
/// stands behind it now. Reached over the **owner session** (a signed-in app's
/// own connection, which any box holding the account admits) unless
/// `principal` says the connection is the machine's store principal's. With a
/// `connection`, requests travel over that real client instead of into the
/// box's handlers in-process ([`Bound::over_connection`]).
#[derive(Clone)]
struct Bound {
    nest: Arc<Mutex<Arc<Nest>>>,
    principal: Option<Arc<MachinePrincipal>>,
    connection: Option<Arc<NestClient>>,
}

/// A connection that authenticates as the machine's **store principal** — the
/// only credential a seedless process holds (`apps/sync-agent-credentials.md`
/// § Credential model). Its bearer is minted over `fauna.auth.device_handshake`
/// against the grant the box holds for the machine's key, so a box that holds
/// none answers `not_registered`: the connection never comes up, no request
/// reaches a handler, and the client's hook voids the slot's registration
/// latch (`fauna_client::ws_device_handshake_bearer`).
struct MachinePrincipal {
    device_key: [u8; 32],
    not_registered: Arc<dyn Fn() + Send + Sync>,
}

impl Bound {
    fn to(nest: &Arc<Nest>) -> Self {
        Self {
            nest: Arc::new(Mutex::new(Arc::clone(nest))),
            principal: None,
            connection: None,
        }
    }

    /// `nest`, reached over `connection` — a real client already connected to
    /// the address that box listens on ([`Nest::boot_listening`]). The box is
    /// named only so its [`Nest::deepest_request`] records where on a store
    /// thread each request over the connection is made.
    fn over_connection(nest: &Arc<Nest>, connection: Arc<NestClient>) -> Self {
        Self {
            connection: Some(connection),
            ..Self::to(nest)
        }
    }

    /// The account of the box behind the address.
    fn account(&self) -> ActorKeypair {
        self.nest.lock().unwrap().account()
    }

    fn rebind(&self, nest: &Arc<Nest>) {
        *self.nest.lock().unwrap() = Arc::clone(nest);
    }

    /// The same bound nest, reached as the store principal of the machine
    /// `base/<name>` — the slot a sign-in there populated.
    fn as_machine_principal(&self, base: &Path, name: &str) -> Self {
        let credentials =
            || CredentialStore::with_file_backend(CRED_NAMESPACE, base.join(name).join("creds"));
        let actor = account().actor_id_hex();
        let writer = fauna_sync_engine::principal_bundle::load_writer_key(&credentials(), &actor)
            .expect("a signed-in app enrolled this machine");
        Self {
            nest: Arc::clone(&self.nest),
            connection: self.connection.clone(),
            principal: Some(Arc::new(MachinePrincipal {
                device_key: writer.verifying_key().to_bytes(),
                not_registered: fauna_sync_engine::principal_bundle::not_registered_voids_latch_in(
                    credentials(),
                    &actor,
                ),
            })),
        }
    }
}

impl RpcRequester for Bound {
    type Error = Refused;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply, Refused>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let nest = Arc::clone(&self.nest.lock().unwrap());
        if let Some(principal) = &self.principal
            && !nest.mints_for(&principal.device_key).await
        {
            (principal.not_registered)();
            return Err(Refused::NoConnection);
        }
        match &self.connection {
            Some(connection) => {
                nest.record_request_depth();
                Self::over(connection, None, kind, payload).await
            }
            None => nest.request(kind, payload).await,
        }
    }
}

impl Bound {
    /// One request over the real connection, its future boxed in this
    /// function's own frame. Awaited inline, the arm's temporaries each took
    /// a slot in [`RpcRequester::request`]'s poll frame on an unoptimized
    /// build — 45 KiB that every in-process request then sat under, and that
    /// [`Nest::deepest_request`] read. This way that frame gains a pointer.
    fn over<'a, Req, Reply>(
        connection: &'a Arc<NestClient>,
        idempotency_key: Option<[u8; 16]>,
        kind: &'static str,
        payload: Req,
    ) -> std::pin::Pin<Box<impl Future<Output = Result<Reply, Refused>> + 'a>>
    where
        Req: serde::Serialize + 'a,
        Reply: serde::de::DeserializeOwned,
    {
        use fauna_protocol::KeyedRpcRequester as _;
        Box::pin(async move {
            Ok(match idempotency_key {
                Some(key) => connection.request_keyed(kind, key, payload).await?,
                None => connection.request(kind, payload).await?,
            })
        })
    }
}

/// In-process there is no envelope and no connection-layer idempotency cache:
/// the key has nowhere to go (the runtime conformance suite's own posture). A
/// real connection carries it.
impl fauna_protocol::KeyedRpcRequester for Bound {
    async fn request_keyed<Req, Reply>(
        &self,
        kind: &'static str,
        idempotency_key: [u8; 16],
        payload: Req,
    ) -> Result<Reply, Refused>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        match &self.connection {
            Some(connection) => {
                self.nest.lock().unwrap().record_request_depth();
                Self::over(connection, Some(idempotency_key), kind, payload).await
            }
            None => self.request(kind, payload).await,
        }
    }
}

/// The app's pin for the bound nest — what an accepted rotation or a fresh
/// bind moves, and what the runtime re-reads at every pass.
#[derive(Clone)]
struct Pin(Arc<Mutex<Vec<[u8; 32]>>>);

impl Pin {
    fn on(key: &SigningKey) -> Self {
        Self(Arc::new(Mutex::new(vec![id_of(key)])))
    }

    fn repin(&self, key: &SigningKey) {
        *self.0.lock().unwrap() = vec![id_of(key)];
    }
}

// ── A device ─────────────────────────────────────────────────────────────────

/// A seed-holding device of the account: its own store and credential slot
/// under `base/<name>`, bound through `bound`, trusting whatever `pin` names.
async fn device(base: &Path, name: &str, bound: &Bound, pin: &Pin) -> AccountStoreHandle {
    linked_device(base, name, bound, pin, None).await
}

/// [`device`], completing the account's linked replicas through `linked`
/// (the secondary leg's connector — `account-sync-plane.md` § The bind leg,
/// ruling 4).
async fn linked_device(
    base: &Path,
    name: &str,
    bound: &Bound,
    pin: &Pin,
    linked: Option<LinkedNestConnector<Bound>>,
) -> AccountStoreHandle {
    runtime_as(
        base,
        name,
        bound,
        pin,
        RuntimePrincipal::SeedHolding(bound.account().into()),
        linked,
    )
    .await
}

/// A runtime on the machine `name` — its store dir and its credential slot are
/// `base/<name>`'s, so two runtimes started under one name are two processes
/// of one machine sharing one store — assembled as `principal`: a signed-in
/// app, or the seedless sync agent.
async fn runtime_as(
    base: &Path,
    name: &str,
    bound: &Bound,
    pin: &Pin,
    principal: RuntimePrincipal,
    linked: Option<LinkedNestConnector<Bound>>,
) -> AccountStoreHandle {
    let pin = pin.clone();
    AccountStoreRuntime::start(AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        store_root: StoreRoot::at(base.join(name).join("state")),
        actor_id_hex: bound.account().actor_id_hex(),
        rpc: bound.clone(),
        process_rpc: None,
        principal,
        credentials: CredentialStore::with_file_backend(
            CRED_NAMESPACE,
            base.join(name).join("creds"),
        ),
        reconnects: None,
        pushes: None,
        // Disarmed: every pass below is driven.
        backstop_interval: Duration::from_secs(3600),
        memberships: None,
        trusted_escrow_holders: Arc::new(move || pin.0.lock().unwrap().clone()),
        attested_predecessors: Default::default(),
        linked_nests: linked,
        owed_nests: None,
        peer_transport: None,
        enrollment_target_device_id: format!("{:0<64}", hex::encode(name.as_bytes())),
    })
    .await
    .expect("the runtime assembles")
}

/// The secondary leg's connector over in-process nests: each linked address
/// answers with its box, presenting the identity the test names for it (the
/// box's own, or — an impostor — another).
fn connector(links: Vec<(&'static str, Arc<Nest>, [u8; 32])>) -> LinkedNestConnector<Bound> {
    gated_connector(links, Arc::new(AtomicBool::new(true)))
}

/// [`connector`], every link reachable only while `reachable` holds.
fn gated_connector(
    links: Vec<(&'static str, Arc<Nest>, [u8; 32])>,
    reachable: Arc<AtomicBool>,
) -> LinkedNestConnector<Bound> {
    Arc::new(move |target: LinkedNestTarget| {
        let found = links
            .iter()
            .filter(|_| reachable.load(Ordering::SeqCst))
            .find(|(url, _, _)| *url == target.nest_url)
            .map(|(_, nest, presented)| (Bound::to(nest), *presented));
        Box::pin(async move {
            let (rpc, bound_identity) =
                found.ok_or_else(|| anyhow::anyhow!("{} is unreachable", target.nest_url))?;
            Ok(LinkedConnection {
                rpc,
                bound_identity,
            })
        })
    })
}

/// How deep an account store thread's stack ran while a linked nest was being
/// connected to ([`dialling_connector`]) — the deepest of every connect.
#[derive(Default)]
struct ConnectDepth {
    /// Connects that came up.
    connects: AtomicUsize,
    /// Where the connector itself was polled, in bytes below the thread's
    /// entry.
    at_connector: AtomicUsize,
    /// The lowest stack page touched while it ran, in bytes below the
    /// thread's entry — zero where the page table cannot be read
    /// ([`stack_pages`]).
    high_water: AtomicUsize,
}

/// The secondary leg's **production** connector
/// (`fauna_client_account_runtime::native_linked_nest_connector`) over the
/// listening box `nest`: a real `NestClient` to the linked address, whose
/// `connect()` awaits the login mint inline — an anonymous connection dialled,
/// the TLS and WebSocket handshakes, the silent challenge — and then the
/// identity that connection is bound to. All of it is polled where the leg
/// calls the connector, on the store thread, under the pass; `depth` records
/// how deep that went. The leg's requests then travel over the connection.
fn dialling_connector(nest: &Arc<Nest>, depth: &Arc<ConnectDepth>) -> LinkedNestConnector<Bound> {
    let native = fauna_client_account_runtime::native_linked_nest_connector(account());
    let (nest, depth) = (Arc::clone(nest), Arc::clone(depth));
    Arc::new(move |target: LinkedNestTarget| {
        let connecting = native(target);
        let (nest, depth) = (Arc::clone(&nest), Arc::clone(&depth));
        Box::pin(async move {
            let at_connector = store_thread_stack_depth();
            let below = stack_pages::forget_below_here();
            let connected = connecting.await?;
            depth.connects.fetch_add(1, Ordering::SeqCst);
            if let Some(at_connector) = at_connector {
                depth.at_connector.fetch_max(at_connector, Ordering::SeqCst);
            }
            if let Some(high_water) = below.and_then(|below| below.deepest_touched()) {
                depth.high_water.fetch_max(high_water, Ordering::SeqCst);
            }
            Ok(LinkedConnection {
                rpc: Bound::over_connection(&nest, connected.rpc),
                bound_identity: connected.bound_identity,
            })
        })
    })
}

/// **A store thread's stack high-water over one stretch of work**, read off
/// the page table. A request-site reading ([`Nest::deepest_request`]) sees
/// only first-party frames above the point it is taken at; what a dial runs
/// below the last first-party frame — the TLS and WebSocket handshakes, in
/// dependency code built unoptimized like everything else in a dev build — it
/// cannot see. The page table can: a stack page is present once something has
/// written to it. [`forget_below_here`] hands every page under the caller's
/// frame back to the kernel, so none is present; [`Below::deepest_touched`]
/// then answers the lowest one present again, which is how deep the work in
/// between ran. Linux only — elsewhere nothing is forgotten and nothing read.
mod stack_pages {
    /// The unused part of a store thread's stack, below the frame that asked.
    pub struct Below {
        #[cfg(target_os = "linux")]
        range: linux::Range,
    }

    /// Forget every stack page below the caller — `None` off a store thread,
    /// and on a platform this cannot read.
    pub fn forget_below_here() -> Option<Below> {
        #[cfg(target_os = "linux")]
        {
            let range = linux::Range::below_here()?;
            range.forget();
            Some(Below { range })
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }

    impl Below {
        /// How far below the thread's entry the lowest page touched since
        /// [`forget_below_here`] starts, in bytes — `None` when none was.
        pub fn deepest_touched(&self) -> Option<usize> {
            #[cfg(target_os = "linux")]
            {
                self.range.deepest_present()
            }
            #[cfg(not(target_os = "linux"))]
            {
                None
            }
        }
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use std::io::{Read, Seek, SeekFrom};

        use fauna_sync_engine::account_runtime::{
            STORE_THREAD_STACK_BUDGET, store_thread_stack_depth,
        };

        /// Kept clear under the caller's frame: the frames of the calls that
        /// forget the pages are themselves below it.
        const LIVE_MARGIN: usize = 16 * 1024;

        /// Whole pages `[low, high)` of the calling store thread's stack,
        /// all below every live frame; `base` is the thread's entry.
        pub struct Range {
            base: usize,
            low: usize,
            high: usize,
            page: usize,
        }

        #[inline(never)]
        fn here() -> usize {
            let here = 0u8;
            std::hint::black_box(std::ptr::from_ref(&here)).addr()
        }

        /// The calling thread's stack as the C library allocated it: its
        /// lowest usable address (the guard is below it) and its size. Asked,
        /// never computed from the thread's entry or read off the process's
        /// mappings: the thread-local block sits at the top of the same
        /// allocation, so the entry is an unknown distance under it, and a
        /// guard the kernel keeps as a marking rather than a mapping of its
        /// own leaves neighbouring threads' stacks in one mapping — a range
        /// reckoned either way reaches into the next thread's block.
        fn this_threads_stack() -> Option<(usize, usize)> {
            // SAFETY: the attribute object is initialised by
            // `pthread_getattr_np` before it is read and destroyed after;
            // the out-parameters are locals.
            unsafe {
                let mut attr = std::mem::MaybeUninit::<libc::pthread_attr_t>::uninit();
                if libc::pthread_getattr_np(libc::pthread_self(), attr.as_mut_ptr()) != 0 {
                    return None;
                }
                let mut lowest = std::ptr::null_mut();
                let mut size = 0usize;
                let rc = libc::pthread_attr_getstack(attr.as_ptr(), &mut lowest, &mut size);
                libc::pthread_attr_destroy(attr.as_mut_ptr());
                (rc == 0).then_some((lowest.addr(), size))
            }
        }

        impl Range {
            pub fn below_here() -> Option<Self> {
                let depth = store_thread_stack_depth()?;
                let here = here();
                // SAFETY: `sysconf` reads a constant.
                let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
                // The engine measured `depth` from a frame of its own a call
                // away from this one: `base` is the entry to within that.
                let base = here + depth;
                let (lowest, size) = this_threads_stack()?;
                assert!(
                    (lowest..lowest + size).contains(&here)
                        && size >= 2 * STORE_THREAD_STACK_BUDGET,
                    "a store thread's stack is {size} bytes from {lowest:#x}, and this frame \
                     is at {here:#x}"
                );
                // A page clear of the guard.
                let low = lowest.next_multiple_of(page) + page;
                let high = (here - LIVE_MARGIN) / page * page;
                (low < high).then_some(Self {
                    base,
                    low,
                    high,
                    page,
                })
            }

            pub fn forget(&self) {
                // SAFETY: the range is whole pages of this thread's own stack
                // mapping, strictly below every live frame (`LIVE_MARGIN`) and
                // above its guard (`GUARD_MARGIN`): nothing reads them before
                // writing them, and `MADV_DONTNEED` on private anonymous
                // memory only makes the next touch a zero-fill fault.
                let rc = unsafe {
                    libc::madvise(
                        std::ptr::with_exposed_provenance_mut(self.low),
                        self.high - self.low,
                        libc::MADV_DONTNEED,
                    )
                };
                assert_eq!(rc, 0, "madvise: {}", std::io::Error::last_os_error());
            }

            /// One 64-bit entry per page; bit 63 is "present", bit 62
            /// "swapped" — either means the page was written.
            pub fn deepest_present(&self) -> Option<usize> {
                let mut entries = vec![0u8; (self.high - self.low) / self.page * 8];
                let mut pagemap = std::fs::File::open("/proc/self/pagemap").expect("pagemap");
                pagemap
                    .seek(SeekFrom::Start((self.low / self.page * 8) as u64))
                    .expect("pagemap seek");
                pagemap.read_exact(&mut entries).expect("pagemap read");
                entries
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .position(|entry| u64::from_ne_bytes(*entry) >> 62 != 0)
                    .map(|index| self.base - (self.low + index * self.page))
            }
        }
    }
}

/// Link `linked` to the account on `nest` as the user does from the Nests
/// page: a pairing row carrying the full self-sync set (`account_replica`
/// included) at `url`.
async fn link(nest: &Nest, linked: &SigningKey, url: &str) {
    nest.state
        .db
        .store_pairing(
            &nest.actor(),
            &id_of(linked),
            &fauna_protocol::pair::default_self_sync(),
            None,
            Some(url),
            None,
        )
        .await
        .unwrap();
}

/// The live rows `nest` serves on the fleet scope sealed under `generation`.
async fn rows_sealed_under(nest: &Nest, generation: &[u8; 32]) -> usize {
    let reply: SyncChangesListReply = nest
        .request(
            "fauna.sync.changes.list",
            SyncChangesListRequest {
                item_class: Some("state-entry".into()),
                scope: Some(ACCOUNT_STATE_FLEET_SCOPE.into()),
                frontier: Some(Default::default()),
                sealed_under: Some(generation.to_vec().into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    reply.changes.len()
}

/// The generations `nest` holds escrow wraps for, for the account.
async fn wrapped_generations(nest: &Nest) -> Vec<[u8; 32]> {
    nest.state
        .db
        .get_generation_escrow_wraps(&actor_bytes(), None)
        .await
        .unwrap()
        .iter()
        .map(|w| w.generation_id)
        .collect()
}

/// The generations a device's merged state reads `Shredded`.
async fn shredded(handle: &AccountStoreHandle) -> Vec<[u8; 32]> {
    handle
        .states_of_kind(KIND_GENERATION_MINT)
        .await
        .unwrap()
        .iter()
        .filter(|e| {
            matches!(
                canonical_decode::<GenerationMintRecord>(&e.value),
                Ok(GenerationMintRecord::Shredded { .. })
            )
        })
        .map(|e| fauna_core::hex32::decode(&e.key).unwrap())
        .collect()
}

/// Every linked nest the last pass completed, with how.
fn linked_outcomes(report: &PumpReport) -> Vec<LinkedOutcome> {
    report
        .linked
        .as_ref()
        .map(|p| p.nests.iter().map(|n| n.outcome.clone()).collect())
        .unwrap_or_default()
}

/// How many whole passes a device may take to settle — generous, and a count,
/// never a clock.
const PASS_BUDGET: usize = 12;

/// Drive passes until one runs clean and finds its bound replica settled;
/// answer every report on the way (the bind leg's verdicts are asserted on
/// them).
async fn settle(handle: &AccountStoreHandle, what: &str) -> Vec<PumpReport> {
    let mut reports = Vec::new();
    for _ in 0..PASS_BUDGET {
        let report = handle.reconcile_now().await.expect("pass");
        let done = report.errors.is_empty()
            && matches!(
                report.bind,
                Some(
                    BindPass::Settled
                        | BindPass::FirstBind { adopted: true }
                        | BindPass::Verified { settled: true }
                )
            );
        reports.push(report);
        if done {
            return reports;
        }
    }
    panic!(
        "{what}: no clean, settled pass within {PASS_BUDGET} passes; last report: {:?}",
        reports.last()
    );
}

/// The live generation-mint rows a device's merged state holds.
async fn mints(handle: &AccountStoreHandle) -> usize {
    handle
        .states_of_kind(KIND_GENERATION_MINT)
        .await
        .unwrap()
        .iter()
        .filter(|e| !e.tombstone)
        .count()
}

/// The holders of every live escrow receipt in a device's merged state.
async fn receipt_holders(handle: &AccountStoreHandle) -> Vec<[u8; 32]> {
    handle
        .states_of_kind(KIND_ESCROW_RECEIPT)
        .await
        .unwrap()
        .iter()
        .filter(|e| !e.tombstone)
        .map(|e| {
            canonical_decode::<EscrowReceiptRecord>(&e.value)
                .expect("a receipt row decodes")
                .holder_id
        })
        .collect()
}

/// A box's deployment seed, as its custody row carries it.
fn custody_entry(seed: u8) -> DeploymentSeedEntry {
    let seed = [seed; 32];
    DeploymentSeedEntry {
        nest_actor_id: fauna_core::data::DeploymentSeedEntry::nest_actor_id_for_seed(seed),
        seed: seed.into(),
        domain: Some("box.example".into()),
        ..Default::default()
    }
}

/// Custody `entry` through the production door — the runtime's
/// `merge_deployment_seeds`, which seals under the generation tip, so it waits
/// for the first-need mint the device-endpoints writer trips — then settle
/// until the row rests at the bound nest.
async fn custody(handle: &AccountStoreHandle, entry: &DeploymentSeedEntry) {
    let mut merged = false;
    for _ in 0..PASS_BUDGET {
        handle.reconcile_now().await.expect("pass");
        if handle
            .merge_deployment_seeds(vec![entry.clone()])
            .await
            .is_ok()
        {
            merged = true;
            break;
        }
    }
    assert!(merged, "the custody write never found a tip to seal under");
    for _ in 0..PASS_BUDGET {
        handle.reconcile_now().await.expect("pass");
        if handle
            .deployment_seed_published(entry.nest_actor_id)
            .await
            .unwrap()
        {
            settle(handle, "after the custody write").await;
            return;
        }
    }
    panic!("the custody row never rested at the bound nest");
}

/// Whether any report says the pass verified a changed replica and settled it.
fn verified_and_settled(reports: &[PumpReport]) -> bool {
    reports
        .iter()
        .any(|r| r.bind == Some(BindPass::Verified { settled: true }))
}

// ── The three cases ──────────────────────────────────────────────────────────

/// **A second nest.** A device custodies a box's deployment seed while bound
/// to nest A, is rebound to nest B — the same account, a different box, whose
/// identity the app pins — and settles; A is lost; a reader holding only the
/// identity seed reads the box's seed from B with the cold read.
///
/// The bind verification registers the device on B (the latch was learned from
/// A); the publish diff completes B's rows; the re-escrow deposits every
/// generation at B, re-receipted by B beside A's receipt; and because A had
/// receipted the tip for this identity, only the holder changed — nothing is
/// minted.
///
/// Red-verified: with the trusted set frozen at assembly (the pin not re-read)
/// B's wraps never land and the reader reads nothing; with the re-escrow
/// minting whenever it re-escrowed the tip, the mint count moves.
#[tokio::test]
async fn a_bind_to_a_second_nest_completes_the_replica_and_a_seed_only_reader_recovers_from_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (ka, kb) = (deployment(0x66), deployment(0x77));
    let a = Nest::boot(&ka).await;
    let b = Nest::boot(&kb).await;
    let bound = Bound::to(&a);
    let pin = Pin::on(&ka);

    let d = device(tmp.path(), "d", &bound, &pin).await;
    let entry = custody_entry(0x5D);
    custody(&d, &entry).await;
    let minted_on_a = mints(&d).await;
    assert!(
        minted_on_a > 0,
        "the custody write sealed under a minted tip"
    );
    assert_eq!(b.devices().await, 0);

    // The rebind: the device is now bound to B, and the app pins B.
    bound.rebind(&b);
    pin.repin(&kb);
    let reports = settle(&d, "after the rebind to B").await;
    assert!(
        verified_and_settled(&reports),
        "a changed replica is verified ahead of the other legs and settled: {:?}",
        reports.iter().map(|r| r.bind).collect::<Vec<_>>()
    );
    assert_eq!(
        b.devices().await,
        1,
        "the bind verification registered this machine on B"
    );
    assert!(
        receipt_holders(&d).await.contains(&id_of(&kb)),
        "every generation is re-receipted by B"
    );
    assert!(
        receipt_holders(&d).await.contains(&id_of(&ka)),
        "A's receipt stands beside B's"
    );
    assert_eq!(
        mints(&d).await,
        minted_on_a,
        "a change of nest mints nothing — only the holder changed"
    );

    d.shutdown().await;
    drop(bound);
    drop(a); // A is lost.

    let read = cold_read_deployment_seeds(&*b, &IDENTITY_SEED)
        .await
        .expect("the cold read");
    assert_eq!(
        read,
        vec![entry.clone()],
        "the seed-only reader reads the box from B"
    );
    assert_eq!(
        fauna_core::data::DeploymentSeedEntry::nest_actor_id_for_seed(read[0].seed.to_array()),
        entry.nest_actor_id,
        "the recovered seed re-creates the box's identity"
    );
}

/// **A rebuilt nest.** A's database is replaced by an empty one booted with the
/// same deployment identity, and the account is re-created there; the running
/// device's next passes make it whole — re-registered, its rows re-pushed, its
/// wraps re-deposited — and then the device is lost and a seed-only reader
/// recovers from the rebuilt A.
///
/// The receipts in merged state still name A's identity, which is why the
/// holdings check exists: a receipt proves a deposit, not a holding. The
/// re-deposit rewrites no receipt row and mints nothing.
///
/// Red-verified: without the holdings check (every acked generation skipped)
/// the rebuilt box holds no wrap and the reader reads nothing; without the
/// bind verification re-arming it, a running device never checks again.
#[tokio::test]
async fn a_rebuilt_nest_is_made_a_replica_and_an_escrow_holder_again() {
    let tmp = tempfile::tempdir().unwrap();
    let ka = deployment(0x66);
    let a = Nest::boot(&ka).await;
    let bound = Bound::to(&a);
    let pin = Pin::on(&ka);

    let d = device(tmp.path(), "d", &bound, &pin).await;
    let entry = custody_entry(0x5E);
    custody(&d, &entry).await;
    let minted = mints(&d).await;
    let receipts = receipt_holders(&d).await;
    assert!(a.wraps().await > 0);

    // The rebuild: same identity, empty database, the account re-created.
    let rebuilt = Nest::boot(&ka).await;
    bound.rebind(&rebuilt);
    drop(a);
    assert_eq!(rebuilt.wraps().await, 0);

    let reports = settle(&d, "after the rebuild").await;
    assert!(
        verified_and_settled(&reports),
        "a rebuilt box is a new replica under the same identity: {:?}",
        reports.iter().map(|r| r.bind).collect::<Vec<_>>()
    );
    assert_eq!(
        rebuilt.devices().await,
        1,
        "the bind verification registered this machine again"
    );
    assert!(
        rebuilt.wraps().await > 0,
        "the holdings check re-deposited the wraps"
    );
    assert_eq!(
        receipt_holders(&d).await,
        receipts,
        "no receipt row is rewritten — the ones in merged state stand"
    );
    assert_eq!(mints(&d).await, minted, "a rebuild mints nothing");

    d.shutdown().await;

    let read = cold_read_deployment_seeds(&*rebuilt, &IDENTITY_SEED)
        .await
        .expect("the cold read");
    assert_eq!(
        read,
        vec![entry],
        "the seed-only reader reads the box from the rebuilt A"
    );
}

/// **A rotated nest.** The box rotates its deployment identity under a running
/// device.
///
/// First, a fresh seed-holding device that pins the successor recovers a
/// generation still receipted only under the predecessor: its escrow-recovery
/// pass admits the predecessor as a verified ancestor, proven by the box's own
/// rotation chain, and it reads the custodied seed.
///
/// Then the box rotates again, and the device that was running all along —
/// never restarted — follows the pin: its next pass re-receipts every
/// generation under the new identity, mints nothing, and keeps sealing.
///
/// Red-verified: without the ancestry the fresh device recovers nothing and
/// reads no custody; with the trusted set frozen at assembly the running
/// device's re-escrow is refused the successor's receipt; with the re-escrow
/// minting on any re-escrowed tip, the mint count moves.
#[tokio::test]
async fn a_rotated_nest_keeps_every_generation_recoverable() {
    let tmp = tempfile::tempdir().unwrap();
    let (ka, ks, kt) = (deployment(0x66), deployment(0x67), deployment(0x68));
    let a = Nest::boot(&ka).await;
    let bound = Bound::to(&a);
    let pin = Pin::on(&ka);

    let d = device(tmp.path(), "d", &bound, &pin).await;
    let entry = custody_entry(0x5F);
    custody(&d, &entry).await;
    let minted = mints(&d).await;

    // ── The first rotation; D is not driven again until the second.
    let rotated = a.rotate(&ka, &ks).await;
    bound.rebind(&rotated);
    pin.repin(&ks);
    assert!(
        receipt_holders(&d).await.iter().all(|h| *h == id_of(&ka)),
        "every generation is receipted only under the predecessor"
    );

    // F: a fresh device, the seed and a pin on the successor — nothing else.
    let f_bound = Bound::to(&rotated);
    let f = device(tmp.path(), "f", &f_bound, &Pin::on(&ks)).await;
    // No sibling keys F in (D is not driven), so the only way F opens the
    // custody row is its own escrow recovery — through the predecessor's
    // receipt (its prologue pass already ran it).
    settle(&f, "the fresh device").await;
    assert_eq!(
        f.deployment_seeds().await.unwrap(),
        vec![entry.clone()],
        "F recovers the generation receipted only under the predecessor and reads the \
         custodied seed"
    );
    f.shutdown().await;

    // ── The second rotation, under D still running.
    let rotated_again = rotated.rotate(&ks, &kt).await;
    bound.rebind(&rotated_again);
    pin.repin(&kt);
    let reports = settle(&d, "the running device after the second rotation").await;
    assert!(
        verified_and_settled(&reports),
        "a rotated box is the same replica under a new identity: {:?}",
        reports.iter().map(|r| r.bind).collect::<Vec<_>>()
    );
    assert!(
        receipt_holders(&d).await.contains(&id_of(&kt)),
        "the running device re-receipted under the successor, with no restart"
    );
    assert_eq!(mints(&d).await, minted, "a rotation mints nothing");
    let second = custody_entry(0x60);
    d.merge_deployment_seeds(vec![second.clone()])
        .await
        .expect("the running device keeps sealing under the successor's trust");
    settle(&d, "after sealing under the successor").await;
    assert!(
        d.deployment_seed_published(second.nest_actor_id)
            .await
            .unwrap()
    );
    d.shutdown().await;
}

/// The bind leg's probe on a nest that has never seen this account's state
/// scope still names the replica — the empty-feed reply is the rebuilt box's
/// first answer, and it must be told apart from the one the device settled on.
#[tokio::test]
async fn an_empty_scope_still_names_its_replica() {
    let a = Nest::boot(&deployment(0x66)).await;
    let first = fauna_sync_engine::bind_leg::probe_bound_replica(&*a)
        .await
        .unwrap();
    assert!(first.is_some(), "an empty feed names its replica");
    let rebuilt = Nest::boot(&deployment(0x66)).await;
    assert_ne!(
        fauna_sync_engine::bind_leg::probe_bound_replica(&*rebuilt)
            .await
            .unwrap(),
        first,
        "a rebuilt database is another replica"
    );
}

// ── The secondary leg (ruling 4) ─────────────────────────────────────────────

/// **A linked nest is completed without ever being bound.** A device bound to
/// nest A — nest B linked to the account with the `account_replica`
/// capability — custodies a box's deployment seed. The device never binds to
/// B and registers nothing there; its secondary leg completes B over a second
/// connection. A and the device are lost; a reader holding only the identity
/// seed recovers the box's seed from B with the cold read.
///
/// Red-verified: with the secondary leg switched off in the pass, nothing is
/// completed at B (this case and both below go red); with the channel-binding
/// check removed, the next case goes red.
#[tokio::test]
async fn a_linked_nest_is_completed_without_ever_being_bound() {
    let tmp = tempfile::tempdir().unwrap();
    let (ka, kb) = (deployment(0x66), deployment(0x77));
    let a = Nest::boot(&ka).await;
    let b = Nest::boot(&kb).await;
    link(&a, &kb, "https://b.test").await;
    let bound = Bound::to(&a);
    let pin = Pin::on(&ka);

    let d = linked_device(
        tmp.path(),
        "d",
        &bound,
        &pin,
        Some(connector(vec![(
            "https://b.test",
            Arc::clone(&b),
            id_of(&kb),
        )])),
    )
    .await;
    let entry = custody_entry(0x5F);
    custody(&d, &entry).await;
    // One more whole pass: the leg deposits at B the generation the custody
    // write's mint made, and its receipt reaches B by the next diff.
    let reports = settle(&d, "after the custody write").await;
    let report = d.reconcile_now().await.expect("pass");
    assert!(
        linked_outcomes(&report)
            .iter()
            .all(|o| matches!(o, LinkedOutcome::Completed(c) if c.errors.is_empty())),
        "the secondary leg completes B cleanly: {:?} (earlier: {:?})",
        report.linked,
        reports.last().map(|r| &r.linked)
    );
    assert_eq!(
        linked_outcomes(&report).len(),
        1,
        "B, and only B, is a linked replica"
    );
    assert_eq!(b.devices().await, 0, "the device never registered on B");
    assert!(b.wraps().await > 0, "every generation is escrowed at B too");
    assert!(
        receipt_holders(&d).await.contains(&id_of(&kb)),
        "B's receipt is written in its own cell"
    );
    assert!(
        receipt_holders(&d).await.contains(&id_of(&ka)),
        "the bound nest's receipt stands beside it"
    );

    d.shutdown().await;
    drop(bound);
    drop(a); // A is lost, and so is the device.

    let read = cold_read_deployment_seeds(&*b, &IDENTITY_SEED)
        .await
        .expect("the cold read");
    assert_eq!(
        read,
        vec![entry.clone()],
        "the seed-only reader reads the box from B, a nest no device was ever bound to"
    );
}

/// The RecoveryKey registration chain `nest` holds for the account, as the
/// verbatim records it serves.
async fn registration_chain(nest: &Nest) -> Vec<Vec<u8>> {
    nest.state
        .db
        .list_recovery_registrations(&nest.actor())
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.record)
        .collect()
}

/// **The chain follows the link, pass by pass** (`identity-succession.md`
/// § Enforcement on the home nest → *Every nest the identity is linked to*,
/// clause (b)). The device is bound to A; B is linked as a replica and C for
/// mail alone, with no `account_replica` capability. A recovery kit made at A
/// after the links — and then replaced there — reaches B and C in the next
/// full pass, as the verbatim records A serves, over the connections the
/// secondary leg opens: a nest that holds an account and no chain can verify
/// no succession, whatever its pairing syncs.
///
/// Red-verified: with the chain step switched off in the pass's leg, the run
/// reports no reconcile at either nest and carries nothing.
#[tokio::test]
async fn a_recovery_kit_made_after_the_link_reaches_every_linked_nest_in_the_next_pass() {
    use fauna_client_recovery::chain_reconcile::{ChainReconcile, ChainSide};
    use fauna_client_recovery::{RecoveryClient, create_kit_with_root};
    use fauna_core::recovery::RecoveryKey;
    use fauna_sync_engine::linked_leg::LinkedChainOutcome;

    let tmp = tempfile::tempdir().unwrap();
    let (ka, kb, kc) = (deployment(0x66), deployment(0x77), deployment(0x78));
    let a = Nest::boot(&ka).await;
    let b = Nest::boot(&kb).await;
    let c = Nest::boot(&kc).await;
    link(&a, &kb, "https://b.test").await;
    a.state
        .db
        .store_pairing(
            &actor_bytes(),
            &id_of(&kc),
            &[fauna_protocol::pair::capability::MAIL_PULL.to_string()],
            None,
            Some("https://c.test"),
            None,
        )
        .await
        .unwrap();
    let bound = Bound::to(&a);
    let pin = Pin::on(&ka);
    let d = linked_device(
        tmp.path(),
        "d",
        &bound,
        &pin,
        Some(connector(vec![
            ("https://b.test", Arc::clone(&b), id_of(&kb)),
            ("https://c.test", Arc::clone(&c), id_of(&kc)),
        ])),
    )
    .await;
    let chains = |report: &PumpReport| -> Vec<LinkedChainOutcome> {
        report
            .linked
            .as_ref()
            .map(|p| p.chains.iter().map(|c| c.outcome.clone()).collect())
            .unwrap_or_default()
    };
    let reports = settle(&d, "before any kit").await;
    assert_eq!(
        chains(reports.last().unwrap()),
        vec![LinkedChainOutcome::Reconciled(ChainReconcile::InStep); 2],
        "no kit anywhere is the same chain at all three"
    );

    // The kit ceremony runs against the bound nest alone, as it does in an app.
    let recovery = RecoveryClient::new(bound.clone());
    let first = || RecoveryKey::from_bytes([0x21; 32]);
    create_kit_with_root(&recovery, &account(), None, first(), &[])
        .await
        .expect("the kit ceremony lands at the bound nest");
    assert!(registration_chain(&b).await.is_empty());

    let report = d.reconcile_now().await.expect("pass");
    assert_eq!(
        chains(&report),
        vec![
            LinkedChainOutcome::Reconciled(ChainReconcile::Extended {
                side: ChainSide::Linked,
                records: 1,
            });
            2
        ],
        "the next pass carries the new registration to both linked nests"
    );
    assert_eq!(
        linked_outcomes(&report).len(),
        1,
        "and only B, the replica, is completed"
    );
    let home = registration_chain(&a).await;
    assert_eq!(home.len(), 1);
    assert_eq!(registration_chain(&b).await, home);
    assert_eq!(registration_chain(&c).await, home);

    // A replacement under the first kit's authority is one more link, and it
    // follows the same way.
    create_kit_with_root(
        &recovery,
        &account(),
        Some(&first()),
        RecoveryKey::from_bytes([0x22; 32]),
        &[],
    )
    .await
    .expect("the kit is replaced at the bound nest");
    d.reconcile_now().await.expect("pass");
    let home = registration_chain(&a).await;
    assert_eq!(home.len(), 2);
    assert_eq!(registration_chain(&b).await, home);
    assert_eq!(registration_chain(&c).await, home);

    let report = d.reconcile_now().await.expect("pass");
    assert_eq!(
        chains(&report),
        vec![LinkedChainOutcome::Reconciled(ChainReconcile::InStep); 2],
        "a carried chain is not carried again"
    );
    d.shutdown().await;
}

/// **A kit ceremony carries the chain at once** (`identity-succession.md`
/// § Enforcement on the home nest → *Every nest the identity is linked to*,
/// clause (b), second sentence). Same fleet as the pass-by-pass case above,
/// but the ceremony's caller wakes the runtime with
/// [`AccountStoreHandle::registration_chain_moved`] and the test never runs a
/// pass itself: a kit made — then replaced — at the bound nest is at the
/// linked nests once the wake has been served. `settled()` is the barrier: the
/// wake is queued ahead of it, so it answers only after the wake's pass.
///
/// Red-verified: with the wake not issued, the linked nests still hold no
/// chain at the barrier.
#[tokio::test]
async fn a_kit_ceremony_wakes_the_runtime_to_carry_the_chain_to_every_linked_nest() {
    use fauna_client_recovery::{RecoveryClient, create_kit_with_root};
    use fauna_core::recovery::RecoveryKey;

    let tmp = tempfile::tempdir().unwrap();
    let (ka, kb, kc) = (deployment(0x66), deployment(0x77), deployment(0x78));
    let a = Nest::boot(&ka).await;
    let b = Nest::boot(&kb).await;
    let c = Nest::boot(&kc).await;
    link(&a, &kb, "https://b.test").await;
    a.state
        .db
        .store_pairing(
            &actor_bytes(),
            &id_of(&kc),
            &[fauna_protocol::pair::capability::MAIL_PULL.to_string()],
            None,
            Some("https://c.test"),
            None,
        )
        .await
        .unwrap();
    let bound = Bound::to(&a);
    let pin = Pin::on(&ka);
    let d = linked_device(
        tmp.path(),
        "d",
        &bound,
        &pin,
        Some(connector(vec![
            ("https://b.test", Arc::clone(&b), id_of(&kb)),
            ("https://c.test", Arc::clone(&c), id_of(&kc)),
        ])),
    )
    .await;
    settle(&d, "before any kit").await;

    let recovery = RecoveryClient::new(bound.clone());
    let first = || RecoveryKey::from_bytes([0x21; 32]);
    create_kit_with_root(&recovery, &account(), None, first(), &[])
        .await
        .expect("the kit ceremony lands at the bound nest");
    assert!(registration_chain(&b).await.is_empty());
    d.registration_chain_moved();
    d.settled().await;
    let home = registration_chain(&a).await;
    assert_eq!(home.len(), 1);
    assert_eq!(
        registration_chain(&b).await,
        home,
        "the replica has the kit"
    );
    assert_eq!(
        registration_chain(&c).await,
        home,
        "so has the mail-only nest"
    );

    create_kit_with_root(
        &recovery,
        &account(),
        Some(&first()),
        RecoveryKey::from_bytes([0x22; 32]),
        &[],
    )
    .await
    .expect("the kit is replaced at the bound nest");
    d.registration_chain_moved();
    d.settled().await;
    let home = registration_chain(&a).await;
    assert_eq!(home.len(), 2);
    assert_eq!(registration_chain(&b).await, home);
    assert_eq!(registration_chain(&c).await, home);
    d.shutdown().await;
}

/// A seed-alone link for `nest`'s account: the next `seq` after the chain head
/// `nest` holds, signed by the identity and the new key alone — no prior-key
/// signature, the shape only a nest's own uncontested window lands.
async fn seed_alone_link(nest: &Nest, root: u8) -> Vec<u8> {
    use fauna_core::recovery::{RecoveryKey, RecoveryKeyRegistration};
    let identity = nest.account();
    let head = nest
        .state
        .db
        .recovery_registration_head(&nest.actor())
        .await
        .unwrap()
        .expect("a chain head to replace");
    let new = RecoveryKey::from_bytes([root; 32]);
    let signed = RecoveryKeyRegistration {
        actor_id: identity.actor_id(),
        recovery_pubkey: new.public(),
        seq: head.seq + 1,
        created_at: fauna_core::data::Timestamp::now(),
    }
    .sign(identity.signing_key(), &new, None)
    .unwrap();
    fauna_core::encoding::canonical_encode(&signed).unwrap()
}

/// Land every window `nest` has parked, as its sweep does once the window has
/// run out uncontested (an injected clock: no wall-clock wait).
async fn land_windows(nest: &Nest) -> usize {
    let after_the_window = fauna_core::data::Timestamp::now_secs()
        + fauna_core::recovery::RECOVERY_REPLACE_GRACE_SECS as i64
        + 3_600;
    let (landed, cancelled) =
        fauna_nest::recovery_handlers::land_due_replacements(&nest.state, after_the_window)
            .await
            .unwrap();
    assert_eq!(cancelled, 0, "no window is cancelled at landing");
    landed
}

/// The chain outcomes of a pass's secondary leg, one per linked nest.
fn chain_outcomes(report: &PumpReport) -> Vec<fauna_sync_engine::linked_leg::LinkedChainOutcome> {
    report
        .linked
        .as_ref()
        .map(|p| p.chains.iter().map(|c| c.outcome.clone()).collect())
        .unwrap_or_default()
}

/// The linked readings the standing alert is fed from, for `actor`: per
/// linked nest, the key its window would land and when.
fn linked_readings(actor: &fauna_core::identity::ActorId) -> Vec<([u8; 32], String, i64)> {
    fauna_client_core::recovery_pending::linked_windows(actor)
        .into_iter()
        .map(|(nest, w)| (nest, w.new_recovery_pubkey_hex, w.lands_at))
        .collect()
}

/// **A seed-alone link is requested at every linked nest, each running its
/// own window** (`identity-succession.md` § Enforcement on the home nest →
/// *Every nest the identity is linked to*, **The chain follows the link**
/// clause (c)), over two real nests. The device is bound to A; B is linked and
/// holds A's chain. A seed-alone replacement is requested at A and lands there
/// after its window — a link no submit can carry, since it bears no prior-key
/// signature. The next pass sends it to B as
/// `fauna.recovery.replacement.request` instead: B's own handler parks it for
/// its own window, and the same pass reads that window back into the linked
/// readings the standing alert is fed from. B's chain moves only when its
/// window lands; then the two are in step and the reading clears.
///
/// The case's account is its own ([`Nest::boot_for`]): the linked readings
/// are process-wide, and every other case's pass rewrites the shared one's.
///
/// Red-verified: with the owed link not requested (the door's default), the
/// pass reports `requested: false` and B serves no window.
#[tokio::test]
async fn an_owed_seed_alone_link_is_requested_at_the_lagging_nest_and_its_window_read_back() {
    use fauna_client_recovery::chain_reconcile::{
        ChainReconcile, ChainSide, fetch_replacement_status, request_replacement_record,
    };
    use fauna_client_recovery::{RecoveryClient, create_kit_with_root};
    use fauna_core::identity::ActorId;
    use fauna_core::recovery::RecoveryKey;
    use fauna_sync_engine::linked_leg::LinkedChainOutcome;

    let tmp = tempfile::tempdir().unwrap();
    let (ka, kb) = (deployment(0x66), deployment(0x77));
    let a = Nest::boot_for(&ka, [0x45; 32]).await;
    let b = Nest::boot_for(&kb, [0x45; 32]).await;
    link(&a, &kb, "https://b.test").await;
    let bound = Bound::to(&a);
    let pin = Pin::on(&ka);
    let d = linked_device(
        tmp.path(),
        "d",
        &bound,
        &pin,
        Some(connector(vec![(
            "https://b.test",
            Arc::clone(&b),
            id_of(&kb),
        )])),
    )
    .await;
    let actor = ActorId(a.actor());
    settle(&d, "before any kit").await;

    create_kit_with_root(
        &RecoveryClient::new(bound.clone()),
        &a.account(),
        None,
        RecoveryKey::from_bytes([0x21; 32]),
        &[],
    )
    .await
    .expect("the kit ceremony lands at the bound nest");
    d.reconcile_now().await.expect("pass");
    assert_eq!(registration_chain(&b).await, registration_chain(&a).await);

    // The seed-alone replacement at A: requested, then landed by A's own sweep.
    request_replacement_record(&bound, &seed_alone_link(&a, 0x22).await)
        .await
        .expect("A parks the seed-alone replacement");
    assert_eq!(land_windows(&a).await, 1, "A lands it after its window");
    assert_eq!(registration_chain(&a).await.len(), 2);
    assert!(
        fetch_replacement_status(&*b).await.unwrap().is_none(),
        "nothing pends at B yet"
    );

    let report = d.reconcile_now().await.expect("pass");
    assert_eq!(
        chain_outcomes(&report),
        vec![LinkedChainOutcome::Reconciled(ChainReconcile::Owed {
            side: ChainSide::Linked,
            submitted: 0,
            requested: true,
        })],
        "the landed seed-alone link is owed at B, and requested there"
    );
    assert_eq!(
        registration_chain(&b).await.len(),
        1,
        "B's chain waits for B's own window"
    );
    let new_key = RecoveryKey::from_bytes([0x22; 32]).public();
    let window = fetch_replacement_status(&*b)
        .await
        .unwrap()
        .expect("B parked the owed link for its own window");
    assert_eq!(
        window.new_recovery_pubkey.as_ref(),
        new_key.as_slice(),
        "the window at B is for the very key A landed"
    );
    assert_eq!(
        linked_readings(&actor),
        vec![(id_of(&kb), hex::encode(new_key), window.lands_at)],
        "the same pass read B's window into the linked readings"
    );

    // The next pass requests it again, and B keeps the clock its first
    // request started.
    d.reconcile_now().await.expect("pass");
    assert_eq!(
        fetch_replacement_status(&*b)
            .await
            .unwrap()
            .map(|w| w.lands_at),
        Some(window.lands_at),
        "a replayed request keeps B's window"
    );

    // B's window lands: the two chains are one again, and nothing pends.
    assert_eq!(land_windows(&b).await, 1, "B lands it after its own window");
    assert_eq!(registration_chain(&b).await, registration_chain(&a).await);
    let report = d.reconcile_now().await.expect("pass");
    assert_eq!(
        chain_outcomes(&report),
        vec![LinkedChainOutcome::Reconciled(ChainReconcile::InStep)]
    );
    assert!(
        linked_readings(&actor).is_empty(),
        "a landed window is no longer a linked reading"
    );
    d.shutdown().await;
}

/// **Each linked nest's pending replacement feeds the alert**
/// (`identity-succession.md` § Enforcement on the home nest → *Every nest the
/// identity is linked to*, clause (c)), over two real nests: a seed-alone
/// replacement requested at the linked nest B alone — with no window at the
/// bound nest A, whose own status read would show nothing — is in the linked
/// readings after one pass. Its own account, as the case above.
///
/// Red-verified: with the leg's window read answering `Unread`, the readings
/// stay empty.
#[tokio::test]
async fn a_window_opened_at_a_linked_nest_alone_is_read_back_in_one_pass() {
    use fauna_client_recovery::chain_reconcile::{
        ChainReconcile, fetch_replacement_status, request_replacement_record,
    };
    use fauna_client_recovery::{RecoveryClient, create_kit_with_root};
    use fauna_core::identity::ActorId;
    use fauna_core::recovery::RecoveryKey;
    use fauna_sync_engine::linked_leg::LinkedChainOutcome;

    let tmp = tempfile::tempdir().unwrap();
    let (ka, kb) = (deployment(0x66), deployment(0x77));
    let a = Nest::boot_for(&ka, [0x46; 32]).await;
    let b = Nest::boot_for(&kb, [0x46; 32]).await;
    link(&a, &kb, "https://b.test").await;
    let bound = Bound::to(&a);
    let pin = Pin::on(&ka);
    let d = linked_device(
        tmp.path(),
        "d",
        &bound,
        &pin,
        Some(connector(vec![(
            "https://b.test",
            Arc::clone(&b),
            id_of(&kb),
        )])),
    )
    .await;
    let actor = ActorId(a.actor());
    settle(&d, "before any kit").await;
    create_kit_with_root(
        &RecoveryClient::new(bound.clone()),
        &a.account(),
        None,
        RecoveryKey::from_bytes([0x21; 32]),
        &[],
    )
    .await
    .expect("the kit ceremony lands at the bound nest");
    d.reconcile_now().await.expect("pass");
    assert_eq!(registration_chain(&b).await, registration_chain(&a).await);
    assert!(linked_readings(&actor).is_empty());

    // A seed thief's request, at B alone.
    let lands_at = request_replacement_record(&*b, &seed_alone_link(&b, 0x66).await)
        .await
        .expect("B parks the seed-alone replacement");
    assert!(
        fetch_replacement_status(&bound).await.unwrap().is_none(),
        "nothing pends at the bound nest"
    );

    let report = d.reconcile_now().await.expect("pass");
    assert_eq!(
        chain_outcomes(&report),
        vec![LinkedChainOutcome::Reconciled(ChainReconcile::InStep)],
        "a parked window is no link: the chains are still one"
    );
    assert_eq!(
        linked_readings(&actor),
        vec![(
            id_of(&kb),
            hex::encode(RecoveryKey::from_bytes([0x66; 32]).public()),
            lands_at
        )],
        "B's window is a linked reading after one pass"
    );
    d.shutdown().await;
}

/// **A linked nest the account administers is custodied without ever being
/// bound.** The device is bound to A; B is linked, and the account is on B's
/// admin roster. The pass's custody leg, over the secondary leg's connection,
/// asks B whether the account administers it, fetches B's deployment seed,
/// checks it against the pairing row's id and merges B's row — retried pass by
/// pass until a generation tip resolves to seal it under. A and the device are
/// lost; a reader holding only the identity seed reads B's own seed back from
/// B. (The case above is the other half: an account that does not administer
/// B captures nothing there.)
///
/// Red-verified: with the pass's custody join stubbed out, B's row never
/// appears in the device's fold.
#[tokio::test]
async fn a_linked_nest_the_account_administers_is_custodied_without_ever_being_bound() {
    use fauna_client_config::{DeploymentSeedCustody, LinkedCustodyLeg};

    let tmp = tempfile::tempdir().unwrap();
    let (ka, kb) = (deployment(0x66), deployment(0x77));
    let a = Nest::boot(&ka).await;
    let b = Nest::boot(&kb).await;
    b.admit_admin().await;
    link(&a, &kb, "https://b.test").await;
    let bound = Bound::to(&a);
    let pin = Pin::on(&ka);

    let d = linked_device(
        tmp.path(),
        "d",
        &bound,
        &pin,
        Some(connector(vec![(
            "https://b.test",
            Arc::clone(&b),
            id_of(&kb),
        )])),
    )
    .await;

    let custodied = |seeds: &[DeploymentSeedEntry]| {
        seeds
            .iter()
            .find(|e| e.nest_actor_id == id_of(&kb))
            .cloned()
    };
    let mut outcomes = Vec::new();
    let mut row = None;
    for _ in 0..PASS_BUDGET {
        let report = d.reconcile_now().await.expect("pass");
        outcomes.push(report.linked_custody);
        row = custodied(&d.deployment_seeds().await.unwrap());
        if row.is_some() {
            break;
        }
    }
    let row = row.unwrap_or_else(|| {
        panic!("B's seed was never custodied within {PASS_BUDGET} passes: {outcomes:?}")
    });
    assert_eq!(row.seed.to_array(), kb.to_bytes(), "B's own seed");
    assert_eq!(row.superseded_by, None);
    // The runtime's own prologue may have captured it before the first driven
    // pass, which then reports the row held.
    assert!(
        matches!(
            outcomes.last().and_then(|o| o.first()).map(|c| &c.leg),
            Some(LinkedCustodyLeg::Ran(run)) if matches!(
                run.custody,
                DeploymentSeedCustody::Captured | DeploymentSeedCustody::AlreadyCustodied
            )
        ),
        "the pass reports the custody: {outcomes:?}"
    );
    assert_eq!(
        d.deployment_seeds().await.unwrap().len(),
        1,
        "B, and only B: the account does not administer A"
    );

    // The row rests at A (the pass shipped it) and at B (the completion that
    // followed the capture); the steady state then makes no capture round trip.
    settle(&d, "after the linked capture").await;
    let report = d.reconcile_now().await.expect("pass");
    assert!(
        matches!(
            report.linked_custody.first().map(|c| &c.leg),
            Some(LinkedCustodyLeg::Ran(run))
                if run.custody == DeploymentSeedCustody::AlreadyCustodied
        ),
        "held on the next pass: {:?}",
        report.linked_custody
    );
    assert_eq!(b.devices().await, 0, "the device never registered on B");

    d.shutdown().await;
    drop(bound);
    drop(a); // A is lost, and so is the device.

    let read = cold_read_deployment_seeds(&*b, &IDENTITY_SEED)
        .await
        .expect("the cold read");
    assert_eq!(
        custodied(&read).map(|e| e.seed.to_array()),
        Some(kb.to_bytes()),
        "the seed-only reader reads B's own seed from B, a nest no device was ever bound to"
    );
}

/// A connection to the linked address that presents another identity than
/// the pairing row's is refused before any account data moves: B's impostor
/// receives nothing.
#[tokio::test]
async fn a_linked_address_answered_by_another_identity_is_never_completed() {
    let tmp = tempfile::tempdir().unwrap();
    let (ka, kb) = (deployment(0x66), deployment(0x77));
    let a = Nest::boot(&ka).await;
    let impostor = Nest::boot(&deployment(0x78)).await;
    link(&a, &kb, "https://b.test").await;
    let bound = Bound::to(&a);
    let pin = Pin::on(&ka);
    let d = linked_device(
        tmp.path(),
        "d",
        &bound,
        &pin,
        Some(connector(vec![(
            "https://b.test",
            Arc::clone(&impostor),
            id_of(&deployment(0x78)),
        )])),
    )
    .await;
    custody(&d, &custody_entry(0x60)).await;
    let report = d.reconcile_now().await.expect("pass");
    assert_eq!(
        linked_outcomes(&report),
        vec![LinkedOutcome::IdentityMismatch {
            presented: id_of(&deployment(0x78))
        }]
    );
    assert_eq!(impostor.wraps().await, 0, "no wrap reached the impostor");
    assert!(
        cold_read_deployment_seeds(&*impostor, &IDENTITY_SEED)
            .await
            .unwrap_or_default()
            .is_empty(),
        "no row reached the impostor"
    );
}

/// **A shred reaches a linked nest.** Two devices bound to A, B linked; the
/// first custodies a box (minting generation 1, completed at B), then signs
/// out. The departure supersedes generation 1 — its one member is gone — and
/// while B cannot be reached the survivor mints past it, re-seals, and A
/// shreds it: every retire of that shred lands at A alone. B comes back, and
/// after ONE secondary leg it serves no row sealed under generation 1 — rows
/// the departed device wrote, retired by the survivor — and holds no wrap of it.
///
/// Red-verified: without the shred widening (the mirrored retires only — none
/// this pass, the shred's having landed while B was away) generation 1's rows
/// stay live on B; without the holder sweep B keeps generation 1's wrap.
#[tokio::test]
async fn a_shred_reaches_a_linked_nest() {
    let tmp = tempfile::tempdir().unwrap();
    let (ka, kb) = (deployment(0x66), deployment(0x77));
    let a = Nest::boot(&ka).await;
    let b = Nest::boot(&kb).await;
    link(&a, &kb, "https://b.test").await;
    let bound = Bound::to(&a);
    let pin = Pin::on(&ka);
    let reachable = Arc::new(AtomicBool::new(true));
    let links = || {
        Some(gated_connector(
            vec![("https://b.test", Arc::clone(&b), id_of(&kb))],
            Arc::clone(&reachable),
        ))
    };

    let d1 = linked_device(tmp.path(), "d1", &bound, &pin, links()).await;
    custody(&d1, &custody_entry(0x61)).await;
    let d2 = linked_device(tmp.path(), "d2", &bound, &pin, links()).await;
    for _ in 0..3 {
        settle(&d2, "d2 joins").await;
        settle(&d1, "d1 sees d2").await;
    }
    let before = wrapped_generations(&b).await;
    assert_eq!(before.len(), 1, "B holds generation 1's wrap");
    let g1 = before[0];
    assert!(
        rows_sealed_under(&b, &g1).await > 0,
        "B serves rows sealed under generation 1"
    );

    // The minter — generation 1's one member — signs out, and B goes away.
    d1.shutdown_for_sign_out().await;
    reachable.store(false, Ordering::SeqCst);

    // The survivor's next tip-sealed write finds generation 1 naming a removed
    // member and mints past it (the first-need mint); drive it until it reads
    // generation 1 shredded, then on until A holds nothing of it.
    custody(&d2, &custody_entry(0x62)).await;
    let mut gone = Vec::new();
    let mut last = None;
    for _ in 0..PASS_BUDGET * 2 {
        let report = d2.reconcile_now().await.expect("pass");
        last = Some((report.generation_reclaim, report.errors.clone()));
        gone = shredded(&d2).await;
        if !gone.is_empty() {
            break;
        }
    }
    assert!(
        gone.contains(&g1),
        "the departure supersedes generation 1 and it shreds (shredded: {gone:?}, mints: {}, \
         last reclaim: {last:?})",
        mints(&d2).await
    );
    for _ in 0..3 {
        d2.reconcile_now().await.expect("pass");
    }
    assert_eq!(
        rows_sealed_under(&a, &g1).await,
        0,
        "the bound nest serves no row under the shredded generation"
    );
    assert!(
        rows_sealed_under(&b, &g1).await > 0 && wrapped_generations(&b).await.contains(&g1),
        "B, away, still holds generation 1"
    );

    // B is back: one pass.
    reachable.store(true, Ordering::SeqCst);
    let report = d2.reconcile_now().await.expect("pass");
    assert_eq!(
        rows_sealed_under(&b, &g1).await,
        0,
        "the linked nest serves no row under the shredded generation after one secondary leg \
         ({:?})",
        report.linked
    );
    assert!(
        !wrapped_generations(&b).await.contains(&g1),
        "the linked holder keeps no wrap of the shredded generation ({:?})",
        report.linked
    );
}

// ── The seed-leg role ────────────────────────────────────────────────────────

/// One machine as a desktop runs it once its user has signed in: the
/// **seedless sync agent**, holding the engine role, and a **signed-in app**
/// beside it on the same store dir and credential slot, which holds the seed,
/// the linked-nest connector and — never the engine role — the seed-leg role.
struct Desktop {
    agent: AccountStoreHandle,
    app: AccountStoreHandle,
}

/// Bring a desktop up on `base/<name>`. The slot a seedless host assembles
/// from is the one a sign-in populates, so a seed-holding runtime enrolls the
/// machine first — with NO linked-nest connector, so nothing it does reaches a
/// linked nest — runs `signed_in` (what the user did before the agent came
/// up), and exits; then the agent takes the engine role, and the app is
/// started beside it.
async fn desktop<F, Fut>(
    base: &Path,
    name: &str,
    bound: &Bound,
    pin: &Pin,
    linked: LinkedNestConnector<Bound>,
    signed_in: F,
) -> Desktop
where
    F: FnOnce(AccountStoreHandle) -> Fut,
    Fut: std::future::Future<Output = AccountStoreHandle>,
{
    let sign_in = signed_in(device(base, name, bound, pin).await).await;
    sign_in.shutdown().await;

    // The agent holds no seed, so no owner session: every connection it has
    // authenticates as the machine's store principal.
    let agent = runtime_as(
        base,
        name,
        &bound.as_machine_principal(base, name),
        pin,
        RuntimePrincipal::Seedless,
        None,
    )
    .await;
    let report = agent.reconcile_now().await.expect("the agent's pass");
    assert!(
        !report.skipped_non_holder && agent.is_engine_holder(),
        "the seedless agent holds the engine role"
    );
    assert!(
        report.linked.is_none() && report.generation_escrow_recovery.is_none(),
        "and runs no seed-only step: {report:?}"
    );
    let app = linked_device(base, name, bound, pin, Some(linked)).await;
    assert!(!app.is_engine_holder(), "the app beside it does not pump");
    Desktop { agent, app }
}

/// One whole round of a desktop: the agent's full pass, then the app's
/// `reconcile_now` — which must answer the role (`skipped_non_holder`) and
/// move no pass counter, whatever its seed pass did. The app's report.
async fn round(desktop: &Desktop) -> PumpReport {
    desktop
        .agent
        .reconcile_now()
        .await
        .expect("the agent's pass");
    let cycles = desktop.app.pump_cycles();
    let report = desktop
        .app
        .reconcile_now()
        .await
        .expect("the app's seed pass");
    assert!(
        report.skipped_non_holder && !desktop.app.is_engine_holder(),
        "the app never takes the engine role from the agent: {report:?}"
    );
    assert!(
        report.walk.is_none() && report.fleet_walk.is_none() && report.outbox.is_none(),
        "a seed pass walks no bound plane and drains no outbox: {report:?}"
    );
    assert_eq!(
        desktop.app.pump_cycles(),
        cycles,
        "a seed pass moves no pass counter"
    );
    report
}

/// The live fleet-scope rows `nest` serves, each as its writer and that
/// writer's own log coordinate.
async fn fleet_rows(nest: &Nest) -> BTreeSet<(String, i64)> {
    let reply: SyncChangesListReply = nest
        .request(
            "fauna.sync.changes.list",
            SyncChangesListRequest {
                item_class: Some("state-entry".into()),
                scope: Some(ACCOUNT_STATE_FLEET_SCOPE.into()),
                frontier: Some(Default::default()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    reply
        .changes
        .into_iter()
        .filter_map(|c| Some((c.origin_writer?, c.origin_seq?)))
        .collect()
}

/// **A linked nest is completed and custodied by an app that never pumps.**
/// The machine's engine role is the seedless agent's from the moment it comes
/// up; the signed-in app beside it holds the seed-leg role and runs its seed
/// pass. Nest B is linked to the account, which administers it. Nothing
/// reached B while the sign-in ran (it carried no connector). Then, without
/// the app ever holding the engine role: B's own deployment seed is custodied
/// over the secondary leg's connection, B is completed — rows, wraps, its
/// receipt in its own cell — and A and the machine are lost; a reader holding
/// only the identity seed reads from B both the box custodied at sign-in and
/// B's own seed.
///
/// Red-verified: with the seed pass not run (a seed-leg holder that is not the
/// engine holder answering as any other non-holder), B's seed is never
/// custodied and B holds no wrap.
///
/// **And the seed pass is a run of the pump for the change generation**
/// (`account-runtime.md` § Multi-instance concurrency → *The seed-leg
/// role*, part 5): the app runs no pass, so no pass counter moves, but the
/// seed pass that custodied B's seed changed an entry a read answers, and the
/// app's open Nests page is owed the repaint. The agent's commits land on its
/// own connection, so only the app's own runs can move the app's generation.
///
/// **And the seed pass reaches B inside half the store thread's stack.** Only
/// the app's connector reaches B, so the depth B's requests were polled at
/// ([`Nest::deepest_request`]) is the linked-nest leg's under a seed pass —
/// the path that aborted a debug app on std's 2 MiB default, and the one
/// `fauna_sync_engine::account_runtime`'s own depth test cannot drive (its
/// fake serves no linked nest). Measured 2026-10-01 on a debug build (aarch64
/// linux): 1,459,344 bytes at B, and 1,880,624 at A (the agent's full passes
/// and the app's bound-plane steps). The depth is read where the request is
/// made, so the in-process handler below it — which no app runs — is not in
/// it. Red-verified with `STORE_THREAD_STACK_BYTES` at 2 MiB (B's reading
/// against 1,048,576). The numbers' owner: `account-runtime.md`
/// § Implementation status today.
#[tokio::test]
async fn a_seed_holder_beside_a_seedless_engine_holder_completes_and_custodies_the_linked_nest() {
    use fauna_client_config::LinkedCustodyLeg;

    let tmp = tempfile::tempdir().unwrap();
    let (ka, kb) = (deployment(0x66), deployment(0x77));
    let a = Nest::boot(&ka).await;
    let b = Nest::boot(&kb).await;
    b.admit_admin().await;
    link(&a, &kb, "https://b.test").await;
    let bound = Bound::to(&a);
    let pin = Pin::on(&ka);
    let entry = custody_entry(0x63);

    let desktop = desktop(
        tmp.path(),
        "m",
        &bound,
        &pin,
        connector(vec![("https://b.test", Arc::clone(&b), id_of(&kb))]),
        |sign_in| {
            let entry = entry.clone();
            async move {
                custody(&sign_in, &entry).await;
                sign_in
            }
        },
    )
    .await;
    assert_eq!(b.wraps().await, 0, "nothing reached B during the sign-in");
    assert!(fleet_rows(&b).await.is_empty());

    let b_seed = |seeds: &[DeploymentSeedEntry]| {
        seeds
            .iter()
            .find(|e| e.nest_actor_id == id_of(&kb))
            .cloned()
    };
    let generation = desktop.app.change_generation();
    let mut reports = Vec::new();
    let mut row = None;
    for _ in 0..PASS_BUDGET {
        reports.push(round(&desktop).await);
        row = b_seed(&desktop.app.deployment_seeds().await.unwrap());
        if row.is_some() {
            break;
        }
    }
    let row = row.unwrap_or_else(|| {
        panic!("B's seed was never custodied within {PASS_BUDGET} rounds: {reports:?}")
    });
    assert_eq!(row.seed.to_array(), kb.to_bytes(), "B's own seed");
    assert!(
        desktop.app.change_generation() > generation,
        "the seed pass that custodied B's seed moved the app's change generation"
    );
    // Two more rounds: the deposit's receipt, written through the bound plane
    // and shipped by the seed pass's own publish step, reaches B by the next
    // leg's diff.
    round(&desktop).await;
    let report = round(&desktop).await;
    assert!(
        linked_outcomes(&report)
            .iter()
            .all(|o| matches!(o, LinkedOutcome::Completed(c) if c.errors.is_empty())),
        "the seed pass completes B cleanly: {:?}",
        report.linked
    );
    assert_eq!(linked_outcomes(&report).len(), 1, "B, and only B");
    assert!(
        matches!(
            report.linked_custody.first().map(|c| &c.leg),
            Some(LinkedCustodyLeg::Ran(_))
        ),
        "the custody arm rides the seed pass's leg: {:?}",
        report.linked_custody
    );
    assert!(
        report.errors.is_empty(),
        "a clean seed pass: {:?}",
        report.errors
    );
    assert_eq!(b.devices().await, 0, "no device ever registered on B");
    assert!(b.wraps().await > 0, "every generation is escrowed at B too");
    assert!(
        receipt_holders(&desktop.app).await.contains(&id_of(&kb)),
        "B's receipt is written in its own cell"
    );

    desktop.app.shutdown().await;
    desktop.agent.shutdown().await;
    for (what, depth) in [
        (
            "the linked nest (the seed pass's linked leg)",
            b.deepest_request(),
        ),
        ("the bound nest", a.deepest_request()),
    ] {
        assert!(
            depth > 0,
            "{what}: no request was polled on a store thread — the probe is dark"
        );
        assert!(
            depth <= STORE_THREAD_STACK_BUDGET,
            "{what}: a request is polled {depth} bytes below the store thread's entry — \
             expected <= {STORE_THREAD_STACK_BUDGET}, half of the thread's stated stack. The \
             poll frames above it grew (an unoptimized build sums every temporary of a long \
             `async fn` into its frame): box the future that grew, or split the function, \
             before raising the stack. Owner: \
             docs/goal/architecture/apps/native-async-execution.md § The rule."
        );
        // The measurement itself, for whoever re-reads it (`--nocapture`).
        eprintln!("store-thread stack depth at {what}: {depth} bytes");
    }
    drop(bound);
    drop(a); // A is lost, and so is the machine.

    let read = cold_read_deployment_seeds(&*b, &IDENTITY_SEED)
        .await
        .expect("the cold read");
    assert_eq!(
        b_seed(&read).map(|e| e.seed.to_array()),
        Some(kb.to_bytes()),
        "the seed-only reader reads B's own seed from B"
    );
    assert!(
        read.contains(&entry),
        "and the box custodied at sign-in: {read:?}"
    );
}

/// **The seed pass connects to the linked nest inside half the store thread's
/// stack — over a real connection.** The case above reaches B through an
/// in-process connector, which hands back a requester with no connection
/// under it. A signed-in app's connector is
/// `fauna_client_account_runtime::native_linked_nest_connector`: a second
/// `NestClient` to the linked address, whose `connect()` awaits the login mint
/// inline — so an anonymous connection is dialled, and its TLS and WebSocket
/// handshakes run, on the store thread under the pass. Here B listens on a
/// loopback port over TLS with a self-signed certificate
/// ([`Nest::boot_listening`]) and the app's connector is the production one
/// ([`dialling_connector`]); the same desktop as above — the seedless agent
/// holding the engine role, the app beside it running its seed pass — then
/// completes B and custodies its seed over that connection.
///
/// Three readings, each held to half the thread's stated stack: where the
/// connector is polled, the deepest stack page touched while it ran (the
/// handshakes' own frames, which no first-party probe sits under —
/// [`stack_pages`]; linux only), and where the leg's requests over the
/// connection are made. Measured 2026-10-01 on a debug build (aarch64 linux):
/// 1,321,376 bytes, 1,723,167 and 1,453,200 — the connect runs 401,791 bytes
/// under the point it is polled at. Red-verified with
/// `STORE_THREAD_STACK_BYTES` at 3 MiB: the connect's reading alone is over
/// 1,572,864, and nothing overflows. The numbers' owner: `account-runtime.md`
/// § Implementation status today.
#[tokio::test]
async fn a_seed_pass_connects_to_the_linked_nest_inside_half_the_store_threads_stack() {
    let tmp = tempfile::tempdir().unwrap();
    let (ka, kb) = (deployment(0x66), deployment(0x77));
    let a = Nest::boot(&ka).await;
    let (b, b_url) = Nest::boot_listening(&kb).await;
    b.admit_admin().await;
    link(&a, &kb, &b_url).await;
    let bound = Bound::to(&a);
    let pin = Pin::on(&ka);
    let connect = Arc::new(ConnectDepth::default());

    let desktop = desktop(
        tmp.path(),
        "m",
        &bound,
        &pin,
        dialling_connector(&b, &connect),
        |sign_in| async move {
            settle(&sign_in, "the sign-in").await;
            sign_in
        },
    )
    .await;
    assert_eq!(b.wraps().await, 0, "nothing reached B during the sign-in");

    let mut reports = Vec::new();
    let mut custodied = false;
    for _ in 0..PASS_BUDGET {
        reports.push(round(&desktop).await);
        custodied = desktop
            .app
            .deployment_seeds()
            .await
            .unwrap()
            .iter()
            .any(|e| e.nest_actor_id == id_of(&kb));
        if custodied {
            break;
        }
    }
    assert!(
        custodied,
        "B's seed was never custodied over the real connection within {PASS_BUDGET} rounds: \
         {reports:?}"
    );
    let report = round(&desktop).await;
    assert!(
        linked_outcomes(&report)
            .iter()
            .all(|o| matches!(o, LinkedOutcome::Completed(c) if c.errors.is_empty())),
        "the seed pass completes B cleanly over the real connection: {:?}",
        report.linked
    );
    assert_eq!(linked_outcomes(&report).len(), 1, "B, and only B");
    assert!(b.wraps().await > 0, "every generation is escrowed at B");

    desktop.app.shutdown().await;
    desktop.agent.shutdown().await;
    assert!(
        connect.connects.load(Ordering::SeqCst) > 0,
        "the production connector never connected"
    );
    let mut readings = vec![
        (
            "the linked nest's connector is polled",
            connect.at_connector.load(Ordering::SeqCst),
        ),
        (
            "the linked leg's requests over the connection are made",
            b.deepest_request(),
        ),
    ];
    if cfg!(target_os = "linux") {
        readings.push((
            "the linked nest's connect (login dial, TLS and WebSocket handshakes) reaches",
            connect.high_water.load(Ordering::SeqCst),
        ));
    }
    for (what, depth) in readings {
        assert!(
            depth > 0,
            "{what}: nothing was read on a store thread — the probe is dark"
        );
        assert!(
            depth <= STORE_THREAD_STACK_BUDGET,
            "{what} {depth} bytes below the store thread's entry — expected <= \
             {STORE_THREAD_STACK_BUDGET}, half of the thread's stated stack. Either the poll \
             frames above the linked leg grew, or the login dial under it did (an unoptimized \
             build sums every temporary of a long `async fn` into its frame): box the future \
             that grew, or split the function, before raising the stack. Owner: \
             docs/goal/architecture/apps/native-async-execution.md § The rule."
        );
        // The measurement itself, for whoever re-reads it (`--nocapture`).
        eprintln!("store-thread stack: {what} {depth} bytes below the thread's entry");
    }
}

/// **A rebuilt box beside a seedless engine holder.** The machine's engine
/// role is the seedless agent's, whose every connection authenticates as the
/// machine's store principal. The bound box is rebuilt — same identity, empty
/// database — so it holds no grant for that principal: the agent's handshake
/// is refused `not_registered`, its pass reaches nothing, and it cannot
/// register (registering needs the owner session). The signed-in app beside it
/// never pumps; its **seed pass** asks the box which replica it is over the
/// owner session, finds it is not the settled one, and registers the machine
/// there. It clears nothing and settles nothing: the agent's next pass, its
/// connection up again, runs the bind verification, settles the replica and
/// makes the box complete, and a reader holding only the identity seed reads
/// the custodied seed from it.
///
/// Red-verified: with no registration in the seed pass the rebuilt box never
/// holds the machine's grant, and the agent's passes stay cut off for every
/// round.
#[tokio::test]
async fn a_seed_holder_beside_a_seedless_engine_holder_registers_the_machine_at_a_rebuilt_nest() {
    let tmp = tempfile::tempdir().unwrap();
    let ka = deployment(0x66);
    let a = Nest::boot(&ka).await;
    let bound = Bound::to(&a);
    let pin = Pin::on(&ka);
    let entry = custody_entry(0x5E);

    let desktop = desktop(
        tmp.path(),
        "m",
        &bound,
        &pin,
        connector(Vec::new()),
        |sign_in| {
            let entry = entry.clone();
            async move {
                custody(&sign_in, &entry).await;
                sign_in
            }
        },
    )
    .await;
    settle(&desktop.agent, "the agent, before the rebuild").await;
    let minted = mints(&desktop.app).await;
    assert!(a.wraps().await > 0);

    // The rebuild: same identity, empty database, the account re-created.
    let rebuilt = Nest::boot(&ka).await;
    bound.rebind(&rebuilt);
    drop(a);

    // The agent alone is cut off, and stays so.
    let cut_off = desktop
        .agent
        .reconcile_now()
        .await
        .expect("the agent's pass");
    assert!(
        !cut_off.errors.is_empty() && cut_off.bind.is_none(),
        "the seedless agent reaches nothing on the rebuilt box: {cut_off:?}"
    );
    assert_eq!(rebuilt.devices().await, 0, "and cannot register there");

    let mut app_reports = Vec::new();
    let mut agent_reports = Vec::new();
    for _ in 0..PASS_BUDGET {
        app_reports.push(seed_pass_alone(&desktop).await);
        let report = desktop
            .agent
            .reconcile_now()
            .await
            .expect("the agent's pass");
        let done = report.errors.is_empty()
            && matches!(
                report.bind,
                Some(BindPass::Settled | BindPass::Verified { settled: true })
            );
        agent_reports.push(report);
        if done {
            break;
        }
    }
    assert_eq!(
        rebuilt.devices().await,
        1,
        "the app's seed pass registered the machine on the rebuilt box; the app's reports: \
         {app_reports:?}"
    );
    assert_eq!(
        app_reports.first().and_then(|r| r.enrollment),
        Some(EnrollmentPass::Registered),
        "by its first seed pass, over the owner session"
    );
    assert!(
        app_reports.iter().all(|r| r.bind.is_none()) && !desktop.app.is_engine_holder(),
        "the seed pass settles no replica and the app never pumps: {app_reports:?}"
    );
    assert!(
        verified_and_settled(&agent_reports),
        "the agent's pass verifies the rebuilt box and settles it: {:?}",
        agent_reports
            .iter()
            .map(|r| (r.bind, &r.errors))
            .collect::<Vec<_>>()
    );
    assert!(
        agent_reports
            .last()
            .is_some_and(|r| r.errors.is_empty() && !r.skipped_non_holder),
        "and ends on a clean pass: {:?}",
        agent_reports.last()
    );
    assert!(
        rebuilt.wraps().await > 0,
        "the holdings check re-deposited the wraps"
    );
    assert_eq!(mints(&desktop.app).await, minted, "a rebuild mints nothing");

    desktop.app.shutdown().await;
    desktop.agent.shutdown().await;

    let read = cold_read_deployment_seeds(&*rebuilt, &IDENTITY_SEED)
        .await
        .expect("the cold read");
    assert_eq!(
        read,
        vec![entry],
        "the seed-only reader reads the box from the rebuilt A"
    );
}

/// The retire record of the machine `name`'s store, read off the store dir by
/// a handle of the test's own — the store is multi-process, and this is one
/// more reader.
async fn retire_record(base: &Path, name: &str) -> Vec<fauna_account_store::types::IssuedRetire> {
    use fauna_account_store::backend::StoreBackend;
    let dir = StoreRoot::at(base.join(name).join("state"))
        .store_dir(&account().actor_id_hex())
        .unwrap();
    fauna_account_store::sqlite::SqliteBackend::open_existing(&dir)
        .unwrap()
        .expect("the machine's store exists")
        .issued_retires()
        .await
        .unwrap()
        .into_iter()
        .map(|(_, retire)| retire)
        .collect()
}

/// The app's `reconcile_now` alone — a seed pass with no agent pass ahead of it.
async fn seed_pass_alone(desktop: &Desktop) -> PumpReport {
    let report = desktop
        .app
        .reconcile_now()
        .await
        .expect("the app's seed pass");
    assert!(report.skipped_non_holder, "{report:?}");
    report
}

/// What the sign-out cases below share: a desktop on machine `m`, bound to A
/// with B linked, and a second device on a machine of its own that has joined
/// the account and been carried to B by the app's seed pass.
struct WithASecondDevice {
    a: Arc<Nest>,
    b: Arc<Nest>,
    desktop: Desktop,
    /// The second device: bound to A, no linked connector.
    device: AccountStoreHandle,
    /// Its writer id, as the feed names it.
    writer: String,
}

/// Bring [`WithASecondDevice`] up under `base`; B answers the desktop's
/// connector only while `b_reachable` holds.
async fn a_desktop_and_a_second_device(
    base: &Path,
    b_reachable: Arc<AtomicBool>,
) -> WithASecondDevice {
    let (ka, kb) = (deployment(0x66), deployment(0x77));
    let a = Nest::boot(&ka).await;
    let b = Nest::boot(&kb).await;
    link(&a, &kb, "https://b.test").await;
    let bound = Bound::to(&a);
    let pin = Pin::on(&ka);

    let desktop = desktop(
        base,
        "m",
        &bound,
        &pin,
        gated_connector(
            vec![("https://b.test", Arc::clone(&b), id_of(&kb))],
            b_reachable,
        ),
        |sign_in| async move {
            custody(&sign_in, &custody_entry(0x64)).await;
            sign_in
        },
    )
    .await;
    round(&desktop).await;
    let writers = |rows: BTreeSet<(String, i64)>| -> BTreeSet<String> {
        rows.into_iter().map(|(w, _)| w).collect()
    };
    let machine = writers(fleet_rows(&a).await);

    let device = device(base, "d", &bound, &pin).await;
    for _ in 0..3 {
        settle(&device, "the second device joins").await;
        settle(&desktop.agent, "the agent sees it").await;
        round(&desktop).await;
    }
    let joined: Vec<String> = writers(fleet_rows(&a).await)
        .difference(&machine)
        .cloned()
        .collect();
    let [writer] = joined.as_slice() else {
        panic!("exactly one new writer joined: {joined:?}");
    };
    WithASecondDevice {
        a,
        b,
        desktop,
        device,
        writer: writer.clone(),
    }
}

/// The live fleet-scope rows `nest` serves under `writer`.
async fn rows_of(nest: &Nest, writer: &str) -> BTreeSet<(String, i64)> {
    fleet_rows(nest)
        .await
        .into_iter()
        .filter(|(w, _)| w == writer)
        .collect()
}

/// **A retire the seedless holder sent reaches the linked nest.** A second
/// device joins the account and B, linked, is completed with its rows by the
/// desktop app's seed pass. The device signs out. The agent — the engine
/// holder, seedless, running no secondary leg — reclaims the departed device's
/// rows at A in its own passes, and each retire it sends rests in the store's
/// retire record. ONE seed pass of the app then re-issues at B every recorded
/// retire whose row B serves at the same coordinates, and clears the record.
///
/// (Coordinates exact here, ruling 5's claim. A row the departed device
/// superseded itself before the agent's retire — its enrollment, replaced by
/// its own removal row — B holds below the retired coordinate: ruling 6, the
/// next case.)
///
/// Red-verified: with the bound plane not recording its retires in the store,
/// the record is empty and B keeps serving the rows.
#[tokio::test]
async fn a_retire_the_seedless_holder_sent_reaches_the_linked_nest_through_the_stores_record() {
    let tmp = tempfile::tempdir().unwrap();
    let WithASecondDevice {
        a,
        b,
        desktop,
        device: d,
        writer: departing,
    } = a_desktop_and_a_second_device(tmp.path(), Arc::new(AtomicBool::new(true))).await;
    let departing = &departing;
    assert!(
        !rows_of(&b, departing).await.is_empty(),
        "the seed pass completed B with the second device's rows"
    );
    assert!(
        retire_record(tmp.path(), "m").await.is_empty(),
        "every seed pass so far cleared the record it read"
    );

    d.shutdown_for_sign_out().await;

    // The agent's passes alone: it reclaims the departed device's rows at A,
    // and what it sends rests in the store.
    for _ in 0..3 {
        desktop
            .agent
            .reconcile_now()
            .await
            .expect("the agent's pass");
    }
    let recorded: BTreeSet<(String, i64)> = retire_record(tmp.path(), "m")
        .await
        .iter()
        .map(|r| (r.writer.to_hex(), r.writer_seq as i64))
        .filter(|(w, _)| w == departing)
        .collect();
    assert!(
        !recorded.is_empty(),
        "the seedless holder's retires of the departed device's rows rest in the store's record"
    );
    assert!(
        fleet_rows(&a).await.is_disjoint(&recorded),
        "A serves none of the rows those retires named"
    );
    let owed: BTreeSet<_> = fleet_rows(&b)
        .await
        .intersection(&recorded)
        .cloned()
        .collect();
    assert!(
        !owed.is_empty(),
        "B, which no leg has reached since, still serves rows the record names: {recorded:?}"
    );

    // One seed pass.
    let report = seed_pass_alone(&desktop).await;
    let still: BTreeSet<_> = fleet_rows(&b).await.intersection(&owed).cloned().collect();
    assert!(
        still.is_empty(),
        "one seed pass re-issues the agent's retires at B; still served there: {still:?} \
         (of {owed:?}; leg: {:?})",
        report.linked
    );
    assert!(
        retire_record(tmp.path(), "m").await.is_empty(),
        "the leg run cleared the record it read"
    );

    desktop.app.shutdown().await;
    desktop.agent.shutdown().await;
}

/// The device-set rows a reader holding only the identity seed folds from
/// `nest` alone: `(cell key, value)`, live rows only.
async fn a_cold_readers_device_set(nest: &Nest) -> Vec<(String, Vec<u8>)> {
    let replica = ColdFleetReplica::open(
        nest,
        account().actor_id(),
        &BackupKey::derive(&IDENTITY_SEED),
        ColdKeySource::Seed(zeroize::Zeroizing::new(IDENTITY_SEED)),
    )
    .await
    .expect("the cold replica");
    replica.walk().await.expect("the cold walk");
    replica
        .states_of_kind(KIND_DEVICE_SET)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| !e.tombstone)
        .map(|e| (e.key, e.value))
        .collect()
}

/// Does a reader holding only the identity seed, reading `nest` alone, fold a
/// fleet in which `device` is a verified member?
async fn a_cold_reader_counts_a_member(nest: &Nest, device: &[u8; 32]) -> bool {
    let rows = a_cold_readers_device_set(nest).await;
    FleetView::build(
        &account().actor_id(),
        rows.iter()
            .map(|(key, value)| (key.as_str(), value.as_slice())),
    )
    .is_verified_member(device)
}

/// Does `nest` serve a device-set row of `device` — its enrollment, or the
/// evidence of its removal — to a reader holding only the identity seed?
async fn serves_a_device_set_row_of(nest: &Nest, device: &[u8; 32]) -> bool {
    let cell = fauna_core::hex32::encode(device);
    a_cold_readers_device_set(nest)
        .await
        .iter()
        .any(|(key, _)| *key == cell)
}

/// **A signed-out device is no member at a linked nest.** A second device
/// joins the account and B, linked, is completed with its rows. The device
/// signs out: it writes its removal over its own enrollment — same item, same
/// writer, so A collapses the enrollment under it. The agent's passes walk
/// the removal in and retire the departed device's other rows at A; the
/// removal row itself is evidence, which no pass retires and a seedless
/// process never does (`account-sync-plane.md` § The bind leg, ruling 7), so
/// A goes on serving it. ONE seed pass of the app pushes it to B — the
/// departing device's own removal row, the one exception to the diff's member
/// test — where it collapses the enrollment as it did at A, and the run's
/// removed-device arm, having found the row at the one replica A lists,
/// retires it at both nests: B serves no row of the departed device that A
/// does not, a reader holding only the identity seed, reading B alone, folds
/// a fleet without it, and neither nest serves a device-set row of it.
///
/// Red-verified (ruling 6, before ruling 7 was built): with the retire
/// mirrored by exact coordinates only, B kept serving the enrollment after the
/// seed pass. Under ruling 7, with the diff's exception reverted the removal
/// row never reaches B, which keeps serving the enrollment; with the arm
/// reverted both nests keep the removal row.
#[tokio::test]
async fn a_signed_out_devices_enrollment_is_retired_at_the_linked_nest_in_one_leg_run() {
    let tmp = tempfile::tempdir().unwrap();
    let WithASecondDevice {
        a,
        b,
        desktop,
        device: d,
        writer: departing,
    } = a_desktop_and_a_second_device(tmp.path(), Arc::new(AtomicBool::new(true))).await;
    let departed = fauna_core::hex32::decode(&departing).unwrap();
    assert!(
        a_cold_reader_counts_a_member(&b, &departed).await,
        "the seed pass completed B with the second device's enrollment"
    );

    d.shutdown_for_sign_out().await;

    // The agent's passes alone: the removal is walked in, and stays served —
    // a seedless process runs no leg, and retires no removal evidence.
    for _ in 0..3 {
        desktop
            .agent
            .reconcile_now()
            .await
            .expect("the agent's pass");
    }
    assert!(
        !a_cold_reader_counts_a_member(&a, &departed).await,
        "A serves no enrollment of the departed device"
    );
    assert!(
        serves_a_device_set_row_of(&a, &departed).await,
        "A keeps the removal evidence until a leg run has carried it"
    );

    // One seed pass.
    let report = seed_pass_alone(&desktop).await;
    let (at_a, at_b) = (rows_of(&a, &departing).await, rows_of(&b, &departing).await);
    assert!(
        at_b.is_subset(&at_a),
        "after one seed pass B serves no row of the departed device that A does not: \
         B {at_b:?}, A {at_a:?} (leg: {:?})",
        report.linked
    );
    assert!(
        !a_cold_reader_counts_a_member(&b, &departed).await,
        "a seed-only reader of B alone folds a fleet without the departed device"
    );
    assert!(
        !serves_a_device_set_row_of(&a, &departed).await
            && !serves_a_device_set_row_of(&b, &departed).await,
        "the leg run that carried the removal evidence to B retired it at both nests \
         (arm: {:?})",
        report.linked.as_ref().map(|l| &l.evidence)
    );

    desktop.app.shutdown().await;
    desktop.agent.shutdown().await;
}

/// **A retire whose record entry was spent while the linked nest was away
/// still reaches it — a round later.** The device signs out while B cannot be
/// reached. The agent retires its rows at A; the app's leg run reaches no
/// linked nest and clears the record it read, as every run does. B returns.
/// The first leg run to reach it has no entry to re-issue — and its reconcile
/// records the rows B still lists in the store's relay plane. The agent's next
/// pass names them again under the rule that retired them, A answers that they
/// are gone, and that answer is recorded at the coordinates B holds. The leg
/// run after it re-issues them (`account-sync-plane.md` § The bind leg,
/// ruling 6, the lost entry).
///
/// The rows this is proven on are the departed device's reach and
/// device-scoped rows, which the pass still retires and the record still
/// mirrors. Its device-set cell is ruling 7's and needs no round: the removal
/// row stayed served at A while B was away, and the first leg run to reach B
/// pushes it there, over the enrollment — B's reader folds a fleet without the
/// departed device from that run on.
///
/// Red-verified: with an answer of `Gone` not recorded, the second leg run has
/// nothing to re-issue and B keeps serving the departed device's rows.
#[tokio::test]
async fn a_retire_whose_record_entry_was_spent_while_the_linked_nest_was_away_reaches_it_a_round_later()
 {
    let tmp = tempfile::tempdir().unwrap();
    let b_reachable = Arc::new(AtomicBool::new(true));
    let WithASecondDevice {
        a,
        b,
        desktop,
        device: d,
        writer: departing,
    } = a_desktop_and_a_second_device(tmp.path(), Arc::clone(&b_reachable)).await;
    let departed = fauna_core::hex32::decode(&departing).unwrap();

    d.shutdown_for_sign_out().await;
    b_reachable.store(false, Ordering::SeqCst);
    for _ in 0..3 {
        desktop
            .agent
            .reconcile_now()
            .await
            .expect("the agent's pass");
    }
    let report = seed_pass_alone(&desktop).await;
    assert_eq!(
        linked_outcomes(&report),
        vec![LinkedOutcome::Unreachable],
        "the leg run reached no linked nest"
    );
    assert!(
        retire_record(tmp.path(), "m").await.is_empty(),
        "and cleared the record it read all the same: one leg run deep"
    );
    assert!(
        serves_a_device_set_row_of(&a, &departed).await
            && a_cold_reader_counts_a_member(&b, &departed).await,
        "a run that reached no replica retires no removal evidence: A still serves it, and B, \
         away, still holds the enrollment"
    );

    // B returns. The first leg run to reach it has no entry for its rows —
    // and carries the removal row, which needs none.
    b_reachable.store(true, Ordering::SeqCst);
    seed_pass_alone(&desktop).await;
    let stale: BTreeSet<_> = rows_of(&b, &departing)
        .await
        .difference(&rows_of(&a, &departing).await)
        .cloned()
        .collect();
    assert!(
        !stale.is_empty(),
        "B still serves rows of the departed device that A retired"
    );
    assert!(
        !a_cold_reader_counts_a_member(&b, &departed).await,
        "the removal row reached B in the first run to reach it"
    );

    // The agent's next pass re-makes the entries, at the coordinates B holds.
    desktop
        .agent
        .reconcile_now()
        .await
        .expect("the agent's pass");
    let remade: BTreeSet<(String, i64)> = retire_record(tmp.path(), "m")
        .await
        .iter()
        .map(|r| (r.writer.to_hex(), r.writer_seq as i64))
        .collect();
    assert!(
        stale.is_subset(&remade),
        "the pass asked A about every row the leg's reconcile recorded from B, and recorded \
         the answers: stale {stale:?}, record {remade:?}"
    );

    // The leg run after it.
    let report = seed_pass_alone(&desktop).await;
    let (at_a, at_b) = (rows_of(&a, &departing).await, rows_of(&b, &departing).await);
    assert!(
        at_b.is_subset(&at_a),
        "a round later B serves no row of the departed device that A does not: \
         B {at_b:?}, A {at_a:?} (leg: {:?})",
        report.linked
    );
    assert!(
        !a_cold_reader_counts_a_member(&b, &departed).await,
        "a seed-only reader of B alone folds a fleet without the departed device"
    );

    desktop.app.shutdown().await;
    desktop.agent.shutdown().await;
}

/// **Two seed holders on one store dir run one leg between them.** Two
/// signed-in apps on one machine, each with its own connector to the linked
/// nest. The first to assemble holds both roles and runs the secondary leg in
/// its pass; the second holds neither and runs none — its connector is never
/// called, and its `reconcile_now` answers the role with every slot empty.
/// When the first exits, the second takes both roles at its next
/// `reconcile_now` and runs the leg.
///
/// Red-verified: with the seed-leg role granted to every seed holder (no
/// election), the second app's connector is called and its report carries a
/// leg.
#[tokio::test]
async fn two_seed_holders_on_one_store_dir_run_one_secondary_leg_between_them() {
    let tmp = tempfile::tempdir().unwrap();
    let (ka, kb) = (deployment(0x66), deployment(0x77));
    let a = Nest::boot(&ka).await;
    let b = Nest::boot(&kb).await;
    link(&a, &kb, "https://b.test").await;
    let bound = Bound::to(&a);
    let pin = Pin::on(&ka);

    // A connector that counts the connections it opened.
    let counting = |opened: &Arc<AtomicUsize>| -> LinkedNestConnector<Bound> {
        let inner = connector(vec![("https://b.test", Arc::clone(&b), id_of(&kb))]);
        let opened = Arc::clone(opened);
        Arc::new(move |target: LinkedNestTarget| {
            opened.fetch_add(1, Ordering::SeqCst);
            inner(target)
        })
    };
    let (first_opened, second_opened) =
        (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));

    let first = linked_device(tmp.path(), "m", &bound, &pin, Some(counting(&first_opened))).await;
    custody(&first, &custody_entry(0x65)).await;
    let second = linked_device(
        tmp.path(),
        "m",
        &bound,
        &pin,
        Some(counting(&second_opened)),
    )
    .await;

    for _ in 0..3 {
        let report = first.reconcile_now().await.expect("the first app's pass");
        assert!(!report.skipped_non_holder);
        assert_eq!(
            linked_outcomes(&report).len(),
            1,
            "the holder of both roles runs the leg in its pass: {:?}",
            report.linked
        );
        let report = second
            .reconcile_now()
            .await
            .expect("the second app's answer");
        assert!(report.skipped_non_holder);
        assert!(
            report.linked.is_none()
                && report.linked_custody.is_empty()
                && report.generation_escrow_recovery.is_none(),
            "a seed holder without the seed-leg role runs no seed-only step: {report:?}"
        );
    }
    assert!(first_opened.load(Ordering::SeqCst) > 0);
    assert_eq!(
        second_opened.load(Ordering::SeqCst),
        0,
        "one leg between them: the second app never opened a connection to B"
    );

    // The roles transfer when their holder exits.
    first.shutdown().await;
    let report = second.reconcile_now().await.expect("the second app's pass");
    assert!(
        !report.skipped_non_holder,
        "the engine role is the second app's now"
    );
    assert_eq!(
        linked_outcomes(&report).len(),
        1,
        "and the seed-leg role with it: {:?}",
        report.linked
    );
    assert!(second_opened.load(Ordering::SeqCst) > 0);
    second.shutdown().await;
}

// ── A removal across two bound nests ─────────────────────────────────────────

/// The fleet id of the machine `base/<name>` — its store principal's writer
/// key, as its sign-in enrolled it.
fn fleet_id(base: &Path, name: &str) -> [u8; 32] {
    let credentials =
        CredentialStore::with_file_backend(CRED_NAMESPACE, base.join(name).join("creds"));
    fauna_sync_engine::principal_bundle::load_writer_key(&credentials, &account().actor_id_hex())
        .expect("the machine is enrolled")
        .verifying_key()
        .to_bytes()
}

/// Does a running device's own merged state fold a fleet in which `device` is
/// a verified member?
async fn counts_a_member(handle: &AccountStoreHandle, device: &[u8; 32]) -> bool {
    let rows = handle.states_of_kind(KIND_DEVICE_SET).await.unwrap();
    FleetView::build(
        &account().actor_id(),
        rows.iter()
            .filter(|e| !e.tombstone)
            .map(|e| (e.key.as_str(), e.value.as_slice())),
    )
    .is_verified_member(device)
}

/// What the cross-nest removal cases share: one account on two nests, each
/// linked at the other, and three seed-holding devices — `near` and `leaving`
/// bound to A, `far` bound to B — each of the two that stay completing the
/// nest it is not bound to over its own secondary leg.
struct TwoBoundNests {
    a: Arc<Nest>,
    b: Arc<Nest>,
    /// Bound to A, with B linked: the sibling that stays.
    near: AccountStoreHandle,
    /// Bound to A: the device that signs out, or is removed.
    leaving: AccountStoreHandle,
    /// Bound to B, with A linked: its only view of A is its own secondary leg.
    far: AccountStoreHandle,
    /// `leaving`'s fleet id.
    departing: [u8; 32],
}

/// Bring [`TwoBoundNests`] up under `base` and run it to a fleet every device
/// reads whole; B answers `near`'s connector only while `near_reaches_b` holds.
async fn three_devices_across_two_bound_nests(
    base: &Path,
    near_reaches_b: Arc<AtomicBool>,
) -> TwoBoundNests {
    let (ka, kb) = (deployment(0x66), deployment(0x77));
    let a = Nest::boot(&ka).await;
    let b = Nest::boot(&kb).await;
    link(&a, &kb, "https://b.test").await;
    link(&b, &ka, "https://a.test").await;
    let (at_a, at_b) = (Bound::to(&a), Bound::to(&b));
    let (pin_a, pin_b) = (Pin::on(&ka), Pin::on(&kb));

    let near = linked_device(
        base,
        "near",
        &at_a,
        &pin_a,
        Some(gated_connector(
            vec![("https://b.test", Arc::clone(&b), id_of(&kb))],
            near_reaches_b,
        )),
    )
    .await;
    custody(&near, &custody_entry(0x67)).await;
    let leaving = device(base, "leaving", &at_a, &pin_a).await;
    for _ in 0..3 {
        settle(&leaving, "the second device joins at A").await;
        settle(&near, "its sibling sees it, and carries it to B").await;
    }
    let far = linked_device(
        base,
        "far",
        &at_b,
        &pin_b,
        Some(connector(vec![(
            "https://a.test",
            Arc::clone(&a),
            id_of(&ka),
        )])),
    )
    .await;
    settle(&far, "the third device joins at B").await;
    let ids = [
        fleet_id(base, "near"),
        fleet_id(base, "leaving"),
        fleet_id(base, "far"),
    ];
    let devices = [&near, &leaving, &far];
    let mut whole = false;
    for _ in 0..PASS_BUDGET {
        for device in devices {
            settle(device, "the fleet converges across both nests").await;
        }
        whole = true;
        for device in devices {
            for id in &ids {
                whole &= counts_a_member(device, id).await;
            }
        }
        if whole {
            break;
        }
    }
    assert!(
        whole,
        "every device reads all three members within {PASS_BUDGET} rounds"
    );
    // Quiet: the wraps, reaches and receipts the join set off have landed.
    for _ in 0..3 {
        for device in devices {
            settle(device, "the fleet goes quiet").await;
        }
    }
    TwoBoundNests {
        a,
        b,
        near,
        leaving,
        far,
        departing: ids[1],
    }
}

/// Run the two devices that stay — the sibling at A first, alone, for as many
/// passes as take it quiet, then both in turn — and answer whether `far`
/// still counts `departing` a member after each of its own passes. The
/// ordering is the adverse one, and an ordinary one: the device bound to B is
/// simply not running while its sibling at A reads the removal and reclaims.
async fn far_reads_after(nests: &TwoBoundNests) -> Vec<bool> {
    for _ in 0..4 {
        nests.near.reconcile_now().await.expect("near's pass");
    }
    assert!(
        !counts_a_member(&nests.near, &nests.departing).await,
        "the sibling bound to the same nest reads the removal"
    );
    let mut reads = Vec::new();
    for _ in 0..PASS_BUDGET / 2 {
        nests.far.reconcile_now().await.expect("far's pass");
        reads.push(counts_a_member(&nests.far, &nests.departing).await);
        nests.near.reconcile_now().await.expect("near's pass");
    }
    reads
}

/// The end state ruling 7's *Proof* names: run the two devices that stay, in
/// turn, until neither nest serves a device-set row of the departed device —
/// the evidence left each nest behind that nest's own gate, once a leg run had
/// found it at the other — and assert both devices still read it removed.
async fn both_nests_shed_the_evidence(nests: &TwoBoundNests) {
    let mut served = (true, true);
    for _ in 0..PASS_BUDGET {
        served = (
            serves_a_device_set_row_of(&nests.a, &nests.departing).await,
            serves_a_device_set_row_of(&nests.b, &nests.departing).await,
        );
        if served == (false, false) {
            break;
        }
        nests.far.reconcile_now().await.expect("far's pass");
        nests.near.reconcile_now().await.expect("near's pass");
    }
    assert_eq!(
        served,
        (false, false),
        "neither nest (A, B) serves a device-set row of the departed device within \
         {PASS_BUDGET} further rounds"
    );
    // Quiet rounds: nothing brings the row back, and the removal stays read.
    for _ in 0..2 {
        nests.far.reconcile_now().await.expect("far's pass");
        nests.near.reconcile_now().await.expect("near's pass");
    }
    assert!(
        !serves_a_device_set_row_of(&nests.a, &nests.departing).await
            && !serves_a_device_set_row_of(&nests.b, &nests.departing).await,
        "and it stays gone from both"
    );
    assert!(
        !counts_a_member(&nests.near, &nests.departing).await
            && !counts_a_member(&nests.far, &nests.departing).await,
        "both remaining devices still read the departed device removed"
    );
}

/// **A sign-out at one nest is read by a device bound to the other.** Three
/// devices of one account, two nests each linked at the other: `leaving` and
/// its sibling `near` are bound to A, `far` to B. `leaving` signs out at A.
/// `near` reads the removal from A's feed; `far`, whose only views of A are B's
/// feed and its own secondary leg, must read it too
/// (`account-sync-plane.md` § The bind leg, ruling 7). `near`'s leg carries
/// the removal row to B — the departing device's own, the diff's one
/// exception to its member test — and both nests end without it.
///
/// Measured red 2026-10-01, before the ruling was built: `near`'s pass retired the removal row at
/// A and forgot it ahead of its own leg, no diff carried a departed writer's
/// row to B, and A's gate never waited for `far` — which counted the
/// signed-out device a member after every one of its passes. Red-verified
/// part by part: with the diff's exception reverted the row is never carried,
/// so neither nest sheds it; with the removed-device arm reverted both nests
/// keep it.
#[tokio::test]
async fn a_sign_out_at_one_nest_is_read_by_a_device_bound_to_the_other() {
    let tmp = tempfile::tempdir().unwrap();
    let nests =
        three_devices_across_two_bound_nests(tmp.path(), Arc::new(AtomicBool::new(true))).await;

    nests.leaving.shutdown_for_sign_out().await;

    let reads = far_reads_after(&nests).await;
    assert_eq!(
        reads.last(),
        Some(&false),
        "the device bound to B reads the sibling that signed out at A removed: {reads:?}"
    );
    both_nests_shed_the_evidence(&nests).await;
    nests.near.shutdown().await;
    nests.far.shutdown().await;
}

/// **The same, when the sibling cannot reach B.** `near` is away from the box
/// B runs on: its secondary leg reaches no linked nest. The removal must still
/// reach `far`, through `far`'s own leg at A — so A must still be serving the
/// evidence when that leg reads it (ruling 7: a leg run that did not reach
/// every linked replica retires no removal evidence). `far`'s own runs then
/// carry the row to B and retire it at both nests.
///
/// Measured red 2026-10-01, before the ruling was built: `near` retired the
/// removal row at A in its own pass, whether or not any leg had carried it.
/// Red-verified: with the pass retiring the evidence again, `far` never reads
/// the sign-out.
#[tokio::test]
async fn a_sign_out_is_read_across_nests_when_the_sibling_cannot_reach_the_other_nest() {
    let tmp = tempfile::tempdir().unwrap();
    let near_reaches_b = Arc::new(AtomicBool::new(true));
    let nests = three_devices_across_two_bound_nests(tmp.path(), Arc::clone(&near_reaches_b)).await;

    near_reaches_b.store(false, Ordering::SeqCst);
    nests.leaving.shutdown_for_sign_out().await;

    let reads = far_reads_after(&nests).await;
    assert_eq!(
        reads.last(),
        Some(&false),
        "the device bound to B reads the sign-out through its own leg at A: {reads:?}"
    );
    both_nests_shed_the_evidence(&nests).await;
    nests.near.shutdown().await;
    nests.far.shutdown().await;
}

/// **A removal at one nest is read by a device bound to the other.** As the
/// sign-out case, for a device that was lost: `near` removes `leaving` from
/// its Devices page — the nest row, then the fleet row, which is `near`'s own
/// and so one the diff pushes under its ordinary member test.
///
/// Measured red 2026-10-01, before the ruling was built: `near`'s pass retired
/// its removal row at A and forgot it before its leg's diff ran.
#[tokio::test]
async fn a_removal_at_one_nest_is_read_by_a_device_bound_to_the_other() {
    use fauna_core::fleet_removal::{NestDeletion, PendingFleetRemoval};

    let tmp = tempfile::tempdir().unwrap();
    let nests =
        three_devices_across_two_bound_nests(tmp.path(), Arc::new(AtomicBool::new(true))).await;

    // The device is lost: it never runs again. Its sibling at A removes it
    // from the Devices page — the nest row, then the fleet row.
    nests.leaving.shutdown().await;
    let row = format!("{:0<64}", hex::encode("leaving".as_bytes()));
    let targets = nests
        .near
        .resolve_fleet_removal(row.clone(), Some(nests.departing))
        .await
        .expect("the removal resolves");
    assert_eq!(targets, vec![nests.departing]);
    let removal = PendingFleetRemoval {
        row: row.clone(),
        targets,
    };
    nests
        .near
        .stage_fleet_removal(removal.clone())
        .await
        .expect("staged");
    let deleted = fauna_client_sync::SyncClient::new(Bound::to(&nests.a))
        .devices_delete(row)
        .await
        .expect("devices.delete");
    assert!(deleted.deleted);
    nests
        .near
        .settle_fleet_removal(removal, NestDeletion::Gone)
        .await
        .expect("settled");

    let reads = far_reads_after(&nests).await;
    assert_eq!(
        reads.last(),
        Some(&false),
        "the device bound to B reads the sibling removed at A removed: {reads:?}"
    );
    both_nests_shed_the_evidence(&nests).await;
    nests.near.shutdown().await;
    nests.far.shutdown().await;
}

/// What `nest`'s retention gate says of the fleet scope, read off its own feed
/// reply by a caller that leaves no mark: the gate's watermark — the lowest
/// counted walk mark, `None` when no mark counts — and the nest-log coordinate
/// of the newest live row.
async fn gate_of(nest: &Nest) -> (Option<i64>, i64) {
    let reply: SyncChangesListReply = nest
        .request(
            "fauna.sync.changes.list",
            SyncChangesListRequest {
                item_class: Some("state-entry".into()),
                scope: Some(ACCOUNT_STATE_FLEET_SCOPE.into()),
                frontier: Some(Default::default()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let tip = reply.changes.iter().map(|c| c.seq).max().unwrap_or(0);
    (reply.retirable_through_seq, tip)
}

/// **A lost device bound to one nest, removed from a device bound to the
/// other, stops counting at its own nest's gate.** `far` is the only device
/// bound to B and is lost: it never runs again. `near`, bound to A, removes it.
/// A's roster holds no row of it — a secondary leg registers no device
/// (`account-sync-plane.md` § The bind leg, ruling 4) — so it is an unaccounted
/// member there and goes by the member-addressed door, its fleet `Removed` row
/// alone. B must stop minting for its key, and its walk mark there must stop
/// counting: B's gate must not hold every later retire behind a device the
/// user has removed (`account-data-taxonomy.md` § The generation machinery →
/// *Fleet-scope reclamation*, clause (4) → *The nest half follows merged
/// state*).
///
/// **Measured red 2026-10-01, before that ruling was built**: after six
/// rounds, every one of `near`'s with a leg run that completed B, B still
/// minted for the removed key and its gate's watermark stood at the lost
/// device's mark under a tip that had moved past it — nothing nest-side was
/// sent to a nest the remover is not bound to. `near`'s secondary leg now
/// revokes the grant at B by key (`fauna_account_plane::removed_grants`, the
/// leg's step 5); reverting that step turns this red again. B keeps the row.
#[tokio::test]
async fn a_lost_device_removed_at_the_other_nest_stops_counting_at_its_own_nests_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let nests =
        three_devices_across_two_bound_nests(tmp.path(), Arc::new(AtomicBool::new(true))).await;
    let lost = fleet_id(tmp.path(), "far");
    assert!(
        nests.b.mints_for(&lost).await,
        "B holds the grant of the one device bound to it"
    );
    let (mark_before, tip_before) = gate_of(&nests.b).await;
    assert!(
        mark_before.is_some(),
        "B's gate counts the mark of the one device bound to it"
    );
    let rows_before = nests.b.devices().await;

    // The device is lost. Its sibling at A opens its Devices page: A's roster
    // accounts for no such member, so it is offered by key and removed by key.
    nests.far.shutdown().await;
    let roster = fauna_client_sync::SyncClient::new(Bound::to(&nests.a))
        .devices_list()
        .await
        .expect("devices.list")
        .devices
        .into_iter()
        .map(|d| {
            let claimed = d
                .principal
                .as_deref()
                .and_then(|p| fauna_core::hex32::decode(p).ok());
            (d.device_id, claimed)
        })
        .collect();
    let view = nests
        .near
        .unaccounted_fleet_members(roster)
        .await
        .expect("the member door's read");
    assert!(
        view.unaccounted.iter().any(|m| m.device_id == lost),
        "no row of A's roster accounts for the device bound to B: {view:?}"
    );
    nests
        .near
        .remove_fleet_member(lost)
        .await
        .expect("removed by key");

    // The devices that stay run until quiet, every pass of `near` with a leg
    // run that reaches B.
    for _ in 0..PASS_BUDGET / 2 {
        nests.near.reconcile_now().await.expect("near's pass");
        nests.leaving.reconcile_now().await.expect("leaving's pass");
    }
    assert!(
        !counts_a_member(&nests.near, &lost).await,
        "the remover reads the lost device removed"
    );

    let mints = nests.b.mints_for(&lost).await;
    let (mark, tip) = gate_of(&nests.b).await;
    assert!(
        !mints && mark.is_none(),
        "B stops minting for the removed device's key and its mark stops counting — \
         B is left with no counted walker, so its gate refuses nothing: mints {mints}, \
         watermark {mark:?} under a tip of {tip} (before the removal: watermark \
         {mark_before:?}, tip {tip_before}); B's roster rows: {}",
        nests.b.devices().await
    );
    assert_eq!(
        nests.b.devices().await,
        rows_before,
        "the grant is revoked by key; B keeps the row — its label and its places"
    );
    nests.near.shutdown().await;
    nests.leaving.shutdown().await;
}

/// **A lost device removed by key stops authenticating at the nest it shares
/// with its remover.** `leaving`, bound to A like `near`, is lost. `near`
/// removes it through the member-addressed door — its fleet `Removed` row
/// alone, no row gesture, so nothing nest-side is sent by the removal itself.
/// `near`'s next full pass reads A's roster, finds the row claiming the removed
/// key and revokes that grant by key (`account-data-taxonomy.md` § The
/// generation machinery → *Fleet-scope reclamation*, clause (4) → *The nest
/// half follows merged state*: the bound nest, in each full pass). A keeps the
/// row. Reverting the pump's removed-grants step turns this red.
#[tokio::test]
async fn a_lost_device_removed_by_key_stops_minting_at_the_nest_it_shares_with_its_remover() {
    let tmp = tempfile::tempdir().unwrap();
    let nests =
        three_devices_across_two_bound_nests(tmp.path(), Arc::new(AtomicBool::new(true))).await;
    let lost = nests.departing;
    assert!(
        nests.a.mints_for(&lost).await,
        "A holds the grant of the device bound to it"
    );
    let near_key = fleet_id(tmp.path(), "near");
    let rows_before = nests.a.devices().await;

    // The device is lost: it never runs again. Its sibling removes it by key.
    nests.leaving.shutdown().await;
    nests
        .near
        .remove_fleet_member(lost)
        .await
        .expect("removed by key");
    for _ in 0..4 {
        nests.near.reconcile_now().await.expect("near's pass");
    }
    assert!(
        !counts_a_member(&nests.near, &lost).await,
        "the remover reads the lost device removed"
    );

    assert!(
        !nests.a.mints_for(&lost).await,
        "A stops minting for the removed device's key"
    );
    assert!(
        nests.a.mints_for(&near_key).await,
        "the remover's own grant stands: its key is never named"
    );
    assert_eq!(
        nests.a.devices().await,
        rows_before,
        "the grant is revoked by key; A keeps the row"
    );
    nests.near.shutdown().await;
    nests.far.shutdown().await;
}

// ── A removal on the fleet plane that the nest half does not follow ──────────

/// The nest `sync_devices` row the machine `name` enrolls on
/// ([`runtime_as`]'s `enrollment_target_device_id`).
fn row_of(name: &str) -> String {
    format!("{:0<64}", hex::encode(name.as_bytes()))
}

/// Whether a device's merged state reads the custodied box `seed` — a
/// tip-sealed, fleet-only row, standing for every such kind.
async fn reads_seed(handle: &AccountStoreHandle, seed: u8) -> bool {
    let id = custody_entry(seed).nest_actor_id;
    handle
        .deployment_seeds()
        .await
        .unwrap()
        .iter()
        .any(|e| e.nest_actor_id == id)
}

/// The generations a device's merged state holds a live mint row for.
async fn minted(handle: &AccountStoreHandle) -> BTreeSet<String> {
    handle
        .states_of_kind(KIND_GENERATION_MINT)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| !e.tombstone)
        .map(|e| e.key)
        .collect()
}

/// The bound nest's roster as the Devices page hands it to the member door:
/// each row with the principal it claims.
async fn roster(bound: &Bound) -> Vec<(String, Option<[u8; 32]>)> {
    fauna_client_sync::SyncClient::new(bound.clone())
        .devices_list()
        .await
        .expect("devices.list")
        .devices
        .into_iter()
        .map(|d| {
            let claimed = d
                .principal
                .as_deref()
                .and_then(|p| fauna_core::hex32::decode(p).ok());
            (d.device_id, claimed)
        })
        .collect()
}

/// Two seed-holding devices of the account on `nest`, `first` enrolled and
/// writing under the tip before `second` exists — so the tip names `first`
/// alone and `second` keys it by top-up — run until each reads a fleet of two
/// and the first custodied seed, then quiet.
async fn two_devices(
    base: &Path,
    bound: &Bound,
    pin: &Pin,
    first: &str,
    second: &str,
) -> (AccountStoreHandle, AccountStoreHandle) {
    let one = device(base, first, bound, pin).await;
    custody(&one, &custody_entry(0x67)).await;
    let two = device(base, second, bound, pin).await;
    let ids = [fleet_id(base, first), fleet_id(base, second)];
    let mut whole = false;
    for _ in 0..PASS_BUDGET {
        settle(&two, "the second device joins").await;
        settle(&one, "the first device sees it").await;
        whole = reads_seed(&two, 0x67).await;
        for device in [&one, &two] {
            for id in &ids {
                whole &= counts_a_member(device, id).await;
            }
        }
        if whole {
            break;
        }
    }
    assert!(
        whole,
        "both devices read a fleet of two and the first seed within {PASS_BUDGET} rounds"
    );
    for _ in 0..3 {
        settle(&two, "the fleet goes quiet").await;
        settle(&one, "the fleet goes quiet").await;
    }
    (one, two)
}

/// What the guardian-marked cases share: a supervised account on one nest and
/// its two seed-holding devices — the guardian's enrolled one, which set the
/// account up and so is the member the generation tip names, and the ward's
/// own — with the guardian's nest row marked by the guardian
/// (`fauna.family.device.mark`).
struct MarkedFleet {
    nest: Arc<Nest>,
    bound: Bound,
    pin: Pin,
    ward: AccountStoreHandle,
    guardian: AccountStoreHandle,
    /// The fleet id the guardian's device enrolled under.
    marked: [u8; 32],
}

impl MarkedFleet {
    async fn up(base: &Path) -> Self {
        let key = deployment(0x66);
        let (nest, guardian_account) = Nest::boot_supervised(&key).await;
        let (bound, pin) = (Bound::to(&nest), Pin::on(&key));
        let (guardian, ward) = two_devices(base, &bound, &pin, "guardian", "ward").await;
        nest.dispatch(
            guardian_account,
            "fauna.family.device.mark",
            common::encode(&FamilyDeviceMarkRequest {
                supervised_actor_id: actor_bytes().to_vec().into(),
                device_id: row_of("guardian"),
                marked: true,
                extra: Default::default(),
            }),
        )
        .await
        .expect("the guardian marks its enrolled device");
        let fleet = Self {
            nest,
            bound,
            pin,
            ward,
            guardian,
            marked: fleet_id(base, "guardian"),
        };
        assert!(fleet.row_is_marked().await);
        fleet
    }

    /// Does the nest still list the guardian's row, marked?
    async fn row_is_marked(&self) -> bool {
        fauna_client_sync::SyncClient::new(self.bound.clone())
            .devices_list()
            .await
            .expect("devices.list")
            .devices
            .iter()
            .any(|d| d.device_id == row_of("guardian") && d.guardian_marked)
    }

    /// The ward's session registers a root-signed grant for `key` onto the
    /// guardian's marked row. The grant is signed by the account's seed, which
    /// the ward's device holds, so a modified app can sign one for any key.
    async fn the_ward_registers_over_the_marked_row(&self, key: &[u8; 32]) {
        let grant = fauna_client_sync::build_principal_grant(&account(), key)
            .expect("the account's seed signs a grant for any key");
        fauna_client_sync::SyncClient::new(self.bound.clone())
            .device_grant_register(row_of("guardian"), grant)
            .await
            .expect("no nest check stands between the account's session and the row's grant");
    }

    /// The guardian's device mints its store principal's next bearer. A box
    /// that holds no grant for its key refuses the handshake, and the refusal
    /// voids the slot's registration latch ([`MachinePrincipal`]).
    async fn the_guardians_principal_connects(&self, base: &Path) -> bool {
        fauna_client_sync::SyncClient::new(self.bound.as_machine_principal(base, "guardian"))
            .devices_list()
            .await
            .is_ok()
    }

    /// The ward's device writes the marked device's fleet `Removed` row by its
    /// key — the member door's writer, which a modified app calls for any
    /// member — and the ward's device runs until it reads it removed. The
    /// guardian's device runs no pass here: its next ones read the row and
    /// mint its successor, and each case says what holds before that.
    async fn the_ward_removes_the_marked_device_by_key(&self) {
        self.ward
            .remove_fleet_member(self.marked)
            .await
            .expect("no fleet-plane check stands between a member and the row");
        for _ in 0..3 {
            self.ward.reconcile_now().await.expect("the ward's pass");
        }
        assert!(
            !counts_a_member(&self.ward, &self.marked).await,
            "the ward's device reads the marked device removed"
        );
    }
}

/// **A ward's fleet removal of the guardian's enrolled device does not end its
/// read.** A supervised account, two devices, the guardian's enrolled one
/// marked. The nest refuses the row's deletion, and the stock Devices page
/// offers the device no other way — its roster row accounts for it. The ward's
/// device writes the fleet `Removed` row by key all the same: the plane has no
/// check, and the nest cannot make one over a sealed row. What that costs the
/// guardian's device is its membership, and nothing it reads
/// (`behavior/family-safety.md` § Full visibility for young children → *The
/// device marker*, the fleet-plane bound):
///
/// - the nest keeps the row, its mark and its grant, so the device goes on
///   authenticating;
/// - the ward's next write under the tip mints past the removed member, and
///   the guardian's device — which holds the account's seed, as every app
///   does — keys that generation from its escrow wrap, unasked, and reads the
///   row (`account-data-taxonomy.md` § The generation machinery → *Escrow
///   recovery*, (4): a removed seed holder keeps full account access);
/// - a row the guardian's device writes afterwards is read by the ward's.
///
/// The guardian's device also mints its successor in these same passes (the
/// case below); the first bullet is asserted before it runs one, and the
/// read is recovered from escrow whichever principal it holds by then.
///
/// Measured 2026-10-01.
#[tokio::test]
async fn a_wards_fleet_removal_of_the_guardian_marked_device_does_not_end_its_read() {
    use fauna_sync_engine::generation_escrow_recover::EscrowRecoveryPass;

    let tmp = tempfile::tempdir().unwrap();
    let fleet = MarkedFleet::up(tmp.path()).await;

    // The promise the nest keeps: the marked row is not the ward's to delete,
    // and no roster row leaves the device to the member door.
    let refused = fauna_client_sync::SyncClient::new(fleet.bound.clone())
        .devices_delete(row_of("guardian"))
        .await
        .expect_err("the ward cannot delete the guardian's marked row");
    assert!(
        matches!(&refused, Refused::Nest(e) if e.code == "fauna.sync.guardian_marked"),
        "refused as guardian-marked: {refused}"
    );
    let offered = fleet
        .ward
        .unaccounted_fleet_members(roster(&fleet.bound).await)
        .await
        .expect("the member door's read");
    assert!(
        !offered
            .unaccounted
            .iter()
            .any(|m| m.device_id == fleet.marked),
        "its own roster row accounts for the marked device: {offered:?}"
    );

    fleet.the_ward_removes_the_marked_device_by_key().await;
    assert!(
        fleet.nest.mints_for(&fleet.marked).await && fleet.row_is_marked().await,
        "the nest keeps what the marker protects: the row, its mark and its grant"
    );

    // The ward writes under the tip. The tip named the removed member, so the
    // write mints past it; the new generation wraps to the ward's device alone.
    let before = minted(&fleet.ward).await;
    custody(&fleet.ward, &custody_entry(0x68)).await;
    let after = minted(&fleet.ward).await;
    assert!(
        after.difference(&before).next().is_some(),
        "the ward's write minted a generation past the removed member: {before:?} → {after:?}"
    );

    // The guardian's device keys it from escrow and reads the row.
    let mut recovered = false;
    for _ in 0..PASS_BUDGET / 2 {
        let report = fleet
            .guardian
            .reconcile_now()
            .await
            .expect("the guardian's pass");
        recovered |= matches!(
            report.generation_escrow_recovery,
            Some(EscrowRecoveryPass::Recovered(_))
        );
        fleet.ward.reconcile_now().await.expect("the ward's pass");
        if reads_seed(&fleet.guardian, 0x68).await {
            break;
        }
    }
    assert!(
        recovered && reads_seed(&fleet.guardian, 0x68).await,
        "the guardian's device keys the later generation from its escrow wrap and reads \
         what the ward sealed under it: recovered {recovered}"
    );

    // And it still writes: the ward's device reads its row.
    fleet
        .guardian
        .merge_deployment_seeds(vec![custody_entry(0x69)])
        .await
        .expect("the guardian's device writes under the tip it recovered");
    for _ in 0..PASS_BUDGET / 2 {
        fleet
            .guardian
            .reconcile_now()
            .await
            .expect("the guardian's pass");
        fleet.ward.reconcile_now().await.expect("the ward's pass");
        if reads_seed(&fleet.ward, 0x69).await {
            break;
        }
    }
    assert!(
        reads_seed(&fleet.ward, 0x69).await,
        "the ward's device reads what the guardian's device wrote after the removal"
    );
    fleet.ward.shutdown().await;
    fleet.guardian.shutdown().await;
}

/// **The guardian's enrolled device, removed on the fleet plane, returns as a
/// successor.** As the case above, to the removal. The marker keeps the
/// device's grant at the nest, so the nest's revoked answer — the evidence a
/// removed seed-holding machine mints its successor principal on — never
/// comes. Its own device-set row reading `Removed` is evidence of the same
/// thing, held on the device: the running device mints a successor, enrolls
/// it on its marked row, and is a member again with no sign-out and nothing
/// the guardian has to notice (`account-replica-posture.md` § The store device
/// principal → *Principal succession after a device delete*, decision 1, the
/// third trigger).
///
/// The successor's grant goes on the same marked row, which carries one
/// grant: once it is registered the nest no longer mints for the removed key.
///
/// Measured red 2026-10-01 before the trigger existed: after six rounds the guardian's device
/// still held the removed principal, every pass of it answered its enrollment
/// `Current`, and the ward's device counted no member on that machine.
/// Red-verified against the build: with the assembly's probe deaf to the
/// pump's finding, the device re-registers the removed key and reassembles on
/// every pass without rotating — the case does not end.
#[tokio::test]
async fn the_guardian_marked_device_removed_on_the_fleet_plane_returns_as_a_successor() {
    let tmp = tempfile::tempdir().unwrap();
    let fleet = MarkedFleet::up(tmp.path()).await;
    fleet.the_ward_removes_the_marked_device_by_key().await;

    let mut verdicts = Vec::new();
    for _ in 0..PASS_BUDGET / 2 {
        let report = fleet
            .guardian
            .reconcile_now()
            .await
            .expect("the guardian's pass");
        verdicts.push(report.enrollment);
        fleet.ward.reconcile_now().await.expect("the ward's pass");
    }
    let successor = fleet_id(tmp.path(), "guardian");
    assert!(
        successor != fleet.marked
            && counts_a_member(&fleet.ward, &successor).await
            && fleet.nest.mints_for(&successor).await
            && fleet.row_is_marked().await,
        "the guardian's device is a member again under a fresh principal, on its marked row: \
         rotated {}, counted by the ward {}, minted for {}, row still marked {}; its \
         enrollment verdicts: {verdicts:?}",
        successor != fleet.marked,
        counts_a_member(&fleet.ward, &successor).await,
        fleet.nest.mints_for(&successor).await,
        fleet.row_is_marked().await
    );
    assert!(
        !fleet.nest.mints_for(&fleet.marked).await,
        "the marked row carries the successor's grant alone: the nest no longer mints for \
         the removed key"
    );
    assert!(
        !counts_a_member(&fleet.ward, &fleet.marked).await,
        "and the removed principal stays removed"
    );
    fleet.ward.shutdown().await;
    fleet.guardian.shutdown().await;
}

/// **A ward's replacement of the marked row's credential is taken back in one
/// pass.** The ward's device registers a grant for a key of its own onto the
/// guardian's marked row: the nest takes it (it cannot tell that write from
/// the guardian's own device registering its next principal), keeps the mark,
/// and stops minting for the guardian's key. The guardian's device does not
/// learn it from a pass — its registration latch still names the row — but
/// from its next bearer mint, which the nest refuses; the refusal voids the
/// latch, and the pass after that registers the same grant on the same row.
/// One write costs one registration, the principal unchanged
/// (`behavior/family-safety.md` § Full visibility for young children → *The
/// device marker*, the replacement bound).
///
/// Measured 2026-10-01.
#[tokio::test]
async fn a_wards_replacement_of_the_marked_rows_credential_is_taken_back_in_one_pass() {
    let tmp = tempfile::tempdir().unwrap();
    let fleet = MarkedFleet::up(tmp.path()).await;
    assert!(fleet.the_guardians_principal_connects(tmp.path()).await);

    let other = deployment(0x7A).verifying_key().to_bytes();
    fleet.the_ward_registers_over_the_marked_row(&other).await;
    assert!(
        !fleet.nest.mints_for(&fleet.marked).await
            && fleet.nest.mints_for(&other).await
            && fleet.row_is_marked().await,
        "the marked row carries the ward's key and keeps its mark"
    );

    // A pass alone does not find out: the latch is the device's memory of a
    // registration, and nothing has told it the memory is stale.
    let unaware = fleet
        .guardian
        .reconcile_now()
        .await
        .expect("the guardian's pass");
    assert_eq!(unaware.enrollment, Some(EnrollmentPass::Current));
    assert!(!fleet.nest.mints_for(&fleet.marked).await);

    // Its next mint is refused, and the pass after that takes the row back.
    assert!(!fleet.the_guardians_principal_connects(tmp.path()).await);
    let back = fleet
        .guardian
        .reconcile_now()
        .await
        .expect("the guardian's pass");
    assert_eq!(
        back.enrollment,
        Some(EnrollmentPass::Registered),
        "the first pass after the refused mint registers again"
    );
    assert!(
        fleet.nest.mints_for(&fleet.marked).await
            && !fleet.nest.mints_for(&other).await
            && fleet.row_is_marked().await
            && fleet_id(tmp.path(), "guardian") == fleet.marked,
        "the same principal, on the same marked row"
    );
    assert!(fleet.the_guardians_principal_connects(tmp.path()).await);
    fleet.ward.shutdown().await;
    fleet.guardian.shutdown().await;
}

/// **A marked device whose key was displaced and then revoked returns as a
/// successor.** Once the ward's key is on the marked row, the guardian's key
/// rides no marked row, so the session arm of `fauna.sync.device_grant.revoke`
/// tombstones it. The guardian's device then gets the answer a deleted device
/// gets — its registration is refused as revoked — and its next seed-holding
/// assembly mints a successor principal and registers it on its own row, the
/// marked one. The two calls cost one enrollment, which is what a fleet-plane
/// removal costs it (`behavior/family-safety.md` § Full visibility for young
/// children → *The device marker*, the replacement bound).
///
/// Measured 2026-10-01.
#[tokio::test]
async fn a_marked_devices_key_displaced_and_then_revoked_returns_as_a_successor() {
    let tmp = tempfile::tempdir().unwrap();
    let fleet = MarkedFleet::up(tmp.path()).await;

    let other = deployment(0x7A).verifying_key().to_bytes();
    fleet.the_ward_registers_over_the_marked_row(&other).await;
    fauna_client_sync::SyncClient::new(fleet.bound.clone())
        .device_grant_revoke(fauna_protocol::sync::DeviceGrantRevokeRequest {
            device_key: fauna_core::hex32::encode(&fleet.marked),
            timestamp_ms: None,
            nonce: None,
            signature: None,
            extra: Default::default(),
        })
        .await
        .expect("the displaced key rides no marked row, so the session arm revokes it");

    assert!(!fleet.the_guardians_principal_connects(tmp.path()).await);
    let removed = fleet
        .guardian
        .reconcile_now()
        .await
        .expect("the guardian's pass");
    assert_eq!(
        removed.enrollment,
        Some(EnrollmentPass::RemovedFromAccount),
        "the running device reads its principal revoked"
    );

    // The next seed-holding assembly on that machine is the ceremony.
    fleet.guardian.shutdown().await;
    let again = device(tmp.path(), "guardian", &fleet.bound, &fleet.pin).await;
    let successor = fleet_id(tmp.path(), "guardian");
    let mut member = false;
    for _ in 0..PASS_BUDGET / 2 {
        again.reconcile_now().await.expect("the guardian's pass");
        fleet.ward.reconcile_now().await.expect("the ward's pass");
        member = counts_a_member(&fleet.ward, &successor).await;
        if member {
            break;
        }
    }
    assert!(
        successor != fleet.marked
            && member
            && fleet.nest.mints_for(&successor).await
            && !fleet.nest.mints_for(&other).await
            && fleet.row_is_marked().await,
        "the guardian's device is back under a fresh principal on its marked row: rotated {}, \
         counted by the ward {member}, minted for {}, the ward's key displaced {}, row still \
         marked {}",
        successor != fleet.marked,
        fleet.nest.mints_for(&successor).await,
        !fleet.nest.mints_for(&other).await,
        fleet.row_is_marked().await
    );
    fleet.ward.shutdown().await;
    again.shutdown().await;
}

/// **A removal the tip does not name is minted past.** The first device wrote
/// under the tip before the second existed, so the tip's member set is the
/// first device alone and the second keyed it by top-up. The second is lost
/// and the first removes it. The removal unseats nothing — every member the
/// tip names is still a member — so the first-need trigger never fires, and
/// the removed device goes on holding the key every later row is sealed
/// under. The mint protocol's trigger (b) is what severs it: the remover
/// closes every generation the removed device may key, so no candidate
/// resolves and the next origination mints
/// (`account-data-taxonomy.md` § The generation machinery → *The mint
/// protocol, trigger (b)*).
///
/// Measured red 2026-10-01 before the closure was built: the
/// remover's next write under the tip sealed under the generation the
/// removed device keys, and no generation was minted. Red again with
/// `fleet_removal::write_removed`'s closure write taken out.
#[tokio::test]
async fn a_removal_the_tip_does_not_name_is_minted_past() {
    let tmp = tempfile::tempdir().unwrap();
    let key = deployment(0x66);
    let nest = Nest::boot(&key).await;
    let (bound, pin) = (Bound::to(&nest), Pin::on(&key));
    let (stays, lost) = two_devices(tmp.path(), &bound, &pin, "stays", "lost").await;
    let removed = fleet_id(tmp.path(), "lost");

    lost.shutdown().await;
    stays
        .remove_fleet_member(removed)
        .await
        .expect("removed by key");
    for _ in 0..3 {
        stays.reconcile_now().await.expect("pass");
    }
    assert!(!counts_a_member(&stays, &removed).await);

    let before = minted(&stays).await;
    custody(&stays, &custody_entry(0x68)).await;
    let after = minted(&stays).await;
    assert!(
        after.difference(&before).next().is_some(),
        "a generation the removed device does not key was minted before the next row was \
         sealed: {before:?} → {after:?}"
    );
    stays.shutdown().await;
}

// ── The seed-only floor: every kind the `__config` blob held ─────────────────
//
// `config-dissolution.md` § The `__config` dissolution schedule → *The closure
// order*, step (5): the test that lands before the CAS-blob bridge is deleted.
// The blob was the one place a device holding the identity seed alone could
// read the account's configuration back from. Every field of it now rests on
// the account-state plane instead: the delegable preference cluster at
// generation 0, and every other field as a tip-sealed kind, whose generation
// key a seed holder recovers from the escrow wrap the bound nest keeps. This
// case is that floor for the whole set at once, through real runtimes.

/// The source box a backup destination list is kept for — any box id; the
/// door does not check it against the bound nest.
const FLOOR_SOURCE_NEST: [u8; 32] = [0xA1; 32];

/// Store one record of the tip-sealed `kind` on `d` through the kind's own
/// door, and answer whether the floor owes it: `true` for every kind that was
/// a `UserConfig` field. **No wildcard arm, on purpose** — a new tip-sealed
/// kind cannot build until it is given a record here or a reason it has none.
async fn store_a_record_of(d: &AccountStoreHandle, kind: TipSealedKind) -> bool {
    use TipSealedKind as K;
    use fauna_core::data::Timestamp;
    let what = kind.kind();
    match kind {
        // Never a `UserConfig` field: the blob held none of these, so closing
        // it takes no floor from them. (The reception key has its own
        // seed-only cases in `conformance_account_state_walk.rs`.)
        K::DeviceEndpoints
        | K::CustodiesHeld
        | K::CustodianEndpoints
        | K::ShareEndpoints
        | K::GroupReceptionKey
        | K::GroupMachineryRoot
        | K::ContactOverlay
        // Born on the plane with the third-party kinds (S8a), never a blob
        // field.
        | K::KindManifest => return false,
        K::GroupShareCeremony => {
            d.merge_group_shares(fauna_core::group_ceremony::GroupShareConfig {
                initiated: vec![],
                invited: vec![fauna_core::group_ceremony::InvitedGroupShare {
                    scope_id: [0x71; 32],
                    initiator: fauna_core::identity::ActorId([0x72; 32]),
                    offer: vec![0x0F, 0x01],
                    updated_at: Timestamp(1_000),
                    ..Default::default()
                }],
            })
            .await
            .expect(what);
        }
        K::Backup => {
            d.write_backup_destinations(
                FLOOR_SOURCE_NEST,
                fauna_core::data::BackupConfig {
                    destinations: vec![fauna_core::data::BackupDestination {
                        destination_id: "dest-1".into(),
                        destination_nest_url: "https://backup.example".into(),
                        destination_actor_pubkey: [7u8; 32],
                        folder_name: "__mail".into(),
                        added_at: 0,
                        display_name: Some("The other box".into()),
                        ..Default::default()
                    }],
                },
            )
            .await
            .expect(what);
        }
        K::Dns => {
            d.write_dns(fauna_core::data::DnsConfig {
                credentials: vec![fauna_core::data::DnsProviderCredential {
                    provider_id: "hetzner".into(),
                    fields: vec![(
                        "api-token".into(),
                        fauna_core::secret::SecretString::from("t0k3n"),
                    )],
                    zones: vec![fauna_core::data::DnsZoneRef {
                        id: "z1".into(),
                        name: "example.com".into(),
                    }],
                    label: "Hetzner (example.com)".into(),
                    created_at: 1,
                }],
                managed_domains: std::collections::BTreeSet::from(["example.com".to_string()]),
                ..Default::default()
            })
            .await
            .expect(what);
        }
        K::Atproto => {
            d.put_app_credential(fauna_core::data::AtprotoAppCredential {
                credential_id: "ivory".into(),
                label: "Ivory".into(),
                secret: fauna_core::secret::SecretByteBuf::new(b"s".to_vec()),
                dm_allowed: false,
                created_at: 1,
            })
            .await
            .expect(what);
        }
        K::SuccessionLedger => {
            let me = account().actor_id();
            d.merge_succession_ledger(
                me,
                fauna_core::succession_ledger::SuccessionLedger {
                    unattested_filter_marks: vec![fauna_core::data::FilterUnattestedMark {
                        filter_id: 42,
                        predecessor: fauna_core::identity::ActorId([0x55; 32]),
                        verdict: fauna_core::data::UnattestedVerdict::Open,
                    }],
                    ..fauna_core::succession_ledger::SuccessionLedger::empty(me)
                },
                Vec::new(),
            )
            .await
            .expect(what);
        }
        K::Subscriptions => {
            d.merge_subscriptions(common::held_period_key())
                .await
                .expect(what);
        }
        K::FolderKeys => {
            d.merge_folder_keys(common::folder_key_custody())
                .await
                .expect(what);
        }
        K::DeploymentSeeds => {
            d.merge_deployment_seeds(vec![custody_entry(0x5D)])
                .await
                .expect(what);
        }
        K::PeerAnchors => {
            let actor = fauna_core::identity::ActorId([0x61; 32]);
            d.merge_peer_anchors(fauna_core::data::PeerAnchors {
                chain_heads: vec![fauna_core::data::PeerChainHead {
                    actor,
                    recovery_pubkey: vec![0x10; 32],
                    seq: 3,
                    first_seen: Timestamp(50),
                    outrun: false,
                }],
                anchor_domains: vec![fauna_core::data::PeerAnchorDomain {
                    actor,
                    domain: "a.example".into(),
                    first_seen: Timestamp(60),
                }],
            })
            .await
            .expect(what);
        }
        K::BlessedNests => {
            d.set_nest_blessed([0xB1; 32], true, 1_700_000_000)
                .await
                .expect(what);
        }
        K::AtprotoIdentity => {
            d.merge_atproto_identity(fauna_core::data::AtprotoIdentityConfig {
                rotation_keys: vec![common::senior_rotation_key()],
                ..Default::default()
            })
            .await
            .expect(what);
        }
        K::Mail => {
            use fauna_core::data::{MailCredential, MailCredentialKind, MsekFingerprint};
            let msek = fauna_core::secret::SecretArray32::new([0x6d; 32]);
            d.write_mail_state(fauna_core::mail_rows::MailStateRow {
                msek: Some(msek.clone()),
                mail_enabled: Some(true),
                ..Default::default()
            })
            .await
            .expect(what);
            d.put_mail_credential(MailCredential {
                credential_id: "default".into(),
                display_name: "Default".into(),
                kind: MailCredentialKind::Plain,
                secret: b"correct horse battery staple".to_vec().into(),
                created_at: 1_700_000_000,
                updated_at: Timestamp::default(),
                wrapped_under: Some(MsekFingerprint::of(&msek)),
                revoked_at_unix: None,
                burned: None,
            })
            .await
            .expect(what);
        }
        K::Follows => {
            d.put_follow(fauna_core::data::FollowedFolder {
                home_nest_url: "https://home.example".into(),
                owner_actor_id: "ab".repeat(32),
                folder_id: 7,
                display_name: "Photos".into(),
                ..Default::default()
            })
            .await
            .expect(what);
        }
        K::CustodyCeremony => {
            d.merge_custody(fauna_core::custody_ceremony::CustodyConfig {
                granted: vec![fauna_core::custody_ceremony::GrantedCustody {
                    grant_id: vec![0x1D; fauna_core::custody_grant::CUSTODY_GRANT_ID_LEN],
                    host: [2u8; 32],
                    channel_hex: "aa".repeat(32),
                    offer: vec![0xF0, 0x1D],
                    updated_at: Timestamp(1_000),
                    ..Default::default()
                }],
                held: vec![],
            })
            .await
            .expect(what);
        }
        K::NostrConfirmation => {
            d.confirm_nostr_npub(1_700_000_000).await.expect(what);
        }
        K::RefusedSchedulingChanges => {
            d.write_refused_scheduling_changes(
                fauna_sync_engine::refused_change_rows::RefusedChangeWrite::Record(Box::new(
                    fauna_core::data::RefusedSchedulingChange {
                        uid_hash: format!("{:064x}", 1u64),
                        author: Some(format!("{:064x}", 2u64)),
                        author_home_nest_url: String::new(),
                        sender_address: String::new(),
                        method: "CANCEL".into(),
                        reason: "not_the_organizer".into(),
                        summary: "Kickoff".into(),
                        first_refused_at: 90,
                        last_refused_at: 100,
                        occurrences: 1,
                        dismissed_through: 0,
                        extra: Default::default(),
                    },
                )),
            )
            .await
            .expect(what);
        }
    }
    true
}

/// One live row of a kind as a device's merged state holds it: key, value,
/// and the merge metadata (a latest-wins kind's stamp).
type LiveRow = (String, Vec<u8>, Option<Vec<u8>>);

/// The live rows of `kind` in a device's merged state, by key.
async fn live_rows(handle: &AccountStoreHandle, kind: &str) -> Vec<LiveRow> {
    let mut rows: Vec<LiveRow> = handle
        .states_of_kind(kind)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| !e.tombstone)
        .map(|e| (e.key, e.value, e.merge_meta))
        .collect();
    rows.sort();
    rows
}

/// What every read door of the floor's tip-sealed kinds answers on `handle`,
/// by kind — each fold rendered, so two devices' answers compare as a whole.
async fn every_fold(handle: &AccountStoreHandle) -> Vec<(&'static str, String)> {
    use fauna_protocol::merge_policy as k;
    let me = account().actor_id();
    macro_rules! fold {
        ($read:expr) => {
            format!("{:?}", $read.await.expect("a local read"))
        };
    }
    vec![
        (k::KIND_GROUP_SHARE_CEREMONY, fold!(handle.group_shares())),
        (
            k::KIND_BACKUP,
            fold!(handle.backup_state(FLOOR_SOURCE_NEST)),
        ),
        (k::KIND_DNS, fold!(handle.dns())),
        (k::KIND_ATPROTO, fold!(handle.atproto())),
        (
            k::KIND_SUCCESSION_LEDGER,
            fold!(handle.succession_ledger(me)),
        ),
        (k::KIND_SUBSCRIPTIONS, fold!(handle.subscriptions())),
        (k::KIND_FOLDER_KEYS, fold!(handle.folder_keys())),
        (k::KIND_DEPLOYMENT_SEEDS, fold!(handle.deployment_seeds())),
        (k::KIND_PEER_ANCHORS, fold!(handle.peer_anchors())),
        (k::KIND_BLESSED_NESTS, fold!(handle.blessed_nests())),
        (k::KIND_ATPROTO_IDENTITY, fold!(handle.atproto_identity())),
        (k::KIND_MAIL, fold!(handle.mail())),
        (k::KIND_FOLLOWS, fold!(handle.follows())),
        (k::KIND_CUSTODY_CEREMONY, fold!(handle.custody())),
        (
            k::KIND_NOSTR_CONFIRMATION,
            fold!(handle.npub_confirmed_at()),
        ),
        (
            k::KIND_REFUSED_SCHEDULING_CHANGES,
            fold!(handle.refused_scheduling_changes()),
        ),
    ]
}

/// **The seed-only floor, every kind at once.** A device stores one record of
/// every kind the `__config` blob used to hold — the four delegable
/// preference records through `put_preference`, and each tip-sealed kind
/// through its own door — and is then lost with no sign-out. A fresh device
/// holding the identity seed and nothing else signs in against the same nest
/// and reads all of it back: the same rows, byte for byte, and the same answer
/// from every read door.
///
/// The preference rows are compared with their stamps: a row that reached the
/// fresh device any way but the plane's walk — the CAS-blob bridge's import,
/// while it still existed — would carry a stamp minted there, under that
/// device's writer id.
///
/// Red-verified by withholding the escrow wraps (every wrap deleted from the
/// nest before the fresh device signs in): it then reads the four preference
/// records and none of the sixteen tip-sealed kinds.
#[tokio::test]
async fn a_fresh_device_holding_only_the_seed_reads_every_kind_the_config_blob_held() {
    let tmp = tempfile::tempdir().unwrap();
    let key = deployment(0x66);
    let nest = Nest::boot(&key).await;
    let (bound, pin) = (Bound::to(&nest), Pin::on(&key));

    let d = device(tmp.path(), "d", &bound, &pin).await;
    // The first tip-sealed write waits for the first-need mint; the rest find
    // the tip it left.
    custody(&d, &custody_entry(0x5D)).await;
    let cluster = common::preference_cluster();
    let mut kinds: Vec<&'static str> = Vec::new();
    for (kind, value) in &cluster {
        d.put_preference(*kind, value.clone())
            .await
            .expect("a preference save");
        kinds.push(kind);
    }
    for kind in TipSealedKind::ALL {
        if store_a_record_of(&d, kind).await {
            kinds.push(kind.kind());
        }
    }
    assert_eq!(
        kinds.len(),
        cluster.len() + 16,
        "the four preference kinds and the sixteen kinds of `config-dissolution.md`'s kinds table"
    );
    settle(&d, "after the writes").await;

    let mut stored = Vec::new();
    for kind in &kinds {
        let rows = live_rows(&d, kind).await;
        assert!(!rows.is_empty(), "the device stored a {kind} record");
        stored.push((*kind, rows));
    }
    let folds = every_fold(&d).await;
    assert_eq!(
        folds.iter().map(|(kind, _)| *kind).collect::<BTreeSet<_>>(),
        kinds[cluster.len()..].iter().copied().collect(),
        "every tip-sealed kind of the floor is read through its door"
    );
    d.shutdown().await;
    drop(d); // Total device loss: no sign-out, nothing left but the seed.

    let f = device(tmp.path(), "f", &bound, &pin).await;
    let mut unread: Vec<&'static str> = Vec::new();
    for _ in 0..PASS_BUDGET {
        f.reconcile_now().await.expect("pass");
        unread.clear();
        for (kind, rows) in &stored {
            // The preference cluster with its stamps; a CRDT kind's metadata
            // is the replica's own bookkeeping.
            let held = live_rows(&f, kind).await;
            let read = if cluster.iter().any(|(k, _)| k == kind) {
                held == *rows
            } else {
                held.iter()
                    .map(|(key, value, _)| (key, value))
                    .eq(rows.iter().map(|(key, value, _)| (key, value)))
            };
            if !read {
                unread.push(kind);
            }
        }
        if unread.is_empty() {
            break;
        }
    }
    assert!(
        unread.is_empty(),
        "the seed-only device reads every kind's rows back; not read within {PASS_BUDGET} \
         passes: {unread:?}"
    );
    assert_eq!(
        every_fold(&f).await,
        folds,
        "every read door answers on the seed-only device as it did on the lost one"
    );
    // Closure step (6)'s floor clause: neither device read the account
    // through the retired `__config` rail — every kind above came off the
    // plane alone.
    assert!(
        !nest.served("fauna.config.get"),
        "no device asked the box for `fauna.config.get`"
    );
    f.shutdown().await;
}
