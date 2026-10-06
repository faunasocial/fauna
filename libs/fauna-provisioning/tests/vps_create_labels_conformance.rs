//! `create_server` labels conformance — pins how each provider maps the
//! `labels: &[(String, String)]` argument onto its native create-request tagging
//! (Hetzner `labels` map; DigitalOcean/Linode/Vultr string `tags` of `k:v` —
//! colon, not `=`, because DigitalOcean tag charset forbids `=`), the invariant
//! that **empty labels emit no tagging field at all** (a direct provider call
//! with no labels stays label-less), and the orchestrator invariant that every
//! provisioned box carries the `managed-by=fauna` marker unioned with whatever
//! the caller passes (`vps.md` § Uninstall — the future client
//! "list/decommission my nests" view filters on it).

use fauna_provisioning::vps::VpsProvider;
use fauna_provisioning::vps::digitalocean::DigitalOcean;
use fauna_provisioning::vps::hetzner::Hetzner;
use fauna_provisioning::vps::linode::Linode;
use fauna_provisioning::vps::vultr::Vultr;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// A create-body matcher asserting a JSON pointer is ABSENT — used to prove
/// empty labels add no tagging field to the request body.
struct FieldAbsent(&'static str);
impl wiremock::Match for FieldAbsent {
    fn matches(&self, req: &Request) -> bool {
        let Ok(body): Result<serde_json::Value, _> = serde_json::from_slice(&req.body) else {
            return false;
        };
        body.pointer(self.0).is_none()
    }
}

// --------------------------------------------------------------------------
// Hetzner — native `labels` string→string map
// --------------------------------------------------------------------------

#[tokio::test]
async fn hetzner_create_server_attaches_labels_map() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/servers"))
        .and(body_partial_json(serde_json::json!({
            "labels": { "fauna-e2e": "1" },
        })))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_string(r#"{"server":{"id":42,"public_net":{"ipv4":{"ip":"1.2.3.4"}}}}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let hetzner = Hetzner::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let labels = vec![("fauna-e2e".to_string(), "1".to_string())];
    let inst = hetzner
        .create_server(&client, "box-1", "fsn1", "cx23", "#cloud-config", &labels)
        .await
        .expect("create_server OK");
    assert_eq!(inst.server_id, "42");
}

#[tokio::test]
async fn hetzner_create_server_empty_labels_omits_field() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/servers"))
        .and(FieldAbsent("/labels"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_string(r#"{"server":{"id":7,"public_net":{"ipv4":{"ip":"1.2.3.4"}}}}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let hetzner = Hetzner::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let inst = hetzner
        .create_server(&client, "box-1", "fsn1", "cx23", "#cloud-config", &[])
        .await
        .expect("create_server OK");
    assert_eq!(inst.server_id, "7");
}

// --------------------------------------------------------------------------
// DigitalOcean — flat string `tags` of `k:v` (DO tag charset forbids `=`)
// --------------------------------------------------------------------------

#[tokio::test]
async fn digitalocean_create_server_encodes_labels_as_colon_tags() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/droplets"))
        .and(body_partial_json(serde_json::json!({
            "tags": ["fauna-e2e:1"],
        })))
        .respond_with(ResponseTemplate::new(202).set_body_string(
            r#"{"droplet":{"id":99,"networks":{"v4":[{"ip_address":"1.2.3.4","type":"public"}]}}}"#,
        ))
        .expect(1)
        .mount(&mock)
        .await;

    let dop = DigitalOcean::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let labels = vec![("fauna-e2e".to_string(), "1".to_string())];
    let inst = dop
        .create_server(
            &client,
            "box-1",
            "nyc1",
            "s-1vcpu-1gb",
            "#cloud-config",
            &labels,
        )
        .await
        .expect("create_server OK");
    assert_eq!(inst.server_id, "99");
}

#[tokio::test]
async fn digitalocean_create_server_empty_labels_omits_tags() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/droplets"))
        .and(FieldAbsent("/tags"))
        .respond_with(ResponseTemplate::new(202).set_body_string(
            r#"{"droplet":{"id":100,"networks":{"v4":[{"ip_address":"1.2.3.4","type":"public"}]}}}"#,
        ))
        .expect(1)
        .mount(&mock)
        .await;

    let dop = DigitalOcean::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let inst = dop
        .create_server(
            &client,
            "box-1",
            "nyc1",
            "s-1vcpu-1gb",
            "#cloud-config",
            &[],
        )
        .await
        .expect("create_server OK");
    assert_eq!(inst.server_id, "100");
}

// --------------------------------------------------------------------------
// Linode — flat string `tags` of `k:v`
// --------------------------------------------------------------------------

#[tokio::test]
async fn linode_create_server_encodes_labels_as_colon_tags() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/linode/instances"))
        .and(body_partial_json(serde_json::json!({
            "tags": ["fauna-e2e:1"],
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"id":123,"ipv4":["1.2.3.4"]}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let linode = Linode::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let labels = vec![("fauna-e2e".to_string(), "1".to_string())];
    let inst = linode
        .create_server(
            &client,
            "box-1",
            "us-east",
            "g6-nanode-1",
            "#cloud-config",
            &labels,
        )
        .await
        .expect("create_server OK");
    assert_eq!(inst.server_id, "123");
}

// --------------------------------------------------------------------------
// Vultr — flat string `tags` of `k:v` (base64 user-data body; assert exact tags)
// --------------------------------------------------------------------------

#[tokio::test]
async fn vultr_create_server_encodes_labels_as_colon_tags() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/instances"))
        .and(body_partial_json(serde_json::json!({
            "tags": ["fauna-e2e:1"],
        })))
        .respond_with(
            ResponseTemplate::new(202)
                .set_body_string(r#"{"instance":{"id":"vultr-1","main_ip":"1.2.3.4"}}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let vultr = Vultr::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let labels = vec![("fauna-e2e".to_string(), "1".to_string())];
    let inst = vultr
        .create_server(
            &client,
            "box-1",
            "ewr",
            "vc2-1c-1gb",
            "#cloud-config",
            &labels,
        )
        .await
        .expect("create_server OK");
    assert_eq!(inst.server_id, "vultr-1");
}

// --------------------------------------------------------------------------
// Orchestrator — every provisioned box carries `managed-by=fauna`, unioned
// with the caller's labels (`vps.md` § Uninstall). Driven through
// `provision_nest_no_dns` (the only orchestration path whose sole network
// traffic is the server step), against the Hetzner provider so the label map
// is asserted natively.
// --------------------------------------------------------------------------

async fn run_no_dns_provision(mock: &MockServer, caller_labels: &[(String, String)]) {
    use std::sync::Mutex;

    use fauna_provisioning::cloud_init::CloudInitParams;
    use fauna_provisioning::orchestrator::provision_nest_no_dns;
    use fauna_provisioning::progress::{CancelFlag, ProvisioningSnapshot};

    // Pre-flight idempotency lookup: no server with this name exists yet.
    Mock::given(method("GET"))
        .and(path("/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"servers":[]}"#))
        .mount(mock)
        .await;

    let hetzner = Hetzner::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let params = CloudInitParams {
        domain: "x.com".into(),
        image_tag: "latest".into(),
        watchtower_poll: 60,
        claim_code: "claim".into(),
        enable_mail: false,
        deployment_seed: None,
    };
    let state = Mutex::new(ProvisioningSnapshot::idle());
    provision_nest_no_dns(
        &client,
        &hetzner,
        "x.com",
        "box-1",
        "fsn1",
        "cx23",
        &params,
        caller_labels,
        &state,
        || {},
        |_ip, _origin| {},
        &CancelFlag::new(),
    )
    .await
    .expect("provision_nest_no_dns OK");
}

#[tokio::test]
async fn orchestrator_attaches_managed_by_label_when_caller_passes_none() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/servers"))
        .and(body_partial_json(serde_json::json!({
            "labels": { "managed-by": "fauna" },
        })))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_string(r#"{"server":{"id":1,"public_net":{"ipv4":{"ip":"1.2.3.4"}}}}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    run_no_dns_provision(&mock, &[]).await;
}

#[tokio::test]
async fn orchestrator_unions_managed_by_with_caller_labels() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/servers"))
        .and(body_partial_json(serde_json::json!({
            "labels": { "managed-by": "fauna", "fauna-e2e": "1" },
        })))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_string(r#"{"server":{"id":2,"public_net":{"ipv4":{"ip":"1.2.3.4"}}}}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    run_no_dns_provision(&mock, &[("fauna-e2e".to_string(), "1".to_string())]).await;
}
