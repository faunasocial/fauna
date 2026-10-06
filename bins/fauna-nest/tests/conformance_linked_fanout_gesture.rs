//! **The seed-alone gesture's fan-out over two real nests** (tier_3) —
//! real-wire, two-nest, *client-driven*. Proves `identity-succession.md`
//! § Enforcement on the home nest → *Every nest the identity is linked to*,
//! clause (c) — the seed-alone replacement is requested at every linked nest,
//! and the veto contests the window at every nest that holds one — through
//! the shared composition every app's Settings gesture runs
//! (`fauna_client_recovery::linked_fanout`), over the native dial the native
//! apps hand it (`linked_fanout::native::NativeLinkedNestDial`).
//!
//! Topology — two in-process nests on distinct sockets with distinct nest
//! identities (the pairing-client harness of
//! `conformance_cross_nest_pairing_client.rs`). One identity is registered on
//! both, holds the same recovery chain at both, and its bound nest A carries a
//! pairing row naming B at B's real address.
//!
//! What only this suite catches over the crate's `FakeNest` cases
//! (`libs/fauna-client-recovery/tests/ceremonies.rs`): the real
//! `fauna.pair.list` reply naming the linked nest, the dial's real connects
//! (owner-authenticated for the request, anonymous for the veto), the
//! bound-identity check over each connection
//! (`fauna_client::trust::connection_bound_identity` — on this plaintext rig
//! the possession proof, the same check the dial runs everywhere: no pin, no
//! trust seed, nothing weakened), and each nest's own window read back through
//! its own `fauna.recovery.replacement.status`.
//!
//! The owed-link request (a seed-alone link landed at one nest only, requested
//! at the lagging nest by the secondary leg) is witnessed over two real nests
//! by `conformance_account_plane_bind.rs`.

mod common;
use common::connected_client;

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_recovery::chain_reconcile::fetch_replacement_status;
use fauna_client_recovery::linked_fanout::native::NativeLinkedNestDial;
use fauna_client_recovery::linked_fanout::{
    LinkedNestOutcome, request_seed_alone_replacement_everywhere,
};
use fauna_client_recovery::status::{RecoveryKitStatus, veto_with_status};
use fauna_client_recovery::{RecoveryClient, create_kit_with_root, request_seed_alone_replacement};
use fauna_core::identity::ActorKeypair;
use fauna_core::recovery::RecoveryKey;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;

/// The recovery root both nests' chains are registered under — the kit the
/// owner holds and vetoes with.
const KIT_ROOT: [u8; 32] = [0x21; 32];

/// One of the user's nests: auth-bootstrap, anonymous discovery, the pairing
/// kinds and the recovery kinds, `require_registration` semantics through the
/// registered user, and a distinct nest identity. Returns `(http_base,
/// state)`.
async fn start_nest() -> (String, Arc<AppState>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let authority = format!("127.0.0.1:{}", addr.port());

    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity::generate()),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::pair_handlers::register_pair_handlers(&mut b);
            fauna_nest::recovery_handlers::register_recovery_handlers(&mut b);
            b.build()
        }),
        auth: AuthState {
            token_store: Arc::new(TokenStore::new()),
            registration: RegistrationConfig {
                handle_domain: Some(authority.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
        ..AppState::for_test(db)
    });

    let app = fauna_nest::build_router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{authority}"), state)
}

/// Two linked nests and one account: the account is a user on both, holds
/// the [`KIT_ROOT`] chain at both, and its bound nest A's pairing row names B
/// at B's real address — the shape the Nests page's link leaves.
struct Rig {
    a: Arc<NestClient>,
    b: Arc<NestClient>,
    b_state: Arc<AppState>,
    identity: ActorKeypair,
}

impl Rig {
    async fn boot() -> Self {
        let (a_base, a_state) = start_nest().await;
        let (b_base, b_state) = start_nest().await;
        let identity = ActorKeypair::generate();
        let actor = identity.actor_id();
        for state in [&a_state, &b_state] {
            state
                .db
                .create_user(&actor.0, "free", "alice")
                .await
                .unwrap();
        }
        let a = connected_client(&a_base, clone(&identity)).await;
        let b = connected_client(&b_base, clone(&identity)).await;
        for nest in [&a, &b] {
            create_kit_with_root(
                &RecoveryClient::new(Arc::clone(nest)),
                &identity,
                None,
                RecoveryKey::from_bytes(KIT_ROOT),
                &[],
            )
            .await
            .expect("the kit ceremony registers the chain");
        }
        a_state
            .db
            .store_pairing(
                &actor.0,
                &b_state.nest_identity.public_key_bytes(),
                &fauna_protocol::pair::default_self_sync(),
                None,
                Some(&b_base),
                None,
            )
            .await
            .unwrap();
        Self {
            a,
            b,
            b_state,
            identity,
        }
    }

    fn bound(&self) -> RecoveryClient<Arc<NestClient>> {
        RecoveryClient::new(Arc::clone(&self.a))
    }

    fn dial(&self) -> NativeLinkedNestDial {
        NativeLinkedNestDial::new(&self.identity)
    }

    fn b_id(&self) -> [u8; 32] {
        self.b_state.nest_identity.public_key_bytes()
    }

    /// The window each nest serves on its own `fauna.recovery.replacement.status`:
    /// `(A's, B's)` — the new key each would land, and when.
    async fn windows(&self) -> (Option<([u8; 32], i64)>, Option<([u8; 32], i64)>) {
        async fn read(nest: &NestClient) -> Option<([u8; 32], i64)> {
            fetch_replacement_status(nest)
                .await
                .expect("the status read answers")
                .map(|w| {
                    let key: [u8; 32] = w.new_recovery_pubkey.as_ref().try_into().unwrap();
                    (key, w.lands_at)
                })
        }
        (read(&self.a).await, read(&self.b).await)
    }
}

fn clone(identity: &ActorKeypair) -> ActorKeypair {
    ActorKeypair::from_secret(*identity.secret_bytes())
}

/// The held kit, as the user pastes it into the veto field.
fn held_kit() -> String {
    RecoveryKey::from_bytes(KIT_ROOT).to_hex()
}

/// **The seed-alone request opens a window at every linked nest** (clause
/// (c)): requested at the bound nest A through the gesture, with the native
/// dial, B is listed from A's pairing rows, dialled as the owner, its binding
/// checked against the row's nest id, and asked with the same record — so A
/// and B each serve their own window for the same new key.
///
/// Red-verified: with `linked_fanout::linked_nests` answering no targets,
/// the fan-out reports no linked nest and B serves no window.
#[tokio::test]
async fn a_seed_alone_request_opens_its_window_at_the_bound_and_the_linked_nest() {
    let rig = Rig::boot().await;
    let (pending, fanout) =
        request_seed_alone_replacement_everywhere(&rig.bound(), &rig.identity, &rig.dial())
            .await
            .expect("the bound nest parks the request");

    assert_eq!(fanout.unlisted, None, "A's pairing rows were listed");
    assert_eq!(fanout.nests.len(), 1, "A's one pairing row names B");
    assert_eq!(fanout.nests[0].nest_id, rig.b_id());
    let LinkedNestOutcome::Answered(b_lands_at) = fanout.nests[0].outcome else {
        panic!("B answered the request: {:?}", fanout.nests[0].outcome);
    };

    let (a, b) = rig.windows().await;
    assert_eq!(
        a,
        Some((pending.recovery_pubkey, pending.lands_at)),
        "A runs the window the gesture answered with"
    );
    assert_eq!(
        b,
        Some((pending.recovery_pubkey, b_lands_at)),
        "B runs its own window for the very same key"
    );
}

/// **The veto clears the window at every nest that holds one** (clause (c)):
/// with the request open at A and B, the held kit's veto through
/// `veto_with_status` — at A over the bound session, at B over an anonymous
/// connection the native dial opens — leaves no window at either, and the
/// gesture's own status re-read shows nothing pending.
///
/// Red-verified: with `linked_fanout::linked_nests` answering no targets, the
/// request opens no window at B to clear (the veto reaches A alone).
#[tokio::test]
async fn the_veto_clears_the_window_at_the_bound_and_the_linked_nest() {
    let rig = Rig::boot().await;
    request_seed_alone_replacement_everywhere(&rig.bound(), &rig.identity, &rig.dial())
        .await
        .expect("the request opens both windows");
    let (a, b) = rig.windows().await;
    assert!(a.is_some() && b.is_some(), "both windows open: {a:?} {b:?}");

    let (cancelled, status) = veto_with_status(
        &rig.bound(),
        rig.identity.actor_id(),
        &held_kit(),
        &rig.dial(),
    )
    .await
    .expect("the bound nest accepts the veto");

    assert!(cancelled, "the veto cancelled a window");
    assert!(
        !matches!(status, RecoveryKitStatus::ReplacementPending(_)),
        "the gesture's status re-read shows nothing pending: {status:?}"
    );
    assert_eq!(rig.windows().await, (None, None), "no window at A or B");
}

/// **A window held only at the linked nest is cleared by the veto made at the
/// bound nest** (clause (c) — a seed thief's request sent to B alone): A has
/// nothing pending, so its own answer is `false`, yet the gesture reaches B
/// through A's pairing rows and cancels B's window, and reports that it
/// cancelled something.
///
/// Red-verified: with `linked_fanout::linked_nests` answering no targets,
/// `cancelled` is `false` and B's window stands.
#[tokio::test]
async fn a_window_held_only_at_the_linked_nest_is_cleared_by_the_veto_at_the_bound_nest() {
    let rig = Rig::boot().await;
    request_seed_alone_replacement(&RecoveryClient::new(Arc::clone(&rig.b)), &rig.identity)
        .await
        .expect("B parks a request made at B alone");
    let (a, b) = rig.windows().await;
    assert_eq!(a, None, "nothing pends at the bound nest");
    assert!(b.is_some(), "B holds the window");

    let (cancelled, _) = veto_with_status(
        &rig.bound(),
        rig.identity.actor_id(),
        &held_kit(),
        &rig.dial(),
    )
    .await
    .expect("the bound nest accepts the veto");

    assert!(cancelled, "the veto reached B and cancelled its window");
    assert_eq!(rig.windows().await, (None, None), "no window at A or B");
}
