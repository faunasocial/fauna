//! [`BearerSource`] — "give me a currently-valid bearer / I just got a 401" —
//! and its impls.
//!
//! [`ReqwestNestContentApi`](crate::content::ReqwestNestContentApi) is generic
//! over this so the same content impl serves both `LaunchMachine`-driven
//! clients (Linux now; macOS/iOS/Windows later) and tests:
//!
//! - [`LaunchMachineBearer`] (feature `launch-machine`) — wraps an
//!   `Arc<fauna_launch_machine::LaunchMachine>`: serves the bearer the machine
//!   holds while the machine itself calls it fresh (`LaunchMachine::fresh_bearer`
//!   — the 60 s pre-expiry buffer on the machine's own clock; falls back to
//!   `LaunchMachine::refresh_token` near/after expiry), and on a 401 calls
//!   `LaunchMachine::notify_401` (an immediate silent-challenge refresh — the
//!   reactive path the TTL pre-expiry timer can't cover).
//! - [`StaticBearer`] — a fixed token; tests only. `notify_401` is a no-op,
//!   so a 401 surfaces after one (re-)try with the same token — exactly the
//!   "never an infinite retry" guarantee, exercised cheaply.
//!
//! The keypair-signing HTTP minter (`KeypairBearer`, which POSTed
//! `/api/v1/auth/token`) was removed when every Rust client moved to minting
//! the bearer over WS-RPC: `fauna-client`'s `AuthClient` via its
//! `WsChallengeBearer` (`fauna.auth.handshake`), and the launch flow via the
//! machine's silent challenge (`fauna.auth.challenge` + `verify`, launch and
//! refresh alike since 2026-09-21 — `login.md` § When to use which). The bare
//! HTTP `/auth/token` route survives nest-side only for the not-yet-migrated
//! native wire consumers (windows, apple test-helpers).

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::ApiError;

/// Source of a bearer token for [`crate::content::ReqwestNestContentApi`].
#[async_trait]
pub trait BearerSource: Send + Sync {
    /// A currently-valid bearer, refreshing proactively if the cached one is
    /// within `fauna_protocol::auth::BEARER_REFRESH_BUFFER_SECS` of expiry
    /// (that constant is the one owner; this crate names it only under the
    /// `launch-machine` feature, hence no intra-doc link). `Err(ApiError::Transport(_))` if a bearer
    /// can't be produced (no identity, refresh failed, transport error).
    async fn bearer(&self) -> Result<String, ApiError>;

    /// [`Self::bearer`] plus **the bearer's expiry** — unix seconds on THIS
    /// machine's clock, anchored at receipt (`docs/goal/behavior/login.md`
    /// § Token lifetime on the client's clock), never the nest's absolute
    /// `expires_at`. Read in one call, so the pair can never describe two
    /// different bearers across a refresh.
    ///
    /// `None` expiry — the default — means "cannot say": a [`StaticBearer`]
    /// holds an opaque token it did not mint. Every source that mints its own
    /// bearer overrides this. The one consumer that needs the expiry, the sync
    /// agent's provisioning hook (`fauna_sync_engine::share_glue::AgentBearerSource`),
    /// treats a `None` as *no bearer* and never guesses a deadline.
    async fn bearer_with_expiry(&self) -> Result<(String, Option<u64>), ApiError> {
        self.bearer().await.map(|token| (token, None))
    }

    /// The bearer this last returned got a `401`. React — invalidate the
    /// cache / refresh — so the next [`Self::bearer`] yields a fresh one.
    /// Default: no-op (e.g. [`StaticBearer`], which can't recover; the
    /// content layer then surfaces the original `401`).
    async fn notify_401(&self) {}

    /// The actor id the bearers this source mints will name, when the source
    /// can say. `None` — the default — means "cannot say", not "no actor": a
    /// [`StaticBearer`] holds an opaque token it cannot attribute, so it opts
    /// out rather than guessing.
    ///
    /// Deliberately **sync**: its one consumer is
    /// `fauna_client::AuthClient::with_bearer_source`, which cross-checks this
    /// against the keypair it was handed at construction time — a plain
    /// function, no runtime in reach. See that constructor for why the two can
    /// disagree at all, and what it costs when they do.
    fn bearer_actor_id(&self) -> Option<[u8; 32]> {
        None
    }

    /// **The session ids this source minted itself** — the current one plus
    /// every earlier own id not yet expired
    /// (`docs/goal/behavior/devices.md` § The client's own session). A sessions
    /// surface folds all of them into the one "this app" row, so a renewal
    /// never paints the app's own predecessor as an unknown second session.
    /// Default: empty — "cannot say", not "no sessions", exactly as
    /// [`Self::bearer_actor_id`]'s `None` is. [`StaticBearer`] holds an opaque
    /// token it did not mint, so it opts out rather than guessing.
    ///
    /// Deliberately **async**, unlike `bearer_actor_id`: that answer is fixed
    /// at construction, whereas this set changes on every mint and lives behind
    /// the holder's own async lock. A sync accessor would have to `try_read`
    /// it — which under contention returns the empty default and silently
    /// under-reports the caller's own sessions — or `blocking_read` it, which
    /// panics on a runtime thread.
    async fn own_token_ids(&self) -> Vec<String> {
        Vec::new()
    }

    /// **The current session id**, read at call time — what `revoke_all`'s
    /// `keep_token_id` is filled from, never a previously painted list: a
    /// renewal between paint and press must not name a dead token
    /// (`devices.md` § The client's own session). `None` when this source
    /// cannot say, or holds no live bearer yet.
    async fn current_token_id(&self) -> Option<String> {
        None
    }
}

/// Forward through an `Arc`, so an already-shared `Arc<dyn BearerSource>` (e.g.
/// the one `fauna_client::AuthClient` holds + hands its own content API) can
/// itself be the `B` of a [`ReqwestNestContentApi`](crate::content::ReqwestNestContentApi).
/// A second content API built from it then shares the client's *one* bearer
/// cache (one token mint, one TTL-refresh loop, one 401-reactive path) rather
/// than minting a parallel token.
#[async_trait]
impl<T: BearerSource + ?Sized> BearerSource for Arc<T> {
    async fn bearer(&self) -> Result<String, ApiError> {
        (**self).bearer().await
    }
    async fn bearer_with_expiry(&self) -> Result<(String, Option<u64>), ApiError> {
        (**self).bearer_with_expiry().await
    }
    async fn notify_401(&self) {
        (**self).notify_401().await;
    }
    fn bearer_actor_id(&self) -> Option<[u8; 32]> {
        (**self).bearer_actor_id()
    }
    async fn own_token_ids(&self) -> Vec<String> {
        (**self).own_token_ids().await
    }
    async fn current_token_id(&self) -> Option<String> {
        (**self).current_token_id().await
    }
}

// ---------------------------------------------------------------------------
// StaticBearer
// ---------------------------------------------------------------------------

/// A fixed bearer. **Tests only.** Never refreshes, never reacts to a 401.
pub struct StaticBearer(pub String);

#[async_trait]
impl BearerSource for StaticBearer {
    async fn bearer(&self) -> Result<String, ApiError> {
        Ok(self.0.clone())
    }
}

// ---------------------------------------------------------------------------
// LaunchMachineBearer
// ---------------------------------------------------------------------------

/// [`BearerSource`] over an `Arc<fauna_launch_machine::LaunchMachine>`. The
/// launch flow drives the machine to `Online` via the silent challenge before
/// constructing the content API; this serves the resulting bearer while the
/// machine calls it fresh (`LaunchMachine::fresh_bearer` — the pre-expiry
/// buffer applied on the machine's OWN clock, the one its deadline was anchored
/// to; refreshing via `LaunchMachine::refresh_token` otherwise), and
/// [`Self::notify_401`] calls `LaunchMachine::notify_401` — an immediate
/// silent-challenge refresh, the reactive path the machine's TTL pre-expiry
/// timer can't cover. Lifts `apps/fauna-linux/src/nest_content_api::ensure_bearer`.
#[cfg(feature = "launch-machine")]
pub struct LaunchMachineBearer(pub std::sync::Arc<fauna_launch_machine::LaunchMachine>);

#[cfg(feature = "launch-machine")]
#[async_trait]
impl BearerSource for LaunchMachineBearer {
    async fn bearer(&self) -> Result<String, ApiError> {
        self.bearer_with_expiry().await.map(|(token, _)| token)
    }

    /// The machine holds the bearer's deadline on its own clock (anchored at
    /// receipt), so this source can always say — its expiry is never `None`.
    async fn bearer_with_expiry(&self) -> Result<(String, Option<u64>), ApiError> {
        if let Some((b, exp)) = self.0.fresh_bearer_with_expiry() {
            return Ok((b, Some(exp)));
        }
        // Expired / missing / near expiry — refresh over the silent challenge.
        self.0.refresh_token().await;
        if let Some((b, exp)) = self.0.current_bearer_with_expiry() {
            return Ok((b, Some(exp)));
        }
        // `refresh_token` no-ops unless the machine is Online/Refreshing, so it
        // can't bootstrap a bearer when the initial silent challenge parked the
        // machine in `Offline { transient: true }` — the common case right
        // after a factory-reset reclaim, when the first challenge raced the
        // nest's restart. Re-run the silent challenge to recover.
        // `retry_silent_challenge` self-guards to `Offline { transient: true }`,
        // so a terminal SecretInvalid / Unauthorized / Locked (or an in-flight /
        // wizard state) is left untouched — we never hammer the nest with a
        // known-bad secret. This keeps the client reconnectable after a factory
        // reset (the client-state-recoverability invariant) on both the HTTP and
        // WS surfaces, which share this `BearerSource`.
        self.0.retry_silent_challenge().await;
        if let Some((b, exp)) = self.0.current_bearer_with_expiry() {
            return Ok((b, Some(exp)));
        }
        // Why the machine has no bearer decides what this failure *means*. The
        // refresh above may have met the nest's pinned identity changing, which
        // parks the machine in `IdentityChanged` — the `known_hosts` verdict.
        // Reporting that as a generic transport failure is the seam that kept
        // linux and tui blind to it: they hand this bearer in via
        // `AuthClient::with_bearer_source`, so the `SupersededLatch`-style side
        // channel on `WsChallengeBearer` cannot see their mint at all. The
        // verdict is one field away on the machine; carry it
        // (`security.md` § Post-auth surfacing — the typed verdict must survive
        // every seam between detection and surface).
        let snapshot = self.0.snapshot();
        if let fauna_launch_machine::LaunchPhase::IdentityChanged {
            pinned_hex,
            seen_hex,
        } = snapshot.phase
        {
            return Err(ApiError::NestIdentityChanged {
                host: self.0.identity_changed_authority().unwrap_or_default(),
                pinned_hex,
                seen_hex,
            });
        }
        // The same carry for the previously-signed-in refusal: the refresh (a
        // post-4401 `notify_401`, or the one above) met `not_registered` — the
        // user was suspended or removed mid-session. The machine reports it on
        // its `sign_in_refused` side channel; the phase alone reads as a
        // generic terminal offline.
        if snapshot.sign_in_refused {
            return Err(ApiError::SignInRefused);
        }
        // And for the succession refusal: the refresh met
        // `fauna.auth.superseded`, which parks the machine in its superseded
        // state with the claimed successor on a side channel — the phase alone
        // reads as a generic terminal offline. Without this carry linux's
        // supervisor stopped as "couldn't obtain a token" and the app sat on a
        // dead session instead of routing to the identity import.
        if let Some(new_actor_id_hex) = snapshot.superseded_successor {
            return Err(ApiError::Superseded { new_actor_id_hex });
        }
        Err(ApiError::Transport(
            "token refresh failed; LaunchMachine not Online".into(),
        ))
    }

    async fn notify_401(&self) {
        self.0.notify_401().await;
    }

    /// The machine mints from the secret it holds, so *it* names the bearer's
    /// actor — the one fact the assembling caller cannot derive from the
    /// keypair it separately hands to `AuthClient`.
    fn bearer_actor_id(&self) -> Option<[u8; 32]> {
        self.0.bearer_actor_id()
    }

    /// The machine is the holder here — it mints on both the refresh and the
    /// silent-challenge arm — so its set is this source's set.
    async fn own_token_ids(&self) -> Vec<String> {
        self.0.own_token_ids()
    }

    async fn current_token_id(&self) -> Option<String> {
        self.0.current_token_id()
    }
}

// ---------------------------------------------------------------------------
// LaunchMachineBearer recovery tests — gated on the `launch-machine` feature so
// they compile only when the impl is built. They drive a `LaunchMachine` via a
// scripted `MockAuthConnector` (no nest, no network).
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "launch-machine"))]
mod launch_machine_bearer_tests {
    use super::*;
    use fauna_launch_machine::{
        InMemoryPersistence, LaunchMachine, LaunchPhase, MockAuthConnector, NullObserver,
        SilentChallengeOutcome,
    };
    use fauna_protocol::auth::VerifyReply;
    use std::sync::Arc;

    /// Far enough out that the recovered bearer is a usable, non-expired token.
    const FAR_FUTURE_SECS: u64 = 4_000_000_000; // ~year 2096

    fn verify_reply(token: &str) -> VerifyReply {
        VerifyReply {
            token: token.into(),
            token_id: "0".repeat(16),
            handle: "alice".into(),
            domain: "nest.example".into(),
            tier: "free".into(),
            expires_at: FAR_FUTURE_SECS,
            // Anchored on this clock at receipt: a full hour, the nest's TTL.
            expires_in: 3600,
            ..Default::default()
        }
    }

    fn persistence() -> Arc<InMemoryPersistence> {
        Arc::new(
            InMemoryPersistence::new()
                .with_identity([0x42u8; 32].to_vec())
                .with_nest_url("https://nest.example"),
        )
    }

    /// The wiring half of the two-source identity cross-check
    /// (`fauna_client::BearerIdentityMismatch`): this bearer must name the actor
    /// the machine actually mints for, and must say **nothing** before the
    /// machine holds a secret.
    ///
    /// Both halves matter. A wrong answer would make the check accuse a healthy
    /// client; a `None` that should have been `Some` would make the check dead
    /// code that always passes — which is the failure mode a defence-in-depth
    /// assertion is most likely to rot into.
    #[tokio::test]
    async fn bearer_actor_id_names_the_machines_own_actor() {
        let connector = MockAuthConnector::new()
            .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply("a.bearer")));
        let machine = LaunchMachine::new_with_connector(
            Arc::new(NullObserver),
            persistence(),
            Arc::new(connector),
        );
        let bearer = LaunchMachineBearer(Arc::clone(&machine));

        // Before `start()` the machine is in `Boot` and holds no secret. That is
        // "cannot say", never "no actor" — the caller must not read it as a
        // mismatch.
        assert_eq!(bearer.bearer_actor_id(), None);

        machine.start().await;
        assert_eq!(machine.snapshot().phase, LaunchPhase::Online);

        // `persistence()` seeds the identity secret `[0x42; 32]`. The launch
        // machine derives the actor id locally from Ed25519 (it deliberately
        // carries no `fauna-core` dependency), so comparing against
        // `fauna-core`'s canonical derivation also pins that the two agree — if
        // they ever diverged, every client assembled this way would report a
        // false mismatch.
        let expected = fauna_core::identity::ActorKeypair::from_secret([0x42u8; 32])
            .actor_id()
            .0;
        assert_eq!(bearer.bearer_actor_id(), Some(expected));
    }

    /// A `LaunchMachine` parked in `Offline { transient: true }` — the common
    /// state right after a factory-reset reclaim, when the initial silent
    /// challenge raced the nest's restart — must still yield a bearer when one
    /// is demanded: `bearer()` re-runs the silent challenge to bootstrap back
    /// to `Online`. Before the fix, `bearer()` only called `refresh_token()`,
    /// which no-ops from any non-`Online` state, so it returned "LaunchMachine
    /// not Online" forever and the client silently degraded (the live 3-client
    /// CalDAV TRACK-A blocker: linux-created events never reached nest).
    #[tokio::test]
    async fn bearer_recovers_a_transiently_offline_machine() {
        // Initial silent challenge (during start) fails transient; the recovery
        // attempt that bearer() drives succeeds.
        let connector = MockAuthConnector::new()
            .push_silent_challenge(SilentChallengeOutcome::Transient {
                error: "nest mid-restart".into(),
            })
            .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply(
                "recovered.bearer",
            )));
        let machine = LaunchMachine::new_with_connector(
            Arc::new(NullObserver),
            persistence(),
            Arc::new(connector),
        );
        machine.start().await;
        // Sanity: start() parked it Offline-transient with no bearer.
        assert_eq!(
            machine.snapshot().phase,
            LaunchPhase::Offline { transient: true }
        );
        assert_eq!(machine.current_bearer(), None);

        let bearer = LaunchMachineBearer(Arc::clone(&machine));
        assert_eq!(bearer.bearer().await.unwrap(), "recovered.bearer");
        assert_eq!(machine.snapshot().phase, LaunchPhase::Online);
    }

    /// A *terminal* Offline (e.g. the secret is invalid) must NOT be retried by
    /// `bearer()` — `retry_silent_challenge` self-guards to the transient case,
    /// so the bearer source surfaces the failure instead of hammering the nest
    /// with a known-bad secret.
    #[tokio::test]
    async fn bearer_does_not_retry_a_terminally_offline_machine() {
        let connector =
            MockAuthConnector::new().push_silent_challenge(SilentChallengeOutcome::SecretInvalid {
                error: "expected 32-byte secret".into(),
            });
        let machine = LaunchMachine::new_with_connector(
            Arc::new(NullObserver),
            persistence(),
            Arc::new(connector),
        );
        machine.start().await;
        assert_eq!(
            machine.snapshot().phase,
            LaunchPhase::Offline { transient: false }
        );

        let bearer = LaunchMachineBearer(Arc::clone(&machine));
        assert!(bearer.bearer().await.is_err());
        // Still terminal — no spurious transition triggered by the bearer path.
        assert_eq!(
            machine.snapshot().phase,
            LaunchPhase::Offline { transient: false }
        );
    }

    /// **The seam that kept linux and tui blind to a changed nest identity.**
    ///
    /// These two apps hand this bearer to `AuthClient::with_bearer_source`, so
    /// the `SupersededLatch`-shaped side channel on `WsChallengeBearer` cannot
    /// observe their mint at all (`fauna_client::AuthClient`'s `superseded`
    /// field records exactly this hole). The machine *already knew* — the
    /// refresh parked it in `IdentityChanged` — and this bearer threw the
    /// verdict away, reporting "token refresh failed; LaunchMachine not Online",
    /// which every consumer reads as an ordinary transport fault and retries.
    ///
    /// Note the verdict is produced by the machine's real classification of a
    /// scripted `IdentityChanged` challenge outcome, not by poking a phase in:
    /// what is under test is that the bearer *reads* what the machine already
    /// concluded.
    #[tokio::test]
    async fn a_machine_that_met_a_changed_identity_says_so_instead_of_a_transport_fault() {
        let connector = MockAuthConnector::new().push_silent_challenge(
            SilentChallengeOutcome::IdentityChanged {
                host: "nest.example".into(),
                pinned_hex: "aa".repeat(32),
                seen_hex: Some("bb".repeat(32)),
                fork: false,
            },
        );
        let machine = LaunchMachine::new_with_connector(
            Arc::new(NullObserver),
            persistence(),
            Arc::new(connector),
        );
        machine.start().await;
        assert!(
            matches!(
                machine.snapshot().phase,
                LaunchPhase::IdentityChanged { .. }
            ),
            "precondition: the machine itself must have reached the verdict"
        );

        let bearer = LaunchMachineBearer(Arc::clone(&machine));
        let err = bearer.bearer().await.expect_err("no bearer is available");
        let ApiError::NestIdentityChanged {
            host,
            pinned_hex,
            seen_hex,
        } = err
        else {
            panic!("the verdict must not be flattened to a transport fault; got {err:?}");
        };
        assert_eq!(host, "nest.example");
        assert_eq!(pinned_hex, "aa".repeat(32));
        assert_eq!(seen_hex.as_deref(), Some("bb".repeat(32).as_str()));
    }

    /// The same seam for the previously-signed-in refusal: a session whose
    /// user an admin suspended mid-session (`onboarding.md` § App-launch
    /// routing, the previously-signed-in row — "the same verdict mid-session
    /// lands the same surface"). The post-`4401` re-mint reaches the machine
    /// through `notify_401`, its refresh meets `not_registered` and parks it in
    /// the refused state; the bearer must say so as a typed verdict, never the
    /// transport fault a reconnect supervisor would treat as retryable.
    ///
    /// Scripted as the real chain: signed in, then every later challenge
    /// refused (the bearer's own `retry_silent_challenge` re-asks once — the
    /// refused state is the one terminal offline a retry may clear).
    #[tokio::test]
    async fn a_machine_whose_sign_in_was_refused_mid_session_says_so_instead_of_a_transport_fault()
    {
        let connector = MockAuthConnector::new()
            .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply("a.bearer")))
            .push_silent_challenge(SilentChallengeOutcome::NotRegistered)
            .push_silent_challenge(SilentChallengeOutcome::NotRegistered);
        let machine = LaunchMachine::new_with_connector(
            Arc::new(NullObserver),
            persistence(),
            Arc::new(connector),
        );
        machine.start().await;
        assert_eq!(machine.snapshot().phase, LaunchPhase::Online);

        let bearer = LaunchMachineBearer(Arc::clone(&machine));
        // The server-revoked bearer (the 4401): the reactive refresh.
        bearer.notify_401().await;
        assert!(
            machine.snapshot().sign_in_refused,
            "precondition: the machine itself must have reached the verdict"
        );

        let err = bearer.bearer().await.expect_err("no bearer is available");
        assert_eq!(
            err,
            ApiError::SignInRefused,
            "the verdict must not be flattened to a transport fault"
        );
    }
    /// The same seam for the succession refusal — the one a device's OWN
    /// stolen-identity ceremony causes the instant the nest commits: the
    /// post-`4401` refresh meets `fauna.auth.superseded`, the machine parks in
    /// its superseded state, and the bearer must name it (linux read it as
    /// "LaunchMachine not Online" and never left the dead session).
    #[tokio::test]
    async fn a_machine_whose_identity_was_succeeded_mid_session_says_so_instead_of_a_transport_fault()
     {
        let successor = "ab".repeat(32);
        let superseded = || SilentChallengeOutcome::Superseded {
            new_actor_id_hex: "ab".repeat(32),
        };
        let connector = MockAuthConnector::new()
            .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply("a.bearer")))
            .push_silent_challenge(superseded())
            .push_silent_challenge(superseded());
        let machine = LaunchMachine::new_with_connector(
            Arc::new(NullObserver),
            persistence(),
            Arc::new(connector),
        );
        machine.start().await;
        assert_eq!(machine.snapshot().phase, LaunchPhase::Online);

        let bearer = LaunchMachineBearer(Arc::clone(&machine));
        bearer.notify_401().await;
        assert_eq!(
            machine.snapshot().superseded_successor.as_deref(),
            Some(successor.as_str()),
            "precondition: the machine itself must have reached the verdict"
        );

        let err = bearer.bearer().await.expect_err("no bearer is available");
        assert_eq!(
            err,
            ApiError::Superseded {
                new_actor_id_hex: successor
            },
            "the verdict must not be flattened to a transport fault"
        );
        assert!(!err.is_transient());
    }
}

// ---------------------------------------------------------------------------
// Base BearerSource tests — the Arc<T> forwarding impl + StaticBearer's
// default no-op notify_401. Ungated (no `launch-machine` feature needed).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Tracks whether `notify_401()` was actually invoked, so the `Arc<T>`
    /// forwarding impl (this module's `impl<T: BearerSource + ?Sized>
    /// BearerSource for Arc<T>`) is proven to reach the inner impl rather than
    /// silently relying on some other default.
    struct RecordingBearer {
        token: String,
        notified: AtomicBool,
        own_ids: Vec<String>,
    }

    #[async_trait]
    impl BearerSource for RecordingBearer {
        async fn bearer(&self) -> Result<String, ApiError> {
            Ok(self.token.clone())
        }
        async fn notify_401(&self) {
            self.notified.store(true, Ordering::SeqCst);
        }
        async fn own_token_ids(&self) -> Vec<String> {
            self.own_ids.clone()
        }
        async fn current_token_id(&self) -> Option<String> {
            self.own_ids.last().cloned()
        }
    }

    /// The module docs promise a second content API built off an
    /// already-shared `Arc<dyn BearerSource>` shares the *one* bearer cache —
    /// that promise depends entirely on `Arc<T>`'s `bearer`/`notify_401` both
    /// forwarding through to the inner impl rather than the trait's own
    /// default `notify_401` (a silent no-op) shadowing it.
    #[tokio::test]
    async fn arc_bearer_source_forwards_bearer_and_notify_401() {
        let inner = Arc::new(RecordingBearer {
            token: "arc-forwarded".into(),
            notified: AtomicBool::new(false),
            own_ids: Vec::new(),
        });
        let via_arc = Arc::clone(&inner);
        assert_eq!(via_arc.bearer().await.unwrap(), "arc-forwarded");
        assert!(!inner.notified.load(Ordering::SeqCst));
        via_arc.notify_401().await;
        assert!(
            inner.notified.load(Ordering::SeqCst),
            "Arc<T>::notify_401 must forward to the inner impl, not no-op"
        );
    }

    /// The same trap one method further on, and the one that would be silent:
    /// `own_token_ids`/`current_token_id` are **defaulted**, so an `Arc<T>`
    /// that failed to forward them would not fail to compile — it would serve
    /// the empty default to every `Arc<dyn BearerSource>` holder, and a
    /// sessions surface would show the app's own rows as strangers with no
    /// error anywhere (`devices.md` § The client's own session).
    #[tokio::test]
    async fn arc_bearer_source_forwards_own_token_ids_and_current_token_id() {
        let inner = Arc::new(RecordingBearer {
            token: "arc-forwarded".into(),
            notified: AtomicBool::new(false),
            own_ids: vec!["aaaaaaaaaaaaaaaa".into(), "bbbbbbbbbbbbbbbb".into()],
        });
        let via_arc = Arc::clone(&inner);
        assert_eq!(
            via_arc.own_token_ids().await,
            vec![
                "aaaaaaaaaaaaaaaa".to_string(),
                "bbbbbbbbbbbbbbbb".to_string()
            ],
            "Arc<T>::own_token_ids must forward, not serve the empty default"
        );
        assert_eq!(
            via_arc.current_token_id().await.as_deref(),
            Some("bbbbbbbbbbbbbbbb"),
            "Arc<T>::current_token_id must forward, not serve the None default"
        );
    }

    /// The default really is "cannot say" — pinned for `StaticBearer` the way
    /// its `notify_401` no-op is, so the opt-out stays deliberate.
    #[tokio::test]
    async fn static_bearer_names_no_own_sessions() {
        let bearer = StaticBearer("t0".into());
        assert!(bearer.own_token_ids().await.is_empty());
        assert_eq!(bearer.current_token_id().await, None);
    }

    /// `StaticBearer` relies entirely on the trait's default `notify_401`
    /// (never overridden) — pin that the default really is a no-op (never
    /// panics) so the documented "can't recover; the content layer surfaces
    /// the original 401" guarantee actually holds for it.
    #[tokio::test]
    async fn static_bearer_notify_401_is_a_no_op() {
        let bearer = StaticBearer("t0".into());
        bearer.notify_401().await; // must not panic
        assert_eq!(bearer.bearer().await.unwrap(), "t0");
    }
}
