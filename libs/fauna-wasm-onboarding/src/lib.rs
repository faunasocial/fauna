//! WASM bindings for Fauna onboarding: provisioning, registrar, OnboardingMachine.
//!
//! Split rationale tracked internally. This crate is loaded only when the
//! user enters the onboarding wizard.

use fauna_wasm_panic_hook::err_to_js;
use wasm_bindgen::prelude::*;

// ── Provisioning bindings ────────────────────────────────────

/// Verify a VPS provider credential and return available locations as JSON.
///
/// Accepts JSON: `{"provider": "hetzner"|"digitalocean"|"vultr"|"ovh"|"linode", ...fields}`
///
/// - hetzner / digitalocean / vultr / gandi / linode: `token`
/// - ovh: `app_key`, `app_secret`, `consumer_key`, `project_id`
///
/// Returns a JSON array of `{id, name, city, country}` objects.
#[wasm_bindgen]
pub fn verify_vps_provider(provider_json: &str) -> js_sys::Promise {
    let provider_json = provider_json.to_string();
    wasm_bindgen_futures::future_to_promise(async move {
        let client = reqwest::Client::new();
        let locations =
            fauna_provisioning::verify_json::verify_vps_from_json(&provider_json, &client)
                .await
                .map_err(|msg| JsValue::from_str(&msg))?;
        let json = serde_json::to_string(&locations)
            .map_err(|e| JsValue::from_str(&format!("serialize error: {e}")))?;
        Ok(JsValue::from_str(&json))
    })
}

/// Verify a DNS provider credential and return available zones as JSON.
///
/// Accepts JSON: `{"provider": "cloudflare"|"namecheap"|"porkbun"|"gandi"|"hetzner", ...fields}`
///
/// - cloudflare / gandi / hetzner: `token`
/// - namecheap: `api_user`, `api_key`, `client_ip`
/// - porkbun: `apikey`, `secretapikey`
///
/// Returns a JSON array of `{id, name}` objects.
#[wasm_bindgen]
pub fn verify_dns_provider(provider_json: &str) -> js_sys::Promise {
    let provider_json = provider_json.to_string();
    wasm_bindgen_futures::future_to_promise(async move {
        let client = reqwest::Client::new();
        let zones = fauna_provisioning::verify_json::verify_dns_from_json(&provider_json, &client)
            .await
            .map_err(|msg| JsValue::from_str(&msg))?;
        let json = serde_json::to_string(&zones)
            .map_err(|e| JsValue::from_str(&format!("serialize error: {e}")))?;
        Ok(JsValue::from_str(&json))
    })
}

// `provision_build_cloud_init` was retired by the provisioning-progress
// design — `CloudInitParams` never carries DKIM private key material (DKIM
// keygen is nest-side only; regression-tested by
// `fauna_provisioning::cloud_init::test_cloud_init_embeds_no_dkim_key`),
// and the standalone shim's caller-supplied-params shape doesn't fit the
// orchestrator's own provisioning-progress flow either way. Web callers go
// through `OnboardingMachine.start_provisioning` (when wired in spec stage 8).

// ── Registrar + Orchestrator bindings ────────────────────────
//
// The standalone `provision_nest`, `provision_nest_no_dns`,
// `provision_with_registration`, and `fetch_dkim` wasm shims were
// retired by the provisioning-progress design (tracked internally), and
// `probe_domain_status` was removed at S4c2 with the HTTP setup-status probe
// it wrapped. Web callers now drive provisioning via
// `OnboardingMachine.start_provisioning` (snapshot-pull progress, idempotent
// retry, soft cancel). Seven page-support shims survive below — they're
// independent of the orchestrator and useful from JS for the dns_config,
// handle_entry, and vps_config pages: `verify_vps_provider`,
// `verify_dns_provider`, `registrar_check`, `registrar_register`,
// `registrar_availability`, `registrar_list_tld_pricing`, and
// `vps_list_server_types` (`docs/goal/architecture/provisioning/registry.md`
// § WASM surface owns the full roster).

fn parse_credentials(
    creds_json: &str,
) -> Result<fauna_provisioning::dispatch::Credentials, JsValue> {
    fauna_provisioning::dispatch::parse_credentials(creds_json)
        .map_err(|msg| JsValue::from_str(&msg))
}

/// `"dns"` | `"vps"` → the machine's `CredentialForm` (hosted-auth wrappers).
fn parse_credential_form(form: &str) -> Result<fauna_onboarding_machine::CredentialForm, JsValue> {
    match form {
        "dns" => Ok(fauna_onboarding_machine::CredentialForm::Dns),
        "vps" => Ok(fauna_onboarding_machine::CredentialForm::Vps),
        other => Err(JsValue::from_str(&format!(
            "unknown credential form {other:?} (expected \"dns\" or \"vps\")"
        ))),
    }
}

fn parse_provider_id(provider_id: &str) -> Result<fauna_provisioning::ProviderId, JsValue> {
    fauna_provisioning::dispatch::parse_provider_id(provider_id)
        .map_err(|msg| JsValue::from_str(&msg))
}

/// Check a domain's availability via the registrar API.
///
/// - `provider_id`: lowercase ProviderId discriminant (e.g. `"porkbun"`).
/// - `creds_json`: JSON object mapping credential field ids to values, keyed
///   by the field ids in `providers.yaml` (e.g. `{"api-key":"...","secret-api-key":"..."}`).
///
/// Returns a JSON-encoded `DomainAvailability` on success.
#[wasm_bindgen]
pub fn registrar_check(provider_id: &str, creds_json: &str, domain: &str) -> js_sys::Promise {
    let provider_id = provider_id.to_string();
    let creds_json = creds_json.to_string();
    let domain = domain.to_string();
    wasm_bindgen_futures::future_to_promise(async move {
        use fauna_provisioning::registrar::Registrar;
        let dispatch = fauna_provisioning::dispatch::resolve_registrar(&provider_id, &creds_json)
            .map_err(|msg| JsValue::from_str(&msg))?;
        let client = reqwest::Client::new();
        let availability = dispatch.check(&client, &domain).await.map_err(err_to_js)?;
        let json = serde_json::to_string(&availability)
            .map_err(|e| JsValue::from_str(&format!("serialize error: {e}")))?;
        Ok(JsValue::from_str(&json))
    })
}

/// Register a domain via the registrar API.
///
/// - `agreed_price_cents`: the price the user confirmed in the UI (in integer
///   cents of the currency returned by `registrar_check`). Some registrars
///   require this in the request body to prevent price-change races.
/// - `contact_json`: either the literal string `"null"`, the empty string, or
///   a JSON-encoded `ContactInfo`. Registrars that use account-level WHOIS
///   contacts (Porkbun) accept `"null"`; others must supply a full contact.
///
/// Returns a JSON-encoded `RegistrationResult` on success.
#[wasm_bindgen]
pub fn registrar_register(
    provider_id: &str,
    creds_json: &str,
    domain: &str,
    years: u32,
    agreed_price_cents: u64,
    contact_json: &str,
) -> js_sys::Promise {
    let provider_id = provider_id.to_string();
    let creds_json = creds_json.to_string();
    let domain = domain.to_string();
    let contact_json = contact_json.to_string();
    wasm_bindgen_futures::future_to_promise(async move {
        let client = reqwest::Client::new();
        let result = fauna_provisioning::dispatch::register_from_json(
            &provider_id,
            &creds_json,
            &domain,
            years,
            agreed_price_cents,
            &contact_json,
            &client,
        )
        .await
        .map_err(|msg| JsValue::from_str(&msg))?;
        let json = serde_json::to_string(&result)
            .map_err(|e| JsValue::from_str(&format!("serialize error: {e}")))?;
        Ok(JsValue::from_str(&json))
    })
}

/// Structured availability outcome for a domain at a registrar — the
/// dns_config wizard's per-provider status text consumes this directly.
///
/// - `provider_id`, `creds_json`, `domain`: same as `registrar_check`.
///
/// Returns a JSON-encoded `RegistrarAvailability` (`{"Buyable":{"price_cents":N,"currency":...}}`,
/// `"Unavailable"`, or `"TldNotSupported"`).
#[wasm_bindgen]
pub fn registrar_availability(
    provider_id: &str,
    creds_json: &str,
    domain: &str,
) -> js_sys::Promise {
    let provider_id = provider_id.to_string();
    let creds_json = creds_json.to_string();
    let domain = domain.to_string();
    wasm_bindgen_futures::future_to_promise(async move {
        use fauna_provisioning::registrar::Registrar;
        let dispatch = fauna_provisioning::dispatch::resolve_registrar(&provider_id, &creds_json)
            .map_err(|msg| JsValue::from_str(&msg))?;
        let client = reqwest::Client::new();
        let avail = dispatch
            .availability(&client, &domain)
            .await
            .map_err(err_to_js)?;
        let json = serde_json::to_string(&avail)
            .map_err(|e| JsValue::from_str(&format!("serialize error: {e}")))?;
        Ok(JsValue::from_str(&json))
    })
}

// `provision_nest`, `provision_nest_no_dns`, `provision_with_registration`,
// and `fetch_dkim` shims removed — see comment at the top of this section.

// ── Handle-first onboarding bindings ─────────────────────────
//
// Surfaces feeding steps 5 and 6 of the handle-first flow:
//   - registrar_list_tld_pricing (step 5: "buy domain for $X" hint)
//   - vps_list_server_types    (step 6: VPS plan radio options)
//
// `probe_domain_status` (step 3: nest_select buttons) was removed along
// with the HTTP setup-status probe it wrapped — no web caller remained.

/// Public-pricing TLD table for a registrar. Returns a JSON-encoded
/// `Option<Vec<TldPriceQuote>>` — `null` for registrars without a public
/// price list. `creds_json` is accepted for API symmetry but ignored by
/// the underlying impl on registrars whose pricing endpoint is no-auth
/// (e.g. Porkbun).
#[wasm_bindgen]
pub fn registrar_list_tld_pricing(provider_id: &str, creds_json: &str) -> js_sys::Promise {
    let provider_id = provider_id.to_string();
    let creds_json = creds_json.to_string();
    wasm_bindgen_futures::future_to_promise(async move {
        use fauna_provisioning::registrar::Registrar;
        let dispatch = fauna_provisioning::dispatch::resolve_registrar(&provider_id, &creds_json)
            .map_err(|msg| JsValue::from_str(&msg))?;
        let client = reqwest::Client::new();
        let result = dispatch
            .list_tld_pricing(&client)
            .await
            .map_err(err_to_js)?;
        let json = serde_json::to_string(&result)
            .map_err(|e| JsValue::from_str(&format!("serialize error: {e}")))?;
        Ok(JsValue::from_str(&json))
    })
}

/// Curated VPS plans for a provider, intersected with what the API
/// currently lists. `curated_ids_json` is a JSON array of the provider's
/// native server-type IDs (e.g. `["cx23","cx33",...]` for Hetzner) —
/// callers source these from `ProviderMeta::curated_offers`. Returns a
/// JSON-encoded `Vec<ServerTypeInfo>`.
#[wasm_bindgen]
pub fn vps_list_server_types(
    provider_id: &str,
    creds_json: &str,
    curated_ids_json: &str,
) -> js_sys::Promise {
    let provider_id = provider_id.to_string();
    let creds_json = creds_json.to_string();
    let curated_ids_json = curated_ids_json.to_string();
    wasm_bindgen_futures::future_to_promise(async move {
        use fauna_provisioning::vps::VpsProvider;
        let id = parse_provider_id(&provider_id)?;
        let creds = parse_credentials(&creds_json)?;
        let curated: Vec<String> = serde_json::from_str(&curated_ids_json)
            .map_err(|e| JsValue::from_str(&format!("invalid curated_ids_json: {e}")))?;
        let curated_refs: Vec<&str> = curated.iter().map(|s| s.as_str()).collect();
        let dispatch =
            fauna_provisioning::dispatch::vps_provider(id, creds, None).ok_or_else(|| {
                JsValue::from_str("provider does not support VPS capability or missing credentials")
            })?;
        let client = reqwest::Client::new();
        let result = dispatch
            .list_server_types(&client, &curated_refs)
            .await
            .map_err(err_to_js)?;
        let json = serde_json::to_string(&result)
            .map_err(|e| JsValue::from_str(&format!("serialize error: {e}")))?;
        Ok(JsValue::from_str(&json))
    })
}

// ── Onboarding machine ──────────────────────────────────────────────────

use fauna_onboarding_machine::{
    OnboardingMachine as InnerMachine, OnboardingObserver as InnerObserver,
};
use std::sync::Arc;

#[wasm_bindgen]
extern "C" {
    pub type JsOnboardingObserver;
    #[wasm_bindgen(method, js_name = onChanged)]
    fn on_changed(this: &JsOnboardingObserver);
}

struct ObserverShim(JsOnboardingObserver);
// SAFETY: wasm32 is single-threaded; the JS object never crosses a thread.
// Send + Sync are required by the Rust trait bounds but never actually
// exercised at runtime.
unsafe impl Send for ObserverShim {}
unsafe impl Sync for ObserverShim {}
impl InnerObserver for ObserverShim {
    fn on_changed(&self) {
        self.0.on_changed()
    }
}

#[wasm_bindgen]
pub struct OnboardingMachine(Arc<InnerMachine>);

/// Web's pending-provision writer: a fresh stateless view over the one
/// localStorage-backed account registry — the same construction
/// `fauna-wasm-launch` uses for its launch persistence, so both halves of the
/// flow read and write one store (`docs/goal/behavior/onboarding.md` § 6 *The
/// pending-provision slot*).
///
/// Deliberately below the struct, not above it: the `#[wasm_bindgen]` attribute
/// on the line above `pub struct OnboardingMachine` belongs to the STRUCT, and
/// anything inserted between the two silently steals it — which un-exports the
/// type and buries the real cause under a page of `RefFromWasmAbi` errors.
fn pending_provision_store() -> Arc<dyn fauna_launch_machine::PendingProvisionStore> {
    Arc::new(
        fauna_client_accounts::AccountRegistry::new(Arc::new(
            fauna_client_accounts::LocalStorageSecretStore,
        ))
        .pending_provision_store(),
    )
}

#[wasm_bindgen]
impl OnboardingMachine {
    /// Production constructor: no provider base-URL override channel exists.
    ///
    /// The JS caller (`$lib/wasm-onboarding`) passes an override argument
    /// unconditionally, but it is only ever non-`undefined` inside the
    /// `__FAUNA_E2E_AUTOMATION__` branch that a production `vite build`
    /// constant-folds away — and this flavor drops the parameter outright, so
    /// the production wasm chunk carries no way to install one at all
    /// (`testing.md` convention 15). JS ignores the extra argument.
    #[cfg(not(feature = "test-helpers"))]
    #[wasm_bindgen(constructor)]
    pub fn new(observer: JsOnboardingObserver) -> Result<OnboardingMachine, JsValue> {
        let observer: Arc<dyn InnerObserver> = Arc::new(ObserverShim(observer));
        Ok(OnboardingMachine(InnerMachine::new_with_persistence(
            observer,
            pending_provision_store(),
        )))
    }

    /// E2E constructor: accepts the provider base-URL override map.
    ///
    /// Construction-time is the channel web needs — the SPA reconstructs the
    /// wizard machine on reload, and only a construction-time map reaches
    /// `WsNestApi`'s `base_url_override` (the shared runtime setter is
    /// HTTP-only by design). Present solely in the `test-helpers` flavor that
    /// `just web-test` builds.
    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(constructor)]
    pub fn new(
        observer: JsOnboardingObserver,
        provider_base_urls: JsValue,
    ) -> Result<OnboardingMachine, JsValue> {
        let map: Option<std::collections::HashMap<String, String>> =
            if provider_base_urls.is_undefined() || provider_base_urls.is_null() {
                None
            } else {
                Some(
                    serde_wasm_bindgen::from_value(provider_base_urls).map_err(|e| {
                        JsValue::from_str(&format!("invalid provider_base_urls: {e}"))
                    })?,
                )
            };
        let observer: Arc<dyn InnerObserver> = Arc::new(ObserverShim(observer));
        let machine = InnerMachine::new_with_provider_base_urls(observer, map);
        machine.set_pending_provision_store_for_test(pending_provision_store());
        Ok(OnboardingMachine(machine))
    }

    #[wasm_bindgen(js_name = step)]
    pub fn step(&self) -> String {
        format!("{:?}", self.0.step())
    }

    #[wasm_bindgen(js_name = currentHandle)]
    pub fn current_handle(&self) -> String {
        self.0.current_handle()
    }

    /// The reach address the provisioning run captured, or `undefined`
    /// (`onboarding.md` § Reach hint). Read at the `LoggedIn` terminal, before
    /// the wizard is torn down, and persisted as the account's reach hint so the
    /// first main-app session opens connected while the domain propagates.
    #[wasm_bindgen(js_name = provisionReachIpv4)]
    pub fn provision_reach_ipv4(&self) -> Option<String> {
        self.0.provision_reach_ipv4()
    }

    /// E2E observability: returns the effective provider base-URL override
    /// for `key` ∈ {"vps","dns","nest"}, or null when none is installed.
    /// Lets fake-cloud tests assert the `?fauna_e2e_provider_base_urls`
    /// query-param channel actually reached the constructed machine.
    ///
    /// `test-helpers` only — it is automation observability with no production
    /// caller, so a production chunk must not export it (`testing.md`
    /// convention 15). Named `*ForTest` (not the shared machine method's bare
    /// `provider_base_url`, which is a real production getter — only THIS
    /// wasm-bindgen wrapper is test-only) so `wasm-seam-check`'s naming-pattern
    /// witness (`scripts/check-wasm-seam-exclusion.py::_SEAM_RE`) recognizes it
    /// as a seam automatically, per that script's own instruction: rename a
    /// non-conforming seam rather than loosen the regex.
    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = providerBaseUrlForTest)]
    pub fn provider_base_url_for_test(&self, key: String) -> Option<String> {
        self.0.provider_base_url(key)
    }

    #[wasm_bindgen(js_name = setCurrentHandle)]
    pub fn set_current_handle(&self, h: String) {
        self.0.set_current_handle(h)
    }

    /// The nest hint (`onboarding.md` § 2 Handle entry → *Nest hint*): hand
    /// the raw `nest` query value over untouched; the machine classifies it,
    /// pre-fills the handle's domain part, and drops an unclassifiable one
    /// silently (`false`).
    #[wasm_bindgen(js_name = setNestHint)]
    pub fn set_nest_hint(&self, raw: String) -> bool {
        self.0.set_nest_hint(raw)
    }

    #[wasm_bindgen(js_name = errorMessage)]
    pub fn error_message(&self) -> Option<String> {
        self.0.error_message()
    }

    #[wasm_bindgen(js_name = clearError)]
    pub fn clear_error(&self) {
        self.0.clear_error()
    }

    #[wasm_bindgen(js_name = isLoading)]
    pub fn is_loading(&self) -> bool {
        self.0.is_loading()
    }

    /// Returns DnsConfigState as a JSON string (callers parse on JS side).
    #[wasm_bindgen(js_name = dnsConfigJson)]
    pub fn dns_config_json(&self) -> String {
        serde_json::to_string(&self.0.dns_config()).unwrap_or_default()
    }

    /// Returns the current `ProviderStatus` as a JSON-serialized value.
    /// JS-side: `JSON.parse(machine.providerStatus())` yields one of:
    /// - `"NotReady"`
    /// - `"ProviderHasDomain"`
    /// - `"RegisteredElsewhere"`
    /// - `{ "UnregisteredBuyable": { "price_cents": <u64>, "currency": <iso-4217-string-or-null> } }`
    /// - `"UnregisteredNotBuyable"`
    /// (Standard serde externally-tagged enum encoding.)
    /// Source of truth: `fauna_onboarding_machine::state::ProviderStatus`.
    #[wasm_bindgen(js_name = providerStatus)]
    pub fn provider_status(&self) -> Result<String, JsValue> {
        let status = self.0.provider_status();
        serde_json::to_string(&status).map_err(err_to_js)
    }

    /// Sets the WHOIS contact for the buy-domain path. `contact_json` is a
    /// JSON-serialized `ContactInfo`. Errors on malformed JSON.
    #[wasm_bindgen(js_name = setContact)]
    pub fn set_contact(&self, contact_json: String) -> Result<(), JsValue> {
        let contact: fauna_provisioning::registrar::ContactInfo =
            serde_json::from_str(&contact_json)
                .map_err(|e| JsValue::from_str(&format!("invalid contact JSON: {e}")))?;
        self.0.set_contact(contact);
        Ok(())
    }

    #[wasm_bindgen(js_name = vpsConfigJson)]
    pub fn vps_config_json(&self) -> String {
        serde_json::to_string(&self.0.vps_config()).unwrap_or_default()
    }

    #[wasm_bindgen(js_name = visibleDnsFieldsJson)]
    pub fn visible_dns_fields_json(&self) -> String {
        serde_json::to_string(&self.0.visible_dns_fields()).unwrap_or_default()
    }

    #[wasm_bindgen(js_name = canVerifyDns)]
    pub fn can_verify_dns(&self) -> bool {
        self.0.can_verify_dns()
    }

    #[wasm_bindgen(js_name = canContinueDns)]
    pub fn can_continue_dns(&self) -> bool {
        self.0.can_continue_dns()
    }

    /// Whether the DNS-provider button for `provider_id` should be selectable
    /// given the current buy_domain / same_provider_for_vps choices. See
    /// `fauna_onboarding_machine::OnboardingMachine::dns_provider_eligible`.
    #[wasm_bindgen(js_name = dnsProviderEligible)]
    pub fn dns_provider_eligible(&self, provider_id: String) -> bool {
        self.0.dns_provider_eligible(provider_id)
    }

    /// The `LocalizedText` explaining why that button is *not* selectable —
    /// empty string when it is. A disabled control owes the user a reason
    /// (`ui/README.md` § Copy comprehensibility rule 5), and the reason belongs
    /// beside the verdict rather than re-derived per app. See
    /// `fauna_onboarding_machine::OnboardingMachine::dns_provider_ineligible_reason`.
    #[wasm_bindgen(js_name = dnsProviderIneligibleReasonJson)]
    pub fn dns_provider_ineligible_reason_json(&self, provider_id: String) -> String {
        self.0
            .dns_provider_ineligible_reason(provider_id)
            .and_then(|t| serde_json::to_string(&t).ok())
            .unwrap_or_default()
    }

    /// Whether the dns_config page should render the WHOIS contact form. See
    /// `fauna_onboarding_machine::OnboardingMachine::should_show_contact_form`.
    #[wasm_bindgen(js_name = shouldShowContactForm)]
    pub fn should_show_contact_form(&self) -> bool {
        self.0.should_show_contact_form()
    }

    /// Whether the registrar-specific notes blurb should be shown. See
    /// `fauna_onboarding_machine::OnboardingMachine::should_show_registrar_notes`.
    #[wasm_bindgen(js_name = shouldShowRegistrarNotes)]
    pub fn should_show_registrar_notes(&self) -> bool {
        self.0.should_show_registrar_notes()
    }

    /// Whether the dns_config page should show the "no supported registrar
    /// carries .{tld}" message. See
    /// `fauna_onboarding_machine::OnboardingMachine::should_show_no_provider_message`.
    #[wasm_bindgen(js_name = shouldShowNoProviderMessage)]
    pub fn should_show_no_provider_message(&self) -> bool {
        self.0.should_show_no_provider_message()
    }

    #[wasm_bindgen(js_name = canVerifyVps)]
    pub fn can_verify_vps(&self) -> bool {
        self.0.can_verify_vps()
    }

    #[wasm_bindgen(js_name = canContinueVps)]
    pub fn can_continue_vps(&self) -> bool {
        self.0.can_continue_vps()
    }

    /// The `LocalizedText` explaining why `vps-config-continue-button` is dead
    /// — empty string when it is live. A disabled control owes the user a
    /// reason (`ui/README.md` § Copy comprehensibility rule 5), and this page
    /// shipped with no explanatory surface at all. The empty-string-for-`None`
    /// encoding is this module's established convention for an optional
    /// `LocalizedText` (`dnsProviderIneligibleReasonJson` above); render the
    /// line only for a non-empty payload, never as a blank row. See
    /// `fauna_onboarding_machine::OnboardingMachine::vps_continue_blocked_reason`.
    #[wasm_bindgen(js_name = vpsContinueBlockedReasonJson)]
    pub fn vps_continue_blocked_reason_json(&self) -> String {
        self.0
            .vps_continue_blocked_reason()
            .and_then(|t| serde_json::to_string(&t).ok())
            .unwrap_or_default()
    }

    // Provisioning step (page 6) button affordances over `overall`. The rules
    // live on `fauna_provisioning::progress::OverallStatus`; these mirror the
    // native apps so web reads typed predicates instead of comparing the
    // serialized status string. See `OnboardingMachine::provisioning_in_progress`.

    /// Cancel button visible + elapsed ticker running (overall == Running).
    #[wasm_bindgen(js_name = provisioningInProgress)]
    pub fn provisioning_in_progress(&self) -> bool {
        self.0.provisioning_in_progress()
    }

    /// Retry button visible (overall == Failed or Cancelled).
    #[wasm_bindgen(js_name = canRetryProvisioning)]
    pub fn can_retry_provisioning(&self) -> bool {
        self.0.can_retry_provisioning()
    }

    /// Wizard-exit Continue button enabled (overall == Succeeded).
    #[wasm_bindgen(js_name = canContinueProvisioning)]
    pub fn can_continue_provisioning(&self) -> bool {
        self.0.can_continue_provisioning()
    }

    /// The `LocalizedText` explaining why `provisioning-continue-button` is
    /// dead — empty string when it is live. The four `○` step glyphs are a
    /// symbol, not a reason (`ui/README.md` rule 5). Same empty-string-for-
    /// `None` convention as `vpsContinueBlockedReasonJson` above. See
    /// `fauna_onboarding_machine::OnboardingMachine::provisioning_continue_blocked_reason`.
    #[wasm_bindgen(js_name = provisioningContinueBlockedReasonJson)]
    pub fn provisioning_continue_blocked_reason_json(&self) -> String {
        self.0
            .provisioning_continue_blocked_reason()
            .and_then(|t| serde_json::to_string(&t).ok())
            .unwrap_or_default()
    }

    #[wasm_bindgen(js_name = beginCreateIdentity)]
    pub fn begin_create_identity(&self) {
        self.0.begin_create_identity()
    }

    #[wasm_bindgen(js_name = beginImportIdentity)]
    pub fn begin_import_identity(&self) {
        self.0.begin_import_identity()
    }

    /// [`Self::begin_import_identity`], carrying the reason the user was *sent*
    /// there — the launch flow's `superseded` refusal
    /// (`identity-succession.md` § Propagation → *Own device fleet*).
    ///
    /// The face of the same shared transition tui and linux route through, for
    /// the same reason the Rust doc gives: the SPA mirrors `errorMessage()`
    /// reactively on every observer tick (`+page.svelte`'s `errorMessageValue`
    /// `$derived`), so a reason written to a page-local slot would be erased by
    /// the very tick this transition fires. Setting step and reason under one
    /// machine mutation is what makes the pair atomic — no render ever shows
    /// the import screen without the explanation that justifies it.
    #[wasm_bindgen(js_name = beginImportIdentityWithReason)]
    pub fn begin_import_identity_with_reason(&self, reason: String) {
        self.0.begin_import_identity_with_reason(reason)
    }

    #[wasm_bindgen(js_name = confirmGeneratedIdentity)]
    pub fn confirm_generated_identity(&self) -> Result<String, JsValue> {
        self.0.confirm_generated_identity().map_err(err_to_js)
    }

    #[wasm_bindgen(js_name = confirmImportedIdentity)]
    pub fn confirm_imported_identity(&self, secret: String) -> Result<String, JsValue> {
        self.0.confirm_imported_identity(secret).map_err(err_to_js)
    }

    #[wasm_bindgen(js_name = seedIdentity)]
    pub fn seed_identity(&self, secret: String) {
        self.0.seed_identity(secret)
    }

    // ── Total-box-loss recovery branch (box-recovery.md § Recovery UI, step 4) ──

    /// `recover-lost-box-button` on `identity_choice` — routes through
    /// `identity_import` (recovery intent), then lands on `nest_recovery`.
    #[wasm_bindgen(js_name = beginRecoverLostBox)]
    pub fn begin_recover_lost_box(&self) {
        self.0.begin_recover_lost_box()
    }

    /// `launch-recover-button` on `launch_retry` — seeds the surviving device's
    /// identity and drops straight into `nest_recovery`.
    #[wasm_bindgen(js_name = seedIdentityForRecovery)]
    pub fn seed_identity_for_recovery(&self, secret: String) {
        self.0.seed_identity_for_recovery(secret)
    }

    /// Push the fetched custodied box list (one `nest_actor_id` hex per box,
    /// from `deploymentSeeds()` / `recoveryBoxesLocal()`) into the machine for
    /// `nest_recovery` to render.
    #[wasm_bindgen(js_name = setRecoveryBoxes)]
    pub fn set_recovery_boxes(&self, boxes: Vec<String>) {
        self.0.set_recovery_boxes(boxes)
    }

    /// The custodied box list rendered on `nest_recovery` (`recover-box-item`).
    #[wasm_bindgen(js_name = recoveryBoxes)]
    pub fn recovery_boxes(&self) -> Vec<String> {
        self.0.recovery_boxes()
    }

    /// Select a custodied box on `nest_recovery` (`recover-box-item`).
    #[wasm_bindgen(js_name = selectRecoveryBox)]
    pub fn select_recovery_box(&self, nest_actor_id: String) {
        self.0.select_recovery_box(nest_actor_id)
    }

    /// `recover-method-cloud-button` — advance to `vps_config` (recovery mode).
    #[wasm_bindgen(js_name = recoverViaCloud)]
    pub fn recover_via_cloud(&self) -> Result<(), JsValue> {
        self.0.recover_via_cloud().map_err(err_to_js)
    }

    /// `recover-method-selfhosted-button` — advance to
    /// `recover_selfhosted_instructions`.
    #[wasm_bindgen(js_name = recoverViaSelfhosted)]
    pub fn recover_via_selfhosted(&self) -> Result<(), JsValue> {
        self.0.recover_via_selfhosted().map_err(err_to_js)
    }

    /// Whether the wizard is in the recovery branch (gates the entry CTAs and
    /// recovery-mode provisioning).
    #[wasm_bindgen(js_name = recoveryIntent)]
    pub fn recovery_intent(&self) -> bool {
        self.0.recovery_intent()
    }

    /// Which entry the recovery branch was reached from (`"Launch"` /
    /// `"Identity"`), or `null` outside it — the `Debug` name, mirroring `step()`.
    /// The glue routes `recover-back-button` on this (came-from-launch tears the
    /// wizard down to `launch_retry`; came-from-identity is `back()`).
    #[wasm_bindgen(js_name = recoveryCameFrom)]
    pub fn recovery_came_from(&self) -> Option<String> {
        self.0.recovery_came_from().map(|e| format!("{e:?}"))
    }

    /// The selected box's `nest_actor_id` (hex) on `nest_recovery`, or `null`.
    #[wasm_bindgen(js_name = recoverySelectedNestId)]
    pub fn recovery_selected_nest_id(&self) -> Option<String> {
        self.0.recovery_selected_nest_id()
    }

    #[wasm_bindgen(js_name = generatedSecret)]
    pub fn generated_secret(&self) -> Option<String> {
        self.0.generated_secret()
    }

    #[wasm_bindgen(js_name = effectiveSecret)]
    pub fn effective_secret(&self) -> Option<String> {
        self.0.effective_secret()
    }

    // Async methods — wrap each as `js_sys::Promise`.

    #[wasm_bindgen(js_name = verifyDns)]
    pub fn verify_dns(&self) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            m.verify_dns().await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    #[wasm_bindgen(js_name = continueFromDns)]
    pub fn continue_from_dns(&self) -> Result<(), JsValue> {
        self.0.continue_from_dns().map_err(err_to_js)
    }

    #[wasm_bindgen(js_name = verifyVps)]
    pub fn verify_vps(&self) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            m.verify_vps().await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Forward transition from `vps_config` → `nest_provisioning`. Async
    /// to mirror the UniFFI surface (the underlying Rust method is async
    /// even though its current body has no await — keeps native and web
    /// apps in lockstep). Errors when `can_continue_vps()` is false.
    #[wasm_bindgen(js_name = continueFromVps)]
    pub fn continue_from_vps(&self) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            m.continue_from_vps().await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Returns a JSON-encoded `ProvisioningSnapshot`. Read on every
    /// observer tick to render the nest_provisioning page; mutations
    /// happen on the orchestrator side.
    #[wasm_bindgen(js_name = provisioningSnapshot)]
    pub fn provisioning_snapshot(&self) -> String {
        serde_json::to_string(&self.0.provisioning_snapshot()).unwrap_or_default()
    }

    /// Returns a JSON-encoded `Vec<BillOfMaterialsItem>` — the
    /// `nest_provisioning` page's top-region price summary (up to two
    /// items: domain, then VPS). Empty array when nothing is chargeable
    /// yet. Source of truth: `fauna_onboarding_machine::state::BillOfMaterialsItem`.
    #[wasm_bindgen(js_name = billOfMaterials)]
    pub fn bill_of_materials(&self) -> String {
        serde_json::to_string(&self.0.bill_of_materials()).unwrap_or_default()
    }

    /// Sets the cancel flag on the running provisioning task. The task
    /// observes it at the next step boundary or retry iteration.
    #[wasm_bindgen(js_name = cancelProvisioning)]
    pub fn cancel_provisioning(&self) {
        self.0.cancel_provisioning();
    }

    /// Re-runs provisioning from the top. Idempotency makes already-done
    /// steps short-circuit on re-run.
    #[wasm_bindgen(js_name = retryProvisioning)]
    pub fn retry_provisioning(&self) {
        Arc::clone(&self.0).retry_provisioning();
    }

    // Sync mutations.

    #[wasm_bindgen(js_name = toggleBuyDomain)]
    pub fn toggle_buy_domain(&self, on: bool) {
        self.0.toggle_buy_domain(on)
    }

    #[wasm_bindgen(js_name = toggleSameProviderForVps)]
    pub fn toggle_same_provider_for_vps(&self, on: bool) {
        self.0.toggle_same_provider_for_vps(on)
    }

    #[wasm_bindgen(js_name = selectDnsProvider)]
    pub fn select_dns_provider(&self, id: String) {
        self.0.select_dns_provider(id)
    }

    #[wasm_bindgen(js_name = setDnsCred)]
    pub fn set_dns_cred(&self, field_id: String, value: String) {
        self.0.set_dns_cred(field_id, value)
    }

    #[wasm_bindgen(js_name = dnsSetUpLater)]
    pub fn dns_set_up_later(&self) {
        self.0.dns_set_up_later()
    }

    #[wasm_bindgen(js_name = selectVpsProvider)]
    pub fn select_vps_provider(&self, id: String) {
        self.0.select_vps_provider(id)
    }

    #[wasm_bindgen(js_name = setVpsCred)]
    pub fn set_vps_cred(&self, field_id: String, value: String) {
        self.0.set_vps_cred(field_id, value)
    }

    #[wasm_bindgen(js_name = selectVpsServerType)]
    pub fn select_vps_server_type(&self, id: String) {
        self.0.select_vps_server_type(id)
    }

    /// `vps-config-mail-mode-toggle` wiring. Records whether the box provisions
    /// the mail subsystem (mail box vs. social-only box) — drives
    /// `CloudInitParams::enable_mail` at provisioning time. The WASM twin of the
    /// UniFFI `set_provision_mail_mode`. Per `docs/goal/behavior/onboarding.md` §5.
    #[wasm_bindgen(js_name = setProvisionMailMode)]
    pub fn set_provision_mail_mode(&self, enabled: bool) {
        self.0.set_provision_mail_mode(enabled);
    }

    /// Whether the `vps-config-mail-mode-toggle` is ON — the user's explicit
    /// choice if set, else the handle's real-domain default. Drives the toggle's
    /// checked state and the `mem_gb ≥ 2` server-type gate (see
    /// `serverTypeAllowedForMail`). The WASM twin of the UniFFI
    /// `provision_mail_mode_enabled`. Per `docs/goal/behavior/onboarding.md` §5.
    #[wasm_bindgen(js_name = provisionMailModeEnabled)]
    pub fn provision_mail_mode_enabled(&self) -> bool {
        self.0.provision_mail_mode_enabled()
    }

    /// `vps-config-update-channel-row[<channel>]` wiring: which builds the
    /// box's updater follows (`stable` / `test` / `dev`). An id that names no
    /// channel is ignored. The WASM twin of the UniFFI
    /// `set_provision_update_channel`, taking the row's id rather than an enum
    /// so the page passes the key it rendered. Per
    /// `docs/goal/behavior/onboarding-provisioning.md` §5.
    #[wasm_bindgen(js_name = setProvisionUpdateChannel)]
    pub fn set_provision_update_channel(&self, channel_id: String) {
        if let Some(c) = fauna_provisioning::cloud_init::UpdateChannel::from_id(&channel_id) {
            self.0.set_provision_update_channel(c);
        }
    }

    /// The selected update channel's id — the row to mark selected. The WASM
    /// twin of the UniFFI `provision_update_channel`.
    #[wasm_bindgen(js_name = provisionUpdateChannel)]
    pub fn provision_update_channel(&self) -> String {
        self.0.provision_update_channel().id().to_string()
    }

    #[wasm_bindgen(js_name = selectVpsLocation)]
    pub fn select_vps_location(&self, id: String) {
        self.0.select_vps_location(id)
    }

    #[wasm_bindgen(js_name = confirmPrice)]
    pub fn confirm_price(&self) {
        self.0.confirm_price()
    }

    // ── hosted-auth (a bundled provider's hosted sign-in, onboarding.md § 4) ──
    // `form` is `"dns"` | `"vps"` — which credential form the field lives on.

    /// Step 1: returns a JSON `HostedAuthPrompt` (`verification_url` to open,
    /// `user_code` to show). Web opens the URL itself — `window.open` must run
    /// inside the click handler's task to survive popup blockers.
    #[wasm_bindgen(js_name = hostedAuthBegin)]
    pub fn hosted_auth_begin(&self, form: String, field_id: String) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            let form = parse_credential_form(&form)?;
            let prompt = m
                .hosted_auth_begin(form, field_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_str(
                &serde_json::to_string(&prompt).unwrap_or_default(),
            ))
        })
    }

    /// Step 2: resolves once the token has landed (`hostedAuthStateJson` →
    /// `"Connected"`) or rejects when the attempt ended.
    #[wasm_bindgen(js_name = hostedAuthWait)]
    pub fn hosted_auth_wait(&self, form: String, field_id: String) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            let form = parse_credential_form(&form)?;
            m.hosted_auth_wait(form, field_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// JSON `HostedAuthState` — the field's button label source.
    #[wasm_bindgen(js_name = hostedAuthStateJson)]
    pub fn hosted_auth_state_json(
        &self,
        form: String,
        field_id: String,
    ) -> Result<String, JsValue> {
        let form = parse_credential_form(&form)?;
        Ok(serde_json::to_string(&self.0.hosted_auth_state(form, field_id)).unwrap_or_default())
    }

    #[wasm_bindgen(js_name = hostedAuthCanBegin)]
    pub fn hosted_auth_can_begin(&self, form: String, field_id: String) -> Result<bool, JsValue> {
        let form = parse_credential_form(&form)?;
        Ok(self.0.hosted_auth_can_begin(form, field_id))
    }

    #[wasm_bindgen(js_name = back)]
    pub fn back(&self) {
        self.0.back()
    }

    #[wasm_bindgen(js_name = setNestUrl)]
    pub fn set_nest_url(&self, url: String) {
        self.0.set_nest_url(url)
    }

    // Snapshot getters consumed by the web wizard's per-stage views.
    #[wasm_bindgen(js_name = nestUrl)]
    pub fn nest_url(&self) -> String {
        self.0.nest_url()
    }

    #[wasm_bindgen(js_name = localNestReachable)]
    pub fn local_nest_reachable(&self) -> bool {
        self.0.local_nest_reachable()
    }

    #[wasm_bindgen(js_name = domainStatus)]
    pub fn domain_status(&self) -> Option<String> {
        self.0.domain_status().map(|s| format!("{s:?}"))
    }

    #[wasm_bindgen(js_name = reset)]
    pub fn reset(&self) {
        self.0.reset()
    }

    // ── HandleCheck (handle-first wizard) ──

    #[wasm_bindgen(js_name = startHandleCheck)]
    pub fn start_handle_check(&self, handle: String) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            m.start_handle_check(handle).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    #[wasm_bindgen(js_name = cancelHandleCheck)]
    pub fn cancel_handle_check(&self) {
        self.0.cancel_handle_check()
    }

    #[wasm_bindgen(js_name = handleCheckSnapshotJson)]
    pub fn handle_check_snapshot_json(&self) -> String {
        serde_json::to_string(&self.0.handle_check_snapshot()).unwrap_or_default()
    }

    #[wasm_bindgen(js_name = setControlCheckbox)]
    pub fn set_control_checkbox(&self, checked: bool) {
        self.0.set_control_checkbox(checked)
    }

    /// Per the target-state doc — this is the Continue button's wiring on
    /// the handle_entry page. Returns the next OnboardingStep as a string
    /// (Rust Debug name, e.g. `"DnsConfig"` / `"InviteRequest"` / `"Done"`).
    /// On `Done` the caller queries `wizardOutcomeJson()` and routes per the
    /// outcome variant.
    #[wasm_bindgen(js_name = submitHandleCheckContinue)]
    pub fn submit_handle_check_continue(&self) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            let step = m.submit_handle_check_continue().await;
            Ok(JsValue::from_str(&format!("{step:?}")))
        })
    }

    // ── InviteRequest (new snapshot-driven flow) ──

    /// invite-request-submit-button wiring. No message arg in the new
    /// flow — the wizard owns the request body. Returns the next
    /// OnboardingStep as its Debug name.
    #[wasm_bindgen(js_name = wizardSubmitInviteRequest)]
    pub fn wizard_submit_invite_request(&self) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            let step = m.wizard_submit_invite_request().await;
            Ok(JsValue::from_str(&format!("{step:?}")))
        })
    }

    #[wasm_bindgen(js_name = inviteRequestSnapshotJson)]
    pub fn invite_request_snapshot_json(&self) -> String {
        serde_json::to_string(&self.0.invite_request_snapshot()).unwrap_or_default()
    }

    #[wasm_bindgen(js_name = recheckInviteStatus)]
    pub fn recheck_invite_status(&self) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            let step = m.recheck_invite_status().await;
            Ok(JsValue::from_str(&format!("{step:?}")))
        })
    }

    #[wasm_bindgen(js_name = verifyOobInviteCode)]
    pub fn verify_oob_invite_code(&self, code: String) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            m.verify_oob_invite_code(code).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    #[wasm_bindgen(js_name = redeemInvite)]
    pub fn redeem_invite(&self) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            let step = m.redeem_invite().await;
            Ok(JsValue::from_str(&format!("{step:?}")))
        })
    }

    /// The pending-invite resume slot, or `undefined` when the wizard is not in
    /// `PendingReview`.
    ///
    /// Replaced `submitInviteRequestContinue` (retired 2026-08-12): the
    /// pending-review journey no longer exits the wizard, so web reads this at
    /// the `wizardSubmitInviteRequest()` return and writes the slot there —
    /// "the only write moment" (`onboarding.md` § 3 Persistence callouts).
    #[wasm_bindgen(js_name = pendingInviteSlot)]
    pub fn pending_invite_slot(&self) -> JsValue {
        match self.0.pending_invite_slot() {
            Some(slot) => serde_wasm_bindgen::to_value(&slot).unwrap_or(JsValue::UNDEFINED),
            None => JsValue::UNDEFINED,
        }
    }

    #[wasm_bindgen(js_name = cancelInviteOp)]
    pub fn cancel_invite_op(&self) {
        self.0.cancel_invite_op()
    }

    #[wasm_bindgen(js_name = seedPendingInvite)]
    pub fn seed_pending_invite(
        &self,
        nest_url: String,
        handle: String,
        request_id: String,
        status_json: String,
    ) {
        self.0
            .seed_pending_invite(nest_url, handle, request_id, status_json)
    }

    /// Lands the wizard on `InviteRequest` with `nest_url` + `handle`
    /// pre-set, snapshot reset to `Idle`. Used by the silent-challenge
    /// "secret unregistered" fallback per target §"App-launch routing"
    /// — saves the user one extra click compared to dropping them at
    /// `handle_entry`. Distinct from `seedPendingInvite` (which restores
    /// a previously submitted request).
    #[wasm_bindgen(js_name = navigateToInviteRequestForKnownNest)]
    pub fn navigate_to_invite_request_for_known_nest(&self, nest_url: String, handle: String) {
        self.0
            .navigate_to_invite_request_for_known_nest(nest_url, handle);
    }

    /// Pre-identity probe over the anonymous WS-RPC connection used by
    /// **app-launch routing** to discriminate claimed (→ invite_request)
    /// vs unclaimed (→ claim_code) nests when silent-challenge reports
    /// the secret isn't registered. Resolves to a JSON `{claimed, mode?}`
    /// string; rejects on any transport / decode failure so the launch
    /// orchestrator can apply the safer-default fallback (assume claimed)
    /// at exactly one site. Replaces the legacy `GET /api/v1/setup-status`
    /// HTTP probe.
    #[wasm_bindgen(js_name = probeSetupStatus)]
    pub fn probe_setup_status(&self, nest_url: String) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            match m.probe_setup_status_at(nest_url).await {
                Ok(status) => Ok(JsValue::from_str(
                    &serde_json::to_string(&status).unwrap_or_default(),
                )),
                Err(e) => Err(JsValue::from_str(&format!("{e:?}"))),
            }
        })
    }

    /// Lands the wizard on `ClaimCode` with `nest_url` + `handle` pre-set,
    /// snapshot reset to `Idle`. Used by the silent-challenge "secret
    /// unregistered + setup-status.claimed=false" fallback per target
    /// `docs/goal/behavior/onboarding.md` § App-launch routing — silent-challenge
    /// fallback table (unclaimed-nest row).
    #[wasm_bindgen(js_name = navigateToClaimCodeForKnownNest)]
    pub fn navigate_to_claim_code_for_known_nest(&self, nest_url: String, handle: String) {
        self.0
            .navigate_to_claim_code_for_known_nest(nest_url, handle);
    }

    /// Factory-reset re-onboard variant: lands on `ClaimCode` with `nest_url` +
    /// `handle` pre-set AND the returned claim `code` stashed for pre-fill
    /// (`claimCodePrefill()`). `fauna.admin.factory_reset` returns the new code
    /// to the client and the human never sees it, so without pre-fill the admin
    /// would be stranded on the claim-code page. Per
    /// `docs/goal/behavior/mail-bridge-lifecycle.md` § Factory reset (Client
    /// affordance) + `onboarding.md` §3a (prefill note).
    #[wasm_bindgen(js_name = navigateToClaimCodeForKnownNestWithCode)]
    pub fn navigate_to_claim_code_for_known_nest_with_code(
        &self,
        nest_url: String,
        handle: String,
        code: String,
    ) {
        self.0
            .navigate_to_claim_code_for_known_nest_with_code(nest_url, handle, code);
    }

    /// The claim code stashed by the factory-reset re-onboard path
    /// (`navigateToClaimCodeForKnownNestWithCode`), or `undefined`. The
    /// claim-code page pre-fills `claim-code-input` from this when non-empty.
    #[wasm_bindgen(js_name = claimCodePrefill)]
    pub fn claim_code_prefill(&self) -> Option<String> {
        self.0.claim_code_prefill()
    }

    // ── ClaimCode (unclaimed-nest branch) ──

    /// Wire-up for `claim-code-submit-button`. Returns the next
    /// `OnboardingStep` as its Debug name. Per
    /// `docs/goal/behavior/onboarding.md` §3a.
    #[wasm_bindgen(js_name = wizardSubmitClaimCode)]
    pub fn wizard_submit_claim_code(&self, code: String) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            let step = m.wizard_submit_claim_code(code).await;
            Ok(JsValue::from_str(&format!("{step:?}")))
        })
    }

    #[wasm_bindgen(js_name = claimCodeSnapshotJson)]
    pub fn claim_code_snapshot_json(&self) -> String {
        serde_json::to_string(&self.0.claim_code_snapshot()).unwrap_or_default()
    }

    // ── Claim-time serving enablement (machine-derived; no checkboxes) ──
    //
    // The `encryption_mode_choice` / `plaintext_mode_consent` pages are retired
    // (no-modes — `docs/goal/architecture/nest/storage-modes.md`). What survives
    // is the serving enablement the first of those pages hosted, and it survives
    // WITHOUT wizard UI (`docs/goal/behavior/onboarding.md` § 3b): the four
    // intents are derived from the handle's locality, so only the getters remain
    // — there is nothing for the client to set.

    /// Read post-onboarding by the authed launch glue: if true, fire
    /// `set_mail_enabled(true)` once the admin session exists (idempotent with
    /// the mail-settings enable path). Per `docs/goal/behavior/onboarding.md` §3b.
    #[wasm_bindgen(js_name = emailEnableRequested)]
    pub fn email_enable_requested(&self) -> bool {
        self.0.email_enable_requested()
    }

    /// Read post-onboarding by the authed launch glue: if true, call
    /// `MailAdminClient::set_caldav_enabled(true)` once the admin session exists.
    /// Mirrors `emailEnableRequested`.
    #[wasm_bindgen(js_name = caldavEnableRequested)]
    pub fn caldav_enable_requested(&self) -> bool {
        self.0.caldav_enable_requested()
    }

    /// Read post-onboarding by the authed launch glue: if true, call
    /// `MailAdminClient::set_carddav_enabled(true)` once the admin session
    /// exists (and, when neither email nor CalDAV minted the shared MSEK,
    /// provision the CardDAV-only mailbox). Mirrors `caldavEnableRequested`.
    #[wasm_bindgen(js_name = carddavEnableRequested)]
    pub fn carddav_enable_requested(&self) -> bool {
        self.0.carddav_enable_requested()
    }

    /// Read post-onboarding by the authed launch glue: if true, call
    /// `MailAdminClient::set_webdav_enabled(true)` once the admin session
    /// exists. Mirrors `carddavEnableRequested`.
    #[wasm_bindgen(js_name = webdavEnableRequested)]
    pub fn webdav_enable_requested(&self) -> bool {
        self.0.webdav_enable_requested()
    }

    // ── NatModeChoice (the admin-claim path's terminal step) ──
    //
    // Per `docs/goal/behavior/onboarding.md` § 3b-bis. Reached after the claim
    // completes; both `submitNatModeChoice` and `deferNatModeChoice` exit the
    // wizard at `Done` with a `LoggedIn` outcome (the seed is a working
    // default, so deferring sends nothing).

    #[wasm_bindgen(js_name = natModeSnapshotJson)]
    pub fn nat_mode_snapshot_json(&self) -> String {
        serde_json::to_string(&self.0.nat_mode_snapshot()).unwrap_or_default()
    }

    /// `public-nat-mode-radio` / `private-nat-mode-radio` wiring. `mode` is the
    /// lowercase wire form (`"public"` / `"private"`) — the same repr the
    /// snapshot JSON carries, so a read and a write round-trip symmetrically.
    #[wasm_bindgen(js_name = selectNatMode)]
    pub fn select_nat_mode(&self, mode: String) -> Result<(), JsValue> {
        use fauna_onboarding_machine::NodeMode;
        let m = NodeMode::from_wire_str(&mode)
            .ok_or_else(|| JsValue::from_str(&format!("unknown NodeMode: {mode}")))?;
        self.0.select_nat_mode(m);
        Ok(())
    }

    /// `nat-mode-confirm-button` wiring. Sends `fauna.setup.nat_mode` to the
    /// nest. Returns the next `OnboardingStep` as its Debug name: `"Done"` on
    /// success — query `wizardOutcomeJson()` (`LoggedIn`); `"NatModeChoice"`
    /// while the user stays on the page after an error.
    #[wasm_bindgen(js_name = submitNatModeChoice)]
    pub fn submit_nat_mode_choice(&self) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            let step = m.submit_nat_mode_choice().await;
            Ok(JsValue::from_str(&format!("{step:?}")))
        })
    }

    /// `nat-mode-defer-button` wiring. Sends nothing (the seeded mode already
    /// works); returns `"Done"` with a `LoggedIn` outcome. The admin can flip
    /// the axis later from Admin → Nest.
    #[wasm_bindgen(js_name = deferNatModeChoice)]
    pub fn defer_nat_mode_choice(&self) -> String {
        format!("{:?}", self.0.defer_nat_mode_choice())
    }

    /// App capability declaration: web has the `trust_prompt` onboarding
    /// screen built (`onboarding.md` § 3b-ter). Call once at construction —
    /// see `fauna_onboarding_machine::OnboardingMachine::set_renders_trust_prompt`.
    #[wasm_bindgen(js_name = setRendersTrustPrompt)]
    pub fn set_renders_trust_prompt(&self, renders: bool) {
        self.0.set_renders_trust_prompt(renders)
    }

    /// `trust-box-grant-button` wiring. Latches the answer for the signed-in
    /// handoff (`takeTrustPromptGranted`) and concludes the wizard. Returns
    /// the next `OnboardingStep` Debug name — always `"Done"`.
    #[wasm_bindgen(js_name = grantDefaultTrust)]
    pub fn grant_default_trust(&self) -> String {
        format!("{:?}", self.0.grant_default_trust())
    }

    /// `trust-box-skip-button` wiring. Latches nothing and concludes the
    /// wizard identically to a grant. Returns the next `OnboardingStep`
    /// Debug name — always `"Done"`.
    #[wasm_bindgen(js_name = skipTrustPrompt)]
    pub fn skip_trust_prompt(&self) -> String {
        format!("{:?}", self.0.skip_trust_prompt())
    }

    /// Consume-once read of the trust_prompt answer at the signed-in
    /// handoff — the only point web holds an authenticated session and can
    /// dispatch `LinkedNestsAction::MintDefaultSet`.
    #[wasm_bindgen(js_name = takeTrustPromptGranted)]
    pub fn take_trust_prompt_granted(&self) -> bool {
        self.0.take_trust_prompt_granted()
    }

    // ── Recovery kit + phrase restore (`onboarding.md` § 1 Identity) ──

    /// App capability declaration: web renders the `recovery_kit` offer —
    /// see `OnboardingMachine::set_renders_recovery_kit`. Call once at
    /// construction, beside `setRendersTrustPrompt`.
    #[wasm_bindgen(js_name = setRendersRecoveryKit)]
    pub fn set_renders_recovery_kit(&self, renders: bool) {
        self.0.set_renders_recovery_kit(renders)
    }

    /// The minted-but-unregistered kit root `recovery-kit-secret-display`
    /// shows (bare 64-hex); `undefined` outside the screen's lifetime.
    #[wasm_bindgen(js_name = recoveryKitSecretHex)]
    pub fn recovery_kit_secret_hex(&self) -> Option<String> {
        self.0.recovery_kit_secret_hex()
    }

    /// The one `fauna://recovery` URI behind `recovery-kit-qr` AND
    /// `recovery-kit-secret-copy-btn` — `OnboardingMachine::recovery_kit_uri`.
    #[wasm_bindgen(js_name = recoveryKitUri)]
    pub fn recovery_kit_uri(&self) -> Option<String> {
        self.0.recovery_kit_uri()
    }

    /// `recovery-kit-confirm-button`: advances to `HandleEntry`, keeping the
    /// root for the signed-in handoff (`takePendingRecoverySecret`).
    #[wasm_bindgen(js_name = confirmRecoveryKit)]
    pub fn confirm_recovery_kit(&self) {
        self.0.confirm_recovery_kit()
    }

    /// `recovery-kit-skip-button`: drops the root so nothing registers.
    #[wasm_bindgen(js_name = skipRecoveryKit)]
    pub fn skip_recovery_kit(&self) {
        self.0.skip_recovery_kit()
    }

    /// Consume-once read of the confirmed kit root at the signed-in handoff —
    /// the one point custody permits its registration. `undefined` if skipped.
    #[wasm_bindgen(js_name = takePendingRecoverySecret)]
    pub fn take_pending_recovery_secret(&self) -> Option<String> {
        self.0
            .take_pending_recovery_secret()
            .map(|s| s.as_str().to_owned())
    }

    /// `restore-from-recovery-kit-button` on `identity_choice`.
    #[wasm_bindgen(js_name = beginRecoveryEntry)]
    pub fn begin_recovery_entry(&self) {
        self.0.begin_recovery_entry()
    }

    /// `recovery-entry-submit-button` — resolves to JSON
    /// `{"restored": bool, "superseded": bool, "message": LocalizedText|null}`:
    /// `restored` when the seed is back (commit it like an import),
    /// `superseded` when the client must route to
    /// `beginImportIdentityWithReason`, and `message` the shared
    /// `RecoveryEntryOutcome::message` to render on `error-message` (it can
    /// ride a successful restore — the predecessors-lost arm).
    #[wasm_bindgen(js_name = submitRecoveryEntry)]
    pub fn submit_recovery_entry(&self, phrase: String) -> js_sys::Promise {
        use fauna_onboarding_machine::RecoveryEntryOutcome as O;
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            let outcome = m.submit_recovery_entry(phrase).await;
            let json = serde_json::json!({
                "restored": matches!(outcome, O::Restored | O::RestoredPredecessorsLost { .. }),
                "superseded": outcome == O::Superseded,
                "message": outcome.message(),
            });
            Ok(JsValue::from_str(&json.to_string()))
        })
    }

    /// JSON `[{actor_id_hex, seed_hex}]` of the predecessor seeds a phrase
    /// restore recovered — empty on every ordinary onboarding. Read at the
    /// signed-in handoff and handed to the account registry's
    /// `persistRestoredPredecessors`.
    #[wasm_bindgen(js_name = restoredPredecessorsJson)]
    pub fn restored_predecessors_json(&self) -> String {
        serde_json::to_string(&self.0.restored_predecessors()).unwrap_or_else(|_| "[]".into())
    }

    // ── Wizard outcome + auxiliary snapshot getters ──

    /// Returns `null` when the wizard hasn't terminated; otherwise a JSON-
    /// serialized `WizardOutcome` (e.g. `{"LoggedIn":{...}}` or
    /// `{"AwaitingManualDns":{...}}`).
    #[wasm_bindgen(js_name = wizardOutcomeJson)]
    pub fn wizard_outcome_json(&self) -> Option<String> {
        self.0
            .wizard_outcome()
            .map(|o| serde_json::to_string(&o).unwrap_or_default())
    }

    /// Returns `null` unless a DNS-provider credential was verified at the
    /// onboarding DNS step (and not the "set up later" manual path); otherwise
    /// a JSON-serialized `CapturedDnsCredential` (`{provider_id, fields,
    /// label}`). The onboarding→launch hand-off channel: the launched web
    /// app reads this at `LoggedIn` and seals it via the post-onboarding
    /// `DnsManagementMachine::PutCredentials` path — one store, one writer
    /// (`docs/goal/behavior/dns-management.md` § Where the credential lives).
    /// The native binding exposes the same via `captured_dns_credential()`.
    #[wasm_bindgen(js_name = capturedDnsCredentialJson)]
    pub fn captured_dns_credential_json(&self) -> Option<String> {
        self.0
            .captured_dns_credential()
            .map(|c| serde_json::to_string(&c).unwrap_or_default())
    }

    #[wasm_bindgen(js_name = identityOriginJson)]
    pub fn identity_origin_json(&self) -> String {
        serde_json::to_string(&self.0.identity_origin()).unwrap_or_default()
    }

    #[wasm_bindgen(js_name = visibleVpsFieldsJson)]
    pub fn visible_vps_fields_json(&self) -> String {
        serde_json::to_string(&self.0.visible_vps_fields()).unwrap_or_default()
    }

    #[wasm_bindgen(js_name = dnsStatusTextKeyJson)]
    pub fn dns_status_text_key_json(&self) -> String {
        serde_json::to_string(&self.0.dns_status_text_key()).unwrap_or_default()
    }

    // ── Provisioning: start / continue ──

    /// Fire-and-forget: spawns the provisioning task in the background.
    /// Callers poll `provisioningSnapshot()` on each observer tick.
    #[wasm_bindgen(js_name = startProvisioning)]
    pub fn start_provisioning(&self) {
        Arc::clone(&self.0).start_provisioning();
    }

    /// Called when the user presses Continue on the `nest_provisioning` page
    /// after provisioning completes. Returns the next `OnboardingStep`'s bare
    /// name (e.g. `DnsPostInstructions` or `Done`) — the same shape the async
    /// submit methods resolve with, because every caller routes through
    /// `handleWizardExit(stepName)`, which compares against the bare `'Done'`.
    /// A JSON-encoded step (`'"Done"'`, quotes included) silently fails that
    /// comparison and the wizard-exit persistence never runs.
    #[wasm_bindgen(js_name = continueFromProvisioning)]
    pub fn continue_from_provisioning(&self) -> String {
        let step = self.0.continue_from_provisioning();
        format!("{step:?}")
    }

    /// Called when the user presses Continue on the `dns_post_instructions`
    /// page. Returns the next `OnboardingStep`'s bare name (see
    /// `continueFromProvisioning` for why it must not be JSON-encoded).
    #[wasm_bindgen(js_name = continueFromDnsPostInstructions)]
    pub fn continue_from_dns_post_instructions(&self) -> String {
        let step = self.0.continue_from_dns_post_instructions();
        format!("{step:?}")
    }

    /// Returns the current list of DNS records to configure as a JSON
    /// string (`Vec<DnsRecordPlain>`). Used to populate the
    /// `dns_post_instructions` page.
    #[wasm_bindgen(js_name = dnsRecordsJson)]
    pub fn dns_records_json(&self) -> String {
        serde_json::to_string(&self.0.dns_records()).unwrap_or_default()
    }

    /// Markdown the `dns_post_instructions` page renders: the captured DNS
    /// records plus the post-provisioning details. `undefined` until the
    /// deferred-DNS run has populated both the records and a successful
    /// provisioning result. Mirrors the UniFFI `dns_post_instructions()`
    /// the native apps call.
    #[wasm_bindgen(js_name = dnsPostInstructions)]
    pub fn dns_post_instructions(&self) -> Option<String> {
        self.0.dns_post_instructions()
    }

    // ── AwaitingManualDns ("Almost ready") surface ──
    //
    // Reached after the deferred-DNS exit (`continueFromDnsPostInstructions`
    // → `Done` with `wizardOutcome == AwaitingManualDns`). The web app
    // saves the slot (the registry's per-actor awaiting-DNS record —
    // `$lib/onboarding/awaiting-dns-store`), renders the surface from
    // `awaitingManualDnsSnapshotJson`, and polls `recheckManualDns` until the
    // freshly-provisioned nest comes online and can be claimed. Every probe
    // here is pre-claim HTTP — there is no WS-RPC until the claim succeeds.
    // Per `docs/goal/behavior/onboarding.md` § "Wizard exit handling".

    /// App-launch hydration: lands the wizard at the AwaitingManualDns exit
    /// from a saved slot. The client calls `seedIdentity(secret)` first.
    /// `dns_records_json` is a JSON-serialized `Vec<DnsRecordPlain>` (the
    /// awaiting-DNS record's `dns_records_json` field).
    #[wasm_bindgen(js_name = seedAwaitingManualDns)]
    pub fn seed_awaiting_manual_dns(
        &self,
        nest_url: String,
        handle: String,
        dns_records_json: String,
        claim_code: String,
    ) {
        let dns_records: Vec<fauna_onboarding_machine::DnsRecordPlain> =
            serde_json::from_str(&dns_records_json).unwrap_or_default();
        self.0
            .seed_awaiting_manual_dns(nest_url, handle, dns_records, claim_code);
    }

    /// JSON-serialized `AwaitingManualDnsSnapshot` for the "Almost ready"
    /// surface (records to display + current state).
    #[wasm_bindgen(js_name = awaitingManualDnsSnapshotJson)]
    pub fn awaiting_manual_dns_snapshot_json(&self) -> String {
        serde_json::to_string(&self.0.awaiting_manual_dns_snapshot()).unwrap_or_default()
    }

    /// The `dns_records_json` field of the awaiting-DNS slot, ready to store.
    ///
    /// Use this instead of `JSON.stringify`-ing the snapshot's records: the
    /// seeder parses this back with serde (`record_type`, …), while the
    /// snapshot JSON above is the *binding's* shape. See
    /// `OnboardingMachine::awaiting_dns_records_json` for the full rationale.
    #[wasm_bindgen(js_name = awaitingDnsRecordsJson)]
    pub fn awaiting_dns_records_json(&self) -> String {
        self.0.awaiting_dns_records_json()
    }

    /// The records the user must add, formatted one per line — render this into
    /// `awaiting-dns-records` and copy *this* on `awaiting-dns-copy-button`, so
    /// web says exactly what the other apps say.
    #[wasm_bindgen(js_name = awaitingDnsRecordsText)]
    pub fn awaiting_dns_records_text(&self) -> String {
        self.0.awaiting_dns_records_text()
    }

    /// Whether `awaiting-dns-copy-button` has anything to copy — bind the
    /// button's `disabled` to its negation. False in the records-less mode (a
    /// resumed standard-path run), where the text above is empty and a click
    /// would copy nothing. Asked rather than derived from the record list, so
    /// web cannot disagree with the other six about which mode the page is in.
    #[wasm_bindgen(js_name = awaitingDnsCopyEnabled)]
    pub fn awaiting_dns_copy_enabled(&self) -> bool {
        self.0.awaiting_dns_copy_enabled()
    }

    /// Relaunch hydration from the stored slot, taking its opaque
    /// `dns_records_json` verbatim (see the Rust method's docs for why web must
    /// not `JSON.parse` + re-shape it itself).
    #[wasm_bindgen(js_name = seedAwaitingManualDnsJson)]
    pub fn seed_awaiting_manual_dns_json(
        &self,
        nest_url: String,
        handle: String,
        dns_records_json: String,
        claim_code: String,
    ) {
        self.0
            .seed_awaiting_manual_dns_json(nest_url, handle, dns_records_json, claim_code);
    }

    /// Relaunch hydration from the stored slot as ONE opaque JSON record — the
    /// `AwaitingDnsRecord` the registry returned (`registryLoadAwaitingDns`),
    /// verbatim. The door the page should use: every field the slot carries
    /// reaches the machine — including the box's built-with identity, which
    /// the machine re-holds as the first-contact root before the surface's
    /// first poll, and its reach address — with nothing re-shaped in JS (see
    /// `seedAwaitingManualDnsJson` for why that matters). Returns `false`
    /// when the JSON is not a record at all, in which case nothing is seeded;
    /// a record missing the newer optional fields still seeds.
    #[wasm_bindgen(js_name = seedAwaitingManualDnsRecordJson)]
    pub fn seed_awaiting_manual_dns_record_json(&self, record_json: String) -> bool {
        match serde_json::from_str::<fauna_launch_machine::AwaitingDnsRecord>(&record_json) {
            Ok(record) => {
                self.0.seed_awaiting_manual_dns_record(record);
                true
            }
            Err(_) => false,
        }
    }

    /// Single-shot poll of the provisioned nest. Resolves to the next
    /// `OnboardingStep` as its Debug name (`"Done"` while still awaiting;
    /// `"NatModeChoice"` once claimed).
    #[wasm_bindgen(js_name = recheckManualDns)]
    pub fn recheck_manual_dns(&self) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            let step = m.recheck_manual_dns().await;
            Ok(JsValue::from_str(&format!("{step:?}")))
        })
    }

    // ── E2E bridge: test-only snapshot setters ──
    //
    // Per the onboarding client target-state design (tracked internally),
    // §"E2E bridge contract", the per-driver `call_machine_method(name,
    // json_arg)` reaches these setters to fixture wizard snapshots
    // without driving the real probe path. Gated behind the
    // `test-helpers` feature so production wasm doesn't ship them;
    // build with `just web-test` for the e2e-enabled bundle.

    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = setHandleCheckSnapshotForTest)]
    pub fn set_handle_check_snapshot_for_test(&self, snap_json: String) -> Result<(), JsValue> {
        let snap: fauna_onboarding_machine::HandleCheckSnapshot = serde_json::from_str(&snap_json)
            .map_err(|e| JsValue::from_str(&format!("invalid HandleCheckSnapshot JSON: {e}")))?;
        self.0.set_handle_check_snapshot_for_test(snap);
        Ok(())
    }

    // ── E2E bridge: the nest-identity (TOFU) pin seam ──
    //
    // Unlike the setters around them these do NOT re-implement anything: they
    // hand the name straight to the machine's shared `call_machine_method*`
    // dispatcher, the same one the native apps route through. That is what
    // makes the pin seam ONE name table rather than a web copy and a native copy
    // (`docs/goal/behavior/onboarding.md` § E2E bridge contract) — web only needs
    // these thin exports because its bridge reflects over the wasm-bindgen
    // surface instead of calling the dispatcher by name.
    //
    // The dispatcher writes whatever pin store this client installed, so the same
    // call seeds `LocalStoragePinStore` here and `DiskPinStore` on native.

    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = setNestIdentityPinForTest)]
    pub fn set_nest_identity_pin_for_test(&self, json: String) -> Result<(), JsValue> {
        self.0
            .call_machine_method("set_nest_identity_pin_for_test".to_string(), json);
        Ok(())
    }

    /// The pin currently held for a nest, as a JSON hex string (`null` when none)
    /// — lets the journey assert the trust button FORGOT the pin without reading
    /// localStorage directly.
    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = nestIdentityPinForTest)]
    pub fn nest_identity_pin_for_test(&self, json: String) -> String {
        self.0
            .call_machine_method_with_result("nest_identity_pin_for_test".to_string(), json)
            .unwrap_or_else(|| "null".to_string())
    }

    /// The deferred-DNS seed, routed through the shared dispatcher.
    ///
    /// ⚠ This exists because the reflecting bridge finds the WRONG method
    /// otherwise. `seedAwaitingManualDns` (above) is the production hydration
    /// binding and takes FOUR POSITIONAL strings; the cross-app e2e contract
    /// for `seed_awaiting_manual_dns` is a single JSON OBJECT
    /// (`{nest_url, handle, dns_records, claim_code}` —
    /// `OnboardingMachine::call_machine_method`'s arm). Web's bridge reflects
    /// over the wasm-bindgen surface by camelCased name, so it used to find the
    /// positional binding and hand it the object as its FIRST argument, leaving
    /// the other three `undefined`. wasm-bindgen coerces rather than rejecting,
    /// so nothing threw: the machine was seeded with a garbage `nest_url` and an
    /// EMPTY record list, and the page wedged hard enough to peg the
    /// single-threaded e2e bridge.
    ///
    /// Same shape, and same reason, as the nest-identity pin exports above:
    /// hand the name straight to the shared dispatcher so there is ONE name
    /// table, not a web copy and a native copy.
    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = seedAwaitingManualDnsForTest)]
    pub fn seed_awaiting_manual_dns_for_test(&self, json: String) {
        self.0
            .call_machine_method("seed_awaiting_manual_dns".to_string(), json);
    }

    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = setInviteRequestSnapshotForTest)]
    pub fn set_invite_request_snapshot_for_test(&self, snap_json: String) -> Result<(), JsValue> {
        let snap: fauna_onboarding_machine::InviteRequestSnapshot =
            serde_json::from_str(&snap_json).map_err(|e| {
                JsValue::from_str(&format!("invalid InviteRequestSnapshot JSON: {e}"))
            })?;
        self.0.set_invite_request_snapshot_for_test(snap);
        Ok(())
    }

    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = setClaimCodeSnapshotForTest)]
    pub fn set_claim_code_snapshot_for_test(&self, snap_json: String) -> Result<(), JsValue> {
        let snap: fauna_onboarding_machine::ClaimCodeSnapshot = serde_json::from_str(&snap_json)
            .map_err(|e| JsValue::from_str(&format!("invalid ClaimCodeSnapshot JSON: {e}")))?;
        self.0.set_claim_code_snapshot_for_test(snap);
        Ok(())
    }

    /// Test-only: inject a verified DNS-provider credential into the wizard's DNS
    /// sub-state so `capturedDnsCredentialJson()` returns it at `LoggedIn` — the
    /// onboarding→launch hand-off the per-app launch glue consumes. The real
    /// onboarding DNS-verify routes through `proxy.fauna.social` (unreachable in
    /// the e2e sandbox), so the web onboarding-launch-glue test injects a
    /// pre-verified credential here instead of driving the real verify. `json` is
    /// `{"provider_id": "...", "fields": {"<field-id>": "<value>"}}` (mirrors
    /// `DnsConfigState`'s `selected_provider_id` + `creds`); sets `verified=true`
    /// and `set_up_later=false` so `captured_dns_credential()` yields `Some`. Pair
    /// with a `fake-dns-ok:<zone>` field value + `enableDnsFakeProviderForTest`
    /// so the launched client's `PutCredentials.verify()` succeeds offline.
    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = setCapturedDnsCredentialForTest)]
    pub fn set_captured_dns_credential_for_test(&self, json: String) -> Result<(), JsValue> {
        #[derive(serde::Deserialize)]
        struct Captured {
            provider_id: String,
            fields: std::collections::HashMap<String, String>,
        }
        let c: Captured = serde_json::from_str(&json).map_err(|e| {
            JsValue::from_str(&format!("invalid captured-DNS-credential JSON: {e}"))
        })?;
        self.0.set_dns_state_for_test(|s| {
            s.selected_provider_id = Some(c.provider_id);
            s.creds = c
                .fields
                .into_iter()
                .map(|(k, v)| (k, fauna_core::secret::SecretString::from(v)))
                .collect();
            s.verified = true;
            s.set_up_later = false;
        });
        Ok(())
    }

    /// Test-only: seed the VPS-config state so `run_provisioning_inner`'s
    /// reads succeed without driving the real `verify_vps()` HTTP round-trip.
    /// Mirrors `setCapturedDnsCredentialForTest` (the DNS twin). `json` is a
    /// `{provider_id, creds, server_types, selected_server_type_id, locations,
    /// selected_location_id}` object; `creds` keys are the provider's
    /// kebab-case field ids (`"api-token"` for Hetzner — snake_case silently
    /// yields a `dispatch::vps_provider(...) -> None`). `server_types` /
    /// `locations` are JSON mirrors of `ServerTypeInfo` / `VpsLocation`.
    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = setVpsStateForTest)]
    pub fn set_vps_state_for_test(&self, json: String) -> Result<(), JsValue> {
        #[derive(serde::Deserialize)]
        struct VpsSeed {
            provider_id: String,
            #[serde(default)]
            creds: std::collections::HashMap<String, String>,
            #[serde(default)]
            server_types: Vec<fauna_provisioning::vps::ServerTypeInfo>,
            #[serde(default)]
            selected_server_type_id: Option<String>,
            #[serde(default)]
            locations: Vec<fauna_provisioning::vps::VpsLocation>,
            #[serde(default)]
            selected_location_id: Option<String>,
        }
        let s: VpsSeed = serde_json::from_str(&json)
            .map_err(|e| JsValue::from_str(&format!("invalid VPS-seed JSON: {e}")))?;
        self.0.set_vps_state_for_test(|st| {
            st.selected_provider_id = Some(s.provider_id);
            st.creds = s
                .creds
                .into_iter()
                .map(|(k, v)| (k, fauna_core::secret::SecretString::from(v)))
                .collect();
            st.server_types = s.server_types;
            st.selected_server_type_id = s.selected_server_type_id;
            st.locations = s.locations;
            st.selected_location_id = s.selected_location_id;
            st.verified = true;
        });
        Ok(())
    }

    /// Test-only: seed a verified DNS-provider selection plus
    /// `dns.buy_domain` + `dns.current_availability` so `provider_status()`
    /// reaches `UnregisteredBuyable` without driving the real
    /// registrar-verify HTTP round-trip — the domain-line twin of
    /// `setVpsStateForTest`, for fixturing `billOfMaterials()`'s optional
    /// domain-registration line. `json` is `{provider_id?, buy_domain,
    /// price_cents, currency?}` (`provider_id` defaults to `"cloudflare"`).
    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = setDnsAvailabilityForTest)]
    pub fn set_dns_availability_for_test(&self, json: String) -> Result<(), JsValue> {
        #[derive(serde::Deserialize)]
        struct DnsAvailabilitySeed {
            #[serde(default = "default_test_dns_provider_id")]
            provider_id: String,
            buy_domain: bool,
            price_cents: u64,
            #[serde(default)]
            currency: Option<String>,
        }
        fn default_test_dns_provider_id() -> String {
            "cloudflare".to_string()
        }
        let s: DnsAvailabilitySeed = serde_json::from_str(&json)
            .map_err(|e| JsValue::from_str(&format!("invalid DNS-availability-seed JSON: {e}")))?;
        self.0.set_dns_availability_for_test(
            s.provider_id,
            s.buy_domain,
            s.price_cents,
            s.currency,
        );
        Ok(())
    }

    /// Test-only: jump the wizard to a given step name. Accepts the
    /// Rust Debug name (e.g. `"HandleEntry"`, `"InviteRequest"`,
    /// `"DnsConfig"`). Used by the e2e bridge before snapshot setters
    /// so the page actually renders.
    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = setStepForTest)]
    pub fn set_step_for_test(&self, step_name: String) -> Result<(), JsValue> {
        use fauna_onboarding_machine::OnboardingStep;
        let step = match step_name.as_str() {
            "IdentityChoice" => OnboardingStep::IdentityChoice,
            "IdentityCreated" => OnboardingStep::IdentityCreated,
            "IdentityImport" => OnboardingStep::IdentityImport,
            "HandleEntry" => OnboardingStep::HandleEntry,
            "DnsConfig" => OnboardingStep::DnsConfig,
            "VpsConfig" => OnboardingStep::VpsConfig,
            "NestProvisioning" => OnboardingStep::NestProvisioning,
            "DnsPostInstructions" => OnboardingStep::DnsPostInstructions,
            "InviteRequest" => OnboardingStep::InviteRequest,
            "ClaimCode" => OnboardingStep::ClaimCode,
            "NatModeChoice" => OnboardingStep::NatModeChoice,
            "NestRecovery" => OnboardingStep::NestRecovery,
            "RecoverSelfhostedInstructions" => OnboardingStep::RecoverSelfhostedInstructions,
            "Done" => OnboardingStep::Done,
            other => {
                return Err(JsValue::from_str(&format!(
                    "unknown OnboardingStep: {other}"
                )));
            }
        };
        self.0.set_step_for_test(step);
        Ok(())
    }

    /// Test-only: inject a `ProvisioningSnapshot` directly. Used by the e2e
    /// bridge to fixture the `nest_provisioning` page without running a real
    /// provisioning task. `snap_json` is a JSON-serialized `ProvisioningSnapshot`.
    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = setProvisioningSnapshotForTest)]
    pub fn set_provisioning_snapshot_for_test(&self, snap_json: String) -> Result<(), JsValue> {
        let snap: fauna_provisioning::progress::ProvisioningSnapshot =
            serde_json::from_str(&snap_json).map_err(|e| {
                JsValue::from_str(&format!("invalid ProvisioningSnapshot JSON: {e}"))
            })?;
        self.0.set_provisioning_snapshot_for_test(snap);
        Ok(())
    }

    /// Test-only: inject DNS records directly. Used by the e2e bridge to
    /// fixture the `dns_post_instructions` page. `records_json` is a
    /// JSON-serialized `Vec<DnsRecordPlain>`.
    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = setDnsRecordsForTest)]
    pub fn set_dns_records_for_test(&self, records_json: String) -> Result<(), JsValue> {
        let records: Vec<fauna_onboarding_machine::DnsRecordPlain> =
            serde_json::from_str(&records_json)
                .map_err(|e| JsValue::from_str(&format!("invalid DnsRecordPlain JSON: {e}")))?;
        self.0.set_dns_records_for_test(records);
        Ok(())
    }

    /// Test-only: hand a bridge method name to the SHARED async dispatcher —
    /// web's machine-bound twin of `callMachineFreeMethodForTest`, and the
    /// same door `apps/fauna-tui/src/automation.rs` routes through.
    ///
    /// The web bridge hook reaches this machine by *reflecting* the camelCased
    /// name onto the `wasm_bindgen` surface, so a dispatcher name with no
    /// hand-written export here is simply unreachable from web — it throws
    /// `OnboardingMachine has no method '<name>'` rather than falling through
    /// to the shared name table every native app already reads. That gap is
    /// invisible until a cross-app test drives the name: `set_provision_labels`
    /// and `set_provision_image_tag` (both dispatcher-only,
    /// `fauna-onboarding-machine/src/machine.rs`) are exactly this shape, and
    /// the first is load-bearing for the live e2e's teardown sweep — without it
    /// a web-driven box carries no `fauna-e2e=1` provider label and the orphan
    /// sweep cannot find it, which costs real money rather than a red test.
    ///
    /// So the fix is the generic door, not two more per-method exports: one
    /// name table, not two (priority #2). Async, like tui's arm, so the machine's
    /// async methods (`verify_dns`, …) run to completion before the ack — through
    /// the sync dispatcher they hit its silent-ignore arm and ack green having
    /// done nothing. Fire-and-forget orchestration (`start_provisioning`) stays
    /// out of the shared dispatcher by design and keeps its own export above.
    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = callMachineMethodForTest)]
    pub fn call_machine_method_for_test(&self, name: String, json_arg: String) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        let method = name.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            Ok(match m.call_machine_method_async(name, json_arg).await {
                // `v` is already the shared `call_machine_method_with_result`
                // convention's JSON-serialized value — publishing it as a bare
                // JS string double-encodes it (`JsValue::from_str` embeds the
                // literal `"..."` as string content instead of unwrapping it),
                // so a `provider_base_url` read crossed the bridge as
                // `"\"http://…\""` instead of `http://…`. tui/linux parse in
                // exactly this spot (`machine_method_result_json`) before
                // publishing; mirror that here with `JSON.parse` rather than
                // introducing a second decode convention.
                //
                // A decode failure here is currently unreachable — the shared
                // convention always serializes valid JSON — but silently
                // collapsing it to the same `NULL` as "no result" is
                // convention 11's silent-decline shape (`parse_bridge_arg`'s
                // own warn-don't-drop precedent, `machine.rs:6194-6206`); warn
                // instead so the two cases stay distinguishable if the shared
                // convention's shape ever changes.
                Some(v) => js_sys::JSON::parse(&v).unwrap_or_else(|error| {
                    tracing::warn!(
                        method,
                        ?error,
                        "e2e bridge: callMachineMethodForTest got unparseable JSON back from the shared dispatcher"
                    );
                    JsValue::NULL
                }),
                None => JsValue::NULL,
            })
        })
    }
}

#[wasm_bindgen(js_name = formatPrice)]
pub fn format_price_wasm(cents: u64, currency: String) -> String {
    fauna_onboarding_machine::format_price(cents, currency)
}

/// `fauna_onboarding_machine::handle_tld` → the TLD of the onboarding handle's
/// domain (the substring after the domain's last dot), or `undefined` when the
/// handle has no real TLD (no `@`, an empty local part / domain, or a dot-less
/// domain) — so the dns_config "none of our registrars carry `.{tld}`" message
/// hides. Mirrors the UniFFI `handle_tld` the native apps call by the same
/// name, single-sourcing the derivation across all seven apps (priority #2).
/// See `docs/goal/behavior/onboarding.md` § 4. DNS configuration.
#[wasm_bindgen(js_name = handleTld)]
pub fn handle_tld_wasm(handle: String) -> Option<String> {
    fauna_onboarding_machine::handle_tld(handle)
}

/// Display label for a VPS server-type radio button:
/// `"{id} — {vcpu} vCPU / {mem_gb} GB / {price/100} {ccy}/mo"`. Mirrors
/// the UniFFI helper that Apple / Kotlin / Linux call by the same name.
/// JS callers pass the JSON-serialized `ServerTypeInfo` they pulled out
/// of `vpsConfigJson()` — the wasm bridge deserializes once and forwards
/// to the shared formatter so the label format stays single-sourced.
#[wasm_bindgen(js_name = serverTypeLabel)]
pub fn server_type_label_wasm(server_type_json: &str) -> String {
    let st: fauna_provisioning::vps::ServerTypeInfo = match serde_json::from_str(server_type_json) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    fauna_onboarding_machine::server_type_label(st)
}

/// `fauna_onboarding_machine::server_type_allowed_for_mail` → RAM gate for the
/// `vps-config-mail-mode-toggle`: whether `st` may be selected given the chosen
/// mail mode (mail ON requires `mem_gb ≥ 2`; mail OFF allows every plan). Mirrors
/// the UniFFI helper the native apps call by the same name, so the gate stays
/// single-sourced. JS callers pass the JSON-serialized `ServerTypeInfo` (from
/// `vpsConfigJson()`) plus the current `provisionMailModeEnabled()` value. An
/// unparseable JSON conservatively returns `true` (don't hide a plan on a parse
/// glitch). Per `docs/goal/behavior/onboarding.md` §5.
#[wasm_bindgen(js_name = serverTypeAllowedForMail)]
pub fn server_type_allowed_for_mail_wasm(server_type_json: &str, enable_mail: bool) -> bool {
    let st: fauna_provisioning::vps::ServerTypeInfo = match serde_json::from_str(server_type_json) {
        Ok(v) => v,
        Err(_) => return true,
    };
    fauna_onboarding_machine::server_type_allowed_for_mail(st, enable_mail)
}

/// `fauna_onboarding_machine::qualify_reclaim_handle` → re-qualify a bare-localpart
/// admin handle with its mail domain for a factory-reset re-claim
/// (`mail-bridge-lifecycle.md` § Factory reset). Mirrors the UniFFI helper the
/// native apps call by the same name, so the qualification rule and the
/// nest-URL-host parse stay single-sourced. JS passes the bare handle, the cached
/// mail domain (or `undefined`), and the nest base URL; an empty/already-`@`
/// handle is returned unchanged.
#[wasm_bindgen(js_name = qualifyReclaimHandle)]
pub fn qualify_reclaim_handle_wasm(
    handle: String,
    domain: Option<String>,
    nest_url: String,
) -> String {
    fauna_onboarding_machine::qualify_reclaim_handle(handle, domain, nest_url)
}

/// `fauna_provisioning::progress::elapsed_display` → the `provisioning-elapsed`
/// ticker as a serialized `LocalizedText` `{ key, args }` (or `null`/`undefined`
/// before the run starts) that the SPA resolves through `resolveLocalized`.
/// Mirrors the UniFFI `provisioning_elapsed` the native apps call, so the
/// elapsed format stays single-sourced. JS passes the snapshot's
/// `started_at_ms`/`finished_at_ms` plus its live tick `now_ms`. See
/// `docs/goal/behavior/value-formatting.md`.
#[wasm_bindgen(js_name = provisioningElapsed)]
pub fn provisioning_elapsed_wasm(
    started_at_ms: Option<f64>,
    finished_at_ms: Option<f64>,
    now_ms: f64,
) -> Result<JsValue, JsValue> {
    let lt = fauna_provisioning::progress::elapsed_display(
        started_at_ms.map(|v| v as u64),
        finished_at_ms.map(|v| v as u64),
        now_ms as u64,
    );
    serde_wasm_bindgen::to_value(&lt).map_err(err_to_js)
}

/// `fauna_provisioning::progress::status_glyph` → the canonical
/// `provisioning-step-checkbox` glyph (`○`/`…`/`—`/`✓`/`✗`). `status` is the
/// serde variant name (`"Pending"`/`"Running"`/…) the SPA reads off the parsed
/// snapshot. Mirrors the UniFFI `provisioning_status_glyph` the native apps
/// call, single-sourcing the glyph across all seven apps. An unrecognised status
/// → empty string.
#[wasm_bindgen(js_name = provisioningStatusGlyph)]
pub fn provisioning_status_glyph_wasm(status: &str) -> String {
    match serde_json::from_value::<fauna_provisioning::progress::StepStatus>(
        serde_json::Value::String(status.to_string()),
    ) {
        Ok(s) => fauna_provisioning::progress::status_glyph(s),
        Err(_) => String::new(),
    }
}

/// `fauna_provisioning::progress::step_label` → the user-visible step name as a
/// serialized `LocalizedText` `{ key, args }` the SPA resolves via
/// `resolveLocalized`. `kind` is the serde variant name (`"Domain"`/…). Mirrors
/// the UniFFI `provisioning_step_label`, single-sourcing the canonical
/// `onboarding.provision.step.*` key family.
#[wasm_bindgen(js_name = provisioningStepLabel)]
pub fn provisioning_step_label_wasm(kind: &str) -> Result<JsValue, JsValue> {
    let kind: fauna_provisioning::progress::ProvisionStep =
        serde_json::from_value(serde_json::Value::String(kind.to_string()))
            .map_err(|e| JsValue::from_str(&format!("invalid ProvisionStep: {e}")))?;
    let lt = fauna_provisioning::progress::step_label(kind);
    serde_wasm_bindgen::to_value(&lt).map_err(err_to_js)
}

/// `fauna_provisioning::progress::substep_label` → the sub-step text as a
/// serialized `LocalizedText` the SPA resolves via `resolveLocalized`. `key` is
/// the serde variant name (`"DomainRegistering"`/…); `cause` fills the
/// `status_retrying` `{cause}` (pass the step's `last_error`, else
/// null/undefined). Mirrors the UniFFI `provisioning_substep_label`.
#[wasm_bindgen(js_name = provisioningSubstepLabel)]
pub fn provisioning_substep_label_wasm(
    key: &str,
    cause: Option<String>,
) -> Result<JsValue, JsValue> {
    let key: fauna_provisioning::progress::SubstepKey =
        serde_json::from_value(serde_json::Value::String(key.to_string()))
            .map_err(|e| JsValue::from_str(&format!("invalid SubstepKey: {e}")))?;
    let lt = fauna_provisioning::progress::substep_label(key, cause);
    serde_wasm_bindgen::to_value(&lt).map_err(err_to_js)
}

/// The `invite_request` page's poll cadence, in ms — the wasm twin of the
/// UniFFI `invite_recheck_poll_ms()`.
///
/// Exported so web reads the one shared number instead of restating it.
/// `onboarding.md` § The pending-invite surface: "read by all 7 apps — never
/// seven hand-copied numbers."
///
/// ⚠ `f64`, not `u64`: wasm-bindgen maps a 64-bit integer to a JS **BigInt**,
/// and `setInterval` does not accept one. These values feed timers directly, so
/// the number type is load-bearing at the call site.
#[wasm_bindgen(js_name = inviteRecheckPollMs)]
pub fn invite_recheck_poll_ms_wasm() -> f64 {
    fauna_onboarding_machine::INVITE_RECHECK_POLL_MS as f64
}

/// The `awaiting_manual_dns` page's poll cadence, in ms — see
/// [`invite_recheck_poll_ms_wasm`], including the `f64` note. Web had a
/// hand-copied `10_000` for this until 2026-08-12; it was one of the copies
/// that bullet names.
#[wasm_bindgen(js_name = awaitingDnsPollMs)]
pub fn awaiting_dns_poll_ms_wasm() -> f64 {
    fauna_onboarding_machine::AWAITING_DNS_POLL_MS as f64
}

// ── Admin-panel NAT-mode control (Admin → Nest) ──────────────────────────────
//
// The wasm twin of the shared `AdminNatModeMachine`
// (`fauna_onboarding_machine::admin_nat_mode`) — the post-onboarding change
// surface for the NAT axis, rendered on `admin-nest` as
// `admin-nest-nat-mode-{public-radio,private-radio,save-button,status}`
// (`docs/goal/behavior/admin.md` § Nest → NAT-mode control). Same commit
// ceremony as the wizard page; dispatch-style (await an action, re-read
// `snapshotJson`).

#[wasm_bindgen]
pub struct AdminNatModeMachine(Arc<fauna_onboarding_machine::AdminNatModeMachine>);

#[wasm_bindgen]
impl AdminNatModeMachine {
    /// `nest_url` is the connected nest; `secret_hex` the admin's identity
    /// secret (the payload signature is the authorization — no bearer).
    #[wasm_bindgen(constructor)]
    pub fn new(nest_url: String, secret_hex: String) -> AdminNatModeMachine {
        AdminNatModeMachine(fauna_onboarding_machine::AdminNatModeMachine::new(
            nest_url,
            secret_hex.into(),
        ))
    }

    /// JSON-serialized `NatModeSnapshot` — the same shape the wizard's
    /// `natModeSnapshotJson` carries.
    #[wasm_bindgen(js_name = snapshotJson)]
    pub fn snapshot_json(&self) -> String {
        serde_json::to_string(&self.0.snapshot()).unwrap_or_default()
    }

    /// Radio wiring; `mode` is the lowercase wire form (`"public"`/`"private"`).
    #[wasm_bindgen(js_name = select)]
    pub fn select(&self, mode: String) -> Result<(), JsValue> {
        use fauna_onboarding_machine::NodeMode;
        let m = NodeMode::from_wire_str(&mode)
            .ok_or_else(|| JsValue::from_str(&format!("unknown NodeMode: {mode}")))?;
        self.0.select(m);
        Ok(())
    }

    /// Page load: read `fauna.setup.status` and pre-select the current mode.
    #[wasm_bindgen(js_name = hydrate)]
    pub fn hydrate(&self) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            m.hydrate().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `admin-nest-nat-mode-save-button` wiring: sign + commit the selected
    /// mode via the mutable `fauna.setup.nat_mode`.
    #[wasm_bindgen(js_name = submit)]
    pub fn submit(&self) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            m.submit().await;
            Ok(JsValue::UNDEFINED)
        })
    }
}

// ── nest_retire ──────────────────────────────────────────────────────────
//
// Retiring an app-provisioned nest (`docs/goal/behavior/nest-retirement.md`).
// Hosted by the wizard host like `nest_recovery`, and reachable from
// `launch_retry` when no nest is up — so this face must load **pre-auth**,
// which is why it lives in this chunk beside the wizard rather than in the
// authenticated one. Dispatch-style, same as `AdminNatModeMachine`: await an
// action, re-read `snapshotJson`.

#[wasm_bindgen]
pub struct NestRetireMachine(Arc<fauna_onboarding_machine::NestRetireMachine>);

#[wasm_bindgen]
impl NestRetireMachine {
    /// All inputs optional. `heldDnsCredsJson[i]` is the `providers.yaml`-keyed
    /// credential bag of `heldDnsProviderIds[i]` — one pair per credential the
    /// app already holds (`fauna.state.dns`), passed **in** because the machine
    /// never reads config and never talks to a nest.
    #[wasm_bindgen(constructor)]
    pub fn new(
        candidate_domains: Vec<String>,
        current_ipv4: Option<String>,
        held_dns_provider_ids: Vec<String>,
        held_dns_creds_json: Vec<String>,
        provider_base_url: Option<String>,
    ) -> NestRetireMachine {
        NestRetireMachine(fauna_onboarding_machine::NestRetireMachine::new(
            candidate_domains,
            current_ipv4,
            held_dns_provider_ids,
            held_dns_creds_json,
            provider_base_url,
        ))
    }

    /// Candidates learned after the page opened — the admin entry's active
    /// local-domain list.
    #[wasm_bindgen(js_name = addCandidateDomains)]
    pub fn add_candidate_domains(&self, domains: Vec<String>) {
        self.0.add_candidate_domains(domains);
    }

    /// Replace the held DNS credentials with ones read after the page opened
    /// (the same index-paired bags as the constructor).
    #[wasm_bindgen(js_name = setHeldDnsJson)]
    pub fn set_held_dns_json(
        &self,
        held_dns_provider_ids: Vec<String>,
        held_dns_creds_json: Vec<String>,
    ) {
        self.0
            .set_held_dns_json(held_dns_provider_ids, held_dns_creds_json);
    }

    /// JSON-serialized `RetireSnapshot`.
    #[wasm_bindgen(js_name = snapshotJson)]
    pub fn snapshot_json(&self) -> String {
        serde_json::to_string(&self.0.snapshot()).unwrap_or_default()
    }

    #[wasm_bindgen(js_name = selectProvider)]
    pub fn select_provider(&self, provider_id: String) {
        self.0.select_provider(provider_id);
    }

    #[wasm_bindgen(js_name = setCredentialField)]
    pub fn set_credential_field(&self, field_id: String, value: String) {
        self.0.set_credential_field(field_id, value);
    }

    #[wasm_bindgen(js_name = select)]
    pub fn select(&self, server_id: String) {
        self.0.select(server_id);
    }

    #[wasm_bindgen(js_name = beginConfirm)]
    pub fn begin_confirm(&self) {
        self.0.begin_confirm();
    }

    #[wasm_bindgen(js_name = setConfirmName)]
    pub fn set_confirm_name(&self, typed: String) {
        self.0.set_confirm_name(typed);
    }

    #[wasm_bindgen(js_name = back)]
    pub fn back(&self) {
        self.0.back();
    }

    /// Leaving the page. Drops the VPS token and any fetched transfer code —
    /// neither outlives the visit (§ Credential stance).
    #[wasm_bindgen(js_name = cancel)]
    pub fn cancel(&self) {
        self.0.cancel();
    }

    #[wasm_bindgen(js_name = verify)]
    pub fn verify(&self) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            m.verify().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    #[wasm_bindgen(js_name = fetchTransferCode)]
    pub fn fetch_transfer_code(&self) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            m.fetch_transfer_code().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// DNS first, then the server. No-op until the typed-name gate passes.
    #[wasm_bindgen(js_name = confirm)]
    pub fn confirm(&self) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            m.confirm().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    #[wasm_bindgen(js_name = retry)]
    pub fn retry(&self) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            m.retry().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Delete the server despite a failed DNS step.
    #[wasm_bindgen(js_name = forceServer)]
    pub fn force_server(&self) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            m.force_server().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// This session's nest's IPv4, once the app has resolved it — re-marks
    /// the *current* badge on rows already listed.
    #[wasm_bindgen(js_name = setCurrentIpv4)]
    pub fn set_current_ipv4(&self, ipv4: Option<String>) {
        self.0.set_current_ipv4(ipv4);
    }

    // ── hosted-auth (the bundled provider's sign-in, reused from vps_config) ──

    /// Step 1: resolves to a JSON `HostedAuthPrompt`, or `null` when the
    /// attempt failed (`hostedAuthStateJson` says why).
    #[wasm_bindgen(js_name = hostedAuthBegin)]
    pub fn hosted_auth_begin(&self, field_id: String) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            Ok(match m.hosted_auth_begin(field_id).await {
                Some(prompt) => {
                    JsValue::from_str(&serde_json::to_string(&prompt).unwrap_or_default())
                }
                None => JsValue::NULL,
            })
        })
    }

    /// Step 2: resolves once the attempt ends — `Connected` or `Failed`.
    #[wasm_bindgen(js_name = hostedAuthWait)]
    pub fn hosted_auth_wait(&self, field_id: String) -> js_sys::Promise {
        let m = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            m.hosted_auth_wait(field_id).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// JSON `HostedAuthState`.
    #[wasm_bindgen(js_name = hostedAuthStateJson)]
    pub fn hosted_auth_state_json(&self, field_id: String) -> String {
        serde_json::to_string(&self.0.hosted_auth_state(field_id)).unwrap_or_default()
    }

    #[wasm_bindgen(js_name = hostedAuthCanBegin)]
    pub fn hosted_auth_can_begin(&self, field_id: String) -> bool {
        self.0.hosted_auth_can_begin(field_id)
    }

    // ── the view's sentences (JSON `LocalizedText` / arrays of it) ──

    #[wasm_bindgen(js_name = confirmSummaryJson)]
    pub fn confirm_summary_json(&self) -> String {
        serde_json::to_string(&self.0.confirm_summary()).unwrap_or_default()
    }

    #[wasm_bindgen(js_name = leftoverLinesJson)]
    pub fn leftover_lines_json(&self) -> String {
        serde_json::to_string(&self.0.leftover_lines()).unwrap_or_default()
    }

    #[wasm_bindgen(js_name = transferCodeNoteJson)]
    pub fn transfer_code_note_json(&self, server_id: String) -> String {
        serde_json::to_string(&self.0.transfer_code_note(server_id)).unwrap_or_default()
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
    fauna_wasm_panic_hook::install("fauna-wasm-onboarding");
}

/// Test-only: deliberately panics, so an e2e can assert the hook above really
/// names this chunk in the browser console — a headless witness, not a
/// review-only claim. Compiled out of every non-`test-helpers` build.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = panicForTestOnly)]
pub fn panic_for_test_only() {
    panic!("deliberate test panic");
}

/// The **machine-free** arms of the cross-app E2E bridge, as a free export.
///
/// Returns `Some(json)` when the shared dispatcher handled the name (the inner
/// string is a reader's JSON result, or `"null"` for a setter), and `None` when
/// the name genuinely needs a live `OnboardingMachine` — which is precisely the
/// signal the SPA bridge uses to decide whether to load the onboarding module at
/// all.
///
/// **Why web needs this.** The pin seed/reader must work on a *live
/// authenticated session*, where web has no onboarding machine: the page that
/// constructs one (`routes/onboarding/+page.svelte`) is the module's only
/// importer, so post-auth the machine-bound exports next to this one are simply
/// not reachable. That is the same no-machine-post-auth wall linux hit
/// (`security.md` § Post-auth surfacing, seam 1), and
/// `call_machine_free_method` is the shared answer to it — this export is web's
/// door onto it, not a second implementation of the name table.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = callMachineFreeMethodForTest)]
pub fn call_machine_free_method_for_test(name: String, json_arg: String) -> Option<String> {
    match fauna_onboarding_machine::call_machine_free_method(&name, &json_arg) {
        fauna_onboarding_machine::FreeMethodOutcome::Handled(v) => {
            Some(v.unwrap_or_else(|| "null".to_string()))
        }
        fauna_onboarding_machine::FreeMethodOutcome::NeedsMachine => None,
    }
}
