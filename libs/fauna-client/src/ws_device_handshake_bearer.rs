//! [`WsDeviceHandshakeBearer`] — a [`BearerSource`] that mints its bearer over
//! the pre-identity `fauna.auth.device_handshake` kind, signing with a
//! **device key** (the store principal's writer key — W5 (account-data-plane.md § Workstreams).4b) instead of the
//! identity keypair. The device-key sibling of
//! [`crate::ws_challenge_bearer::WsChallengeBearer`], for the process whose
//! nest leg must authenticate *as the machine's store principal*: the account
//! runtime's data path today, W5.5's app-dead agent next
//! (`docs/goal/architecture/apps/sync-agent.md` § Credential model;
//! `docs/goal/architecture/account-replica-posture.md` § The store device
//! principal — "each process mints its own" bearer, so the sessions list
//! stays an honest per-process record).
//!
//! Minting succeeds only once the enrollment ceremony has registered the
//! principal's `RenewBearer` grant on the nest — on a machine's very first
//! sign-in a mint before that is refused `not_registered`. So
//! [`spawn_connect_retry`] takes a [`ConnectGate`]: the account runtime's
//! principal waits for its grant to be registered before the first dial, and
//! a host that starts registered (the sync agent) dials at once. Until
//! 2026-09-28 the first `connect` raced the ceremony and simply retried —
//! one to five refused mints per first sign-in, each spending the nest's
//! failed-credential throttle (`transport-connection.md` § The dial budget).

use std::sync::Arc;

use async_trait::async_trait;

use fauna_nest_http::{ApiError, BearerSource};

use crate::auth_client::{AuthClient, pinned_http_client};
use crate::client::NestClient;
use crate::token_cache::TokenCache;
use crate::ws_challenge_bearer::{LockedLatch, SupersededLatch, map_anon_err};

/// [`BearerSource`] minting over `fauna.auth.device_handshake` with a device
/// signing key. Holds no identity keypair by construction — the whole point
/// is a nest leg that never touches the seed (T10's "no seed touch" auth
/// path).
///
/// One deliberate difference from [`crate::ws_challenge_bearer::WsChallengeBearer`]:
/// a plain owned URL rather than the shared SRV cell (this bearer backs a
/// client built beside the app's — `AuthClient::bearer_only` keeps its own
/// cell, same as the linux `LaunchMachineBearer` posture).
///
/// It latches a supersession exactly as that sibling does, and for the same
/// reason: the refusal is terminal (`succession-propagation.md`). The user
/// hears of it through the app session's own mint; this latch is what stops
/// the data path from re-dialling the handshake for ever to be refused again.
/// Until 2026-09-28 it had none, so a retired identity's device principal kept
/// presenting its credential after the switch away from it, one dial of the
/// process's per-nest dial budget each time (`transport-connection.md` § No
/// dialer outlives its owner).
pub struct WsDeviceHandshakeBearer {
    nest_url: String,
    actor_id: [u8; 32],
    device_signing_key: ed25519_dalek::SigningKey,
    token_cache: TokenCache,
    superseded: SupersededLatch,
    /// The lock's twin of `superseded` — the device handshake enforces the
    /// account lockout too (`login.md` § the handshake's side effects).
    locked: LockedLatch,
    on_not_registered: Option<NotRegisteredHook>,
}

/// What a host does when the nest answers this principal's handshake
/// `fauna.auth.not_registered`: void the machine's registration latch, so the
/// next pass of a runtime holding the owner session registers the grant again
/// by the grant-first probe (`account-replica-posture.md` § The store device
/// principal — the latch is a memory about one nest replica, and a second
/// nest or a rebuilt box holds no grant for it). Without it the connect retry
/// loops for ever against a nest that will never answer otherwise.
pub type NotRegisteredHook = Arc<dyn Fn() + Send + Sync>;

impl WsDeviceHandshakeBearer {
    pub fn new(
        nest_url: impl Into<String>,
        actor_id: [u8; 32],
        device_signing_key: ed25519_dalek::SigningKey,
    ) -> Self {
        Self {
            nest_url: nest_url.into().trim_end_matches('/').to_string(),
            actor_id,
            device_signing_key,
            token_cache: TokenCache::default(),
            superseded: SupersededLatch::default(),
            locked: LockedLatch::default(),
            on_not_registered: None,
        }
    }

    /// Run `hook` whenever the nest answers the handshake `not_registered`
    /// ([`NotRegisteredHook`]).
    #[must_use]
    pub fn with_not_registered_hook(mut self, hook: NotRegisteredHook) -> Self {
        self.on_not_registered = Some(hook);
        self
    }

    /// The superseded-refusal channel this mint publishes on — shared with the
    /// [`AuthClient`] it backs, whose reconnect supervisor stops on it.
    pub fn superseded_latch(&self) -> SupersededLatch {
        self.superseded.clone()
    }

    /// The locked-refusal channel this mint publishes on — shared with the
    /// [`AuthClient`] it backs, whose reconnect supervisor holds on it.
    pub fn locked_latch(&self) -> LockedLatch {
        self.locked.clone()
    }

    /// One `fauna.auth.device_handshake` round trip via the shared
    /// [`fauna_anon_client::mint_bearer_over_device_handshake`] — the
    /// [`TokenCache::bearer`] `fetch` callback.
    async fn fetch_mint(&self) -> Result<fauna_anon_client::MintedBearer, ApiError> {
        // Latched → answer from the latch, never another dial (the sibling
        // `WsChallengeBearer::fetch_mint` does the same).
        if let Some(refusal) = self.superseded.get() {
            return Err(map_anon_err(
                fauna_anon_client::AnonClientError::Rpc(refusal),
                &self.nest_url,
            ));
        }
        if let Some(refusal) = self.locked.standing() {
            return Err(map_anon_err(
                fauna_anon_client::AnonClientError::Rpc(refusal),
                &self.nest_url,
            ));
        }
        fauna_anon_client::mint_bearer_over_device_handshake(
            &self.nest_url,
            self.actor_id,
            &self.device_signing_key,
        )
        .await
        .inspect(|_| self.locked.clear())
        .map_err(|e| {
            if let fauna_anon_client::AnonClientError::Rpc(err) = &e {
                self.superseded.observe(err);
                self.locked.observe(err);
                if err.code == fauna_protocol::RpcError::CODE_NOT_REGISTERED
                    && let Some(hook) = &self.on_not_registered
                {
                    hook();
                }
            }
            map_anon_err(e, &self.nest_url)
        })
    }
}

#[async_trait]
impl BearerSource for WsDeviceHandshakeBearer {
    async fn bearer(&self) -> Result<String, ApiError> {
        self.token_cache.bearer(|| self.fetch_mint()).await
    }

    async fn bearer_with_expiry(&self) -> Result<(String, Option<u64>), ApiError> {
        let (token, expires_at) = self
            .token_cache
            .bearer_with_expiry(|| self.fetch_mint())
            .await?;
        Ok((token, Some(expires_at)))
    }

    async fn notify_401(&self) {
        self.token_cache.clear().await;
    }

    /// The cache is the holder, so its set is this source's set
    /// (`docs/goal/behavior/devices.md` § The client's own session).
    async fn own_token_ids(&self) -> Vec<String> {
        self.token_cache.own_token_ids().await
    }

    async fn current_token_id(&self) -> Option<String> {
        self.token_cache.current_token_id().await
    }
}

/// A [`NestClient`] whose whole auth lifecycle is the store principal's:
/// bearer-only (`AuthClient::bearer_only` — no identity keypair anywhere in
/// the client), minting over `fauna.auth.device_handshake` with the writer
/// key. This is the client an app hands the account runtime as
/// `AccountRuntimeParams::process_rpc`, and the client W5.5's agent will run
/// everything on.
///
/// `on_not_registered` is the host's [`NotRegisteredHook`] — every production
/// host passes one that voids the machine's registration latch.
pub fn device_principal_nest_client(
    nest_url: &str,
    actor_id: [u8; 32],
    writer_key: ed25519_dalek::SigningKey,
    on_not_registered: Option<NotRegisteredHook>,
) -> Arc<NestClient> {
    let mut bearer = WsDeviceHandshakeBearer::new(nest_url, actor_id, writer_key);
    if let Some(hook) = on_not_registered {
        bearer = bearer.with_not_registered_hook(hook);
    }
    let superseded = bearer.superseded_latch();
    let locked = bearer.locked_latch();
    let auth = Arc::new(
        AuthClient::bearer_only(
            nest_url.to_string(),
            actor_id,
            Arc::new(bearer),
            pinned_http_client(nest_url),
        )
        // The supervisor's terminal test reads it, so a superseded principal's
        // reconnect loop stops instead of backing off for ever.
        .with_superseded_latch(superseded)
        // And its hold reads this one: a locked account's loop waits out the
        // lock instead of re-signing a doomed handshake.
        .with_locked_latch(locked),
    );
    NestClient::with_auth(auth)
}

/// Attempts after which a still-failing connect has stopped being a passing
/// fault and started being a **dead data path** — counted from the gate's
/// opening ([`ConnectGate`]), since nothing is dialled before it.
///
/// The backoff reaches its 30 s ceiling around attempt 7, so this first fires
/// minutes in — well past any honest first-sign-in window — and it fires
/// **once**, because the loop then keeps retrying at the ceiling and a line
/// every 30 s forever is noise, not diagnosis.
const CONNECT_RETRY_WARN_AFTER: u32 = 8;

/// Whether this attempt number is the one that earns the standing warn:
/// exactly once, at the boundary, never again.
///
/// A pure function so the policy has a witness — the loop itself spawns a
/// task and retries forever against a live nest, which is not a shape a unit
/// test can pin.
fn warns_at(attempt: u32) -> bool {
    attempt == CONNECT_RETRY_WARN_AFTER
}

/// When [`spawn_connect_retry`] may make its first attempt.
pub enum ConnectGate {
    /// Dial at once — a host whose principal starts with its grant already
    /// registered (the sync agent, which hosts only a principal a signed-in
    /// app enrolled, and the conformance tests).
    Open,
    /// Dial nothing until the receiver reads `true`: the account runtime's
    /// grant-registered signal (`AccountStoreHandle::subscribe_grant_registered`),
    /// open from the start on every launch after a machine's first and
    /// otherwise flipped once the machine's enrollment registration lands. A
    /// dropped sender ends the retry: the runtime that would have opened it
    /// is gone, and so is the reason to connect.
    AfterGrant(tokio::sync::watch::Receiver<bool>),
}

/// How often a gated retry that is still waiting checks whether every owner
/// has let go of its client — a closed gate must not keep the task alive
/// past the client it exists for, and a weak handle cannot be awaited.
const GATE_OWNER_CHECK: std::time::Duration = std::time::Duration::from_secs(30);

/// Wait for `gate` to open, or for the retry to have no reason left to run.
/// `true` = dial.
async fn gate_opens(gate: ConnectGate, client: &std::sync::Weak<NestClient>) -> bool {
    let ConnectGate::AfterGrant(mut rx) = gate else {
        return true;
    };
    if *rx.borrow() {
        return true;
    }
    tracing::debug!("device-principal client: waiting for the grant registration before dialling");
    loop {
        match tokio::time::timeout(GATE_OWNER_CHECK, rx.wait_for(|registered| *registered)).await {
            Ok(Ok(_)) => return true,
            Ok(Err(_)) => {
                tracing::debug!(
                    "device-principal client: the grant signal's runtime is gone; retry ends"
                );
                return false;
            }
            Err(_) if client.strong_count() == 0 => {
                tracing::debug!(
                    "device-principal client: dropped by its owner while gated; retry ends"
                );
                return false;
            }
            Err(_) => {}
        }
    }
}

/// Bring a device-principal client's connection up, retrying until the first
/// `connect` lands (the reconnect supervisor owns the connection from then
/// on). The first attempt waits on `gate` ([`ConnectGate`]), so an account
/// runtime's principal never mints before its grant is on the nest; after
/// that a failure is a retry, never a verdict (an offline nest, a redeploy).
/// Backoff-paced (`fauna_protocol::reconnect::
/// Backoff`), and gives up the moment every owner has let go of the client:
/// the task holds it only weakly between attempts. It used to hold it
/// strongly, so a caller that dropped its client (and the `JoinHandle`, whose
/// drop does not abort) left the loop — and the client it kept alive — dialling
/// for the life of the process; on a machine whose credential had died, one
/// per failed mount, for days (`transport-connection.md` § Connection
/// lifecycle). Pinned by `tests::the_retry_ends_when_its_client_is_dropped`.
///
/// **Why this loop's outcome is logged at all** (2026-08-26): this client is
/// the account runtime's `process_rpc`, and every plane write rides it. All
/// of those legs are local-first and retry on a later pass — *except* the
/// generation mint's escrow deposit, which is the one synchronous nest call
/// on the path. So a connection that never comes up is invisible everywhere
/// but there, where it refuses every `GenerationTip` origination for the life
/// of the process with the plane still reading healthy. Until this commit the
/// loop said nothing on success and only `debug!` on failure, on a target no
/// diagnostic env enabled — so "is the principal's connection up?" could not
/// be answered from any log. Measured on the share journey's linux seats
/// (`escrow deposit failed: rpc disconnected (was_in_flight=false)`, 272×);
/// the mechanism itself is proven good by
/// `conformance_account_runtime::v14_the_first_need_mint_crosses_the_store_principals_own_connection`.
pub fn spawn_connect_retry(
    client: Arc<NestClient>,
    gate: ConnectGate,
) -> tokio::task::JoinHandle<()> {
    let client = Arc::downgrade(&client);
    tokio::spawn(async move {
        if !gate_opens(gate, &client).await {
            return;
        }
        // The agent renewal loop's ceiling-then-grow idiom (`fauna-sync-agent`
        // `renewal.rs`) — no jitter needed for one process per machine.
        let mut backoff = fauna_protocol::reconnect::Backoff::new(
            std::time::Duration::from_millis(500),
            std::time::Duration::from_secs(30),
        );
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let Some(client) = client.upgrade() else {
                tracing::debug!(
                    attempt,
                    "device-principal client: dropped by its owner; retry ends"
                );
                return;
            };
            match client.connect().await {
                Err(e) if client.auth().is_superseded() => {
                    // Terminal: this identity has been succeeded, and no retry
                    // can clear that (`succession-propagation.md`).
                    tracing::info!(
                        attempt,
                        "device-principal client: identity superseded; retry ends: {e}"
                    );
                    return;
                }
                Ok(()) => {
                    tracing::debug!(
                        attempt,
                        "device-principal client: connection UP — the account runtime's data \
                         path (plane writes, and the generation mint's escrow deposit) rides it"
                    );
                    return;
                }
                Err(e) => {
                    tracing::debug!(
                        attempt,
                        "device-principal client: connect not up yet (retrying): {e}"
                    );
                    if warns_at(attempt) {
                        tracing::warn!(
                            attempt,
                            "device-principal client: still not connected after {attempt} \
                             attempts — the account runtime's data path is DOWN, so every \
                             `GenerationTip` origination refuses (no tip can be minted: the \
                             escrow deposit has no reachable door). Retrying at the backoff \
                             ceiling; last error: {e}"
                        );
                    }
                    let d = backoff.ceiling();
                    backoff.grow();
                    // Let go before sleeping, so an owner dropping the client
                    // mid-nap ends this loop at its next wake.
                    drop(client);
                    tokio::time::sleep(d).await;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A retry whose client every owner has dropped stops, instead of keeping
    /// that client alive and dialling on its behalf for ever. The nest here
    /// refuses the handshake's TCP connect outright (nothing listens), which is
    /// the dead-data-path shape the loop never recovers from on its own.
    #[tokio::test(start_paused = true)]
    async fn the_retry_ends_when_its_client_is_dropped() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let client = device_principal_nest_client(
            &url,
            [3u8; 32],
            ed25519_dalek::SigningKey::from_bytes(&[4u8; 32]),
            None,
        );
        let retry = spawn_connect_retry(Arc::clone(&client), ConnectGate::Open);
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        assert!(
            !retry.is_finished(),
            "the retry keeps going while its client is owned"
        );

        drop(client);
        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
        assert!(
            retry.is_finished(),
            "the retry outlived every owner of its client — a dialler nothing can stop"
        );
    }

    /// A superseded principal is terminal: once the latch holds the refusal, the
    /// mint answers it without dialling (nothing listens here, so a dial would
    /// come back as a transport fault), the client reads as superseded — the
    /// supervisor's terminal test — and the connect retry ends on its own while
    /// the client is still owned.
    #[tokio::test(start_paused = true)]
    async fn a_superseded_principal_stops_dialling() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let bearer = WsDeviceHandshakeBearer::new(
            &url,
            [3u8; 32],
            ed25519_dalek::SigningKey::from_bytes(&[4u8; 32]),
        );
        bearer
            .superseded_latch()
            .observe(&fauna_protocol::RpcError::superseded(&[9u8; 32]));
        let err = bearer
            .bearer()
            .await
            .expect_err("a retired principal mints nothing");
        assert!(
            matches!(&err, ApiError::Status { message, .. }
                if message == fauna_protocol::RpcError::CODE_SUPERSEDED),
            "answered from the latch, not by a dial; got {err:?}"
        );

        let client = device_principal_nest_client(
            &url,
            [3u8; 32],
            ed25519_dalek::SigningKey::from_bytes(&[4u8; 32]),
            None,
        );
        assert!(!client.auth().is_superseded());
        client
            .auth()
            .superseded_latch_for_test()
            .observe(&fauna_protocol::RpcError::superseded(&[9u8; 32]));
        assert!(
            client.auth().is_superseded(),
            "the principal's latch reaches the AuthClient the supervisor reads"
        );
        let retry = spawn_connect_retry(Arc::clone(&client), ConnectGate::Open);
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        assert!(
            retry.is_finished(),
            "a superseded principal's connect retry must end, not back off for ever"
        );
    }

    /// **A gated retry dials nothing until the grant is registered** — the
    /// one-to-five `not_registered` mints a first sign-in used to spend
    /// (`transport-connection.md` § The dial budget). A listener that accepts
    /// and counts stands in for the nest: nothing reaches it while the gate is
    /// closed, however long the wait, and the first attempt lands at once when
    /// it opens.
    #[tokio::test(start_paused = true)]
    async fn a_gated_retry_dials_nothing_until_the_grant_is_registered() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let accepted = |l: &std::net::TcpListener| {
            // The kernel completes a loopback connect on its own, so a dial
            // is visible in the accept queue whatever the client did next.
            std::thread::sleep(std::time::Duration::from_millis(20));
            let mut n = 0;
            while l.accept().is_ok() {
                n += 1;
            }
            n
        };
        let client = device_principal_nest_client(
            &url,
            [3u8; 32],
            ed25519_dalek::SigningKey::from_bytes(&[4u8; 32]),
            None,
        );
        let (registered, rx) = tokio::sync::watch::channel(false);
        let retry = spawn_connect_retry(Arc::clone(&client), ConnectGate::AfterGrant(rx));

        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        assert_eq!(
            accepted(&listener),
            0,
            "a closed gate dialled the nest — a mint the nest can only refuse"
        );
        assert!(
            !retry.is_finished(),
            "a closed gate waits; it does not give up"
        );

        registered.send_replace(true);
        let mut dialled = 0;
        // Well inside the first backoff step (500 ms): the first attempt is
        // the gate's own, not a retry's.
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            dialled += accepted(&listener);
            if dialled > 0 {
                break;
            }
        }
        assert!(
            dialled > 0,
            "the open gate's first attempt never reached the nest"
        );
    }

    /// The gate's sender gone (the runtime that would have opened it shut
    /// down) ends a gated retry without a dial.
    #[tokio::test(start_paused = true)]
    async fn a_gated_retry_ends_with_its_runtime() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let client = device_principal_nest_client(
            &url,
            [3u8; 32],
            ed25519_dalek::SigningKey::from_bytes(&[4u8; 32]),
            None,
        );
        let (registered, rx) = tokio::sync::watch::channel(false);
        let retry = spawn_connect_retry(Arc::clone(&client), ConnectGate::AfterGrant(rx));
        drop(registered);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        assert!(
            retry.is_finished(),
            "a gate no one can open must not wait for ever"
        );
    }

    /// The standing warn fires **once**, and only after a passing fault has
    /// had its chance to clear. Both halves matter and they pull against each
    /// other: warn too early and a nest redeploy cries wolf on every run;
    /// warn repeatedly and the line is 30 s noise forever, which is how a
    /// real dead data path gets scrolled past.
    #[test]
    fn the_dead_data_path_warns_exactly_once_and_not_during_a_passing_fault() {
        let warned: Vec<u32> = (1..=200).filter(|a| warns_at(*a)).collect();
        assert_eq!(
            warned,
            vec![CONNECT_RETRY_WARN_AFTER],
            "exactly one attempt earns the warn"
        );
        assert!(
            !warns_at(1) && !warns_at(2) && !warns_at(3),
            "a failure in the first attempts is not a verdict (this function's own doc), \
             so they must stay silent"
        );
    }
}
