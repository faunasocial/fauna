//! The DNS-to-server credential mirror runs the server listing itself —
//! `docs/goal/behavior/onboarding-provisioning.md` § 3d → *Why one provider's
//! token was asked for twice*. With `dns-same-provider-checkbox` on, Continue
//! on `dns_config` carries the provider's token to the server step; the
//! server page must then list the locations and offers on its own, so the
//! person never types the token a second time, and while that check runs the
//! page says it is checking rather than asking for the token.
//!
//! Every test waits for the background check to land before it ends (see
//! `vps_offers_follow_location.rs` on wiremock's pooled servers).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use fauna_onboarding_machine::{OnboardingMachine, OnboardingStep, observer::CountingObserver};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A Hetzner project (one token covers DNS and the server) with one location
/// and one server type; `locations_status` answers the token check and
/// `delay` holds every answer back — the slow provider.
async fn mount_hetzner(server: &MockServer, locations_status: u16, delay: Duration) {
    Mock::given(method("GET"))
        .and(path("/locations"))
        .respond_with(
            ResponseTemplate::new(locations_status)
                .set_body_json(serde_json::json!({
                    "locations": [{"name": "fsn1", "description": "Falkenstein DC Park 1",
                                   "city": "Falkenstein", "country": "DE"}]
                }))
                .set_delay(delay),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/server_types"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "server_types": [{"name": "cx23", "cores": 2, "memory": 4.0, "disk": 40,
                "prices": [{"location": "fsn1", "price_monthly": {"gross": "4.5100"}}]}]
        })))
        .mount(server)
        .await;
}

/// A machine whose DNS step holds Hetzner's token with the same-provider box
/// on (its default) — the state the person leaves `dns_config` in: the token
/// verified and the handle's zone found on it, so Continue is offered
/// (`continue_from_dns` refuses a press the page would not offer). The verify
/// itself is seeded rather than driven: these tests are about the mirror.
fn dns_step_done(server: &MockServer) -> Arc<OnboardingMachine> {
    let urls: HashMap<String, String> = ["vps", "dns", "nest"]
        .into_iter()
        .map(|k| (k.to_string(), server.uri()))
        .collect();
    let m = OnboardingMachine::new_with_provider_base_urls(CountingObserver::new(), Some(urls));
    m.set_current_handle("admin@example.com".into());
    m.select_dns_provider("hetzner".into());
    m.set_dns_cred("api-token".into(), "tok".into());
    m.set_dns_state_for_test(|d| {
        d.verified = true;
        d.buy_domain = false;
        d.current_zones = vec![fauna_provisioning::dns::DnsZone {
            id: "z".into(),
            name: "example.com".into(),
            nameservers: Vec::new(),
        }];
    });
    assert!(
        m.dns_continue_possibility().is_possible(),
        "the DNS step is done: Continue is offered"
    );
    assert!(
        m.dns_config().same_provider_for_vps,
        "the box is on by default"
    );
    m
}

/// Wait until the background check has ended (`verifying` false again).
async fn check_settles(m: &OnboardingMachine) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while m.vps_config().verifying
            || m.vps_config().locations.is_empty() && m.error_message().is_none()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the mirrored credential's check never ended");
}

fn continue_reason(m: &OnboardingMachine) -> Option<String> {
    m.vps_continue_blocked_reason().map(|t| t.key)
}

#[tokio::test]
async fn a_mirrored_credential_arrives_with_locations_and_offers_listed() {
    let server = MockServer::start().await;
    mount_hetzner(&server, 200, Duration::ZERO).await;
    let m = dns_step_done(&server);

    m.continue_from_dns().unwrap();
    assert_eq!(m.step(), OnboardingStep::VpsConfig);
    check_settles(&m).await;

    // No `set_vps_cred`, no `verify_vps`: the page opens ready.
    let vps = m.vps_config();
    assert_eq!(vps.selected_provider_id.as_deref(), Some("hetzner"));
    assert!(vps.verified);
    assert_eq!(
        vps.locations
            .iter()
            .map(|l| l.id.as_str())
            .collect::<Vec<_>>(),
        ["fsn1"]
    );
    assert_eq!(vps.selected_location_id.as_deref(), Some("fsn1"));
    assert_eq!(
        vps.server_types
            .iter()
            .map(|t| t.id.as_str())
            .collect::<Vec<_>>(),
        ["cx23"]
    );
    // What is left is the person's choice of plan, never the token again.
    assert_eq!(
        continue_reason(&m).as_deref(),
        Some("onboarding.vps_config.status_pick_server_type")
    );
}

#[tokio::test]
async fn while_the_mirrored_credential_is_checked_the_page_says_so() {
    let server = MockServer::start().await;
    mount_hetzner(&server, 200, Duration::from_millis(500)).await;
    let m = dns_step_done(&server);

    m.continue_from_dns().unwrap();

    // The page opens checking — never "enter this provider's credentials".
    assert!(m.vps_config().verifying);
    assert!(!m.can_verify_vps(), "no second Verify while the check runs");
    assert_eq!(
        continue_reason(&m).as_deref(),
        Some("provisioning.checking_credentials")
    );
    check_settles(&m).await;
    assert!(m.vps_config().verified);
}

#[tokio::test]
async fn a_failed_check_of_the_mirrored_credential_surfaces_the_providers_error() {
    let server = MockServer::start().await;
    mount_hetzner(&server, 401, Duration::ZERO).await;
    let m = dns_step_done(&server);

    m.continue_from_dns().unwrap();
    check_settles(&m).await;

    let vps = m.vps_config();
    assert!(!vps.verified && !vps.verifying);
    assert!(vps.locations.is_empty());
    let err = m.error_message().expect("the provider's error is shown");
    assert!(err.starts_with("Verify failed:"), "{err}");
    assert_eq!(
        continue_reason(&m).as_deref(),
        Some("onboarding.vps_config.status_verify_credentials")
    );
}

/// A UniFFI shell presses Continue from its UI thread, which has no tokio
/// runtime: the check must still run.
#[test]
fn the_mirror_lists_for_a_caller_with_no_runtime() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (_server, m) = rt.block_on(async {
        let server = MockServer::start().await;
        mount_hetzner(&server, 200, Duration::ZERO).await;
        let m = dns_step_done(&server);
        (server, m)
    });
    assert!(tokio::runtime::Handle::try_current().is_err());

    m.continue_from_dns().unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !m.vps_config().verified {
        assert!(
            std::time::Instant::now() < deadline,
            "the mirrored credential was never checked"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(m.vps_config().selected_location_id.as_deref(), Some("fsn1"));
}
