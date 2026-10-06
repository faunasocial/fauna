use super::{
    ContactInfo, DomainAvailability, Registrar, RegistrarAvailability, RegistrationResult,
};
use crate::error::{ProvisionError, ensure_success};
use crate::proxy::{BuildEnv, default_api_base, default_api_base_for_env};
use serde::Deserialize;

/// Direct Gandi v5 API base. Native builds hit this directly; wasm32
/// builds route through `proxy.fauna.social/gandi/v5` (or whatever
/// `FAUNA_PROXY_URL` was set to at compile time) — see `crate::proxy`.
const DIRECT_API: &str = "https://api.gandi.net/v5";
const PROXY_PREFIX: &str = "gandi/v5";

#[derive(Deserialize)]
struct CheckResponse {
    products: Vec<CheckProduct>,
}

#[derive(Deserialize)]
struct CheckProduct {
    status: String,
    currency: Option<String>,
    #[serde(default)]
    prices: Vec<CheckPrice>,
}

#[derive(Deserialize)]
struct CheckPrice {
    duration: u32,
    duration_unit: String,
    #[serde(rename = "type")]
    kind: String,
    price_after_taxes: f64,
}

/// Phase-1 response of `fetch_default_contact`'s two-call chain. Only `id` is
/// consumed — used as the path parameter for the organizations call below.
#[derive(Deserialize)]
struct GandiUserInfo {
    id: String,
}

/// Phase-2 response of `fetch_default_contact`. Field names mirror Gandi's API
/// (`given`/`family`/`streetaddr`/`zip`), matching the request body shape used
/// by `register()` above. `#[serde(default)]` on every field — missing keys
/// become empty strings so `ContactInfo` always materializes; the wizard's
/// form lets the user fill any blanks.
#[derive(Deserialize)]
struct GandiOrganization {
    #[serde(default)]
    given: String,
    #[serde(default)]
    family: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    phone: String,
    /// Gandi's `streetaddr` → `ContactInfo.address1`.
    #[serde(default)]
    streetaddr: String,
    #[serde(default)]
    city: String,
    #[serde(default)]
    state: String,
    /// Gandi's `zip` → `ContactInfo.postal_code`.
    #[serde(default)]
    zip: String,
    /// ISO 3166-1 alpha-2.
    #[serde(default)]
    country: String,
}

fn dollars_to_cents(d: f64) -> u64 {
    (d * 100.0).round() as u64
}

pub struct GandiRegistrar {
    token: String,
    api_base: String,
}

impl GandiRegistrar {
    pub fn new(token: String) -> Self {
        Self {
            token,
            api_base: default_api_base(DIRECT_API, PROXY_PREFIX),
        }
    }

    /// [`Self::new`] with the build env supplied explicitly — lets a native
    /// test assert the browser routing. See `tests/cors_policy_bijection.rs`.
    pub fn new_for_env(token: String, env: BuildEnv) -> Self {
        Self {
            token,
            api_base: default_api_base_for_env(DIRECT_API, PROXY_PREFIX, env),
        }
    }

    /// Test-only constructor that lets the conformance harness point the
    /// adapter at a wiremock server.
    pub fn with_base_url(token: String, api_base: String) -> Self {
        Self { token, api_base }
    }

    /// The API base this adapter will call.
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// Single-round-trip GET to /domain/check, returning the first product
    /// from the response. Shared by `check()` (which loses the raw status
    /// detail when collapsing into `DomainAvailability`) and `availability()`
    /// (which preserves it to distinguish `TldNotSupported` from
    /// `Unavailable`).
    async fn fetch_check_product(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<CheckProduct, ProvisionError> {
        let resp = client
            .get(format!("{}/domain/check", self.api_base))
            .query(&[("name", domain), ("processes", "create")])
            .bearer_auth(&self.token)
            .send()
            .await?;
        let resp = ensure_success(resp).await?;
        let parsed: CheckResponse = resp.json().await.map_err(ProvisionError::parse)?;
        parsed
            .products
            .into_iter()
            .next()
            .ok_or_else(|| ProvisionError::Parse("no products in check response".into()))
    }
}

impl Registrar for GandiRegistrar {
    async fn verify(&self, client: &reqwest::Client) -> Result<(), ProvisionError> {
        let resp = client
            .get(format!("{}/organization/user-info", self.api_base))
            .bearer_auth(&self.token)
            .send()
            .await?;
        ensure_success(resp).await?;
        Ok(())
    }

    async fn check(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<DomainAvailability, ProvisionError> {
        let product = self.fetch_check_product(client, domain).await?;
        // `available` and `available_premium` both mean buyable for our
        // purposes; everything else (including `available_reserved`,
        // `unavailable`, `unknown`, `error_*`) collapses to false.
        let available = matches!(product.status.as_str(), "available" | "available_premium");
        let price_first_year_cents = product
            .prices
            .iter()
            .find(|p| p.kind == "create" && p.duration == 1 && p.duration_unit == "y")
            .map(|p| dollars_to_cents(p.price_after_taxes));
        Ok(DomainAvailability {
            domain: domain.to_string(),
            available,
            price_first_year_cents,
            // Renewal price comes from a separate `processes=renew` call;
            // not fetched here to keep `check` to a single round-trip.
            price_renewal_cents: None,
            currency: product.currency,
        })
    }

    async fn availability(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<RegistrarAvailability, ProvisionError> {
        let product = self.fetch_check_product(client, domain).await?;
        match product.status.as_str() {
            "available" | "available_premium" => {
                let price_cents = product
                    .prices
                    .iter()
                    .find(|p| p.kind == "create" && p.duration == 1 && p.duration_unit == "y")
                    .map(|p| dollars_to_cents(p.price_after_taxes));
                match price_cents {
                    Some(cents) => Ok(RegistrarAvailability::Buyable {
                        price_cents: cents,
                        currency: product.currency,
                        renewal_cents: None,
                    }),
                    None => Ok(RegistrarAvailability::Unavailable),
                }
            }
            "unknown" => Ok(RegistrarAvailability::TldNotSupported),
            s if s.starts_with("error_") => Ok(RegistrarAvailability::TldNotSupported),
            // "unavailable", "available_reserved", and anything else
            // collapse to Unavailable — the wizard treats them as
            // "this domain isn't for sale here" rather than a TLD-coverage
            // problem.
            _ => Ok(RegistrarAvailability::Unavailable),
        }
    }

    async fn register(
        &self,
        client: &reqwest::Client,
        domain: &str,
        years: u32,
        _agreed_price_cents: u64,
        contact: Option<&ContactInfo>,
    ) -> Result<RegistrationResult, ProvisionError> {
        let contact = contact.ok_or_else(|| {
            ProvisionError::Other("Gandi requires WHOIS contact (requires_contact = true)".into())
        })?;
        let owner = serde_json::json!({
            "given": contact.first_name,
            "family": contact.last_name,
            "email": contact.email,
            "phone": contact.phone,
            "streetaddr": contact.address1,
            "city": contact.city,
            "state": contact.state,
            "zip": contact.postal_code,
            "country": contact.country,
            // Gandi's contact `type` enum: 0 = individual, 1 = company,
            // 2 = association, 3 = public body, 4 = reseller. The wizard
            // only collects individual contacts today.
            "type": 0,
        });
        let body = serde_json::json!({
            "fqdn": domain,
            "duration": years,
            "owner": owner,
        });
        let resp = client
            .post(format!("{}/domain/domains", self.api_base))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        ensure_success(resp).await?;
        // Gandi's create endpoint returns 202 with no nameservers — they're
        // assigned by LiveDNS at activation time. Caller fetches them via
        // the DNS verify step.
        Ok(RegistrationResult {
            domain: domain.to_string(),
            nameservers: Vec::new(),
        })
    }

    /// Two-call chain to prefill the wizard's WHOIS contact form from the
    /// user's Gandi account profile:
    ///   1. `GET /v5/organization/user-info` — returns the calling user's UUID.
    ///   2. `GET /v5/organization/organizations/{id}` — returns the user's
    ///      organization profile (name, address, phone, etc.).
    ///
    /// Best-effort: any failure (network error, non-2xx, parse error) maps to
    /// `Ok(None)` so the wizard's `verify_dns` step is never blocked by a
    /// prefill miss. Missing fields in the organizations response become empty
    /// strings (see `GandiOrganization`'s `#[serde(default)]`).
    async fn fetch_default_contact(
        &self,
        client: &reqwest::Client,
    ) -> Result<Option<ContactInfo>, ProvisionError> {
        // Phase 1: get the calling user's UUID.
        let user_resp = client
            .get(format!("{}/organization/user-info", self.api_base))
            .bearer_auth(&self.token)
            .send()
            .await;
        let user: GandiUserInfo = match user_resp {
            Ok(r) if r.status().is_success() => match r.json().await {
                Ok(u) => u,
                Err(_) => return Ok(None),
            },
            _ => return Ok(None),
        };

        // Phase 2: fetch the user's organization profile.
        let org_resp = client
            .get(format!(
                "{}/organization/organizations/{}",
                self.api_base, user.id
            ))
            .bearer_auth(&self.token)
            .send()
            .await;
        let org: GandiOrganization = match org_resp {
            Ok(r) if r.status().is_success() => match r.json().await {
                Ok(o) => o,
                Err(_) => return Ok(None),
            },
            _ => return Ok(None),
        };

        Ok(Some(ContactInfo {
            first_name: org.given,
            last_name: org.family,
            email: org.email,
            phone: org.phone,
            address1: org.streetaddr,
            city: org.city,
            state: org.state,
            postal_code: org.zip,
            country: org.country,
        }))
    }
}

#[cfg(test)]
mod tests {
    //! The hermetic conformance tests live in
    //! `libs/fauna-provisioning/tests/registrar_conformance.rs`. This module
    //! holds optional live-API tests that no-op unless their gating env var
    //! is set, so the default test run stays hermetic.

    use super::*;

    /// Live `verify` against Gandi's sandbox API. Only runs when
    /// `FAUNA_GANDI_SANDBOX_TOKEN` is set in the environment; absence is a
    /// pass (no-op). Catches API drift that the wiremock conformance tests
    /// can't see.
    ///
    /// Set up: create a sandbox PAT at <https://account.gandi.net/personal-access-tokens>
    /// (sandbox account is separate from production; sign up at
    /// <https://id.sandbox.gandi.net/>).
    #[tokio::test]
    async fn gandi_sandbox_verify() {
        let Ok(token) = std::env::var("FAUNA_GANDI_SANDBOX_TOKEN") else {
            return;
        };
        let gandi =
            GandiRegistrar::with_base_url(token, "https://api.sandbox.gandi.net/v5".to_string());
        let client = reqwest::Client::new();
        let result = gandi.verify(&client).await;
        assert!(result.is_ok(), "sandbox verify failed: {:?}", result);
    }
}
