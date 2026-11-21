//! Spaceship as a registrar (`docs/goal/architecture/provisioning/registry.md`
//! § Delegation doors, the Spaceship paragraph).
//!
//! - `verify` — `GET /domains?take=1&skip=0`: proves the key pair is accepted.
//! - `check` / `availability` — `GET /domains/{domain}/available` (its
//!   `result` tells *taken* from *TLD not sold* structurally), then for an
//!   available name `GET /domains/{domain}/pricing` for the one-year register
//!   and renew prices.
//! - `register` — Spaceship references contacts by id, so the wizard's contact
//!   is saved first (`PUT /contacts` → `contactId`) and named under every role;
//!   the create (`POST /domains/{domain}`) is answered `202` with an async
//!   operation (header `spaceship-async-operationid`), polled through
//!   `GET /async-operations/{id}` to `success` or `failed` inside the call,
//!   bounded.
//! - `set_nameservers` — the nameserver door, `PUT /domains/{domain}/nameservers`
//!   with `provider: custom` and 2 to 12 `hosts`.

use std::time::Duration;

use serde::Deserialize;

use super::{
    ContactInfo, DomainAvailability, Registrar, RegistrarAvailability, RegistrationResult,
    normalize_epp_phone,
};
use crate::error::{ProvisionError, ensure_success};
use crate::proxy::BuildEnv;
use crate::spaceship_api::SpaceshipApi;

/// Spaceship's bounds on a custom delegation (`hosts`: 2..12 items).
pub const MIN_NAMESERVERS: usize = 2;
pub const MAX_NAMESERVERS: usize = 12;

/// How the registration's async operation is waited for: one status read per
/// `interval`, at most `attempts` reads. Hard-coded — nobody chooses it; the
/// default is three minutes, which a domain create normally finishes well
/// inside, and the timeout error tells the user the create may still land.
#[derive(Debug, Clone, Copy)]
pub struct PollPolicy {
    pub interval: Duration,
    pub attempts: u32,
}

impl Default for PollPolicy {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(2),
            attempts: 90,
        }
    }
}

pub struct SpaceshipRegistrar {
    api: SpaceshipApi,
    poll: PollPolicy,
}

impl SpaceshipRegistrar {
    pub fn new(api_key: String, api_secret: String) -> Self {
        Self::from_api(SpaceshipApi::new(api_key, api_secret))
    }

    /// [`Self::new`] with the build env supplied explicitly — lets a native
    /// test assert the browser routing (`tests/cors_policy_bijection.rs`).
    pub fn new_for_env(api_key: String, api_secret: String, env: BuildEnv) -> Self {
        Self::from_api(SpaceshipApi::new_for_env(api_key, api_secret, env))
    }

    /// Point the adapter at a wiremock server or the e2e fake.
    pub fn with_base_url(api_key: String, api_secret: String, api_base: String) -> Self {
        Self::from_api(SpaceshipApi::with_base_url(api_key, api_secret, api_base))
    }

    fn from_api(api: SpaceshipApi) -> Self {
        Self {
            api,
            poll: PollPolicy::default(),
        }
    }

    /// Replace the async-operation wait (the conformance tests poll with no
    /// interval).
    pub fn with_poll(mut self, poll: PollPolicy) -> Self {
        self.poll = poll;
        self
    }

    /// The API base this adapter calls.
    pub fn api_base(&self) -> &str {
        self.api.api_base()
    }

    async fn availability_result(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<String, ProvisionError> {
        let resp = self
            .api
            .request(
                client,
                reqwest::Method::GET,
                &format!("/domains/{domain}/available"),
            )
            .send()
            .await?;
        let body: AvailableResponse = ensure_success(resp)
            .await?
            .json()
            .await
            .map_err(ProvisionError::parse)?;
        Ok(body.result)
    }

    /// The one-year register and renew prices, in integer cents.
    async fn one_year_prices(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<OneYearPrices, ProvisionError> {
        let resp = self
            .api
            .request(
                client,
                reqwest::Method::GET,
                &format!("/domains/{domain}/pricing"),
            )
            .send()
            .await?;
        let body: PricingResponse = ensure_success(resp)
            .await?
            .json()
            .await
            .map_err(ProvisionError::parse)?;
        let register = one_year(&body.prices.register);
        let renew = one_year(&body.prices.renew);
        Ok(OneYearPrices {
            register_cents: register.as_ref().map(|p| p.0),
            renew_cents: renew.as_ref().map(|p| p.0),
            currency: register.or(renew).map(|p| p.1),
        })
    }

    /// Save the wizard's contact as a Spaceship contact and answer its id.
    async fn save_contact(
        &self,
        client: &reqwest::Client,
        contact: &ContactInfo,
    ) -> Result<String, ProvisionError> {
        let resp = self
            .api
            .request(client, reqwest::Method::PUT, "/contacts")
            .json(&contact_body(contact)?)
            .send()
            .await?;
        let saved: SavedContact = ensure_success(resp)
            .await?
            .json()
            .await
            .map_err(ProvisionError::parse)?;
        Ok(saved.contact_id)
    }

    /// Wait for an async operation to end; `Ok` on `success`.
    async fn await_operation(
        &self,
        client: &reqwest::Client,
        domain: &str,
        operation_id: &str,
    ) -> Result<(), ProvisionError> {
        for attempt in 0..self.poll.attempts {
            if attempt > 0 && !self.poll.interval.is_zero() {
                fauna_sleep::sleep(self.poll.interval).await;
            }
            let resp = self
                .api
                .request(
                    client,
                    reqwest::Method::GET,
                    &format!("/async-operations/{operation_id}"),
                )
                .send()
                .await?;
            let op: AsyncOperation = ensure_success(resp)
                .await?
                .json()
                .await
                .map_err(ProvisionError::parse)?;
            match op.status.as_str() {
                "success" => return Ok(()),
                "failed" => {
                    return Err(ProvisionError::Other(format!(
                        "Spaceship could not register {domain}: {}",
                        failure_details(op.details.as_ref())
                    )));
                }
                // `pending` — and any status the reference does not list —
                // keeps waiting until the bound.
                _ => {}
            }
        }
        let waited = self.poll.interval.as_secs() * u64::from(self.poll.attempts);
        Err(ProvisionError::Other(format!(
            "Spaceship had not finished registering {domain} after about {waited} s \
             (operation {operation_id}). It may still complete: check the domain on your \
             Spaceship account before trying again."
        )))
    }
}

/// The Spaceship contact body for the wizard's single contact. Empty optional
/// fields are left out rather than sent blank (the API validates what is sent).
fn contact_body(contact: &ContactInfo) -> Result<serde_json::Value, ProvisionError> {
    let phone = normalize_epp_phone("Spaceship", &contact.phone)?;
    let mut body = serde_json::json!({
        "firstName": contact.first_name,
        "lastName": contact.last_name,
        "email": contact.email,
        "address1": contact.address1,
        "city": contact.city,
        "country": contact.country.to_ascii_uppercase(),
        "phone": phone,
    });
    if !contact.state.trim().is_empty() {
        body["stateProvince"] = serde_json::json!(contact.state);
    }
    if !contact.postal_code.trim().is_empty() {
        body["postalCode"] = serde_json::json!(contact.postal_code);
    }
    Ok(body)
}

/// The create body. Every role names the one saved contact (the wizard collects
/// one). WHOIS privacy is on (`level: high`): a hard-coded constant, as
/// Namecheap's is — no deployment wants the user's home address published, and a
/// real choice would be app UI, never a flag. `autoRenew` is off: the price the
/// user confirmed authorizes this one charge, and renewal stays their act at
/// Spaceship, which the domain-expiry alert warns them of
/// (`registry.md` § Registrar trait shape and why).
fn create_body(years: u32, contact_id: &str) -> serde_json::Value {
    serde_json::json!({
        "autoRenew": false,
        "years": years.clamp(1, 10),
        "privacyProtection": { "level": "high", "userConsent": false },
        "contacts": {
            "registrant": contact_id,
            "admin": contact_id,
            "tech": contact_id,
            "billing": contact_id,
        },
    })
}

/// A failed operation's `details` object, compacted for the error message.
/// It is provider text: shown to the user as data, never acted on, and capped.
fn failure_details(details: Option<&serde_json::Value>) -> String {
    const CAP: usize = 300;
    let text = match details {
        None | Some(serde_json::Value::Null) => return "no reason given".to_string(),
        Some(v) => v.to_string(),
    };
    if text.chars().count() <= CAP {
        text
    } else {
        format!("{}…", text.chars().take(CAP).collect::<String>())
    }
}

/// The one-year entry of a price list, as (cents, currency).
fn one_year(entries: &[PriceEntry]) -> Option<(u64, String)> {
    let e = entries
        .iter()
        .find(|e| e.period.value == 1 && e.period.unit.eq_ignore_ascii_case("y"))?;
    let price = &e.price_in_default_currency.final_price;
    Some((
        crate::money::parse_decimal_to_cents(&price.amount)?,
        price.currency.clone(),
    ))
}

struct OneYearPrices {
    register_cents: Option<u64>,
    renew_cents: Option<u64>,
    currency: Option<String>,
}

#[derive(Deserialize)]
struct AvailableResponse {
    /// `available` | `taken` | `invalidDomainName` | `tldNotSupported` |
    /// `unexpectedError`.
    result: String,
}

#[derive(Deserialize)]
struct PricingResponse {
    prices: PriceTable,
}

#[derive(Deserialize)]
struct PriceTable {
    #[serde(default)]
    register: Vec<PriceEntry>,
    #[serde(default)]
    renew: Vec<PriceEntry>,
}

#[derive(Deserialize)]
struct PriceEntry {
    period: Period,
    #[serde(rename = "priceInDefaultCurrency")]
    price_in_default_currency: PriceInCurrency,
}

#[derive(Deserialize)]
struct Period {
    value: u32,
    unit: String,
}

#[derive(Deserialize)]
struct PriceInCurrency {
    /// What is charged (promotions applied); `regularPrice` is the list price.
    #[serde(rename = "finalPrice")]
    final_price: Money,
}

#[derive(Deserialize)]
struct Money {
    amount: String,
    currency: String,
}

#[derive(Deserialize)]
struct SavedContact {
    #[serde(rename = "contactId")]
    contact_id: String,
}

#[derive(Deserialize)]
struct AsyncOperation {
    status: String,
    #[serde(default)]
    details: Option<serde_json::Value>,
}

impl Registrar for SpaceshipRegistrar {
    async fn verify(&self, client: &reqwest::Client) -> Result<(), ProvisionError> {
        let resp = self
            .api
            .request(client, reqwest::Method::GET, "/domains?take=1&skip=0")
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
        let available = self.availability_result(client, domain).await? == "available";
        let prices = if available {
            Some(self.one_year_prices(client, domain).await?)
        } else {
            None
        };
        Ok(DomainAvailability {
            domain: domain.to_string(),
            available,
            price_first_year_cents: prices.as_ref().and_then(|p| p.register_cents),
            price_renewal_cents: prices.as_ref().and_then(|p| p.renew_cents),
            currency: prices.and_then(|p| p.currency),
        })
    }

    async fn availability(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<RegistrarAvailability, ProvisionError> {
        match self.availability_result(client, domain).await?.as_str() {
            "available" => {
                let prices = self.one_year_prices(client, domain).await?;
                Ok(match prices.register_cents {
                    Some(price_cents) => RegistrarAvailability::Buyable {
                        price_cents,
                        currency: prices.currency,
                        renewal_cents: prices.renew_cents,
                    },
                    // Available but not quotable for one year: nothing the
                    // wizard can put a confirmed price on.
                    None => RegistrarAvailability::Unavailable,
                })
            }
            "tldNotSupported" => Ok(RegistrarAvailability::TldNotSupported),
            "taken" => Ok(RegistrarAvailability::Unavailable),
            "invalidDomainName" => Err(ProvisionError::Other(format!(
                "Spaceship says {domain} is not a valid domain name"
            ))),
            other => Err(ProvisionError::Other(format!(
                "Spaceship could not determine whether {domain} is available ({other})"
            ))),
        }
    }

    async fn register(
        &self,
        client: &reqwest::Client,
        domain: &str,
        years: u32,
        agreed_price_cents: u64,
        contact: Option<&ContactInfo>,
    ) -> Result<RegistrationResult, ProvisionError> {
        let contact = contact.ok_or_else(|| {
            ProvisionError::Other(
                "Spaceship requires a WHOIS contact (requires_contact = true)".into(),
            )
        })?;
        // Spaceship's create carries no price acknowledgement (Porkbun's `cost`
        // does), so the price is read again here and the create refused if it
        // rose above what the user confirmed — the race `agreed_price_cents`
        // exists to close (`registry.md` § Registrar trait shape and why).
        let prices = self.one_year_prices(client, domain).await?;
        match prices.register_cents {
            Some(now) if now <= agreed_price_cents => {}
            Some(now) => {
                return Err(ProvisionError::Other(format!(
                    "Spaceship's price for {domain} is now {} {} for the first year, above \
                     the {} you confirmed. Nothing was charged; check the price again.",
                    crate::money::format_cents(now),
                    prices.currency.as_deref().unwrap_or(""),
                    crate::money::format_cents(agreed_price_cents),
                )));
            }
            None => {
                return Err(ProvisionError::Other(format!(
                    "Spaceship no longer quotes a one-year registration of {domain}. Nothing \
                     was charged."
                )));
            }
        }

        let contact_id = self.save_contact(client, contact).await?;
        let resp = self
            .api
            .request(client, reqwest::Method::POST, &format!("/domains/{domain}"))
            .json(&create_body(years, &contact_id))
            .send()
            .await?;
        let resp = ensure_success(resp).await?;
        let operation_id = resp
            .headers()
            .get("spaceship-async-operationid")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .ok_or_else(|| {
                ProvisionError::Parse(format!(
                    "Spaceship accepted the registration of {domain} (HTTP {}) without an \
                     operation id to follow",
                    resp.status().as_u16()
                ))
            })?;
        self.await_operation(client, domain, &operation_id).await?;

        Ok(RegistrationResult {
            domain: domain.to_string(),
            // The create reports no nameservers: a fresh registration lands on
            // Spaceship's own DNS, and the wizard discovers the zone through
            // the DNS verify that follows (`registry.md` § Modes → *Why
            // `purchase` doesn't show a separate DNS step`) — as Namecheap's.
            nameservers: Vec::new(),
        })
    }

    /// `PUT /domains/{domain}/nameservers` with `provider: custom` — the
    /// nameserver door (`registry.md` § Delegation doors). Spaceship takes 2 to
    /// 12 hosts; a list outside that is refused here, before any request.
    async fn set_nameservers(
        &self,
        client: &reqwest::Client,
        domain: &str,
        nameservers: &[String],
    ) -> Result<(), ProvisionError> {
        let n = nameservers.len();
        if !(MIN_NAMESERVERS..=MAX_NAMESERVERS).contains(&n) {
            return Err(ProvisionError::Other(format!(
                "Spaceship needs between {MIN_NAMESERVERS} and {MAX_NAMESERVERS} nameservers \
                 for {domain}; {n} given"
            )));
        }
        let hosts: Vec<&str> = nameservers
            .iter()
            .map(|h| h.trim().trim_end_matches('.'))
            .collect();
        let resp = self
            .api
            .request(
                client,
                reqwest::Method::PUT,
                &format!("/domains/{domain}/nameservers"),
            )
            .json(&serde_json::json!({ "provider": "custom", "hosts": hosts }))
            .send()
            .await?;
        ensure_success(resp).await?;
        Ok(())
    }

    /// Spaceship references contacts by id and the API exposes no account
    /// default, so the wizard collects the contact and the adapter saves it.
    fn requires_contact(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact() -> ContactInfo {
        ContactInfo {
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            email: "ada@example.com".into(),
            phone: "+44 20 7946 0958".into(),
            address1: "1 Example St".into(),
            city: "London".into(),
            state: String::new(),
            postal_code: "N1 1AA".into(),
            country: "gb".into(),
        }
    }

    #[test]
    fn contact_body_normalises_the_phone_and_omits_blank_optionals() {
        let body = contact_body(&contact()).unwrap();
        assert_eq!(body["phone"], "+44.2079460958");
        assert_eq!(body["country"], "GB");
        assert_eq!(body["postalCode"], "N1 1AA");
        assert!(body.get("stateProvince").is_none());
    }

    #[test]
    fn contact_body_refuses_a_phone_it_would_have_to_guess_at() {
        let mut c = contact();
        c.phone = "2079460958".into();
        let err = contact_body(&c).unwrap_err().to_string();
        assert!(
            err.contains("Spaceship") && err.contains("+1.5551234567"),
            "{err}"
        );
    }

    #[test]
    fn create_body_names_one_contact_under_every_role_with_privacy_on() {
        let body = create_body(1, "C1");
        for role in ["registrant", "admin", "tech", "billing"] {
            assert_eq!(body["contacts"][role], "C1");
        }
        assert_eq!(body["privacyProtection"]["level"], "high");
        assert_eq!(body["autoRenew"], false);
        assert_eq!(body["years"], 1);
    }

    #[test]
    fn failure_details_are_capped() {
        let long = serde_json::json!({ "reason": "x".repeat(1000) });
        assert!(failure_details(Some(&long)).chars().count() <= 301);
        assert_eq!(failure_details(None), "no reason given");
    }
}
