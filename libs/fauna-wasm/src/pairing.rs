//! `#[wasm_bindgen]` exposure of the user-settings Linked-nests machine to the
//! Svelte SPA — the WASM twin of the UniFFI exposure in `fauna-ffi/src/pairing.rs`
//! (tracked internally). Lets the web `linked-nests` page drive
//! the shared `fauna-client-pair` machine instead of re-implementing link/list/
//! unlink logic in TypeScript (priority #2/#3; `docs/goal/behavior/
//! linked-nests.md` § Where logic lives: "Per-app — render the snapshot,
//! dispatch actions; no pairing logic in any shell").
//!
//! Built from the SPA's singleton browser `WsRpcClient` (the
//! `WsRpcClient::linkedNestsMachine()` factory in `src/rpc.rs`). The surface
//! (`snapshot` / `hydrate` / `dispatch`) mirrors the UniFFI one exactly.
//!
//! **Unlike the other `wasm_admin_machine!` machines this one is hand-written**:
//! both-ends linking (`LinkedNestsAction::LinkBoth`) needs a *peer connector*
//! supplied at construction (a second authenticated WS-RPC client to a peer
//! nest), which the shared macro — built for a single-client `build(client)` —
//! cannot express. The `snapshot`/`hydrate`/`dispatch` bodies are otherwise
//! identical to the macro's.
//!
//! The whole module is `#[cfg(target_arch = "wasm32")]` (gated at the `mod` site
//! in `lib.rs`), like `src/mail_admin.rs`.

use std::rc::Rc;

use fauna_client_pair::{
    BackupConnect, LinkedNestsAction, LinkedNestsMachine, PairNestError, PeerConnect,
    build_linked_nests_machine_with_peer, build_linked_nests_machine_with_peer_and_trust,
};
use fauna_core::identity::ActorKeypair;
use fauna_rpc_wasm::WsRpcClient as InnerClient;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

use crate::rpc::{err_to_js, from_js, to_js};

/// Bound on the both-ends peer connect (`connect_peer`). Short enough that an
/// unreachable peer surfaces a connect error well within the user-settings
/// error window, long enough for a real peer's WS open + bearer mint over a slow
/// link. The connector fast-fails on the first `Disconnected` (a refused peer
/// fails in ~1 s), so this is only the backstop for a hung (SYN-dropped) connect.
const PEER_CONNECT_TIMEOUT_MS: u32 = 12_000;

/// Test-only: move the Nests trust facet's RENDER clock
/// (`fauna_client_capabilities::trust_clock`) — grant liveness and the
/// auto-renew due decision, never the mint clock — so `expiring soon` /
/// `paused` are reachable without waiting out the ~90-day window (testing.md
/// convention 14). The wasm twin of the UniFFI `set_trust_clock_offset_secs`,
/// driven by the SPA's `trust_facet_advance_clock` command. Gated on
/// `test-helpers` (convention 15); the `ForTest` suffix is what
/// `scripts/check-wasm-seam-exclusion.py` keys on.
///
/// ⚠ Module-wide, and nothing auto-resets it — zero it once the lapse
/// assertions are done.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = trustSetClockOffsetForTest)]
pub fn trust_set_clock_offset_for_test(offset_secs: f64) {
    fauna_client_capabilities::trust_clock::set_clock_offset_secs(offset_secs as i64);
}

/// Whether a projected scope list marks a **bounded** (content-sealing-epochs)
/// mail grant — the wasm twin of the UniFFI `trust_scope_is_bounded_mail_grant`
/// free fn. `scope` is the JS array off a Now-lens grant row's `scope` field or
/// a History-lens entry's `scope` field (both `TrustScope[]`, `{class, kind,
/// tier}`). The Nests-page trust facet's honest-bound copy
/// (`nests.md` § Honest bound; flip-checklist line 6) uses this to switch
/// between the standing and bounded-regime wording — never re-derive the
/// (class, kind, tier) check in TypeScript (priority #2).
#[wasm_bindgen(js_name = isBoundedMailGrant)]
pub fn is_bounded_mail_grant(scope: JsValue) -> Result<bool, JsValue> {
    let scope: Vec<fauna_client_pair::TrustScope> = from_js(scope)?;
    Ok(fauna_client_pair::trust_scope_is_bounded_mail_grant(scope))
}

// ── shell-facing labels: shared enum → i18n key mapping (wasm twin of the
// UniFFI exports in `fauna_client_pair::trust`) ─────────────────────────
//
// Each returns a `LocalizedText` `{ key, args }` the SPA resolves through
// `resolveLocalized` — linux/tui (native) and android (UniFFI) already
// consume the same shared functions, so web stops hand-rolling its own
// `scopeLabel`/`statusLabel`/`backupStatusLabel`/`mintOptionLabel` match
// statements (priority #2).

/// `fauna_client_pair::grant_scope_labels` — a grant row's or History entry's
/// whole scope line (`nest-trust-grant-scope` / `nest-trust-history-item`'s
/// scope segment), one `LocalizedText` per tuple, naming a folder grant's
/// folder (the row's `folder`, resolved in shared Rust) in place of the bare
/// folder read; a scopeless `Revoke` of such a grant yields the folder alone.
/// `scope` is the row's `scope[]`, `folder` its `folder` (absent → `null`).
#[wasm_bindgen(js_name = grantScopeLabels)]
pub fn grant_scope_labels(scope: JsValue, folder: JsValue) -> Result<JsValue, JsValue> {
    let scope: Vec<fauna_client_pair::TrustScope> = from_js(scope)?;
    let folder: Option<fauna_client_pair::TrustFolder> = from_js(folder)?;
    to_js(&fauna_client_pair::grant_scope_labels(scope, folder))
}

/// `fauna_client_pair::status_label` — a grant's `TrustLiveness`
/// (`nest-trust-grant-status`).
#[wasm_bindgen(js_name = statusLabel)]
pub fn status_label(l: JsValue) -> Result<JsValue, JsValue> {
    let l: fauna_client_pair::TrustLiveness = from_js(l)?;
    to_js(&fauna_client_pair::status_label(l))
}

/// `fauna_client_pair::backup_status_label` — a backup row's
/// `TrustBackupStatus` (`nest-trust-backup-status`). `Unreachable` ("we could
/// not ask the destination") stays distinct from `Missing` ("it answered and
/// holds no such trust") — collapsing them would let a flaky network read as
/// a revoked backup.
#[wasm_bindgen(js_name = backupStatusLabel)]
pub fn backup_status_label(s: JsValue) -> Result<JsValue, JsValue> {
    let s: fauna_client_pair::TrustBackupStatus = from_js(s)?;
    to_js(&fauna_client_pair::backup_status_label(s))
}

/// `fauna_client_pair::mint_duration_options` — the durations
/// `nest-trust-mint-duration-select` offers, in display order (serde variant
/// names: `"OneOff"`, `"Standard"`), so web can never offer a third.
#[wasm_bindgen(js_name = mintDurationOptions)]
pub fn mint_duration_options() -> Result<JsValue, JsValue> {
    to_js(&fauna_client_pair::mint_duration_options())
}

/// `fauna_client_pair::duration_label` — a duration option's localized label.
#[wasm_bindgen(js_name = durationLabel)]
pub fn duration_label(d: JsValue) -> Result<JsValue, JsValue> {
    let d: fauna_client_pair::TrustGrantDuration = from_js(d)?;
    to_js(&fauna_client_pair::duration_label(d))
}

/// `view_model::AUTO_RENEW_CHECK_SECS` — the auto-renew loop's cadence
/// (`nests.md` § Expiry / renewal → *Duration and blessing*), a hard-coded
/// Rust constant the SPA's timer reads rather than re-spells.
#[wasm_bindgen(js_name = autoRenewCheckSecs)]
pub fn auto_renew_check_secs() -> f64 {
    fauna_client_capabilities::view_model::AUTO_RENEW_CHECK_SECS as f64
}

/// `fauna_client_pair::mint_option_label` — a mint-picker option's use case
/// (`nest-trust-mint-scope-select`). The paywalled option carries its tier via
/// the `{tier}` named placeholder. `o` is a `LinkedNestRow.mint_options` entry
/// (the full `TrustMintOption`, not just its `use_case`/`tier`).
#[wasm_bindgen(js_name = mintOptionLabel)]
pub fn mint_option_label(o: JsValue) -> Result<JsValue, JsValue> {
    let o: fauna_client_pair::TrustMintOption = from_js(o)?;
    to_js(&fauna_client_pair::mint_option_label(&o))
}

#[wasm_bindgen]
pub struct WasmLinkedNestsMachine {
    inner: Rc<LinkedNestsMachine>,
}

/// Build the both-ends peer connector from the SPA's *peer-token-provider
/// factory*. The factory is a JS `(peerUrl: string) => (forceRefresh: boolean)
/// => Promise<string>`: given a peer nest's origin it returns a token provider
/// bound to that origin (the SPA's `getAuthToken(secret, peerUrl, …)` over
/// `challengeVerify`). The both-ends `connect_peer` calls it, then builds the peer
/// `WsRpcClient` with the user's *same* identity (the actor id is identical on
/// both nests) — so a nest **address** links both ends in one action, matching
/// native (`linked-nests.md` § One action seeds both ends). The single-end
/// `Link` path never touches the connector. Shared by [`WasmLinkedNestsMachine`]'s
/// pairing-only + trust-enabled constructors.
pub(crate) fn make_peer_connect(
    actor_id_hex: String,
    peer_token_provider_factory: js_sys::Function,
) -> PeerConnect {
    Rc::new(move |peer_url: String| {
        let factory = peer_token_provider_factory.clone();
        let actor_id_hex = actor_id_hex.clone();
        Box::pin(async move {
            let tp = factory
                .call1(&JsValue::NULL, &JsValue::from_str(&peer_url))
                .map_err(|e| {
                    PairNestError::Rejected(format!("peer token-provider factory threw: {e:?}"))
                })?;
            let tp: js_sys::Function = tp.dyn_into().map_err(|_| {
                PairNestError::Rejected("peer token-provider factory must return a function".into())
            })?;
            // Honor the `PeerConnect` contract — hand back a *connected* client.
            // Wait (bounded) for the socket to come up so an unreachable peer
            // surfaces a connect error right here (the wasm twin of native
            // `connect_peer`'s `NestClient::connect().await?`), instead of letting
            // the first `this_nest` request stall for the full per-request
            // deadline (~30 s).
            let client = InnerClient::connect(peer_url.clone(), actor_id_hex, tp);
            client
                .wait_until_connected(PEER_CONNECT_TIMEOUT_MS)
                .await
                // `PairNestError::Transient` Display already reads
                // "nest unreachable: {0}", so the detail is just the address.
                .map_err(|_| PairNestError::Transient(peer_url.clone()))?;
            Ok(client)
        })
    })
}

/// Build the backup-destination connector the Nests-page trust facet needs to
/// read and revoke **writer** grants at each destination, over that
/// destination's own authenticated connection.
///
/// Reuses the enroll path's proven two-step connect verbatim
/// (`rpc::resolve_and_authorize_destination_inner`: anonymous
/// `fauna.auth.handshake` for the bearer, then a `TokenWsRpcClient` session) —
/// the same second-origin problem `make_peer_connect` solves, and the reason
/// this connector is passed *into* `fauna-client-pair` rather than built there.
/// The resolved domain is discarded; the proven identity is handed back so the
/// shared door (`fauna_client_backup::trust::connect_destination`) can hold it
/// to the one the owner enrolled — possession proof plus origin pin, web's
/// structural ceiling (`security.md` § Transport trust, the web rows).
///
/// Routing this through the destination is the whole point of the writer row —
/// a revoke must still work when the source nest is precisely what the user is
/// revoking (`nests.md` § Trust facet — backup rows).
/// `pub(crate)` because the Backups page's audit loop needs the *same* connector
/// without the linked-nests machine around it (`rpc.rs`'s `backupAuditRunPass`
/// over `fauna_client_pair::wasm_backup_destination_connector`) — one copy of the
/// two-step connect for both callers.
pub(crate) fn make_backup_connect(secret_hex: String) -> BackupConnect {
    Rc::new(move |url: String| {
        let secret_hex = secret_hex.clone();
        Box::pin(async move {
            crate::rpc::resolve_and_authorize_destination_inner(&secret_hex, &url)
                .await
                .map(|(proven_id, _domain, authed)| (authed, proven_id))
                // The seam is stringly-typed on both platforms; keep the JS-side
                // detail rather than collapsing it to a bare "failed" — this
                // string is the row's `unreachable` diagnosis.
                .map_err(|e| {
                    e.as_string()
                        .unwrap_or_else(|| format!("connect to backup destination {url}: {e:?}"))
                })
        })
    })
}

impl WasmLinkedNestsMachine {
    /// Build over the SPA's browser WS-RPC client + a peer-token-provider factory
    /// (see [`make_peer_connect`]) — pairing only (no trust facet). The web
    /// `linked-nests` page that doesn't render trust uses this; the Nests page
    /// uses [`Self::build_with_trust`].
    pub(crate) fn build(
        client: InnerClient,
        peer_token_provider_factory: js_sys::Function,
    ) -> Self {
        let connect = make_peer_connect(
            client.actor_id_hex().to_string(),
            peer_token_provider_factory,
        );
        Self {
            inner: Rc::new(build_linked_nests_machine_with_peer(client, connect)),
        }
    }

    /// Build with **both-ends linking and the Nests-page trust facet** — the wasm
    /// twin of native's `build_linked_nests_machine_with_trust`, so the web Nests
    /// page links a nest by address *and* shows/mints its trust grants (priority
    /// #1: web is not a reduced surface). `secret` is the actor's 32-byte ed25519
    /// identity; the trust seams sign the grant-event log
    /// (`fauna.state.succession-ledger`) with it (the raw key never crosses back to JS — the seam
    /// signs). Errors if `secret` is not 32 bytes.
    pub(crate) fn build_with_trust(
        client: InnerClient,
        peer_token_provider_factory: js_sys::Function,
        secret: Vec<u8>,
    ) -> Result<Self, JsValue> {
        let arr: [u8; 32] = secret
            .try_into()
            .map_err(|_| JsValue::from_str("secret must be 32 bytes"))?;
        let keypair = ActorKeypair::from_secret(arr);
        let connect = make_peer_connect(
            client.actor_id_hex().to_string(),
            peer_token_provider_factory,
        );
        // The backup trust rows need a connector to each destination, built here
        // for the second-origin reason `make_peer_connect` exists. With it
        // wired, web's facet carries the same two backup rows native does —
        // priority #1: web is not a reduced surface.
        let backup_connect = make_backup_connect(hex::encode(arr));
        // The set names a web-serve paywall grant's folder is named from —
        // this tab's folder custody plus the owner's folder list over the same
        // session, as on every native app.
        let folder_names = fauna_client_pair::TrustFolderNames::new(
            &keypair,
            std::sync::Arc::new(fauna_client_folders::CustodyOwnedSetNames {
                keys: crate::account_runtime::folder_key_store(keypair.actor_id_hex()),
                nest: client.clone(),
            }),
        );
        Ok(Self {
            inner: Rc::new(
                build_linked_nests_machine_with_peer_and_trust(
                    client,
                    connect,
                    keypair,
                    crate::account_runtime::ledger_seam(),
                    // The backup rows' list and marks: this tab's per-box
                    // `fauna.state.backup` over the same handle.
                    crate::account_runtime::backup_seam(),
                    // The blessing door: the account plane's
                    // `fauna.state.blessed-nests` over this tab's runtime handle —
                    // the toggle, the one-tap trust and the auto-renew loop all
                    // read and write it here, as on every native app.
                    std::sync::Arc::new(
                        fauna_account_seams::blessed_nests::PlaneBlessedNests::new(
                            crate::account_runtime::handle,
                        ),
                    ),
                    crate::account_runtime::mail_store(),
                    Some(backup_connect),
                    crate::account_runtime::period_key_store(),
                )
                .with_folder_names(folder_names),
            ),
        })
    }
}

#[wasm_bindgen]
impl WasmLinkedNestsMachine {
    /// The rendered snapshot as a plain JS object (sync).
    #[wasm_bindgen(js_name = snapshot)]
    pub fn snapshot(&self) -> Result<JsValue, JsValue> {
        to_js(&self.inner.snapshot())
    }

    /// Initial page load. Resolves `undefined`; read state via `snapshot()`.
    #[wasm_bindgen(js_name = hydrate)]
    pub fn hydrate(&self) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            m.hydrate().await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Dispatch an action (a JS object decoded into `LinkedNestsAction`).
    /// Resolves `undefined` on success; read state via `snapshot()`.
    #[wasm_bindgen(js_name = dispatch)]
    pub fn dispatch(&self, action: JsValue) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            let action: LinkedNestsAction = from_js(action)?;
            m.dispatch(action).await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// The 32-byte id the connection is **bound** to — the `target_nest_id`
    /// the web `admin-dns` cert-issuance dispatch (`DnsAction::IssueCert` /
    /// `BeginManualIssueCert`) seals the issued LAN-TLS cert to. A thin
    /// passthrough over the shared [`LinkedNestsMachine::bound_nest_id`] (the
    /// origin's pin, checked against the nest's own `fauna.nest.info` claim —
    /// a nest claiming another id is refused, so no cert is sealed to a
    /// sibling), so web resolves it the same way native does (linux
    /// `resolve_this_nest_id`). The id rides as a JS `number[]`; the URL half
    /// isn't needed for issuance, so only the id is returned.
    #[wasm_bindgen(js_name = thisNestId)]
    pub fn this_nest_id(&self) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            let id = m.bound_nest_id().await.map_err(err_to_js)?;
            to_js(&id)
        })
    }
}
