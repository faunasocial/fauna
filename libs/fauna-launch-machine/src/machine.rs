//! Public surface.

use std::sync::{Arc, Mutex, Weak};

use crate::auth::SilentChallengeOutcome;
use crate::connector::{AuthConnector, WsAuthConnector};
use crate::observer::LaunchObserver;
use crate::persistence::LaunchPersistence;
use crate::probe::ClaimProbe;
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
use crate::snapshots::LaunchPhase;
use crate::snapshots::{
    LaunchIdentity, LaunchSnapshot, LaunchWizardEntry, RefreshReason, TokenStatus,
};
use crate::state::State;

/// The message `last_error` carries for each refusal — the fallback every app
/// already renders, so a build that does not yet read
/// `LaunchSnapshot::account_index_refusal` still tells the user something true.
/// An app that DOES read the field renders these same keys itself, with the
/// action each verdict allows.
fn account_index_refusal_message(refusal: crate::AccountIndexRefusal) -> &'static str {
    use fauna_i18n::strings::onboarding::launch;
    match refusal {
        crate::AccountIndexRefusal::NewerBuild { .. } => launch::INDEX_NEWER_BUILD,
        crate::AccountIndexRefusal::Malformed => launch::INDEX_MALFORMED,
    }
}

struct Inner {
    state: State,
    token: TokenStatus,
    last_error: Option<String>,
    /// Last identity the LAUNCH's silent challenge confirmed. Survives token
    /// refreshes and offline dips — a refresh ignores its reply's metadata (the
    /// launch owns the cached identity), so the last confirmed one stands until
    /// the next launch replaces it.
    identity: Option<LaunchIdentity>,
    /// The account-index refusal `start` read, if any — see
    /// `LaunchSnapshot::account_index_refusal`.
    account_index_refusal: Option<crate::AccountIndexRefusal>,
    /// **The session ids this machine minted** — the current one plus every
    /// earlier own id not yet expired (`docs/goal/behavior/devices.md` § The
    /// client's own session). Lives on `Inner` rather than inside
    /// `State::Online` deliberately: the set must survive the
    /// `Online → Refreshing → Online` round trip, and a renewal's predecessor
    /// is precisely what it exists to remember. Memory only, never persisted.
    own_token_ids: fauna_protocol::auth::OwnSessionIds,
    /// The `locked_until` whose one scheduled refresh has already run
    /// (`devices.md` § The locked state: *exactly one* refresh per lock). A
    /// refresh the nest answers with the same unlock time — this device's
    /// clock runs ahead of the nest's — must not arm another, or the machine
    /// re-signs a doomed ceremony in a loop. Cleared when a ceremony lands
    /// Online.
    lock_refresh_spent_for: Option<u64>,
}

/// How long past `locked_until` the one scheduled refresh waits. The unlock
/// time is the nest's clock and the sleep is this device's; a refresh that
/// lands a moment early is answered locked again and the one attempt is spent,
/// so a few seconds of slack absorbs ordinary clock disagreement.
const LOCK_LAPSE_GRACE_SECS: u64 = 5;

/// The longest single nap of the scheduled lock refresh. The remaining time is
/// re-read off the wall clock after each nap — a timer does not advance while
/// a device is suspended, so one long sleep would overrun the unlock time by
/// however long the lid was shut — and each wake is where a state change
/// cancels the refresh.
const LOCK_RECHECK_SECS: u64 = 60;

#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct LaunchMachine {
    /// This machine's own handle, for the one task it arms itself: the
    /// scheduled refresh at `locked_until`. Weak, so a parked task never keeps
    /// a dropped machine alive.
    me: Weak<Self>,
    inner: Mutex<Inner>,
    persistence: Arc<dyn LaunchPersistence>,
    observer: Arc<dyn LaunchObserver>,
    /// Pre-identity auth transport. Production wiring builds a
    /// [`WsAuthConnector`] (WS-RPC `fauna.auth.*` over the anonymous
    /// connection); tests inject a `MockAuthConnector` via
    /// `new_with_connector`.
    connector: Arc<dyn AuthConnector>,
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl LaunchMachine {
    #[cfg_attr(feature = "uniffi", uniffi::constructor)]
    pub fn new(
        observer: Arc<dyn LaunchObserver>,
        persistence: Arc<dyn LaunchPersistence>,
    ) -> Arc<Self> {
        Self::with_connector(observer, persistence, Arc::new(WsAuthConnector))
    }

    /// Read-only view for clients to render UI. Cheap.
    pub fn snapshot(&self) -> LaunchSnapshot {
        let inner = self.inner.lock().unwrap();
        LaunchSnapshot {
            phase: inner.state.to_phase(),
            token: inner.token.clone(),
            last_error: inner.last_error.clone(),
            superseded_successor: inner.state.superseded_successor(),
            identity_fork: inner.state.identity_fork(),
            identity: inner.identity.clone(),
            account_index_refusal: inner.account_index_refusal,
            sign_in_refused: inner.state.sign_in_refused(),
            locked_until_secs: inner.state.locked_until_secs(),
        }
    }

    /// The nest authority this machine met a changed identity at, when it is
    /// parked in `IdentityChanged` — `None` in every other state.
    ///
    /// A narrow read rather than a new `LaunchPhase` field on purpose:
    /// `LaunchPhase` is UniFFI-exported and switched over exhaustively by all
    /// seven apps (`State::Superseded`'s docs carry the full reasoning), and the
    /// only caller that needs the host is `fauna_nest_http::LaunchMachineBearer`,
    /// naming the nest in the `ApiError::NestIdentityChanged` it now raises
    /// instead of a generic transport failure.
    ///
    /// Native-only, like its one caller: `authority_of` lives in the native
    /// `fauna-anon-client` (the wasm trust seam is `fauna-rpc-wasm`'s instead).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn identity_changed_authority(&self) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        match &inner.state {
            State::IdentityChanged { nest_url, .. } => {
                Some(fauna_anon_client::authority_of(nest_url))
            }
            _ => None,
        }
    }

    /// Bearer token if the machine is currently `Online`. HTTP layers
    /// in clients call this on every request and fall back to nothing
    /// (queueing or 401-driven refresh) otherwise.
    pub fn current_bearer(&self) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        match &inner.state {
            State::Online { bearer, .. } => Some(bearer.clone()),
            _ => None,
        }
    }
}

// Deliberately NOT in the `#[uniffi::export]` block above: this is a
// cross-check seam for Rust callers assembling a client, not an app-rendered
// fact, and keeping it out leaves the UniFFI surface (and its generated
// bindings) untouched.
impl LaunchMachine {
    /// The actor id the bearer this machine mints names — i.e. *who the nest
    /// will think the caller is*. `None` before the machine holds a secret
    /// (Boot/Hydrating/Offline/wizard states), or if the held secret is not
    /// 32 bytes.
    ///
    /// Exists so a caller assembling a client from **two** independent identity
    /// sources — this machine (which mints the bearer) and a separately-supplied
    /// keypair (which becomes the authenticated WS *path* actor) — can prove
    /// they agree. They are not structurally tied to each other, and when they
    /// disagree the nest refuses every WS upgrade with a `403` and the app goes
    /// silently dark for as long as the reconnect backoff lasts; that failure
    /// was diagnosable only from a *nest* log until this seam existed (2026-08).
    ///
    /// Derives the id the same way `auth.rs` does when it signs — the actor id
    /// *is* the Ed25519 public key — so this needs no `fauna-core` dependency
    /// (see the `ed25519-dalek` note in `Cargo.toml`).
    pub fn bearer_actor_id(&self) -> Option<[u8; 32]> {
        let inner = self.inner.lock().unwrap();
        // Only the states that actually carry the secret the bearer is (or will
        // be) minted from. `SilentChallenge` is deliberately included: a client
        // assembled while the first challenge is still in flight mints from that
        // same secret the moment it lands.
        let secret = match &inner.state {
            State::Online { secret, .. }
            | State::Refreshing { secret, .. }
            | State::SilentChallenge { secret, .. } => secret,
            _ => return None,
        };
        let secret_arr = <[u8; 32]>::try_from(secret.as_slice()).ok()?;
        Some(
            ed25519_dalek::SigningKey::from_bytes(&secret_arr)
                .verifying_key()
                .to_bytes(),
        )
    }

    /// **The session ids this machine minted** — the current one plus every
    /// earlier own id not yet expired (`docs/goal/behavior/devices.md` § The
    /// client's own session). A sessions surface folds all of them into its
    /// one "this app" row, so an hourly renewal never paints the app's own
    /// predecessor as an unknown second session.
    ///
    /// Out of the `#[uniffi::export]` block for the same reason
    /// [`Self::bearer_actor_id`] is: this is the seam a Rust caller assembling
    /// a client reads (`fauna_nest_http::LaunchMachineBearer` forwards it as
    /// `BearerSource::own_token_ids`). The app-rendered surface that will
    /// consume it — the sessions page and `SessionsClient` — is gated on the
    /// user's ruling about that surface and brings
    /// its own export when it lands.
    pub fn own_token_ids(&self) -> Vec<String> {
        let mut inner = self.inner.lock().unwrap();
        let now = Self::now_secs_opt();
        inner.own_token_ids.prune(now);
        inner.own_token_ids.ids_at(now)
    }

    /// The current session id, read at call time — what `revoke_all`'s
    /// `keep_token_id` is filled from, never a previously painted list
    /// (`devices.md` § The client's own session).
    pub fn current_token_id(&self) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        inner.own_token_ids.current_at(Self::now_secs_opt())
    }

    /// The bearer, only while it is worth presenting: `Online` and more than
    /// [`fauna_protocol::auth::BEARER_REFRESH_BUFFER_SECS`] from its deadline
    /// **on this machine's clock**. `None` means refresh first (a bearer inside
    /// its buffer, or none at all, or a clock this crate cannot read — which is
    /// never "maximally fresh", the direction `fauna_client::token_cache` pins
    /// for the same reason).
    ///
    /// The spend rule lives here, beside the deadline it reads, so the one
    /// consumer (`fauna_nest_http::LaunchMachineBearer`) cannot compare the
    /// deadline against a *different* clock than the one it was anchored to —
    /// which is exactly what it did before 2026-09-21, with `SystemTime`.
    pub fn fresh_bearer(&self) -> Option<String> {
        self.fresh_bearer_with_expiry().map(|(bearer, _)| bearer)
    }

    /// [`Self::fresh_bearer`] with the deadline it was judged against — unix
    /// seconds on this machine's clock, anchored at receipt. Read under one
    /// lock, so the pair names one bearer.
    pub fn fresh_bearer_with_expiry(&self) -> Option<(String, u64)> {
        let now = Self::now_secs_opt()?;
        self.current_bearer_with_expiry()
            .filter(|(_, expires_at_secs)| {
                *expires_at_secs
                    > now.saturating_add(fauna_protocol::auth::BEARER_REFRESH_BUFFER_SECS)
            })
    }

    /// The `Online` bearer with its deadline on this machine's clock, whatever
    /// its freshness — what `fauna_nest_http::LaunchMachineBearer` serves after
    /// a refresh, so it can publish the expiry beside the token
    /// (`BearerSource::bearer_with_expiry`) instead of reading the two apart.
    pub fn current_bearer_with_expiry(&self) -> Option<(String, u64)> {
        let inner = self.inner.lock().unwrap();
        match &inner.state {
            State::Online {
                bearer,
                expires_at_secs,
                ..
            } => Some((bearer.clone(), *expires_at_secs)),
            _ => None,
        }
    }

    /// A mint reply's deadline on this machine's clock — the one conversion
    /// every bearer holder makes at receipt
    /// (`fauna_protocol::auth::deadline_on_own_clock`; `login.md` § Token
    /// lifetime on the client's clock), read through the launch clock like
    /// every other `now` in this crate so the wrong-clock witness sees it.
    fn deadline_on_own_clock(expires_in: u64, expires_at: u64) -> u64 {
        fauna_protocol::auth::deadline_on_own_clock(Self::now_secs_opt(), expires_in, expires_at)
    }

    /// Unix seconds, or `None` when the clock cannot be read. `None` prunes
    /// nothing — [`fauna_protocol::auth::OwnSessionIds::prune`] owns that
    /// direction and the reason for it.
    ///
    /// **Through [`crate::launch_clock`], never `SystemTime` directly — and
    /// never `fauna_core::data::Timestamp` directly either.** This crate
    /// reaches wasm (`fauna-wasm-launch` compiles the whole machine for the
    /// web SPA), where `SystemTime::now()` panics; `Timestamp` switches to the
    /// `js_sys::Date` backend there, and this crate's `Cargo.toml` already
    /// forwards fauna-core's `js` feature for exactly that. `launch_clock`
    /// wraps `Timestamp` and adds the e2e clock offset that lets a test launch
    /// this machine with a wrong clock (compiled out of release builds), so
    /// every clock read in this crate goes through it — a read that bypassed
    /// it would be one the wrong-clock witness cannot see.
    ///
    /// `now_secs_or_zero` folds an unreadable clock to `0`, which this maps to
    /// `None` rather than passing on: pruning against a 1970 `now` would read
    /// every live session as lapsed, and dropping a live own id paints the
    /// app's own session as a stranger.
    fn now_secs_opt() -> Option<u64> {
        match crate::launch_clock::now_secs_or_zero() {
            secs if secs > 0 => Some(secs as u64),
            _ => None,
        }
    }
}

// Non-exported builders. `with_connector` is the shared constructor; it can't
// live in the `#[uniffi::export]` block above because `Arc<dyn AuthConnector>`
// isn't a UniFFI-representable parameter.
impl LaunchMachine {
    fn with_connector(
        observer: Arc<dyn LaunchObserver>,
        persistence: Arc<dyn LaunchPersistence>,
        connector: Arc<dyn AuthConnector>,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            inner: Mutex::new(Inner {
                state: State::Boot,
                token: TokenStatus::None,
                last_error: None,
                identity: None,
                account_index_refusal: None,
                own_token_ids: Default::default(),
                lock_refresh_spent_for: None,
            }),
            persistence,
            observer,
            connector,
        })
    }

    /// Construct with an injected [`AuthConnector`] — tests pass a
    /// `MockAuthConnector` to drive state transitions without a network.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn new_with_connector(
        observer: Arc<dyn LaunchObserver>,
        persistence: Arc<dyn LaunchPersistence>,
        connector: Arc<dyn AuthConnector>,
    ) -> Arc<Self> {
        Self::with_connector(observer, persistence, connector)
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl LaunchMachine {
    /// Drive the launch flow. Reads from the persistence trait, branches
    /// on the four cases per `docs/goal/behavior/onboarding.md` § App-launch
    /// routing, and transitions to the appropriate phase. Case 1 (identity +
    /// nest_url) runs the silent challenge inline over the [`AuthConnector`].
    pub async fn start(&self) {
        self.transition(|inner| {
            inner.state = State::Hydrating;
        });

        // The account-index refusal outranks every row below, for the same
        // reason the pending-factory-reset row does (`onboarding.md`
        // § App-launch routing): on either verdict the registry answers no
        // session account, so `load_identity` is `None` and the table's last
        // row would drop the user into FRESH ONBOARDING — offering to make a
        // new identity to someone whose accounts are sitting intact behind a
        // blob this build merely cannot parse. Terminal, never a retry: no
        // amount of retrying reparses it.
        if let Some(refusal) = self.persistence.account_index_refusal() {
            self.transition(|inner| {
                inner.account_index_refusal = Some(refusal);
                inner.last_error = Some(account_index_refusal_message(refusal).to_string());
                inner.state = State::Offline { transient: false };
            });
            return;
        }

        let identity = self.persistence.load_identity();
        let nest_url = self.persistence.load_nest_url();
        let pending = self.persistence.load_pending_invite();

        // The pending-factory-reset row is checked BEFORE every other row
        // (common.md § Client-state recoverability, gaps CR-1 + CR-2): the box this
        // identity last authenticated against may have been wiped to fresh/unclaimed,
        // in which case the saved `nest_url`'s silent challenge would only fail
        // through to `launch_retry` — and the claim the user owes is the one this
        // slot pins with the code the client minted before dispatching the reset.
        // Resuming it is the whole reason the slot survives the crash.
        //
        // But the slot is deliberately NOT cleared when the reset *dispatch* fails,
        // because an error cannot distinguish "the nest never reset" from "the nest
        // reset and the reply was lost" (clearing in the second case is CR-1). So a
        // permanently-failed dispatch can leave a slot behind for a box that is
        // still claimed and healthy — and because this row short-circuits ahead of
        // everything else, that stale slot would hijack EVERY launch, with no in-app
        // exit (CR-2). Hence the boot reconcile: ask the box itself.
        if identity.is_some()
            && let Some(slot) = self.persistence.load_pending_factory_reset()
        {
            match self.connector.probe_claim(&slot.nest_url).await {
                // Fresh/unclaimed: the reset really landed. Resume the pre-filled
                // claim — the slot holds the code the wiped box booted with.
                ClaimProbe::Unclaimed => {
                    self.transition(|inner| {
                        inner.state = State::WizardAt {
                            entry: LaunchWizardEntry::PendingFactoryReset,
                        };
                    });
                    return;
                }
                // Claimed with a FRESH slot: `Claimed` does NOT imply stale here —
                // between the reset dispatch and the box's self-exit the box still
                // answers `Claimed`, so a launch in that window (the web flow
                // enters it by design: the admin page navigates to onboarding the
                // moment the reply lands) would otherwise clear the only copy of
                // the claim code moments before the wipe executes — CR-1 data
                // loss through CR-2's own reconcile (measured on web,
                // 2026-07-16). Time is the discriminator the probe alone cannot
                // be: within the mint grace, honor the slot (the pre-filled claim
                // page shows a visible, retryable "already claimed" error if the
                // reset really did fail); past it, take the stale-slot arm below.
                ClaimProbe::Claimed
                    if (crate::launch_clock::now_millis_or_zero() / 1000)
                        .saturating_sub(slot.minted_at_secs)
                        < crate::persistence::FACTORY_RESET_CLAIM_GRACE_SECS =>
                {
                    self.transition(|inner| {
                        inner.state = State::WizardAt {
                            entry: LaunchWizardEntry::PendingFactoryReset,
                        };
                    });
                    return;
                }
                // Claimed, slot old: the box
                // was never reset (the dispatch failed for good), or it has
                // already been re-claimed. Either way the slot is stale. Clear it
                // and take the ordinary rows — the silent challenge below just
                // logs the admin back in.
                ClaimProbe::Claimed => {
                    self.persistence.delete_pending_factory_reset();
                }
                // The box didn't answer, so we know nothing — and "nothing" must
                // not be read as "claimed". Deleting the slot here would destroy
                // the only copy of the claim code for a box that may really have
                // been wiped, which is CR-1 again, reached through a network blip.
                // Keep the slot, resume the claim (the code is what the user needs
                // to see anyway), and re-probe on the next launch.
                ClaimProbe::Unreachable => {
                    self.transition(|inner| {
                        inner.state = State::WizardAt {
                            entry: LaunchWizardEntry::PendingFactoryReset,
                        };
                    });
                    return;
                }
            }
        }

        // The awaiting-manual-dns row is checked BEFORE the silent-challenge
        // row (onboarding.md § App-launch routing): while DNS is still pending
        // the nest is unreachable by definition, so a challenge against a saved
        // `nest_url` would only fail through to `launch_retry`. Any identity +
        // slot pair resumes the deferred nest, whatever else is stored.
        if identity.is_some() && self.persistence.load_awaiting_dns().is_some() {
            self.transition(|inner| {
                inner.state = State::WizardAt {
                    entry: LaunchWizardEntry::AwaitingManualDns,
                };
            });
            return;
        }

        match (identity, nest_url, pending) {
            (Some(secret), Some(nest_url), _) => {
                self.transition(|inner| {
                    inner.state = State::SilentChallenge {
                        secret: secret.clone(),
                        nest_url: nest_url.clone(),
                        attempt: 1,
                    };
                });
                self.run_silent_challenge_phase(&secret, &nest_url).await;
            }
            (Some(_), None, Some(_)) => self.transition(|inner| {
                inner.state = State::WizardAt {
                    entry: LaunchWizardEntry::InviteRequest,
                };
            }),
            (Some(_), None, None) => self.transition(|inner| {
                inner.state = State::WizardAt {
                    entry: LaunchWizardEntry::HandleEntry,
                };
            }),
            (None, _, _) => self.transition(|inner| {
                inner.state = State::WizardAt {
                    entry: LaunchWizardEntry::IdentityChoice,
                };
            }),
        }
    }

    /// Explicit token refresh. Caller-driven (e.g., before launching a
    /// long-running operation). Mints over the silent challenge —
    /// `fauna.auth.challenge` + `fauna.auth.verify`, the launch ceremony —
    /// so a wrong client clock cannot refuse it (`login.md` § When to use
    /// which). No-op if the machine isn't in a refreshable state.
    pub async fn refresh_token(&self) {
        self.refresh_internal(RefreshReason::Manual).await;
    }

    /// HTTP layer reports a 401 on a content endpoint. Trigger an
    /// immediate refresh over the silent challenge so the caller can
    /// retry. Headline new behavior — the per-app HTTP layers today
    /// don't have a 401-reactive interceptor. No-op if not in a
    /// refreshable state.
    pub async fn notify_401(&self) {
        self.refresh_internal(RefreshReason::Got401).await;
    }

    /// Re-run the silent-challenge fast path. Meaningful only from
    /// `Offline { transient: true }` — that's the surface a user sees
    /// when the first launch attempt hit a transient network/server
    /// error. Other phases (Online, Refreshing, WizardAt, the terminal
    /// `Offline { transient: false }`, the in-flight SilentChallenge,
    /// or the early Boot/Hydrating) don't make sense to retry: either
    /// the user is already authenticated, the wizard owns the next
    /// step, or the failure is terminal and needs explicit intervention.
    /// All of those cases return without side effects.
    ///
    /// Re-reads identity + nest_url from persistence; if either is
    /// missing, also a no-op.
    pub async fn retry_silent_challenge(&self) {
        // Two states earn a retry: the unclassifiable reachability fault, and
        // the previously-signed-in row's refusal — the one terminal offline
        // whose remedy (the admin's restore) happens off this device, so a
        // retry is the user's way back in. Every other terminal stays a no-op:
        // an outdated nest, a changed identity, a succeeded key, an unreadable
        // index — nothing a retry can change.
        let should_retry = {
            let inner = self.inner.lock().unwrap();
            matches!(
                inner.state,
                State::Offline { transient: true } | State::SignInRefused
            )
        };
        if !should_retry {
            return;
        }

        let identity = self.persistence.load_identity();
        let nest_url = self.persistence.load_nest_url();
        let (secret, nest_url) = match (identity, nest_url) {
            (Some(s), Some(u)) if !u.is_empty() => (s, u),
            _ => return,
        };

        // attempt: 2 marks "this is at least the second try". Future
        // refinement: track total attempts across retries on the
        // machine itself, so UI can show "(try N)" honestly.
        self.transition(|inner| {
            inner.state = State::SilentChallenge {
                secret: secret.clone(),
                nest_url: nest_url.clone(),
                attempt: 2,
            };
        });
        self.run_silent_challenge_phase(&secret, &nest_url).await;
    }

    /// The explicit "trust this nest" recovery from
    /// [`LaunchPhase::IdentityChanged`]: forget the TOFU pin for the nest (via
    /// the connector's trust seam — the `ssh-keygen -R host` analogue), then
    /// re-run the silent challenge, which re-TOFUs against whatever identity
    /// the nest now proves. Meaningful only from `IdentityChanged`; every
    /// other phase returns without side effects — in particular the pin is
    /// NEVER forgotten outside this user-approved action (security.md
    /// § Transport trust: no silent re-pin, ever).
    pub async fn trust_nest_identity(&self) {
        let (secret, nest_url) = {
            let inner = self.inner.lock().unwrap();
            match &inner.state {
                // Rotation-chain fork evidence carries NO re-trust on this
                // surface (`box-recovery.md` § Client acceptance): forgetting
                // the pin here would hand the pin to whichever side of the
                // fork answers next. Enforced in the shared machine so every
                // app inherits the refusal even before it renders the fork
                // distinctly (`LaunchSnapshot::identity_fork`).
                State::IdentityChanged { fork: true, .. } => return,
                State::IdentityChanged {
                    secret, nest_url, ..
                } => (secret.clone(), nest_url.clone()),
                _ => return,
            }
        };
        self.connector.forget_identity_pin(&nest_url).await;
        self.transition(|inner| {
            inner.last_error = None;
            inner.state = State::SilentChallenge {
                secret: secret.clone(),
                nest_url: nest_url.clone(),
                attempt: 2,
            };
        });
        self.run_silent_challenge_phase(&secret, &nest_url).await;
    }
}

// Long-running loops — not part of the UniFFI surface. Caller spawns
// these on its own runtime (FaunaClient's tokio runtime on Linux; the
// SPA's main task on web). The FFI/WASM facade for long-running tasks
// is a Phase 2 concern; per-app adoptions wrap this loop in
// platform-native scheduling for now.
impl LaunchMachine {
    /// TTL-scheduled background refresh. Wakes 60 s before each cached
    /// bearer's deadline and calls `refresh_token()`, then re-arms
    /// with the new token's expiry. Exits when the machine moves out of
    /// `Online`/`Refreshing` (e.g. user signs out, terminal `Offline`).
    ///
    /// **Every deadline here is on this machine's own clock.** The nest's
    /// `expires_at` is never compared against a client clock read: the mint
    /// reply's `expires_in` is anchored to the launch clock at receipt
    /// ([`Self::deadline_on_own_clock`]), so a device hours ahead of the nest
    /// sleeps a full TTL like any other instead of saturating to zero and
    /// re-minting in a hot loop. Only with no readable clock does the nest's
    /// absolute deadline stand, and [`Self::refresh_sleep_secs`]'s floor
    /// bounds that worst case to one mint per buffer interval.
    ///
    /// Closes the Phase 1 deferral noted in the original A1 brief
    /// ("Token refresh loop: scheduled before TTL expiry, plus reactive
    /// on notify_401()"). The reactive path lives in `notify_401()`;
    /// this is the proactive partner.
    ///
    /// Spawning is the caller's responsibility — typically:
    ///
    /// ```ignore
    /// let machine = LaunchMachine::new(observer, persistence);
    /// machine.start().await;
    /// runtime.spawn(machine.clone().ttl_refresh_loop());
    /// ```
    ///
    /// Consumes `Arc<Self>` so the loop captures ownership of the
    /// machine handle for its lifetime.
    pub async fn ttl_refresh_loop(self: Arc<Self>) {
        // Whether the previous iteration was this loop's own refresh — the one
        // case the re-arm is floored (see `refresh_sleep_secs`).
        let mut just_refreshed = false;
        loop {
            let snap = self.snapshot();
            // Phase is the authoritative gate: any non-Online state
            // means the loop's purpose is over (user signed out, terminal
            // Offline, or back in the wizard). Refreshing is a transient
            // mid-call state — wait it out.
            match snap.phase {
                crate::snapshots::LaunchPhase::Online => {}
                crate::snapshots::LaunchPhase::Refreshing { .. } => {
                    cross_platform_sleep_secs(1).await;
                    continue;
                }
                _ => return,
            }

            let expires_at_secs = match snap.token {
                crate::snapshots::TokenStatus::Valid { expires_at_secs } => expires_at_secs,
                _ => {
                    // Online phase but token isn't Valid — only happens
                    // in test setups (set_phase_for_test sets phase
                    // without updating token). Exit cleanly rather than
                    // spin.
                    return;
                }
            };

            let now_secs = crate::launch_clock::now_millis_or_zero() / 1000;
            let sleep_secs = Self::refresh_sleep_secs(expires_at_secs, now_secs, just_refreshed);
            just_refreshed = false;
            if sleep_secs > 0 {
                cross_platform_sleep_secs(sleep_secs).await;
            }

            // Re-check after the sleep — state may have changed (user
            // signed out, another path refreshed, etc.). If `expires_at`
            // shifted, someone else refreshed; loop to re-read.
            match self.snapshot().token {
                crate::snapshots::TokenStatus::Valid {
                    expires_at_secs: new_expires,
                } if new_expires == expires_at_secs => {
                    // Same token still cached — refresh it.
                }
                crate::snapshots::TokenStatus::Valid { .. } => {
                    // Different expiry; another path already refreshed.
                    // Loop to compute the next sleep against the new
                    // expiry.
                    continue;
                }
                _ => return,
            }

            self.refresh_token().await;
            just_refreshed = true;
            // Loop continues with the post-refresh expiry (or exits if
            // the refresh failed — its transition will have moved state
            // to Offline, which the next iteration's snapshot read
            // detects).
        }
    }

    /// How long the TTL loop sleeps before refreshing a bearer whose deadline
    /// (on this machine's clock) is `expires_at_secs`, read at `now_secs`.
    ///
    /// The pre-expiry buffer is the shared client half of the token-lifetime
    /// contract, so this loop wakes on the same schedule every bearer cache
    /// refreshes on. ⚠ Read the constant — a buffer at or above the nest's
    /// TTL makes the sleep saturate to 0 and the loop spin (the pin lives
    /// beside `auth_core::TOKEN_TTL_SECS`).
    ///
    /// `just_refreshed` floors the sleep at one buffer interval: a successful
    /// refresh whose reply still computes to "already spent" can only mean the
    /// deadline and the clock disagree (a clock that jumped between receipt
    /// and read, or a nest-absolute deadline kept because the clock could not
    /// be read — the shape that used to re-mint in a hot loop), and the floor
    /// bounds that to one mint per buffer interval rather
    /// than one per iteration. The first iteration is never floored: a token
    /// genuinely inside its buffer (an app resumed from sleep) should refresh
    /// at once.
    pub(crate) fn refresh_sleep_secs(
        expires_at_secs: u64,
        now_secs: u64,
        just_refreshed: bool,
    ) -> u64 {
        let sleep = expires_at_secs
            .saturating_sub(fauna_protocol::auth::BEARER_REFRESH_BUFFER_SECS)
            .saturating_sub(now_secs);
        if just_refreshed {
            sleep.max(fauna_protocol::auth::BEARER_REFRESH_BUFFER_SECS)
        } else {
            sleep
        }
    }
}

/// Seconds-shaped call-site adapter over the shared cross-target sleep
/// (`fauna_sleep::sleep`), which owns the tokio-vs-browser-timer split this used
/// to spell out by hand.
async fn cross_platform_sleep_secs(secs: u64) {
    fauna_sleep::sleep(std::time::Duration::from_secs(secs)).await;
}

// Private helpers — not part of the UniFFI surface. UniFFI's export
// macro can't process generics on `transition`, and these are
// implementation details anyway.
impl LaunchMachine {
    async fn refresh_internal(&self, reason: RefreshReason) {
        let (secret, nest_url) = {
            let inner = self.inner.lock().unwrap();
            match &inner.state {
                State::Online { secret, .. } | State::Refreshing { secret, .. } => {
                    let secret = secret.clone();
                    let nest_url = self.persistence.load_nest_url().unwrap_or_default();
                    (secret, nest_url)
                }
                _ => return,
            }
        };
        if nest_url.is_empty() {
            return;
        }

        self.transition(|inner| {
            inner.state = State::Refreshing {
                secret: secret.clone(),
                reason,
            };
            inner.token = TokenStatus::Refreshing;
        });

        // The same ceremony the launch took — nonce-signed, no client timestamp,
        // so immune to a wrong client clock (`login.md` § When to use which).
        // No reach hint on a refresh: the domain reached this nest at launch,
        // and a `Transient` here lands `Offline { transient: true }`, whose
        // retry re-runs the launch phase with its own hint fallback.
        let outcome = self
            .connector
            .silent_challenge(&nest_url, None, &secret)
            .await;
        match outcome {
            SilentChallengeOutcome::Success(v) => {
                // The verify reply's cached metadata (`handle`/`domain`/`tier`)
                // is ignored on a refresh: the launch phase owns the persisted
                // identity, and a mid-session rename shows on the next launch —
                // the behaviour the handshake refresh (which carried no metadata)
                // already had.
                let expires_at_secs = Self::deadline_on_own_clock(v.expires_in, v.expires_at);
                self.transition(|inner| {
                    inner.last_error = None;
                    inner.lock_refresh_spent_for = None;
                    inner.token = TokenStatus::Valid { expires_at_secs };
                    // The predecessor stays in the set: it is listed nest-side
                    // until it expires, and both rows are this app's.
                    inner
                        .own_token_ids
                        .record(v.token_id.clone(), expires_at_secs);
                    inner.own_token_ids.prune(Self::now_secs_opt());
                    inner.state = State::Online {
                        secret: secret.clone(),
                        bearer: v.token,
                        token_id: v.token_id,
                        expires_at_secs,
                    };
                });
            }
            SilentChallengeOutcome::NotRegistered => {
                // The nest this session was signed in to no longer signs the
                // identity in — suspended, or removed; the client cannot tell
                // (no oracle on the wire, by design). The mid-session twin of
                // the launch arm's previously-signed-in row, landed on the SAME
                // surface (`security.md` § Post-auth surfacing): terminal for
                // the scheduler, the verdict on the `sign_in_refused` side
                // channel, and the localized copy — never the raw wire code —
                // in `last_error`. A retry after the admin's restore is the
                // way back in (`retry_silent_challenge`).
                self.transition(|inner| {
                    inner.token = TokenStatus::Expired;
                    inner.state = State::SignInRefused;
                    Self::set_last_error(
                        inner,
                        fauna_i18n::strings::onboarding::launch::SIGN_IN_REFUSED.into(),
                    );
                });
            }
            SilentChallengeOutcome::SecretInvalid { error } => {
                self.transition(|inner| {
                    inner.token = TokenStatus::Expired;
                    inner.state = State::Offline { transient: false };
                    Self::set_last_error(inner, error);
                });
            }
            SilentChallengeOutcome::NeedsUpdate { message } => {
                // The nest is in degraded "needs-update" mode. Land in the
                // non-transient offline state (no retry spin) and paint the
                // localized, actionable update banner — distinguishable from a
                // transient connectivity blip (version-compatibility.md Dim 4).
                self.transition(|inner| {
                    inner.token = TokenStatus::Expired;
                    inner.state = State::Offline { transient: false };
                    Self::set_last_error(inner, message);
                });
            }
            SilentChallengeOutcome::Transient { error } => {
                self.transition(|inner| {
                    inner.state = State::Offline { transient: true };
                    Self::set_last_error(inner, error);
                });
            }
            SilentChallengeOutcome::IdentityChanged {
                host,
                pinned_hex,
                seen_hex,
                fork,
            } => {
                // A mid-session identity change is the same MITM signal as a
                // launch-time one — drop the token and block on the explicit
                // re-trust surface (never keep talking over an untrusted
                // connection; § Connection-teardown rule).
                self.transition(|inner| {
                    inner.token = TokenStatus::Expired;
                    inner.state = State::IdentityChanged {
                        secret: secret.clone(),
                        nest_url: nest_url.clone(),
                        pinned_hex,
                        seen_hex,
                        fork,
                    };
                    Self::set_last_error(
                        inner,
                        format!("nest identity changed for {host} — explicit re-trust required"),
                    );
                });
            }
            SilentChallengeOutcome::Superseded { new_actor_id_hex } => {
                // The account was re-pointed to a successor identity. Terminal:
                // drop the token and park in `Superseded`, which projects to the
                // non-retry offline phase. Before this arm the refusal fell into
                // `Transient`, so the machine re-signed a permanently-doomed
                // ceremony for as long as the app ran.
                self.transition(|inner| {
                    inner.token = TokenStatus::Expired;
                    Self::set_last_error(
                        inner,
                        format!(
                            "this identity was succeeded — import the new identity \
                             {new_actor_id_hex}"
                        ),
                    );
                    inner.state = State::Superseded { new_actor_id_hex };
                });
            }
            SilentChallengeOutcome::Locked { locked_until_secs } => {
                // Verify refused a locked account (`login.md` § Silent
                // Challenge). Terminal until `locked_until`: re-signing before
                // then only re-earns the refusal, so park rather than back off.
                self.park_locked(locked_until_secs);
            }
        }
    }

    /// Land a `fauna.auth.account_locked` refusal: token dropped, the unlock
    /// time on the snapshot side channel, the time-free localized copy in
    /// `last_error` — and the one refresh at `locked_until` armed. Shared by
    /// the refresh arm and the launch arm.
    fn park_locked(&self, locked_until_secs: u64) {
        self.transition(|inner| {
            inner.token = TokenStatus::Expired;
            Self::set_last_error(
                inner,
                fauna_i18n::strings::onboarding::launch::ACCOUNT_LOCKED.into(),
            );
            inner.state = State::Locked { locked_until_secs };
        });
        self.arm_lock_refresh(locked_until_secs);
    }

    /// Arm the one refresh `devices.md` § The locked state schedules at
    /// `locked_until`. The machine arms it itself rather than leaving it to a
    /// caller-spawned loop (the shape [`Self::ttl_refresh_loop`] has), because a
    /// lock is met on the launch path too — before any app has a session to
    /// hang a loop on — and four of the seven apps never run that loop.
    ///
    /// Nothing is armed for a lock whose refresh has already run: the nest
    /// answered the refresh with the same unlock time, so another would only
    /// re-earn it.
    fn arm_lock_refresh(&self, locked_until_secs: u64) {
        if self.inner.lock().unwrap().lock_refresh_spent_for == Some(locked_until_secs) {
            return;
        }
        let task = Self::lock_refresh_task(self.me.clone(), locked_until_secs);
        #[cfg(not(target_arch = "wasm32"))]
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(task);
            }
            // Every path that parks runs inside an async export or a caller's
            // runtime, so this is a caller driving the machine from a bare
            // executor — the lock still stands, it just clears on relaunch.
            Err(_) => tracing::warn!(
                target: "fauna_launch",
                "no runtime to schedule the refresh at locked_until on; the lock clears on the \
                 next launch"
            ),
        }
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(task);
    }

    /// Sleep until `locked_until`, then run the one refresh. Holds the machine
    /// weakly and re-reads the state on every wake, so a machine that was
    /// dropped, or that left this lock by any other route, ends the task.
    async fn lock_refresh_task(me: Weak<Self>, locked_until_secs: u64) {
        loop {
            let now_secs = crate::launch_clock::now_millis_or_zero() / 1000;
            let sleep_secs = Self::lock_refresh_sleep_secs(locked_until_secs, now_secs);
            if sleep_secs == 0 {
                break;
            }
            cross_platform_sleep_secs(sleep_secs.min(LOCK_RECHECK_SECS)).await;
            let Some(machine) = me.upgrade() else { return };
            if machine.snapshot().locked_until_secs != Some(locked_until_secs) {
                return;
            }
        }
        let Some(machine) = me.upgrade() else { return };
        machine.refresh_after_lock(locked_until_secs).await;
    }

    /// How long the scheduled lock refresh still has to wait, read at
    /// `now_secs` on this machine's clock. Zero once the lock has lapsed by
    /// [`LOCK_LAPSE_GRACE_SECS`]. An unreadable clock reads as `now = 0`, which
    /// never comes due: with no clock the machine cannot know the lock lapsed,
    /// and signing on a guess is the doomed ceremony this state exists to stop.
    pub(crate) fn lock_refresh_sleep_secs(locked_until_secs: u64, now_secs: u64) -> u64 {
        locked_until_secs
            .saturating_add(LOCK_LAPSE_GRACE_SECS)
            .saturating_sub(now_secs)
    }

    /// The one refresh at `locked_until`: the launch ceremony again, from the
    /// persisted identity (a locked state holds no secret). Claims the attempt
    /// and leaves `Locked` under one lock, so two tasks armed for the same lock
    /// — a relaunch re-parks it while the first still sleeps — run it once.
    async fn refresh_after_lock(&self, locked_until_secs: u64) {
        let (secret, nest_url) = match (
            self.persistence.load_identity(),
            self.persistence.load_nest_url(),
        ) {
            (Some(s), Some(u)) if !u.is_empty() => (s, u),
            _ => return,
        };
        let claimed = {
            let mut inner = self.inner.lock().unwrap();
            let still_this_lock = matches!(
                inner.state,
                State::Locked { locked_until_secs: l } if l == locked_until_secs
            );
            if still_this_lock && inner.lock_refresh_spent_for != Some(locked_until_secs) {
                inner.lock_refresh_spent_for = Some(locked_until_secs);
                inner.state = State::SilentChallenge {
                    secret: secret.clone(),
                    nest_url: nest_url.clone(),
                    attempt: 2,
                };
                true
            } else {
                false
            }
        };
        if !claimed {
            return;
        }
        self.observer.on_changed();
        self.run_silent_challenge_phase(&secret, &nest_url).await;
    }

    /// One silent challenge, applying the **reach hint**'s dial rule
    /// (`onboarding.md` § Reach hint): the domain first, always; the hint only
    /// when that domain dial failed to *reach* anything; and the hint deleted
    /// the moment the domain answers.
    ///
    /// Three properties this shape buys, none of which survive being re-decided
    /// per app — which is why the rule is here rather than in seven call sites:
    ///
    /// - **A stale hint is harmless.** A box re-provisioned onto a new address
    ///   leaves a hint pointing at someone else's machine; because the domain is
    ///   tried first and the hint only ever answers a *reachability* failure,
    ///   the stale address is reached only when the real box could not be, and
    ///   the pin refuses it there.
    /// - **Only `Transient` earns the fallback.** `NotRegistered`, `NeedsUpdate`
    ///   and `Superseded` are the nest *answering* — the domain resolved, so a
    ///   second dial would ask a question already answered. `IdentityChanged` is
    ///   terminal by design (`security.md` § Transport trust) and re-dialling it
    ///   by another authority is precisely the silent re-pin the pin exists to
    ///   prevent.
    /// - **Deletion needs the domain to have reached the RIGHT box.** A hint
    ///   that failed is a failed fallback, not a wrong address; deleting it
    ///   there would throw away the only way to reach a box whose DNS is still
    ///   hours out. And `IdentityChanged` — though the socket did open, so the
    ///   domain does resolve — is the one answer where deleting would be
    ///   actively harmful: the pin refuses precisely when the domain now points
    ///   at a *different* machine, and the hint is then the only address left
    ///   that still reaches the real one.
    async fn run_silent_challenge_phase(&self, secret: &[u8], nest_url: &str) {
        let outcome = self
            .connector
            .silent_challenge(nest_url, None, secret)
            .await;
        let outcome = match outcome {
            SilentChallengeOutcome::Transient { .. } => match self.persistence.load_reach_ipv4() {
                Some(hint) => {
                    self.connector
                        .silent_challenge(nest_url, Some(&hint), secret)
                        .await
                }
                None => outcome,
            },
            changed @ SilentChallengeOutcome::IdentityChanged { .. } => changed,
            // The domain reached the pinned box and it answered. Whatever it
            // said, the account no longer needs to remember an address.
            answered => {
                self.persistence.delete_reach_ipv4();
                answered
            }
        };
        match outcome {
            SilentChallengeOutcome::Success(v) => {
                self.persistence.save_authenticated(
                    nest_url.to_string(),
                    v.handle.clone(),
                    v.domain.clone(),
                    v.tier.clone(),
                );
                // There is no storage mode to route on (`storage-modes.md`): the
                // `storage_mode_pending` flag left the wire 2026-09-24, and a
                // successful verify lands Online.
                // Anchored to this machine's clock at receipt — the deadline
                // every later read compares against (`ttl_refresh_loop`,
                // `fresh_bearer`, the own-session pruning).
                let expires_at_secs = Self::deadline_on_own_clock(v.expires_in, v.expires_at);
                self.transition(|inner| {
                    inner.token = TokenStatus::Valid { expires_at_secs };
                    inner.last_error = None;
                    inner.lock_refresh_spent_for = None;
                    // Publish the confirmed identity on the same notification
                    // that turns the app online, so an observer never sees
                    // Online-with-a-stale-address even for one frame.
                    inner.identity = Some(LaunchIdentity {
                        handle: v.handle,
                        domain: v.domain,
                        tier: v.tier,
                    });
                    inner
                        .own_token_ids
                        .record(v.token_id.clone(), expires_at_secs);
                    inner.own_token_ids.prune(Self::now_secs_opt());
                    inner.state = State::Online {
                        secret: secret.to_vec(),
                        bearer: v.token,
                        token_id: v.token_id,
                        expires_at_secs,
                    };
                });
            }
            SilentChallengeOutcome::NotRegistered => {
                // 404 on /verify means the actor isn't registered on this
                // nest. Two sub-branches per
                // `docs/goal/behavior/onboarding.md` § App-launch routing —
                // silent-challenge fallback table:
                //   * claimed nest → wizard at invite_request (admin can
                //     issue an invite).
                //   * unclaimed nest → wizard at claim_code (no admin
                //     exists yet; user must claim).
                // Mirrors the handle-check arm in
                // `fauna-onboarding-machine`'s `start_handle_check`:
                // any connect / transport / server failure → assume claimed
                // (the safer default — invite_request is graceful, while
                // claim_code only works when a live claim-code file sits
                // next to the nest). The probe rides the pre-identity
                // anonymous WS-RPC connection (`fauna.setup.status`); the
                // former HTTP `GET /api/v1/setup-status` was retired.
                //
                // Only a definite `Unclaimed` routes to claim_code; `Unreachable`
                // falls in with `Claimed`, which is this caller's safe default (and
                // the opposite of the pending-factory-reset reconcile's — the reason
                // the probe reports all three outcomes rather than a bool).
                //
                // The claimed branch is NOT the invite wizard any more (the
                // previously-signed-in row, designed 2026-09-24): this row runs
                // only on a stored identity + nest_url, which the store writes
                // only after a real sign-in or claim here — so the nest is one
                // this app was signed in to, and an opaque `not_registered`
                // from it means it no longer signs this identity in
                // (suspended, or removed; no oracle on the wire, by design).
                // The wizard would offer to re-join a nest that already holds
                // the account, and a suspended actor's invite submit is refused
                // as already registered. Land the honest surface instead: the
                // `sign_in_refused` side channel + localized copy, retry-able
                // once the admin restores (`retry_silent_challenge`). The
                // deleted-then-relaunched user reaches the invite path through
                // "Use a different nest" → handle_entry, where the wizard's own
                // handle check routes them.
                match self.connector.probe_claim(nest_url).await {
                    ClaimProbe::Unclaimed => {
                        self.transition(|inner| {
                            inner.state = State::WizardAt {
                                entry: LaunchWizardEntry::ClaimCode,
                            };
                        });
                    }
                    ClaimProbe::Claimed | ClaimProbe::Unreachable => {
                        self.transition(|inner| {
                            inner.state = State::SignInRefused;
                            Self::set_last_error(
                                inner,
                                fauna_i18n::strings::onboarding::launch::SIGN_IN_REFUSED.into(),
                            );
                        });
                    }
                }
            }
            SilentChallengeOutcome::Transient { error } => {
                self.transition(|inner| {
                    inner.state = State::Offline { transient: true };
                    Self::set_last_error(inner, error);
                });
            }
            SilentChallengeOutcome::NeedsUpdate { message } => {
                // Degraded nest — non-transient offline + actionable update
                // banner (the silent-challenge twin of the token-refresh arm).
                self.transition(|inner| {
                    inner.token = TokenStatus::Expired;
                    inner.state = State::Offline { transient: false };
                    Self::set_last_error(inner, message);
                });
            }
            SilentChallengeOutcome::SecretInvalid { error } => {
                self.transition(|inner| {
                    inner.state = State::Offline { transient: false };
                    Self::set_last_error(inner, error);
                });
            }
            SilentChallengeOutcome::IdentityChanged {
                host,
                pinned_hex,
                seen_hex,
                fork,
            } => {
                // The known_hosts case (security.md § Transport trust): auto-entry is blocked; the client renders the
                // `launch_identity_changed` surface. Recovery is explicit —
                // `trust_nest_identity()` (forget + re-TOFU + re-challenge)
                // or the wizard fallthrough. Keep secret + nest_url on the
                // state so the re-trust can re-run without re-hydrating.
                // (`fork: true` — rotation-chain fork evidence — additionally
                // disables the re-trust arm; box-recovery.md § Client
                // acceptance.)
                self.transition(|inner| {
                    inner.state = State::IdentityChanged {
                        secret: secret.to_vec(),
                        nest_url: nest_url.to_string(),
                        pinned_hex,
                        seen_hex,
                        fork,
                    };
                    Self::set_last_error(
                        inner,
                        format!("nest identity changed for {host} — explicit re-trust required"),
                    );
                });
            }
            SilentChallengeOutcome::Superseded { new_actor_id_hex } => {
                // Same terminal verdict as the token-refresh arm, reached by the
                // other ceremony. The secret is fine and re-signing is futile —
                // the remedy is importing the successor, not retrying.
                self.transition(|inner| {
                    inner.token = TokenStatus::Expired;
                    Self::set_last_error(
                        inner,
                        format!(
                            "this identity was succeeded — import the new identity \
                             {new_actor_id_hex}"
                        ),
                    );
                    inner.state = State::Superseded { new_actor_id_hex };
                });
            }
            SilentChallengeOutcome::Locked { locked_until_secs } => {
                self.park_locked(locked_until_secs);
            }
        }
    }

    fn transition<F: FnOnce(&mut Inner)>(&self, f: F) {
        {
            let mut inner = self.inner.lock().unwrap();
            f(&mut inner);
        }
        self.observer.on_changed();
    }

    /// Set the launch error banner **and** log it once at the producer. The
    /// per-app launch screen paints `last_error` reactively, so logging in
    /// the render would re-fire on every repaint (observability.md § Log on the
    /// *event*, not the *paint*). Funnelling every `last_error` write through
    /// here logs once, on the transition that sets it; shared Rust → all six
    /// apps. Takes `&mut Inner` so it composes inside a `transition` closure
    /// without re-locking. The message is the same user-facing string the
    /// banner shows (redaction-safe — already displayed).
    fn set_last_error(inner: &mut Inner, error: String) {
        tracing::warn!(target: "fauna_launch", "{error}");
        inner.last_error = Some(error);
    }
}

// ---------------------------------------------------------------------------
// Test helpers — exposed behind `test-helpers` so integration tests can
// drive the machine into specific states for downstream HTTP / refresh
// work without re-deriving them through the routing decision.
// ---------------------------------------------------------------------------

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[cfg_attr(all(feature = "uniffi", feature = "test-helpers"), uniffi::export)]
impl LaunchMachine {
    /// Force the visible phase. Intended for tests that need to exercise
    /// downstream behavior without going through `start()`.
    pub fn set_phase_for_test(&self, phase: LaunchPhase) {
        self.transition(|inner| {
            inner.state = match phase {
                LaunchPhase::Boot => State::Boot,
                LaunchPhase::Hydrating => State::Hydrating,
                LaunchPhase::SilentChallenge { attempt } => State::SilentChallenge {
                    secret: vec![],
                    nest_url: String::new(),
                    attempt,
                },
                LaunchPhase::Refreshing { reason } => State::Refreshing {
                    secret: vec![],
                    reason,
                },
                LaunchPhase::Online => State::Online {
                    secret: vec![],
                    bearer: String::new(),
                    token_id: String::new(),
                    expires_at_secs: 0,
                },
                LaunchPhase::Offline { transient } => State::Offline { transient },
                LaunchPhase::WizardAt { entry } => State::WizardAt { entry },
                LaunchPhase::IdentityChanged {
                    pinned_hex,
                    seen_hex,
                } => State::IdentityChanged {
                    secret: vec![],
                    nest_url: String::new(),
                    pinned_hex,
                    seen_hex,
                    fork: false,
                },
            };
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observer::NullObserver;
    use crate::persistence::InMemoryPersistence;

    #[tokio::test]
    async fn start_with_empty_persistence_routes_to_identity_choice() {
        // Sanity unit test alongside the integration tests in
        // tests/four_case_branch.rs — keeps the lib self-checking even
        // when the test-helpers feature isn't enabled.
        let m = LaunchMachine::new(Arc::new(NullObserver), Arc::new(InMemoryPersistence::new()));
        m.start().await;
        assert_eq!(
            m.snapshot().phase,
            LaunchPhase::WizardAt {
                entry: LaunchWizardEntry::IdentityChoice
            }
        );
    }

    #[test]
    fn snapshot_initial_is_boot() {
        let m = LaunchMachine::new(Arc::new(NullObserver), Arc::new(InMemoryPersistence::new()));
        assert_eq!(m.snapshot().phase, LaunchPhase::Boot);
        assert_eq!(m.snapshot().token, TokenStatus::None);
    }

    /// Rotation-chain fork evidence carries NO re-trust (`box-recovery.md`
    /// § Client acceptance): `trust_nest_identity()` must refuse — forgetting
    /// the pin would hand it to whichever side of the fork answers next — and
    /// the snapshot must expose the fork so apps can render the surface
    /// honestly. The ordinary changed case keeps its re-trust (pinned by
    /// `four_case_branch.rs`'s existing coverage); this pins the refusal.
    #[tokio::test]
    async fn fork_evidence_blocks_the_re_trust_action() {
        let m = LaunchMachine::new(Arc::new(NullObserver), Arc::new(InMemoryPersistence::new()));
        m.transition(|inner| {
            inner.state = State::IdentityChanged {
                secret: vec![9; 32],
                nest_url: "https://forked.example".into(),
                pinned_hex: "aa".repeat(32),
                seen_hex: Some("bb".repeat(32)),
                fork: true,
            };
        });
        let snap = m.snapshot();
        assert!(
            snap.identity_fork,
            "the fork rides the snapshot side channel"
        );
        assert!(
            matches!(snap.phase, LaunchPhase::IdentityChanged { .. }),
            "fork is NOT a new phase — the warning surface is shared"
        );

        m.trust_nest_identity().await;
        assert!(
            matches!(m.snapshot().phase, LaunchPhase::IdentityChanged { .. }),
            "re-trust must refuse on fork evidence — the state may not move"
        );
        assert!(m.snapshot().identity_fork, "still marked after the refusal");
    }

    /// The TTL loop's re-arm: the ordinary case sleeps to one buffer before the
    /// deadline; a token already inside its buffer refreshes at once on the
    /// FIRST iteration; and a refresh that still computes to "spent" (a
    /// clock that jumped, or a nest-absolute deadline kept for want of a
    /// readable clock) is floored to one buffer interval, so the loop can
    /// no longer re-mint per iteration.
    #[test]
    fn refresh_sleep_is_floored_only_after_the_loops_own_refresh() {
        let buffer = fauna_protocol::auth::BEARER_REFRESH_BUFFER_SECS;
        let now = 1_700_000_000;
        assert_eq!(
            LaunchMachine::refresh_sleep_secs(now + 3600, now, false),
            3600 - buffer
        );
        assert_eq!(
            LaunchMachine::refresh_sleep_secs(now + 3600, now, true),
            3600 - buffer,
            "the floor never lengthens a healthy re-arm"
        );
        assert_eq!(
            LaunchMachine::refresh_sleep_secs(now + 5, now, false),
            0,
            "inside the buffer on the first pass → refresh now"
        );
        // The deadline is hours in this clock's past, yet the loop just
        // refreshed.
        assert_eq!(
            LaunchMachine::refresh_sleep_secs(now + 3600, now + 6 * 3600, true),
            buffer,
            "a just-refreshed 'spent' token waits one buffer, not zero"
        );
    }

    /// The scheduled lock refresh comes due a grace past `locked_until` on this
    /// machine's clock, and never on a clock that cannot be read.
    #[test]
    fn the_lock_refresh_comes_due_a_grace_past_locked_until() {
        let now = 1_700_000_000u64;
        assert_eq!(
            LaunchMachine::lock_refresh_sleep_secs(now + 3600, now),
            3600 + LOCK_LAPSE_GRACE_SECS
        );
        assert_eq!(
            LaunchMachine::lock_refresh_sleep_secs(now, now),
            LOCK_LAPSE_GRACE_SECS,
            "at the unlock time itself the grace is still owed"
        );
        assert_eq!(
            LaunchMachine::lock_refresh_sleep_secs(now - LOCK_LAPSE_GRACE_SECS, now),
            0
        );
        assert_eq!(LaunchMachine::lock_refresh_sleep_secs(now - 3600, now), 0);
        assert!(
            LaunchMachine::lock_refresh_sleep_secs(now + 3600, 0) > 0,
            "an unreadable clock (now = 0) never reads a lock as lapsed"
        );
    }

    /// A mint reply's `expires_in` lands a deadline on THIS machine's clock,
    /// whatever the nest's absolute `expires_at` says. Pinned through the
    /// public `TokenStatus` the loop and `fresh_bearer` read, over a scripted
    /// launch. (The older-nest arm — no `expires_in`, `expires_at` kept —
    /// retired 2026-09-24 with the field's fallback.)
    #[tokio::test]
    async fn a_mint_reply_is_anchored_to_the_machines_own_clock() {
        use crate::connector::MockAuthConnector;
        use fauna_protocol::auth::VerifyReply;
        let now = crate::launch_clock::now_secs_or_zero() as u64;
        // The nest's clock is six hours BEHIND this machine's.
        let nest_expires_at = now - 6 * 3600 + 3600;
        let connector = MockAuthConnector::new().push_silent_challenge(
            SilentChallengeOutcome::Success(VerifyReply {
                token: "t".into(),
                token_id: "0".repeat(16),
                expires_at: nest_expires_at,
                expires_in: 3600,
                ..Default::default()
            }),
        );
        let p = Arc::new(
            InMemoryPersistence::new()
                .with_identity(vec![0x42; 32])
                .with_nest_url("https://nest.example"),
        );
        let anchored =
            LaunchMachine::new_with_connector(Arc::new(NullObserver), p, Arc::new(connector));
        anchored.start().await;
        assert_eq!(anchored.snapshot().phase, LaunchPhase::Online);
        match anchored.snapshot().token {
            TokenStatus::Valid { expires_at_secs } => assert!(
                (now + 3600 - 5..=now + 3600 + 5).contains(&expires_at_secs),
                "deadline must be ~now+3600 on this clock, got {expires_at_secs} (now {now})"
            ),
            other => panic!("expected Valid, got {other:?}"),
        }
        assert_eq!(
            anchored.fresh_bearer().as_deref(),
            Some("t"),
            "a full TTL away on this clock → served without a refresh"
        );
        assert_eq!(
            anchored.own_token_ids(),
            vec!["0".repeat(16)],
            "the own-session id is pruned on the same clock, so it is live"
        );
    }

    #[test]
    fn only_a_fork_sets_the_snapshot_flag() {
        let m = LaunchMachine::new(Arc::new(NullObserver), Arc::new(InMemoryPersistence::new()));
        m.transition(|inner| {
            inner.state = State::IdentityChanged {
                secret: vec![9; 32],
                nest_url: "https://changed.example".into(),
                pinned_hex: "aa".repeat(32),
                seen_hex: Some("bb".repeat(32)),
                fork: false,
            };
        });
        assert!(!m.snapshot().identity_fork);
    }
}
