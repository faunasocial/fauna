use reqwest::Client;
use serde::Deserialize;

use crate::error::{ProvisionError, ensure_success};
use crate::vps::{ManagedServer, ServerTypeInfo, VpsInstance, VpsLocation, VpsProvider};

const API_BASE: &str = "https://api.ovh.com";

/// OVH Cloud VPS provider.
pub struct Ovh {
    pub app_key: String,
    pub app_secret: String,
    pub consumer_key: String,
    pub project_id: String,
    pub api_base: String,
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
            project_id: String::new(), // set after project selection (post_verify_select)
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
    async fn verify(&self, client: &Client) -> Result<Vec<VpsLocation>, ProvisionError> {
        let timestamp = self.get_timestamp(client).await?;
        let url = format!("{}/1.0/cloud/project", self.api_base);
        let resp = self.signed_get(client, &url, timestamp).await?;

        let resp = ensure_success(resp).await?;

        // Returns a list of project IDs; we return the project as a single location
        let projects: Vec<String> = resp.json().await?;
        Ok(projects
            .into_iter()
            .map(|p| VpsLocation {
                id: p.clone(),
                name: p,
                city: String::new(),
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
        // OVH's `POST /cloud/project/{id}/instance` has no arbitrary label/tag
        // field, so labels are ignored here (documented no-op, like
        // `list_server_types`). OVH is not the e2e sweep target; production
        // passes empty labels anyway.
        _labels: &[(String, String)],
    ) -> Result<VpsInstance, ProvisionError> {
        let timestamp = self.get_timestamp(client).await?;
        let url = format!(
            "{}/1.0/cloud/project/{}/instance",
            self.api_base, self.project_id
        );

        let body = serde_json::json!({
            "name": name,
            "flavorId": server_type,
            "region": location,
            "imageId": "ubuntu-24.04",
            "userData": user_data,
        });
        let body_str = serde_json::to_string(&body).map_err(ProvisionError::parse)?;

        let resp = self.signed_post(client, &url, &body_str, timestamp).await?;

        let resp = ensure_success(resp).await?;

        let instance: OvhInstance = resp.json().await?;
        let ipv4 = instance
            .ip_addresses
            .into_iter()
            .find(|a| a.version == 4 && a.addr_type == "public")
            .map(|a| a.ip)
            .ok_or_else(|| ProvisionError::Parse("no public IPv4 address found".into()))?;

        Ok(VpsInstance {
            server_id: instance.id,
            ipv4,
        })
    }

    async fn delete_server(
        &self,
        client: &Client,
        instance: &VpsInstance,
    ) -> Result<(), ProvisionError> {
        let timestamp = self.get_timestamp(client).await?;
        let url = format!(
            "{}/1.0/cloud/project/{}/instance/{}",
            self.api_base, self.project_id, instance.server_id
        );
        let resp = self.signed_delete(client, &url, timestamp).await?;
        crate::vps::finish_delete(resp).await
    }

    async fn list_server_types(
        &self,
        _client: &Client,
        _curated_ids: &[&str],
    ) -> Result<Vec<ServerTypeInfo>, ProvisionError> {
        // OVH catalog is project- and region-scoped with per-region currency,
        // and the cloud API has no endpoint that returns flavor pricing
        // (verified — `cloud.instance.InstancePrice` is defined in
        // `/1.0/cloud.json` but no path uses it as a response type).
        // Returning empty hides OVH from the curated picker without breaking
        // the trait. Tracked: docs/goal/architecture/provisioning/registry.md.
        Ok(vec![])
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

    async fn find_server_by_name(
        &self,
        client: &Client,
        name: &str,
    ) -> Result<Option<VpsInstance>, ProvisionError> {
        // OVH /cloud/project/{id}/instance returns the instance list; filter by name.
        let timestamp = self.get_timestamp(client).await?;
        let url = format!(
            "{}/1.0/cloud/project/{}/instance",
            self.api_base, self.project_id
        );
        let resp = self.signed_get(client, &url, timestamp).await?;

        let resp = ensure_success(resp).await?;

        let instances: Vec<OvhInstance> = resp.json().await?;
        Ok(crate::vps::find_matching_server(
            instances,
            |inst| inst.name == name,
            |inst| {
                inst.ip_addresses
                    .into_iter()
                    .find(|a| a.version == 4 && a.addr_type == "public")
                    .map(|addr| VpsInstance {
                        server_id: inst.id,
                        ipv4: addr.ip,
                    })
            },
        ))
    }

    /// **The deliberate exception to marker filtering.** OVH's
    /// instance-create API has no label or tag field at all (see
    /// `create_server`), so there is nothing to filter on: this returns the
    /// project list *unfiltered*, every row `marked: false`. The retire view
    /// shows those behind an unmarked note, and its mandatory typed per-box
    /// confirm — the same one every provider gets — is what discharges the
    /// risk of a non-fauna box appearing in the list
    /// (`vps.md` § Uninstall → *Listing primitive*).
    ///
    /// The endpoint returns the whole project in one array, so there is no
    /// pagination to follow.
    async fn list_managed_servers(
        &self,
        client: &Client,
    ) -> Result<Vec<ManagedServer>, ProvisionError> {
        let timestamp = self.get_timestamp(client).await?;
        let url = format!(
            "{}/1.0/cloud/project/{}/instance",
            self.api_base, self.project_id
        );
        let resp = self.signed_get(client, &url, timestamp).await?;
        let resp = ensure_success(resp).await?;

        let instances: Vec<OvhInstance> = resp.json().await?;
        Ok(instances
            .into_iter()
            .map(|inst| {
                let ipv4 = inst
                    .ip_addresses
                    .iter()
                    .find(|a| a.version == 4 && a.addr_type == "public")
                    .map(|a| a.ip.clone());
                let ipv6 = inst
                    .ip_addresses
                    .iter()
                    .find(|a| a.version == 6 && a.addr_type == "public")
                    .map(|a| a.ip.clone());
                ManagedServer {
                    server_id: inst.id,
                    name: inst.name,
                    ipv4,
                    ipv6,
                    labels: Vec::new(),
                    created_at: inst.created,
                    marked: false,
                }
            })
            .collect())
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
        Ovh {
            app_key: "myAppKey".into(),
            app_secret: "myAppSecret".into(),
            consumer_key: "myConsumerKey".into(),
            project_id: "myProjectId".into(),
            api_base: API_BASE.to_string(),
        }
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
        let public_v4 = parsed
            .ip_addresses
            .iter()
            .find(|a| a.version == 4 && a.addr_type == "public")
            .unwrap();
        assert_eq!(public_v4.ip, "198.51.100.7");
    }
}
