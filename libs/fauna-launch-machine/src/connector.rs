//! [`AuthConnector`] — the launch machine's transport seam for the
//! pre-identity auth ceremonies.
//!
//! `LaunchMachine` holds an `Arc<dyn AuthConnector>` rather than calling the
//! WS-RPC connectors directly, for one reason: the machine is a leaf crate that
//! **cannot spin a real nest** (no `fauna-nest` dep — and adding one would form
//! a Cargo cycle). The seam is *dyn-safe* — it operates at the **outcome**
//! level ([`SilentChallengeOutcome`]), not the
//! generic [`fauna_protocol::RpcRequester`] level — so the production impl
//! ([`WsAuthConnector`]) and a scripted test impl ([`MockAuthConnector`]) are
//! interchangeable. State-machine transition tests inject the mock; the real WS
//! round-trip is proven by the tier_3
//! `bins/fauna-nest/tests/launch_machine_auth_roundtrip.rs`.

use crate::auth::SilentChallengeOutcome;
use crate::probe::ClaimProbe;

/// The launch machine's auth transport. Outcome-level (dyn-safe) so it can be
/// stored behind `Arc<dyn AuthConnector>` and swapped for a mock in tests.
///
/// `MaybeSendSync` supertrait + the `async_trait`/`async_trait(?Send)` cfg pair
/// give one trait body across both targets: native callers get `Send` futures
/// (so `LaunchMachine::ttl_refresh_loop` stays `tokio::spawn`-able), while the
/// wasm `!Send` browser-WebSocket transport relaxes it (see
/// `fauna_protocol::requester` module docs).
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait AuthConnector: fauna_protocol::MaybeSendSync {
    /// `fauna.auth.challenge` then `fauna.auth.verify` over a fresh anonymous
    /// connection — the launch silent-challenge fast path **and** every
    /// refresh (TTL-scheduled and 401-reactive): one ceremony for every bearer
    /// the machine mints, none of them refusable by a wrong client clock
    /// (`login.md` § When to use which; the `fauna.auth.handshake` refresh
    /// seam this trait used to carry was removed 2026-09-21).
    ///
    /// `reach_ipv4` is the account's **reach hint** (`onboarding.md` § Reach
    /// hint): `None` dials `nest_url` as written, `Some(ip)` dials the same
    /// nest at that address while keeping `nest_url`'s host as the name — the
    /// TLS SNI and `Host` natively, the bridge-cert authority on wasm. Whether
    /// to pass it is the machine's decision and never this seam's: the policy
    /// (domain first, hint only on a reachability failure, delete on the first
    /// domain success) lives in [`crate::LaunchMachine`] so that all seven apps
    /// inherit one copy of it.
    async fn silent_challenge(
        &self,
        nest_url: &str,
        reach_ipv4: Option<&str>,
        secret: &[u8],
    ) -> SilentChallengeOutcome;
    /// `fauna.setup.status` over a fresh anonymous connection. Two callers with
    /// opposite safe defaults for an unreachable box, so the outcome stays
    /// three-way and neither default is baked in here (see [`crate::ClaimProbe`]):
    /// the `NotRegistered` fallback routes between `invite_request` (claimed *or*
    /// unreachable) and `claim_code` (unclaimed); the pending-factory-reset boot
    /// reconcile clears its slot only on a definite `Claimed`.
    async fn probe_claim(&self, nest_url: &str) -> ClaimProbe;
    /// Forget the TOFU nest-identity pin for `nest_url` — the explicit,
    /// user-approved recovery behind [`crate::LaunchMachine::trust_nest_identity`]
    /// (the `ssh-keygen -R host` analogue). The next connect re-TOFUs. Never
    /// called automatically; only the machine's re-trust action reaches it
    /// (security.md § Transport trust).
    async fn forget_identity_pin(&self, nest_url: &str);
}

/// Production connector: each call opens one short-lived anonymous WS-RPC
/// connection (native `fauna-anon-client`, wasm `fauna-rpc-wasm`) and runs the
/// ceremony, mirroring the per-request shape of the HTTP twins it replaced.
///
/// **This is where — and the only place where — the dial seam applies inside
/// the machine.** Every method resolves its `nest_url` through
/// [`crate::dial::resolved_dial_url`] before opening a socket, so the machine's
/// own state, its `save_authenticated` write and the scripted
/// [`MockAuthConnector`] all keep the literal stored URL. Resolving up in
/// `LaunchMachine` instead would put a harness URL into `State::Online` and from
/// there into the long-term store — the at-rest corruption `dial.rs` exists to
/// avoid.
pub struct WsAuthConnector;

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl AuthConnector for WsAuthConnector {
    async fn silent_challenge(
        &self,
        nest_url: &str,
        reach_ipv4: Option<&str>,
        secret: &[u8],
    ) -> SilentChallengeOutcome {
        crate::auth::connect_silent_challenge(
            &crate::dial::resolved_dial_url(nest_url),
            reach_ipv4,
            secret,
        )
        .await
    }
    async fn probe_claim(&self, nest_url: &str) -> ClaimProbe {
        crate::probe::probe_setup_status(&crate::dial::resolved_dial_url(nest_url)).await
    }
    async fn forget_identity_pin(&self, nest_url: &str) {
        // Native pins key on the URL's authority (`host[:port]`, the same key
        // the graduation used); web pins key on the nest URL string itself
        // (the SPA's `silentSignIn` convention). Each side forgets through
        // the same store its checker consulted, so forget and check can never
        // disagree about the key.
        //
        // Resolved for the same reason the ceremonies above are: the pin was
        // TOFU'd against whatever authority the socket actually reached, so
        // forgetting the literal would leave the real pin in place.
        let nest_url = &crate::dial::resolved_dial_url(nest_url);
        #[cfg(not(target_arch = "wasm32"))]
        {
            let host = fauna_anon_client::authority_of(nest_url);
            fauna_anon_client::forget_identity_pin(&host);
        }
        #[cfg(target_arch = "wasm32")]
        {
            use fauna_client_core::nest_trust::NestIdentityPinStore;
            fauna_client_core::nest_trust::LocalStoragePinStore.remove(nest_url);
        }
    }
}

// ---------------------------------------------------------------------------
// Test helper: a scripted connector for state-machine transition tests.
// ---------------------------------------------------------------------------

/// Scripted [`AuthConnector`] for state-machine tests — returns canned outcomes
/// without any network. Each queue pops one outcome per call; when only one
/// remains it sticks (so a TTL loop that refreshes repeatedly keeps seeing the
/// last scripted outcome). An empty queue yields a `Transient` fault.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub struct MockAuthConnector {
    /// One queue for launch and refresh alike — the machine runs the same
    /// ceremony for both, so a test scripts the launch reply first and each
    /// refresh's reply after it.
    silent_challenge: std::sync::Mutex<std::collections::VecDeque<SilentChallengeOutcome>>,
    claim: std::sync::Mutex<ClaimProbe>,
    claim_probe_calls: std::sync::Mutex<usize>,
    forgotten_pins: std::sync::Mutex<Vec<String>>,
    /// The reach hint each `silent_challenge` call carried, in order. Recording
    /// the argument rather than a bare call count is what lets a test assert the
    /// *order* — that the domain is dialled first and the hint only after — which
    /// is the whole safety property of a possibly-stale hint.
    silent_challenge_hints: std::sync::Mutex<Vec<Option<String>>>,
}

// `new()` owns the defaults (notably the `Claimed` probe fallback), so `Default`
// delegates rather than deriving — a derive would demand a `Default` for
// `ClaimProbe`, and a probe *outcome* has no meaningful default.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
impl Default for MockAuthConnector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
impl MockAuthConnector {
    pub fn new() -> Self {
        Self {
            silent_challenge: std::sync::Mutex::new(std::collections::VecDeque::new()),
            // Default matches the `NotRegistered` fallback's "assume claimed".
            claim: std::sync::Mutex::new(ClaimProbe::Claimed),
            claim_probe_calls: std::sync::Mutex::new(0),
            forgotten_pins: std::sync::Mutex::new(Vec::new()),
            silent_challenge_hints: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The reach hint each `silent_challenge` call carried, in order — `None`
    /// for a plain domain dial. Both the count and the sequence matter: the
    /// hint's stale-address safety rests on the domain always being first.
    pub fn silent_challenge_hints(&self) -> Vec<Option<String>> {
        self.silent_challenge_hints.lock().unwrap().clone()
    }

    /// How many times `probe_claim` was called — lets a test assert the
    /// pending-factory-reset reconcile costs the ordinary launch path no round
    /// trip (the probe is for the slot row only).
    pub fn claim_probe_calls(&self) -> usize {
        *self.claim_probe_calls.lock().unwrap()
    }

    /// The nest URLs `forget_identity_pin` was called with, in order — lets a
    /// transition test assert the re-trust action actually forgot the pin.
    pub fn forgotten_pins(&self) -> Vec<String> {
        self.forgotten_pins.lock().unwrap().clone()
    }

    /// Queue one `silent_challenge` outcome (FIFO; last one sticks). The
    /// first queued outcome answers the launch; the ones after it answer the
    /// refreshes, in order.
    pub fn push_silent_challenge(self, o: SilentChallengeOutcome) -> Self {
        self.silent_challenge.lock().unwrap().push_back(o);
        self
    }

    /// What `probe_claim` returns (default [`ClaimProbe::Claimed`]).
    pub fn with_claim_probe(self, claim: ClaimProbe) -> Self {
        *self.claim.lock().unwrap() = claim;
        self
    }
}

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
fn pop_or_clone<T: Clone>(q: &std::sync::Mutex<std::collections::VecDeque<T>>) -> Option<T> {
    let mut q = q.lock().unwrap();
    if q.len() > 1 {
        q.pop_front()
    } else {
        q.front().cloned()
    }
}

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl AuthConnector for MockAuthConnector {
    async fn silent_challenge(
        &self,
        _nest_url: &str,
        reach_ipv4: Option<&str>,
        _secret: &[u8],
    ) -> SilentChallengeOutcome {
        self.silent_challenge_hints
            .lock()
            .unwrap()
            .push(reach_ipv4.map(str::to_string));
        pop_or_clone(&self.silent_challenge).unwrap_or(SilentChallengeOutcome::Transient {
            error: "mock: no scripted silent_challenge outcome".into(),
        })
    }
    async fn probe_claim(&self, _nest_url: &str) -> ClaimProbe {
        *self.claim_probe_calls.lock().unwrap() += 1;
        *self.claim.lock().unwrap()
    }
    async fn forget_identity_pin(&self, nest_url: &str) {
        self.forgotten_pins
            .lock()
            .unwrap()
            .push(nest_url.to_string());
    }
}
