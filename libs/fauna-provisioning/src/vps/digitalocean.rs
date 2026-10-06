use reqwest::Client;
use serde::Deserialize;

use crate::error::ProvisionError;
use crate::vps::{ManagedServer, ServerTypeInfo, VpsInstance, VpsLocation, VpsProvider};

const API_BASE: &str = "https://api.digitalocean.com/v2";

/// DigitalOcean VPS provider.
pub struct DigitalOcean {
    pub token: String,
    pub api_base: String,
}

#[derive(Deserialize)]
struct RegionsResponse {
    regions: Vec<DoRegion>,
}

#[derive(Deserialize)]
struct DoRegion {
    slug: String,
    name: String,
    available: bool,
}

#[derive(Deserialize)]
struct CreateDropletResponse {
    droplet: DoDroplet,
}

#[derive(Deserialize)]
struct DoDroplet {
    id: u64,
    #[serde(default)]
    name: String,
    /// Flat `k:v` tag strings (`create_server` sends them via `kv_tags`).
    #[serde(default)]
    tags: Vec<String>,
    /// RFC 3339 creation instant, list responses only.
    #[serde(default)]
    created_at: Option<String>,
    networks: DoNetworks,
}

#[derive(Deserialize)]
struct DropletsResponse {
    droplets: Vec<DoDroplet>,
}

#[derive(Deserialize)]
struct GetDropletResponse {
    droplet: DoDroplet,
}

#[derive(Deserialize)]
struct DoNetworks {
    v4: Vec<DoNetworkV4>,
    #[serde(default)]
    v6: Vec<DoNetworkV4>,
}

/// `GET /droplets` paging envelope — `links.pages.next` is absent on the last
/// page. Only its *presence* is read: the adapter re-sends its own filtered
/// URL with the next page number rather than following the provider's link,
/// so a hostile or misconfigured `next` can never redirect the walk off the
/// user's own account.
#[derive(Deserialize)]
struct DropletsListResponse {
    #[serde(default)]
    droplets: Vec<DoDroplet>,
    #[serde(default)]
    links: Option<DoLinks>,
}

#[derive(Deserialize)]
struct DoLinks {
    #[serde(default)]
    pages: Option<DoPages>,
}

#[derive(Deserialize)]
struct DoPages {
    #[serde(default)]
    next: Option<String>,
}

#[derive(Deserialize)]
struct DoNetworkV4 {
    ip_address: String,
    #[serde(rename = "type")]
    network_type: String,
}

#[derive(Deserialize)]
struct SizesResponse {
    sizes: Vec<DoSize>,
}

#[derive(Deserialize)]
struct DoSize {
    slug: String,
    vcpus: u32,
    memory: u32,        // MB
    disk: u32,          // GB
    price_monthly: f64, // USD
    available: bool,
}

impl DigitalOcean {
    pub fn new(token: String) -> Self {
        Self::with_base_url(token, API_BASE.to_string())
    }
    pub fn with_base_url(token: String, api_base: String) -> Self {
        Self { token, api_base }
    }
}

impl VpsProvider for DigitalOcean {
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
            .filter(|r| r.available)
            .map(|r| VpsLocation {
                id: r.slug.clone(),
                name: r.name,
                city: r.slug,
                country: String::new(),
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
        let mut body = serde_json::json!({
            "name": name,
            "region": location,
            "size": server_type,
            "image": "ubuntu-24-04-x64",
            "user_data": user_data,
            "ipv6": true,
        });
        if let Some(tags) = crate::vps::kv_tags(labels) {
            body["tags"] = tags;
        }

        let resp = crate::vps::json_bearer(
            client,
            reqwest::Method::POST,
            format!("{}/droplets", self.api_base),
            &self.token,
            body,
        )
        .await?;

        let data: CreateDropletResponse = resp.json().await?;
        let ipv4 = data
            .droplet
            .networks
            .v4
            .into_iter()
            .find(|n| n.network_type == "public")
            .map(|n| n.ip_address)
            .ok_or_else(|| ProvisionError::Parse("no public IPv4 address found".into()))?;

        Ok(VpsInstance {
            server_id: data.droplet.id.to_string(),
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
            format!("{}/droplets/{}", self.api_base, instance.server_id),
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
            format!("{}/sizes?per_page=200", self.api_base),
            &self.token,
            &[],
        )
        .await?;

        let data: SizesResponse = resp.json().await?;
        let mut out = Vec::with_capacity(curated_ids.len());
        for curated in curated_ids {
            if let Some(s) = data
                .sizes
                .iter()
                .find(|s| s.slug == *curated && s.available)
            {
                out.push(ServerTypeInfo {
                    id: s.slug.clone(),
                    vcpu: s.vcpus,
                    mem_gb: s.memory as f32 / 1024.0,
                    disk_gb: s.disk,
                    price_monthly_cents: (s.price_monthly * 100.0).round() as u64,
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
        let url = format!("{}/droplets/{}", self.api_base, instance.server_id);
        crate::vps::set_ptr_bearer(
            client,
            reqwest::Method::PUT,
            url,
            &self.token,
            serde_json::json!({ "name": fqdn }),
        )
        .await
    }

    async fn find_server_by_name(
        &self,
        client: &Client,
        name: &str,
    ) -> Result<Option<VpsInstance>, ProvisionError> {
        let resp = crate::vps::get_bearer(
            client,
            format!("{}/droplets", self.api_base),
            &self.token,
            &[("name", name)],
        )
        .await?;

        let data: DropletsResponse = resp.json().await?;
        Ok(crate::vps::find_matching_server(
            data.droplets,
            |d| d.name == name,
            |d| {
                d.networks
                    .v4
                    .into_iter()
                    .find(|n| n.network_type == "public")
                    .map(|public| VpsInstance {
                        server_id: d.id.to_string(),
                        ipv4: public.ip_address,
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
        let mut page: u32 = 1;

        for _ in 0..crate::vps::MAX_LIST_PAGES {
            let resp = crate::vps::get_bearer(
                client,
                format!("{}/droplets", self.api_base),
                &self.token,
                &[
                    ("tag_name", tag.as_str()),
                    ("page", &page.to_string()),
                    ("per_page", "200"),
                ],
            )
            .await?;

            let data: DropletsListResponse = resp.json().await?;
            let empty = data.droplets.is_empty();
            for d in data.droplets {
                let labels = crate::vps::tags_to_labels(&d.tags);
                // Re-check the marker: `tag_name` is DigitalOcean's filter,
                // but the guarantee is ours.
                if !crate::vps::has_marker(&labels) {
                    continue;
                }
                let public_v4 = d
                    .networks
                    .v4
                    .into_iter()
                    .find(|n| n.network_type == "public")
                    .map(|n| n.ip_address);
                let public_v6 = d
                    .networks
                    .v6
                    .into_iter()
                    .find(|n| n.network_type == "public")
                    .map(|n| n.ip_address);
                out.push(ManagedServer {
                    server_id: d.id.to_string(),
                    name: d.name,
                    ipv4: public_v4,
                    ipv6: public_v6,
                    labels,
                    created_at: d.created_at,
                    marked: true,
                });
            }

            let has_next = data
                .links
                .and_then(|l| l.pages)
                .and_then(|p| p.next)
                .is_some_and(|n| !n.is_empty());
            if !has_next || empty {
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
        // DigitalOcean derives the PTR from the droplet name, so the
        // pre-flight check reads the current droplet name and treats it as
        // the PTR. `set_ptr` renames the droplet to the FQDN.
        let url = format!("{}/droplets/{}", self.api_base, instance.server_id);
        let resp = crate::vps::get_bearer(client, url, &self.token, &[]).await?;

        let data: GetDropletResponse = resp.json().await?;
        Ok(Some(data.droplet.name).filter(|n| !n.is_empty()))
    }

    fn base_url(&self) -> &str {
        &self.api_base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_digitalocean_parses_regions_response() {
        let json = r#"{
            "regions": [
                { "slug": "nyc1", "name": "New York 1", "available": true },
                { "slug": "ams2", "name": "Amsterdam 2", "available": false },
                { "slug": "sfo3", "name": "San Francisco 3", "available": true }
            ]
        }"#;
        let parsed: RegionsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.regions.len(), 3);
        let available: Vec<_> = parsed.regions.iter().filter(|r| r.available).collect();
        assert_eq!(available.len(), 2);
        assert_eq!(available[0].slug, "nyc1");
    }

    #[test]
    fn test_digitalocean_parses_sizes_response() {
        let json = r#"{
            "sizes": [
                {
                    "slug": "s-1vcpu-1gb",
                    "vcpus": 1,
                    "memory": 1024,
                    "disk": 25,
                    "price_monthly": 6.0,
                    "available": true
                }
            ]
        }"#;
        let parsed: SizesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.sizes[0].vcpus, 1);
    }

    #[test]
    fn test_digitalocean_parses_droplet_response() {
        let json = r#"{
            "droplet": {
                "id": 42,
                "networks": {
                    "v4": [
                        { "ip_address": "10.0.0.1", "type": "private" },
                        { "ip_address": "203.0.113.5", "type": "public" }
                    ]
                }
            }
        }"#;
        let parsed: CreateDropletResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.droplet.id, 42);
        let public = parsed
            .droplet
            .networks
            .v4
            .iter()
            .find(|n| n.network_type == "public")
            .unwrap();
        assert_eq!(public.ip_address, "203.0.113.5");
    }
}
