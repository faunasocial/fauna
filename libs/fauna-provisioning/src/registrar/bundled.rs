//! Registrar half of the generic bundled provider — the open Fauna Bundled
//! Provider API v1 (`docs/goal/architecture/provisioning/bundled-provider-api.md`
//! § Endpoints). The registrant is always the **user** (spec § Exit), so
//! `requires_contact()` is `true` and the wizard collects the WHOIS contact;
//! `auth_code()` is the mandatory transfer-out door the trait has no slot for.

use serde::Deserialize;

use super::{
    ContactInfo, DomainAvailability, Registrar, RegistrarAvailability, RegistrationResult,
    TldPriceQuote,
};
use crate::bundled_api::{BundledApi, error_code};
use crate::error::{ProvisionError, ensure_success};

pub struct BundledRegistrar {
    api: BundledApi,
}

/// `GET /v1/domains/check` (spec § Shapes → `Availability`).
#[derive(Deserialize)]
struct CheckResponse {
    #[serde(default)]
    name: String,
    status: String,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    registration_cents: Option<u64>,
    #[serde(default)]
    renewal_cents: Option<u64>,
}

#[derive(Deserialize)]
struct RegisterResponse {
    #[serde(default)]
    name: String,
    #[serde(default)]
    nameservers: Vec<String>,
}

#[derive(Deserialize)]
struct PricingResponse {
    currency: String,
    #[serde(default)]
    tlds: Vec<TldJson>,
}

#[derive(Deserialize)]
struct TldJson {
    tld: String,
    registration_cents: u64,
    renewal_cents: u64,
}

#[derive(Deserialize)]
struct AuthCodeResponse {
    #[serde(default)]
    auth_code: Option<String>,
    #[serde(default)]
    available_after: Option<String>,
}

/// `GET /v1/domains/{name}/auth-code` outcome (spec § Exit, guarantee 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthCode {
    /// The transfer authorization code, ready to hand to the gaining registrar.
    Ready(String),
    /// A registry-imposed transfer lock still applies; the code becomes
    /// available at this RFC 3339 instant (`202 available_after`).
    AvailableAfter(String),
}

impl BundledRegistrar {
    pub fn new(base_url: String, token: String) -> Self {
        Self {
            api: BundledApi::new(base_url, token),
        }
    }

    pub fn base_url(&self) -> &str {
        self.api.base_url()
    }

    /// The exit door: the domain's transfer authorization code on demand. Not
    /// a `Registrar` trait method (no other registrar adapter exposes one —
    /// the trait is the wizard's surface, this is the leaving-user's), but
    /// mandatory for a conformant intermediary and pinned by the conformance
    /// suite.
    pub async fn auth_code(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<AuthCode, ProvisionError> {
        let resp = self
            .api
            .get(client, &format!("/domains/{domain}/auth-code"), &[])
            .await?;
        let status = resp.status().as_u16();
        let body: AuthCodeResponse = resp.json().await?;
        match (status, body.auth_code, body.available_after) {
            (_, Some(code), _) if !code.is_empty() => Ok(AuthCode::Ready(code)),
            (202, None, Some(when)) => Ok(AuthCode::AvailableAfter(when)),
            _ => Err(ProvisionError::parse(
                "auth-code response carries neither auth_code nor available_after",
            )),
        }
    }

    async fn check_response(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<CheckResponse, ProvisionError> {
        let resp = self
            .api
            .get(client, "/domains/check", &[("name", domain)])
            .await?;
        Ok(resp.json().await?)
    }
}

impl Registrar for BundledRegistrar {
    async fn verify(&self, client: &reqwest::Client) -> Result<(), ProvisionError> {
        self.api.me(client).await.map(|_| ())
    }

    async fn check(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<DomainAvailability, ProvisionError> {
        let c = self.check_response(client, domain).await?;
        let available = c.status == "available";
        Ok(DomainAvailability {
            domain: if c.name.is_empty() {
                domain.to_string()
            } else {
                c.name
            },
            available,
            price_first_year_cents: if available {
                c.registration_cents
            } else {
                None
            },
            price_renewal_cents: if available { c.renewal_cents } else { None },
            currency: c.currency,
        })
    }

    async fn availability(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<RegistrarAvailability, ProvisionError> {
        let c = self.check_response(client, domain).await?;
        Ok(match (c.status.as_str(), c.registration_cents) {
            ("available", Some(price_cents)) => RegistrarAvailability::Buyable {
                price_cents,
                currency: c.currency,
                renewal_cents: c.renewal_cents,
            },
            // Available but unquoted: nothing confirmable to show — the same
            // fold the trait's default `availability()` applies.
            ("available", None) => RegistrarAvailability::Unavailable,
            ("tld_not_supported", _) => RegistrarAvailability::TldNotSupported,
            _ => RegistrarAvailability::Unavailable,
        })
    }

    async fn register(
        &self,
        client: &reqwest::Client,
        domain: &str,
        years: u32,
        agreed_price_cents: u64,
        contact: Option<&ContactInfo>,
    ) -> Result<RegistrationResult, ProvisionError> {
        // Spec § Exit guarantee 1: the registrant is the user, so a
        // registration with no contact is a registration in the wrong name.
        let contact = contact.ok_or_else(|| {
            ProvisionError::Other("a bundled provider registers the domain in the user's name — a registrant contact is required".into())
        })?;
        let body = serde_json::json!({
            "name": domain,
            "years": years,
            "agreed_price_cents": agreed_price_cents,
            "contact": contact,
            // A hard-coded constant, never a knob (the Namecheap rule): there
            // is no deployment in which publishing the user's home address is
            // the wanted behaviour.
            "whois_privacy": true,
        });
        let resp = client
            .post(self.api.url("/domains"))
            .bearer_auth_for(&self.api)
            .json(&body)
            .send()
            .await?;
        if resp.status().as_u16() == 409 {
            // `price_changed`: never silently re-quote — the user confirmed a
            // specific price (registry.md § Registrar trait shape).
            let body = resp.text().await.unwrap_or_default();
            let code = error_code(&body).unwrap_or_default();
            return Err(ProvisionError::provider(409, format!("{code}: {body}")));
        }
        let resp = ensure_success(resp).await?;
        let r: RegisterResponse = resp.json().await?;
        Ok(RegistrationResult {
            domain: if r.name.is_empty() {
                domain.to_string()
            } else {
                r.name
            },
            nameservers: r.nameservers,
        })
    }

    fn requires_contact(&self) -> bool {
        true
    }

    async fn fetch_default_contact(
        &self,
        client: &reqwest::Client,
    ) -> Result<Option<ContactInfo>, ProvisionError> {
        let resp = client
            .get(self.api.url("/contact"))
            .bearer_auth_for(&self.api)
            .send()
            .await?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        let resp = ensure_success(resp).await?;
        Ok(Some(resp.json().await?))
    }

    async fn list_tld_pricing(
        &self,
        client: &reqwest::Client,
    ) -> Result<Option<Vec<TldPriceQuote>>, ProvisionError> {
        // Unauthenticated by spec: it quotes before the user has signed in.
        let resp = client.get(self.api.url("/pricing/tlds")).send().await?;
        let resp = ensure_success(resp).await?;
        let p: PricingResponse = resp.json().await?;
        Ok(Some(
            p.tlds
                .into_iter()
                .map(|t| TldPriceQuote {
                    tld: t.tld.trim_start_matches('.').to_ascii_lowercase(),
                    registration_cents: t.registration_cents,
                    renewal_cents: t.renewal_cents,
                    currency: p.currency.clone(),
                })
                .collect(),
        ))
    }
}

/// `RequestBuilder::bearer_auth` with the token held privately by the shared
/// transport — so this adapter's bespoke calls (`409` inspection, the `404`
/// contact miss) carry the same header the generic helpers add.
trait BearerFor {
    fn bearer_auth_for(self, api: &BundledApi) -> Self;
}

impl BearerFor for reqwest::RequestBuilder {
    fn bearer_auth_for(self, api: &BundledApi) -> Self {
        api.apply_bearer(self)
    }
}
