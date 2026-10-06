use super::{ContactInfo, DomainAvailability, Registrar, RegistrationResult, TldPriceQuote};
use crate::error::{ProvisionError, ensure_success};
use serde::Deserialize;

const BASE: &str = "https://api.porkbun.com/api/json/v3";

pub struct PorkbunRegistrar {
    api_key: String,
    secret_api_key: String,
    api_base: String,
}

impl PorkbunRegistrar {
    pub fn new(api_key: String, secret_api_key: String) -> Self {
        Self {
            api_key,
            secret_api_key,
            api_base: BASE.to_string(),
        }
    }

    /// Test-only: point the adapter at a wiremock server instead of the real
    /// Porkbun API. Mirrors `GandiRegistrar::with_base_url` / the `dispatch`
    /// `api-base` cred override.
    pub fn with_base_url(api_key: String, secret_api_key: String, api_base: String) -> Self {
        Self {
            api_key,
            secret_api_key,
            api_base,
        }
    }

    fn auth_body(&self) -> serde_json::Value {
        serde_json::json!({
            "apikey": self.api_key,
            "secretapikey": self.secret_api_key,
        })
    }
}

#[derive(Deserialize)]
struct PingResponse {
    status: String,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct CheckResponse {
    status: String,
    response: Option<CheckDetails>,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct CheckDetails {
    avail: String,         // "yes" | "no"
    price: Option<String>, // dollars string, e.g. "9.73"
    #[serde(rename = "regularPrice")]
    regular_price: Option<String>,
}

#[derive(Deserialize)]
struct CreateResponse {
    status: String,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct PricingResponse {
    pricing: std::collections::BTreeMap<String, PricingEntry>,
}

#[derive(Deserialize)]
struct PricingEntry {
    registration: String,
    renewal: String,
}

/// Parse a Porkbun USD dollars-and-cents string like "9.73" into 973 (integer
/// cents). Returns None on unparseable input.
fn parse_usd_to_cents(s: &str) -> Option<u64> {
    crate::money::parse_decimal_to_cents(s)
}

impl Registrar for PorkbunRegistrar {
    async fn verify(&self, client: &reqwest::Client) -> Result<(), ProvisionError> {
        let body = self.auth_body();
        let resp = client
            .post(format!("{}/ping", self.api_base))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let parsed: PingResponse = resp.json().await.map_err(ProvisionError::parse)?;
        // Porkbun returns HTTP 400 + {status: "ERROR", message: ...} on bad creds.
        if parsed.status != "SUCCESS" {
            return Err(ProvisionError::provider(status.as_u16(), parsed.message));
        }
        Ok(())
    }

    async fn check(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<DomainAvailability, ProvisionError> {
        let body = self.auth_body();
        let url = format!("{}/domain/checkDomain/{}", self.api_base, domain);
        let resp = client.post(&url).json(&body).send().await?;
        let status = resp.status();
        let parsed: CheckResponse = resp.json().await.map_err(ProvisionError::parse)?;
        if parsed.status != "SUCCESS" {
            return Err(ProvisionError::provider(status.as_u16(), parsed.message));
        }
        let details = parsed
            .response
            .ok_or_else(|| ProvisionError::Parse("missing response".into()))?;
        Ok(DomainAvailability {
            domain: domain.to_string(),
            available: details.avail == "yes",
            price_first_year_cents: details.price.as_deref().and_then(parse_usd_to_cents),
            price_renewal_cents: details
                .regular_price
                .as_deref()
                .and_then(parse_usd_to_cents),
            currency: Some("USD".into()),
        })
    }

    async fn register(
        &self,
        client: &reqwest::Client,
        domain: &str,
        _years: u32, // Porkbun's create endpoint doesn't accept a years field;
        // renewal/duration is account-default (minimum 1 yr).
        agreed_price_cents: u64,
        _contact: Option<&ContactInfo>, // Porkbun uses the account-level contact.
    ) -> Result<RegistrationResult, ProvisionError> {
        let body = serde_json::json!({
            "apikey": self.api_key,
            "secretapikey": self.secret_api_key,
            "cost": agreed_price_cents,
            "agreeToTerms": "yes",
        });
        let url = format!("{}/domain/create/{}", self.api_base, domain);
        let resp = client.post(&url).json(&body).send().await?;
        let status = resp.status();
        let parsed: CreateResponse = resp.json().await.map_err(ProvisionError::parse)?;
        if parsed.status != "SUCCESS" {
            return Err(ProvisionError::provider(status.as_u16(), parsed.message));
        }
        Ok(RegistrationResult {
            domain: domain.to_string(),
            nameservers: vec![
                "curitiba.ns.porkbun.com".into(),
                "fortaleza.ns.porkbun.com".into(),
                "maceio.ns.porkbun.com".into(),
                "salvador.ns.porkbun.com".into(),
            ],
        })
    }

    fn requires_contact(&self) -> bool {
        false
    }

    async fn list_tld_pricing(
        &self,
        client: &reqwest::Client,
    ) -> Result<Option<Vec<TldPriceQuote>>, ProvisionError> {
        // Public endpoint — no auth body needed. Porkbun returns
        // `{ status: "SUCCESS", pricing: { "com": { registration, renewal, ... }, ... } }`.
        let resp = client
            .post(format!("{}/pricing/get", self.api_base))
            .send()
            .await?;
        let resp = ensure_success(resp).await?;
        let data: PricingResponse = resp.json().await.map_err(ProvisionError::parse)?;
        let mut out = Vec::with_capacity(data.pricing.len());
        for (tld, entry) in data.pricing {
            // The pricing endpoint returns "0.00" for some TLDs the user can't
            // actually register (e.g. country-restricted ones); we keep them
            // since the UI filter is "is this TLD in my domain?" rather than
            // a generic browse.
            let reg = parse_usd_to_cents(&entry.registration).unwrap_or(0);
            let ren = parse_usd_to_cents(&entry.renewal).unwrap_or(0);
            out.push(TldPriceQuote {
                tld,
                registration_cents: reg,
                renewal_cents: ren,
                currency: "USD".to_string(),
            });
        }
        Ok(Some(out))
    }
}

#[cfg(test)]
mod tests {
    use super::parse_usd_to_cents;

    use super::PricingResponse;

    #[test]
    fn parses_pricing_response() {
        let json = r#"{
            "status": "SUCCESS",
            "pricing": {
                "com": { "registration": "9.13", "renewal": "9.13", "transfer": "9.13" },
                "net": { "registration": "10.79", "renewal": "10.79", "transfer": "10.79" }
            }
        }"#;
        let parsed: PricingResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.pricing.contains_key("com"));
        assert_eq!(parsed.pricing.get("net").unwrap().registration, "10.79");
    }

    #[test]
    fn parse_prices() {
        assert_eq!(parse_usd_to_cents("9.73"), Some(973));
        assert_eq!(parse_usd_to_cents("10"), Some(1000));
        assert_eq!(parse_usd_to_cents("9.7"), Some(970));
        assert_eq!(parse_usd_to_cents("100.00"), Some(10000));
        assert_eq!(parse_usd_to_cents(""), None);
        assert_eq!(parse_usd_to_cents("abc"), None);
    }
}
