//! VPS PTR conformance harness — pins per-provider request shape using wiremock.
//!
//! Each provider gets a happy-path test (correct path/body/auth) plus
//! error-path and idempotency tests. Mirrors `dns_conformance.rs` style.

use fauna_provisioning::error::ProvisionError;
use fauna_provisioning::vps::digitalocean::DigitalOcean;
use fauna_provisioning::vps::hetzner::Hetzner;
use fauna_provisioning::vps::linode::Linode;
use fauna_provisioning::vps::ovh::Ovh;
use fauna_provisioning::vps::vultr::Vultr;
use fauna_provisioning::vps::{VpsInstance, VpsProvider};
use wiremock::matchers::{body_partial_json, header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn instance(server_id: &str, ipv4: &str) -> VpsInstance {
    VpsInstance {
        server_id: server_id.into(),
        ipv4: ipv4.into(),
    }
}

// --------------------------------------------------------------------------
// Hetzner
// --------------------------------------------------------------------------

#[tokio::test]
async fn hetzner_set_ptr_happy_path() {
    let mock = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/servers/12345/actions/change_dns_ptr"))
        .and(header("authorization", "Bearer test-token"))
        .and(body_partial_json(serde_json::json!({
            "ip": "1.2.3.4",
            "dns_ptr": "nest.example.com",
        })))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&mock)
        .await;

    let hetzner = Hetzner::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = hetzner
        .set_ptr(&client, &instance("12345", "1.2.3.4"), "nest.example.com")
        .await;

    assert!(result.is_ok(), "expected set_ptr OK, got {:?}", result);
}

#[tokio::test]
async fn hetzner_set_ptr_401_maps_to_provider_error() {
    let mock = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/servers/12345/actions/change_dns_ptr"))
        .respond_with(
            ResponseTemplate::new(401).set_body_string(r#"{"error":{"message":"unauthorized"}}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let hetzner = Hetzner::with_base_url("bad-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = hetzner
        .set_ptr(&client, &instance("12345", "1.2.3.4"), "nest.example.com")
        .await;

    match result {
        Err(ProvisionError::Provider { status, body }) => {
            assert_eq!(status, 401);
            assert!(body.contains("unauthorized"), "body was {body}");
        }
        other => panic!("expected Provider 401, got {other:?}"),
    }
}

#[tokio::test]
async fn hetzner_set_ptr_422_maps_to_provider_error() {
    let mock = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/servers/12345/actions/change_dns_ptr"))
        .respond_with(
            ResponseTemplate::new(422)
                .set_body_string(r#"{"error":{"message":"PTR does not forward-resolve to IP"}}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let hetzner = Hetzner::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = hetzner
        .set_ptr(&client, &instance("12345", "1.2.3.4"), "nest.example.com")
        .await;

    match result {
        Err(ProvisionError::Provider { status, .. }) => assert_eq!(status, 422),
        other => panic!("expected Provider 422, got {other:?}"),
    }
}

#[tokio::test]
async fn hetzner_set_ptr_is_idempotent() {
    let mock = MockServer::start().await;

    // Mock returns 201 unconditionally; we expect the impl to issue
    // exactly two identical requests when called twice.
    Mock::given(method("POST"))
        .and(path("/servers/12345/actions/change_dns_ptr"))
        .and(body_partial_json(serde_json::json!({
            "ip": "1.2.3.4",
            "dns_ptr": "nest.example.com",
        })))
        .respond_with(ResponseTemplate::new(201))
        .expect(2)
        .mount(&mock)
        .await;

    let hetzner = Hetzner::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let inst = instance("12345", "1.2.3.4");

    hetzner
        .set_ptr(&client, &inst, "nest.example.com")
        .await
        .unwrap();
    hetzner
        .set_ptr(&client, &inst, "nest.example.com")
        .await
        .unwrap();
    // Mock's `.expect(2)` fails the test on drop if not satisfied.
}

// --------------------------------------------------------------------------
// DigitalOcean
// --------------------------------------------------------------------------

#[tokio::test]
async fn digitalocean_set_ptr_happy_path() {
    let mock = MockServer::start().await;

    // DO returns the droplet object; we only check status, but the
    // mock has to return something parseable-or-ignored. An empty
    // JSON object suffices because the impl doesn't deserialize.
    Mock::given(method("PUT"))
        .and(path("/droplets/777"))
        .and(header("authorization", "Bearer test-token"))
        .and(body_partial_json(serde_json::json!({
            "name": "nest.example.com",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&mock)
        .await;

    let dop = DigitalOcean::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = dop
        .set_ptr(&client, &instance("777", "192.0.2.10"), "nest.example.com")
        .await;

    assert!(result.is_ok(), "expected set_ptr OK, got {:?}", result);
}

#[tokio::test]
async fn digitalocean_set_ptr_401_maps_to_provider_error() {
    let mock = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/droplets/777"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_string(r#"{"id":"unauthorized","message":"bad token"}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let dop = DigitalOcean::with_base_url("bad-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = dop
        .set_ptr(&client, &instance("777", "192.0.2.10"), "nest.example.com")
        .await;

    match result {
        Err(ProvisionError::Provider { status, .. }) => assert_eq!(status, 401),
        other => panic!("expected Provider 401, got {other:?}"),
    }
}

#[tokio::test]
async fn digitalocean_set_ptr_422_maps_to_provider_error() {
    let mock = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/droplets/777"))
        .respond_with(
            ResponseTemplate::new(422)
                .set_body_string(r#"{"id":"validation_failed","message":"name in use"}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let dop = DigitalOcean::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = dop
        .set_ptr(&client, &instance("777", "192.0.2.10"), "nest.example.com")
        .await;

    match result {
        Err(ProvisionError::Provider { status, .. }) => assert_eq!(status, 422),
        other => panic!("expected Provider 422, got {other:?}"),
    }
}

#[tokio::test]
async fn digitalocean_set_ptr_is_idempotent() {
    let mock = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/droplets/777"))
        .and(body_partial_json(
            serde_json::json!({"name": "nest.example.com"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(2)
        .mount(&mock)
        .await;

    let dop = DigitalOcean::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let inst = instance("777", "192.0.2.10");
    dop.set_ptr(&client, &inst, "nest.example.com")
        .await
        .unwrap();
    dop.set_ptr(&client, &inst, "nest.example.com")
        .await
        .unwrap();
}

// --------------------------------------------------------------------------
// Vultr
// --------------------------------------------------------------------------

#[tokio::test]
async fn vultr_set_ptr_happy_path() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/reverse/ipv4"))
        .and(header("authorization", "Bearer test-token"))
        .and(body_partial_json(serde_json::json!({
            "ip": "203.0.113.5",
            "reverse": "nest.example.com",
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&mock)
        .await;

    let vultr = Vultr::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = vultr
        .set_ptr(
            &client,
            &instance("instance-abc", "203.0.113.5"),
            "nest.example.com",
        )
        .await;
    assert!(result.is_ok(), "expected set_ptr OK, got {:?}", result);
}

#[tokio::test]
async fn vultr_set_ptr_401_maps_to_provider_error() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/reverse/ipv4"))
        .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":"Invalid API key"}"#))
        .expect(1)
        .mount(&mock)
        .await;

    let vultr = Vultr::with_base_url("bad".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = vultr
        .set_ptr(&client, &instance("i", "203.0.113.5"), "nest.example.com")
        .await;
    match result {
        Err(ProvisionError::Provider { status, .. }) => assert_eq!(status, 401),
        other => panic!("expected Provider 401, got {other:?}"),
    }
}

#[tokio::test]
async fn vultr_set_ptr_400_maps_to_provider_error() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/reverse/ipv4"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string(r#"{"error":"reverse must be a valid hostname"}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let vultr = Vultr::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = vultr
        .set_ptr(&client, &instance("i", "203.0.113.5"), "bad fqdn")
        .await;
    match result {
        Err(ProvisionError::Provider { status, .. }) => assert_eq!(status, 400),
        other => panic!("expected Provider 400, got {other:?}"),
    }
}

#[tokio::test]
async fn vultr_set_ptr_is_idempotent() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/reverse/ipv4"))
        .and(body_partial_json(serde_json::json!({
            "ip": "203.0.113.5",
            "reverse": "nest.example.com",
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(2)
        .mount(&mock)
        .await;

    let vultr = Vultr::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let inst = instance("i", "203.0.113.5");
    vultr
        .set_ptr(&client, &inst, "nest.example.com")
        .await
        .unwrap();
    vultr
        .set_ptr(&client, &inst, "nest.example.com")
        .await
        .unwrap();
}

// --------------------------------------------------------------------------
// Linode
// --------------------------------------------------------------------------

#[tokio::test]
async fn linode_set_ptr_happy_path() {
    let mock = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/networking/ips/198.51.100.7"))
        .and(header("authorization", "Bearer test-token"))
        .and(body_partial_json(serde_json::json!({
            "rdns": "nest.example.com",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&mock)
        .await;

    let linode = Linode::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = linode
        .set_ptr(
            &client,
            &instance("4242", "198.51.100.7"),
            "nest.example.com",
        )
        .await;
    assert!(result.is_ok(), "expected set_ptr OK, got {:?}", result);
}

#[tokio::test]
async fn linode_set_ptr_401_maps_to_provider_error() {
    let mock = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/networking/ips/198.51.100.7"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_string(r#"{"errors":[{"reason":"invalid token"}]}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;
    let linode = Linode::with_base_url("bad".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = linode
        .set_ptr(&client, &instance("i", "198.51.100.7"), "nest.example.com")
        .await;
    match result {
        Err(ProvisionError::Provider { status, .. }) => assert_eq!(status, 401),
        other => panic!("expected Provider 401, got {other:?}"),
    }
}

#[tokio::test]
async fn linode_set_ptr_400_maps_to_provider_error() {
    let mock = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/networking/ips/198.51.100.7"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string(r#"{"errors":[{"reason":"rdns does not resolve to ip"}]}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;
    let linode = Linode::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = linode
        .set_ptr(&client, &instance("i", "198.51.100.7"), "nest.example.com")
        .await;
    match result {
        Err(ProvisionError::Provider { status, .. }) => assert_eq!(status, 400),
        other => panic!("expected Provider 400, got {other:?}"),
    }
}

#[tokio::test]
async fn linode_set_ptr_is_idempotent() {
    let mock = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/networking/ips/198.51.100.7"))
        .and(body_partial_json(
            serde_json::json!({"rdns": "nest.example.com"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(2)
        .mount(&mock)
        .await;
    let linode = Linode::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let inst = instance("i", "198.51.100.7");
    linode
        .set_ptr(&client, &inst, "nest.example.com")
        .await
        .unwrap();
    linode
        .set_ptr(&client, &inst, "nest.example.com")
        .await
        .unwrap();
}

// --------------------------------------------------------------------------
// OVH
// --------------------------------------------------------------------------

#[tokio::test]
async fn ovh_set_ptr_happy_path() {
    let mock = MockServer::start().await;

    // OVH timestamp endpoint hit by signed_post.
    Mock::given(method("GET"))
        .and(path("/1.0/auth/time"))
        .respond_with(ResponseTemplate::new(200).set_body_string("1700000000"))
        .mount(&mock)
        .await;

    Mock::given(method("POST"))
        .and(path("/1.0/ip/198.51.100.9/reverse"))
        .and(header_exists("X-Ovh-Application"))
        .and(header_exists("X-Ovh-Consumer"))
        .and(header_exists("X-Ovh-Timestamp"))
        .and(header_exists("X-Ovh-Signature"))
        .and(body_partial_json(serde_json::json!({
            "ipReverse": "198.51.100.9",
            "reverse": "nest.example.com",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&mock)
        .await;

    let ovh = Ovh::with_base_url(
        "app-key".into(),
        "app-secret".into(),
        "consumer-key".into(),
        mock.uri(),
    );
    let client = reqwest::Client::new();
    let result = ovh
        .set_ptr(
            &client,
            &instance("ovh-instance-1", "198.51.100.9"),
            "nest.example.com",
        )
        .await;

    assert!(result.is_ok(), "expected set_ptr OK, got {:?}", result);
}

#[tokio::test]
async fn ovh_set_ptr_401_maps_to_provider_error() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/1.0/auth/time"))
        .respond_with(ResponseTemplate::new(200).set_body_string("1700000000"))
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/1.0/ip/198.51.100.9/reverse"))
        .respond_with(
            ResponseTemplate::new(401).set_body_string(r#"{"message":"Invalid signature"}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let ovh = Ovh::with_base_url(
        "app-key".into(),
        "app-secret".into(),
        "consumer-key".into(),
        mock.uri(),
    );
    let client = reqwest::Client::new();
    let result = ovh
        .set_ptr(&client, &instance("i", "198.51.100.9"), "nest.example.com")
        .await;
    match result {
        Err(ProvisionError::Provider { status, .. }) => assert_eq!(status, 401),
        other => panic!("expected Provider 401, got {other:?}"),
    }
}

#[tokio::test]
async fn ovh_set_ptr_400_maps_to_provider_error() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/1.0/auth/time"))
        .respond_with(ResponseTemplate::new(200).set_body_string("1700000000"))
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/1.0/ip/198.51.100.9/reverse"))
        .respond_with(
            ResponseTemplate::new(400).set_body_string(r#"{"message":"invalid hostname"}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;
    let ovh = Ovh::with_base_url(
        "app-key".into(),
        "app-secret".into(),
        "consumer-key".into(),
        mock.uri(),
    );
    let client = reqwest::Client::new();
    let result = ovh
        .set_ptr(&client, &instance("i", "198.51.100.9"), "bad")
        .await;
    match result {
        Err(ProvisionError::Provider { status, .. }) => assert_eq!(status, 400),
        other => panic!("expected Provider 400, got {other:?}"),
    }
}

#[tokio::test]
async fn ovh_set_ptr_is_idempotent() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/1.0/auth/time"))
        .respond_with(ResponseTemplate::new(200).set_body_string("1700000000"))
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/1.0/ip/198.51.100.9/reverse"))
        .and(body_partial_json(serde_json::json!({
            "ipReverse": "198.51.100.9",
            "reverse": "nest.example.com",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(2)
        .mount(&mock)
        .await;
    let ovh = Ovh::with_base_url(
        "app-key".into(),
        "app-secret".into(),
        "consumer-key".into(),
        mock.uri(),
    );
    let client = reqwest::Client::new();
    let inst = instance("i", "198.51.100.9");
    ovh.set_ptr(&client, &inst, "nest.example.com")
        .await
        .unwrap();
    ovh.set_ptr(&client, &inst, "nest.example.com")
        .await
        .unwrap();
}
