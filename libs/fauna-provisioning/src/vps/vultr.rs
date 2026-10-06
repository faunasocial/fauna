use base64::Engine;
use reqwest::Client;
use serde::Deserialize;

use crate::error::ProvisionError;
use crate::proxy::{BuildEnv, default_api_base, default_api_base_for_env};
use crate::vps::{ManagedServer, ServerTypeInfo, VpsInstance, VpsLocation, VpsProvider};

/// Direct Vultr v2 API base. Native builds hit this directly; wasm32
/// builds route through the CORS proxy (`cors_policy: proxy` in
/// `i18n/providers.yaml`) — see `crate::proxy`.
const DIRECT_API: &str = "https://api.vultr.com/v2";
const PROXY_PREFIX: &str = "vultr/v2";

/// Vultr VPS provider.
pub struct Vultr {
    pub token: String,
    pub api_base: String,
}

#[derive(Deserialize)]
struct RegionsResponse {
    regions: Vec<VultrRegion>,
}

#[derive(Deserialize)]
struct VultrRegion {
    id: String,
    city: String,
    country: String,
}

#[derive(Deserialize)]
struct CreateInstanceResponse {
    instance: VultrInstance,
}

#[derive(Deserialize)]
struct VultrInstance {
    id: String,
    #[serde(default)]
    label: String,
    main_ip: String,
    /// Flat `k:v` tag strings (`create_server` sends them via `kv_tags`).
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    v6_main_ip: Option<String>,
    /// Creation instant, list responses only.
    #[serde(default)]
    date_created: Option<String>,
}

#[derive(Deserialize)]
struct InstancesResponse {
    instances: Vec<VultrInstance>,
}

/// `GET /instances` paging envelope — Vultr pages by opaque cursor; an empty
/// or absent `meta.links.next` is the last page.
#[derive(Deserialize)]
struct InstancesPageResponse {
    #[serde(default)]
    instances: Vec<VultrInstance>,
    #[serde(default)]
    meta: Option<VultrMeta>,
}

#[derive(Deserialize)]
struct VultrMeta {
    #[serde(default)]
    links: Option<VultrLinks>,
}

#[derive(Deserialize)]
struct VultrLinks {
    #[serde(default)]
    next: Option<String>,
}

#[derive(Deserialize)]
struct ReverseIpv4Response {
    reverse_ipv4s: Vec<VultrReverseEntry>,
}

#[derive(Deserialize)]
struct VultrReverseEntry {
    ip: String,
    reverse: String,
}

#[derive(Deserialize)]
struct PlansResponse {
    plans: Vec<VultrPlan>,
}

#[derive(Deserialize)]
struct VultrPlan {
    id: String,
    vcpu_count: u32,
    ram: u32,          // MB
    disk: u32,         // GB
    monthly_cost: f64, // USD
}

impl Vultr {
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

    pub fn with_base_url(token: String, api_base: String) -> Self {
        Self { token, api_base }
    }

    /// The API base this adapter will call.
    pub fn api_base(&self) -> &str {
        &self.api_base
    }
}

impl VpsProvider for Vultr {
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
            .regions
            .into_iter()
            .map(|r| VpsLocation {
                id: r.id.clone(),
                name: r.id,
                city: r.city,
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
        let user_data_b64 = base64::engine::general_purpose::STANDARD.encode(user_data);

        let mut body = serde_json::json!({
            "label": name,
            "region": location,
            "plan": server_type,
            "os_id": 2284, // os_id 2284 = Ubuntu 24.04 x64
            "user_data": user_data_b64,
        });
        if let Some(tags) = crate::vps::kv_tags(labels) {
            body["tags"] = tags;
        }

        let resp = crate::vps::json_bearer(
            client,
            reqwest::Method::POST,
            format!("{}/instances", self.api_base),
            &self.token,
            body,
        )
        .await?;

        let data: CreateInstanceResponse = resp.json().await?;
        Ok(VpsInstance {
            server_id: data.instance.id,
            ipv4: data.instance.main_ip,
        })
    }

    async fn delete_server(
        &self,
        client: &Client,
        instance: &VpsInstance,
    ) -> Result<(), ProvisionError> {
        crate::vps::delete_server_bearer(
            client,
            format!("{}/instances/{}", self.api_base, instance.server_id),
            &self.token,
        )
        .await
    }

    async fn list_server_types(
        &self,
        client: &Client,
        curated_ids: &[&str],
    ) -> Result<Vec<ServerTypeInfo>, ProvisionError> {
        let resp = crate::vps::get_bearer(
            client,
            format!("{}/plans?per_page=500", self.api_base),
            &self.token,
            &[],
        )
        .await?;

        let data: PlansResponse = resp.json().await?;
        let mut out = Vec::with_capacity(curated_ids.len());
        for curated in curated_ids {
            if let Some(p) = data.plans.iter().find(|p| p.id == *curated) {
                out.push(ServerTypeInfo {
                    id: p.id.clone(),
                    vcpu: p.vcpu_count,
                    mem_gb: p.ram as f32 / 1024.0,
                    disk_gb: p.disk,
                    price_monthly_cents: (p.monthly_cost * 100.0).round() as u64,
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
        let url = format!("{}/reverse/ipv4", self.api_base);
        crate::vps::set_ptr_bearer(
            client,
            reqwest::Method::POST,
            url,
            &self.token,
            serde_json::json!({
                "ip": instance.ipv4,
                "reverse": fqdn,
            }),
        )
        .await
    }

    async fn find_server_by_name(
        &self,
        client: &Client,
        name: &str,
    ) -> Result<Option<VpsInstance>, ProvisionError> {
        // Vultr filters by `label` query parameter on /v2/instances.
        let resp = crate::vps::get_bearer(
            client,
            format!("{}/instances", self.api_base),
            &self.token,
            &[("label", name)],
        )
        .await?;

        let data: InstancesResponse = resp.json().await?;
        Ok(crate::vps::find_matching_server(
            data.instances,
            |inst| inst.label == name,
            |inst| {
                (!inst.main_ip.is_empty()).then_some(VpsInstance {
                    server_id: inst.id,
                    ipv4: inst.main_ip,
                })
            },
        ))
    }

    async fn list_managed_servers(
        &self,
        client: &Client,
    ) -> Result<Vec<ManagedServer>, ProvisionError> {
        let tag = crate::vps::marker_tag();
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;

        for _ in 0..crate::vps::MAX_LIST_PAGES {
            let mut query: Vec<(&str, &str)> = vec![("tag", tag.as_str()), ("per_page", "500")];
            if let Some(c) = cursor.as_deref() {
                query.push(("cursor", c));
            }
            let resp = crate::vps::get_bearer(
                client,
                format!("{}/instances", self.api_base),
                &self.token,
                &query,
            )
            .await?;

            let data: InstancesPageResponse = resp.json().await?;
            for inst in data.instances {
                let labels = crate::vps::tags_to_labels(&inst.tags);
                // Re-check the marker: `tag` is Vultr's filter, but the
                // guarantee is ours.
                if !crate::vps::has_marker(&labels) {
                    continue;
                }
                out.push(ManagedServer {
                    server_id: inst.id,
                    name: inst.label,
                    ipv4: Some(inst.main_ip).filter(|ip| !ip.is_empty()),
                    ipv6: inst.v6_main_ip.filter(|ip| !ip.is_empty()),
                    labels,
                    created_at: inst.date_created,
                    marked: true,
                });
            }

            match data
                .meta
                .and_then(|m| m.links)
                .and_then(|l| l.next)
                .filter(|n| !n.is_empty())
            {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        Ok(out)
    }

    async fn get_ptr(
        &self,
        client: &Client,
        instance: &VpsInstance,
    ) -> Result<Option<String>, ProvisionError> {
        // Per Vultr docs the per-instance reverse IPv4 endpoint returns the
        // current PTR settings for the IP. An unset PTR is reported as an
        // empty string in the `reverse` field.
        let url = format!(
            "{}/instances/{}/ipv4/reverse",
            self.api_base, instance.server_id
        );
        let resp = crate::vps::get_bearer(client, url, &self.token, &[]).await?;

        let data: ReverseIpv4Response = resp.json().await?;
        Ok(data
            .reverse_ipv4s
            .into_iter()
            .find(|e| e.ip == instance.ipv4)
            .map(|e| e.reverse)
            .filter(|s| !s.is_empty()))
    }

    fn base_url(&self) -> &str {
        &self.api_base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vultr_parses_regions_response() {
        let json = r#"{
            "regions": [
                { "id": "ewr", "city": "New Jersey", "country": "US" },
                { "id": "ams", "city": "Amsterdam", "country": "NL" }
            ]
        }"#;
        let parsed: RegionsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.regions.len(), 2);
        assert_eq!(parsed.regions[0].id, "ewr");
        assert_eq!(parsed.regions[1].country, "NL");
    }

    #[test]
    fn test_vultr_parses_instance_response() {
        let json = r#"{
            "instance": {
                "id": "cb676a46-066d-49d4-aa03-f075c2ee18ab",
                "main_ip": "192.0.2.10"
            }
        }"#;
        let parsed: CreateInstanceResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.instance.id, "cb676a46-066d-49d4-aa03-f075c2ee18ab");
        assert_eq!(parsed.instance.main_ip, "192.0.2.10");
    }

    #[test]
    fn test_vultr_parses_plans_response() {
        let json = r#"{
            "plans": [
                {
                    "id": "vc2-1c-1gb",
                    "vcpu_count": 1,
                    "ram": 1024,
                    "disk": 25,
                    "monthly_cost": 6.0
                }
            ]
        }"#;
        let parsed: PlansResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.plans[0].id, "vc2-1c-1gb");
    }

    #[test]
    fn test_vultr_user_data_base64_encoded() {
        let user_data = "#cloud-config\npackages:\n  - curl\n";
        let encoded = base64::engine::general_purpose::STANDARD.encode(user_data);
        // Verify it's valid base64 that decodes back correctly
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&encoded)
            .unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), user_data);
    }
}
