//! `#[wasm_bindgen]` face over the app side of the region content plane
//! (`libs/fauna-client-region`; `docs/goal/behavior/region-blocking.md`
//! § The content plane) — the web twin of the UniFFI `FfiRegionPlane`
//! (`libs/fauna-ffi/src/region.rs`). One shared plane, exposed once at each
//! boundary (priority #2).
//!
//! What stays in the SPA is what the design allows to diverge: the leaf
//! (`navigator.language`, handed over as the raw BCP 47 tag — the region
//! subtag is parsed here, by the shared `declared_from_bcp47`), where the
//! device record's bytes live (browser storage: the page has no filesystem),
//! and the paint.

use std::cell::RefCell;
use std::rc::Rc;

use fauna_client_region::{
    DeclaredRegion, PolicyState, RegionPlane, declared_from_bcp47, effective_registry,
};
use fauna_core::obligation::{ContentPolicy, ViewerThresholds, render_verdict_composed};
use js_sys::Promise;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

use crate::rpc::{WsRpcClient, from_js, to_js};

fn now_secs() -> u64 {
    (js_sys::Date::now() / 1000.0) as u64
}

struct Inner {
    plane: RegionPlane,
    last_refresh: Option<u64>,
}

/// The device's region plane — one per browser profile, opened at app start
/// **ahead of** the first fetch (§ Fail posture) and kept across sign-out
/// (a region is a fact about the device, not the account).
#[wasm_bindgen]
pub struct WasmRegionPlane {
    inner: Rc<RefCell<Inner>>,
}

#[wasm_bindgen]
impl WasmRegionPlane {
    /// Open the plane. `language_tag` is the leaf — `navigator.language`;
    /// `record` is the device record the SPA last persisted (`null` when none).
    #[wasm_bindgen(constructor)]
    pub fn new(language_tag: Option<String>, record: Option<Vec<u8>>) -> WasmRegionPlane {
        let declared = language_tag.as_deref().and_then(declared_from_bcp47);
        let (declared, registry) = e2e_overrides(declared);
        let mut plane = RegionPlane::new(declared, registry);
        plane.load(record.as_deref(), now_secs());
        WasmRegionPlane {
            inner: Rc::new(RefCell::new(Inner {
                plane,
                last_refresh: None,
            })),
        }
    }

    /// Ask the session's nest (the relay) for every policy on the declared
    /// chain and fold the answers. Resolves to the new device record
    /// (`Uint8Array`) when it changed — the SPA persists it and repaints the
    /// surfaces that composed a verdict — or `null`. A failed ask writes
    /// nothing; nothing declared asks nothing.
    pub fn refresh(&self, client: &WsRpcClient) -> Promise {
        let inner = Rc::clone(&self.inner);
        let nest = client.inner_client();
        future_to_promise(async move {
            let chain = {
                let mut i = inner.borrow_mut();
                let chain = i.plane.chain();
                if chain.is_empty() {
                    return Ok(JsValue::NULL);
                }
                i.last_refresh = Some(now_secs());
                chain
            };
            let replies = fauna_client_region::fetch_chain(&nest, &chain).await;
            let mut i = inner.borrow_mut();
            if !i.plane.apply_replies(replies, now_secs()) {
                return Ok(JsValue::NULL);
            }
            // Empty: the stored record did not decode, and is never written
            // over (`RegionPlane::to_bytes`) — answer "nothing to persist".
            let bytes = i.plane.to_bytes();
            if bytes.is_empty() {
                return Ok(JsValue::NULL);
            }
            Ok(js_sys::Uint8Array::from(bytes.as_slice()).into())
        })
    }

    /// Whether the shared cadence
    /// (`fauna_core::region_authority::REFRESH_INTERVAL_SECS`) makes a
    /// [`Self::refresh`] due — the SPA's periodic tick asks this.
    #[wasm_bindgen(js_name = refreshDue)]
    pub fn refresh_due(&self) -> bool {
        fauna_client_region::refresh_due(self.inner.borrow().last_refresh, now_secs())
    }

    /// Forget the refresh clock on an identity change; the plane stays.
    #[wasm_bindgen(js_name = clearSession)]
    pub fn clear_session(&self) {
        self.inner.borrow_mut().last_refresh = None;
    }

    /// The render decision for one item — the twin of the native
    /// `FfiRegionPlane::render`: the region, the guardian floor and the
    /// viewer's own thresholds composed strictest-wins over `labels` with the
    /// chain's scorer factors joined. Returns
    /// `{ verdict, placeholder: { verb, region, authorityName, reason } | null }`;
    /// `placeholder` is set exactly when the region drove a `block` or
    /// `collapse`.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        labels: JsValue,
        content_policy: JsValue,
        own_spam_permille: Option<u16>,
        own_phishing_permille: Option<u16>,
        content_id_hex: Option<String>,
        author_hex: Option<String>,
        text: String,
        hashtags: Vec<String>,
        has_media: bool,
        lang: String,
    ) -> Result<JsValue, JsValue> {
        let labels: Vec<fauna_core::content_category::ContentLabelEntry> = from_js(labels)?;
        let policy: Option<ContentPolicy> =
            if content_policy.is_null() || content_policy.is_undefined() {
                None
            } else {
                Some(from_js(content_policy)?)
            };
        let own = own_spam_permille
            .zip(own_phishing_permille)
            .map(|(s, p)| ViewerThresholds {
                spam_permille: s,
                phishing_permille: p,
            });
        let inner = self.inner.borrow();
        let id = content_id_hex
            .as_deref()
            .and_then(|h| fauna_core::hex32::decode(h).ok());
        let joined = inner.plane.join_labels(id.as_ref(), &labels, || {
            let author = author_hex
                .as_deref()
                .and_then(|h| fauna_core::hex32::decode(h).ok())
                .map(fauna_core::identity::ActorId)
                .unwrap_or(fauna_core::identity::ActorId([0; 32]));
            fauna_client_region::scorer_input(author, &text, &hashtags, has_media)
        });
        let composed =
            render_verdict_composed(&joined, policy.as_ref(), own, &inner.plane.rule_sets());
        let placeholder = fauna_client_region::placeholder_for(&composed, &lang).map(|p| {
            serde_json::json!({
                "verb": p.verb.as_str(),
                "region": p.region.as_str(),
                "authorityName": p.authority_name,
                "reason": p.reason,
            })
        });
        to_js(&serde_json::json!({
            "verdict": composed.verdict.as_str(),
            "placeholder": placeholder,
        }))
    }

    /// What the settings region surface paints:
    /// `{ declared: { code, source, sourceLabelKey } | null, policies: [{ region,
    /// authorityName, sequence, issuedAt, state, inertVersion }], lastCheckedAt,
    /// stale }` — `state` is `applied` | `inert` | `malformed`; times are unix
    /// seconds (numbers, never bigints).
    pub fn view(&self) -> Result<JsValue, JsValue> {
        let view = self.inner.borrow().plane.view(now_secs());
        let declared = view.declared.map(|d| {
            serde_json::json!({
                "code": d.code.as_str(),
                "source": d.source,
                "sourceLabelKey": d.source.label_key(),
            })
        });
        let policies: Vec<serde_json::Value> = view
            .policies
            .into_iter()
            .map(|p| {
                let (state, inert_version) = match p.state {
                    PolicyState::Applied => ("applied", None),
                    PolicyState::Inert { version } => ("inert", Some(version)),
                    PolicyState::Malformed(_) => ("malformed", None),
                };
                serde_json::json!({
                    "region": p.region.as_str(),
                    "authorityName": p.authority_name,
                    "sequence": p.sequence as f64,
                    "issuedAt": p.issued_at as f64,
                    "state": state,
                    "inertVersion": inert_version,
                })
            })
            .collect();
        to_js(&serde_json::json!({
            "declared": declared,
            "policies": policies,
            "lastCheckedAt": view.last_checked_at.map(|t| t as f64),
            "stale": view.stale,
        }))
    }
}

/// The browser storage keys a journey writes before a reload to declare the
/// synthetic region and trust its test-only authority — web's twin of the
/// native apps' `FAUNA_E2E_REGION_DECLARED` / `FAUNA_E2E_REGION_REGISTRY`
/// environment (a page reads no environment). `test-helpers` builds only
/// (convention 15): a production `just web` build never names them.
#[cfg(feature = "test-helpers")]
fn e2e_overrides(
    declared: Option<DeclaredRegion>,
) -> (
    Option<DeclaredRegion>,
    fauna_core::region_authority::RegionRegistry,
) {
    use fauna_client_region::RegionSource;
    use fauna_client_region::registry::E2E_REGISTRY_ENV;
    use fauna_client_region::source::E2E_DECLARED_ENV;
    let storage = web_sys::window().and_then(|w| w.local_storage().ok().flatten());
    let read = |key: &str| {
        storage
            .as_ref()
            .and_then(|s| s.get_item(key).ok().flatten())
            .filter(|v| !v.is_empty())
    };
    let declared = match read(E2E_DECLARED_ENV) {
        Some(code) => DeclaredRegion::from_os_code(&code, RegionSource::BrowserLocale),
        None => declared,
    };
    let registry = match read(E2E_REGISTRY_ENV) {
        Some(seed) => fauna_client_region::registry::seeded_registry(&seed),
        None => effective_registry(),
    };
    (declared, registry)
}

#[cfg(not(feature = "test-helpers"))]
fn e2e_overrides(
    declared: Option<DeclaredRegion>,
) -> (
    Option<DeclaredRegion>,
    fauna_core::region_authority::RegionRegistry,
) {
    (declared, effective_registry())
}
