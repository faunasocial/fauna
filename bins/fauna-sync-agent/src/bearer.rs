//! `BearerSource` over the app-provisioned on-demand hydration capability.
//!
//! The bearer-only `AuthClient` the hydration host's `SyncEngine` runs over needs a
//! live nest bearer but holds no identity keypair to mint one. The WinUI app
//! provisions (and periodically refreshes) the bearer over the named pipe into
//! `SyncServiceState.capability`; this `BearerSource` reads the *current* token from
//! that shared slot, so a `RefreshBearer` IPC transparently updates the token the
//! already-built engine presents — no engine rebuild, and no stale-token failure an
//! hour into a session once the first bearer expires.
//!
//! This is why the host uses `CapabilityBearer`, not `fauna_nest_http::StaticBearer`:
//! `StaticBearer` ("tests only") snapshots a single token and never sees a refresh,
//! so it would silently strand hydration the moment the provisioned bearer expired.
//! See `docs/goal/behavior/file-sync.md` § On-Demand Files.

use std::ops::Deref;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use fauna_ipc::sync::SyncCapability;
use fauna_nest_http::{ApiError, BearerSource};
use tokio::sync::{Notify, RwLock, watch};
use zeroize::Zeroizing;

/// How long a reader whose bearer the nest rejected waits for the renewal
/// loop's answer before its supervisor retries on its own curve. Sized over
/// one device-handshake mint, so in the ordinary case the retry carries the
/// fresh bearer; a loop that is not running (or a mint stuck past this) costs
/// the reader one ordinary backoff step, no more.
const REJECTION_ANSWER_WAIT: Duration = Duration::from_secs(30);

/// The agent's one capability slot (`SyncServiceState.capability`) — and the
/// way back from its readers to the renewal loop.
///
/// Every client the agent runs reads its bearer from here, so this is also
/// where a reader reports that the nest refused what it read: a `401` to the
/// slot's bearer, or `not_registered` on the device-principal leg, which signs
/// with the key the renewal loop renews with. The loop takes the report and
/// asks the nest at once instead of at the bearer's renewal lead
/// (`sync-agent-credentials.md` § Credential model → *A refused renewal is
/// terminal*, its *A rejected credential asks at once* sub-bullet).
///
/// Derefs to the slot's lock, so a reader that only wants the capability reads
/// it exactly as before.
pub struct CapabilitySlot {
    capability: RwLock<Option<SyncCapability>>,
    rejection: StdMutex<Option<Rejection>>,
    reported: Notify,
    answered: watch::Sender<u64>,
}

/// What a reader reported to the renewal loop ([`CapabilitySlot`]).
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Rejection {
    /// A client presented this bearer and the nest answered `401`.
    Bearer(Zeroizing<String>),
    /// The device-principal leg's handshake was answered `not_registered`.
    Principal,
}

impl std::fmt::Debug for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bearer(_) => f.write_str("Bearer(<redacted>)"),
            Self::Principal => f.write_str("Principal"),
        }
    }
}

impl CapabilitySlot {
    pub fn new(capability: Option<SyncCapability>) -> Arc<Self> {
        Arc::new(Self {
            capability: RwLock::new(capability),
            rejection: StdMutex::new(None),
            reported: Notify::new(),
            answered: watch::channel(0).0,
        })
    }

    /// A reader presented `token` and the nest refused it with a `401`.
    pub(crate) fn report_rejected_bearer(&self, token: &str) {
        self.report(Rejection::Bearer(Zeroizing::new(token.to_owned())));
    }

    /// The device-principal leg was answered `fauna.auth.not_registered`.
    pub(crate) fn report_principal_refused(&self) {
        self.report(Rejection::Principal);
    }

    fn report(&self, rejection: Rejection) {
        {
            let mut pending = self
                .rejection
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // One pending report. A principal refusal asks about whatever
            // bearer the slot holds, so it subsumes a bearer report; between
            // two bearer reports the later is the more current.
            if *pending != Some(Rejection::Principal) {
                *pending = Some(rejection);
            }
        }
        self.reported.notify_one();
    }

    /// The renewal loop's side: take the pending report, if any.
    pub(crate) fn take_rejection(&self) -> Option<Rejection> {
        self.rejection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    /// Resolves once a reader has reported since the last call returned.
    pub(crate) async fn rejection_reported(&self) {
        self.reported.notified().await;
    }

    /// The renewal loop has acted on a report: wake every reader waiting for
    /// its answer.
    pub(crate) fn answer(&self) {
        self.answered.send_modify(|n| *n = n.wrapping_add(1));
    }
}

impl Deref for CapabilitySlot {
    type Target = RwLock<Option<SyncCapability>>;

    fn deref(&self) -> &Self::Target {
        &self.capability
    }
}

/// Reads the current bearer token from the shared, app-provisioned capability slot
/// (`SyncServiceState.capability`). Refreshing the capability over IPC updates the
/// token this source returns, with no engine rebuild.
pub struct CapabilityBearer {
    capability: Arc<CapabilitySlot>,
    /// The bearer this source last handed out — what a `401` refers to.
    presented: StdMutex<Option<Zeroizing<String>>>,
}

impl CapabilityBearer {
    pub fn new(capability: Arc<CapabilitySlot>) -> Self {
        Self {
            capability,
            presented: StdMutex::new(None),
        }
    }
}

/// Whether the nest has refused this capability's renewal for good — the
/// device grant the renewal key rides is gone (`fauna.auth.not_registered`
/// on the store principal; `sync-agent-credentials.md` § Credential model →
/// *A refused renewal is terminal*). Carried in the one slot every consumer
/// already reads, as an **empty bearer**: no app ever provisions one, so the
/// state needs no second flag for a consumer to forget to check, and any app
/// provision or `RefreshBearer` — which writes a real token — re-arms it.
pub(crate) fn renewal_refused(cap: &SyncCapability) -> bool {
    cap.bearer.token.is_empty()
}

/// Enter [`renewal_refused`]: zeroize and drop the bearer the nest will no
/// longer renew.
pub(crate) fn mark_renewal_refused(cap: &mut SyncCapability) {
    use zeroize::Zeroize;
    cap.bearer.token.zeroize();
    cap.bearer.token = String::new();
    cap.bearer.expires_at = 0;
}

#[async_trait::async_trait]
impl BearerSource for CapabilityBearer {
    async fn bearer(&self) -> Result<String, ApiError> {
        match self.capability.read().await.as_ref() {
            None => Err(ApiError::Transport(
                "on-demand hydration capability not provisioned".to_string(),
            )),
            // A refused renewal: no bearer to present, and presenting none is
            // the point — every client over this slot fails *before* its
            // socket, so a dead credential dials nothing until an app signs in
            // again (`transport-connection.md` § Connection lifecycle).
            Some(cap) if renewal_refused(cap) => Err(ApiError::SignInRefused),
            Some(cap) => {
                *self
                    .presented
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    Some(Zeroizing::new(cap.bearer.token.clone()));
                Ok(cap.bearer.token.clone())
            }
        }
    }

    /// The nest refused the bearer this source last handed out: hand it back
    /// to the renewal loop through the slot, and wait (bounded) for its
    /// answer, so the caller's retry reads whatever the loop decided — a fresh
    /// bearer, or the empty one of a terminal refusal.
    async fn notify_401(&self) {
        let Some(presented) = self
            .presented
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        else {
            return;
        };
        let mut answers = self.capability.answered.subscribe();
        self.capability.report_rejected_bearer(&presented);
        let _ = tokio::time::timeout(REJECTION_ANSWER_WAIT, answers.changed()).await;
    }
}

/// Build a bearer-only `AuthClient` over `nest_url` + `actor_id`, sourcing its
/// bearer from `capability` via [`CapabilityBearer`] and its HTTP client from
/// [`fauna_client::pinned_http_client`] — the shape independently hand-built at
/// three call sites (`state::SyncServiceState::nest_rpc_client`,
/// `custodian::host_stint`, every engine's control-plane client). **Not** a
/// cross-nest set's byte-plane client, which the shared builder
/// (`fauna_sync_engine::engine_lifecycle::assemble_engine`) sources from a
/// `WriteTokenBearer` instead — genuinely different.
pub(crate) fn bearer_only_auth_client(
    capability: Arc<CapabilitySlot>,
    nest_url: String,
    actor_id: [u8; 32],
) -> Arc<fauna_client::AuthClient> {
    let bearer: Arc<dyn BearerSource> = Arc::new(CapabilityBearer::new(capability));
    let http = fauna_client::pinned_http_client(&nest_url);
    Arc::new(fauna_client::AuthClient::bearer_only(
        nest_url, actor_id, bearer, http,
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use fauna_ipc::sync::BearerToken;

    #[tokio::test]
    async fn capability_bearer_returns_current_token_and_follows_refresh() {
        let slot = CapabilitySlot::new(None);
        let bearer = CapabilityBearer::new(slot.clone());

        // No capability provisioned yet → Transport error (no bearer available).
        assert!(
            bearer.bearer().await.is_err(),
            "unprovisioned capability must not yield a bearer"
        );

        // Provision → returns the provisioned token.
        *slot.write().await = Some(SyncCapability::new(
            vec![1u8; 32],
            vec![2u8; 32],
            "https://nest.example".into(),
            "dev-test".into(),
            BearerToken::new("tok-1".into(), 4_000_000_000),
        ));
        assert_eq!(bearer.bearer().await.unwrap(), "tok-1");

        // A RefreshBearer-style update of the SAME slot is seen immediately — this is
        // the whole reason the host uses CapabilityBearer over a one-shot StaticBearer.
        if let Some(cap) = slot.write().await.as_mut() {
            cap.bearer.token = "tok-2".into();
        }
        assert_eq!(bearer.bearer().await.unwrap(), "tok-2");
    }

    /// A refused renewal yields no bearer — typed as the sign-in refusal it
    /// is — and the next app-pushed token re-arms the slot.
    #[tokio::test]
    async fn a_refused_renewal_yields_no_bearer_until_the_app_pushes_one() {
        let slot = CapabilitySlot::new(Some(SyncCapability::new(
            vec![1u8; 32],
            vec![2u8; 32],
            "https://nest.example".into(),
            "dev-test".into(),
            BearerToken::new("tok-1".into(), 4_000_000_000),
        )));
        let bearer = CapabilityBearer::new(slot.clone());
        mark_renewal_refused(slot.write().await.as_mut().unwrap());
        assert!(matches!(
            bearer.bearer().await,
            Err(ApiError::SignInRefused)
        ));

        if let Some(cap) = slot.write().await.as_mut() {
            cap.bearer = BearerToken::new("tok-2".into(), 4_000_000_000);
        }
        assert_eq!(bearer.bearer().await.unwrap(), "tok-2");
    }

    /// A nest that answers every WS upgrade `401` — a dead credential's whole
    /// view of the world — and counts the dials.
    pub(crate) async fn refusing_nest() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let dials = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&dials);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let _ = sock
                        .write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n")
                        .await;
                });
            }
        });
        (url, dials)
    }

    /// **A `401` reaches the renewal loop, naming the bearer it refused**
    /// (`sync-agent-credentials.md` § Credential model → *A rejected
    /// credential asks at once*). A real control-plane client over the slot
    /// dials a nest that refuses its unexpired bearer; its supervisor's
    /// refresh must hand that bearer back through the slot, where the renewal
    /// loop takes it — rather than re-presenting the same dead token at the
    /// backoff ceiling until the renewal lead, which is what a source with no
    /// `notify_401` did.
    #[tokio::test]
    async fn a_401_hands_the_presented_bearer_back_to_the_renewal_loop() {
        let (url, _dials) = refusing_nest().await;
        let slot = CapabilitySlot::new(Some(SyncCapability::new(
            vec![1u8; 32],
            vec![7u8; 32],
            url.clone(),
            "dev-test".into(),
            BearerToken::new("dead".into(), u64::MAX),
        )));
        let client = fauna_client::NestClient::with_auth(bearer_only_auth_client(
            Arc::clone(&slot),
            url,
            [7u8; 32],
        ));
        let connecting = tokio::spawn({
            let client = Arc::clone(&client);
            async move { client.connect().await }
        });

        // Stand in for the renewal loop: the report must arrive.
        tokio::time::timeout(Duration::from_secs(20), slot.rejection_reported())
            .await
            .expect("a 401 never reached the renewal loop");
        assert_eq!(
            slot.take_rejection(),
            Some(Rejection::Bearer(Zeroizing::new("dead".to_owned()))),
            "the report names the bearer the nest refused"
        );
        slot.answer();
        connecting.abort();
        client.disconnect().await;
    }

    /// A principal refusal subsumes a pending bearer report; it is never
    /// narrowed back to one.
    #[test]
    fn a_principal_refusal_subsumes_a_bearer_report() {
        let slot = CapabilitySlot::new(None);
        slot.report_rejected_bearer("a");
        slot.report_principal_refused();
        slot.report_rejected_bearer("b");
        assert_eq!(slot.take_rejection(), Some(Rejection::Principal));
        assert_eq!(slot.take_rejection(), None);
    }
}
