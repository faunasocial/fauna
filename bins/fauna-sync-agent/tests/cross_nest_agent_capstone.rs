//! **The two-nest CLIENT-STACK capstone.**
//!
//! Four sessions landed the cross-nest writer client leg with green
//! unit tests and *no* end-to-end run. Each one closed its own task honestly
//! flagged "unit-pinned only". This module is where that stops: it drives the
//! whole chain — owner share on nest H, member accept on nest F, the agent's
//! own custody resolution of the foreign set, the REAL `fauna-sync-agent`
//! process, bytes both ways, and a demotion — and asserts the behaviour
//! `file-sync.md` § Multi-writer shared sets promises.
//!
//! ## What is real here
//!
//! * **Two real nests**, H (the set's home, where Alice owns it) and F (the
//!   member's own nest), as in-process `fauna-nest` routers on distinct loopback
//!   ports, federating over the plain-HTTP loopback carve-out
//!   (`federation_channel::validate_peer_url` — the same affordance
//!   `conformance_federation_channel` / `conformance_inbox` use).
//! * **The real agent binary** (`CARGO_BIN_EXE_fauna-sync-agent`) as a child
//!   process in a private world, provisioned and bound over the REAL per-user
//!   unix socket — the `agent_process_tier3` shape, one topology further out.
//! * **The agent's own resolution.** The capability carries the member's
//!   `BackupKey` and a bearer — no content key: the agent reads the member's own
//!   folder-key custody itself (the account plane's `fauna.state.folder-keys`,
//!   through the store it mounts as the machine's enrolled principal) and
//!   resolves the foreign set from its `ForeignFolder` custody record
//!   (`on-demand-files.md` § Shared sets on a capability host → *One
//!   mechanism*). The test runs the same producer over the same custody to name
//!   what the agent will serve; a hand-built `FolderEngineKeys` fixture would
//!   re-commit the exact substitution this task exists to catch.
//! * **The member's app.** The accept's custody write lands through a real
//!   account runtime on a machine of its own (`common::AppSeat`) — the plane's
//!   door, sealed under the account's generation. The owner is not the subject:
//!   her custody is an in-memory store with the door's semantics.
//! * **The real share/accept.** `FoldersAuthor::share_set` across nests and
//!   `join_folder_welcome` on the member side, so the `writer` grant is
//!   *carried* by the federation relay rather than asserted by the test.
//!
//! ## What is deliberately NOT covered (and where it is)
//!
//! * **The GUI render** of the revoked row (`folder-access-revoked-warning`) —
//!   linux-side, unit-pinned. This module asserts the state the row
//!   renders *from*, over the agent's own IPC (`LocationInfo::access_revoked`).
//! * **TLS trust.** Both nests are plain-HTTP loopback. A cross-nest byte plane
//!   dials the HOME nest directly and no handshake ever graduates an SPKI pin for
//!   it (the design relays every control-plane call through the member's own
//!   nest), so a **self-signed** home nest is refused by
//!   `store_pinned_reqwest_tls`'s `RequireWebPki` policy. That is a real and
//!   separate gap; papering over it
//!   here with `danger_accept_invalid_certs` (as the engine-direct conformance
//!   test does) would hide it, so this module simply does not exercise TLS and
//!   says so.
//! * **Post-bind live delivery to the member.** A cross-nest member has NO
//!   remote-change nudge — `notify_sync_changed`'s recipients are same-nest by
//!   construction, and a federated nudge kind is deliberately not ratified
//!   (`file-sync.md` § Remote-change nudge) — so a change the owner records
//!   AFTER the member's engine is up reaches the member on the rescan cadence
//!   (300 s: the phase-5 de-knob made the constant the only value).
//!   Sub-cadence post-bind latency is therefore not a promise this test may
//!   assert; the nudge plane has its own same-nest proof
//!   (`same_nest_push_nudge`). What IS promised — and asserted here — is the
//!   eager first pull: a freshly-bound member receives what is already in the
//!   set without waiting out a cadence (`file-sync.md` § Config status: *"the
//!   cadence governs the catch-up rhythm, never the first pull"*).
//!
//! ## Order is load-bearing (two rots this module caught in itself, 2026-08-20)
//!
//! This suite sat red-and-unwatched with "a different
//! failure stage each run", suspected load-sensitive. Both stages were
//! test-side defects; neither was the relay:
//!
//! * **The owner's seed upload happens BEFORE the member binds** (L3 before
//!   L4), so the bind's eager pull delivers it *causally*. The original order
//!   (bind first, upload after) made delivery a race between the member
//!   engine's startup pull and the owner's upload — no post-upload trigger
//!   exists in this topology below the 300 s tick (no cross-nest nudge, see
//!   above) — so the verdict flipped with machine load. Do not reorder back.
//! * **The owner's read-back matches rows by `path_hash` and OPENS the sealed
//!   label** — never by resting plaintext `path`. Post-S9-flip the nest rests
//!   no plaintext path on a sealed plane (`record_change_core`'s
//!   `rests_plaintext_paths`), so `changes.list` serves `path: None` here and
//!   a plaintext matcher hangs forever — which is how this suite sat
//!   deterministically red from the S9 flip until 2026-08-20 while nothing
//!   ran it. Opening the label under the owner's custody is also the richer
//!   assertion: a member sealing under its OWN BackupKey instead of the set's
//!   shared M2 key would produce an unopenable label — the silent
//!   substitution this capstone exists to catch.
//!
//! ## The in-process twin
//!
//! Every test stages the same world ([`stage`]), the share's access its one
//! input. The capstone then drives the real agent process; its twin builds the
//! member's engine in-process through the one builder
//! (`engine_lifecycle::build_engine`) with only his `BackupKey`, a bearer and
//! the machine principal his app enrolled — the capability host's shape,
//! custody read through a throwaway fleet replica (decision 1′) — and hydrates
//! and writes the foreign set from his custody record alone
//! (`on-demand-files.md` § Shared sets on a capability host → *One mechanism*,
//! question 2). The reader tests build the same host over a `reader` share
//! (decision 3): read-only, enumerating, refusing every write, and hydrating
//! under the read token its byte plane takes in place of the write token.
//!
//! ## Run
//!
//! ```text
//! cargo test -p fauna-sync-agent --features tier3-nest --test cross_nest_agent_capstone
//! ```

#![cfg(unix)]

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{
    AgentWorld, AppSeat, await_agent, await_file_content_within, await_serving_engine,
    connected_client, expect_ok, list_locations,
};
use fauna_client_folders::FolderKeyStore;
use fauna_core::identity::ActorKeypair;
use fauna_ipc::sync::{BearerToken, RequestMethod, SyncCapability};
use fauna_nest::routes::AppState;

const SET_NAME: &str = "xnest-docs";
const MEMBER_DEVICE: [u8; 32] = [0x0b; 32];
const FAR_FUTURE: u64 = u64::MAX / 2;

/// Every wait in this module is bounded (`testing.md` § point 9). Two processes,
/// two nests and a federation relay sit between a gesture and its effect.
///
/// The CAUSAL window: a generous ceiling on an event-driven chain whose trigger
/// has already fired — the eager bind-time pull of a seed the owner recorded
/// BEFORE the bind (measured ~0.5–3 s healthy). Deliberately far below the
/// 300 s rescan default: if the eager pull ever stops covering foreign-routed
/// sets again (the 300.1 s defect this module found at birth), delivery slips
/// to the first tick and this window REDs — its size is that regression's
/// guard.
const SYNC_WINDOW: Duration = Duration::from_secs(90);

/// The BACKSTOP window, for convergence legs whose only retry path is the
/// rescan tick (300 s default): the member's watcher→seal→mint→bytes→relay
/// upload (L5) and the demotion park (L6). Each chain is seconds when healthy,
/// but the ratified posture for a transiently-failed leg is "heals at the next
/// tick" (`file-sync.md`: a dropped event costs at most one rescan interval),
/// and the agent's WS session was observed churning mid-test (2026-08-20 run
/// log) — a sub-tick window turns that legal heal into a red. This one spans a
/// full tick plus margin, so the verdict is about *convergence* — the thing
/// the product promises — never scheduling luck. Deadline polls return on
/// arrival, so a healthy run spends seconds here, not minutes.
const BACKSTOP_WINDOW: Duration = Duration::from_secs(420);

// ─────────────────────────────────────────────────────────────────────────
// The two nests — `common::start_nest` (round 32): the member's byte plane
// talks to H's, and bytes that do not really move would make this whole
// capstone vacuous; plain HTTP on loopback is the federation relay's
// documented in-process peer affordance (see this module's docs on TLS).
// ─────────────────────────────────────────────────────────────────────────

// ─────────────────────────────────────────────────────────────────────────
// The owner's side
//
// Alice is NOT the subject of this capstone — the member's agent stack is — so
// her engine is built directly here. Her half is same-nest and already proven; what it must do is put real M2-sealed bytes on H for the
// member to pull, and read the member's back.
// ─────────────────────────────────────────────────────────────────────────

/// Everything the owner's half needs, in one value.
///
/// Grouped rather than passed positionally on purpose: the fields are three
/// `&str`-ish and two byte-blob neighbours, and transposing two of them would be
/// silent. That is the same footgun a prior fix removed from
/// `record_foreign_set` / `originate_welcome_deliver` by taking whole structs.
struct Owner {
    /// The HOME nest's base URL — where the set lives and its bytes are served.
    h_base: String,
    nest: Arc<fauna_client::NestClient>,
    secret: [u8; 32],
    /// The owner's folder-key custody.
    custody: Arc<dyn FolderKeyStore>,
    channel_id: [u8; 32],
    /// The set's MLS group id. The owner's own custody record
    /// (`FolderKeyCustody`) stores only `(channel_id, keys)` — the group id is
    /// public routing material, read here from the member's foreign record,
    /// which is the same group by construction (the reference conformance test
    /// does the same).
    group_id: Vec<u8>,
}

impl Owner {
    /// Alice's own engine over H, keyed from her custody copy of the set's M2
    /// content keys.
    async fn engine(
        &self,
        h_state: &Arc<AppState>,
        watch_dir: PathBuf,
    ) -> fauna_sync_engine::engine::SyncEngine {
        use fauna_client_folders::custody;
        use fauna_nest_http::{BearerSource, StaticBearer};

        let keypair = ActorKeypair::from_secret(self.secret);
        let held = self.custody.load().await.expect("alice's custody");
        let keys =
            custody::content_keys(&held, &self.channel_id).expect("owner staged genesis custody");
        let set_nonce = custody::set_nonce_for_channel(&held, &self.channel_id)
            .expect("the owner's custody holds the set's nonce");

        let device: [u8; 32] = [0x0a; 32];
        let token = h_state
            .auth
            .token_store
            .insert(keypair.actor_id(), 3600)
            .await;
        let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer(token));
        let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
            self.h_base.clone(),
            ActorKeypair::from_secret(self.secret),
            bearer,
            reqwest::Client::new(),
        ));

        {
            use fauna_protocol::RpcRequester;
            let _: fauna_protocol::sync::SyncRegisterReply = self
                .nest
                .request(
                    "fauna.sync.register",
                    fauna_protocol::sync::SyncRegisterRequest {
                        device_id: hex::encode(device),
                        label: "alice-dev".into(),
                        capabilities: "read,write".into(),
                        ..Default::default()
                    },
                )
                .await
                .expect("register the owner's device");
        }

        let engine = fauna_sync_engine::engine::SyncEngine::new(
            watch_dir,
            fauna_sync_engine::db::SyncDb::open_in_memory().unwrap(),
            fauna_sync_engine::nest_client::SyncClient::new(auth, &device),
            Some(SET_NAME.to_string()),
            device,
            None, // mls
            None, // epoch_secret
            None, // backup_key — None ⇒ the content-key seal path runs
            Some(self.group_id.clone()),
            Some(keys),
            fauna_core::format::ConflictPolicy::Auto,
            fauna_core::format::FormatRegistry::new(),
            fauna_sync_engine::ignore::IgnoreMatcher::default(),
            4,
            fauna_sync_engine::transfer::TransferPool::new(
                Arc::new(fauna_sync_engine::adaptive::AdaptiveConcurrency::fixed(4)),
                None,
            ),
            Arc::clone(&self.nest),
            fauna_sync_engine::config::SyncMode::Sync, // bidirectional test root
        );
        // A seed-holding device signs directly with the identity key — the nest
        // refuses an unsigned record.
        engine.set_change_signer(
            Some(Arc::new(
                fauna_protocol::sync_writer_sig::ChangeSigner::direct(&keypair),
            )),
            Some(set_nonce),
        );
        engine
    }

    /// Poll H's change log until the member's record lands, then read the bytes
    /// back under the shared content key — the owner's end of the round trip.
    async fn await_reads_back(&self, rel: &str, expected: &[u8], writer_hex: &str) {
        use fauna_client_folders::custody;
        use fauna_core::file_download::FileDownloadKeys;
        use fauna_protocol::RpcRequester;
        use fauna_protocol::sync::{SyncChangesListReply, SyncChangesListRequest};

        let held = self.custody.load().await.expect("alice's custody");
        let keys = custody::content_keys(&held, &self.channel_id).expect("owner custody");
        let download_keys = FileDownloadKeys {
            backup_key: None,
            mls_group_id: Some(self.group_id.clone()),
            content_keys: Some(keys.clone()),
            ..Default::default()
        };

        let deadline = Instant::now() + BACKSTOP_WINDOW;
        loop {
            let listed: SyncChangesListReply = self
                .nest
                .request(
                    "fauna.sync.changes.list",
                    SyncChangesListRequest {
                        // Schema 114 rests a sealed set's name NULL, so the owner
                        // addresses her own set by its hash, never its plaintext.
                        name_hash: Some(fauna_protocol::ByteBuf::from(
                            fauna_core::path_crypto::set_name_hash(SET_NAME).to_vec(),
                        )),
                        ..Default::default()
                    },
                )
                .await
                .expect("owner reads her set's change log");

            // Post-S9-flip the nest rests no plaintext path on a sealed plane:
            // `changes.list` serves `path: None` plus the sealed label, and
            // readers key on `path_hash` then open the seal (the engine's
            // `open_sealed_change_paths` is this matcher's production twin).
            // Matching on resting plaintext `path` here hangs forever — the
            // deterministic red this suite carried from the S9 flip.
            let want_hash = hex::encode(fauna_core::sync::path_hash(rel));
            if let Some(change) = listed.changes.iter().find(|c| c.path_hash == want_hash) {
                assert_eq!(
                    change.author_actor_id.as_deref(),
                    Some(writer_hex),
                    "the record must be nest-stamped to the cross-nest writer, never \
                     client-asserted"
                );
                // The member's sealed label must OPEN under the set's SHARED
                // custody: an unopenable label means the cross-nest writer
                // sealed under its own BackupKey instead of the set's M2
                // content key — the silent substitution this capstone hunts.
                let sealed = change.path_sealed.as_ref().expect(
                    "a sealed-plane change row must carry path_sealed \
                     (S9: the seal is the only label)",
                );
                match fauna_core::label_custody::render_path(
                    &download_keys,
                    Some(&sealed[..]),
                    "",
                    hex::decode(&change.path_hash).ok().as_deref(),
                    fauna_core::path_crypto::LabelField::SyncChangePath,
                ) {
                    fauna_core::path_crypto::SealedLabelRender::Sealed(p) if p == rel => {}
                    other => panic!(
                        "the member's sealed path label did not open to {rel:?} under \
                         the set's shared content key (got {other:?}) — the cross-nest \
                         writer sealed under the WRONG key"
                    ),
                }
                let manifest_hex = change
                    .manifest_hash
                    .clone()
                    .expect("a create carries a manifest");
                let digest: [u8; 32] = hex::decode(&manifest_hex).unwrap().try_into().unwrap();
                let fetcher = fauna_client::ForeignPublicChunkFetcher::with_http(
                    &self.h_base,
                    reqwest::Client::new(),
                );
                let bytes = fauna_core::file_download::download_file_bytes_by_manifest(
                    &fetcher,
                    &FileDownloadKeys {
                        backup_key: None,
                        mls_group_id: Some(self.group_id.clone()),
                        content_keys: Some(keys.clone()),
                        ..Default::default()
                    },
                    fauna_core::data::ContentHash::from_digest_raw(digest),
                    change.content_key_version,
                    rel,
                )
                .await
                .expect(
                    "the owner must be able to open the cross-nest writer's file under the \
                     SHARED content key — a decrypt failure here means the member's engine \
                     sealed under the wrong key (an unbound engine falling back to its own \
                     BackupKey)",
                );
                assert_eq!(
                    bytes, expected,
                    "the owner read the cross-nest writer's file byte-for-byte"
                );
                return;
            }

            assert!(
                Instant::now() < deadline,
                "the member's record never reached the HOME nest's change log within {:?}. \
                 The member's control plane rides its OWN nest, which must relay \
                 `changes.record` to the home nest — if nothing arrived, the write half of \
                 the foreign routing did not engage (the window spans a full rescan tick, \
                 so even the tick-heal path had its chance).",
                BACKSTOP_WINDOW
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Staging — shared by the agent capstone and its in-process twin
// ─────────────────────────────────────────────────────────────────────────

/// The two-nest world every test starts from: the owner's set on H, shared
/// to a member on F under the access [`stage`] was handed, accepted, and the
/// owner's seed already recorded on H (order is load-bearing — module docs).
struct Staged {
    h_base: String,
    f_base: String,
    f_state: Arc<AppState>,
    bob_secret: [u8; 32],
    bob_id: fauna_core::identity::ActorId,
    bob_hex: String,
    bob_nest: Arc<fauna_client::NestClient>,
    /// The member's app on a machine of its own — where his custody is
    /// written, and whose pump keys every machine he enrolls afterwards.
    bob_app: AppSeat,
    alice_nest: Arc<fauna_client::NestClient>,
    channel_hex: String,
    /// The member's foreign set as the seedless producer resolves it — what
    /// names the set every host serves.
    entry: fauna_core::folder_keys::FolderEngineKeys,
    owner: Owner,
    /// The owner's seed, recorded on H before any member host starts.
    rel: &'static str,
    owner_payload: Vec<u8>,
    /// Kept alive for the test's length: the owner's watch dir.
    _owner_watch: tempfile::TempDir,
}

/// Stage L1–L3: the owner's cross-nest share under `access` (`"writer"` or
/// `"reader"`), the member's accept, the seedless resolution naming the
/// foreign set, and the owner's seed on H.
async fn stage(access: &'static str) -> Staged {
    use fauna_client_conversations::NestConversationsRpc;
    use fauna_client_folders::orchestration::FoldersAuthor;
    use fauna_client_folders::{FoldersClient, NestFolderCustodySink, custody};
    use fauna_conversations::backend::ConversationsRpc;
    use fauna_conversations::backends::fauna_mls::{FaunaMlsBackend, join_folder_welcome};
    use fauna_mls::engine::MlsEngine;

    // Make BOTH in-process nests' and the owner engine's tracing visible — a
    // red here must diagnose itself (`testing.md` § point 6), and the nest
    // side (federation relay refusals, record gates) logs nowhere else. The
    // agent child's stderr rides its own `RUST_LOG` env separately.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();

    // ── Topology ────────────────────────────────────────────────────────
    // The share addresses Bob by actor id with an explicit peer URL, so neither
    // nest needs a `handle_domain`: this capstone is about the sync/keying/
    // routing chain, and handle discovery has its own two-nest coverage in
    // `conformance_cross_nest_conversations_client`.
    let (h_base, h_state) = common::start_nest().await;
    let (f_base, f_state) = common::start_nest().await;
    let f_authority = f_base.trim_start_matches("http://").to_string();

    // Alice owns the set on H; Bob is a writer member living on F.
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    h_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();
    let alice_engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).unwrap());

    let mut bob_secret = [0u8; 32];
    getrandom::fill(&mut bob_secret).unwrap();
    let bob_id = ActorKeypair::from_secret(bob_secret).actor_id();
    let bob_hex = hex::encode(bob_id.0);
    f_state
        .db
        .create_user_with_handle(&bob_id.0, "free", "bob", None)
        .await
        .unwrap();
    let bob_engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(bob_secret)).unwrap());
    let bob_pkgs = bob_engine.generate_key_packages_bytes(1).unwrap();
    f_state
        .db
        .put_key_package("bob-kp-0", &bob_id.0, &bob_pkgs[0], 0, FAR_FUTURE)
        .await
        .unwrap();

    // ── L1: the owner creates the set and shares it across nests,
    // production path — the create mints the set's nonce into her custody,
    // which every signed record binds to and the share carries to the member.
    let alice_nest = connected_client(&h_base, ActorKeypair::from_secret(alice_secret)).await;
    let alice_custody: Arc<dyn FolderKeyStore> =
        Arc::new(fauna_client_folders::MemoryFolderKeyStore::default());
    fauna_client_folders::create_set(
        &FoldersClient::new(Arc::clone(&alice_nest)),
        &*alice_custody,
        fauna_protocol::folders::FolderCreateRequest {
            name: SET_NAME.to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("alice creates the set");
    let alice_author = FoldersAuthor::new(
        FoldersClient::new(Arc::clone(&alice_nest)),
        ActorKeypair::from_secret(alice_secret),
        Arc::clone(&alice_custody),
        Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        alice_engine.clone(),
    );
    let convs = fauna_client_conversations::ConversationsClient::new(Arc::clone(&alice_nest));
    let outcome = alice_author
        .share_set(
            &convs,
            SET_NAME,
            bob_id,
            Some(f_base.clone()),
            Some(access.to_string()),
        )
        .await
        .expect("cross-nest share through the production owner path");
    let channel_id = outcome.channel_id;
    let channel_hex = hex::encode(channel_id);

    // ── L1b: the member accepts, and the grant must have SURVIVED the relay ──
    let inbox = f_state.db.list_inbox_all(&bob_id.0).await.unwrap();
    let env = fauna_protocol::inbox::InboxEnvelope::from_canonical_bytes(&inbox[0].1)
        .expect("canonical inbox envelope");
    let staged = env.decode_welcome().expect("welcome envelope");
    assert_eq!(
        staged.access.as_deref(),
        Some(access),
        "the share-time grant must reach the recipient's staged welcome — \
         without it every app renders a foreign writer as a reader and never \
         offers the bind"
    );

    let bob_nest = connected_client(&f_base, ActorKeypair::from_secret(bob_secret)).await;
    let bob_app = AppSeat::sign_in(&f_state, &f_base, bob_secret).await;
    let bob_custody = bob_app.folder_keys();
    let bob_backend = FaunaMlsBackend::new(
        bob_engine.clone(),
        Arc::new(NestConversationsRpc::new(Arc::clone(&bob_nest))) as Arc<dyn ConversationsRpc>,
        format!("bob@{f_authority}"),
        bob_id,
    );
    bob_backend.set_folder_custody_sink(Arc::new(NestFolderCustodySink::new(
        Arc::clone(&bob_nest),
        Arc::clone(&bob_custody),
    )));
    join_folder_welcome(
        &bob_backend,
        &channel_hex,
        &staged.welcome_bytes,
        &h_base,
        &fauna_conversations::session::FolderWelcomeContext {
            set_name: staged.set_name.clone(),
            access: staged.access.clone(),
            home_nest_actor_id: staged.home_nest_actor_id.clone(),
            set_name_seal: fauna_core::label_custody::SealedSetName::from_wire(
                staged.set_name_sealed.as_deref().map(|b| &b[..]),
                staged.set_name_hash.as_deref().map(|b| &b[..]),
            ),
            ..Default::default()
        },
    )
    .await
    .expect("bob joins the cross-nest shared set");

    // The accept must durably record the foreign set — a cold client start has
    // nothing else to learn this set's home nest or group from.
    let bob_held = bob_custody.load().await.expect("bob's custody");
    let foreign_record = custody::find_foreign_set(&bob_held, &channel_id)
        .expect("the accept wrote the foreign-set record");
    assert_eq!(
        foreign_record.access.as_deref(),
        Some(access),
        "the grant must be recorded durably, not just observed in flight"
    );
    let group_id = foreign_record.mls_group_id.clone();

    // ── L2: the agent's own resolution, by the producer it runs ─────────
    // A foreign set is in NO nest projection, so the producer must union the
    // member's own custody — the fold the agent reads through its mounted
    // store, read here through his app's.
    let decoded =
        fauna_client_folders::resolve_engine_keys(&*bob_custody, bob_id, Arc::clone(&bob_nest))
            .await
            .expect("the member's custody resolves");
    let entry = decoded
        .iter()
        .find(|b| b.folder == SET_NAME)
        .unwrap_or_else(|| {
            panic!(
                "the foreign set {SET_NAME:?} is absent from the resolution \
                 (saw {:?}). The agent would build no engine for it: the member's \
                 cross-nest binding would never sync.",
                decoded.iter().map(|b| &b.folder).collect::<Vec<_>>()
            )
        });
    assert_eq!(
        entry.foreign_routing(),
        Some((h_base.clone(), channel_hex.clone())),
        "a foreign set's entry must carry BOTH routing fields — half-populated \
         reads as same-nest, which would send the member's writes to a nest that has \
         never heard of this set"
    );
    assert!(
        entry.content_keys.is_some(),
        "the member joined the MLS group, so the set's M2 content keys must be in \
         custody; without them the engine fails closed and never syncs"
    );

    // ── L3: the owner seeds the set BEFORE the member binds ─────────────
    // Order is load-bearing (module docs § Order is load-bearing): a
    // cross-nest member has no nudge and a 300 s cadence, so the ONLY
    // sub-tick delivery mechanism is the bind-time eager pull — which
    // delivers this upload *causally* precisely because it is already
    // recorded on H when the bind happens. Alice is NOT the subject of this
    // capstone; her engine is built directly (same-nest). What her half must do is put real M2-sealed bytes on H for
    // the member's eager pull to fetch.
    let owner = Owner {
        h_base: h_base.clone(),
        nest: Arc::clone(&alice_nest),
        secret: alice_secret,
        custody: alice_custody,
        channel_id,
        group_id: group_id.clone(),
    };
    let owner_watch = tempfile::tempdir().unwrap();
    let alice_sync = owner
        .engine(&h_state, owner_watch.path().to_path_buf())
        .await;

    let owner_payload: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
    let rel = "from-alice.bin";
    std::fs::write(owner_watch.path().join(rel), &owner_payload).unwrap();
    alice_sync
        .upload_file(rel)
        .await
        .expect("the owner uploads under the set's M2 content key");

    Staged {
        h_base,
        f_base,
        f_state,
        bob_secret,
        bob_id,
        bob_hex,
        bob_nest,
        bob_app,
        alice_nest,
        channel_hex,
        entry: entry.clone(),
        owner,
        rel,
        owner_payload,
        _owner_watch: owner_watch,
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The capstone
// ─────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn cross_nest_writer_binds_round_trips_and_parks_through_the_agent() {
    use fauna_client_folders::FoldersClient;

    let Staged {
        f_base,
        f_state,
        bob_secret,
        bob_id,
        bob_hex,
        bob_nest,
        bob_app,
        alice_nest,
        entry,
        owner,
        rel,
        owner_payload,
        ..
    } = stage("writer").await;
    let bob_backup_key = fauna_core::crypto::BackupKey::derive(&bob_secret);

    // ── L4: the real agent process, provisioned with no content key ─────
    let world = AgentWorld::new("fauna-xnest-capstone-");
    let socket = world.socket_path();
    let member_location = world.root().join("bob-bound");
    std::fs::create_dir_all(&member_location).unwrap();

    // The member's app enrols this machine first, so the agent loads a
    // `SyncWrite` writer key as its change signer and reads his custody as the
    // machine's enrolled principal — which his other machine's next pass keys
    // for the account's generation. The enrolling app is gone before the agent
    // starts.
    common::enroll_as_w5_app(
        bob_secret,
        &world,
        &f_base,
        &bob_hex,
        None,
        hex::encode(MEMBER_DEVICE),
    )
    .await;
    bob_app.sync().await;
    let _agent = world.spawn_agent_trusting(&[&f_base]);
    await_agent(&socket);

    // The member's bearer is on his OWN nest F: every control-plane call rides
    // F and F relays the federated ones to H. That is the whole cross-nest shape.
    let bob_http_token = f_state.auth.token_store.insert(bob_id, 3600).await;
    // The member's own BackupKey: it is what opens the account's state rows —
    // his custody among them, which is where the agent finds the foreign set
    // and its keys.
    let cap = SyncCapability::new(
        bob_backup_key.to_bytes().to_vec(),
        bob_id.0.to_vec(),
        f_base.clone(),
        hex::encode(MEMBER_DEVICE),
        BearerToken::new(bob_http_token, 4_000_000_000),
    );
    expect_ok(&socket, RequestMethod::ProvisionCapability(cap));

    // Register the member's write device on his own nest (the app does this at
    // login; `changes.record` keys its device gate on the connection actor).
    let _: fauna_protocol::sync::SyncRegisterReply = {
        use fauna_protocol::RpcRequester;
        bob_nest
            .request(
                "fauna.sync.register",
                fauna_protocol::sync::SyncRegisterRequest {
                    device_id: hex::encode(MEMBER_DEVICE),
                    label: "bob-agent".into(),
                    capabilities: "read,write".into(),
                    ..Default::default()
                },
            )
            .await
            .expect("register the member's device")
    };

    // The bind gesture, exactly as the provisioner performs it.
    expect_ok(
        &socket,
        RequestMethod::AddLocation {
            path: member_location.display().to_string(),
        },
    );
    expect_ok(
        &socket,
        RequestMethod::SetLocationFolder {
            path: member_location.display().to_string(),
            folder: SET_NAME.to_string(),
            // The ref the shared resolver stamps on the foreign entry — what
            // `folder_ref_for_row` hands every app's bind gesture.
            folder_id: entry.folder_id.clone(),
        },
    );

    // The agent keys the set from its own custody read, which the provision
    // above started (every reconcile is a content-key edge) and which lands
    // asynchronously — so wait, bounded, for the engine it names to serve.
    await_serving_engine(&socket, SET_NAME, SYNC_WINDOW);

    // ── The eager pull must deliver the pre-bind seed (foreign READ leg) ─
    // This is `set_foreign_routing`'s `changes.list` half — built,
    // first run by this module. The trigger is causal: the seed was recorded
    // on H before the bind, and `run_watch_loop` pulls once eagerly at entry.
    //
    // ⚠ SYNC_WINDOW's size is itself an assertion (see its doc): when this
    // capstone was first run the file arrived at **300.1 s** — startup
    // converged only the local half and the first tick was consumed, so a
    // freshly-bound member watched an empty folder for a full cadence.
    // Raising this window above 300 s would silently re-admit that defect;
    // fix the engine instead.
    await_file_content_within(
        &member_location.join(rel),
        &owner_payload,
        SYNC_WINDOW,
        "the owner's file never reached the cross-nest member's bound folder",
        "capstone",
        "The bytes did not round-trip through the agent's cross-nest engine. If the \
         file never arrived at all, suspect the routing half (the member's engine \
         polling its OWN nest's change log, which holds no rows for a set that nest \
         does not claim); if it arrived corrupt, suspect the key (an UNBOUND engine \
         falling back to this actor's own BackupKey).",
    );

    // ── L5: the member writes; the owner must read it back ──────────────
    let member_payload: Vec<u8> = (0..30_000u32)
        .map(|i| (i.wrapping_mul(7) % 251) as u8)
        .collect();
    let member_rel = "from-bob.bin";
    let tmp = member_location.join(format!("{member_rel}.tmp"));
    std::fs::write(&tmp, &member_payload).unwrap();
    std::fs::rename(&tmp, member_location.join(member_rel)).unwrap();

    owner
        .await_reads_back(member_rel, &member_payload, &bob_hex)
        .await;

    // ── L6: demotion parks the engine, fail-closed AND visible ──────────
    FoldersClient::new(Arc::clone(&alice_nest))
        .members_set_access(fauna_protocol::folders::MemberSetAccessRequest {
            name: SET_NAME.into(),
            actor_id: bob_hex.clone(),
            access: "reader".into(),
            ..Default::default()
        })
        .await
        .expect("owner demotes the cross-nest writer to reader");

    // The member's next local edit must meet a typed refusal at the mint and park.
    let after = member_location.join("after-demotion.txt");
    std::fs::write(&after, b"written after the grant was revoked\n").unwrap();

    let deadline = Instant::now() + BACKSTOP_WINDOW;
    let mut parked = false;
    while Instant::now() < deadline {
        if list_locations(&socket)
            .iter()
            .any(|f| f.folder.as_deref() == Some(SET_NAME) && f.access_revoked)
        {
            parked = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(
        parked,
        "after the demotion the agent must report the folder `access_revoked` — a \
         writer whose grant is gone and whose folder still claims to be syncing is \
         the SILENT un-sync `file-sync.md`'s iron rule forbids"
    );

    // D4's other half: local files are never touched.
    assert!(
        after.exists() && member_location.join(member_rel).exists(),
        "parking must leave the user's local files exactly where they are"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// The in-process twin — one builder, custody is the row
// ─────────────────────────────────────────────────────────────────────────

/// A capability host shaped like `FfiFileProviderHost`, built over the
/// member's cross-nest set — what every in-process test below drives.
struct InProcessHost {
    built: fauna_sync_engine::engine_lifecycle::BuiltEngine,
    /// The host's root — where a hydrated file and a local edit land.
    root: tempfile::TempDir,
    _state_dir: tempfile::TempDir,
    /// The enrolled machine's slot: the host's custody replica reads it.
    _slot_world: AgentWorld,
}

/// Build the member's cross-nest set the way an in-process capability host
/// does (`on-demand-files.md` § Shared sets on a capability host → *One
/// mechanism*, question 2): handed only the member's `BackupKey`, a bearer on
/// his OWN nest and the machine principal his app enrolled — no content key,
/// no identity seed — through the one builder (`build_engine`), from his
/// custody record alone, read through a throwaway fleet replica as the
/// enrolled device it is (decision 1′). `reader_hosting` is the host's own
/// shape; the record's access decides what is built under it.
async fn in_process_host(
    staged: &Staged,
    reader_hosting: fauna_sync_engine::engine_lifecycle::ReaderHosting,
) -> InProcessHost {
    use fauna_nest_http::{BearerSource, StaticBearer};
    use fauna_sync_engine::engine_lifecycle::{EngineCredential, EngineParams, build_engine};

    let Staged {
        h_base,
        f_base,
        f_state,
        bob_secret,
        bob_id,
        bob_hex,
        channel_hex,
        entry,
        ..
    } = staged;
    let (bob_secret, bob_id) = (*bob_secret, *bob_id);

    // The host's capability: a bearer on the member's own nest F, and his
    // `BackupKey` — the `FfiFileProviderHost::app_dead` shape.
    let token = f_state.auth.token_store.insert(bob_id, 3600).await;
    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer(token));
    let auth = Arc::new(fauna_client::AuthClient::bearer_only(
        f_base.clone(),
        bob_id.0,
        bearer,
        fauna_client::pinned_http_client(f_base),
    ));
    let nest_rpc = fauna_client::NestClient::with_auth(Arc::clone(&auth));
    nest_rpc
        .connect()
        .await
        .expect("the host connects its own bearer control plane");

    // The host's change signer: what the member's app enrolled into this
    // machine's slot, handed over the way a capability host is provisioned
    // (`principal_bundle::load_change_signer`).
    let slot_world = AgentWorld::new("fauna-xnest-host-slot-");
    common::enroll_as_w5_app(
        bob_secret,
        &slot_world,
        f_base,
        bob_hex,
        None,
        hex::encode(MEMBER_DEVICE),
    )
    .await;
    let change_signer = fauna_sync_engine::principal_bundle::load_change_signer(
        &slot_world.shared_credentials(),
        &bob_id.0,
    )
    .expect("the enrollment left a SyncWrite signer in the slot");

    // The host's custody reader: a throwaway fleet replica keyed as that
    // machine principal (`fauna_ffi::capability_host_folder_keys`'s shape), over
    // the slot's retained-key custody — what an in-process host installs — which
    // holds the generation the enrolling app recovered from escrow.
    let writer_key = fauna_sync_engine::principal_bundle::load_writer_key(
        &slot_world.shared_credentials(),
        bob_hex,
    )
    .expect("the enrollment left the machine principal in the slot");
    let folder_keys = fauna_sync_engine::cold_folder_keys::ColdReplicaFolderKeys::spawn(
        fauna_client::SelfConnecting(fauna_client::NestClient::with_auth(Arc::clone(&auth))),
        bob_id,
        fauna_core::crypto::BackupKey::derive(&bob_secret),
        fauna_sync_engine::cold_replica::ColdKeySource::Device {
            writer_key: Box::new(writer_key),
            custody: Arc::new(fauna_sync_engine::principal_bundle::SlotRetainedKeys::over(
                Arc::new(slot_world.shared_credentials()),
                bob_hex.clone(),
                slot_world
                    .shared_store_root()
                    .store_dir(bob_hex)
                    .expect("the slot machine's store dir"),
            )),
        },
    )
    .expect("the host's custody replica starts");
    {
        use fauna_client_folders::FolderKeyReader;
        let held = folder_keys
            .load()
            .await
            .expect("the host's replica walks the member's fleet scope");
        assert!(
            fauna_client_folders::custody::find_foreign_set(
                &held,
                &hex::decode(channel_hex).unwrap().try_into().unwrap()
            )
            .is_some(),
            "the host's replica must read the member's foreign-set record as the enrolled \
             machine — an empty custody here means it keyed no generation (no wrap for the \
             principal and none in the slot's retained keys)"
        );
    }

    let state_dir = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let folder_ref = fauna_core::folder_keys::FolderRef::parse(&entry.folder_id)
        .expect("the resolved entry carries the set's ref");
    assert!(
        matches!(folder_ref, fauna_core::folder_keys::FolderRef::Foreign(_)),
        "the member's set is cross-nest"
    );
    let built = build_engine(EngineParams {
        state_dir: state_dir.path().to_path_buf(),
        watch_dir: root.path().to_path_buf(),
        folder_ref,
        device_id: MEMBER_DEVICE,
        // The host registers its device (`changes.record` keys its gate on it).
        device_label: Some("bob-file-provider".to_string()),
        auth,
        nest_rpc,
        mls: None,
        credential: EngineCredential::BackupKey(fauna_core::crypto::BackupKey::derive(&bob_secret)),
        progress_tx: None,
        predecessor_backup_keys: Vec::new(),
        predecessor_actor_ids: Vec::new(),
        learned_predecessors: Default::default(),
        access_gate: None,
        // The seed-less File Provider shape, provisioned with the app's signer.
        change_signer: Some(Arc::new(change_signer)),
        folder_keys: Arc::new(folder_keys),
        reader_hosting,
    })
    .await
    .expect(
        "the one builder must build a cross-nest set from the member's custody record alone — \
         a refusal here means the foreign arm did not find the record, or the host's replica \
         could not key it as the enrolled machine",
    );
    assert_eq!(
        built.engine.foreign_routing(),
        Some((h_base.clone(), channel_hex.clone())),
        "the control plane relays through the member's own nest to the home nest"
    );
    assert_eq!(
        built.engine.byte_plane_nest_url(),
        *h_base,
        "the byte plane dials the HOME nest"
    );
    assert!(
        built.held_version.is_some(),
        "keyed from custody — the member joined the group, so the generation is there"
    );
    InProcessHost {
        built,
        root,
        _state_dir: state_dir,
        _slot_world: slot_world,
    }
}

impl InProcessHost {
    /// Enumerate the set (the relayed `changes.list`) into placeholders, then
    /// open the owner's seed off the home nest under the set's content key.
    async fn hydrates_the_owners_seed(&self, staged: &Staged) {
        let fold = self
            .built
            .engine
            .populate_placeholders_from_nest()
            .await
            .expect("the relayed change log populates the host's placeholders");
        assert!(
            fold.recorded >= 1,
            "the owner's pre-seeded file must enumerate on the cross-nest host"
        );
        let bytes = self
            .built
            .engine
            .download_file_bytes(staged.rel)
            .await
            .expect("the owner's file hydrates off the home nest under the set's content key");
        assert_eq!(
            bytes, staged.owner_payload,
            "the in-process host read the owner's file byte-for-byte"
        );
    }
}

/// The capstone's in-process twin (`on-demand-files.md` § Shared sets on a
/// capability host → *One mechanism*, question 2): a capability host
/// ([`in_process_host`]) builds the cross-nest set from the member's custody
/// record alone, hydrates the owner's file off the HOME nest, and writes one
/// back that the owner opens under the set's shared key.
///
/// It is built as a control-inverted host is — `ReaderHosting::ReadOnly` — so
/// it also pins decision 3's other half: a `writer` record still builds the
/// ordinary two-way host there.
///
/// The agent test above proves the same builder behind the real agent
/// process; this one proves it with no agent at all, which is what an
/// in-process host (apple's File Provider extension, android's provider) runs.
#[tokio::test(flavor = "multi_thread")]
async fn an_in_process_capability_host_hydrates_and_writes_a_cross_nest_set_from_custody() {
    let staged = stage("writer").await;
    let host = in_process_host(
        &staged,
        fauna_sync_engine::engine_lifecycle::ReaderHosting::ReadOnly,
    )
    .await;
    assert!(
        !host.built.read_only && !host.built.engine.is_read_only(),
        "a `writer` custody record builds the ordinary two-way host, even on a host that \
         builds a reader's set read-only"
    );
    host.hydrates_the_owners_seed(&staged).await;

    // Write: seal under the set's key, bytes to the home nest, the record
    // relayed — and the owner opens it.
    let member_payload: Vec<u8> = (0..24_000u32)
        .map(|i| (i.wrapping_mul(11) % 251) as u8)
        .collect();
    let member_rel = "from-bob-in-process.bin";
    std::fs::write(host.root.path().join(member_rel), &member_payload).unwrap();
    host.built
        .engine
        .upload_file(member_rel)
        .await
        .expect("the in-process host's write records through the relay");
    staged
        .owner
        .await_reads_back(member_rel, &member_payload, &staged.bob_hex)
        .await;
}

/// A READER's cross-nest set on a control-inverted host (`on-demand-files.md`
/// § Shared sets on a capability host, decision 3): the owner shares it
/// `reader`, the member's custody record says so, and the one builder under
/// `ReaderHosting::ReadOnly` builds a host with no write half — it enumerates
/// the set through the relayed change log, reports itself read-only, and the
/// provider face refuses an ingest and a delete with the typed `ReadOnlyHost`
/// before anything is sealed or recorded: the home nest's change log still
/// holds the owner's seed alone.
#[tokio::test(flavor = "multi_thread")]
async fn a_cross_nest_readers_host_is_read_only_and_records_nothing_on_the_home_nest() {
    use fauna_sync_engine::provider_face::{ReadOnlyHost, WriteAck, serve_delete, serve_ingest};

    let staged = stage("reader").await;
    let host = in_process_host(
        &staged,
        fauna_sync_engine::engine_lifecycle::ReaderHosting::ReadOnly,
    )
    .await;
    let engine = &host.built.engine;
    assert!(
        host.built.read_only && engine.is_read_only(),
        "a `reader` custody record on a control-inverted host builds read-only"
    );

    let fold = engine
        .populate_placeholders_from_nest()
        .await
        .expect("a reader's relayed change log populates the host's placeholders");
    assert!(
        fold.recorded >= 1,
        "the owner's pre-seeded file must enumerate on the reader's cross-nest host"
    );

    let local_rel = "from-reader.bin";
    std::fs::write(host.root.path().join(local_rel), b"a reader's edit").unwrap();
    let refused = |r: anyhow::Result<WriteAck>| {
        r.err()
            .is_some_and(|e| e.downcast_ref::<ReadOnlyHost>().is_some())
    };
    assert!(
        refused(serve_ingest(engine, local_rel).await),
        "an ingest on a reader's host is refused with the typed ReadOnlyHost"
    );
    assert!(
        refused(serve_delete(engine, staged.rel).await),
        "a delete on a reader's host is refused with the typed ReadOnlyHost"
    );

    // Nothing reached the home nest: its change log holds the owner's seed alone.
    let listed: fauna_protocol::sync::SyncChangesListReply = {
        use fauna_protocol::RpcRequester;
        staged
            .alice_nest
            .request(
                "fauna.sync.changes.list",
                fauna_protocol::sync::SyncChangesListRequest {
                    // Schema 114 rests a sealed set's name NULL, so the owner
                    // addresses her own set by its hash, never its plaintext.
                    name_hash: Some(fauna_protocol::ByteBuf::from(
                        fauna_core::path_crypto::set_name_hash(SET_NAME).to_vec(),
                    )),
                    ..Default::default()
                },
            )
            .await
            .expect("the owner reads her set's change log")
    };
    let seed_hash = hex::encode(fauna_core::sync::path_hash(staged.rel));
    assert_eq!(
        listed
            .changes
            .iter()
            .map(|c| c.path_hash.as_str())
            .collect::<Vec<_>>(),
        vec![seed_hash.as_str()],
        "a refused write on a reader's host must record nothing on the home nest — its \
         change log holds the owner's seed alone, untombstoned"
    );
}

/// A reader's cross-nest host HYDRATES: the owner's file opens off the home
/// nest under the set's content key.
///
/// A cross-nest engine's chunk and manifest GETs take a bearer, and the write
/// mint refuses a reader — so before a reader's engine took the read token
/// (`fauna.federation.folder.read_token.mint`, `federation.md` § Cross-nest
/// shared folders + channel append → *Relay serving across nests*) its first
/// read failed `403 write_token.get refused` and parked the engine as a
/// demoted writer. `engine_lifecycle::assemble_engine` now gives a
/// `read_only` binding the read-token bearer; this pins it.
#[tokio::test(flavor = "multi_thread")]
async fn a_cross_nest_readers_host_hydrates_the_owners_file() {
    let staged = stage("reader").await;
    let host = in_process_host(
        &staged,
        fauna_sync_engine::engine_lifecycle::ReaderHosting::ReadOnly,
    )
    .await;
    host.hydrates_the_owners_seed(&staged).await;
}
