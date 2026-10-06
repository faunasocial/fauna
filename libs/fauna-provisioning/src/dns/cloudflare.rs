use serde::Deserialize;

use crate::dns::{DnsProvider, DnsRecord, DnsZone};
use crate::error::{ProvisionError, ensure_success};
use crate::proxy::{BuildEnv, default_api_base, default_api_base_for_env};

/// Direct Cloudflare v4 API base. Native builds hit this directly; wasm32
/// builds route through the CORS proxy (`cors_policy: proxy` in
/// `i18n/providers.yaml`) — see `crate::proxy`.
const DIRECT_API: &str = "https://api.cloudflare.com/client/v4";
const PROXY_PREFIX: &str = "cloudflare/client/v4";

/// Build the Cloudflare `POST /dns_records` body for `record`. Most record types
/// take a flat `content` string (plus an optional `priority` for MX), but
/// **TLSA** (DANE — the `_25._tcp.mail.<primary>` floor-MX pin) is the exception:
/// Cloudflare **rejects** `content:"3 1 1 <hex>"` on create and requires a
/// structured `data` object `{usage, selector, matching_type, certificate}`
/// (verified against the v4 API, 2026-06-07). So parse the four-component RDATA
/// into that object for TLSA; everything else stays the content-string path.
fn cloudflare_create_body(record: &DnsRecord) -> Result<serde_json::Value, ProvisionError> {
    if record.record_type == "TLSA" {
        let data = tlsa_data_object(&record.value).ok_or_else(|| {
            ProvisionError::Other(format!(
                "malformed TLSA RDATA for Cloudflare (want `<usage> <selector> <matching> <hex>`): {:?}",
                record.value
            ))
        })?;
        return Ok(serde_json::json!({
            "type": "TLSA",
            "name": record.name,
            "data": data,
            "ttl": record.ttl,
        }));
    }
    let mut body = serde_json::json!({
        "type": record.record_type,
        "name": record.name,
        "content": record.value,
        "ttl": record.ttl,
    });
    if let Some(priority) = record.priority {
        body["priority"] = serde_json::json!(priority);
    }
    Ok(body)
}

/// Parse `"<usage> <selector> <matching> <hex>"` TLSA RDATA into Cloudflare's
/// `data` object. `None` when the shape is wrong (three leading `u8`s + exactly
/// one non-empty hex certificate token) so the caller fails loudly rather than
/// posting a body Cloudflare would reject.
fn tlsa_data_object(rdata: &str) -> Option<serde_json::Value> {
    let (usage, selector, matching, certificate) =
        fauna_mail::dns::verify::parse_tlsa_fields(rdata)?;
    Some(serde_json::json!({
        "usage": usage,
        "selector": selector,
        "matching_type": matching,
        "certificate": certificate,
    }))
}

/// Cloudflare DNS provider.
pub struct Cloudflare {
    pub token: String,
    api_base: String,
}

#[derive(Deserialize)]
struct ZonesResponse {
    success: bool,
    result: Vec<CloudflareZone>,
    #[serde(default)]
    errors: Vec<CloudflareError>,
}

#[derive(Deserialize)]
struct CloudflareZone {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct CreateRecordResponse {
    success: bool,
    #[serde(default)]
    errors: Vec<CloudflareError>,
}

#[derive(Deserialize)]
struct CloudflareError {
    message: String,
}

impl Cloudflare {
    pub fn new(token: String) -> Self {
        Self::with_base_url(token, default_api_base(DIRECT_API, PROXY_PREFIX))
    }

    /// [`Self::new`] with the build env supplied explicitly — lets a native
    /// test assert the browser routing. See `tests/cors_policy_bijection.rs`.
    pub fn new_for_env(token: String, env: BuildEnv) -> Self {
        Self::with_base_url(
            token,
            default_api_base_for_env(DIRECT_API, PROXY_PREFIX, env),
        )
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

#[derive(Deserialize)]
struct DnsRecordsResponse {
    success: bool,
    #[serde(default)]
    result: Vec<CloudflareDnsRecord>,
    #[serde(default)]
    errors: Vec<CloudflareError>,
}

#[derive(Deserialize)]
struct CloudflareDnsRecord {
    /// Provider record id — only consumed by `delete_record` (the public
    /// `DnsRecord` deliberately omits it). `find_records` ignores it.
    #[serde(default)]
    id: String,
    #[serde(rename = "type", default)]
    record_type: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    ttl: u32,
    #[serde(default)]
    priority: Option<u32>,
}

impl DnsProvider for Cloudflare {
    async fn verify(&self, client: &reqwest::Client) -> Result<Vec<DnsZone>, ProvisionError> {
        let resp = client
            .get(format!("{}/zones?per_page=50", self.api_base))
            .bearer_auth(&self.token)
            .send()
            .await?;

        let resp = ensure_success(resp).await?;

        let data: ZonesResponse = resp.json().await?;
        if !data.success {
            let msg = data
                .errors
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(ProvisionError::Other(format!("Cloudflare error: {msg}")));
        }

        Ok(data
            .result
            .into_iter()
            .map(|z| DnsZone {
                id: z.id,
                name: z.name,
            })
            .collect())
    }

    async fn create_record(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        record: &DnsRecord,
    ) -> Result<(), ProvisionError> {
        let body = cloudflare_create_body(record)?;

        let resp = client
            .post(format!("{}/zones/{zone_id}/dns_records", self.api_base))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;

        let resp = ensure_success(resp).await?;

        let data: CreateRecordResponse = resp.json().await?;
        if !data.success {
            let msg = data
                .errors
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(ProvisionError::Other(format!("Cloudflare error: {msg}")));
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
        let resp = client
            .get(format!("{}/zones/{zone_id}/dns_records", self.api_base))
            .bearer_auth(&self.token)
            .query(&[("name", name), ("type", record_type), ("per_page", "50")])
            .send()
            .await?;

        let resp = ensure_success(resp).await?;

        let data: DnsRecordsResponse = resp.json().await?;
        if !data.success {
            let msg = data
                .errors
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(ProvisionError::Other(format!("Cloudflare error: {msg}")));
        }

        Ok(data
            .result
            .into_iter()
            .map(|r| DnsRecord {
                record_type: r.record_type,
                name: r.name,
                value: r.content,
                ttl: r.ttl,
                priority: r.priority,
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
        // Resolve the provider record id by listing the (name, type) records
        // and matching on content; Cloudflare deletes by record id.
        let resp = client
            .get(format!("{}/zones/{zone_id}/dns_records", self.api_base))
            .bearer_auth(&self.token)
            .query(&[("name", name), ("type", record_type), ("per_page", "50")])
            .send()
            .await?;

        let resp = ensure_success(resp).await?;

        let data: DnsRecordsResponse = resp.json().await?;
        if !data.success {
            let msg = data
                .errors
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(ProvisionError::Other(format!("Cloudflare error: {msg}")));
        }

        // Idempotent: nothing matching this value means it's already gone.
        let Some(rec) = data.result.into_iter().find(|r| r.content == value) else {
            return Ok(());
        };

        let del = client
            .delete(format!(
                "{}/zones/{zone_id}/dns_records/{}",
                self.api_base, rec.id
            ))
            .bearer_auth(&self.token)
            .send()
            .await?;

        let del = ensure_success(del).await?;

        let data: CreateRecordResponse = del.json().await?;
        if !data.success {
            let msg = data
                .errors
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(ProvisionError::Other(format!("Cloudflare error: {msg}")));
        }

        Ok(())
    }

    /// Cloudflare's v4 DNS API consumes and returns **fully-qualified** owner
    /// names (`mail.example.com`, `_acme-challenge.example.com`), so the
    /// orchestrator must NOT relativize record owners for it — unlike every
    /// other provider (see [`DnsProvider::record_names_relative_to_zone`]).
    fn record_names_relative_to_zone(&self) -> bool {
        false
    }

    fn base_url(&self) -> &str {
        &self.api_base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cloudflare_parses_zones_response() {
        let json = r#"{
            "success": true,
            "result": [
                {"id": "abc123", "name": "example.com"},
                {"id": "def456", "name": "another.org"}
            ],
            "errors": []
        }"#;
        let parsed: ZonesResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.success);
        assert_eq!(parsed.result.len(), 2);
        assert_eq!(parsed.result[0].id, "abc123");
        assert_eq!(parsed.result[0].name, "example.com");
    }

    #[test]
    fn test_cloudflare_parses_create_record_response() {
        let json = r#"{
            "success": true,
            "errors": []
        }"#;
        let parsed: CreateRecordResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.success);
        assert!(parsed.errors.is_empty());
    }

    #[test]
    fn test_cloudflare_parses_error_response() {
        let json = r#"{
            "success": false,
            "result": [],
            "errors": [{"message": "Invalid API token"}]
        }"#;
        let parsed: ZonesResponse = serde_json::from_str(json).unwrap();
        assert!(!parsed.success);
        assert_eq!(parsed.errors.len(), 1);
        assert_eq!(parsed.errors[0].message, "Invalid API token");
    }

    fn rec(record_type: &str, value: &str) -> DnsRecord {
        DnsRecord {
            record_type: record_type.into(),
            name: "_25._tcp.mail.example.com".into(),
            value: value.into(),
            ttl: 3600,
            priority: None,
        }
    }

    /// A TLSA create body is the structured `data` object (Cloudflare rejects a
    /// `content` string for TLSA), with the four RDATA components parsed out.
    #[test]
    fn tlsa_create_body_is_structured_data() {
        let body = cloudflare_create_body(&rec("TLSA", "3 1 1 abc123")).unwrap();
        assert_eq!(body["type"], "TLSA");
        assert_eq!(body["name"], "_25._tcp.mail.example.com");
        assert!(body.get("content").is_none(), "TLSA must not use content");
        assert_eq!(body["data"]["usage"], 3);
        assert_eq!(body["data"]["selector"], 1);
        assert_eq!(body["data"]["matching_type"], 1);
        assert_eq!(body["data"]["certificate"], "abc123");
    }

    /// A non-TLSA record keeps the flat `content` shape (+ MX priority).
    #[test]
    fn non_tlsa_create_body_uses_content() {
        let body = cloudflare_create_body(&rec("TXT", "v=spf1 mx ~all")).unwrap();
        assert_eq!(body["type"], "TXT");
        assert_eq!(body["content"], "v=spf1 mx ~all");
        assert!(body.get("data").is_none());
    }

    /// Malformed TLSA RDATA fails loudly rather than posting a rejected body.
    #[test]
    fn malformed_tlsa_rdata_is_rejected() {
        assert!(tlsa_data_object("3 1 1").is_none(), "missing cert");
        assert!(
            tlsa_data_object("3 1 1 abc def").is_none(),
            "trailing token"
        );
        assert!(tlsa_data_object("x 1 1 abc").is_none(), "non-int usage");
        assert!(cloudflare_create_body(&rec("TLSA", "garbage")).is_err());
    }
}
