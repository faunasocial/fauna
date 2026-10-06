//! DNS half of the generic bundled provider — the open Fauna Bundled Provider
//! API v1 (`docs/goal/architecture/provisioning/bundled-provider-api.md`
//! § Endpoints). Owner names arrive zone-relative with `@` at the apex and are
//! passed verbatim (`registry.md` § The owner-name contract); record ids stay
//! inside this adapter — `delete_record` resolves the id by name + type +
//! value, the value-based seam every DNS adapter presents.

use serde::Deserialize;

use crate::bundled_api::{BundledApi, deserialize_id};
use crate::dns::{DnsProvider, DnsRecord, DnsZone};
use crate::error::ProvisionError;

pub struct BundledDns {
    api: BundledApi,
}

/// A record as the spec spells it (spec § Shapes → `Record`).
#[derive(Deserialize)]
struct RecordJson {
    #[serde(deserialize_with = "deserialize_id")]
    id: String,
    #[serde(rename = "type")]
    record_type: String,
    name: String,
    value: String,
    #[serde(default)]
    ttl: u32,
    #[serde(default)]
    priority: Option<u32>,
}

#[derive(Deserialize)]
struct RecordsResponse {
    #[serde(default)]
    records: Vec<RecordJson>,
}

impl BundledDns {
    pub fn new(base_url: String, token: String) -> Self {
        Self {
            api: BundledApi::new(base_url, token),
        }
    }

    async fn list(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        name: &str,
        record_type: &str,
    ) -> Result<Vec<RecordJson>, ProvisionError> {
        let resp = self
            .api
            .get(
                client,
                &format!("/zones/{zone_id}/records"),
                &[("name", name), ("type", record_type)],
            )
            .await?;
        let r: RecordsResponse = resp.json().await?;
        // Exact-match filtering is the server's job by spec; re-filtering
        // here costs nothing and keeps a lenient implementation from
        // returning a neighbour's record into the idempotency check.
        Ok(r.records
            .into_iter()
            .filter(|rec| rec.name == name && rec.record_type == record_type)
            .collect())
    }
}

impl DnsProvider for BundledDns {
    async fn verify(&self, client: &reqwest::Client) -> Result<Vec<DnsZone>, ProvisionError> {
        let me = self.api.me(client).await?;
        Ok(me
            .zones
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
        let mut body = serde_json::json!({
            "type": record.record_type,
            "name": record.name,
            "value": record.value,
            "ttl": record.ttl,
        });
        if let Some(p) = record.priority {
            body["priority"] = serde_json::json!(p);
        }
        self.api
            .post_json(client, &format!("/zones/{zone_id}/records"), &body)
            .await?;
        Ok(())
    }

    async fn find_records(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        name: &str,
        record_type: &str,
    ) -> Result<Vec<DnsRecord>, ProvisionError> {
        Ok(self
            .list(client, zone_id, name, record_type)
            .await?
            .into_iter()
            .map(|r| DnsRecord {
                record_type: r.record_type,
                name: r.name,
                value: r.value,
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
        let matching = self.list(client, zone_id, name, record_type).await?;
        // Already absent → idempotent success (the trait contract).
        let Some(rec) = matching.into_iter().find(|r| r.value == value) else {
            return Ok(());
        };
        let resp = self
            .api
            .delete_raw(client, &format!("/zones/{zone_id}/records/{}", rec.id))
            .await?;
        crate::vps::finish_delete(resp).await
    }

    fn base_url(&self) -> &str {
        self.api.base_url()
    }
}
