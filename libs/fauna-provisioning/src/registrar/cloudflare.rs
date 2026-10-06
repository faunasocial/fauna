//! Cloudflare Registrar adapter.
//!
//! **Beta-risk judgment (resolved, don't re-derive):** Cloudflare's own docs
//! call this "the first beta release of the Registrar API" and disclaim
//! renewals, transfers, and contact updates, plus "only a subset of
//! supported Cloudflare Registrar extensions [TLDs]." That subset gap turns
//! out to be handled *structurally* by the API itself — `domain-check`
//! returns `registrable: false` with `reason: "extension_not_supported_via_api"`
//! for an unsupported TLD, which `availability()` below maps straight to
//! `RegistrarAvailability::TldNotSupported` (no static TLD list needed, and
//! a cleaner signal than the Namecheap/Gandi precedents, which infer
//! unsupported-TLD from an absent price or an `unknown`/`error_*` status).
//! The remaining beta gaps (no renewals/transfers/contact-update) are outside
//! this trait's surface (`Registrar` only registers and checks), so they are
//! accepted as-is: worth confirming against current docs again before a
//! future session builds renewal/transfer support on top of this file.
use super::{
    ContactInfo, DomainAvailability, Registrar, RegistrarAvailability, RegistrationResult,
};
use crate::error::{ProvisionError, ensure_success};
use crate::proxy::{BuildEnv, default_api_base, default_api_base_for_env};
use serde::Deserialize;

/// Direct Cloudflare v4 API base — same host + path prefix as the DNS
/// adapter (`dns/cloudflare.rs`); the registrar surface lives under
/// `/accounts/{account_id}/registrar/*` beneath it. Native builds hit this
/// directly; wasm32 builds route through `proxy.fauna.social/cloudflare/client/v4`
/// (the same CORS-proxy row DNS already registers — see `crate::proxy`).
const DIRECT_API: &str = "https://api.cloudflare.com/client/v4";
const PROXY_PREFIX: &str = "cloudflare/client/v4";

/// A domain Cloudflare will never sell (it's their own, permanently
/// registered) — used only by `verify()` as an inert `domain-check` probe.
/// Cloudflare's beta Registrar API exposes no dedicated account/credential
/// check endpoint, so this exercises exactly the auth + account_id +
/// Registrar-permission path `check()`/`register()` need, with no side
/// effects and a response that will always parse regardless of pricing.
const VERIFY_PROBE_DOMAIN: &str = "cloudflare.com";

pub struct CloudflareRegistrar {
    token: String,
    account_id: String,
    api_base: String,
}

#[derive(Deserialize)]
struct DomainCheckResponse {
    success: bool,
    #[serde(default)]
    errors: Vec<CloudflareApiError>,
    result: Option<DomainCheckResult>,
}

#[derive(Deserialize)]
struct DomainCheckResult {
    domains: Vec<CheckedDomain>,
}

#[derive(Deserialize)]
struct CheckedDomain {
    registrable: bool,
    #[serde(default)]
    pricing: Option<DomainPricing>,
    /// Present when `registrable == false`. `"extension_not_supported_via_api"`
    /// is the structural TldNotSupported signal (see module doc); any other
    /// value (e.g. `"domain_unavailable"`) collapses to `Unavailable`.
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
struct DomainPricing {
    currency: String,
    registration_cost: String,
    renewal_cost: String,
}

#[derive(Deserialize)]
struct CloudflareApiError {
    message: String,
}

#[derive(Deserialize)]
struct RegistrationResponse {
    success: bool,
    #[serde(default)]
    errors: Vec<CloudflareApiError>,
}

/// Parse a Cloudflare registrar decimal-money string ("8.57", "11.00") into
/// integer cents. Returns `None` on unparseable input.
fn parse_decimal_to_cents(s: &str) -> Option<u64> {
    crate::money::parse_decimal_to_cents(s)
}

impl CloudflareRegistrar {
    pub fn new(token: String, account_id: String) -> Self {
        Self {
            token,
            account_id,
            api_base: default_api_base(DIRECT_API, PROXY_PREFIX),
        }
    }

    /// [`Self::new`] with the build env supplied explicitly — lets a native
    /// test assert the browser routing. See `tests/cors_policy_bijection.rs`.
    pub fn new_for_env(token: String, account_id: String, env: BuildEnv) -> Self {
        Self {
            token,
            account_id,
            api_base: default_api_base_for_env(DIRECT_API, PROXY_PREFIX, env),
        }
    }

    /// Test-only constructor that lets the conformance harness point the
    /// adapter at a wiremock server.
    pub fn with_base_url(token: String, account_id: String, api_base: String) -> Self {
        Self {
            token,
            account_id,
            api_base,
        }
    }

    /// The API base this adapter will call.
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    fn registrar_url(&self, tail: &str) -> String {
        format!(
            "{}/accounts/{}/registrar/{}",
            self.api_base, self.account_id, tail
        )
    }

    /// Single-round-trip POST to `domain-check`, returning the (sole)
    /// checked domain. Shared by `check()` and `availability()`, mirroring
    /// Gandi's `fetch_check_product` split.
    async fn check_domain(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<CheckedDomain, ProvisionError> {
        let resp = client
            .post(self.registrar_url("domain-check"))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "domains": [domain] }))
            .send()
            .await?;
        let resp = ensure_success(resp).await?;
        let data: DomainCheckResponse = resp.json().await.map_err(ProvisionError::parse)?;
        if !data.success {
            let msg = data
                .errors
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(ProvisionError::Other(format!("Cloudflare error: {msg}")));
        }
        data.result
            .and_then(|r| r.domains.into_iter().next())
            .ok_or_else(|| ProvisionError::Parse("domain-check returned no domains".into()))
    }
}

impl Registrar for CloudflareRegistrar {
    async fn verify(&self, client: &reqwest::Client) -> Result<(), ProvisionError> {
        self.check_domain(client, VERIFY_PROBE_DOMAIN).await?;
        Ok(())
    }

    async fn check(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<DomainAvailability, ProvisionError> {
        let checked = self.check_domain(client, domain).await?;
        let (price_first_year_cents, price_renewal_cents, currency) = match &checked.pricing {
            Some(p) => (
                parse_decimal_to_cents(&p.registration_cost),
                parse_decimal_to_cents(&p.renewal_cost),
                Some(p.currency.clone()),
            ),
            None => (None, None, None),
        };
        Ok(DomainAvailability {
            domain: domain.to_string(),
            available: checked.registrable,
            price_first_year_cents,
            price_renewal_cents,
            currency,
        })
    }

    /// Overrides the default so an API-unsupported TLD reads as
    /// `TldNotSupported` rather than "already taken" — see the module doc
    /// for why this is a structural (not guessed) signal.
    async fn availability(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<RegistrarAvailability, ProvisionError> {
        let checked = self.check_domain(client, domain).await?;
        if checked.registrable {
            match checked
                .pricing
                .as_ref()
                .and_then(|p| parse_decimal_to_cents(&p.registration_cost))
            {
                Some(price_cents) => Ok(RegistrarAvailability::Buyable {
                    price_cents,
                    currency: checked.pricing.map(|p| p.currency),
                    renewal_cents: None,
                }),
                None => Ok(RegistrarAvailability::Unavailable),
            }
        } else if checked.reason.as_deref() == Some("extension_not_supported_via_api") {
            Ok(RegistrarAvailability::TldNotSupported)
        } else {
            Ok(RegistrarAvailability::Unavailable)
        }
    }

    async fn register(
        &self,
        client: &reqwest::Client,
        domain: &str,
        _years: u32, // Cloudflare's registrations endpoint has no duration field.
        _agreed_price_cents: u64, // ...nor a price-acknowledgement field.
        _contact: Option<&ContactInfo>, // Account-level contact only (requires_contact() = false).
    ) -> Result<RegistrationResult, ProvisionError> {
        let resp = client
            .post(self.registrar_url("registrations"))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "domain_name": domain }))
            .send()
            .await?;
        let resp = ensure_success(resp).await?;
        let data: RegistrationResponse = resp.json().await.map_err(ProvisionError::parse)?;
        if !data.success {
            let msg = data
                .errors
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(ProvisionError::Other(format!("Cloudflare error: {msg}")));
        }
        Ok(RegistrationResult {
            domain: domain.to_string(),
            // The registration response carries no nameservers field.
            // Cloudflare forces its own nameservers on any domain registered
            // through it, assigned per-zone (not a fixed pair like Porkbun's)
            // — the wizard's post-registration `dns_verify(cloudflare, ...)`
            // step discovers the zone, same as Gandi/Namecheap.
            nameservers: Vec::new(),
        })
    }

    fn requires_contact(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::parse_decimal_to_cents;

    #[test]
    fn parse_prices() {
        assert_eq!(parse_decimal_to_cents("8.57"), Some(857));
        assert_eq!(parse_decimal_to_cents("11.00"), Some(1100));
        assert_eq!(parse_decimal_to_cents("10"), Some(1000));
        assert_eq!(parse_decimal_to_cents(""), None);
        assert_eq!(parse_decimal_to_cents("abc"), None);
    }
}
