use serde::Deserialize;

use crate::dns::{DnsProvider, DnsRecord, DnsZone};
use crate::error::{ProvisionError, ensure_success};
use crate::proxy::{BuildEnv, default_api_base, default_api_base_for_env};

/// Direct Gandi v5 API base. Native builds hit this directly; wasm32
/// builds route through the CORS proxy — see `crate::proxy`.
const DIRECT_API: &str = "https://api.gandi.net/v5";
const PROXY_PREFIX: &str = "gandi/v5";

/// Gandi DNS provider.
pub struct Gandi {
    pub token: String,
    api_base: String,
}

#[derive(Deserialize)]
struct GandiDomain {
    fqdn: String,
    id: String,
}

#[derive(Deserialize)]
struct GandiRecord {
    #[serde(default)]
    rrset_name: String,
    #[serde(default)]
    rrset_type: String,
    #[serde(default)]
    rrset_values: Vec<String>,
    #[serde(default)]
    rrset_ttl: u32,
}

impl Gandi {
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
}

impl DnsProvider for Gandi {
    async fn verify(&self, client: &reqwest::Client) -> Result<Vec<DnsZone>, ProvisionError> {
        let resp = client
            .get(format!("{}/domain/domains", self.api_base))
            .bearer_auth(&self.token)
            .send()
            .await?;

        let resp = ensure_success(resp).await?;

        let data: Vec<GandiDomain> = resp.json().await?;
        Ok(data
            .into_iter()
            .map(|d| DnsZone {
                id: d.id,
                name: d.fqdn,
            })
            .collect())
    }

    async fn create_record(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        record: &DnsRecord,
    ) -> Result<(), ProvisionError> {
        // Gandi expects MX priority encoded into the value, e.g.
        // `"10 mail.example.com"`, not as a separate field. The official
        // go-gandi SDK has no `priority` field on its DomainRecord.
        let value = match record.priority {
            Some(p) => format!("{} {}", p, record.value),
            None => record.value.clone(),
        };
        let body = serde_json::json!({
            "rrset_type": record.record_type,
            "rrset_name": record.name,
            "rrset_values": [value],
            "rrset_ttl": record.ttl,
        });

        let resp = client
            .post(format!(
                "{}/domain/domains/{}/records",
                self.api_base, zone_id
            ))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;

        ensure_success(resp).await?;

        Ok(())
    }

    async fn find_records(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        name: &str,
        record_type: &str,
    ) -> Result<Vec<DnsRecord>, ProvisionError> {
        // Gandi's name endpoint: /domain/domains/{fqdn}/records/{rrset_name}/{rrset_type}.
        // `name` is already the zone-relative owner Gandi wants ("@" at the
        // apex) — the seam relativizes for every provider leaving
        // `record_names_relative_to_zone` at its `true` default. Used verbatim:
        // see the contract note on that method.
        let url = format!(
            "{}/domain/domains/{}/records/{}/{}",
            self.api_base, zone_id, name, record_type
        );
        let resp = client.get(&url).bearer_auth(&self.token).send().await?;

        // Gandi returns 404 when no record matches the name+type combo.
        if resp.status().as_u16() == 404 {
            return Ok(vec![]);
        }
        let resp = ensure_success(resp).await?;

        // Endpoint returns a single rrset object (not an array) with
        // `rrset_values` listing the record values.
        let rec: GandiRecord = resp.json().await?;
        Ok(rec
            .rrset_values
            .into_iter()
            .map(|v| DnsRecord {
                record_type: rec.rrset_type.clone(),
                name: rec.rrset_name.clone(),
                value: v,
                ttl: rec.rrset_ttl,
                priority: None,
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
        // Gandi groups records into a single RRset per (name, type). A
        // value-based delete must read the RRset, drop the one value, then
        // either PUT back the survivors or DELETE the whole RRset if none
        // remain — whole-RRset DELETE on a multi-value set would erase
        // unrelated records.
        // `name` is the already-relative owner (see `find_records`).
        let url = format!(
            "{}/domain/domains/{}/records/{}/{}",
            self.api_base, zone_id, name, record_type
        );

        let resp = client.get(&url).bearer_auth(&self.token).send().await?;
        // 404 → the RRset is already gone; idempotent success.
        if resp.status().as_u16() == 404 {
            return Ok(());
        }
        let resp = ensure_success(resp).await?;
        let rec: GandiRecord = resp.json().await?;

        // Value absent → nothing to do (don't rewrite the RRset needlessly).
        if !rec.rrset_values.iter().any(|v| v == value) {
            return Ok(());
        }
        let remaining: Vec<String> = rec
            .rrset_values
            .into_iter()
            .filter(|v| v != value)
            .collect();

        let result = if remaining.is_empty() {
            client.delete(&url).bearer_auth(&self.token).send().await?
        } else {
            let body = serde_json::json!({
                "rrset_values": remaining,
                "rrset_ttl": rec.rrset_ttl,
            });
            client
                .put(&url)
                .bearer_auth(&self.token)
                .json(&body)
                .send()
                .await?
        };

        // A concurrent teardown may have removed the RRset between our GET and
        // this write — treat a 404 as success.
        if result.status().as_u16() == 404 {
            return Ok(());
        }
        ensure_success(result).await?;

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
    fn test_gandi_parses_domain_list() {
        let json = r#"[
            {"fqdn": "example.com", "id": "abc-123"},
            {"fqdn": "another.org", "id": "def-456"}
        ]"#;
        let parsed: Vec<GandiDomain> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].fqdn, "example.com");
        assert_eq!(parsed[0].id, "abc-123");
    }
}
