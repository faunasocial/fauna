//! The VPS offers follow the chosen location —
//! `docs/goal/architecture/provisioning/registry.md` § Server offers follow the
//! location, and `docs/goal/behavior/onboarding-provisioning.md` § 5 ("the
//! radio lists the offers of the selected location and re-lists when it
//! changes"). Verify lists for the default location; a location change
//! re-lists through `select_vps_location`, dropping a selected offer the new
//! location does not sell; the same location again lists nothing.

use std::sync::Arc;
use std::time::Duration;

use fauna_onboarding_machine::{OnboardingMachine, observer::CountingObserver};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A DigitalOcean account with two regions; `s-2vcpu-4gb` is sold only in
/// `nyc3`, and `s-1vcpu-2gb` costs the same in both.
async fn mount_digitalocean(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/regions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "regions": [
                {"slug": "nyc3", "name": "New York 3", "available": true},
                {"slug": "ams3", "name": "Amsterdam 3", "available": true}
            ]
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sizes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sizes": [
                {"slug": "s-1vcpu-2gb", "vcpus": 1, "memory": 2048, "disk": 50,
                 "price_monthly": 12.0, "available": true, "regions": ["nyc3", "ams3"]},
                {"slug": "s-2vcpu-4gb", "vcpus": 2, "memory": 4096, "disk": 80,
                 "price_monthly": 24.0, "available": true, "regions": ["nyc3"]}
            ]
        })))
        .mount(server)
        .await;
}

async fn verified_machine(server: &MockServer) -> Arc<OnboardingMachine> {
    mount_digitalocean(server).await;
    let urls = [("vps".to_string(), server.uri())].into_iter().collect();
    let m = OnboardingMachine::new_with_provider_base_urls(CountingObserver::new(), Some(urls));
    m.select_vps_provider("digitalocean".into());
    m.set_vps_cred("api-token".into(), "test-token".into());
    m.verify_vps().await.expect("verify");
    m
}

fn offer_ids(m: &OnboardingMachine) -> Vec<String> {
    m.vps_config()
        .server_types
        .into_iter()
        .map(|t| t.id)
        .collect()
}

async fn size_listings(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.path() == "/sizes")
        .count()
}

#[tokio::test]
async fn verify_lists_the_offers_of_the_default_location() {
    let server = MockServer::start().await;
    let m = verified_machine(&server).await;

    assert_eq!(m.vps_config().selected_location_id.as_deref(), Some("nyc3"));
    assert_eq!(offer_ids(&m), ["s-1vcpu-2gb", "s-2vcpu-4gb"]);
}

#[tokio::test]
async fn a_location_change_re_lists_in_the_background() {
    let server = MockServer::start().await;
    let m = verified_machine(&server).await;

    // The setter is sync and starts the refresh on the current runtime.
    m.select_vps_location("ams3".into());

    tokio::time::timeout(Duration::from_secs(10), async {
        while offer_ids(&m) != ["s-1vcpu-2gb"] {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("offers never re-listed for ams3: {:?}", offer_ids(&m)));
}

#[tokio::test]
async fn a_location_change_drops_a_selection_the_new_location_lacks() {
    let server = MockServer::start().await;
    let m = verified_machine(&server).await;
    m.select_vps_server_type("s-2vcpu-4gb".into());

    m.select_vps_location("ams3".into());
    m.clone().refresh_vps_offers().await;

    assert_eq!(offer_ids(&m), ["s-1vcpu-2gb"]);
    assert_eq!(m.vps_config().selected_server_type_id, None);
}

#[tokio::test]
async fn a_location_change_keeps_a_selection_the_new_location_sells() {
    let server = MockServer::start().await;
    let m = verified_machine(&server).await;
    m.select_vps_server_type("s-1vcpu-2gb".into());

    m.select_vps_location("ams3".into());
    m.clone().refresh_vps_offers().await;

    assert_eq!(
        m.vps_config().selected_server_type_id.as_deref(),
        Some("s-1vcpu-2gb")
    );
}

#[tokio::test]
async fn the_same_location_again_lists_nothing() {
    let server = MockServer::start().await;
    let m = verified_machine(&server).await;
    assert_eq!(size_listings(&server).await, 1);

    m.select_vps_location("nyc3".into());
    m.clone().refresh_vps_offers().await;

    assert_eq!(size_listings(&server).await, 1);
    assert_eq!(offer_ids(&m), ["s-1vcpu-2gb", "s-2vcpu-4gb"]);
}

#[tokio::test]
async fn before_verify_a_location_lists_nothing() {
    let server = MockServer::start().await;
    mount_digitalocean(&server).await;
    let urls = [("vps".to_string(), server.uri())].into_iter().collect();
    let m = OnboardingMachine::new_with_provider_base_urls(CountingObserver::new(), Some(urls));
    m.select_vps_provider("digitalocean".into());
    m.set_vps_cred("api-token".into(), "test-token".into());

    m.select_vps_location("ams3".into());
    m.clone().refresh_vps_offers().await;

    assert_eq!(size_listings(&server).await, 0);
    assert!(offer_ids(&m).is_empty());
}
