//! VPS `delete_server` conformance harness — pins per-provider request shape
//! (path / method / auth) using wiremock. Mirrors `vps_set_ptr_conformance.rs`.
//!
//! `delete_server` is the decommission primitive that backs a client-side
//! Uninstall flow (`docs/goal/architecture/installers/vps.md` § Uninstall) and
//! lets the live-provision e2e teardown exercise the crate path rather than
//! hitting the provider endpoint by hand. It is **idempotent**: deleting an
//! already-absent server (404) is a success — a decommission that finds the box
//! already gone has achieved its goal, and double-teardown must not raise.

use fauna_provisioning::error::ProvisionError;
use fauna_provisioning::vps::digitalocean::DigitalOcean;
use fauna_provisioning::vps::hetzner::Hetzner;
use fauna_provisioning::vps::linode::Linode;
use fauna_provisioning::vps::ovh::Ovh;
use fauna_provisioning::vps::vultr::Vultr;
use fauna_provisioning::vps::{VpsInstance, VpsProvider};
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn instance(server_id: &str, ipv4: &str) -> VpsInstance {
    VpsInstance {
        server_id: server_id.into(),
        ipv4: ipv4.into(),
    }
}

// --------------------------------------------------------------------------
// Hetzner — DELETE /servers/{id}
// --------------------------------------------------------------------------

#[tokio::test]
async fn hetzner_delete_server_happy_path() {
    let mock = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/servers/12345"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"action":{"id":1}}"#))
        .expect(1)
        .mount(&mock)
        .await;

    let hetzner = Hetzner::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = hetzner
        .delete_server(&client, &instance("12345", "1.2.3.4"))
        .await;
    assert!(result.is_ok(), "expected delete_server OK, got {result:?}");
}

#[tokio::test]
async fn hetzner_delete_server_404_is_idempotent_ok() {
    let mock = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/servers/12345"))
        .respond_with(
            ResponseTemplate::new(404).set_body_string(r#"{"error":{"code":"not_found"}}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let hetzner = Hetzner::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = hetzner
        .delete_server(&client, &instance("12345", "1.2.3.4"))
        .await;
    assert!(
        result.is_ok(),
        "404 must be a no-op success, got {result:?}"
    );
}

#[tokio::test]
async fn hetzner_delete_server_401_maps_to_provider_error() {
    let mock = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/servers/12345"))
        .respond_with(
            ResponseTemplate::new(401).set_body_string(r#"{"error":{"message":"unauthorized"}}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let hetzner = Hetzner::with_base_url("bad-token".into(), mock.uri());
    let client = reqwest::Client::new();
    match hetzner
        .delete_server(&client, &instance("12345", "1.2.3.4"))
        .await
    {
        Err(ProvisionError::Provider { status, body }) => {
            assert_eq!(status, 401);
            assert!(body.contains("unauthorized"), "body was {body}");
        }
        other => panic!("expected Provider 401, got {other:?}"),
    }
}

// --------------------------------------------------------------------------
// DigitalOcean — DELETE /droplets/{id}
// --------------------------------------------------------------------------

#[tokio::test]
async fn digitalocean_delete_server_happy_path() {
    let mock = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/droplets/777"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&mock)
        .await;

    let dop = DigitalOcean::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = dop
        .delete_server(&client, &instance("777", "192.0.2.10"))
        .await;
    assert!(result.is_ok(), "expected delete_server OK, got {result:?}");
}

#[tokio::test]
async fn digitalocean_delete_server_404_is_idempotent_ok() {
    let mock = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/droplets/777"))
        .respond_with(ResponseTemplate::new(404).set_body_string(r#"{"id":"not_found"}"#))
        .expect(1)
        .mount(&mock)
        .await;

    let dop = DigitalOcean::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = dop
        .delete_server(&client, &instance("777", "192.0.2.10"))
        .await;
    assert!(
        result.is_ok(),
        "404 must be a no-op success, got {result:?}"
    );
}

#[tokio::test]
async fn digitalocean_delete_server_401_maps_to_provider_error() {
    let mock = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/droplets/777"))
        .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"id":"unauthorized"}"#))
        .expect(1)
        .mount(&mock)
        .await;

    let dop = DigitalOcean::with_base_url("bad".into(), mock.uri());
    let client = reqwest::Client::new();
    match dop
        .delete_server(&client, &instance("777", "192.0.2.10"))
        .await
    {
        Err(ProvisionError::Provider { status, .. }) => assert_eq!(status, 401),
        other => panic!("expected Provider 401, got {other:?}"),
    }
}

// --------------------------------------------------------------------------
// Linode — DELETE /linode/instances/{id}
// --------------------------------------------------------------------------

#[tokio::test]
async fn linode_delete_server_happy_path() {
    let mock = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/linode/instances/4242"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&mock)
        .await;

    let linode = Linode::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = linode
        .delete_server(&client, &instance("4242", "198.51.100.7"))
        .await;
    assert!(result.is_ok(), "expected delete_server OK, got {result:?}");
}

#[tokio::test]
async fn linode_delete_server_404_is_idempotent_ok() {
    let mock = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/linode/instances/4242"))
        .respond_with(
            ResponseTemplate::new(404).set_body_string(r#"{"errors":[{"reason":"Not found"}]}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let linode = Linode::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = linode
        .delete_server(&client, &instance("4242", "198.51.100.7"))
        .await;
    assert!(
        result.is_ok(),
        "404 must be a no-op success, got {result:?}"
    );
}

#[tokio::test]
async fn linode_delete_server_401_maps_to_provider_error() {
    let mock = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/linode/instances/4242"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_string(r#"{"errors":[{"reason":"Invalid token"}]}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let linode = Linode::with_base_url("bad".into(), mock.uri());
    let client = reqwest::Client::new();
    match linode
        .delete_server(&client, &instance("4242", "198.51.100.7"))
        .await
    {
        Err(ProvisionError::Provider { status, .. }) => assert_eq!(status, 401),
        other => panic!("expected Provider 401, got {other:?}"),
    }
}

// --------------------------------------------------------------------------
// Vultr — DELETE /instances/{id}
// --------------------------------------------------------------------------

#[tokio::test]
async fn vultr_delete_server_happy_path() {
    let mock = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/instances/instance-abc"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&mock)
        .await;

    let vultr = Vultr::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = vultr
        .delete_server(&client, &instance("instance-abc", "203.0.113.5"))
        .await;
    assert!(result.is_ok(), "expected delete_server OK, got {result:?}");
}

#[tokio::test]
async fn vultr_delete_server_404_is_idempotent_ok() {
    let mock = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/instances/instance-abc"))
        .respond_with(ResponseTemplate::new(404).set_body_string(r#"{"error":"not found"}"#))
        .expect(1)
        .mount(&mock)
        .await;

    let vultr = Vultr::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = vultr
        .delete_server(&client, &instance("instance-abc", "203.0.113.5"))
        .await;
    assert!(
        result.is_ok(),
        "404 must be a no-op success, got {result:?}"
    );
}

#[tokio::test]
async fn vultr_delete_server_401_maps_to_provider_error() {
    let mock = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/instances/instance-abc"))
        .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":"Invalid API key"}"#))
        .expect(1)
        .mount(&mock)
        .await;

    let vultr = Vultr::with_base_url("bad".into(), mock.uri());
    let client = reqwest::Client::new();
    match vultr
        .delete_server(&client, &instance("instance-abc", "203.0.113.5"))
        .await
    {
        Err(ProvisionError::Provider { status, .. }) => assert_eq!(status, 401),
        other => panic!("expected Provider 401, got {other:?}"),
    }
}

// --------------------------------------------------------------------------
// OVH — signed DELETE /1.0/cloud/project/{pid}/instance/{id}
// --------------------------------------------------------------------------

fn ovh_with_project(uri: String) -> Ovh {
    let mut ovh = Ovh::with_base_url(
        "app-key".into(),
        "app-secret".into(),
        "consumer-key".into(),
        uri,
    );
    ovh.project_id = "proj-1".into();
    ovh
}

#[tokio::test]
async fn ovh_delete_server_happy_path() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/1.0/auth/time"))
        .respond_with(ResponseTemplate::new(200).set_body_string("1700000000"))
        .mount(&mock)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/1.0/cloud/project/proj-1/instance/ovh-instance-1"))
        .and(header_exists("X-Ovh-Application"))
        .and(header_exists("X-Ovh-Consumer"))
        .and(header_exists("X-Ovh-Timestamp"))
        .and(header_exists("X-Ovh-Signature"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&mock)
        .await;

    let ovh = ovh_with_project(mock.uri());
    let client = reqwest::Client::new();
    let result = ovh
        .delete_server(&client, &instance("ovh-instance-1", "198.51.100.9"))
        .await;
    assert!(result.is_ok(), "expected delete_server OK, got {result:?}");
}

#[tokio::test]
async fn ovh_delete_server_404_is_idempotent_ok() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/1.0/auth/time"))
        .respond_with(ResponseTemplate::new(200).set_body_string("1700000000"))
        .mount(&mock)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/1.0/cloud/project/proj-1/instance/ovh-instance-1"))
        .respond_with(ResponseTemplate::new(404).set_body_string(r#"{"message":"not found"}"#))
        .expect(1)
        .mount(&mock)
        .await;

    let ovh = ovh_with_project(mock.uri());
    let client = reqwest::Client::new();
    let result = ovh
        .delete_server(&client, &instance("ovh-instance-1", "198.51.100.9"))
        .await;
    assert!(
        result.is_ok(),
        "404 must be a no-op success, got {result:?}"
    );
}

#[tokio::test]
async fn ovh_delete_server_401_maps_to_provider_error() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/1.0/auth/time"))
        .respond_with(ResponseTemplate::new(200).set_body_string("1700000000"))
        .mount(&mock)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/1.0/cloud/project/proj-1/instance/ovh-instance-1"))
        .respond_with(
            ResponseTemplate::new(401).set_body_string(r#"{"message":"Invalid signature"}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let ovh = ovh_with_project(mock.uri());
    let client = reqwest::Client::new();
    match ovh
        .delete_server(&client, &instance("ovh-instance-1", "198.51.100.9"))
        .await
    {
        Err(ProvisionError::Provider { status, .. }) => assert_eq!(status, 401),
        other => panic!("expected Provider 401, got {other:?}"),
    }
}
