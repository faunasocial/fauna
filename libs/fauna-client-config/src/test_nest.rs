//! The crate's one honest `RpcRequester` harness — a stateful in-memory fake
//! nest answering the kinds this crate's legs dispatch (`fauna.nest.info`, the
//! admin probe, the deployment-seed hand-off and rotation, the rotation chain,
//! the succession status). `pub(crate)` so every sibling module's tests drive
//! the same fake rather than each testing its own copy.

use fauna_core::identity::ActorKeypair;
use fauna_protocol::{ByteBuf, RpcError, RpcErrorClass, RpcRequester};
use std::sync::Mutex;

pub(crate) use fauna_client_testkit::block_on;

/// Error type for the fake — `Rpc(RpcError)` so `RpcErrorClass` can surface a
/// refusal's code.
#[derive(Debug)]
pub(crate) enum FakeError {
    Rpc(RpcError),
}
impl core::fmt::Display for FakeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Rpc(e) => write!(f, "rpc {}", e.code),
        }
    }
}
impl RpcErrorClass for FakeError {
    fn is_rejection(&self) -> bool {
        true
    }
    fn as_rpc_error(&self) -> Option<&RpcError> {
        match self {
            Self::Rpc(e) => Some(e),
        }
    }
}

/// The fake nest. The wrapper future is `Ready` on first poll, so it runs with
/// no tokio.
#[derive(Default)]
pub(crate) struct FakeConfigNest {
    /// `fauna.account.am_i_admin`'s canned answer — the custody-leg tests'
    /// knob. Defaults `false` (every test that never sets this is, correctly, a
    /// non-admin).
    pub(crate) is_admin: Mutex<bool>,
    /// `fauna.admin.deployment_seed.get`'s canned reply — `None` unless a
    /// custody-leg test sets it.
    pub(crate) handoff_seed: Mutex<Option<String>>,
    /// `fauna.recovery.succession.status`'s canned `succeeded_at`, in epoch
    /// **seconds**. `None` — the default — is the honest answer for the
    /// overwhelming majority of accounts: they are not a successor.
    pub(crate) succession_committed_at: Mutex<Option<i64>>,
    /// Make `fauna.recovery.succession.status` *fail* instead of answering — a
    /// refusal, or an unreachable nest. The filter raise must
    /// degrade to its pre-existing outcome, never propagate it.
    pub(crate) succession_status_fails: Mutex<bool>,
    /// Every kind dispatched, in order — lets a test assert which round trips
    /// a leg made rather than just its outcome.
    pub(crate) calls: Mutex<Vec<&'static str>>,
    /// `fauna.auth.rotation_chain`'s canned chain — empty (a box that never
    /// rotated) unless a mark-reconcile test sets it.
    pub(crate) rotation_chain: Mutex<Vec<fauna_protocol::nest_rotation::SignedNestRotation>>,
    /// The plane custody the box holds — the plane rotation drive's tests hand
    /// their store here, and the rotate ceremony snapshots what of it was
    /// published when it arrived.
    pub(crate) rotate_plane: Mutex<Option<crate::test_helpers::FakeDeploymentSeedStore>>,
    /// The published custody rows as they stood when the rotate ceremony
    /// arrived — the evidence for the published-before-dispatch ordering.
    pub(crate) plane_at_rotate: Mutex<Option<Vec<fauna_core::data::DeploymentSeedEntry>>>,
}

impl RpcRequester for FakeConfigNest {
    type Error = FakeError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.calls.lock().unwrap().push(kind);
        let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
        let reply = match kind {
            // The custody leg resolves the box's own domain for the custody
            // label (`resolve_box_domain`). This fake reports a real domain so
            // the domain-capture assertion has something to see.
            "fauna.nest.info" => fauna_protocol::encode_canonical(&nest_info_reply("box.example")),
            "fauna.account.am_i_admin" => {
                fauna_protocol::encode_canonical(&fauna_protocol::account::AmIAdminReply {
                    admin: *self.is_admin.lock().unwrap(),
                    extra: Default::default(),
                })
            }
            "fauna.admin.deployment_seed.get" => fauna_protocol::encode_canonical(
                &fauna_protocol::admin::AdminDeploymentSeedGetReply {
                    deployment_seed: self.handoff_seed.lock().unwrap().clone().map(Into::into),
                    extra: Default::default(),
                },
            ),
            fauna_protocol::nest_rotation::ROTATION_CHAIN_KIND => fauna_protocol::encode_canonical(
                &fauna_protocol::nest_rotation::RotationChainReply {
                    chain: self.rotation_chain.lock().unwrap().clone(),
                    extra: Default::default(),
                },
            ),
            // The ceremony commits: it adopts the seed it was sent and acks with
            // the derived identity, after snapshotting what of the plane custody
            // it could see.
            "fauna.admin.deployment_seed.rotate" => {
                let req: fauna_protocol::admin::AdminDeploymentSeedRotateRequest =
                    fauna_protocol::decode_strict(&bytes).expect("decode rotate request");
                let seen = self
                    .rotate_plane
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|plane| plane.published())
                    .unwrap_or_default();
                *self.plane_at_rotate.lock().unwrap() = Some(seen);
                let sent = ActorKeypair::from_secret(
                    fauna_core::hex32::decode(req.new_seed.as_str()).expect("hex seed"),
                )
                .actor_id();
                fauna_protocol::encode_canonical(
                    &fauna_protocol::admin::AdminDeploymentSeedRotateReply {
                        nest_actor_id: ByteBuf::from(sent.0.to_vec()),
                        seq: 1,
                        already_rotated: false,
                        extra: Default::default(),
                    },
                )
            }
            fauna_protocol::recovery::SUCCESSION_STATUS_KIND => {
                if *self.succession_status_fails.lock().unwrap() {
                    return Err(FakeError::Rpc(RpcError::new(
                        "fauna.protocol.unknown_kind",
                        "error.protocol.unknown_kind",
                    )));
                }
                fauna_protocol::encode_canonical(&fauna_protocol::recovery::SuccessionStatusReply {
                    succeeded_at: *self.succession_committed_at.lock().unwrap(),
                    ..Default::default()
                })
            }
            other => panic!("unexpected kind {other}"),
        }
        .expect("encode reply");
        Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
    }
}

/// A minimal valid `fauna.nest.info` reply carrying `domain` — what the custody
/// leg reads via `resolve_box_domain` to label the custody entry.
fn nest_info_reply(domain: &str) -> fauna_protocol::discovery::NestInfoReply {
    fauna_protocol::discovery::NestInfoReply {
        domain: domain.into(),
        nest_id: "00".repeat(32),
        version: "test".into(),
        software: "fauna".into(),
        protocols: vec!["fauna".into()],
        capabilities: vec![],
        iroh_relay_url: None,
        subhandles: false,
        registration: None,
        moderation: fauna_protocol::discovery::ModerationInfo {
            extra: Default::default(),
        },
        ..Default::default()
    }
}
