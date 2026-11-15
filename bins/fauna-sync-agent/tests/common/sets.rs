//! The shared-set fixtures the agent's tier3 tests share: actors on a real
//! in-process nest, the owner's authoring seat (a real account runtime and a
//! real MLS engine driving the production `FoldersAuthor` ceremonies), and the
//! member-side custody ingest. Lifted out of `agent_custody_rekey.rs` when
//! `presence_root_tier3.rs` needed the same owner and member.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fauna_client_folders::FolderKeyStore;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_nest::routes::AppState;
use fauna_protocol::RpcRequester;

use super::{AppSeat, connected_client};

const FAR_FUTURE: u64 = u64::MAX / 2;

/// How long a write's change-log record may take to appear: the chain it
/// bounds (watcher flush → seal → upload → record) is seconds when healthy and
/// far below the 300 s rescan tick.
pub const RECORD_WINDOW: Duration = Duration::from_secs(90);

pub struct Actor {
    pub secret: [u8; 32],
    pub id: ActorId,
}

impl Actor {
    pub async fn create(state: &AppState, handle: &str) -> Self {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).unwrap();
        let id = ActorKeypair::from_secret(secret).actor_id();
        state
            .db
            .create_user_with_handle(&id.0, "free", handle, None)
            .await
            .unwrap();
        // Group welcomes ride the Group kind; open the inbox so the share's
        // welcome is accepted (`direct-messages.md` § Reach policy).
        state.db.set_inbox_mode(&id.0, "open").await.unwrap();
        Self { secret, id }
    }

    pub fn keypair(&self) -> ActorKeypair {
        ActorKeypair::from_secret(self.secret)
    }

    pub fn backup_key(&self) -> fauna_core::crypto::BackupKey {
        fauna_core::crypto::BackupKey::derive(&self.secret)
    }

    /// Publish one key package so an owner can add this actor to a group.
    pub async fn publish_key_package(
        &self,
        state: &AppState,
        engine: &fauna_mls::engine::MlsEngine,
    ) {
        let kp = engine.generate_key_packages_bytes(1).unwrap();
        state
            .db
            .put_key_package(
                &format!("{}-kp", hex::encode(&self.id.0[..4])),
                &self.id.0,
                &kp[0],
                0,
                FAR_FUTURE,
            )
            .await
            .unwrap();
    }
}

/// The owner's authoring seat — the owner's app on another of the owner's
/// machines. Nothing here touches the agent.
pub struct OwnerSeat {
    /// The one set this seat authors.
    pub set_name: &'static str,
    pub nest: Arc<fauna_client::NestClient>,
    pub app: AppSeat,
    pub custody: Arc<dyn FolderKeyStore>,
    pub author: fauna_client_folders::orchestration::FoldersAuthor<
        Arc<fauna_client::NestClient>,
        Arc<fauna_mls::engine::MlsEngine>,
    >,
    pub convs: fauna_client_conversations::ConversationsClient<Arc<fauna_client::NestClient>>,
}

impl OwnerSeat {
    pub async fn open(
        state: &Arc<AppState>,
        base: &str,
        owner: &Actor,
        set_name: &'static str,
    ) -> Self {
        let nest = connected_client(base, owner.keypair()).await;
        let app = AppSeat::sign_in(state, base, owner.secret).await;
        let custody = app.folder_keys();
        let engine =
            Arc::new(fauna_mls::engine::MlsEngine::new_in_memory(owner.keypair()).unwrap());
        let author = fauna_client_folders::orchestration::FoldersAuthor::new(
            fauna_client_folders::FoldersClient::new(Arc::clone(&nest)),
            owner.keypair(),
            Arc::clone(&custody),
            Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
            engine,
        );
        let convs = fauna_client_conversations::ConversationsClient::new(Arc::clone(&nest));
        Self {
            set_name,
            nest,
            app,
            custody,
            author,
            convs,
        }
    }

    /// Create the set (production owner path): its nonce is minted into
    /// custody first, and every record the agent signs binds to it — a set
    /// created straight in the nest's db has none, so its records go out
    /// unsigned and the nest refuses them. Returns the nest's folder id.
    pub async fn create_set(&self) -> i64 {
        self.author
            .create_set(fauna_protocol::folders::FolderCreateRequest {
                name: self.set_name.to_string(),
                ..Default::default()
            })
            .await
            .expect("the owner creates the set")
            .id
    }

    /// Share the set with `member` (production owner path); returns the channel.
    pub async fn share(&self, member: &Actor, access: &str) -> [u8; 32] {
        self.author
            .share_set(
                &self.convs,
                self.set_name,
                member.id,
                None,
                Some(access.to_string()),
            )
            .await
            .expect("the owner shares the set")
            .channel_id
    }

    /// Remove `member` — rotates the set's content key and advances its floor.
    pub async fn remove(&self, channel: [u8; 32], member: &Actor) {
        self.author
            .remove_member(self.set_name, channel, member.id)
            .await
            .expect("the owner removes the member (rotate-on-removal)");
    }

    /// The owner's custody keys for the set right now.
    pub async fn keys(&self, channel: [u8; 32]) -> fauna_core::folder_keys::FolderContentKeys {
        self.envelope(channel).await.keys
    }

    /// What the owner's content-key envelope carries to a member right now: the
    /// custody keys and the set's nonce, which every signed record binds to.
    pub async fn envelope(
        &self,
        channel: [u8; 32],
    ) -> fauna_core::folder_keys::ContentKeyEnvelopePayload {
        let custody = self.custody.load().await.expect("the owner's custody");
        fauna_core::folder_keys::ContentKeyEnvelopePayload {
            keys: fauna_client_folders::custody::content_keys(&custody, &channel)
                .expect("owner custody"),
            set_nonce: fauna_client_folders::custody::set_nonce_for_channel(&custody, &channel),
            minted_by: None,
            retired_set_nonces: Vec::new(),
            served_at: None,
            unserved_at: None,
        }
    }

    /// The set's change-log record of `rel`, if any: its content-key version.
    pub async fn recorded_version(&self, rel: &str) -> Option<Option<u64>> {
        use fauna_protocol::sync::{SyncChangesListReply, SyncChangesListRequest};
        let listed: SyncChangesListReply = self
            .nest
            .request(
                "fauna.sync.changes.list",
                SyncChangesListRequest {
                    folder: Some(self.set_name.to_string()),
                    // The agent's engine stamps the set's sealed name, after
                    // which the nest's row answers to the hash alone.
                    name_hash: Some(fauna_protocol::ByteBuf::from(
                        fauna_core::path_crypto::set_name_hash(self.set_name).to_vec(),
                    )),
                    ..Default::default()
                },
            )
            .await
            .expect("the owner lists the set's change log");
        let want = hex::encode(fauna_core::sync::path_hash(rel));
        listed
            .changes
            .iter()
            .filter(|c| c.path_hash == want)
            .map(|c| c.content_key_version)
            .next()
    }

    /// Wait for `rel`'s record and return its content-key version.
    pub async fn await_recorded(&self, rel: &str, what: &str) -> Option<u64> {
        let deadline = Instant::now() + RECORD_WINDOW;
        loop {
            if let Some(version) = self.recorded_version(rel).await {
                return version;
            }
            assert!(
                Instant::now() < deadline,
                "{what}: {rel} was never recorded within {RECORD_WINDOW:?}"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

/// Apply the owner's envelope to a member's custody the way the member app's
/// custody ingest does (`custody_ingest.rs`, `do_merge_and_persist`): the keys
/// first — a first ingest creates the entry — then the owner's nonce over the
/// member's copy. Returns whether the KEYS advanced.
pub fn apply_envelope(
    custody: &mut fauna_core::data::FoldersConfig,
    channel: [u8; 32],
    envelope: &fauna_core::folder_keys::ContentKeyEnvelopePayload,
) -> bool {
    let advanced =
        fauna_client_folders::custody::merge_received_keys(custody, channel, envelope.keys.clone());
    if let Some(nonce) = envelope.set_nonce {
        fauna_client_folders::custody::record_received_set_nonce(
            custody,
            &channel,
            nonce,
            fauna_core::data::Timestamp::now().0,
        );
    }
    advanced
}

/// Give the member's custody the owner's envelope for `channel` — what the
/// member's app's custody ingest writes: a plane write through the app's
/// store, which the nest nudges the member's devices about.
pub async fn ingest_custody(
    app: &AppSeat,
    channel: [u8; 32],
    envelope: fauna_core::folder_keys::ContentKeyEnvelopePayload,
) {
    let (_, advanced) = fauna_client_folders::key_reader::update(&*app.folder_keys(), |custody| {
        apply_envelope(custody, channel, &envelope)
    })
    .await
    .expect("the member's custody takes the generation");
    assert!(
        advanced,
        "the write must actually advance the member's custody"
    );
}

/// The same custody write, with the nest's `state-fleet` nudge lost: the rows
/// rest on the nest and no device of the member is told.
pub async fn store_custody_without_a_nudge(
    app: &AppSeat,
    channel: [u8; 32],
    envelope: fauna_core::folder_keys::ContentKeyEnvelopePayload,
) {
    app.lose_nudges(true);
    ingest_custody(app, channel, envelope).await;
    app.lose_nudges(false);
}

/// Register `device` as one of the actor's sync devices — `changes.record`
/// gates on it.
pub async fn register_agent_device(nest: &fauna_client::NestClient, device: [u8; 32]) {
    let _: fauna_protocol::sync::SyncRegisterReply = nest
        .request(
            "fauna.sync.register",
            fauna_protocol::sync::SyncRegisterRequest {
                device_id: hex::encode(device),
                label: "agent".into(),
                capabilities: "read,write".into(),
                ..Default::default()
            },
        )
        .await
        .expect("register the agent's device");
}
