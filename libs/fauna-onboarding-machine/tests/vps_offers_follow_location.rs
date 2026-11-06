//! The VPS offers follow the chosen location —
//! `docs/goal/architecture/provisioning/registry.md` § Server offers follow the
//! location, and `docs/goal/behavior/onboarding-provisioning.md` § 5 ("the
//! radio lists the offers of the selected location and re-lists when it
//! changes"). Verify lists for the default location; a location change
//! re-lists through `select_vps_location`, dropping a selected offer the new
//! location does not sell; the same location again lists nothing; each change
//! lists exactly once.
//!
//! Every test that starts a re-list waits for it to land before it ends:
//! wiremock hands each `MockServer::start` a pooled server, so a listing still
//! in flight when one test drops its server is received — and counted — by
//! the next test that draws the same one.

use std::sync::Arc;
use std::time::Duration;

use fauna_onboarding_machine::{OnboardingMachine, observer::CountingObserver};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A DigitalOcean account with two regions; `s-2vcpu-4gb` is sold only in
/// `nyc3`, and `s-1vcpu-2gb` costs the same in both.
async fn mount_digitalocean(server: &MockServer) {
    mount_digitalocean_with_sizes_delay(server, Duration::ZERO).await;
}

/// [`mount_digitalocean`], each size listing answered only after `delay` —
/// the slow provider that holds a listing in flight.
async fn mount_digitalocean_with_sizes_delay(server: &MockServer, delay: Duration) {
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
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({
                    "sizes": [
                        {"slug": "s-1vcpu-2gb", "vcpus": 1, "memory": 2048, "disk": 50,
                         "price_monthly": 12.0, "available": true, "regions": ["nyc3", "ams3"]},
                        {"slug": "s-2vcpu-4gb", "vcpus": 2, "memory": 4096, "disk": 80,
                         "price_monthly": 24.0, "available": true, "regions": ["nyc3"]}
                    ]
                }))
                .set_delay(delay),
        )
        .mount(server)
        .await;
}

async fn verified_machine(server: &MockServer) -> Arc<OnboardingMachine> {
    mount_digitalocean(server).await;
    verify(server).await
}

async fn verify(server: &MockServer) -> Arc<OnboardingMachine> {
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

/// Wait until the offers read `want` — the background re-list has landed.
async fn offers_settle(m: &OnboardingMachine, want: &[&str]) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while offer_ids(m) != want {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("offers never settled on {want:?}: {:?}", offer_ids(m)));
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

    // The setter is sync and starts the refresh on the machine's own runtime.
    m.select_vps_location("ams3".into());

    offers_settle(&m, &["s-1vcpu-2gb"]).await;
    assert_eq!(size_listings(&server).await, 2, "verify's listing + ams3's");
}

/// A change back to a location whose listing is still in flight lists
/// nothing more: with the provider slow, `ams3 → nyc3 → ams3` lists ams3
/// once (and nyc3, verify's, not again).
#[tokio::test]
async fn a_location_listing_in_flight_is_not_listed_again() {
    let server = MockServer::start().await;
    mount_digitalocean_with_sizes_delay(&server, Duration::from_millis(500)).await;
    let m = verify(&server).await;

    m.select_vps_location("ams3".into());
    m.select_vps_location("nyc3".into());
    m.select_vps_location("ams3".into());

    offers_settle(&m, &["s-1vcpu-2gb"]).await;
    assert_eq!(
        size_listings(&server).await,
        2,
        "verify's listing + one for ams3"
    );
}

#[tokio::test]
async fn a_location_change_drops_a_selection_the_new_location_lacks() {
    let server = MockServer::start().await;
    let m = verified_machine(&server).await;
    m.select_vps_server_type("s-2vcpu-4gb".into());

    m.select_vps_location("ams3".into());
    offers_settle(&m, &["s-1vcpu-2gb"]).await;

    assert_eq!(offer_ids(&m), ["s-1vcpu-2gb"]);
    assert_eq!(m.vps_config().selected_server_type_id, None);
}

#[tokio::test]
async fn a_location_change_keeps_a_selection_the_new_location_sells() {
    let server = MockServer::start().await;
    let m = verified_machine(&server).await;
    m.select_vps_server_type("s-1vcpu-2gb".into());

    m.select_vps_location("ams3".into());
    offers_settle(&m, &["s-1vcpu-2gb"]).await;

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

    // Unchanged: nothing is started, so there is nothing to wait for.
    m.select_vps_location("nyc3".into());

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

    // The started refresh returns before any listing, unverified; verify's
    // own path is the first that may list.
    m.select_vps_location("ams3".into());
    m.verify_vps().await.expect("verify");

    assert_eq!(m.vps_config().selected_location_id.as_deref(), Some("ams3"));
    assert_eq!(
        size_listings(&server).await,
        1,
        "verify's listing, for ams3"
    );
    assert_eq!(offer_ids(&m), ["s-1vcpu-2gb"]);
}

/// A UniFFI shell calls the setter from its UI thread, which has no tokio
/// runtime (the Swift main thread, a WinUI or Android UI thread): the re-list
/// must still run, with nothing more from the caller than the setter.
#[test]
fn a_location_change_re_lists_for_a_caller_with_no_runtime() {
    // The fake and the verify run on a runtime of the test's own, which keeps
    // the fake serving; the setter is then called from this thread, outside it.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (_server, m) = rt.block_on(async {
        let server = MockServer::start().await;
        let m = verified_machine(&server).await;
        (server, m)
    });
    assert!(tokio::runtime::Handle::try_current().is_err());

    m.select_vps_location("ams3".into());

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while offer_ids(&m) != ["s-1vcpu-2gb"] {
        assert!(
            std::time::Instant::now() < deadline,
            "offers never re-listed for ams3: {:?}",
            offer_ids(&m)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
