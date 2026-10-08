//! [`ClientChannel`] — the bearer client's [`SupervisedChannel`] impl.
//!
//! The reconnect/backoff loop itself ([`fauna_ws_substrate::run_supervisor`])
//! is substrate-neutral and shared with the nest↔nest federation channel; this
//! module supplies the client-specific half it is parameterised over: how a
//! connection is established + authed (the bearer-subprotocol handshake), what
//! per-connection serving is set up (the push bridge), and how a 4401
//! `AuthExpired` close is recovered (clear + re-mint the bearer).

use std::sync::Arc;

use async_trait::async_trait;
use fauna_protocol::{KindRegistry, RpcDispatcher};
use fauna_ws_substrate::{ConnectedAdapter, DialDemand, SupervisedChannel};

use crate::auth_client::AuthClient;
use crate::error::NestClientError;
use crate::push::PushBroker;

/// The bearer client's lifecycle hooks for [`fauna_ws_substrate::run_supervisor`].
pub(crate) struct ClientChannel {
    pub auth: Arc<AuthClient>,
    pub pushes: Arc<PushBroker>,
    /// The same table `NestClient` reads deadlines from, handed to every
    /// dispatcher the supervisor builds so the wire `replay_forbidden` hint
    /// survives each reconnect.
    pub kind_registry: Arc<KindRegistry>,
    /// The e2e agent's reconnect-pace override, shared with the owning
    /// `NestClient` (see [`BackoffOverride`]). Always `None` in a release build,
    /// whose only writer is compiled out.
    pub backoff_override: BackoffOverride,
    /// The owning `NestClient`'s parked requests (`fauna_ws_substrate::DialDemand`).
    pub dial_demand: Arc<DialDemand>,
    /// The push device this client's connections serve, shared with the owning
    /// `NestClient` (see [`PushPresence`]); announced on every connect.
    pub push_presence: PushPresence,
}

/// The push device id a client announces as `fauna.push.presence` on every
/// (re)connect, or `None` for a client that announces nothing — the default,
/// so a caller that does not opt in announces nothing.
///
/// Opt-in per caller on purpose: the id must be the one this install's push
/// row is keyed under, and an announced id that matches no row would make the
/// nest treat the app's own row as absent and dial it while the app is open
/// (`common.md` § Registration → *Every connection announces*). Written by
/// `NestClient::set_push_presence`, read on each connect.
pub(crate) type PushPresence = Arc<std::sync::Mutex<Option<String>>>;

/// Ceiling on one presence announce. The nest registers the kind at 5 s; this
/// is the client-side backstop for a nest that never answers.
const PRESENCE_ANNOUNCE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Announce `device_id` on `dispatcher`'s connection — best-effort, and never
/// in the way of the connection's own serving: a refusal or failure
/// leaves the connection unannounced.
pub(crate) async fn announce_presence(dispatcher: Arc<RpcDispatcher>, device_id: String) {
    let payload = match fauna_protocol::encode_payload(&fauna_protocol::push::PresenceRequest {
        device_id,
        extra: Default::default(),
    }) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("push presence: encode failed: {e}");
            return;
        }
    };
    let mut idem = [0u8; 16];
    if getrandom::fill(&mut idem).is_err() {
        return;
    }
    let reply: Result<fauna_protocol::push::PresenceReply, _> = dispatcher
        .request_encoded(
            "fauna.push.presence",
            idem,
            payload,
            PRESENCE_ANNOUNCE_DEADLINE,
            tokio::time::sleep(PRESENCE_ANNOUNCE_DEADLINE),
        )
        .await;
    if let Err(e) = reply {
        tracing::debug!("push presence: not announced on this connection: {e}");
    }
}

/// Backoff bounds `(initial, max)` a test has asked the supervisor to use
/// instead of its own — read on every backed-off failure, so a change reaches
/// a supervisor that is already running without a reconnect.
///
/// Written only by `NestClient::set_reconnect_backoff_for_test`, which is
/// compiled out of release artifacts (convention 15); the read is left in
/// every build so the seam needs no cfg on the channel's construction sites,
/// and reads `None` wherever nothing could have written it.
pub(crate) type BackoffOverride =
    Arc<std::sync::Mutex<Option<(std::time::Duration, std::time::Duration)>>>;

#[async_trait]
impl SupervisedChannel for ClientChannel {
    /// The push-bridge task pumping this connection's dispatcher broadcast into
    /// the long-lived broker; awaited on disconnect so its exit is confirmed.
    type Session = tokio::task::JoinHandle<()>;
    type Error = NestClientError;

    async fn connect(&self) -> Result<Box<dyn ConnectedAdapter>, NestClientError> {
        let adapter = crate::ws_adapter::connect_with_subprotocol_bearer(&self.auth).await?;
        Ok(Box::new(adapter) as Box<dyn ConnectedAdapter>)
    }

    /// A `401` on the WS upgrade (mapped to [`NestClientError::Api`] with
    /// `status == 401` by `ws_adapter::map_ws_connect_err`) means the server
    /// rejected our bearer at connect time — recoverable by re-minting
    /// ([`refresh_auth`](Self::refresh_auth)). The supervisor uses this to drive
    /// the refresh-then-retry path instead of an endless backoff with the dead
    /// token (the bug a nest factory-reset triggers: it wipes the token store,
    /// so the cached bearer is rejected at the upgrade, never via a 4401 close).
    fn connect_error_is_auth_rejection(err: &NestClientError) -> bool {
        matches!(err, NestClientError::Api { status: 401, .. })
    }

    /// A `401` (above) or a `429` on the upgrade: the nest is up and refusing
    /// this client, so dial-on-demand does not hurry the next dial. A `429`'s
    /// `Retry-After` is held for every dial to that nest by the shared dial
    /// budget (`fauna_ws_substrate::dial_budget`), not here.
    fn connect_error_is_answered_refusal(err: &NestClientError) -> bool {
        matches!(
            err,
            NestClientError::Api {
                status: 401 | 429,
                ..
            }
        )
    }

    /// Three refusals no retry can clear:
    ///
    /// 1. **This identity has been succeeded** — the account now belongs to a
    ///    different keypair, so no bearer this client can mint will ever be
    ///    accepted again (`identity-succession.md` § Propagation → *Own
    ///    device fleet*: "each device's next connect gets the `superseded`
    ///    refusal"). Read off the mint's latch rather than the error value:
    ///    by the time a refusal has crossed the `BearerSource` boundary it is
    ///    a flattened `ApiError` and the successor is gone, which is exactly
    ///    why the latch exists.
    /// 2. **The nest rejected our subprotocol at the upgrade** (HTTP `426`,
    ///    mapped by `ws_adapter::map_ws_connect_err`) — a genuine
    ///    client/server version skew that re-minting a bearer or waiting out
    ///    a backoff cannot fix (`transport.md` § Close codes).
    /// 3. **The nest's pinned identity changed** — the `known_hosts` verdict
    ///    from the bearer mint's graduation. Retrying it is retrying a possible
    ///    MITM: every attempt re-signs a handshake at a nest we can no longer
    ///    authenticate, and the user watches "Connecting…" instead of the
    ///    warning (`security.md` § Post-auth surfacing). Unlike case 1 this is
    ///    read **off the error value**, because the taxonomy now carries it —
    ///    which is what lets it work for linux and tui too, whose
    ///    caller-supplied `LaunchMachineBearer` no latch on this channel can
    ///    see (`auth_client.rs`'s `superseded` field note).
    ///
    /// Stopping here is what turns an endless "Connecting…" into a refusal
    /// the app can name — and it stops re-signing a doomed handshake at the
    /// nest every backoff interval.
    fn connect_error_is_terminal(&self, err: &NestClientError) -> bool {
        self.auth.is_superseded()
            || matches!(
                err,
                NestClientError::SubprotocolMismatch | NestClientError::NestIdentityChanged { .. }
            )
            // Case 1 by value: a caller-supplied `LaunchMachineBearer` has no
            // latch, and carries the refusal typed instead (`map_api_err`).
            || matches!(
                err,
                NestClientError::Rpc(e) if e.code == fauna_protocol::RpcError::CODE_SUPERSEDED
            )
    }

    /// A locked account (`fauna.auth.account_locked`) is terminal until its
    /// own `locked_until` — read off the mint's latch, like case 1 above, since
    /// the unlock time does not survive the `BearerSource` boundary. The
    /// supervisor holds its dials until the latch stops standing; the mint
    /// answers from that same latch meanwhile, so nothing is signed before
    /// then (`devices.md` § The locked state).
    fn connect_error_hold(&self, _err: &NestClientError) -> Option<std::time::Duration> {
        self.auth.locked_hold()
    }

    fn kind_registry(&self) -> Option<KindRegistry> {
        Some((*self.kind_registry).clone())
    }

    fn backoff_override(&self) -> Option<(std::time::Duration, std::time::Duration)> {
        *self
            .backoff_override
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn dial_demand(&self) -> Option<Arc<DialDemand>> {
        Some(Arc::clone(&self.dial_demand))
    }

    async fn on_connect(
        &self,
        dispatcher: &Arc<RpcDispatcher>,
    ) -> Result<Self::Session, NestClientError> {
        let session = self.pushes.bridge_from(dispatcher.push_subscriber());
        // Every connection announces which push device it serves, once per
        // (re)connect (`common.md` § Registration). Spawned, so a slow nest
        // never holds up the connect; the dispatcher handle it holds is
        // released when the reply (or the deadline) arrives.
        let device_id = self
            .push_presence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(device_id) = device_id {
            tokio::spawn(announce_presence(Arc::clone(dispatcher), device_id));
        }
        Ok(session)
    }

    async fn on_disconnect(&self, session: Self::Session) {
        // The dispatcher (and its broadcast) was already dropped by the
        // supervisor, so the bridge sees `Closed` and ends; await to confirm.
        let _ = session.await;
    }

    async fn refresh_auth(&self) -> Result<(), NestClientError> {
        // Server-revoked or expired bearer (4401): drop it and re-mint before
        // the immediate reconnect, so the new connection carries a fresh token.
        self.auth.clear_token().await;
        self.auth.ensure_auth().await.map(|_| ())
    }

    /// On a transport connect-failure, re-resolve `_fauna._tcp.<host>` for the
    /// persisted nest URL: an admin may have changed the nest's client-facing
    /// serving port, leaving the cached `nest_url` pointing at a dead port. If
    /// the SRV now advertises a different port, [`AuthClient::try_srv_recover`]
    /// atomically swaps the live URL (shared with the bearer mint + the content
    /// API) and returns `true`, so the supervisor reconnects immediately on the
    /// new port (the offline-client SRV self-heal,
    /// `docs/goal/architecture/nest/common.md` § Serving ports, path 2).
    async fn recover_endpoint(&self) -> bool {
        self.auth.try_srv_recover().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::push::PushBroker;

    /// The wiring pin for the *client's* leg of the `replay_forbidden` hint.
    ///
    /// `dispatcher.rs`'s hint tests prove the mechanism given an attached
    /// registry; this proves the bearer client actually attaches one — the link
    /// in the flow (`NestClient` holds a registry → `ClientChannel` carries it →
    /// `run_supervisor` attaches it to every reconnect's dispatcher → the hint
    /// reaches the nest) that no dispatcher-level test can see. Without it,
    /// deleting the `kind_registry` override below reddens nothing and every
    /// shipped client silently stops sending the hint.
    #[test]
    fn the_bearer_channel_hands_the_supervisor_a_populated_registry() {
        let keypair = fauna_core::identity::ActorKeypair::generate();
        let channel = ClientChannel {
            auth: Arc::new(AuthClient::new("https://nest.invalid".to_string(), keypair)),
            pushes: PushBroker::new(8),
            kind_registry: Arc::new(fauna_protocol::KindRegistry::full()),
            backoff_override: Default::default(),
            dial_demand: DialDemand::new(),
            push_presence: Default::default(),
        };

        let registry = channel
            .kind_registry()
            .expect("the bearer channel must supply a registry, else no request carries the hint");

        // Not merely non-empty: it must be the *production* table. The bug this
        // guards is exactly a plausible-looking registry that declares almost
        // nothing (`default_with_protocol_kinds` registers one kind), which is
        // what left the whole mechanism inert before 2026-07-31.
        assert_eq!(
            registry
                .meta("fauna.posts.interact")
                .map(|m| m.forbid_replay),
            Some(true),
            "the supplied registry must declare the forbid-replay kinds"
        );
    }

    fn test_channel() -> ClientChannel {
        let keypair = fauna_core::identity::ActorKeypair::generate();
        ClientChannel {
            auth: Arc::new(AuthClient::new("https://nest.invalid".to_string(), keypair)),
            pushes: PushBroker::new(8),
            kind_registry: Arc::new(fauna_protocol::KindRegistry::full()),
            backoff_override: Default::default(),
            dial_demand: DialDemand::new(),
            push_presence: Default::default(),
        }
    }

    /// A `426`-derived `SubprotocolMismatch` must stop the reconnect loop the
    /// same way a superseded identity does — no retry or bearer refresh can
    /// fix a genuine version skew. Regression for the row this closes: before
    /// the fix, `ws_adapter::map_ws_connect_err` never produced this variant
    /// from a real upgrade rejection, so this hook was unreachable in
    /// production (`ws_adapter::tests::
    /// map_ws_connect_err_classifies_a_real_426_upgrade_rejection` pins the
    /// other half).
    #[test]
    fn subprotocol_mismatch_is_terminal() {
        let channel = test_channel();
        assert!(channel.connect_error_is_terminal(&NestClientError::SubprotocolMismatch));
    }

    /// An ordinary transport hiccup must NOT be terminal — only the named
    /// unrecoverable refusals stop the loop; every other error keeps backing
    /// off and retrying.
    #[test]
    fn an_ordinary_transport_error_is_not_terminal() {
        let channel = test_channel();
        assert!(!channel.connect_error_is_terminal(&NestClientError::WebSocket("boom".into())));
    }

    /// The bug `security.md` § Post-auth surfacing names in as many words: the
    /// hourly bearer re-mint meets a changed pinned identity, and the supervisor
    /// — whose whole job is retrying transport failures — treats a MITM signal
    /// as transient. Before the taxonomy carried the verdict this error arrived
    /// as `Auth("ws bearer: …")` and fell through to the backoff path, so the
    /// client re-signed a doomed handshake every interval while the user waited
    /// on an indefinite "Connecting…" with the real reason nowhere on screen.
    ///
    /// Note what this pin does NOT depend on: no latch, no `AuthClient` state.
    /// A `test_channel()` whose mint has latched nothing still stops, which is
    /// precisely the property linux and tui need — their caller-supplied
    /// `LaunchMachineBearer` has no latch for the supervisor to read.
    #[test]
    fn a_changed_nest_identity_is_terminal() {
        let channel = test_channel();
        assert!(!channel.auth.is_superseded(), "no latch is involved here");
        assert!(
            channel.connect_error_is_terminal(&NestClientError::NestIdentityChanged {
                host: "nest.invalid".into(),
                pinned_hex: "aa".repeat(32),
                seen_hex: Some("bb".repeat(32)),
            })
        );
    }

    /// A locked account holds the reconnect loop until its unlock time rather
    /// than stopping it (the lock lapses by itself) or backing off (each dial
    /// before then re-signs a ceremony the nest must refuse) — and once the
    /// lock has lapsed the hold is gone, so the loop dials again.
    #[test]
    fn a_locked_account_holds_until_its_unlock_time_and_no_longer() {
        let channel = test_channel();
        let err = NestClientError::Auth("ws bearer: fauna.auth.account_locked".into());
        assert_eq!(channel.connect_error_hold(&err), None, "nothing latched");

        let now = fauna_protocol::client_clock::now_secs().expect("the client clock reads");
        let locked = |until: u64| {
            let mut e = fauna_protocol::RpcError::new(
                fauna_protocol::RpcError::CODE_ACCOUNT_LOCKED,
                "error.x",
            );
            e.details = Some(Box::new(fauna_protocol::Value::Integer(until.into())));
            e
        };

        channel
            .auth
            .locked_latch_for_test()
            .observe(&locked(now + 3600));
        let hold = channel
            .connect_error_hold(&err)
            .expect("a standing lock holds the loop");
        assert!((3590..=3600).contains(&hold.as_secs()), "got {hold:?}");
        assert_eq!(channel.auth.locked_until_secs(), Some(now + 3600));
        assert!(
            !channel.connect_error_is_terminal(&err),
            "a lock lapses by itself — stopping the loop would strand the client past it"
        );

        channel
            .auth
            .locked_latch_for_test()
            .observe(&locked(now - 60));
        assert_eq!(
            channel.connect_error_hold(&err),
            None,
            "a lapsed lock holds nothing: the loop dials again"
        );
    }

    /// The channel's connect leg announces the named push device on the new
    /// connection — `fauna.push.presence { device_id }`, once per (re)connect
    /// (`common.md` § Registration → *Every connection announces*). The
    /// supervisor calls `on_connect` for every connection it brings up, so
    /// pinning it here pins the reconnect case too.
    #[tokio::test]
    async fn on_connect_announces_the_named_push_device() {
        use futures_util::StreamExt;

        let channel = test_channel();
        *channel.push_presence.lock().unwrap() = Some("dev-42".into());

        let (client_end, mut server_end) = fauna_protocol::test_transport::make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(client_end);
        tokio::spawn(driver);
        let dispatcher = Arc::new(dispatcher);
        let _session = channel.on_connect(&dispatcher).await.expect("connect leg");

        let bytes = tokio::time::timeout(std::time::Duration::from_secs(5), server_end.next())
            .await
            .expect("an announce frame reaches the wire")
            .expect("stream open")
            .expect("no transport error");
        let fauna_protocol::Frame::Request(req) = fauna_protocol::decode_frame(&bytes).unwrap()
        else {
            panic!("expected a Request frame");
        };
        assert_eq!(req.kind, "fauna.push.presence");
        let payload = fauna_cbor::encode_canonical(&req.payload).unwrap();
        let presence: fauna_protocol::push::PresenceRequest =
            fauna_cbor::decode_strict(&payload).unwrap();
        assert_eq!(presence.device_id, "dev-42");
    }
}
