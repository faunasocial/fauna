use base64::Engine;
use rand::Rng;
use reqwest::Client;
use serde::Deserialize;

use crate::error::{ProvisionError, ensure_success};
use crate::vps::{ManagedServer, ServerTypeInfo, VpsInstance, VpsLocation, VpsProvider};

const API_BASE: &str = "https://api.linode.com/v4";

/// Linode VPS provider.
pub struct Linode {
    pub token: String,
    pub api_base: String,
}

fn random_root_pass() -> String {
    let mut rng = rand::thread_rng();
    const CHARSET: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*";
    (0..32)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

#[derive(Deserialize)]
struct RegionsResponse {
    data: Vec<LinodeRegion>,
}

#[derive(Deserialize)]
struct LinodeRegion {
    id: String,
    label: String,
    country: String,
}

#[derive(Deserialize)]
struct LinodeInstance {
    id: u64,
    #[serde(default)]
    label: String,
    ipv4: Vec<String>,
    /// Flat `k:v` tag strings (`create_server` sends them via `kv_tags`).
    #[serde(default)]
    tags: Vec<String>,
    /// Linode reports a single IPv6 slug string, not a list.
    #[serde(default)]
    ipv6: Option<String>,
    /// Creation instant, list responses only.
    #[serde(default)]
    created: Option<String>,
}

#[derive(Deserialize)]
struct InstancesResponse {
    data: Vec<LinodeInstance>,
}

/// `GET /linode/instances` paging envelope — Linode reports the current page
/// and the total page count rather than a cursor.
#[derive(Deserialize)]
struct InstancesPageResponse {
    #[serde(default)]
    data: Vec<LinodeInstance>,
    #[serde(default)]
    page: u32,
    #[serde(default)]
    pages: u32,
}

#[derive(Deserialize)]
struct LinodeIpAddress {
    rdns: Option<String>,
}

#[derive(Deserialize)]
struct TypesResponse {
    data: Vec<LinodeType>,
}

#[derive(Deserialize)]
struct LinodeType {
    id: String,
    vcpus: u32,
    memory: u32, // MB
    disk: u32,   // MB
    price: LinodePrice,
}

#[derive(Deserialize)]
struct LinodePrice {
    monthly: f64, // USD
}

impl Linode {
    pub fn new(token: String) -> Self {
        Self::with_base_url(token, API_BASE.to_string())
    }
    pub fn with_base_url(token: String, api_base: String) -> Self {
        Self { token, api_base }
    }
}

impl VpsProvider for Linode {
    async fn verify(&self, client: &Client) -> Result<Vec<VpsLocation>, ProvisionError> {
        let resp = crate::vps::get_bearer(
            client,
            format!("{}/regions", self.api_base),
            &self.token,
            &[],
        )
        .await?;

        let data: RegionsResponse = resp.json().await?;
        Ok(data
            .data
            .into_iter()
            .map(|r| VpsLocation {
                id: r.id.clone(),
                name: r.label,
                city: r.id,
                country: r.country,
            })
            .collect())
    }

    async fn create_server(
        &self,
        client: &Client,
        name: &str,
        location: &str,
        server_type: &str,
        user_data: &str,
        labels: &[(String, String)],
    ) -> Result<VpsInstance, ProvisionError> {
        let root_pass = random_root_pass();
        let user_data_b64 = base64::engine::general_purpose::STANDARD.encode(user_data);

        let mut body = serde_json::json!({
            "label": name,
            "type": server_type,
            "region": location,
            "image": "linode/ubuntu24.04",
            "root_pass": root_pass,
            "metadata": {
                "user_data": user_data_b64,
            },
        });
        if let Some(tags) = crate::vps::kv_tags(labels) {
            body["tags"] = tags;
        }

        let resp = crate::vps::json_bearer(
            client,
            reqwest::Method::POST,
            format!("{}/linode/instances", self.api_base),
            &self.token,
            body,
        )
        .await?;

        let instance: LinodeInstance = resp.json().await?;
        let ipv4 = instance
            .ipv4
            .into_iter()
            .next()
            .ok_or_else(|| ProvisionError::Parse("no IPv4 address found".into()))?;

        Ok(VpsInstance {
            server_id: instance.id.to_string(),
            ipv4,
        })
    }

    async fn delete_server(
        &self,
        client: &Client,
        instance: &VpsInstance,
    ) -> Result<(), ProvisionError> {
        crate::vps::delete_server_bearer(
            client,
            format!("{}/linode/instances/{}", self.api_base, instance.server_id),
            &self.token,
        )
        .await
    }

    async fn list_server_types(
        &self,
        client: &Client,
        curated_ids: &[&str],
    ) -> Result<Vec<ServerTypeInfo>, ProvisionError> {
        // /v4/linode/types is unauthenticated; bearer-auth is harmless.
        let resp = crate::vps::get_bearer(
            client,
            format!("{}/linode/types", self.api_base),
            &self.token,
            &[],
        )
        .await?;

        let data: TypesResponse = resp.json().await?;
        let mut out = Vec::with_capacity(curated_ids.len());
        for curated in curated_ids {
            if let Some(t) = data.data.iter().find(|t| t.id == *curated) {
                out.push(ServerTypeInfo {
                    id: t.id.clone(),
                    vcpu: t.vcpus,
                    mem_gb: t.memory as f32 / 1024.0,
                    disk_gb: t.disk / 1024,
                    price_monthly_cents: (t.price.monthly * 100.0).round() as u64,
                    currency: "USD".to_string(),
                });
            }
        }
        Ok(out)
    }

    async fn set_ptr(
        &self,
        client: &Client,
        instance: &VpsInstance,
        fqdn: &str,
    ) -> Result<(), ProvisionError> {
        let url = format!("{}/networking/ips/{}", self.api_base, instance.ipv4);
        crate::vps::set_ptr_bearer(
            client,
            reqwest::Method::PUT,
            url,
            &self.token,
            serde_json::json!({ "rdns": fqdn }),
        )
        .await
    }

    async fn find_server_by_name(
        &self,
        client: &Client,
        name: &str,
    ) -> Result<Option<VpsInstance>, ProvisionError> {
        // Linode filters via the X-Filter header (JSON-encoded). Match by label.
        let filter = serde_json::json!({ "label": name }).to_string();
        let resp = client
            .get(format!("{}/linode/instances", self.api_base))
            .bearer_auth(&self.token)
            .header("X-Filter", filter)
            .send()
            .await?;

        let resp = ensure_success(resp).await?;

        let data: InstancesResponse = resp.json().await?;
        Ok(crate::vps::find_matching_server(
            data.data,
            |inst| inst.label == name,
            |inst| {
                inst.ipv4.into_iter().next().map(|ipv4| VpsInstance {
                    server_id: inst.id.to_string(),
                    ipv4,
                })
            },
        ))
    }

    async fn list_managed_servers(
        &self,
        client: &Client,
    ) -> Result<Vec<ManagedServer>, ProvisionError> {
        // Linode filters via the X-Filter header (JSON-encoded), the same
        // seam `find_server_by_name` uses — a `tags` match here.
        let filter = serde_json::json!({ "tags": crate::vps::marker_tag() }).to_string();
        let mut out = Vec::new();
        let mut page: u32 = 1;

        for _ in 0..crate::vps::MAX_LIST_PAGES {
            let resp = client
                .get(format!("{}/linode/instances", self.api_base))
                .bearer_auth(&self.token)
                .header("X-Filter", &filter)
                .query(&[("page", page.to_string())])
                .send()
                .await?;
            let resp = ensure_success(resp).await?;

            let data: InstancesPageResponse = resp.json().await?;
            for inst in data.data {
                let labels = crate::vps::tags_to_labels(&inst.tags);
                // Re-check the marker: the X-Filter is Linode's, but the
                // guarantee is ours.
                if !crate::vps::has_marker(&labels) {
                    continue;
                }
                out.push(ManagedServer {
                    server_id: inst.id.to_string(),
                    name: inst.label,
                    ipv4: inst.ipv4.into_iter().next(),
                    ipv6: inst.ipv6.filter(|s| !s.is_empty()),
                    labels,
                    created_at: inst.created,
                    marked: true,
                });
            }

            if data.pages <= data.page.max(page) {
                break;
            }
            page += 1;
        }

        Ok(out)
    }

    async fn get_ptr(
        &self,
        client: &Client,
        instance: &VpsInstance,
    ) -> Result<Option<String>, ProvisionError> {
        let url = format!("{}/networking/ips/{}", self.api_base, instance.ipv4);
        let resp = crate::vps::get_bearer(client, url, &self.token, &[]).await?;

        let data: LinodeIpAddress = resp.json().await?;
        Ok(data.rdns.filter(|s| !s.is_empty()))
    }

    fn base_url(&self) -> &str {
        &self.api_base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_linode_parses_regions_response() {
        let json = r#"{
            "data": [
                { "id": "us-east", "label": "Newark, NJ, USA", "country": "us" },
                { "id": "eu-west", "label": "London, England, UK", "country": "gb" }
            ]
        }"#;
        let parsed: RegionsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.data.len(), 2);
        assert_eq!(parsed.data[0].id, "us-east");
        assert_eq!(parsed.data[1].country, "gb");
    }

    #[test]
    fn test_linode_parses_create_response() {
        let json = r#"{
            "id": 123456,
            "ipv4": ["203.0.113.1", "192.168.1.1"]
        }"#;
        let parsed: LinodeInstance = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.id, 123456);
        assert_eq!(parsed.ipv4.len(), 2);
        assert_eq!(parsed.ipv4[0], "203.0.113.1");
    }

    #[test]
    fn test_linode_parses_types_response() {
        let json = r#"{
            "data": [
                {
                    "id": "g6-nanode-1",
                    "vcpus": 1,
                    "memory": 1024,
                    "disk": 25600,
                    "price": { "monthly": 5.0 }
                }
            ]
        }"#;
        let parsed: TypesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.data[0].id, "g6-nanode-1");
        assert_eq!(parsed.data[0].price.monthly, 5.0);
    }

    #[test]
    fn test_linode_random_root_pass_length() {
        let pass = random_root_pass();
        assert_eq!(pass.len(), 32);
    }
}
