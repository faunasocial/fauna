//! `create_server` image conformance — pins the operating-system image each
//! named provider's create request asks for. Every server Fauna creates runs
//! Ubuntu 26.04 LTS (`vps.md` § Minimum VPS Requirements, the Operating system
//! row); each identifier below was read off the provider's own image listing on
//! 2026-10-20 — but OVH's, which the adapter resolves to the region's image
//! *id* at create time (`registry.md` § Server offers follow the location →
//! *OVH*).

use fauna_provisioning::vps::VpsProvider;
use fauna_provisioning::vps::digitalocean::DigitalOcean;
use fauna_provisioning::vps::hetzner::Hetzner;
use fauna_provisioning::vps::linode::Linode;
use fauna_provisioning::vps::ovh::Ovh;
use fauna_provisioning::vps::vultr::Vultr;
use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn hetzner_create_server_asks_for_ubuntu_26_04() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/servers"))
        .and(body_partial_json(
            serde_json::json!({ "image": "ubuntu-26.04" }),
        ))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_string(r#"{"server":{"id":42,"public_net":{"ipv4":{"ip":"1.2.3.4"}}}}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let hetzner = Hetzner::with_base_url("test-token".into(), mock.uri());
    let inst = hetzner
        .create_server(
            &reqwest::Client::new(),
            "box-1",
            "fsn1",
            "cx23",
            "#cloud-config",
            &[],
        )
        .await
        .expect("create_server OK");
    assert_eq!(inst.server_id, "42");
}

#[tokio::test]
async fn digitalocean_create_server_asks_for_ubuntu_26_04() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/droplets"))
        .and(body_partial_json(
            serde_json::json!({ "image": "ubuntu-26-04-x64" }),
        ))
        .respond_with(ResponseTemplate::new(202).set_body_string(
            r#"{"droplet":{"id":99,"networks":{"v4":[{"ip_address":"1.2.3.4","type":"public"}]}}}"#,
        ))
        .expect(1)
        .mount(&mock)
        .await;

    let dop = DigitalOcean::with_base_url("test-token".into(), mock.uri());
    let inst = dop
        .create_server(
            &reqwest::Client::new(),
            "box-1",
            "nyc1",
            "s-1vcpu-1gb",
            "#cloud-config",
            &[],
        )
        .await
        .expect("create_server OK");
    assert_eq!(inst.server_id, "99");
}

#[tokio::test]
async fn linode_create_server_asks_for_ubuntu_26_04() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/linode/instances"))
        .and(body_partial_json(
            serde_json::json!({ "image": "linode/ubuntu26.04" }),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"id":123,"ipv4":["1.2.3.4"]}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let linode = Linode::with_base_url("test-token".into(), mock.uri());
    let inst = linode
        .create_server(
            &reqwest::Client::new(),
            "box-1",
            "us-east",
            "g6-nanode-1",
            "#cloud-config",
            &[],
        )
        .await
        .expect("create_server OK");
    assert_eq!(inst.server_id, "123");
}

/// Vultr names its images by number: `os_id` 2760 is "Ubuntu 26.04 LTS x64" in
/// `GET /v2/os`.
#[tokio::test]
async fn vultr_create_server_asks_for_ubuntu_26_04() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/instances"))
        .and(body_partial_json(serde_json::json!({ "os_id": 2760 })))
        .respond_with(
            ResponseTemplate::new(202)
                .set_body_string(r#"{"instance":{"id":"vultr-1","main_ip":"1.2.3.4"}}"#),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let vultr = Vultr::with_base_url("test-token".into(), mock.uri());
    let inst = vultr
        .create_server(
            &reqwest::Client::new(),
            "box-1",
            "ewr",
            "vc2-1c-1gb",
            "#cloud-config",
            &[],
        )
        .await
        .expect("create_server OK");
    assert_eq!(inst.server_id, "vultr-1");
}

/// The region's flavor list and its linux image list, as OVH answers them —
/// the create resolves both ids in the region.
async fn mount_ovh_region_lists(mock: &MockServer, images: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path("/1.0/auth/time"))
        .respond_with(ResponseTemplate::new(200).set_body_string("1700000000"))
        .mount(mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/1.0/cloud/project/proj-1/flavor"))
        .and(query_param("region", "GRA11"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": "flavor-win", "name": "d2-2", "osType": "windows", "region": "GRA11",
              "available": true, "vcpus": 1, "ram": 2, "disk": 25 },
            { "id": "flavor-lin", "name": "d2-2", "osType": "linux", "region": "GRA11",
              "available": true, "vcpus": 1, "ram": 2, "disk": 25 }
        ])))
        .mount(mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/1.0/cloud/project/proj-1/image"))
        .and(query_param("region", "GRA11"))
        .and(query_param("osType", "linux"))
        .respond_with(ResponseTemplate::new(200).set_body_json(images))
        .mount(mock)
        .await;
}

fn ovh(uri: String) -> Ovh {
    Ovh::with_base_url(
        "app-key".into(),
        "app-secret".into(),
        "consumer-key".into(),
        uri,
    )
}

#[tokio::test]
async fn ovh_create_server_asks_for_ubuntu_26_04() {
    let mock = MockServer::start().await;
    mount_ovh_region_lists(
        &mock,
        serde_json::json!([
            { "id": "img-2404", "name": "Ubuntu 24.04", "region": "GRA11" },
            { "id": "img-2604", "name": "Ubuntu 26.04", "region": "GRA11" }
        ]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/1.0/cloud/project/proj-1/instance"))
        .and(body_partial_json(serde_json::json!({
            "flavorId": "flavor-lin",
            "imageId": "img-2604",
            "region": "GRA11",
            "monthlyBilling": true
        })))
        .respond_with(ResponseTemplate::new(201).set_body_string(
            r#"{"id":"ovh-1","ipAddresses":[{"ip":"1.2.3.4","version":4,"type":"public"}]}"#,
        ))
        .expect(1)
        .mount(&mock)
        .await;

    let inst = ovh(mock.uri())
        .create_server(
            &reqwest::Client::new(),
            "box-1",
            "proj-1/GRA11",
            "d2-2",
            "#cloud-config",
            &[],
        )
        .await
        .expect("create_server OK");
    assert_eq!(inst.server_id, "proj-1/ovh-1");
}

/// A region with no 26.04 image is a clear error naming the region and the
/// release — never a fall-back to an older Ubuntu, and never a POST.
#[tokio::test]
async fn ovh_create_server_without_a_26_04_image_is_an_error() {
    let mock = MockServer::start().await;
    mount_ovh_region_lists(
        &mock,
        serde_json::json!([{ "id": "img-2404", "name": "Ubuntu 24.04", "region": "GRA11" }]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/1.0/cloud/project/proj-1/instance"))
        .respond_with(ResponseTemplate::new(201))
        .expect(0)
        .mount(&mock)
        .await;

    let err = ovh(mock.uri())
        .create_server(
            &reqwest::Client::new(),
            "box-1",
            "proj-1/GRA11",
            "d2-2",
            "#cloud-config",
            &[],
        )
        .await
        .expect_err("no 26.04 image in the region");
    let msg = err.to_string();
    assert!(msg.contains("GRA11"), "error names the region: {msg}");
    assert!(msg.contains("26.04"), "error names the release: {msg}");
}
