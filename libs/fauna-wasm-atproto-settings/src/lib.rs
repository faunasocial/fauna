//! WASM bindings for the Fauna Bluesky/ATProto integration-depth settings page
//! — the page-level `AtprotoSettingsMachine` (the four-rung depth selector +
//! transition card, the hosted-identity panel, and the full-PDS login-plane
//! surface it gates: app credentials, connected-app sessions, the
//! external-apps kill-switch). The web atproto-settings route renders off
//! this shared machine via `$lib/wasm-atproto-settings`, mirroring
//! `fauna-wasm-labeler-catalog` (the page-level wrapper shape) — one shared
//! surface across clients (priority #1/#2; `docs/goal/ui/atproto.md`).
//!
//! Unlike labeler-catalog, this machine's mint/reveal secrets are custodied on
//! the account plane (`fauna.state.atproto`, D3), so the constructor additionally takes the actor's raw
//! secret to derive an `ActorKeypair` — the same shape `fauna-wasm`'s
//! `WasmMailSettingsMachine::build` uses.
//!
//! This chunk also hosts web's [`fauna_client_alerts::CriticalAlerts`]
//! registry for feeder #1, the ATProto genesis-seniority custody check
//! (`docs/goal/behavior/critical-alerts.md`). A native app shares ONE
//! process-wide registry across every page; web cannot — each lazy-loaded
//! wasm chunk is a **separately compiled binary with its own linear memory**,
//! so a Rust `Arc` cannot cross from this module into another one. The
//! registry therefore lives here (the only chunk with a feeder today, module
//! statics living as long as this JS module stays imported — i.e. the app
//! session), and the SPA's `$lib/critical-alerts.ts` is the aggregation layer
//! a native app gets for free from its single process: it wires this
//! chunk's `subscribeCriticalAlerts`/`criticalAlertsActive` into one shared
//! store the shell renders, and does the same for any future chunk that
//! grows its own feeder.

use std::sync::Arc;

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

use fauna_atproto_settings_machine::{
    AtprotoSettingsMachine as InnerMachine, AtprotoSettingsObserver as InnerObserver,
};
use fauna_client_alerts::CriticalAlerts;
use fauna_client_alerts::wasm_glue::{AlertsRegistryCell, JsCriticalAlertsObserver};
use fauna_core::identity::ActorKeypair;

use fauna_wasm_panic_hook::err_to_js;

static ALERTS_REGISTRY: AlertsRegistryCell = AlertsRegistryCell::new();

/// The registry feeder #1 posts to; constructed once per loaded module
/// instance (an app session, on web).
fn alerts_registry() -> Arc<CriticalAlerts> {
    ALERTS_REGISTRY.get()
}

/// Repaint hook: fires on every post/clear against this chunk's registry.
/// `$lib/critical-alerts.ts` calls this once the chunk loads.
#[wasm_bindgen(js_name = subscribeCriticalAlerts)]
pub fn subscribe_critical_alerts(observer: JsCriticalAlertsObserver) {
    fauna_client_alerts::wasm_glue::subscribe(&ALERTS_REGISTRY, observer);
}

/// The active alerts as a JSON array of `{ key, lines: [{ key, args }] }` —
/// same shape as [`AtprotoSettingsMachine::snapshot_json`], parsed by the
/// caller and each line resolved through `resolveLocalized`.
#[wasm_bindgen(js_name = criticalAlertsActive)]
pub fn critical_alerts_active() -> String {
    fauna_client_alerts::wasm_glue::active_json(&ALERTS_REGISTRY)
}

/// The **pre-fetch** page state as JSON — what the route renders after mount
/// and before its first `refresh()` resolves. Same shape as
/// [`WasmAtprotoSettingsMachine::snapshot_json`], parsed by the same caller-side
/// parser, so the route has exactly one snapshot type.
///
/// **Why this exists** (the UniFFI twin carries the long form): the default is
/// not "all fields empty" — five fields are deliberately non-zero, and
/// `hosted_gate_reason` is a ratified UI obligation (a closed gate must say why,
/// `ui/README.md` § Copy comprehensibility rule 5). Every app that
/// hand-rolled a stand-in for it drifted or shipped the inverse; web's was a
/// one-key `PENDING_GATE_REASON` const that papered over that single field while
/// the other four defaults came from ad-hoc per-site fallbacks.
#[wasm_bindgen(js_name = atprotoSettingsPrefetchSnapshot)]
pub fn atproto_settings_prefetch_snapshot() -> String {
    serde_json::to_string(&fauna_atproto_settings_machine::AtprotoSettingsSnapshot::default())
        .unwrap_or_default()
}

/// Drop every active alert — the identity-teardown boundary (sign-out,
/// account switch, nest-untrust, factory reset;
/// `critical-alerts.md` § Mechanism → *Lifetime*). Safe to call even with no
/// alert ever posted.
#[wasm_bindgen(js_name = clearAllCriticalAlerts)]
pub fn clear_all_critical_alerts() {
    fauna_client_alerts::wasm_glue::clear_all(&ALERTS_REGISTRY);
}

/// The D10 delegation row's granted capabilities in **user voice**, one
/// `LocalizedText` per wire spelling in cert order, as serde JSON — the shared
/// answer behind `atproto-delegation-scope`.
///
/// Exists because the snapshot carries the row's `capabilities` as their wire
/// spellings (`"Post"`, `"UpdateProfile"`) and the page must not re-derive the
/// map: tui and linux hold the row as Rust and call
/// `DelegationRow::capability_labels`, but a shell across this boundary has
/// only fields, and a hand-written TS copy would be the fourth (priority #4 —
/// the fourth copy is where they start disagreeing). An unrecognized
/// capability degrades to its wire form rather than vanishing, because
/// silently dropping one would *understate* a grant.
#[wasm_bindgen(js_name = delegationCapabilityLabels)]
pub fn delegation_capability_labels(capabilities: Vec<String>) -> String {
    let labels: Vec<fauna_core::localized::LocalizedText> = capabilities
        .iter()
        .map(|c| fauna_atproto_settings_machine::delegation_capability_label(c))
        .collect();
    serde_json::to_string(&labels).unwrap_or_default()
}

/// The D10 delegation row's liveness in **user voice**, as serde JSON — the
/// shared answer behind `atproto-delegation-status`'s prose.
///
/// Same cross-boundary reasoning as [`delegation_capability_labels`]. ⚠ The
/// **wire** spelling, not this text, is what the e2e asserts — it rides the
/// leaf's `state` attr — so a wording change never breaks a test.
#[wasm_bindgen(js_name = delegationStatusLabel)]
pub fn delegation_status_label(liveness: String) -> String {
    serde_json::to_string(&fauna_atproto_settings_machine::delegation_liveness_label(
        &liveness,
    ))
    .unwrap_or_default()
}

/// The identity summary's status in **user voice**, as serde JSON — the shared
/// answer behind `atproto-hosted-handle`'s status word (tui and linux call
/// `identity_status_label` in-process; a web shell has only the wire string).
/// An unrecognized status degrades to its wire word rather than blanking.
#[wasm_bindgen(js_name = identityStatusLabel)]
pub fn identity_status_label(status: String) -> String {
    serde_json::to_string(&fauna_atproto_settings_machine::identity_status_label(
        &status,
    ))
    .unwrap_or_default()
}

/// Test-only: point this module's genesis-seniority custody check (feeder #1,
/// above) at a fake PLC directory for the rest of this page's lifetime — the
/// wasm twin of native's `FAUNA_ATPROTO_PLC_DIRECTORY_URL` env var (a browser
/// has no process environment). Gated on this crate's off-by-default
/// `test-helpers` feature, so it does NOT ship in production wasm (mirrors
/// `fauna-wasm`'s `enableDnsFakeProviderForTest`). Invoked only from the
/// Playwright e2e bridge (`window.__fauna_enableFakePlcDirectoryForTest`),
/// never by production code.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = enableFakePlcDirectoryForTest)]
pub fn enable_fake_plc_directory_for_test(url: String) {
    fauna_client_atproto::genesis_verify::enable_fake_plc_directory_for_test(url);
}

/// Test-only: move the D10 delegation row's **render** clock, so the
/// `expiring_soon` / `expired` liveness states are reachable without waiting
/// out the real ~90-day window (testing.md convention 14 — a fake clock, never
/// a sleep). The wasm twin of native's `atproto_delegation_advance_clock` agent
/// command; web's caller is `$lib/atproto-delegation-e2e`, behind the SPA's own
/// `__FAUNA_E2E_AUTOMATION__` define.
///
/// **Never the mint clock** — `authorizeExternalApps` always stamps a fresh
/// cert with the real wall clock (see `delegation_clock`'s module docs), so an
/// offset left behind lapses the very next delegation this page mints. Pass `0`
/// to reset; nothing auto-resets it.
///
/// ⚠ Reaching the setter needs BOTH gates: this crate's `test-helpers` feature
/// (which is what puts the export in the `pkg-test` flavor) **and** the machine
/// crate's `e2e-agent`, which `test-helpers` forwards. `wasm-pack` builds
/// `--release`, so `cfg(debug_assertions)` is off and the forward is the only
/// thing that compiles the offset in at all — without it this export would
/// exist and silently do nothing.
///
/// ⚠ **`f64`, not `i64`, and that is not a rounding compromise.** wasm-bindgen
/// maps an `i64` parameter to a JS **BigInt**, so an ordinary JS number reaches
/// it as `TypeError: Cannot convert 6912000 to a BigInt` — which surfaces as a
/// bridge 500 in the middle of a journey test and reads like a product failure.
/// The native FFI twin keeps `i64` (UniFFI has real 64-bit integers on both
/// Kotlin and Swift); only this boundary needs the `f64` hop, and an offset in
/// **seconds** is exactly representable there far beyond any window a test
/// moves.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = atprotoDelegationSetClockOffsetForTest)]
pub fn atproto_delegation_set_clock_offset_for_test(offset_secs: f64) {
    fauna_atproto_settings_machine::set_delegation_clock_offset_secs(offset_secs as i64);
}

#[wasm_bindgen]
extern "C" {
    pub type JsAtprotoSettingsObserver;
    #[wasm_bindgen(method, js_name = onChanged)]
    fn on_changed(this: &JsAtprotoSettingsObserver);
}

struct ObserverShim(JsAtprotoSettingsObserver);
// SAFETY: wasm32 is single-threaded; the JS object never crosses a thread.
unsafe impl Send for ObserverShim {}
unsafe impl Sync for ObserverShim {}
impl InnerObserver for ObserverShim {
    fn on_changed(&self) {
        self.0.on_changed()
    }
}

#[wasm_bindgen]
pub struct AtprotoSettingsMachine(Arc<InnerMachine>);

#[wasm_bindgen]
impl AtprotoSettingsMachine {
    /// Build the page machine over the SPA core chunk's socket, lent as
    /// `port` (a `SharedRpcPort` — `$lib/rpc`'s `sharedRpcPort`; the owner's
    /// `requestRaw` runs every request this machine makes, so web keeps one
    /// WebSocket per actor). Throws on an object that is not a port. `secret`
    /// is the actor's 32-byte ed25519 seed (signs the D10 delegation, and backs
    /// the BackupKey derivation for the rotation-key custody —
    /// same shape as `build_mail_settings_machine`). Wire the credential store
    /// with `setAccountPort` next; state starts empty — call `refresh()` after.
    #[wasm_bindgen(constructor)]
    pub fn new(
        observer: JsAtprotoSettingsObserver,
        port: fauna_rpc_wasm::JsRpcPort,
        secret: Vec<u8>,
    ) -> Result<AtprotoSettingsMachine, JsValue> {
        let arr: [u8; 32] = secret
            .try_into()
            .map_err(|_| JsValue::from_str("secret must be 32 bytes"))?;
        let keypair = ActorKeypair::from_secret(arr);
        let observer: Arc<dyn InnerObserver> = Arc::new(ObserverShim(observer));
        let client = fauna_rpc_wasm::WsRpcClient::over_port(port.into())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(AtprotoSettingsMachine(
            fauna_atproto_settings_machine::build_atproto_settings_machine(
                client,
                keypair,
                observer,
                Some(alerts_registry()),
            ),
        ))
    }

    /// Wire the tab's account runtime, lent as `port` (a `SharedAccountPort`
    /// — `$lib/account-runtime`'s `sharedAccountPort`, minted for this
    /// machine's account), as the page's two account-plane seams — the machine
    /// and adapters the six native apps run, the runtime answering from the
    /// core chunk (`account-client-lifecycle.md` § The client-side lifecycle →
    /// *The account port*, decision (h)):
    ///
    /// - the ATProto identity custody door: the held senior rotation keys and
    ///   their custody records (`fauna.state.atproto-identity`) are read and
    ///   joined through it;
    /// - the credential store: the minted app-credential secrets
    ///   (`fauna.state.atproto`) rest there.
    ///
    /// With no runtime serving this account every custody read is "cannot
    /// verify", no credential is revealable, and every write is refused.
    /// Throws on an object that is not a port. Call it beside the constructor,
    /// before the first `refresh()`.
    #[wasm_bindgen(js_name = setAccountPort)]
    pub fn set_account_port(&self, port: fauna_account_port::JsAccountPort) -> Result<(), JsValue> {
        let port: JsValue = port.into();
        let transport = || {
            fauna_account_port::JsAccountTransport::new(port.clone())
                .map_err(|e| JsValue::from_str(&e.to_string()))
        };
        self.0.set_identity_store(Arc::new(
            fauna_client_atproto::port::PortAtprotoIdentityStore::new(transport()?),
        ));
        self.0.set_credential_store(Arc::new(
            fauna_atproto_settings_machine::port::PortAtprotoCredentials::new(transport()?),
        ));
        Ok(())
    }

    /// The whole renderable ATProto settings surface in one JSON object —
    /// the depth selector (level, gate verdict, staged transition card), the
    /// hosted-identity panel, and the F1 login-plane rows
    /// (`fauna_atproto_settings_machine::snapshots::AtprotoSettingsSnapshot`,
    /// serde JSON). Never carries a secret — mint/reveal return theirs
    /// directly, once.
    #[wasm_bindgen(js_name = snapshotJson)]
    pub fn snapshot_json(&self) -> String {
        serde_json::to_string(&self.0.snapshot()).unwrap_or_default()
    }

    /// Re-read credential rows + kill-switch state + live sessions. Resolves
    /// when done (read `snapshotJson` for the result / `error`).
    #[wasm_bindgen(js_name = refresh)]
    pub fn refresh(&self) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        future_to_promise(async move {
            inner.refresh().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    // ── The depth selector (`ui/atproto.md` § Layout & flow) ────────────

    /// Select a target level (`atproto-depth-*`). Never mutates the level by
    /// itself: an effectful move stages the transition card for an explicit
    /// confirm; the one effect-free move (Off → Linked) applies immediately.
    /// Resolves when done (read `snapshotJson` for the result / `error`).
    #[wasm_bindgen(js_name = selectLevel)]
    pub fn select_level(&self, target_level: String) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        future_to_promise(async move {
            inner.select_level(target_level).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Confirm the staged transition (`atproto-depth-confirm`) — the ONE nest
    /// call per confirmed level change. Resolves when done (read
    /// `snapshotJson` for the result / `error`; on failure the card stays
    /// open with the page error populated).
    #[wasm_bindgen(js_name = confirmTransition)]
    pub fn confirm_transition(&self) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        future_to_promise(async move {
            inner.confirm_transition().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Close the staged transition card without changes
    /// (`atproto-depth-cancel`). Synchronous — the machine notifies the
    /// registered observer itself.
    #[wasm_bindgen(js_name = cancelTransition)]
    pub fn cancel_transition(&self) {
        self.0.cancel_transition();
    }

    /// Choose the mint's DID method (`atproto-did-method-*`; pre-mint only).
    /// Synchronous — the machine notifies the registered observer itself.
    #[wasm_bindgen(js_name = setDidMethod)]
    pub fn set_did_method(&self, method: String) {
        self.0.set_did_method(method);
    }

    /// The history-backfill opt-in (`atproto-history-backfill`; rides the
    /// next minting transition's card). Synchronous — the machine notifies
    /// the registered observer itself.
    #[wasm_bindgen(js_name = setHistoryBackfill)]
    pub fn set_history_backfill(&self, enabled: bool) {
        self.0.set_history_backfill(enabled);
    }

    /// Mint a new app credential, resolving its secret **once** (shown for the
    /// user to copy into their ATProto app — it is not re-shown by
    /// `snapshotJson`, only by `revealSecret`). Rejects on nest/store failure;
    /// on a partial local-save failure the secret still resolves, with
    /// `snapshotJson().error` set to explain (callers must surface both).
    #[wasm_bindgen(js_name = mint)]
    pub fn mint(&self, label: String, dm_allowed: bool) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        future_to_promise(async move {
            let secret = inner.mint(label, dm_allowed).await.map_err(err_to_js)?;
            Ok(JsValue::from_str(secret.as_str()))
        })
    }

    /// Recover a credential's secret from this client's own local config (a
    /// pure local read — the nest structurally cannot answer, D3). Gate the
    /// affordance on the row's `revealable` flag; rejects
    /// `SecretUnavailable` otherwise (a normal sibling-device state, not an
    /// error to alarm on).
    #[wasm_bindgen(js_name = revealSecret)]
    pub fn reveal_secret(&self, credential_id: String) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        future_to_promise(async move {
            let secret = inner
                .reveal_secret(credential_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_str(secret.as_str()))
        })
    }

    /// Revoke an app credential (cascades to its sessions), then refresh.
    /// Resolves when done (read `snapshotJson` for the result / `error`).
    #[wasm_bindgen(js_name = revoke)]
    pub fn revoke(&self, credential_id: String) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        future_to_promise(async move {
            inner.revoke(credential_id).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Revoke one live session by its hex id, then refresh. Resolves when
    /// done (read `snapshotJson` for the result / `error`).
    #[wasm_bindgen(js_name = revokeSession)]
    pub fn revoke_session(&self, session_id_hex: String) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        future_to_promise(async move {
            inner.revoke_session(session_id_hex).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Flip the per-account external-apps kill-switch, then refresh. Resolves
    /// when done (read `snapshotJson` for the result / `error`).
    #[wasm_bindgen(js_name = setExternalAppsEnabled)]
    pub fn set_external_apps_enabled(&self, enabled: bool) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        future_to_promise(async move {
            inner.set_external_apps_enabled(enabled).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    // ── D10 delegated authoring (`atproto-pds-full.md` § D10) ────────────
    //
    // EXPORTED 2026-08-15, on exactly the condition the machine crate's own
    // withholding comment set. `AtprotoSettingsMachine::{authorize,deauthorize}
    // _external_apps` were deliberately kept off the wasm face while web
    // rendered no delegation row — an exported-but-uncalled surface is the
    // *dark capability* class (`ui/nests.md` § Trust facet). Web builds the row
    // in this same change, so the export lands with its renderer, which is the
    // rule rather than an exception to it.

    /// Authorize external ATProto apps to post as this account — the whole D10
    /// mint ceremony in one gesture (`atproto-delegation-authorize`).
    ///
    /// Also the **renewal** gesture: provisioning overwrites the stored cert
    /// with a freshly dated one, so a lapsed grant recovers without a revoke
    /// first (`atproto-pds-full.md` § App surface → *Re-authorizing is the
    /// renewal gesture*). Resolves when done (read `snapshotJson` for the
    /// resulting row / `error`).
    #[wasm_bindgen(js_name = authorizeExternalApps)]
    pub fn authorize_external_apps(&self) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        future_to_promise(async move {
            inner.authorize_external_apps().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Revoke the authoring delegation (`atproto-delegation-revoke`) —
    /// destructive: it destroys the signing sub-key `K` nest-side. Already
    /// published posts stay verifiable forever (their cert rides their own
    /// wire), so this stops *future* authoring, never history. Resolves when
    /// done (read `snapshotJson` for the result / `error`).
    #[wasm_bindgen(js_name = deauthorizeExternalApps)]
    pub fn deauthorize_external_apps(&self) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        future_to_promise(async move {
            inner.deauthorize_external_apps().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    // ── The OAuth consent ceremony (F4 rung 2) ───────────────────────────

    /// Answer one pending consent request (`atproto-consent-approve` /
    /// `atproto-consent-deny`), then refresh. This call — over the user's own
    /// authed connection — is the ceremony's trust root; a decline is
    /// recorded, never a local dismiss, so the waiting browser gets a clean
    /// refusal instead of a timeout. Resolves when done (read `snapshotJson`
    /// for the result / `error`).
    #[wasm_bindgen(js_name = resolveConsent)]
    pub fn resolve_consent(&self, consent_id_hex: String, approved: bool) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        future_to_promise(async move {
            inner.resolve_consent(consent_id_hex, approved).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    // ── The 72 h recovery-fork contest ceremony (`atproto-contest-*`) ────
    //
    // Wholly client-side (`atproto-identity-custody.md` § The 72 h
    // recovery-fork contest, decision 9): every call here is the browser's
    // own connection, never a nest round trip through `inner` — the async
    // ones are network round trips to the public PLC directory all the same,
    // which is why `requestContest` still returns a `Promise`.

    /// Open the contest ceremony (`atproto-contest`). Synchronous — the
    /// machine notifies the registered observer itself.
    #[wasm_bindgen(js_name = openContestConfirm)]
    pub fn open_contest_confirm(&self) {
        self.0.open_contest_confirm();
    }

    /// Close the ceremony without acting (`atproto-contest-cancel`).
    /// Synchronous — the machine notifies the registered observer itself.
    #[wasm_bindgen(js_name = cancelContest)]
    pub fn cancel_contest(&self) {
        self.0.cancel_contest();
    }

    /// Record the scoped consent and run the contest converge
    /// (`atproto-contest-confirm`) — builds, signs and submits the recovery
    /// fork over this client's own connection to the public PLC directory.
    /// Resolves when done (read `snapshotJson` for the result / `error`; on
    /// failure the card stays open with the page error populated).
    #[wasm_bindgen(js_name = requestContest)]
    pub fn request_contest(&self) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        future_to_promise(async move {
            inner.request_contest().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    // ── "Delete my Bluesky presence" (`atproto-delete-*`) ────────────────
    //
    // Row 7's six-app trickle-down: open/cancel are pure-local machine
    // mutations (the machine notifies the registered observer itself, the
    // contest ceremony's shape above); confirm is the one wire call, the
    // nest's own `fauna.bridges.atproto.delete_presence`.

    /// Open the delete ceremony (`atproto-delete-presence`). Synchronous —
    /// the machine notifies the registered observer itself.
    #[wasm_bindgen(js_name = openDeleteConfirm)]
    pub fn open_delete_confirm(&self) {
        self.0.open_delete_confirm();
    }

    /// Close the ceremony without acting (`atproto-delete-cancel`).
    /// Synchronous — the machine notifies the registered observer itself.
    #[wasm_bindgen(js_name = cancelDelete)]
    pub fn cancel_delete(&self) {
        self.0.cancel_delete();
    }

    /// Perform the sweep (`atproto-delete-confirm`) — the ONE wire call of
    /// the ceremony. Resolves when done (read `snapshotJson` for the result /
    /// `error`; on failure the card stays open with the page error
    /// populated).
    #[wasm_bindgen(js_name = confirmDelete)]
    pub fn confirm_delete(&self) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        future_to_promise(async move {
            inner.confirm_delete().await;
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
    fauna_wasm_panic_hook::install("fauna-wasm-atproto-settings");
}

/// Test-only: deliberately panics, so an e2e can assert the hook above really
/// names this chunk in the browser console — a headless witness, not a
/// review-only claim. Compiled out of every non-`test-helpers` build.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = panicForTestOnly)]
pub fn panic_for_test_only() {
    panic!("deliberate test panic");
}
