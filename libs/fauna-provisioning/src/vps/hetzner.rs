use std::collections::HashMap;

use reqwest::Client;
use serde::Deserialize;

use crate::error::ProvisionError;
use crate::vps::{ManagedServer, ServerTypeInfo, VpsInstance, VpsLocation, VpsProvider};

const API_BASE: &str = "https://api.hetzner.cloud/v1";

/// Hetzner Cloud VPS provider.
pub struct Hetzner {
    pub token: String,
    pub api_base: String,
}

/// `GET /locations`. The wizard's verify used to read `GET /datacenters`,
/// which the provider REMOVED (it answers `410 deprecated_api_endpoint`): a
/// server is placed by location now, and the location list is the one live
/// source of them.
#[derive(Deserialize)]
struct LocationsResponse {
    locations: Vec<HetznerLocation>,
}

#[derive(Deserialize)]
struct HetznerLocation {
    /// The location id `create_server` takes, e.g. `fsn1`.
    name: String,
    /// The human-readable name, e.g. `Falkenstein DC Park 1`.
    #[serde(default)]
    description: String,
    city: String,
    country: String,
}

impl From<HetznerLocation> for VpsLocation {
    fn from(l: HetznerLocation) -> Self {
        VpsLocation {
            // The display name is what the location picker shows and selects
            // by; fall back to the city when the provider sends none.
            name: if l.description.is_empty() {
                l.city.clone()
            } else {
                l.description
            },
            id: l.name,
            city: l.city,
            country: l.country,
        }
    }
}

#[derive(Deserialize)]
struct CreateServerResponse {
    server: HetznerServer,
}

#[derive(Deserialize)]
struct HetznerServer {
    id: u64,
    /// Present on list/get responses, absent from the create echo we parse —
    /// `default` rather than `Option` because a nameless server is a name we
    /// simply do not know, and the list row renders an empty string.
    #[serde(default)]
    name: String,
    /// Hetzner's native string→string label map (`create_server` sends it).
    #[serde(default)]
    labels: HashMap<String, String>,
    /// RFC 3339 creation instant, list responses only.
    #[serde(default)]
    created: Option<String>,
    public_net: HetznerPublicNet,
}

#[derive(Deserialize)]
struct HetznerPublicNet {
    ipv4: HetznerIpv4,
    #[serde(default)]
    ipv6: Option<HetznerIpv6>,
}

#[derive(Deserialize)]
struct HetznerIpv4 {
    ip: String,
    #[serde(default)]
    dns_ptr: Option<String>,
}

#[derive(Deserialize)]
struct HetznerIpv6 {
    #[serde(default)]
    ip: String,
}

#[derive(Deserialize)]
struct ServersResponse {
    servers: Vec<HetznerServer>,
}

/// `GET /servers` paging envelope — `meta.pagination.next_page` is `null` on
/// the last page.
#[derive(Deserialize)]
struct ListServersResponse {
    #[serde(default)]
    servers: Vec<HetznerServer>,
    #[serde(default)]
    meta: Option<HetznerMeta>,
}

#[derive(Deserialize)]
struct HetznerMeta {
    #[serde(default)]
    pagination: Option<HetznerPagination>,
}

#[derive(Deserialize)]
struct HetznerPagination {
    #[serde(default)]
    next_page: Option<u32>,
}

#[derive(Deserialize)]
struct GetServerResponse {
    server: HetznerServer,
}

#[derive(Deserialize)]
struct ServerTypesResponse {
    server_types: Vec<HetznerServerType>,
}

#[derive(Deserialize)]
struct HetznerServerType {
    name: String,
    cores: u32,
    memory: f32, // GB
    disk: u32,   // GB
    prices: Vec<HetznerPriceEntry>,
}

#[derive(Deserialize)]
struct HetznerPriceEntry {
    #[allow(dead_code)]
    location: String,
    price_monthly: HetznerPrice,
}

#[derive(Deserialize)]
struct HetznerPrice {
    /// EUR amount as decimal string, e.g. "4.51". Hetzner returns both
    /// `net` and `gross`; we use gross for the user-facing price.
    gross: String,
}

impl Hetzner {
    pub fn new(token: String) -> Self {
        Self::with_base_url(token, API_BASE.to_string())
    }
    pub fn with_base_url(token: String, api_base: String) -> Self {
        Self { token, api_base }
    }
}

impl VpsProvider for Hetzner {
    async fn verify(&self, client: &Client) -> Result<Vec<VpsLocation>, ProvisionError> {
        let resp = crate::vps::get_bearer(
            client,
            format!("{}/locations", self.api_base),
            &self.token,
            &[],
        )
        .await?;

        let data: LocationsResponse = resp.json().await?;
        Ok(data.locations.into_iter().map(VpsLocation::from).collect())
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
            "server_type": server_type,
            "location": location,
            "image": "ubuntu-24.04",
            "user_data": user_data,
        });
        // Hetzner accepts a native `labels` string→string map on create.
        if !labels.is_empty() {
            let map: serde_json::Map<String, serde_json::Value> = labels
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect();
            body["labels"] = serde_json::Value::Object(map);
        }

        let resp = crate::vps::json_bearer(
            client,
            reqwest::Method::POST,
            format!("{}/servers", self.api_base),
            &self.token,
            body,
        )
        .await?;

        let data: CreateServerResponse = resp.json().await?;
        Ok(VpsInstance {
            server_id: data.server.id.to_string(),
            ipv4: data.server.public_net.ipv4.ip,
        })
    }

    async fn delete_server(
        &self,
        client: &Client,
        instance: &VpsInstance,
    ) -> Result<(), ProvisionError> {
        crate::vps::delete_server_bearer(
            client,
            format!("{}/servers/{}", self.api_base, instance.server_id),
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
            format!("{}/server_types", self.api_base),
            &self.token,
            &[],
        )
        .await?;

        let data: ServerTypesResponse = resp.json().await?;
        let mut out = Vec::with_capacity(curated_ids.len());
        for curated in curated_ids {
            if let Some(t) = data.server_types.iter().find(|t| t.name == *curated) {
                let first = t
                    .prices
                    .first()
                    .ok_or_else(|| ProvisionError::provider(0, "no price entries"))?;
                let cents = parse_eur_to_cents(&first.price_monthly.gross)?;
                out.push(ServerTypeInfo {
                    id: t.name.clone(),
                    vcpu: t.cores,
                    mem_gb: t.memory,
                    disk_gb: t.disk,
                    price_monthly_cents: cents,
                    currency: "EUR".to_string(),
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
        let url = format!(
            "{}/servers/{}/actions/change_dns_ptr",
            self.api_base, instance.server_id
        );
        crate::vps::set_ptr_bearer(
            client,
            reqwest::Method::POST,
            url,
            &self.token,
            serde_json::json!({
                "ip": instance.ipv4,
                "dns_ptr": fqdn,
            }),
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
            format!("{}/servers", self.api_base),
            &self.token,
            &[("name", name)],
        )
        .await?;

        let data: ServersResponse = resp.json().await?;
        Ok(data.servers.into_iter().next().map(|s| VpsInstance {
            server_id: s.id.to_string(),
            ipv4: s.public_net.ipv4.ip,
        }))
    }

    async fn list_managed_servers(
        &self,
        client: &Client,
    ) -> Result<Vec<ManagedServer>, ProvisionError> {
        let selector = crate::vps::marker_selector();
        let mut out = Vec::new();
        let mut page: u32 = 1;

        for _ in 0..crate::vps::MAX_LIST_PAGES {
            let resp = crate::vps::get_bearer(
                client,
                format!("{}/servers", self.api_base),
                &self.token,
                &[
                    ("label_selector", selector.as_str()),
                    ("page", &page.to_string()),
                    ("per_page", "50"),
                ],
            )
            .await?;

            let data: ListServersResponse = resp.json().await?;
            for s in data.servers {
                let labels: Vec<(String, String)> = s.labels.into_iter().collect();
                // Re-check the marker: the `label_selector` is Hetzner's, but
                // the guarantee is ours.
                if !crate::vps::has_marker(&labels) {
                    continue;
                }
                out.push(ManagedServer {
                    server_id: s.id.to_string(),
                    name: s.name,
                    ipv4: Some(s.public_net.ipv4.ip).filter(|ip| !ip.is_empty()),
                    ipv6: s.public_net.ipv6.map(|v| v.ip).filter(|ip| !ip.is_empty()),
                    labels,
                    created_at: s.created,
                    marked: true,
                });
            }

            match data
                .meta
                .and_then(|m| m.pagination)
                .and_then(|p| p.next_page)
            {
                Some(next) => page = next,
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
        let resp = crate::vps::get_bearer(
            client,
            format!("{}/servers/{}", self.api_base, instance.server_id),
            &self.token,
            &[],
        )
        .await?;

        let data: GetServerResponse = resp.json().await?;
        Ok(data
            .server
            .public_net
            .ipv4
            .dns_ptr
            .filter(|s| !s.is_empty()))
    }

    fn base_url(&self) -> &str {
        &self.api_base
    }
}

/// "4.51" → 451; "10" → 1000; "10.5" → 1050.
fn parse_eur_to_cents(decimal: &str) -> Result<u64, ProvisionError> {
    crate::money::parse_decimal_to_cents(decimal)
        .ok_or_else(|| ProvisionError::provider(0, format!("bad price decimal: {decimal}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `GET /locations` answers with today (read off the live API,
    /// 2026-10-04), mapped to what the wizard's location picker consumes: the
    /// id `create_server` takes, and a name a person can read.
    #[test]
    fn test_hetzner_maps_the_locations_response() {
        let json = r#"{
            "locations": [
                {
                    "id": 1, "name": "fsn1", "description": "Falkenstein DC Park 1",
                    "city": "Falkenstein", "country": "DE", "network_zone": "eu-central"
                },
                { "name": "sin", "city": "Singapore", "country": "SG" }
            ]
        }"#;
        let parsed: LocationsResponse = serde_json::from_str(json).unwrap();
        let locations: Vec<VpsLocation> = parsed
            .locations
            .into_iter()
            .map(VpsLocation::from)
            .collect();
        assert_eq!(locations.len(), 2);
        assert_eq!(locations[0].id, "fsn1");
        assert_eq!(locations[0].name, "Falkenstein DC Park 1");
        assert_eq!(locations[0].city, "Falkenstein");
        assert_eq!(locations[0].country, "DE");
        // No description: the city stands in, never an empty picker row.
        assert_eq!(locations[1].id, "sin");
        assert_eq!(locations[1].name, "Singapore");
    }

    /// `verify` reads `/locations` — and nothing at the removed
    /// `/datacenters`, which is what broke every Hetzner verify in the wizard.
    #[tokio::test]
    async fn verify_reads_locations_not_the_removed_datacenters_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/locations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "locations": [{
                    "name": "fsn1", "description": "Falkenstein DC Park 1",
                    "city": "Falkenstein", "country": "DE"
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/datacenters"))
            .respond_with(ResponseTemplate::new(410))
            .expect(0)
            .mount(&server)
            .await;
        let hetzner = Hetzner::with_base_url("tkn".into(), server.uri());
        let locations = hetzner.verify(&Client::new()).await.expect("verify");
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].id, "fsn1");
    }

    #[test]
    fn test_hetzner_parses_server_types_response() {
        let json = r#"{
            "server_types": [
                {
                    "name": "cx22",
                    "cores": 2,
                    "memory": 4.0,
                    "disk": 40,
                    "prices": [{
                        "location": "fsn1",
                        "price_monthly": { "gross": "4.51" }
                    }]
                }
            ]
        }"#;
        let parsed: ServerTypesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.server_types[0].cores, 2);
        assert_eq!(parsed.server_types[0].prices[0].price_monthly.gross, "4.51");
    }

    #[test]
    fn test_parse_eur_to_cents_full() {
        assert_eq!(parse_eur_to_cents("4.51").unwrap(), 451);
    }

    #[test]
    fn test_parse_eur_to_cents_whole() {
        assert_eq!(parse_eur_to_cents("10").unwrap(), 1000);
    }

    #[test]
    fn test_parse_eur_to_cents_one_decimal() {
        assert_eq!(parse_eur_to_cents("10.5").unwrap(), 1050);
    }

    #[test]
    fn test_hetzner_parses_create_server_response() {
        let json = r#"{
            "server": {
                "id": 12345,
                "public_net": {
                    "ipv4": { "ip": "1.2.3.4" }
                }
            }
        }"#;
        let parsed: CreateServerResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.server.id, 12345);
        assert_eq!(parsed.server.public_net.ipv4.ip, "1.2.3.4");
    }
}
