// JSON-envelope verify entry points for VPS/DNS provider credentials — the
// shared implementation behind `fauna-ffi::provisioning::{verify_vps_provider,
// verify_dns_provider}` and their `fauna-wasm-onboarding` twins, which were
// byte-identical copies of this match logic differing only in their platform
// error/async wrapper (`FfiError` + `block_on` vs `JsValue` +
// `future_to_promise`) — those two crates now keep only that thin shell.
//
// This is a distinct field-naming convention from `dispatch::{vps_provider,
// dns_provider}` (which key `Credentials` by the kebab-case `providers.yaml`
// field ids for the registrar/orchestrator flows): the wizard's verify-step
// JSON envelope predates that convention and uses its own per-provider field
// names (`token`, `app_key`, ...). Preserved as-is rather than unified onto
// `dispatch`, to avoid touching the wire contract both binding layers already
// ship.

use crate::dns::DnsProvider;
use crate::vps::VpsProvider;

fn json_str(val: &serde_json::Value, key: &str) -> Result<String, String> {
    val[key]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("missing field: {key}"))
}

/// Verify a VPS provider's credentials and return available locations.
///
/// `provider_json` must contain a `"provider"` field set to one of:
/// - hetzner / digitalocean / vultr / linode: `token`
/// - ovh: `app_key`, `app_secret`, `consumer_key`, `project_id`
pub async fn verify_vps_from_json(
    provider_json: &str,
    client: &reqwest::Client,
) -> Result<Vec<crate::vps::VpsLocation>, String> {
    let val: serde_json::Value =
        serde_json::from_str(provider_json).map_err(|e| format!("invalid JSON: {e}"))?;
    let provider = json_str(&val, "provider")?;
    match provider.as_str() {
        "hetzner" => {
            let token = json_str(&val, "token")?;
            crate::vps::hetzner::Hetzner::new(token)
                .verify(client)
                .await
                .map_err(|e| e.to_string())
        }
        "digitalocean" => {
            let token = json_str(&val, "token")?;
            crate::vps::digitalocean::DigitalOcean::new(token)
                .verify(client)
                .await
                .map_err(|e| e.to_string())
        }
        "vultr" => {
            let token = json_str(&val, "token")?;
            crate::vps::vultr::Vultr::new(token)
                .verify(client)
                .await
                .map_err(|e| e.to_string())
        }
        "ovh" => {
            let app_key = json_str(&val, "app_key")?;
            let app_secret = json_str(&val, "app_secret")?;
            let consumer_key = json_str(&val, "consumer_key")?;
            let project_id = json_str(&val, "project_id")?;
            let mut ovh = crate::vps::ovh::Ovh::new(app_key, app_secret, consumer_key);
            ovh.project_id = project_id;
            ovh.verify(client).await.map_err(|e| e.to_string())
        }
        "linode" => {
            let token = json_str(&val, "token")?;
            crate::vps::linode::Linode::new(token)
                .verify(client)
                .await
                .map_err(|e| e.to_string())
        }
        other => Err(format!("unknown VPS provider: {other}")),
    }
}

/// Verify a DNS provider's credentials and return available zones.
///
/// `provider_json` must contain a `"provider"` field set to one of:
/// - cloudflare / gandi / hetzner: `token`
/// - namecheap: `api_user`, `api_key`, `client_ip`
/// - porkbun: `apikey`, `secretapikey`
pub async fn verify_dns_from_json(
    provider_json: &str,
    client: &reqwest::Client,
) -> Result<Vec<crate::dns::DnsZone>, String> {
    let val: serde_json::Value =
        serde_json::from_str(provider_json).map_err(|e| format!("invalid JSON: {e}"))?;
    let provider = json_str(&val, "provider")?;
    match provider.as_str() {
        "cloudflare" => {
            let token = json_str(&val, "token")?;
            crate::dns::cloudflare::Cloudflare::new(token)
                .verify(client)
                .await
                .map_err(|e| e.to_string())
        }
        "namecheap" => {
            let api_user = json_str(&val, "api_user")?;
            let api_key = json_str(&val, "api_key")?;
            let client_ip = json_str(&val, "client_ip")?;
            let mut nc = crate::dns::namecheap::Namecheap::new(api_user, api_key);
            // Only an optimisation: the setter ignores an empty value
            // (Namecheap rejects an empty `ClientIp` outright, error
            // 1010105) and the adapter self-heals from its own seed.
            nc.set_client_ip(client_ip);
            nc.verify(client).await.map_err(|e| e.to_string())
        }
        "porkbun" => {
            let apikey = json_str(&val, "apikey")?;
            let secretapikey = json_str(&val, "secretapikey")?;
            crate::dns::porkbun::Porkbun::new(apikey, secretapikey)
                .verify(client)
                .await
                .map_err(|e| e.to_string())
        }
        "gandi" => {
            let token = json_str(&val, "token")?;
            crate::dns::gandi::Gandi::new(token)
                .verify(client)
                .await
                .map_err(|e| e.to_string())
        }
        "hetzner" => {
            let token = json_str(&val, "token")?;
            crate::dns::hetzner::HetznerDns::new(token)
                .verify(client)
                .await
                .map_err(|e| e.to_string())
        }
        other => Err(format!("unknown DNS provider: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invalid_json_is_rejected() {
        let client = reqwest::Client::new();
        let err = verify_vps_from_json("not json", &client).await.unwrap_err();
        assert!(err.starts_with("invalid JSON"), "{err}");
    }

    #[tokio::test]
    async fn missing_provider_field_is_rejected() {
        let client = reqwest::Client::new();
        assert_eq!(
            verify_vps_from_json("{}", &client).await.unwrap_err(),
            "missing field: provider"
        );
        assert_eq!(
            verify_dns_from_json("{}", &client).await.unwrap_err(),
            "missing field: provider"
        );
    }

    #[tokio::test]
    async fn unknown_provider_is_rejected() {
        let client = reqwest::Client::new();
        assert_eq!(
            verify_vps_from_json(r#"{"provider":"nope"}"#, &client)
                .await
                .unwrap_err(),
            "unknown VPS provider: nope"
        );
        assert_eq!(
            verify_dns_from_json(r#"{"provider":"nope"}"#, &client)
                .await
                .unwrap_err(),
            "unknown DNS provider: nope"
        );
    }

    #[tokio::test]
    async fn hetzner_vps_missing_token_is_rejected() {
        let client = reqwest::Client::new();
        assert_eq!(
            verify_vps_from_json(r#"{"provider":"hetzner"}"#, &client)
                .await
                .unwrap_err(),
            "missing field: token"
        );
    }

    #[tokio::test]
    async fn ovh_vps_missing_project_id_is_rejected() {
        let client = reqwest::Client::new();
        let err = verify_vps_from_json(
            r#"{"provider":"ovh","app_key":"a","app_secret":"b","consumer_key":"c"}"#,
            &client,
        )
        .await
        .unwrap_err();
        assert_eq!(err, "missing field: project_id");
    }

    #[tokio::test]
    async fn ovh_vps_missing_earlier_field_is_rejected_before_project_id() {
        let client = reqwest::Client::new();
        let err = verify_vps_from_json(r#"{"provider":"ovh"}"#, &client)
            .await
            .unwrap_err();
        assert_eq!(err, "missing field: app_key");
    }

    #[tokio::test]
    async fn namecheap_dns_missing_client_ip_is_rejected() {
        let client = reqwest::Client::new();
        let err = verify_dns_from_json(
            r#"{"provider":"namecheap","api_user":"u","api_key":"k"}"#,
            &client,
        )
        .await
        .unwrap_err();
        assert_eq!(err, "missing field: client_ip");
    }

    #[tokio::test]
    async fn porkbun_dns_missing_secretapikey_is_rejected() {
        let client = reqwest::Client::new();
        let err = verify_dns_from_json(r#"{"provider":"porkbun","apikey":"k"}"#, &client)
            .await
            .unwrap_err();
        assert_eq!(err, "missing field: secretapikey");
    }
}
