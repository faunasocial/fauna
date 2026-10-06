//! WASM bindings for Fauna's LaunchMachine.
//!
//! Mirrors fauna-wasm-onboarding's pattern for the JS-side observer, and a
//! `LaunchMachine` wasm wrapper exposing snapshot, current_bearer, start,
//! refresh_token, notify_401.
//!
//! **Persistence is NOT a JS seam (CR-3).** The machine routes on the shared
//! `RegistryLaunchPersistence` over `LocalStorageSecretStore` — the same
//! per-actor registry store the switcher's `WasmAccountRegistry` (fauna-wasm
//! chunk) drives — so web cannot hand the machine a bespoke single-slot store:
//! the four-in-language-impls divergence is unrepresentable, not just fixed.
//! Every slot is the per-actor row and nothing else — there is no global
//! single-slot key beside it (`docs/goal/architecture/long-term-store.md`) —
//! and the `registry*` slot accessors below are what the SPA's slot stores
//! wrap.
//!
//! Loaded by the web app at app-launch time (see
//! `apps/fauna-web/src/routes/+layout.{ts,svelte}` after Phase 2 adoption).

use std::sync::Arc;

// The registry store is wasm32-only (`web_store.rs` is gated on the browser's
// localStorage), so everything that touches it below carries the same gate —
// the native `cargo check` compiles this crate too, just without the
// browser-backed half (the `just wasm` build is the gate that compiles it all;
// memory/build-system: no native gate reaches a wasm32-only block).
#[cfg(target_arch = "wasm32")]
use fauna_client_accounts::{
    AccountRegistry, LocalStorageSecretStore, RegistryLaunchPersistence, SecretStore,
};
use fauna_launch_machine::LaunchMachine as InnerMachine;
#[cfg(target_arch = "wasm32")]
use fauna_launch_machine::LaunchObserver as InnerObserver;
#[cfg(target_arch = "wasm32")]
use fauna_launch_machine::{
    AwaitingDnsRecord, LaunchPersistence as InnerPersistence, PendingInviteRecord,
};
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
fn store() -> Arc<dyn SecretStore> {
    Arc::new(LocalStorageSecretStore)
}

/// A fresh adapter view over the one localStorage-backed registry store.
/// Stateless and cheap — every read/write goes straight to localStorage.
#[cfg(target_arch = "wasm32")]
fn persistence() -> RegistryLaunchPersistence {
    registry().launch_persistence()
}

#[cfg(target_arch = "wasm32")]
fn registry() -> AccountRegistry {
    AccountRegistry::new(store())
}

/// Run one synchronous registry mutation inside the cross-tab mutation lock
/// and hand JS a Promise of its result — every slot writer below is a
/// registry mutator, and tabs share one `localStorage`
/// (`fauna_client_accounts::with_web_mutation_lock`; the same shape
/// `fauna-wasm`'s `WasmAccountRegistry` mutators take). The machine's own
/// `save_authenticated` write is guarded by the shared adapter itself.
#[cfg(target_arch = "wasm32")]
fn mutate(mutate: impl FnOnce() -> JsValue + 'static) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move {
        Ok(fauna_client_accounts::with_web_mutation_lock(mutate).await)
    })
}

// ── Observer ─────────────────────────────────────────────────────────────

#[wasm_bindgen]
extern "C" {
    pub type JsLaunchObserver;
    #[wasm_bindgen(method, js_name = onChanged)]
    fn on_changed(this: &JsLaunchObserver);
}

#[cfg(target_arch = "wasm32")]
struct ObserverShim(JsLaunchObserver);
#[cfg(target_arch = "wasm32")]
// SAFETY: wasm32 is single-threaded; the JS object never crosses a thread.
unsafe impl Send for ObserverShim {}
#[cfg(target_arch = "wasm32")]
unsafe impl Sync for ObserverShim {}
#[cfg(target_arch = "wasm32")]
impl InnerObserver for ObserverShim {
    fn on_changed(&self) {
        self.0.on_changed()
    }
}

// ── Pending factory reset (gap CR-1) ─────────────────────────────────────

/// Mint the post-reset claim code and durably persist it **before** the caller
/// dispatches `fauna.admin.factory_reset` — see
/// `fauna_launch_machine::mint_and_persist_pending_factory_reset`.
///
/// Writes the ACTIVE account's per-actor slot through the same registry store
/// the machine routes on, then pin
/// the returned code onto the reset request (`factoryReset(secretHex, code)`).
/// The code is only returned once the row is in `localStorage`, so a crash the
/// instant this returns still leaves a resumable slot — which is the whole point
/// (`docs/goal/architecture/nest/common.md` § Client-state recoverability).
/// Returns `undefined` when the row could NOT be persisted (a `localStorage`
/// quota error, private-browsing) — the shared rail reads the row back and
/// refuses to hand out a code it could not save. The caller MUST then abort
/// the reset rather than dispatch one whose code it failed to save.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(
    js_name = mintAndPersistPendingFactoryReset,
    unchecked_return_type = "Promise<string | undefined>"
)]
pub fn mint_and_persist_pending_factory_reset(nest_url: String, handle: String) -> js_sys::Promise {
    mutate(move || {
        fauna_launch_machine::mint_and_persist_pending_factory_reset(
            &persistence(),
            nest_url,
            handle,
        )
        .map(JsValue::from)
        .unwrap_or(JsValue::UNDEFINED)
    })
}

// ── Registry-backed slot accessors ───────────────────────────────────────
//
// The SPA's slot stores (`pending-invite-store.ts` / `awaiting-dns-store.ts` /
// `pending-factory-reset-store.ts`) wrap these instead of touching
// localStorage directly, so every read/write goes through the SAME per-actor
// registry seam the machine routes on — the gate and the machine can never
// disagree, and a TS-side delete can never leave the per-actor row behind.
// Records cross the boundary as serde JSON strings (snake_case keys).

/// Whether the store holds an identity for the active account — the TS gates'
/// (`hasPendingFactoryResetSlot` / `hasAwaitingDnsSlot`) identity half.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = registryHasIdentity)]
pub fn registry_has_identity() -> bool {
    persistence().load_identity().is_some()
}

/// The active account's pending-invite record as serde JSON, or `undefined`.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = registryLoadPendingInvite)]
pub fn registry_load_pending_invite() -> Option<String> {
    let rec = persistence().load_pending_invite()?;
    serde_json::to_string(&rec).ok()
}

/// Write the active account's pending-invite slot from serde JSON. A payload
/// that does not parse as `PendingInviteRecord` is refused (returns `false`)
/// rather than stored — a corrupt slot would read back as absent anyway.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = registrySavePendingInvite, unchecked_return_type = "Promise<boolean>")]
pub fn registry_save_pending_invite(json: String) -> js_sys::Promise {
    if serde_json::from_str::<PendingInviteRecord>(&json).is_err() {
        return js_sys::Promise::resolve(&JsValue::FALSE);
    }
    mutate(move || {
        let reg = registry();
        let Some(active) = reg.active() else {
            return JsValue::FALSE;
        };
        reg.set_pending_invite_json(&active, &json);
        JsValue::TRUE
    })
}

/// Clear the active account's pending-invite slot.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = registryDeletePendingInvite, unchecked_return_type = "Promise<void>")]
pub fn registry_delete_pending_invite() -> js_sys::Promise {
    mutate(|| {
        persistence().delete_pending_invite();
        JsValue::UNDEFINED
    })
}

/// The active account's awaiting-manual-dns record as serde JSON, or `undefined`.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = registryLoadAwaitingDns)]
pub fn registry_load_awaiting_dns() -> Option<String> {
    let rec = persistence().load_awaiting_dns()?;
    serde_json::to_string(&rec).ok()
}

/// Write the active account's awaiting-manual-dns slot from serde JSON. Same
/// refuse-unparseable contract as [`registry_save_pending_invite`].
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = registrySaveAwaitingDns, unchecked_return_type = "Promise<boolean>")]
pub fn registry_save_awaiting_dns(json: String) -> js_sys::Promise {
    if serde_json::from_str::<AwaitingDnsRecord>(&json).is_err() {
        return js_sys::Promise::resolve(&JsValue::FALSE);
    }
    mutate(move || {
        let reg = registry();
        let Some(active) = reg.active() else {
            return JsValue::FALSE;
        };
        reg.set_awaiting_dns_json(&active, &json);
        JsValue::TRUE
    })
}

/// Clear the active account's awaiting-manual-dns slot (claim completed).
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = registryClearAwaitingDns, unchecked_return_type = "Promise<void>")]
pub fn registry_clear_awaiting_dns() -> js_sys::Promise {
    mutate(|| {
        let reg = registry();
        if let Some(active) = reg.active() {
            reg.clear_awaiting_dns(&active);
        }
        JsValue::UNDEFINED
    })
}

/// The active account's pending-factory-reset record as serde JSON, or
/// `undefined`. Written only through [`mint_and_persist_pending_factory_reset`].
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = registryLoadPendingFactoryReset)]
pub fn registry_load_pending_factory_reset() -> Option<String> {
    let rec = persistence().load_pending_factory_reset()?;
    serde_json::to_string(&rec).ok()
}

/// Clear the active account's pending-factory-reset slot (re-claim landed).
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = registryDeletePendingFactoryReset, unchecked_return_type = "Promise<void>")]
pub fn registry_delete_pending_factory_reset() -> js_sys::Promise {
    mutate(|| {
        persistence().delete_pending_factory_reset();
        JsValue::UNDEFINED
    })
}

// ── LaunchMachine ────────────────────────────────────────────────────────

#[wasm_bindgen]
pub struct LaunchMachine(Arc<InnerMachine>);

#[wasm_bindgen]
impl LaunchMachine {
    /// Build over the registry-backed persistence (the ONLY persistence web
    /// can hand the machine — see the module header). The observer stays a JS
    /// seam: rendering is the SPA's job; storage is not.
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen(constructor)]
    pub fn new(observer: JsLaunchObserver) -> Self {
        #[cfg(feature = "test-helpers")]
        {
            seed_launch_clock_from_storage();
            seed_nest_dial_override_from_storage();
        }
        let observer: Arc<dyn InnerObserver> = Arc::new(ObserverShim(observer));
        let persistence: Arc<dyn InnerPersistence> = Arc::new(persistence());
        Self(InnerMachine::new(observer, persistence))
    }

    /// Drive the launch flow. Async — JS gets a Promise.
    #[wasm_bindgen]
    pub fn start(&self) -> js_sys::Promise {
        let m = self.0.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            m.start().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Returns the current LaunchSnapshot serialized to JSON. JS callers
    /// `JSON.parse` it.
    ///
    /// The **whole** snapshot, field for field per `snapshots.rs` — not a
    /// hand-picked subset. So an additive side channel there (today
    /// `superseded_successor` and `identity_fork`) reaches JS the moment it is
    /// added, and the only thing a client owes is declaring and reading it
    /// (`apps/fauna-web/src/lib/launch-snapshot.ts`).
    #[wasm_bindgen(js_name = snapshotJson)]
    pub fn snapshot_json(&self) -> String {
        serde_json::to_string(&self.0.snapshot()).unwrap_or_default()
    }

    /// Bearer token if the machine is `Online`, else `null`.
    #[wasm_bindgen(js_name = currentBearer)]
    pub fn current_bearer(&self) -> Option<String> {
        self.0.current_bearer()
    }

    /// The id of the session the current bearer names, else `null`
    /// (`docs/goal/behavior/devices.md` § The client's own session). The web
    /// launch path primes the SPA's bearer cache straight off this machine
    /// (`apps/fauna-web/src/routes/onboarding/+page.svelte`), so without this
    /// the one token web never minted through `getAuthToken` — the launch
    /// token — would be the one token it could not name.
    #[wasm_bindgen(js_name = currentTokenId)]
    pub fn current_token_id(&self) -> Option<String> {
        self.0.current_token_id()
    }

    /// Manual token refresh.
    #[wasm_bindgen(js_name = refreshToken)]
    pub fn refresh_token(&self) -> js_sys::Promise {
        let m = self.0.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            m.refresh_token().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Tell the machine an HTTP layer received a 401 — triggers refresh.
    #[wasm_bindgen(js_name = notify401)]
    pub fn notify_401(&self) -> js_sys::Promise {
        let m = self.0.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            m.notify_401().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Re-run the silent challenge — the `launch-retry-button` CTA on the
    /// transient-error launch surface. Meaningful only from
    /// `Offline { transient: true }`; every other phase is a no-op, so the
    /// terminal `Offline { transient: false }` (an outdated nest) and
    /// `IdentityChanged` can never be retried into.
    #[wasm_bindgen(js_name = retrySilentChallenge)]
    pub fn retry_silent_challenge(&self) -> js_sys::Promise {
        let m = self.0.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            m.retry_silent_challenge().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// The explicit "trust this nest" recovery from `LaunchPhase::IdentityChanged`
    /// (`nest-identity-changed-trust-button`): forget the TOFU pin through the
    /// connector's trust seam — on wasm, `LocalStoragePinStore::remove`, the same
    /// store the check consulted — then re-run the silent challenge, which
    /// re-TOFUs against whatever identity the nest now proves.
    ///
    /// A no-op in every other phase: the pin is never forgotten outside this
    /// user-approved action (security.md § Transport trust — no
    /// silent re-pin, ever).
    #[wasm_bindgen(js_name = trustNestIdentity)]
    pub fn trust_nest_identity(&self) -> js_sys::Promise {
        let m = self.0.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            m.trust_nest_identity().await;
            Ok(JsValue::UNDEFINED)
        })
    }
}

// ── Panic hook ───────────────────────────────────────────────────────────
//
// Each wasm chunk is its own module with its own Rust runtime, so a hook
// installed in one chunk covers none of the others (see the
// `fauna-wasm-panic-hook` crate doc comment). `#[wasm_bindgen(start)]` runs
// automatically the moment this chunk's module is instantiated — no SPA-side
// call site to add or remember, unlike `fauna-wasm`'s explicit `installLogging`.
#[wasm_bindgen(start)]
fn panic_hook_start() {
    fauna_wasm_panic_hook::install("fauna-wasm-launch");
}

/// Test-only: deliberately panics, so an e2e can assert the hook above really
/// names this chunk in the browser console — a headless witness, not a
/// review-only claim. Compiled out of every non-`test-helpers` build.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = panicForTestOnly)]
pub fn panic_for_test_only() {
    panic!("deliberate test panic");
}

// ── Launch clock (e2e) ───────────────────────────────────────────────────
//
// Web's leg of the wrong-clock launch witness
// (`docs/features/connect-and-sign-in.md` outcome 9). Native apps seed
// `fauna_launch_machine::launch_clock`'s offset from the
// `FAUNA_E2E_CLOCK_OFFSET_SECS` environment variable on first use; a browser
// has no environment, so web's seed is a localStorage key of the SAME name,
// which the harness writes beside the identity. It is read synchronously in
// the `LaunchMachine` constructor — before `start()` runs the silent
// challenge — so it cannot race the SPA's asynchronously-installed automation
// surface, and it survives the `hard_reload()` that re-instantiates this
// module (and so zeroes the in-memory static). Compile-gated outer, stored key
// inner (convention 15): a production build has neither the read nor the
// exports below. `test-helpers` forwards `fauna-launch-machine/e2e-agent`,
// which the offset itself is gated on — `wasm-pack` builds release, so there
// is no `debug_assertions` to open it.

/// Seed the launch clock's offset from localStorage, if the harness set it.
/// Unset or unparseable leaves the real clock, as for the native env seed.
#[cfg(all(target_arch = "wasm32", feature = "test-helpers"))]
fn seed_launch_clock_from_storage() {
    use fauna_launch_machine::launch_clock;
    if let Some(offset) = store()
        .get(launch_clock::OFFSET_ENV)
        .and_then(|raw| raw.trim().parse::<i64>().ok())
    {
        launch_clock::set_clock_offset_secs(offset);
    }
}

// ── Nest dial override (e2e) ─────────────────────────────────────────────
//
// `fauna_launch_machine::dial`'s override is a `static`, and each wasm chunk is
// its own linear memory: the copy `OnboardingMachine::set_provider_base_urls`
// installs lives in the onboarding chunk, and this chunk's — the one the
// machine's `WsAuthConnector` resolves through — stayed empty. So a tab whose
// stored `nest_url` is not itself dialable (a typed loopback handle resolves to
// `https://`, a harness nest serves plain HTTP) reached `LoggedIn` through the
// wizard and then failed every silent challenge the launch route ran. The SPA
// keeps the override in `sessionStorage` (`$lib/api`'s
// `NEST_DIAL_OVERRIDE_KEY`, written at module load from the harness's query
// param, before this constructor can run); it is read here for the same
// reasons the clock seed is — synchronously, ahead of `start()`, and again
// after any reload that re-instantiates this module. Compile-gated like the
// clock (convention 15).

/// The `sessionStorage` key `$lib/api` keeps the dial override under.
#[cfg(all(target_arch = "wasm32", feature = "test-helpers"))]
const NEST_DIAL_OVERRIDE_KEY: &str = "fauna_e2e_nest_dial_override";

/// Mirror the SPA's dial override into this chunk's copy of the seam. Absent
/// clears it: a stale override would point a later launch at a torn-down nest.
#[cfg(all(target_arch = "wasm32", feature = "test-helpers"))]
fn seed_nest_dial_override_from_storage() {
    use wasm_bindgen::JsCast;
    let stored = (|| {
        let storage =
            js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("sessionStorage")).ok()?;
        let get_item: js_sys::Function =
            js_sys::Reflect::get(&storage, &JsValue::from_str("getItem"))
                .ok()?
                .dyn_into()
                .ok()?;
        get_item
            .call1(&storage, &JsValue::from_str(NEST_DIAL_OVERRIDE_KEY))
            .ok()?
            .as_string()
    })();
    fauna_launch_machine::set_nest_dial_override(stored);
}

/// The launch clock's offset in seconds — one half of the `clock` state key
/// (`fauna_e2e_agent::CLOCK_KEY` owns the shape) web's automation surface
/// publishes. `f64`, not `i64`: an `i64` crosses wasm-bindgen as a `BigInt`,
/// which `JSON.stringify` refuses; hours-scale seconds are exact in an `f64`.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = launchClockOffsetSecsForTest)]
pub fn launch_clock_offset_secs_for_test() -> f64 {
    fauna_launch_machine::launch_clock::clock_offset_secs() as f64
}

/// Epoch seconds on the launch clock (real clock plus the offset above) — the
/// other half of the `clock` state key.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = launchClockNowSecsForTest)]
pub fn launch_clock_now_secs_for_test() -> f64 {
    fauna_launch_machine::launch_clock::now_secs_or_zero() as f64
}
