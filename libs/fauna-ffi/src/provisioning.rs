use crate::{FfiError, general_err};

fn block_on<F, T>(fut: F) -> Result<T, FfiError>
where
    F: std::future::Future<Output = Result<T, FfiError>>,
{
    let rt = tokio::runtime::Runtime::new().map_err(|e| FfiError::General {
        msg: format!("failed to create tokio runtime: {e}"),
    })?;
    rt.block_on(fut)
}

/// Verify a VPS provider's credentials and return available locations as JSON.
///
/// `provider_json` must contain a `"provider"` field set to one of:
/// - hetzner / digitalocean / vultr / linode: `token`
/// - ovh: `app_key`, `app_secret`, `consumer_key`, `project_id`
///
/// Returns a JSON array of `{id, name, city, country}` objects.
#[uniffi::export]
pub fn verify_vps_provider(provider_json: String) -> Result<String, FfiError> {
    block_on(async move {
        let client = reqwest::Client::new();
        let locations =
            fauna_provisioning::verify_json::verify_vps_from_json(&provider_json, &client)
                .await
                .map_err(|msg| FfiError::General { msg })?;
        serde_json::to_string(&locations).map_err(|e| FfiError::General {
            msg: format!("serialization error: {e}"),
        })
    })
}

/// Verify a DNS provider's credentials and return available zones as JSON.
///
/// `provider_json` must contain a `"provider"` field set to one of:
/// - cloudflare / gandi / hetzner: `token`
/// - namecheap: `api_user`, `api_key`, `client_ip`
/// - porkbun: `apikey`, `secretapikey`
///
/// Returns a JSON array of `{id, name}` objects.
#[uniffi::export]
pub fn verify_dns_provider(provider_json: String) -> Result<String, FfiError> {
    block_on(async move {
        let client = reqwest::Client::new();
        let zones = fauna_provisioning::verify_json::verify_dns_from_json(&provider_json, &client)
            .await
            .map_err(|msg| FfiError::General { msg })?;
        serde_json::to_string(&zones).map_err(|e| FfiError::General {
            msg: format!("serialization error: {e}"),
        })
    })
}

/// Check a domain's availability via the registrar API.
///
/// - `provider_id`: lowercase ProviderId discriminant (e.g. `"gandi"`).
/// - `creds_json`: JSON object mapping credential field ids to values
///   (same shape as elsewhere in this module).
///
/// Returns a JSON-encoded `DomainAvailability`.
#[uniffi::export]
pub fn registrar_check(
    provider_id: String,
    creds_json: String,
    domain: String,
) -> Result<String, FfiError> {
    block_on(async move {
        use fauna_provisioning::registrar::Registrar;
        let dispatch = fauna_provisioning::dispatch::resolve_registrar(&provider_id, &creds_json)
            .map_err(|msg| FfiError::General { msg })?;
        let client = reqwest::Client::new();
        let avail = dispatch
            .check(&client, &domain)
            .await
            .map_err(general_err)?;
        serde_json::to_string(&avail).map_err(|e| FfiError::General {
            msg: format!("serialization error: {e}"),
        })
    })
}

/// Structured availability outcome consumed by the dns_config wizard's
/// per-provider status text. Maps to one of `Buyable { price_cents, currency }`,
/// `Unavailable`, or `TldNotSupported`.
///
/// - `provider_id`, `creds_json`, `domain`: same as `registrar_check`.
///
/// Returns a JSON-encoded `RegistrarAvailability`.
#[uniffi::export]
pub fn registrar_availability(
    provider_id: String,
    creds_json: String,
    domain: String,
) -> Result<String, FfiError> {
    block_on(async move {
        use fauna_provisioning::registrar::Registrar;
        let dispatch = fauna_provisioning::dispatch::resolve_registrar(&provider_id, &creds_json)
            .map_err(|msg| FfiError::General { msg })?;
        let client = reqwest::Client::new();
        let avail = dispatch
            .availability(&client, &domain)
            .await
            .map_err(general_err)?;
        serde_json::to_string(&avail).map_err(|e| FfiError::General {
            msg: format!("serialization error: {e}"),
        })
    })
}

/// Register a domain via the registrar API. Charges the registrar account.
///
/// - `agreed_price_cents`: the price the user confirmed in the UI (integer
///   cents of the currency from `registrar_check`/`registrar_availability`).
/// - `contact_json`: either the literal string `"null"`, the empty string, or
///   a JSON-encoded `ContactInfo`. Registrars that use account-level WHOIS
///   contacts (Porkbun) accept `"null"`; others must supply a full contact.
///
/// Returns a JSON-encoded `RegistrationResult`.
#[uniffi::export]
pub fn registrar_register(
    provider_id: String,
    creds_json: String,
    domain: String,
    years: u32,
    agreed_price_cents: u64,
    contact_json: String,
) -> Result<String, FfiError> {
    block_on(async move {
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
        .map_err(|msg| FfiError::General { msg })?;
        serde_json::to_string(&result).map_err(|e| FfiError::General {
            msg: format!("serialization error: {e}"),
        })
    })
}

// NOTE (2026-07-15 dark-rail audit): the standalone `registrar_list_tld_pricing`
// and `vps_list_server_types` free fns were deleted — the shared
// `OnboardingMachine` absorbed both reads (registrar pricing surfaces via
// `RegistrarAvailability::Buyable`; server types via the machine's
// `vps_dispatch.list_server_types` → snapshot), and no client ever called the
// raw fns. The wasm twins in `fauna-wasm-onboarding` are equally caller-less
// but carry an explicit keep-comment — flagged to their owner, not touched here.

/// `fauna_provisioning::progress::elapsed_display` → the `provisioning-elapsed`
/// ticker string as a `LocalizedText` `{ key, args }` the client resolves
/// through its i18n pipeline (`onboarding.nest_provisioning.elapsed_template` +
/// `{seconds}`). `None` until the run has started (hide the row); freezes at
/// `finished_at_ms`. Clients pass the snapshot's `started_at_ms`/`finished_at_ms`
/// plus their live tick `now_ms`. See `docs/goal/behavior/value-formatting.md`.
///
/// Gated behind `value-format` for the same reason `mod value_format` is (see
/// `lib.rs`): the `fauna_core::LocalizedText` return makes `uniffi-bindgen-go`
/// emit an uncompilable bare `fauna_core` import, so the Go mail-bridge's
/// `--no-default-features` binding build must drop it. The native apps
/// (default-features) keep it.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn provisioning_elapsed(
    started_at_ms: Option<u64>,
    finished_at_ms: Option<u64>,
    now_ms: u64,
) -> Option<fauna_core::localized::LocalizedText> {
    fauna_provisioning::progress::elapsed_display(started_at_ms, finished_at_ms, now_ms)
}

/// `fauna_provisioning::progress::status_glyph` → the canonical status-column
/// glyph for the `provisioning-step-checkbox` (`○`/`…`/`—`/`✓`/`✗`). A plain
/// `String` (a locale-invariant symbol), so unlike the two label exports below it
/// carries no `LocalizedText`; it rides the same `value-format` gate purely to
/// keep the whole provisioning step-display trio out of the Go mail-bridge
/// `--no-default-features` bindings — avoiding an off-Windows `fauna-mail-go`
/// regen this dev machine can't run (no Go). The native apps render the
/// step-row column through this instead of each re-deriving the glyph.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn provisioning_status_glyph(status: fauna_provisioning::progress::StepStatus) -> String {
    fauna_provisioning::progress::status_glyph(status)
}

/// `fauna_provisioning::progress::step_label` → the user-visible step name as a
/// `LocalizedText` `{ key }` (canonical `onboarding.provision.step.*`) the client
/// resolves through its own i18n pipeline. Gated for the same `LocalizedText` /
/// `uniffi-bindgen-go` reason as `provisioning_elapsed` above.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn provisioning_step_label(
    kind: fauna_provisioning::progress::ProvisionStep,
) -> fauna_core::localized::LocalizedText {
    fauna_provisioning::progress::step_label(kind)
}

/// `fauna_provisioning::progress::substep_label` → the sub-step text as a
/// `LocalizedText`. For `StatusRetrying` the `{cause}` arg is filled from `cause`
/// (the step's `last_error`) so the client substitutes it instead of showing a
/// literal `{cause}`. Gated for the same `LocalizedText` / `uniffi-bindgen-go`
/// reason as `provisioning_elapsed` above.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn provisioning_substep_label(
    key: fauna_provisioning::progress::SubstepKey,
    cause: Option<String>,
) -> fauna_core::localized::LocalizedText {
    fauna_provisioning::progress::substep_label(key, cause)
}

// The standalone provisioning FFI exports (`provision_nest`,
// `provision_nest_no_dns`, `provision_with_registration`,
// `provision_build_cloud_init`, `fetch_dkim`) were retired by the
// provisioning-progress design (tracked internally).
// Callers now go through `fauna_onboarding_machine::OnboardingMachine`,
// which owns a `ProvisioningSnapshot` (push-via-observer, pull-via-getter),
// generates DKIM keys client-side, and wires retry/cancel through a
// shared `CancelFlag`. Apple/Android FFI bindings will be regenerated as
// part of per-app wiring (spec stage 8).
