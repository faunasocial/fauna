//! VPS `list_managed_servers` conformance harness — pins, per provider, the
//! **marker filter** the adapter sends, that it **follows pagination to the
//! end**, and that a **non-fauna server is excluded** even when the provider
//! ignores the filter. Mirrors `vps_delete_server_conformance.rs`.
//!
//! This is the listing primitive behind the retire view
//! (`docs/goal/architecture/installers/vps.md` § Uninstall → *Listing
//! primitive*; the app half is `docs/goal/behavior/nest-retirement.md`).
//! Filtering to `managed-by=fauna` is what keeps a person from ever being
//! shown — let alone able to delete — a non-fauna server that happens to live
//! in the same cloud account, so the exclusion pin is a safety property, not a
//! tidiness one. The adapter re-checks the marker client-side rather than
//! trusting the query: a provider that silently ignores an unknown filter
//! would otherwise hand the view the user's whole account.
//!
//! OVH is the deliberate exception — its instance API has no label field at
//! all (`create_server` documents the no-op), so it lists the project
//! unfiltered with every row `marked: false`, and the view's typed per-box
//! confirm is what discharges the risk there.

use fauna_provisioning::vps::digitalocean::DigitalOcean;
use fauna_provisioning::vps::hetzner::Hetzner;
use fauna_provisioning::vps::linode::Linode;
use fauna_provisioning::vps::ovh::Ovh;
use fauna_provisioning::vps::vultr::Vultr;
use fauna_provisioning::vps::{MANAGED_BY_LABEL, VpsProvider};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// `managed-by=fauna` — the selector spelling (Hetzner, bundled provider).
fn selector() -> String {
    format!("{}={}", MANAGED_BY_LABEL.0, MANAGED_BY_LABEL.1)
}

/// `managed-by:fauna` — the flat tag spelling (DigitalOcean, Linode, Vultr).
fn tag() -> String {
    format!("{}:{}", MANAGED_BY_LABEL.0, MANAGED_BY_LABEL.1)
}

// --------------------------------------------------------------------------
// Hetzner — GET /servers?label_selector=managed-by=fauna, page-number paging
// --------------------------------------------------------------------------

#[tokio::test]
async fn hetzner_list_sends_label_selector_and_follows_pagination() {
    let mock = MockServer::start().await;

    // Page 1 — carries `meta.pagination.next_page: 2`.
    Mock::given(method("GET"))
        .and(path("/servers"))
        .and(query_param("label_selector", selector()))
        .and(query_param("page", "1"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "servers": [{
                "id": 1,
                "name": "alpha-example-com",
                "labels": { "managed-by": "fauna" },
                "created": "2026-01-02T03:04:05+00:00",
                "public_net": {
                    "ipv4": { "ip": "203.0.113.1" },
                    "ipv6": { "ip": "2001:db8::1/64" }
                }
            }],
            "meta": { "pagination": { "next_page": 2 } }
        })))
        .expect(1)
        .mount(&mock)
        .await;

    // Page 2 — `next_page: null` ends the walk.
    Mock::given(method("GET"))
        .and(path("/servers"))
        .and(query_param("label_selector", selector()))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "servers": [{
                "id": 2,
                "name": "beta-example-com",
                "labels": { "managed-by": "fauna" },
                "public_net": { "ipv4": { "ip": "203.0.113.2" } }
            }],
            "meta": { "pagination": { "next_page": null } }
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let hetzner = Hetzner::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let servers = hetzner.list_managed_servers(&client).await.unwrap();

    assert_eq!(servers.len(), 2, "both pages must be walked: {servers:?}");
    assert_eq!(servers[0].server_id, "1");
    assert_eq!(servers[0].name, "alpha-example-com");
    assert_eq!(servers[0].ipv4.as_deref(), Some("203.0.113.1"));
    assert_eq!(servers[0].ipv6.as_deref(), Some("2001:db8::1/64"));
    assert_eq!(
        servers[0].created_at.as_deref(),
        Some("2026-01-02T03:04:05+00:00")
    );
    assert!(servers[0].marked, "a label-filtered row is marked");
    assert_eq!(servers[1].server_id, "2");
    assert_eq!(servers[1].ipv6, None, "absent ipv6 stays None");
}

#[tokio::test]
async fn hetzner_list_excludes_a_non_fauna_server() {
    let mock = MockServer::start().await;
    // The provider ignores the selector and answers with the whole account.
    Mock::given(method("GET"))
        .and(path("/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "servers": [
                {
                    "id": 1,
                    "name": "fauna-box",
                    "labels": { "managed-by": "fauna" },
                    "public_net": { "ipv4": { "ip": "203.0.113.1" } }
                },
                {
                    "id": 99,
                    "name": "someone-elses-production-db",
                    "labels": { "team": "data" },
                    "public_net": { "ipv4": { "ip": "203.0.113.99" } }
                }
            ],
            "meta": { "pagination": { "next_page": null } }
        })))
        .mount(&mock)
        .await;

    let hetzner = Hetzner::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let servers = hetzner.list_managed_servers(&client).await.unwrap();

    assert_eq!(servers.len(), 1, "unmarked rows must never surface");
    assert_eq!(servers[0].server_id, "1");
}

// --------------------------------------------------------------------------
// DigitalOcean — GET /droplets?tag_name=managed-by:fauna, links.pages.next
// --------------------------------------------------------------------------

#[tokio::test]
async fn digitalocean_list_sends_tag_name_and_follows_pagination() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/droplets"))
        .and(query_param("tag_name", tag()))
        .and(query_param("page", "1"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "droplets": [{
                "id": 777,
                "name": "alpha-example-com",
                "tags": ["managed-by:fauna"],
                "created_at": "2026-01-02T03:04:05Z",
                "networks": {
                    "v4": [
                        { "ip_address": "10.0.0.1", "type": "private" },
                        { "ip_address": "203.0.113.5", "type": "public" }
                    ],
                    "v6": [{ "ip_address": "2001:db8::5", "type": "public" }]
                }
            }],
            "links": { "pages": { "next": "https://api.example/v2/droplets?page=2" } }
        })))
        .expect(1)
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/droplets"))
        .and(query_param("tag_name", tag()))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "droplets": [{
                "id": 778,
                "name": "beta-example-com",
                "tags": ["managed-by:fauna"],
                "networks": { "v4": [{ "ip_address": "203.0.113.6", "type": "public" }] }
            }],
            "links": { "pages": {} }
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let dop = DigitalOcean::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let servers = dop.list_managed_servers(&client).await.unwrap();

    assert_eq!(servers.len(), 2, "both pages must be walked: {servers:?}");
    assert_eq!(servers[0].server_id, "777");
    assert_eq!(servers[0].name, "alpha-example-com");
    assert_eq!(
        servers[0].ipv4.as_deref(),
        Some("203.0.113.5"),
        "the public v4 address, never the private one"
    );
    assert_eq!(servers[0].ipv6.as_deref(), Some("2001:db8::5"));
    assert!(servers[0].marked);
    assert_eq!(
        servers[0].labels,
        vec![("managed-by".to_string(), "fauna".to_string())],
        "flat k:v tags decode back to label pairs"
    );
    assert_eq!(servers[1].server_id, "778");
}

#[tokio::test]
async fn digitalocean_list_excludes_a_non_fauna_server() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/droplets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "droplets": [
                {
                    "id": 777,
                    "name": "fauna-box",
                    "tags": ["managed-by:fauna"],
                    "networks": { "v4": [{ "ip_address": "203.0.113.5", "type": "public" }] }
                },
                {
                    "id": 999,
                    "name": "someone-elses-production-db",
                    "tags": ["team:data"],
                    "networks": { "v4": [{ "ip_address": "203.0.113.99", "type": "public" }] }
                }
            ],
            "links": { "pages": {} }
        })))
        .mount(&mock)
        .await;

    let dop = DigitalOcean::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let servers = dop.list_managed_servers(&client).await.unwrap();

    assert_eq!(servers.len(), 1, "unmarked rows must never surface");
    assert_eq!(servers[0].server_id, "777");
}

// --------------------------------------------------------------------------
// Linode — GET /linode/instances, X-Filter tag match, page/pages paging
// --------------------------------------------------------------------------

#[tokio::test]
async fn linode_list_sends_tag_filter_and_follows_pagination() {
    let mock = MockServer::start().await;

    let expected_filter = serde_json::json!({ "tags": tag() }).to_string();

    Mock::given(method("GET"))
        .and(path("/linode/instances"))
        .and(query_param("page", "1"))
        .and(header("x-filter", expected_filter.as_str()))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "id": 123,
                "label": "alpha-example-com",
                "tags": ["managed-by:fauna"],
                "created": "2026-01-02T03:04:05",
                "ipv4": ["203.0.113.10"],
                "ipv6": "2001:db8::10/128"
            }],
            "page": 1,
            "pages": 2
        })))
        .expect(1)
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/linode/instances"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "id": 124,
                "label": "beta-example-com",
                "tags": ["managed-by:fauna"],
                "ipv4": ["203.0.113.11"]
            }],
            "page": 2,
            "pages": 2
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let linode = Linode::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let servers = linode.list_managed_servers(&client).await.unwrap();

    assert_eq!(servers.len(), 2, "both pages must be walked: {servers:?}");
    assert_eq!(servers[0].server_id, "123");
    assert_eq!(servers[0].name, "alpha-example-com");
    assert_eq!(servers[0].ipv4.as_deref(), Some("203.0.113.10"));
    assert_eq!(servers[0].ipv6.as_deref(), Some("2001:db8::10/128"));
    assert!(servers[0].marked);
    assert_eq!(servers[1].server_id, "124");
}

#[tokio::test]
async fn linode_list_excludes_a_non_fauna_server() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/linode/instances"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                {
                    "id": 123,
                    "label": "fauna-box",
                    "tags": ["managed-by:fauna"],
                    "ipv4": ["203.0.113.10"]
                },
                {
                    "id": 999,
                    "label": "someone-elses-production-db",
                    "tags": [],
                    "ipv4": ["203.0.113.99"]
                }
            ],
            "page": 1,
            "pages": 1
        })))
        .mount(&mock)
        .await;

    let linode = Linode::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let servers = linode.list_managed_servers(&client).await.unwrap();

    assert_eq!(servers.len(), 1, "unmarked rows must never surface");
    assert_eq!(servers[0].server_id, "123");
}

// --------------------------------------------------------------------------
// Vultr — GET /instances?tag=managed-by:fauna, meta.links.next cursor paging
// --------------------------------------------------------------------------

#[tokio::test]
async fn vultr_list_sends_tag_and_follows_cursor_pagination() {
    let mock = MockServer::start().await;

    // First page: no cursor sent, a cursor handed back.
    Mock::given(method("GET"))
        .and(path("/instances"))
        .and(query_param("tag", tag()))
        .and(header("authorization", "Bearer test-token"))
        .and(wiremock::matchers::query_param_is_missing("cursor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "instances": [{
                "id": "cb676a46-066d-49d4-aa03-f075c2ee18ab",
                "label": "alpha-example-com",
                "tags": ["managed-by:fauna"],
                "date_created": "2026-01-02T03:04:05+00:00",
                "main_ip": "203.0.113.20",
                "v6_main_ip": "2001:db8::20"
            }],
            "meta": { "links": { "next": "CURSOR2" } }
        })))
        .expect(1)
        .mount(&mock)
        .await;

    // Second page: the cursor is echoed back, and `next` is empty.
    Mock::given(method("GET"))
        .and(path("/instances"))
        .and(query_param("tag", tag()))
        .and(query_param("cursor", "CURSOR2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "instances": [{
                "id": "dd676a46-066d-49d4-aa03-f075c2ee18ac",
                "label": "beta-example-com",
                "tags": ["managed-by:fauna"],
                "main_ip": "203.0.113.21"
            }],
            "meta": { "links": { "next": "" } }
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let vultr = Vultr::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let servers = vultr.list_managed_servers(&client).await.unwrap();

    assert_eq!(servers.len(), 2, "both pages must be walked: {servers:?}");
    assert_eq!(servers[0].server_id, "cb676a46-066d-49d4-aa03-f075c2ee18ab");
    assert_eq!(servers[0].name, "alpha-example-com");
    assert_eq!(servers[0].ipv4.as_deref(), Some("203.0.113.20"));
    assert_eq!(servers[0].ipv6.as_deref(), Some("2001:db8::20"));
    assert!(servers[0].marked);
    assert_eq!(servers[1].name, "beta-example-com");
}

#[tokio::test]
async fn vultr_list_excludes_a_non_fauna_server() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/instances"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "instances": [
                {
                    "id": "aaaa",
                    "label": "fauna-box",
                    "tags": ["managed-by:fauna"],
                    "main_ip": "203.0.113.20"
                },
                {
                    "id": "bbbb",
                    "label": "someone-elses-production-db",
                    "tags": ["team:data"],
                    "main_ip": "203.0.113.99"
                }
            ],
            "meta": { "links": { "next": "" } }
        })))
        .mount(&mock)
        .await;

    let vultr = Vultr::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let servers = vultr.list_managed_servers(&client).await.unwrap();

    assert_eq!(servers.len(), 1, "unmarked rows must never surface");
    assert_eq!(servers[0].server_id, "aaaa");
}

// --------------------------------------------------------------------------
// OVH — the deliberate exception: unfiltered project list, every row unmarked
// --------------------------------------------------------------------------

#[tokio::test]
async fn ovh_list_is_unfiltered_and_every_row_is_unmarked() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/1.0/auth/time"))
        .respond_with(ResponseTemplate::new(200).set_body_string("1700000000"))
        .mount(&mock)
        .await;

    // No label/tag query param is sent — OVH instance-create has no label
    // field, so there is nothing to filter on and the whole project lists.
    Mock::given(method("GET"))
        .and(path("/1.0/cloud/project/proj-1/instance"))
        .and(wiremock::matchers::header_exists("x-ovh-signature"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "instance-uuid-1",
                "name": "alpha-example-com",
                "created": "2026-01-02T03:04:05+00:00",
                "ipAddresses": [
                    { "ip": "10.0.0.5", "version": 4, "type": "private" },
                    { "ip": "198.51.100.7", "version": 4, "type": "public" },
                    { "ip": "2001:db8::7", "version": 6, "type": "public" }
                ]
            },
            {
                "id": "instance-uuid-2",
                "name": "someone-elses-production-db",
                "ipAddresses": [{ "ip": "198.51.100.8", "version": 4, "type": "public" }]
            }
        ])))
        .expect(1)
        .mount(&mock)
        .await;

    let mut ovh = Ovh::with_base_url(
        "myAppKey".into(),
        "myAppSecret".into(),
        "myConsumerKey".into(),
        mock.uri(),
    );
    ovh.project_id = "proj-1".into();

    let client = reqwest::Client::new();
    let servers = ovh.list_managed_servers(&client).await.unwrap();

    assert_eq!(
        servers.len(),
        2,
        "OVH lists the project unfiltered — the view's typed confirm is the guard"
    );
    assert!(
        servers.iter().all(|s| !s.marked),
        "OVH boxes carry no marker, so every row reports marked: false"
    );
    assert_eq!(servers[0].name, "alpha-example-com");
    assert_eq!(servers[0].ipv4.as_deref(), Some("198.51.100.7"));
    assert_eq!(servers[0].ipv6.as_deref(), Some("2001:db8::7"));
    assert_eq!(
        servers[0].created_at.as_deref(),
        Some("2026-01-02T03:04:05+00:00")
    );
}

// --------------------------------------------------------------------------
// `ManagedServer` → `VpsInstance`, the `delete_server` / `get_ptr` take
// --------------------------------------------------------------------------

#[tokio::test]
async fn a_listed_server_converts_into_the_delete_take() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "servers": [{
                "id": 4242,
                "name": "alpha-example-com",
                "labels": { "managed-by": "fauna" },
                "public_net": { "ipv4": { "ip": "203.0.113.42" } }
            }],
            "meta": { "pagination": { "next_page": null } }
        })))
        .mount(&mock)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/servers/4242"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&mock)
        .await;

    let hetzner = Hetzner::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let servers = hetzner.list_managed_servers(&client).await.unwrap();

    let instance: fauna_provisioning::vps::VpsInstance = (&servers[0]).into();
    assert_eq!(instance.server_id, "4242");
    assert_eq!(instance.ipv4, "203.0.113.42");

    hetzner
        .delete_server(&client, &instance)
        .await
        .expect("a listed row must be deletable without a second lookup");
}
