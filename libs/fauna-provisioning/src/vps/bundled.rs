//! VPS half of the generic bundled provider — the open Fauna Bundled Provider
//! API v1 (`docs/goal/architecture/provisioning/bundled-provider-api.md`
//! § Endpoints). Cloud-init passes through **verbatim** (`user_data`), the
//! orchestrator's `managed-by: fauna` label rides the `labels` map, and the
//! intermediary's `GET /v1/server_types` is *already* its curated catalog —
//! so `curated_ids` (the registry's `["*"]` wildcard, `registry.md` § Bundled
//! provider) is ignored and the first five entries are offered in the order
//! the provider returned them.

use std::collections::HashMap;

use serde::Deserialize;

use crate::bundled_api::{BundledApi, deserialize_id};
use crate::error::ProvisionError;
use crate::vps::{ManagedServer, ServerTypeInfo, VpsInstance, VpsLocation, VpsProvider};

/// How many catalog entries the wizard's server-type radio shows — the same
/// "max 5" convention `curated_offers` follows for the named providers.
pub const MAX_OFFERED_SERVER_TYPES: usize = 5;

pub struct BundledVps {
    api: BundledApi,
}

#[derive(Deserialize)]
struct ServerJson {
    #[serde(deserialize_with = "deserialize_id")]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    ipv4: String,
    /// Spec § Shapes → `Server`: optional, absent or `null` when the box has
    /// no IPv6.
    #[serde(default)]
    ipv6: Option<String>,
    /// Spec § Shapes → `Server`: labels are a flat string map.
    #[serde(default)]
    labels: HashMap<String, String>,
}

#[derive(Deserialize)]
struct ServersResponse {
    #[serde(default)]
    servers: Vec<ServerJson>,
}

#[derive(Deserialize)]
struct ServerTypeJson {
    id: String,
    vcpu: u32,
    mem_gb: f32,
    disk_gb: u32,
    price_monthly_cents: u64,
    currency: String,
}

#[derive(Deserialize)]
struct ServerTypesResponse {
    #[serde(default)]
    server_types: Vec<ServerTypeJson>,
}

#[derive(Deserialize)]
struct PtrResponse {
    #[serde(default)]
    ptr: Option<String>,
}

impl BundledVps {
    pub fn new(base_url: String, token: String) -> Self {
        Self {
            api: BundledApi::new(base_url, token),
        }
    }
}

impl VpsProvider for BundledVps {
    async fn verify(&self, client: &reqwest::Client) -> Result<Vec<VpsLocation>, ProvisionError> {
        let me = self.api.me(client).await?;
        Ok(me
            .locations
            .into_iter()
            .map(|l| VpsLocation {
                id: l.id,
                name: l.name,
                city: l.city,
                country: l.country,
            })
            .collect())
    }

    async fn create_server(
        &self,
        client: &reqwest::Client,
        name: &str,
        location: &str,
        server_type: &str,
        user_data: &str,
        labels: &[(String, String)],
    ) -> Result<VpsInstance, ProvisionError> {
        let labels: HashMap<&str, &str> = labels
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let body = serde_json::json!({
            "name": name,
            "location": location,
            "server_type": server_type,
            "user_data": user_data,
            "labels": labels,
        });
        let resp = self.api.post_json(client, "/servers", &body).await?;
        let s: ServerJson = resp.json().await?;
        Ok(VpsInstance {
            server_id: s.id,
            ipv4: s.ipv4,
        })
    }

    async fn delete_server(
        &self,
        client: &reqwest::Client,
        instance: &VpsInstance,
    ) -> Result<(), ProvisionError> {
        let resp = self
            .api
            .delete_raw(client, &format!("/servers/{}", instance.server_id))
            .await?;
        crate::vps::finish_delete(resp).await
    }

    async fn list_server_types(
        &self,
        client: &reqwest::Client,
        _curated_ids: &[&str],
    ) -> Result<Vec<ServerTypeInfo>, ProvisionError> {
        let resp = self.api.get(client, "/server_types", &[]).await?;
        let r: ServerTypesResponse = resp.json().await?;
        Ok(r.server_types
            .into_iter()
            .take(MAX_OFFERED_SERVER_TYPES)
            .map(|t| ServerTypeInfo {
                id: t.id,
                vcpu: t.vcpu,
                mem_gb: t.mem_gb,
                disk_gb: t.disk_gb,
                price_monthly_cents: t.price_monthly_cents,
                currency: t.currency,
            })
            .collect())
    }

    async fn set_ptr(
        &self,
        client: &reqwest::Client,
        instance: &VpsInstance,
        fqdn: &str,
    ) -> Result<(), ProvisionError> {
        self.api
            .put_json(
                client,
                &format!("/servers/{}/ptr", instance.server_id),
                &serde_json::json!({ "ptr": fqdn }),
            )
            .await?;
        Ok(())
    }

    async fn find_server_by_name(
        &self,
        client: &reqwest::Client,
        name: &str,
    ) -> Result<Option<VpsInstance>, ProvisionError> {
        let resp = self.api.get(client, "/servers", &[("name", name)]).await?;
        let r: ServersResponse = resp.json().await?;
        Ok(r.servers.into_iter().next().map(|s| VpsInstance {
            server_id: s.id,
            ipv4: s.ipv4,
        }))
    }

    /// Spec § Exit guarantee 2 — the server list is enumerable by the user's
    /// own token through `GET /v1/servers?label=managed-by=fauna`.
    ///
    /// The v1 `Server` shape carries no IPv6, no creation instant and no
    /// paging envelope (spec § Endpoints / § Shapes), so those are `None` and
    /// the single response is the whole list; v1 evolves additively, so a
    /// later implementation adding them cannot break this parse.
    async fn list_managed_servers(
        &self,
        client: &reqwest::Client,
    ) -> Result<Vec<ManagedServer>, ProvisionError> {
        let resp = self
            .api
            .get(
                client,
                "/servers",
                &[("label", &crate::vps::marker_selector())],
            )
            .await?;
        let r: ServersResponse = resp.json().await?;
        Ok(r.servers
            .into_iter()
            .filter_map(|s| {
                let labels: Vec<(String, String)> = s.labels.into_iter().collect();
                // Re-check the marker: `?label=` is the intermediary's filter,
                // but the guarantee is ours.
                if !crate::vps::has_marker(&labels) {
                    return None;
                }
                Some(ManagedServer {
                    server_id: s.id,
                    name: s.name,
                    ipv4: Some(s.ipv4).filter(|ip| !ip.is_empty()),
                    ipv6: s.ipv6.filter(|ip| !ip.is_empty()),
                    labels,
                    created_at: None,
                    marked: true,
                })
            })
            .collect())
    }

    async fn get_ptr(
        &self,
        client: &reqwest::Client,
        instance: &VpsInstance,
    ) -> Result<Option<String>, ProvisionError> {
        let resp = self
            .api
            .get(client, &format!("/servers/{}/ptr", instance.server_id), &[])
            .await?;
        let r: PtrResponse = resp.json().await?;
        Ok(r.ptr.filter(|p| !p.is_empty()))
    }

    fn base_url(&self) -> &str {
        self.api.base_url()
    }
}
