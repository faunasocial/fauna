//! OVH Public Cloud — a location is `<project>/<region>`, every server id is
//! `<project>/<instance>`, offers are curated flavor names priced from the
//! public order catalogue, and create resolves the flavor and image *ids* in
//! the region (`docs/goal/architecture/provisioning/registry.md` § Server
//! offers follow the location → *OVH: a location is a project and a region*).

use reqwest::Client;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::error::{ProvisionError, ensure_success};
use crate::vps::{ManagedServer, ServerTypeInfo, VpsInstance, VpsLocation, VpsProvider};

const API_BASE: &str = "https://api.ovh.com";

/// The name of the Ubuntu 26.04 LTS image in a region's linux image list —
/// every server Fauna creates runs it (`vps.md` § Minimum VPS Requirements).
/// Read off OVH's published API shape, not yet a signed listing: the live
/// witness run with an OVH credential confirms the exact string
/// (`registry.md` § Server offers follow the location → *OVH*, the witness
/// bullet).
pub const OVH_UBUNTU_IMAGE_NAME: &str = "Ubuntu 26.04";

/// OVH Cloud VPS provider. Holds credentials only: the project travels in
/// every location and server id, so the adapter keeps no chosen-project state.
pub struct Ovh {
    pub app_key: String,
    pub app_secret: String,
    pub consumer_key: String,
    pub api_base: String,
}

/// Split a `<project>/<region>` location (or a `<project>/<instance>` server
/// id) into its two halves.
fn split_location(id: &str) -> Result<(&str, &str), ProvisionError> {
    match id.split_once('/') {
        Some((project, rest)) if !project.is_empty() && !rest.is_empty() => Ok((project, rest)),
        _ => Err(ProvisionError::Other(format!(
            "OVH id {id:?} is not of the form <project>/<region-or-instance>"
        ))),
    }
}

impl Ovh {
    pub fn new(app_key: String, app_secret: String, consumer_key: String) -> Self {
        Self::with_base_url(app_key, app_secret, consumer_key, API_BASE.to_string())
    }
    pub fn with_base_url(
        app_key: String,
        app_secret: String,
        consumer_key: String,
        api_base: String,
    ) -> Self {
        Self {
            app_key,
            app_secret,
            consumer_key,
            api_base,
        }
    }

    fn sign(&self, method: &str, url: &str, body: &str, timestamp: u64) -> String {
        let pre_hash = format!(
            "{}+{}+{}+{}+{}+{}",
            self.app_secret, self.consumer_key, method, url, body, timestamp
        );
        let digest = sha1_smol::Sha1::from(pre_hash).digest();
        format!("$1${}", digest)
    }

    async fn get_timestamp(&self, client: &Client) -> Result<u64, ProvisionError> {
        let resp = client
            .get(format!("{}/1.0/auth/time", self.api_base))
            .send()
            .await?;

        let resp = ensure_success(resp).await?;

        let ts: u64 = resp.json().await?;
        Ok(ts)
    }

    /// A signed GET whose 2xx body is parsed as `T`.
    async fn get_json<T: DeserializeOwned>(
        &self,
        client: &Client,
        url: &str,
        timestamp: u64,
    ) -> Result<T, ProvisionError> {
        let resp = self.signed_get(client, url, timestamp).await?;
        let resp = ensure_success(resp).await?;
        Ok(resp.json().await?)
    }

    /// The account's subsidiary (`GET /1.0/me`) — which public catalogue
    /// prices it; a consumer key that cannot read it is not a usable
    /// credential.
    async fn subsidiary(&self, client: &Client, timestamp: u64) -> Result<String, ProvisionError> {
        let me: OvhMe = self
            .get_json(client, &format!("{}/1.0/me", self.api_base), timestamp)
            .await?;
        Ok(me.ovh_subsidiary)
    }

    /// The project ids the credentials can see (`GET /1.0/cloud/project`),
    /// bounded by [`crate::vps::MAX_LIST_PAGES`].
    async fn projects(
        &self,
        client: &Client,
        timestamp: u64,
    ) -> Result<Vec<String>, ProvisionError> {
        let url = format!("{}/1.0/cloud/project", self.api_base);
        let mut projects: Vec<String> = self.get_json(client, &url, timestamp).await?;
        projects.truncate(crate::vps::MAX_LIST_PAGES as usize);
        Ok(projects)
    }

    /// The region's flavors (`GET /1.0/cloud/project/{p}/flavor?region=<r>`).
    async fn flavors(
        &self,
        client: &Client,
        project: &str,
        region: &str,
        timestamp: u64,
    ) -> Result<Vec<OvhFlavor>, ProvisionError> {
        let url = format!(
            "{}/1.0/cloud/project/{project}/flavor?region={region}",
            self.api_base
        );
        self.get_json(client, &url, timestamp).await
    }

    /// Every instance of one project (the whole project in one array — OVH
    /// does not paginate it).
    async fn instances(
        &self,
        client: &Client,
        project: &str,
        timestamp: u64,
    ) -> Result<Vec<OvhInstance>, ProvisionError> {
        let url = format!("{}/1.0/cloud/project/{project}/instance", self.api_base);
        self.get_json(client, &url, timestamp).await
    }

    async fn signed_get(
        &self,
        client: &Client,
        url: &str,
        timestamp: u64,
    ) -> Result<reqwest::Response, ProvisionError> {
        let sig = self.sign("GET", url, "", timestamp);
        let resp = client
            .get(url)
            .header("X-Ovh-Application", &self.app_key)
            .header("X-Ovh-Consumer", &self.consumer_key)
            .header("X-Ovh-Timestamp", timestamp.to_string())
            .header("X-Ovh-Signature", sig)
            .send()
            .await?;
        Ok(resp)
    }

    async fn signed_post(
        &self,
        client: &Client,
        url: &str,
        body_json: &str,
        timestamp: u64,
    ) -> Result<reqwest::Response, ProvisionError> {
        let sig = self.sign("POST", url, body_json, timestamp);
        let resp = client
            .post(url)
            .header("X-Ovh-Application", &self.app_key)
            .header("X-Ovh-Consumer", &self.consumer_key)
            .header("X-Ovh-Timestamp", timestamp.to_string())
            .header("X-Ovh-Signature", sig)
            .header("Content-Type", "application/json")
            .body(body_json.to_string())
            .send()
            .await?;
        Ok(resp)
    }

    async fn signed_delete(
        &self,
        client: &Client,
        url: &str,
        timestamp: u64,
    ) -> Result<reqwest::Response, ProvisionError> {
        let sig = self.sign("DELETE", url, "", timestamp);
        let resp = client
            .delete(url)
            .header("X-Ovh-Application", &self.app_key)
            .header("X-Ovh-Consumer", &self.consumer_key)
            .header("X-Ovh-Timestamp", timestamp.to_string())
            .header("X-Ovh-Signature", sig)
            .send()
            .await?;
        Ok(resp)
    }
}

#[derive(Deserialize)]
struct OvhMe {
    #[serde(rename = "ovhSubsidiary")]
    ovh_subsidiary: String,
}

#[derive(Deserialize)]
struct OvhProject {
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct OvhRegion {
    name: String,
    #[serde(default)]
    status: String,
    #[serde(rename = "type", default)]
    region_type: String,
    #[serde(rename = "datacenterLocation", default)]
    datacenter_location: String,
    #[serde(rename = "countryCode", default)]
    country_code: String,
}

#[derive(Deserialize)]
struct OvhFlavor {
    id: String,
    name: String,
    #[serde(rename = "osType", default)]
    os_type: String,
    /// Absent reads as available: OVH lists the field, a missing one is no
    /// reason to hide an offer.
    #[serde(default)]
    available: Option<bool>,
    #[serde(default)]
    vcpus: u32,
    /// GB.
    #[serde(default)]
    ram: f32,
    /// GB.
    #[serde(default)]
    disk: u32,
    #[serde(rename = "planCodes", default)]
    plan_codes: OvhPlanCodes,
}

impl OvhFlavor {
    fn is_linux_offer(&self, name: &str) -> bool {
        self.name == name && self.os_type == "linux" && self.available.unwrap_or(true)
    }
}

#[derive(Deserialize, Default)]
struct OvhPlanCodes {
    #[serde(default)]
    monthly: Option<String>,
}

#[derive(Deserialize)]
struct OvhImage {
    id: String,
    #[serde(default)]
    name: String,
}

/// The public order catalogue (`GET /1.0/order/catalog/public/cloud`) — about
/// 8 MB of JSON, of which only the currency and the addons' prices are read.
#[derive(Deserialize)]
struct OvhCatalogue {
    locale: OvhCatalogueLocale,
    #[serde(default)]
    addons: Vec<OvhCatalogueAddon>,
}

#[derive(Deserialize)]
struct OvhCatalogueLocale {
    #[serde(rename = "currencyCode")]
    currency_code: String,
}

#[derive(Deserialize)]
struct OvhCatalogueAddon {
    #[serde(rename = "planCode")]
    plan_code: String,
    #[serde(default)]
    pricings: Vec<OvhCataloguePricing>,
}

#[derive(Deserialize)]
struct OvhCataloguePricing {
    #[serde(default)]
    interval: u32,
    #[serde(rename = "intervalUnit", default)]
    interval_unit: String,
    /// 10⁻⁸ of the currency unit (micro-cents).
    #[serde(default)]
    price: u64,
    #[serde(default)]
    tax: u64,
}

impl OvhCatalogue {
    /// The gross monthly price, in cents, of a monthly plan code.
    fn monthly_cents(&self, plan_code: &str) -> Option<u64> {
        let addon = self.addons.iter().find(|a| a.plan_code == plan_code)?;
        let pricing = addon
            .pricings
            .iter()
            .find(|p| p.interval == 1 && p.interval_unit == "month")?;
        Some((pricing.price + pricing.tax) / 1_000_000)
    }
}

#[derive(Deserialize)]
struct OvhInstance {
    id: String,
    #[serde(default)]
    name: String,
    /// Creation instant, list responses only.
    #[serde(default)]
    created: Option<String>,
    #[serde(rename = "ipAddresses")]
    ip_addresses: Vec<OvhIpAddress>,
}

impl OvhInstance {
    fn public_ip(&self, version: u8) -> Option<String> {
        self.ip_addresses
            .iter()
            .find(|a| a.version == version && a.addr_type == "public")
            .map(|a| a.ip.clone())
    }
}

#[derive(Deserialize)]
struct OvhReverseEntry {
    #[serde(rename = "reverse")]
    reverse: Option<String>,
}

#[derive(Deserialize)]
struct OvhIpAddress {
    ip: String,
    version: u8,
    #[serde(rename = "type")]
    addr_type: String,
}

impl VpsProvider for Ovh {
    /// Every `UP` region of type `region` in every project the credential
    /// sees, as `<project>/<region>` locations — `localzone` and
    /// `region-3-az` are priced apart, and the latter needs an availability
    /// zone the create body does not carry. The project's description
    /// prefixes the name only when more than one project is seen.
    async fn verify(&self, client: &Client) -> Result<Vec<VpsLocation>, ProvisionError> {
        let timestamp = self.get_timestamp(client).await?;
        self.subsidiary(client, timestamp).await?;
        let projects = self.projects(client, timestamp).await?;
        let several = projects.len() > 1;
        let mut out = Vec::new();
        for project in &projects {
            let base = format!("{}/1.0/cloud/project/{project}", self.api_base);
            let prefix = if several {
                let detail: OvhProject = self.get_json(client, &base, timestamp).await?;
                let label = detail
                    .description
                    .filter(|d| !d.is_empty())
                    .unwrap_or_else(|| project.clone());
                format!("{label} · ")
            } else {
                String::new()
            };
            let regions: Vec<String> = self
                .get_json(client, &format!("{base}/region"), timestamp)
                .await?;
            for region in regions
                .into_iter()
                .take(crate::vps::MAX_LIST_PAGES as usize)
            {
                let detail: OvhRegion = self
                    .get_json(client, &format!("{base}/region/{region}"), timestamp)
                    .await?;
                if detail.status != "UP" || detail.region_type != "region" {
                    continue;
                }
                out.push(VpsLocation {
                    id: format!("{project}/{}", detail.name),
                    name: format!("{prefix}{} ({})", detail.datacenter_location, detail.name),
                    city: detail.datacenter_location,
                    country: detail.country_code,
                });
            }
        }
        Ok(out)
    }

    async fn create_server(
        &self,
        client: &Client,
        name: &str,
        location: &str,
        server_type: &str,
        user_data: &str,
        // OVH's `POST /cloud/project/{id}/instance` has no arbitrary label/tag
        // field, so labels are ignored here (see `list_managed_servers`).
        _labels: &[(String, String)],
    ) -> Result<VpsInstance, ProvisionError> {
        let (project, region) = split_location(location)?;
        let timestamp = self.get_timestamp(client).await?;

        let flavor_id = self
            .flavors(client, project, region, timestamp)
            .await?
            .into_iter()
            .find(|f| f.is_linux_offer(server_type))
            .map(|f| f.id)
            .ok_or_else(|| {
                ProvisionError::Other(format!(
                    "OVH region {region} offers no linux flavor {server_type}"
                ))
            })?;

        let images_url = format!(
            "{}/1.0/cloud/project/{project}/image?region={region}&osType=linux",
            self.api_base
        );
        let images: Vec<OvhImage> = self.get_json(client, &images_url, timestamp).await?;
        let image_id = images
            .into_iter()
            .find(|i| i.name == OVH_UBUNTU_IMAGE_NAME)
            .map(|i| i.id)
            .ok_or_else(|| {
                ProvisionError::Other(format!(
                    "OVH region {region} has no {OVH_UBUNTU_IMAGE_NAME} image; \
                     every server Fauna creates runs Ubuntu 26.04 LTS — pick another region"
                ))
            })?;

        let url = format!("{}/1.0/cloud/project/{project}/instance", self.api_base);
        let body = serde_json::json!({
            "name": name,
            "flavorId": flavor_id,
            "imageId": image_id,
            "region": region,
            "userData": user_data,
            // The monthly plan is the price the picker showed.
            "monthlyBilling": true,
        });
        let body_str = serde_json::to_string(&body).map_err(ProvisionError::parse)?;

        let resp = self.signed_post(client, &url, &body_str, timestamp).await?;

        let resp = ensure_success(resp).await?;

        let instance: OvhInstance = resp.json().await?;
        let ipv4 = instance
            .public_ip(4)
            .ok_or_else(|| ProvisionError::Parse("no public IPv4 address found".into()))?;

        Ok(VpsInstance {
            server_id: format!("{project}/{}", instance.id),
            ipv4,
        })
    }

    async fn delete_server(
        &self,
        client: &Client,
        instance: &VpsInstance,
    ) -> Result<(), ProvisionError> {
        let (project, instance_id) = split_location(&instance.server_id)?;
        let timestamp = self.get_timestamp(client).await?;
        let url = format!(
            "{}/1.0/cloud/project/{project}/instance/{instance_id}",
            self.api_base
        );
        let resp = self.signed_delete(client, &url, timestamp).await?;
        crate::vps::finish_delete(resp).await
    }

    /// The curated flavors as the chosen `<project>/<region>` sells them: the
    /// region's linux, available flavor of each curated name, joined on its
    /// monthly plan code to the public catalogue of the account's subsidiary
    /// for the gross monthly price. A curated name absent from the region,
    /// or with no monthly plan in the catalogue, is not offered.
    async fn list_server_types(
        &self,
        client: &Client,
        curated_ids: &[&str],
        location: &str,
    ) -> Result<Vec<ServerTypeInfo>, ProvisionError> {
        let (project, region) = split_location(location)?;
        let timestamp = self.get_timestamp(client).await?;
        let subsidiary = self.subsidiary(client, timestamp).await?;
        let flavors = self.flavors(client, project, region, timestamp).await?;

        // Public and unauthenticated — no signature.
        let resp = client
            .get(format!("{}/1.0/order/catalog/public/cloud", self.api_base))
            .query(&[("ovhSubsidiary", subsidiary.as_str())])
            .send()
            .await?;
        let catalogue: OvhCatalogue = ensure_success(resp).await?.json().await?;

        Ok(curated_ids
            .iter()
            .filter_map(|name| {
                let flavor = flavors.iter().find(|f| f.is_linux_offer(name))?;
                let plan = flavor.plan_codes.monthly.as_deref()?;
                let price_monthly_cents = catalogue.monthly_cents(plan)?;
                Some(ServerTypeInfo {
                    id: flavor.name.clone(),
                    vcpu: flavor.vcpus,
                    mem_gb: flavor.ram,
                    disk_gb: flavor.disk,
                    price_monthly_cents,
                    currency: catalogue.locale.currency_code.clone(),
                })
            })
            .collect())
    }

    async fn set_ptr(
        &self,
        client: &Client,
        instance: &VpsInstance,
        fqdn: &str,
    ) -> Result<(), ProvisionError> {
        let timestamp = self.get_timestamp(client).await?;
        let url = format!("{}/1.0/ip/{}/reverse", self.api_base, instance.ipv4);
        let body = serde_json::json!({
            "ipReverse": instance.ipv4,
            "reverse": fqdn,
        });
        let body_str = serde_json::to_string(&body)
            .map_err(|e| ProvisionError::Other(format!("serialize ptr body: {e}")))?;

        let resp = self.signed_post(client, &url, &body_str, timestamp).await?;

        ensure_success(resp).await?;
        Ok(())
    }

    /// Walks every project the credential sees (bounded by
    /// [`crate::vps::MAX_LIST_PAGES`]); the match's id is
    /// `<project>/<instance>`.
    async fn find_server_by_name(
        &self,
        client: &Client,
        name: &str,
    ) -> Result<Option<VpsInstance>, ProvisionError> {
        let timestamp = self.get_timestamp(client).await?;
        for project in self.projects(client, timestamp).await? {
            let instances = self.instances(client, &project, timestamp).await?;
            let found = crate::vps::find_matching_server(
                instances,
                |inst| inst.name == name,
                |inst| {
                    inst.public_ip(4).map(|ipv4| VpsInstance {
                        server_id: format!("{project}/{}", inst.id),
                        ipv4,
                    })
                },
            );
            if found.is_some() {
                return Ok(found);
            }
        }
        Ok(None)
    }

    /// **The deliberate exception to marker filtering.** OVH's
    /// instance-create API has no label or tag field at all (see
    /// `create_server`), so there is nothing to filter on: this returns the
    /// account's servers *unfiltered*, every row `marked: false`. The retire
    /// view shows those behind an unmarked note, and its mandatory typed
    /// per-box confirm — the same one every provider gets — is what
    /// discharges the risk of a non-fauna box appearing in the list
    /// (`vps.md` § Uninstall → *Listing primitive*).
    ///
    /// It walks every project the credentials see, and each row's
    /// `server_id` is `<project>/<instance>`, so `delete_server` reaches the
    /// right project. Each project's endpoint returns the whole project in
    /// one array, so there is no pagination to follow; the project walk is
    /// bounded by [`crate::vps::MAX_LIST_PAGES`].
    async fn list_managed_servers(
        &self,
        client: &Client,
    ) -> Result<Vec<ManagedServer>, ProvisionError> {
        let timestamp = self.get_timestamp(client).await?;
        let mut out = Vec::new();
        for project in self.projects(client, timestamp).await? {
            let instances = self.instances(client, &project, timestamp).await?;
            out.extend(instances.into_iter().map(|inst| ManagedServer {
                server_id: format!("{project}/{}", inst.id),
                ipv4: inst.public_ip(4),
                ipv6: inst.public_ip(6),
                name: inst.name,
                labels: Vec::new(),
                created_at: inst.created,
                marked: false,
            }));
        }
        Ok(out)
    }

    async fn get_ptr(
        &self,
        client: &Client,
        instance: &VpsInstance,
    ) -> Result<Option<String>, ProvisionError> {
        let timestamp = self.get_timestamp(client).await?;
        let url = format!(
            "{}/1.0/ip/{}/reverse/{}",
            self.api_base, instance.ipv4, instance.ipv4
        );
        let resp = self.signed_get(client, &url, timestamp).await?;

        // Many OVH "reverse not set" responses come back as 404; treat as None.
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        let resp = ensure_success(resp).await?;

        let entry: OvhReverseEntry = resp.json().await?;
        Ok(entry.reverse.filter(|s| !s.is_empty()))
    }

    fn base_url(&self) -> &str {
        &self.api_base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ovh() -> Ovh {
        Ovh::new(
            "myAppKey".into(),
            "myAppSecret".into(),
            "myConsumerKey".into(),
        )
    }

    #[test]
    fn test_ovh_signature_format() {
        let ovh = make_ovh();
        let sig = ovh.sign(
            "GET",
            "https://api.ovh.com/1.0/cloud/project",
            "",
            1234567890,
        );
        assert!(sig.starts_with("$1$"), "signature must start with $1$");
        // $1$ prefix (3) + 40 hex chars (SHA1)
        assert_eq!(sig.len(), 43, "signature must be $1$ + 40 hex SHA1 chars");
    }

    #[test]
    fn test_ovh_signature_is_deterministic() {
        let ovh = make_ovh();
        let sig1 = ovh.sign(
            "POST",
            "https://api.ovh.com/1.0/cloud/project/abc/instance",
            "{}",
            9999,
        );
        let sig2 = ovh.sign(
            "POST",
            "https://api.ovh.com/1.0/cloud/project/abc/instance",
            "{}",
            9999,
        );
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn test_ovh_parses_instance_response() {
        let json = r#"{
            "id": "instance-uuid-1234",
            "ipAddresses": [
                { "ip": "10.0.0.5", "version": 4, "type": "private" },
                { "ip": "198.51.100.7", "version": 4, "type": "public" },
                { "ip": "2001:db8::1", "version": 6, "type": "public" }
            ]
        }"#;
        let parsed: OvhInstance = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.id, "instance-uuid-1234");
        assert_eq!(parsed.public_ip(4).as_deref(), Some("198.51.100.7"));
    }

    #[test]
    fn split_location_takes_project_and_region() {
        assert_eq!(split_location("proj-1/GRA11").unwrap(), ("proj-1", "GRA11"));
        assert!(split_location("GRA11").is_err());
        assert!(split_location("/GRA11").is_err());
        assert!(split_location("proj-1/").is_err());
    }

    #[test]
    fn catalogue_price_is_gross_monthly_in_cents() {
        let catalogue: OvhCatalogue = serde_json::from_value(serde_json::json!({
            "locale": { "currencyCode": "EUR", "subsidiary": "IE" },
            "plans": [{ "ignored": true }],
            "addons": [{
                "planCode": "d2-2.monthly.postpaid",
                "invoiceName": "d2-2",
                "pricings": [
                    { "interval": 0, "intervalUnit": "none", "price": 1, "tax": 0 },
                    { "interval": 1, "intervalUnit": "month", "price": 571000000, "tax": 131330000 }
                ]
            }]
        }))
        .unwrap();
        assert_eq!(catalogue.monthly_cents("d2-2.monthly.postpaid"), Some(702));
        assert_eq!(catalogue.monthly_cents("b2-7.monthly.postpaid"), None);
    }
}
