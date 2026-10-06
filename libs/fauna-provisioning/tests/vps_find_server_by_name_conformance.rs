//! VPS `find_server_by_name` conformance harness — pins per-provider request
//! shape and response parsing using wiremock. Mirrors
//! `vps_delete_server_conformance.rs`.
//!
//! `find_server_by_name` backs the orchestrator's pre-flight idempotency
//! check (`orchestrator.rs` § the name-only lookup before `create_server`):
//! re-running provisioning on the same domain must find the previously
//! created server rather than fail on a duplicate. DigitalOcean, Vultr,
//! Linode and OVH share `vps::find_matching_server`'s loop-and-match shape
//! (the server-list endpoint isn't guaranteed to filter to an exact match,
//! and a matched-by-name entry with no usable IP yet must be skipped rather
//! than treated as found) — these tests pin the match, the name-filter's
//! "no" (a non-matching entry that merely shares the query as a name
//! *prefix*, so a loosened `starts_with` predicate would wrongly accept it,
//! and whose extraction would otherwise succeed, so a dropped filter call
//! would wrongly accept it too), and the fall-through case (a first
//! name-match with no usable IP followed by a second that has one) for each
//! of the four.

use fauna_provisioning::vps::VpsProvider;
use fauna_provisioning::vps::digitalocean::DigitalOcean;
use fauna_provisioning::vps::linode::Linode;
use fauna_provisioning::vps::ovh::Ovh;
use fauna_provisioning::vps::vultr::Vultr;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

// --------------------------------------------------------------------------
// DigitalOcean — GET /droplets?name=
// --------------------------------------------------------------------------

#[tokio::test]
async fn digitalocean_find_server_by_name_matches_the_public_network() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/droplets"))
        .and(query_param("name", "nest-example"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "droplets": [{
                "id": 777,
                "name": "nest-example",
                "networks": { "v4": [
                    { "ip_address": "10.0.0.1", "type": "private" },
                    { "ip_address": "192.0.2.10", "type": "public" }
                ] }
            }]
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let dop = DigitalOcean::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let got = dop
        .find_server_by_name(&client, "nest-example")
        .await
        .unwrap();
    let inst = got.expect("expected a match");
    assert_eq!(inst.server_id, "777");
    assert_eq!(inst.ipv4, "192.0.2.10");
}

#[tokio::test]
async fn digitalocean_find_server_by_name_no_match_is_none() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/droplets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "droplets": [{
                "id": 888,
                "name": "nest-example-2",
                "networks": { "v4": [ { "ip_address": "192.0.2.20", "type": "public" } ] }
            }]
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let dop = DigitalOcean::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    assert!(
        dop.find_server_by_name(&client, "nest-example")
            .await
            .unwrap()
            .is_none(),
        "a droplet whose name only shares a prefix with the query must not match"
    );
}

#[tokio::test]
async fn digitalocean_find_server_by_name_falls_through_to_the_next_usable_match() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/droplets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "droplets": [
                {
                    "id": 1,
                    "name": "nest-example",
                    "networks": { "v4": [ { "ip_address": "10.0.0.1", "type": "private" } ] }
                },
                {
                    "id": 2,
                    "name": "nest-example",
                    "networks": { "v4": [ { "ip_address": "192.0.2.30", "type": "public" } ] }
                }
            ]
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let dop = DigitalOcean::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let inst = dop
        .find_server_by_name(&client, "nest-example")
        .await
        .unwrap()
        .expect("expected the second, usable match");
    assert_eq!(inst.server_id, "2");
    assert_eq!(inst.ipv4, "192.0.2.30");
}

#[tokio::test]
async fn digitalocean_find_server_by_name_skips_a_droplet_with_no_public_network() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/droplets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "droplets": [{
                "id": 1,
                "name": "nest-example",
                "networks": { "v4": [ { "ip_address": "10.0.0.1", "type": "private" } ] }
            }]
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let dop = DigitalOcean::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    assert!(
        dop.find_server_by_name(&client, "nest-example")
            .await
            .unwrap()
            .is_none(),
        "a name match with no public network must not be treated as found"
    );
}

// --------------------------------------------------------------------------
// Vultr — GET /instances?label=
// --------------------------------------------------------------------------

#[tokio::test]
async fn vultr_find_server_by_name_matches() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/instances"))
        .and(query_param("label", "nest-example"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "instances": [
                { "id": "vultr-1", "label": "nest-example", "main_ip": "203.0.113.5" }
            ]
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let vultr = Vultr::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let inst = vultr
        .find_server_by_name(&client, "nest-example")
        .await
        .unwrap()
        .expect("expected a match");
    assert_eq!(inst.server_id, "vultr-1");
    assert_eq!(inst.ipv4, "203.0.113.5");
}

#[tokio::test]
async fn vultr_find_server_by_name_no_match_is_none() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/instances"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "instances": [
                { "id": "vultr-2", "label": "nest-example-2", "main_ip": "203.0.113.20" }
            ]
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let vultr = Vultr::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    assert!(
        vultr
            .find_server_by_name(&client, "nest-example")
            .await
            .unwrap()
            .is_none(),
        "an instance whose label only shares a prefix with the query must not match"
    );
}

#[tokio::test]
async fn vultr_find_server_by_name_falls_through_to_the_next_usable_match() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/instances"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "instances": [
                { "id": "vultr-1", "label": "nest-example", "main_ip": "" },
                { "id": "vultr-2", "label": "nest-example", "main_ip": "203.0.113.30" }
            ]
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let vultr = Vultr::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let inst = vultr
        .find_server_by_name(&client, "nest-example")
        .await
        .unwrap()
        .expect("expected the second, usable match");
    assert_eq!(inst.server_id, "vultr-2");
    assert_eq!(inst.ipv4, "203.0.113.30");
}

#[tokio::test]
async fn vultr_find_server_by_name_skips_an_instance_with_no_ip_yet() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/instances"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "instances": [
                { "id": "vultr-1", "label": "nest-example", "main_ip": "" }
            ]
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let vultr = Vultr::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    assert!(
        vultr
            .find_server_by_name(&client, "nest-example")
            .await
            .unwrap()
            .is_none(),
        "a name match with an unassigned (empty) main_ip must not be treated as found"
    );
}

// --------------------------------------------------------------------------
// Linode — GET /linode/instances with X-Filter header
// --------------------------------------------------------------------------

#[tokio::test]
async fn linode_find_server_by_name_matches() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/linode/instances"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                { "id": 4242, "label": "nest-example", "ipv4": ["198.51.100.7"] }
            ]
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let linode = Linode::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let inst = linode
        .find_server_by_name(&client, "nest-example")
        .await
        .unwrap()
        .expect("expected a match");
    assert_eq!(inst.server_id, "4242");
    assert_eq!(inst.ipv4, "198.51.100.7");
}

#[tokio::test]
async fn linode_find_server_by_name_no_match_is_none() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/linode/instances"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [ { "id": 4243, "label": "nest-example-2", "ipv4": ["198.51.100.20"] } ]
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let linode = Linode::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    assert!(
        linode
            .find_server_by_name(&client, "nest-example")
            .await
            .unwrap()
            .is_none(),
        "an instance whose label only shares a prefix with the query must not match"
    );
}

#[tokio::test]
async fn linode_find_server_by_name_falls_through_to_the_next_usable_match() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/linode/instances"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                { "id": 4242, "label": "nest-example", "ipv4": [] },
                { "id": 4244, "label": "nest-example", "ipv4": ["198.51.100.30"] }
            ]
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let linode = Linode::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let inst = linode
        .find_server_by_name(&client, "nest-example")
        .await
        .unwrap()
        .expect("expected the second, usable match");
    assert_eq!(inst.server_id, "4244");
    assert_eq!(inst.ipv4, "198.51.100.30");
}

#[tokio::test]
async fn linode_find_server_by_name_skips_an_instance_with_no_ipv4_yet() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/linode/instances"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [ { "id": 4242, "label": "nest-example", "ipv4": [] } ]
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let linode = Linode::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    assert!(
        linode
            .find_server_by_name(&client, "nest-example")
            .await
            .unwrap()
            .is_none(),
        "a name match with no ipv4 entries yet must not be treated as found"
    );
}

// --------------------------------------------------------------------------
// OVH — signed GET /1.0/cloud/project/{pid}/instance
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
async fn ovh_find_server_by_name_matches_the_public_ipv4() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/1.0/auth/time"))
        .respond_with(ResponseTemplate::new(200).set_body_string("1700000000"))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/1.0/cloud/project/proj-1/instance"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "ovh-1",
                "name": "nest-example",
                "ipAddresses": [
                    { "ip": "2001:db8::1", "version": 6, "type": "public" },
                    { "ip": "198.51.100.9", "version": 4, "type": "public" }
                ]
            }
        ])))
        .expect(1)
        .mount(&mock)
        .await;

    let ovh = ovh_with_project(mock.uri());
    let client = reqwest::Client::new();
    let inst = ovh
        .find_server_by_name(&client, "nest-example")
        .await
        .unwrap()
        .expect("expected a match");
    assert_eq!(inst.server_id, "ovh-1");
    assert_eq!(inst.ipv4, "198.51.100.9");
}

#[tokio::test]
async fn ovh_find_server_by_name_no_match_is_none() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/1.0/auth/time"))
        .respond_with(ResponseTemplate::new(200).set_body_string("1700000000"))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/1.0/cloud/project/proj-1/instance"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "ovh-2",
                "name": "nest-example-2",
                "ipAddresses": [
                    { "ip": "198.51.100.20", "version": 4, "type": "public" }
                ]
            }
        ])))
        .expect(1)
        .mount(&mock)
        .await;

    let ovh = ovh_with_project(mock.uri());
    let client = reqwest::Client::new();
    assert!(
        ovh.find_server_by_name(&client, "nest-example")
            .await
            .unwrap()
            .is_none(),
        "an instance whose name only shares a prefix with the query must not match"
    );
}

#[tokio::test]
async fn ovh_find_server_by_name_falls_through_to_the_next_usable_match() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/1.0/auth/time"))
        .respond_with(ResponseTemplate::new(200).set_body_string("1700000000"))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/1.0/cloud/project/proj-1/instance"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "ovh-1",
                "name": "nest-example",
                "ipAddresses": [ { "ip": "2001:db8::1", "version": 6, "type": "public" } ]
            },
            {
                "id": "ovh-2",
                "name": "nest-example",
                "ipAddresses": [ { "ip": "198.51.100.30", "version": 4, "type": "public" } ]
            }
        ])))
        .expect(1)
        .mount(&mock)
        .await;

    let ovh = ovh_with_project(mock.uri());
    let client = reqwest::Client::new();
    let inst = ovh
        .find_server_by_name(&client, "nest-example")
        .await
        .unwrap()
        .expect("expected the second, usable match");
    assert_eq!(inst.server_id, "ovh-2");
    assert_eq!(inst.ipv4, "198.51.100.30");
}

#[tokio::test]
async fn ovh_find_server_by_name_skips_an_instance_with_no_public_ipv4_yet() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/1.0/auth/time"))
        .respond_with(ResponseTemplate::new(200).set_body_string("1700000000"))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/1.0/cloud/project/proj-1/instance"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "ovh-1",
                "name": "nest-example",
                "ipAddresses": [ { "ip": "2001:db8::1", "version": 6, "type": "public" } ]
            }
        ])))
        .expect(1)
        .mount(&mock)
        .await;

    let ovh = ovh_with_project(mock.uri());
    let client = reqwest::Client::new();
    assert!(
        ovh.find_server_by_name(&client, "nest-example")
            .await
            .unwrap()
            .is_none(),
        "a name match with no public IPv4 yet must not be treated as found"
    );
}
