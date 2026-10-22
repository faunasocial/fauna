//! The **SSH door**, wizard side — `docs/goal/behavior/onboarding-provisioning.md`
//! § 5 (the `vps-config-ssh-door-toggle`, default off) and
//! `docs/goal/architecture/installers/vps.md` § The SSH door → *The key*.
//!
//! These drive the real `run_provisioning_inner` (deferred-DNS path, the
//! cheapest that reaches `create_server`) against a wiremock cloud and read the
//! cloud-init the provider RECEIVED: off renders no door at all; on renders the
//! key-only sshd with exactly the public key the device minted, and the private
//! half is in this device's `~/.ssh` (desktop) or waits for its one hand-over
//! (phone/web) — never in a snapshot, never in the provider's request.

use std::sync::Arc;

use fauna_onboarding_machine::nest_api::ClaimAdminResponse;
use fauna_onboarding_machine::{FakeNestApi, OnboardingMachine, observer::CountingObserver};
use fauna_provisioning::progress::OverallStatus;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DOOR_DROPIN: &str = "/etc/ssh/sshd_config.d/0-fauna-door.conf";

async fn mount_cloud(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"servers": []})))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/servers"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_string(r#"{"server":{"id":42,"public_net":{"ipv4":{"ip":"5.6.7.8"}}}}"#),
        )
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/servers/42/actions/change_dns_ptr"))
        .respond_with(ResponseTemplate::new(201))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/servers/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "server": {"public_net": {"ipv4": {"dns_ptr": null}}}
        })))
        .mount(server)
        .await;
}

fn provider_urls(server: &MockServer) -> std::collections::HashMap<String, String> {
    [
        ("vps".to_string(), server.uri()),
        ("dns".to_string(), server.uri()),
        ("nest".to_string(), server.uri()),
    ]
    .into_iter()
    .collect()
}

fn machine(server: &MockServer) -> Arc<OnboardingMachine> {
    let m = OnboardingMachine::new_with_provider_base_urls(
        CountingObserver::new(),
        Some(provider_urls(server)),
    );
    seed_run(&m);
    m
}

/// Identity, handle, a selected VPS, mail off, DNS deferred.
fn seed_run(m: &OnboardingMachine) {
    m.begin_create_identity();
    m.set_current_handle("alice@example.com".into());
    m.select_vps_provider("hetzner".into());
    m.set_vps_cred("api-token".into(), "test-token".into());
    m.set_vps_state_for_test(|v| {
        v.server_types = vec![fauna_provisioning::vps::ServerTypeInfo {
            id: "cx22".into(),
            vcpu: 2,
            mem_gb: 4.0,
            disk_gb: 40,
            price_monthly_cents: 500,
            currency: "EUR".into(),
        }];
        v.selected_server_type_id = Some("cx22".into());
        v.locations = vec![fauna_provisioning::vps::VpsLocation {
            id: "fsn1".into(),
            name: "Falkenstein".into(),
            city: "Falkenstein".into(),
            country: "DE".into(),
        }];
        v.selected_location_id = Some("fsn1".into());
        v.enable_mail = Some(false);
    });
    m.set_dns_state_for_test(|d| d.set_up_later = true);
}

/// Run to a terminal status (convention 14: a deadline on a named state, never
/// a fixed settle) and return the cloud-init `POST /servers` carried.
async fn run_and_read_user_data(server: &MockServer, m: &Arc<OnboardingMachine>) -> String {
    m.clone().start_provisioning();
    // The create first: a re-run starts from the last run's terminal status.
    for _ in 0..600 {
        let created = server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .any(|r| r.method.as_str() == "POST" && r.url.path() == "/servers");
        if created {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    for _ in 0..600 {
        if !matches!(
            m.provisioning_snapshot().overall,
            OverallStatus::Idle | OverallStatus::Running
        ) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        m.provisioning_snapshot().overall,
        OverallStatus::Succeeded,
        "{:?} / {:?}",
        m.provisioning_snapshot().final_error,
        m.error_message()
    );
    let requests = server.received_requests().await.expect("recorded");
    let create = requests
        .iter()
        .find(|r| r.method.as_str() == "POST" && r.url.path() == "/servers")
        .expect("one create_server");
    let body: serde_json::Value = serde_json::from_slice(&create.body).unwrap();
    body["user_data"].as_str().expect("user_data").to_string()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fauna-door-wizard-{name}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// No secret material in anything the machine shows or a log could print.
fn assert_nowhere(m: &OnboardingMachine, user_data: &str, private: &str) {
    let body = private
        .lines()
        .find(|l| !l.starts_with("-----"))
        .expect("a base64 body line");
    assert!(
        !user_data.contains(body),
        "the private key reached the provider"
    );
    for (what, text) in [
        (
            "vps_config",
            serde_json::to_string(&m.vps_config()).unwrap(),
        ),
        (
            "provisioning_snapshot",
            serde_json::to_string(&m.provisioning_snapshot()).unwrap(),
        ),
        ("readout", format!("{:?}", m.ssh_door_readout())),
    ] {
        assert!(!text.contains(body), "the private key leaked into {what}");
    }
}

#[test]
fn the_toggle_is_off_by_default_and_follows_the_gesture() {
    let m = OnboardingMachine::new(CountingObserver::new());
    assert!(!m.provision_ssh_door_enabled(), "default off for every box");
    m.set_provision_ssh_door(true);
    assert!(m.provision_ssh_door_enabled());
    m.set_provision_ssh_door(false);
    assert!(!m.provision_ssh_door_enabled());
}

#[tokio::test]
async fn off_renders_no_door_and_mints_no_key() {
    let server = MockServer::start().await;
    mount_cloud(&server).await;
    let dir = scratch("off");
    let m = machine(&server);
    m.set_ssh_door_key_dir(Some(dir.join(".ssh").display().to_string()));

    let user_data = run_and_read_user_data(&server, &m).await;
    assert!(!user_data.contains(DOOR_DROPIN), "{user_data}");
    assert!(!user_data.contains("ssh-ed25519"), "{user_data}");
    assert!(!user_data.contains("ssh_pwauth"), "{user_data}");
    assert!(m.ssh_door_readout().is_none());
    assert!(m.take_ssh_door_private_key().is_none());
    assert!(!dir.join(".ssh").exists(), "nothing written to ~/.ssh");
    std::fs::remove_dir_all(dir).unwrap();
}

/// A desktop app: the key lands in `~/.ssh` with its `Host` entry, the box's
/// cloud-init carries exactly its public half, and the machine keeps no copy.
#[tokio::test]
async fn on_a_desktop_the_key_goes_to_dot_ssh_and_the_box_gets_its_public_half() {
    let server = MockServer::start().await;
    mount_cloud(&server).await;
    let home = scratch("desktop");
    let ssh = home.join(".ssh");
    let m = machine(&server);
    m.set_ssh_door_key_dir(Some(ssh.display().to_string()));
    m.set_provision_ssh_door(true);

    let user_data = run_and_read_user_data(&server, &m).await;
    assert!(user_data.contains(DOOR_DROPIN), "{user_data}");

    let readout = m.ssh_door_readout().expect("a door run has a readout");
    let key_path = ssh.join("fauna-example.com");
    assert_eq!(readout.domain, "example.com");
    assert_eq!(readout.command, "ssh example.com");
    assert_eq!(
        readout.key_path.as_deref(),
        Some(key_path.display().to_string().as_str())
    );
    let private = std::fs::read_to_string(&key_path).expect("the key file");
    assert!(private.starts_with(fauna_provisioning::ssh_door::PEM_BEGIN));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&key_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let config = std::fs::read_to_string(ssh.join("config")).unwrap();
    assert!(
        config.contains("Host example.com\n    User root\n"),
        "{config}"
    );

    // The box admits the key this device holds: the authorized line's key
    // material is the file's public half (the comment names the box).
    let authorized = user_data
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("- ssh-ed25519 ")
                .or_else(|| l.trim().strip_prefix("ssh-ed25519 "))
        })
        .unwrap_or_else(|| panic!("an ssh-ed25519 line in:\n{user_data}"));
    assert!(
        authorized.ends_with("fauna-door@example.com"),
        "{authorized}"
    );
    assert!(
        readout.fingerprint.starts_with("SHA256:"),
        "{}",
        readout.fingerprint
    );

    assert!(
        m.take_ssh_door_private_key().is_none(),
        "a desktop app's key is in ~/.ssh; the machine keeps no copy"
    );
    assert_nowhere(&m, &user_data, &private);
    std::fs::remove_dir_all(home).unwrap();
}

/// A phone or web: no `~/.ssh`, so the key waits for its ONE hand-over and the
/// command names the handed-over file.
#[tokio::test]
async fn without_dot_ssh_the_key_is_handed_over_once() {
    let server = MockServer::start().await;
    mount_cloud(&server).await;
    let m = machine(&server);
    m.set_provision_ssh_door(true);

    let user_data = run_and_read_user_data(&server, &m).await;
    assert!(user_data.contains(DOOR_DROPIN));
    let readout = m.ssh_door_readout().expect("readout");
    assert_eq!(readout.command, "ssh -i fauna-example.com root@example.com");
    assert_eq!(readout.key_path, None);

    let private = m.take_ssh_door_private_key().expect("the one hand-over");
    assert!(private.starts_with(fauna_provisioning::ssh_door::PEM_BEGIN));
    assert!(m.take_ssh_door_private_key().is_none(), "once");
    assert!(m.ssh_door_readout().is_some(), "the command stays shown");
    assert_nowhere(&m, &user_data, &private);
}

/// A retried run renders the key the first run minted — the box a Retry
/// resumes already carries it.
#[tokio::test]
async fn a_retry_reuses_the_minted_key() {
    let server = MockServer::start().await;
    mount_cloud(&server).await;
    let m = machine(&server);
    m.set_provision_ssh_door(true);
    let first = run_and_read_user_data(&server, &m).await;
    let fp = m.ssh_door_readout().unwrap().fingerprint;

    server.reset().await;
    mount_cloud(&server).await;
    let second = run_and_read_user_data(&server, &m).await;
    let line = |u: &str| {
        u.lines()
            .find(|l| l.contains("ssh-ed25519 "))
            .map(str::to_string)
            .unwrap()
    };
    assert_eq!(line(&first), line(&second));
    assert_eq!(m.ssh_door_readout().unwrap().fingerprint, fp);

    // Turned off before a later run: that run shows no door.
    m.set_provision_ssh_door(false);
    server.reset().await;
    mount_cloud(&server).await;
    let third = run_and_read_user_data(&server, &m).await;
    assert!(!third.contains(DOOR_DROPIN));
    assert!(m.ssh_door_readout().is_none());
    assert!(m.take_ssh_door_private_key().is_none());
}

// ── The one admitted address (onboarding-provisioning.md § 5) ────────────────

/// The standard path's DNS steps and the box's `/health`, beside
/// [`mount_cloud`]: the run reaches the claim, which the fake nest API plays.
async fn mount_dns_and_health(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/zones"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "zones": [{"id": 1, "name": "example.com"}]
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/zones/1/rrsets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"rrsets": []})))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/zones/1/rrsets"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({})))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/health"))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
}

/// A standard (DNS not deferred) run whose claim the fake nest answers with
/// `ssh_door_source`.
async fn claiming_machine(
    server: &MockServer,
    ssh_door_source: Option<&str>,
) -> Arc<OnboardingMachine> {
    mount_cloud(server).await;
    mount_dns_and_health(server).await;
    let fake = Arc::new(FakeNestApi::new());
    fake.set_claim_admin_response(Ok(ClaimAdminResponse {
        ok: true,
        token: Some("tok".into()),
        expires_at: Some(0),
        ssh_door_source: ssh_door_source.map(str::to_string),
        ..Default::default()
    }));
    let m = OnboardingMachine::with_nest_api_and_provider_base_urls(
        CountingObserver::new(),
        fake,
        Some(provider_urls(server)),
    );
    seed_run(&m);
    m.select_dns_provider("hetzner".into());
    m.set_dns_cred("api-token".into(), "test-token".into());
    m.set_dns_state_for_test(|d| {
        d.set_up_later = false;
        d.buy_domain = false;
    });
    m
}

/// The claim reply's echo of the address the nest handed the host is what
/// the provisioning page names as the door's one admitted address — at once,
/// though the host's door pass admits it up to a minute later.
#[tokio::test]
async fn the_claim_replys_address_is_the_doors_admitted_address() {
    let server = MockServer::start().await;
    let m = claiming_machine(&server, Some("203.0.113.7")).await;
    m.set_provision_ssh_door(true);
    let user_data = run_and_read_user_data(&server, &m).await;
    assert!(user_data.contains(DOOR_DROPIN));
    let readout = m.ssh_door_readout().expect("a door run has a readout");
    assert_eq!(readout.admitted_from.as_deref(), Some("203.0.113.7"));
}

/// A nest that wrote no address (no maintenance mount) leaves the readout
/// without one; a door-less run shows no door whatever the nest echoed.
#[tokio::test]
async fn no_echoed_address_or_no_door_names_none() {
    let server = MockServer::start().await;
    let m = claiming_machine(&server, None).await;
    m.set_provision_ssh_door(true);
    run_and_read_user_data(&server, &m).await;
    assert_eq!(m.ssh_door_readout().expect("readout").admitted_from, None);

    let server = MockServer::start().await;
    let m = claiming_machine(&server, Some("203.0.113.7")).await;
    run_and_read_user_data(&server, &m).await;
    assert!(m.ssh_door_readout().is_none());
}
