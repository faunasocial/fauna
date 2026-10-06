use serde::Deserialize;

use crate::dns::{DnsProvider, DnsRecord, DnsZone};
use crate::error::{ProvisionError, ensure_success};

const API_BASE: &str = "https://api.porkbun.com/api/json/v3";

/// Porkbun's apex spelling is a **blank string**, not `@`: its API documents the
/// create `name` as the "Subdomain for the record (e.g. 'www', '*' for wildcard,
/// blank for root)" and `retrieveByNameType`'s `subdomain` as "Omit or leave
/// empty for root domain records". `@` is not an accepted spelling on either.
///
/// That makes Porkbun the one adapter whose apex sentinel differs from the shared
/// one, and translating it is the **only** transform this adapter applies to an
/// owner name. Everything else arrives ready to use: the seam relativizes before
/// an adapter sees it (Porkbun leaves `record_names_relative_to_zone` at its
/// `true` default), so a non-apex name goes to the wire verbatim on both the
/// write and read halves — see the contract note on that method.
///
/// A literal `@` went out on the wire until 2026-08-01, creating apex records at
/// `@.<zone>`, because the pre-relativized `@` never matched the old `name ==
/// zone_id` test this replaced.
fn porkbun_apex(name: &str) -> Option<String> {
    (name == "@").then(String::new)
}

/// The owner name Porkbun's endpoints want: the apex sentinel translated, every
/// other name untouched. Shared by the write and read halves so they cannot
/// drift apart (they did, until 2026-08-01: the read half also stripped a
/// `.<zone>` suffix, second-guessing a relative name the seam had already
/// produced — dead on every production path, and corrupting for a legitimate
/// owner that happens to end with the zone name).
fn porkbun_name(name: &str) -> String {
    porkbun_apex(name).unwrap_or_else(|| name.to_string())
}

/// Porkbun DNS provider.
pub struct Porkbun {
    pub apikey: String,
    pub secretapikey: String,
    api_base: String,
}

#[derive(Deserialize)]
struct DomainListResponse {
    status: String,
    #[serde(default)]
    domains: Vec<PorkbunDomain>,
}

#[derive(Deserialize)]
struct PorkbunDomain {
    domain: String,
}

#[derive(Deserialize)]
struct CreateRecordResponse {
    status: String,
}

impl Porkbun {
    pub fn new(apikey: String, secretapikey: String) -> Self {
        Self::with_base_url(apikey, secretapikey, API_BASE.to_string())
    }
    /// Test-only constructor that lets the conformance harness point the
    /// adapter at a wiremock server.
    pub fn with_base_url(apikey: String, secretapikey: String, api_base: String) -> Self {
        Self {
            apikey,
            secretapikey,
            api_base,
        }
    }
}

#[derive(Deserialize)]
struct RetrieveRecordsResponse {
    status: String,
    #[serde(default)]
    records: Vec<PorkbunExistingRecord>,
}

#[derive(Deserialize)]
struct PorkbunExistingRecord {
    /// Provider record id — only consumed by `delete_record` (Porkbun deletes
    /// by id); `find_records` ignores it.
    #[serde(default)]
    id: String,
    #[serde(rename = "type", default)]
    record_type: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    content: String,
    /// Porkbun returns ttl as a string ("600"). Parse to u32 with a default of 0.
    #[serde(default, deserialize_with = "deserialize_string_u32")]
    ttl: u32,
    /// Porkbun returns prio as a string or null. Convert to Option<u32>.
    #[serde(default, deserialize_with = "deserialize_string_opt_u32")]
    prio: Option<u32>,
}

fn deserialize_string_u32<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = Option::<String>::deserialize(deserializer)?.unwrap_or_default();
    Ok(s.parse().unwrap_or(0))
}

fn deserialize_string_opt_u32<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = Option::<String>::deserialize(deserializer)?;
    Ok(s.and_then(|v| v.parse().ok()))
}

impl DnsProvider for Porkbun {
    async fn verify(&self, client: &reqwest::Client) -> Result<Vec<DnsZone>, ProvisionError> {
        let body = serde_json::json!({
            "apikey": self.apikey,
            "secretapikey": self.secretapikey,
        });

        let resp = client
            .post(format!("{}/domain/listAll", self.api_base))
            .json(&body)
            .send()
            .await?;

        let resp = ensure_success(resp).await?;

        let data: DomainListResponse = resp.json().await?;
        if data.status != "SUCCESS" {
            return Err(ProvisionError::Other(format!(
                "Porkbun error: status={}",
                data.status
            )));
        }

        Ok(data
            .domains
            .into_iter()
            .map(|d| DnsZone {
                id: d.domain.clone(),
                name: d.domain,
            })
            .collect())
    }

    async fn create_record(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        record: &DnsRecord,
    ) -> Result<(), ProvisionError> {
        let mut body = serde_json::json!({
            "apikey": self.apikey,
            "secretapikey": self.secretapikey,
            "type": record.record_type,
            "name": porkbun_name(&record.name),
            "content": record.value,
            "ttl": record.ttl.to_string(),
        });

        if let Some(priority) = record.priority {
            body["prio"] = serde_json::json!(priority.to_string());
        }

        let resp = client
            .post(format!("{}/dns/create/{zone_id}", self.api_base))
            .json(&body)
            .send()
            .await?;

        let resp = ensure_success(resp).await?;

        let data: CreateRecordResponse = resp.json().await?;
        if data.status != "SUCCESS" {
            return Err(ProvisionError::Other(format!(
                "Porkbun error: status={}",
                data.status
            )));
        }

        Ok(())
    }

    async fn find_records(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        name: &str,
        record_type: &str,
    ) -> Result<Vec<DnsRecord>, ProvisionError> {
        // Porkbun's typed-by-name endpoint: /dns/retrieveByNameType/{domain}/{type}/{subdomain}.
        // `name` arrives already zone-relative, so an apex arrives as `@`. A blank
        // subdomain drops the URL segment, which is how Porkbun addresses the root.
        let subdomain = porkbun_name(name);

        let body = serde_json::json!({
            "apikey": self.apikey,
            "secretapikey": self.secretapikey,
        });

        let url = if subdomain.is_empty() {
            format!(
                "{}/dns/retrieveByNameType/{zone_id}/{record_type}",
                self.api_base
            )
        } else {
            format!(
                "{}/dns/retrieveByNameType/{zone_id}/{record_type}/{subdomain}",
                self.api_base
            )
        };

        let resp = client.post(&url).json(&body).send().await?;
        let resp = ensure_success(resp).await?;

        let data: RetrieveRecordsResponse = resp.json().await?;
        if data.status != "SUCCESS" {
            return Err(ProvisionError::Other(format!(
                "Porkbun error: status={}",
                data.status
            )));
        }

        Ok(data
            .records
            .into_iter()
            .map(|r| DnsRecord {
                record_type: r.record_type,
                name: r.name,
                value: r.content,
                ttl: r.ttl,
                priority: r.prio,
            })
            .collect())
    }

    async fn delete_record(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        name: &str,
        record_type: &str,
        value: &str,
    ) -> Result<(), ProvisionError> {
        // Resolve the provider record id via retrieveByNameType (same endpoint
        // `find_records` uses), match on content, then delete by id — Porkbun
        // has no value-scoped delete.
        let subdomain = porkbun_name(name);

        let auth = serde_json::json!({
            "apikey": self.apikey,
            "secretapikey": self.secretapikey,
        });

        let retrieve_url = if subdomain.is_empty() {
            format!(
                "{}/dns/retrieveByNameType/{zone_id}/{record_type}",
                self.api_base
            )
        } else {
            format!(
                "{}/dns/retrieveByNameType/{zone_id}/{record_type}/{subdomain}",
                self.api_base
            )
        };

        let resp = client.post(&retrieve_url).json(&auth).send().await?;
        let resp = ensure_success(resp).await?;
        let data: RetrieveRecordsResponse = resp.json().await?;
        if data.status != "SUCCESS" {
            return Err(ProvisionError::Other(format!(
                "Porkbun error: status={}",
                data.status
            )));
        }

        // Idempotent: nothing matching this value means it's already gone.
        let Some(rec) = data.records.into_iter().find(|r| r.content == value) else {
            return Ok(());
        };

        let del = client
            .post(format!("{}/dns/delete/{zone_id}/{}", self.api_base, rec.id))
            .json(&auth)
            .send()
            .await?;
        let del = ensure_success(del).await?;
        let data: CreateRecordResponse = del.json().await?;
        if data.status != "SUCCESS" {
            return Err(ProvisionError::Other(format!(
                "Porkbun error: status={}",
                data.status
            )));
        }

        Ok(())
    }

    fn base_url(&self) -> &str {
        &self.api_base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_porkbun_parses_domain_list() {
        let json = r#"{
            "status": "SUCCESS",
            "domains": [
                {"domain": "example.com"},
                {"domain": "another.org"}
            ]
        }"#;
        let parsed: DomainListResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.status, "SUCCESS");
        assert_eq!(parsed.domains.len(), 2);
        assert_eq!(parsed.domains[0].domain, "example.com");
    }

    #[test]
    fn test_porkbun_parses_create_response() {
        let json = r#"{"status": "SUCCESS"}"#;
        let parsed: CreateRecordResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.status, "SUCCESS");
    }
}
