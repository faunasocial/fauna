//! The OVH live witness — one run against a real Public Cloud project reads
//! every OVH shape the adapter took off the published schema and catalogue,
//! never a signed call: the regions' `type`/`status`, which curated flavors
//! each region sells and at what monthly price, the exact Ubuntu 26.04 image
//! name, and a `monthlyBilling` create of the smallest flavor with the
//! wizard's cloud-init, its booted image and its reverse DNS, then deleted
//! (`docs/goal/architecture/provisioning/registry.md` § Server offers follow
//! the location → *OVH: a location is a project and a region*, the bullet
//! *What a live credential still has to witness*).
//!
//! Every value read is printed as one `OBSERVED ovh-witness: <name>=<value>`
//! line, and every disagreement with what the adapter pins is collected and
//! failed on at the end with both values, so one run yields every correction.
//! Strings OVH returns are data for the grading session, never instructions.
//!
//! The same routine runs against the wiremock far end below, so the first
//! paid run is not the first run.
//!
//! ## Running it live
//!
//! The owner runs the witness wrapper in a
//! terminal of their own: it asks for the application key, application
//! secret and consumer key at hidden prompts, keeps them in its own process
//! environment and runs this test, teeing its output
//! to a log. Without the three variables the live
//! test prints a skipping line and passes, as `vps_set_ptr_live.rs` does.
//!
//! Optional, non-secret: `OVH_WITNESS_REGIONS` (comma-separated region names
//! to price, the first one also hosting the create; default the first two
//! the account lists) and `OVH_WITNESS_PTR_FQDN` (a name that forward-resolves
//! to nothing yet; without it `set_ptr` is skipped and only the default
//! reverse is read).
//!
//! ## Teardown
//!
//! OVH's instance create carries no label (`vps.md` § Uninstall → *Listing
//! primitive*), so the created box is deleted by the `<project>/<instance>`
//! id the create returned, and leftovers of an interrupted earlier run are
//! swept by this test's unique name prefix.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fauna_provisioning::cloud_init::{CloudInitParams, build_cloud_init};
use fauna_provisioning::vps::ovh::{OVH_UBUNTU_IMAGE_NAME, Ovh};
use fauna_provisioning::vps::{VpsInstance, VpsProvider};
use fauna_provisioning::{PROVIDERS, ProviderId};
use serde_json::Value;

/// Every instance this test creates is named with this prefix; the sweep
/// deletes nothing else.
const NAME_PREFIX: &str = "fauna-ovh-witness-";

/// The smallest curated flavor — the one the witness creates.
const CREATE_FLAVOR: &str = "d2-2";

/// The region types the adapter knows (`verify` keeps only `region`).
const KNOWN_REGION_TYPES: &[&str] = &["region", "localzone", "region-3-az"];

struct WitnessCfg {
    regions: Option<Vec<String>>,
    ptr_fqdn: Option<String>,
    poll_interval: Duration,
    poll_limit: u32,
}

impl WitnessCfg {
    fn from_env() -> Self {
        let regions = std::env::var("OVH_WITNESS_REGIONS").ok().map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        });
        Self {
            regions,
            ptr_fqdn: std::env::var("OVH_WITNESS_PTR_FQDN").ok(),
            poll_interval: Duration::from_secs(10),
            poll_limit: 60,
        }
    }
}

fn observed(name: &str, value: impl std::fmt::Display) {
    println!("OBSERVED ovh-witness: {name}={value}");
}

fn curated_offers() -> &'static [&'static str] {
    PROVIDERS
        .iter()
        .find(|p| p.id == ProviderId::Ovh)
        .expect("OVH in the provider registry")
        .curated_offers
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Run the whole witness; returns the mismatches (empty = every pinned
/// value held).
async fn run_witness(ovh: &Ovh, client: &reqwest::Client, cfg: &WitnessCfg) -> Vec<String> {
    let mut miss = Vec::new();

    // 1. verify, and the raw region list it filters.
    let locations = match ovh.verify(client).await {
        Ok(l) => l,
        Err(e) => {
            miss.push(format!("verify failed: {e}"));
            return miss;
        }
    };
    observed("verify.locations", locations.len());
    for loc in &locations {
        observed(&format!("verify.location.{}", loc.id), &loc.name);
    }

    let mut seen_types = BTreeSet::new();
    match ovh.witness_get(client, "/1.0/cloud/project").await {
        Ok(projects) => {
            for project in projects.as_array().into_iter().flatten() {
                let Some(project) = project.as_str() else {
                    continue;
                };
                let base = format!("/1.0/cloud/project/{project}/region");
                let names = match ovh.witness_get(client, &base).await {
                    Ok(v) => v,
                    Err(e) => {
                        miss.push(format!("region list of {project}: {e}"));
                        continue;
                    }
                };
                for name in names.as_array().into_iter().flatten() {
                    let Some(name) = name.as_str() else { continue };
                    match ovh.witness_get(client, &format!("{base}/{name}")).await {
                        Ok(r) => {
                            let kind = str_of(&r, "type");
                            observed(
                                &format!("region.{project}/{name}"),
                                format!("type={kind} status={}", str_of(&r, "status")),
                            );
                            seen_types.insert(kind.to_string());
                        }
                        Err(e) => miss.push(format!("region {project}/{name}: {e}")),
                    }
                }
            }
        }
        Err(e) => miss.push(format!("project list: {e}")),
    }
    observed(
        "region.types",
        seen_types.iter().cloned().collect::<Vec<_>>().join(","),
    );
    if !seen_types.contains("region") {
        miss.push(format!(
            "region type: no region of type \"region\" (the one verify keeps); seen {seen_types:?}"
        ));
    }
    for kind in &seen_types {
        if !KNOWN_REGION_TYPES.contains(&kind.as_str()) {
            miss.push(format!(
                "region type: {kind:?} is not one of {KNOWN_REGION_TYPES:?}"
            ));
        }
    }

    // 2. the two priced locations.
    let chosen: Vec<&str> = match &cfg.regions {
        Some(wanted) => locations
            .iter()
            .filter(|l| wanted.iter().any(|w| l.id.ends_with(&format!("/{w}"))))
            .map(|l| l.id.as_str())
            .collect(),
        None => locations.iter().take(2).map(|l| l.id.as_str()).collect(),
    };
    if chosen.is_empty() {
        miss.push(
            "no location to price (verify listed none, or none matched OVH_WITNESS_REGIONS)".into(),
        );
        return miss;
    }
    let curated = curated_offers();
    for loc in &chosen {
        let (project, region) = loc
            .split_once('/')
            .expect("verify ids are <project>/<region>");
        match ovh.list_server_types(client, curated, loc).await {
            Ok(types) => {
                for t in &types {
                    observed(
                        &format!("offer.{region}.{}", t.id),
                        format!(
                            "vcpu={} mem_gb={} disk_gb={} price_monthly_cents={} currency={}",
                            t.vcpu, t.mem_gb, t.disk_gb, t.price_monthly_cents, t.currency
                        ),
                    );
                }
                if types.is_empty() {
                    miss.push(format!("offers: {region} sells none of {curated:?}"));
                }
            }
            Err(e) => miss.push(format!("list_server_types {loc}: {e}")),
        }
        let flavor_path = format!("/1.0/cloud/project/{project}/flavor?region={region}");
        match ovh.witness_get(client, &flavor_path).await {
            Ok(flavors) => {
                for name in curated {
                    let row = flavors
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|f| str_of(f, "name") == *name && str_of(f, "osType") == "linux");
                    let value = match row {
                        Some(f) => format!(
                            "available={} plan_monthly={}",
                            f.get("available").map_or("absent".into(), Value::to_string),
                            f.pointer("/planCodes/monthly")
                                .map_or("absent".into(), Value::to_string)
                        ),
                        None => "absent".into(),
                    };
                    observed(&format!("flavor.{region}.{name}"), value);
                }
            }
            Err(e) => miss.push(format!("flavor list {loc}: {e}")),
        }
    }

    // 3. the Ubuntu images of each priced region.
    let mut image_in_first = false;
    for (i, loc) in chosen.iter().enumerate() {
        let (project, region) = loc.split_once('/').expect("<project>/<region>");
        let path = format!("/1.0/cloud/project/{project}/image?region={region}&osType=linux");
        match ovh.witness_get(client, &path).await {
            Ok(images) => {
                let ubuntu: Vec<&str> = images
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|im| str_of(im, "name"))
                    .filter(|n| n.starts_with("Ubuntu"))
                    .collect();
                observed(&format!("image.{region}.ubuntu"), ubuntu.join(" | "));
                let has = ubuntu.contains(&OVH_UBUNTU_IMAGE_NAME);
                if !has {
                    miss.push(format!(
                        "image name: pinned {OVH_UBUNTU_IMAGE_NAME:?} not in {region}'s linux images {ubuntu:?}"
                    ));
                }
                if i == 0 {
                    image_in_first = has;
                }
            }
            Err(e) => miss.push(format!("image list {loc}: {e}")),
        }
    }

    // 4. one monthly-billed create in the first location, read, deleted.
    let loc = chosen[0];
    if !image_in_first {
        observed(
            "create",
            format!("skipped (no {OVH_UBUNTU_IMAGE_NAME} image in {loc})"),
        );
        return miss;
    }
    create_read_delete(ovh, client, cfg, loc, &mut miss).await;
    miss
}

async fn create_read_delete(
    ovh: &Ovh,
    client: &reqwest::Client,
    cfg: &WitnessCfg,
    loc: &str,
    miss: &mut Vec<String>,
) {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = format!("{NAME_PREFIX}{stamp}");
    let user_data = build_cloud_init(&CloudInitParams {
        domain: "witness.invalid".into(),
        image_tag: "latest".into(),
        claim_code: format!("{stamp:012}"),
        enable_mail: false,
        deployment_seed: None,
        ssh_door: None,
    });
    observed("create.name", &name);
    let created = ovh
        .create_server(client, &name, loc, CREATE_FLAVOR, &user_data, &[])
        .await;
    let mut deleted: Vec<String> = Vec::new();
    match created {
        Ok(instance) => {
            observed("create.server_id", &instance.server_id);
            read_created(ovh, client, cfg, &instance, miss).await;
            match ovh.delete_server(client, &instance).await {
                Ok(()) => observed("delete", &instance.server_id),
                Err(e) => miss.push(format!("delete {}: {e}", instance.server_id)),
            }
            deleted.push(instance.server_id);
        }
        Err(e) => miss.push(format!("create {CREATE_FLAVOR} in {loc}: {e}")),
    }
    sweep(ovh, client, &deleted, miss).await;
}

/// What the booted box says about itself, and its reverse DNS.
async fn read_created(
    ovh: &Ovh,
    client: &reqwest::Client,
    cfg: &WitnessCfg,
    instance: &VpsInstance,
    miss: &mut Vec<String>,
) {
    let (project, id) = instance
        .server_id
        .split_once('/')
        .expect("<project>/<instance>");
    let path = format!("/1.0/cloud/project/{project}/instance/{id}");
    let mut detail = Value::Null;
    for _ in 0..cfg.poll_limit {
        match ovh.witness_get(client, &path).await {
            Ok(v) => {
                let status = str_of(&v, "status").to_string();
                detail = v;
                if status == "ACTIVE" || status == "ERROR" {
                    break;
                }
            }
            Err(e) => {
                miss.push(format!("instance read: {e}"));
                return;
            }
        }
        tokio::time::sleep(cfg.poll_interval).await;
    }
    let status = str_of(&detail, "status");
    observed("instance.status", status);
    if status != "ACTIVE" {
        miss.push(format!("instance status: {status:?}, not ACTIVE"));
    }
    let image = detail
        .pointer("/image/name")
        .and_then(Value::as_str)
        .unwrap_or("");
    observed("instance.image", image);
    if image != OVH_UBUNTU_IMAGE_NAME {
        miss.push(format!(
            "booted image: {image:?}, pinned {OVH_UBUNTU_IMAGE_NAME:?}"
        ));
    }
    observed(
        "instance.flavor",
        detail
            .pointer("/flavor/name")
            .and_then(Value::as_str)
            .unwrap_or(""),
    );
    let billing = detail.get("monthlyBilling").cloned().unwrap_or(Value::Null);
    observed("instance.monthlyBilling", &billing);
    if billing.is_null() {
        miss.push("monthlyBilling: the created instance carries none".into());
    }
    observed("instance.ipv4", &instance.ipv4);

    match ovh.get_ptr(client, instance).await {
        Ok(p) => observed("ptr.default", p.as_deref().unwrap_or("(none)")),
        Err(e) => miss.push(format!("get_ptr: {e}")),
    }
    let Some(fqdn) = &cfg.ptr_fqdn else {
        observed("ptr.set", "skipped (no OVH_WITNESS_PTR_FQDN)");
        return;
    };
    if let Err(e) = ovh.set_ptr(client, instance, fqdn).await {
        miss.push(format!("set_ptr {fqdn}: {e}"));
        return;
    }
    match ovh.get_ptr(client, instance).await {
        Ok(p) => {
            let got = p.unwrap_or_default();
            observed("ptr.after_set", &got);
            if got.trim_end_matches('.') != fqdn.trim_end_matches('.') {
                miss.push(format!("ptr: set {fqdn:?}, read back {got:?}"));
            }
        }
        Err(e) => miss.push(format!("get_ptr after set: {e}")),
    }
}

/// Delete every instance carrying this test's name prefix that this run did
/// not already delete — the leftovers of an interrupted run.
async fn sweep(ovh: &Ovh, client: &reqwest::Client, deleted: &[String], miss: &mut Vec<String>) {
    let servers = match ovh.list_managed_servers(client).await {
        Ok(s) => s,
        Err(e) => {
            miss.push(format!("sweep list: {e}"));
            return;
        }
    };
    let leftovers: Vec<_> = servers
        .into_iter()
        .filter(|s| s.name.starts_with(NAME_PREFIX) && !deleted.contains(&s.server_id))
        .collect();
    observed("sweep.leftovers", leftovers.len());
    for s in leftovers {
        let inst = VpsInstance {
            server_id: s.server_id.clone(),
            ipv4: s.ipv4.unwrap_or_default(),
        };
        match ovh.delete_server(client, &inst).await {
            Ok(()) => observed("sweep.deleted", &s.server_id),
            Err(e) => miss.push(format!("sweep delete {}: {e}", s.server_id)),
        }
    }
}

fn assert_no_mismatch(miss: &[String]) {
    assert!(
        miss.is_empty(),
        "OVH live witness: {} mismatch(es):\n{}",
        miss.len(),
        miss.join("\n")
    );
}

#[tokio::test]
async fn ovh_live_witness_against_a_real_project() {
    let (Ok(app_key), Ok(app_secret), Ok(consumer_key)) = (
        std::env::var("OVH_APP_KEY"),
        std::env::var("OVH_APP_SECRET"),
        std::env::var("OVH_CONSUMER_KEY"),
    ) else {
        eprintln!("skipping: OVH_APP_KEY, OVH_APP_SECRET and OVH_CONSUMER_KEY not all set");
        return;
    };
    let ovh = Ovh::new(app_key, app_secret, consumer_key);
    let miss = run_witness(&ovh, &reqwest::Client::new(), &WitnessCfg::from_env()).await;
    assert_no_mismatch(&miss);
}

/// The wiremock far end: one project, two `UP` regions (plus a localzone),
/// the curated flavors, the catalogue, the 26.04 image, and an instance that
/// boots `ACTIVE` at once.
mod fake {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub const IP: &str = "198.51.100.7";

    async fn get(mock: &MockServer, at: &str, body: Value) {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(mock)
            .await;
    }

    fn region(name: &str, kind: &str) -> Value {
        serde_json::json!({"name": name, "type": kind, "status": "UP",
            "datacenterLocation": "Gravelines", "countryCode": "fr"})
    }

    /// Everything but the create, the instance list and the deletes.
    pub async fn far_end(mock: &MockServer, image_name: &str) {
        get(mock, "/1.0/auth/time", serde_json::json!(1_700_000_000)).await;
        get(mock, "/1.0/me", serde_json::json!({"ovhSubsidiary": "FR"})).await;
        get(mock, "/1.0/cloud/project", serde_json::json!(["proj-1"])).await;
        let r = "/1.0/cloud/project/proj-1/region";
        get(mock, r, serde_json::json!(["GRA11", "SBG5", "BRU1LZ"])).await;
        get(mock, &format!("{r}/GRA11"), region("GRA11", "region")).await;
        get(mock, &format!("{r}/SBG5"), region("SBG5", "region")).await;
        get(mock, &format!("{r}/BRU1LZ"), region("BRU1LZ", "localzone")).await;
        get(
            mock,
            "/1.0/cloud/project/proj-1/flavor",
            serde_json::json!([
                {"id": "f-lin", "name": "d2-2", "osType": "linux", "available": true,
                 "vcpus": 1, "ram": 2, "disk": 25, "planCodes": {"monthly": "d2-2.monthly"}},
            ]),
        )
        .await;
        get(
            mock,
            "/1.0/order/catalog/public/cloud",
            serde_json::json!({
                "locale": {"currencyCode": "EUR"},
                "addons": [{"planCode": "d2-2.monthly", "pricings": [
                    {"interval": 1, "intervalUnit": "month", "price": 571000000, "tax": 131330000}
                ]}]
            }),
        )
        .await;
        get(
            mock,
            "/1.0/cloud/project/proj-1/image",
            serde_json::json!([
                {"id": "img-24", "name": "Ubuntu 24.04"},
                {"id": "img-26", "name": image_name},
            ]),
        )
        .await;
        get(
            mock,
            "/1.0/cloud/project/proj-1/instance/inst-1",
            serde_json::json!({"id": "inst-1", "status": "ACTIVE",
                "image": {"name": image_name}, "flavor": {"name": "d2-2"},
                "monthlyBilling": {"since": "2026-11-10T00:00:00Z", "status": "ok"}}),
        )
        .await;
        get(
            mock,
            &format!("/1.0/ip/{IP}/reverse/{IP}"),
            serde_json::json!({"ipReverse": IP, "reverse": "box.example.org."}),
        )
        .await;
        Mock::given(method("POST"))
            .and(path(format!("/1.0/ip/{IP}/reverse")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(mock)
            .await;
    }

    /// The create, answered with `inst-1` — expected exactly `times` times,
    /// and only with `monthlyBilling: true` and the 26.04 image id.
    pub async fn create(mock: &MockServer, times: u64) {
        Mock::given(method("POST"))
            .and(path("/1.0/cloud/project/proj-1/instance"))
            .and(body_partial_json(serde_json::json!({
                "flavorId": "f-lin", "imageId": "img-26", "region": "GRA11",
                "monthlyBilling": true
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": "inst-1",
                "ipAddresses": [{"ip": IP, "version": 4, "type": "public"}]
            })))
            .expect(times)
            .mount(mock)
            .await;
    }

    pub async fn instances(mock: &MockServer, list: Value) {
        get(mock, "/1.0/cloud/project/proj-1/instance", list).await;
    }

    pub async fn delete(mock: &MockServer, id: &str, times: u64) {
        Mock::given(method("DELETE"))
            .and(path(format!("/1.0/cloud/project/proj-1/instance/{id}")))
            .respond_with(ResponseTemplate::new(200))
            .expect(times)
            .mount(mock)
            .await;
    }

    pub fn cfg() -> WitnessCfg {
        WitnessCfg {
            regions: None,
            ptr_fqdn: Some("box.example.org".into()),
            poll_interval: Duration::ZERO,
            poll_limit: 3,
        }
    }

    pub fn ovh(mock: &MockServer) -> Ovh {
        Ovh::with_base_url("k".into(), "s".into(), "c".into(), mock.uri())
    }
}

#[tokio::test]
async fn witness_is_green_against_the_fake_far_end_and_deletes_its_box() {
    let mock = wiremock::MockServer::start().await;
    fake::far_end(&mock, OVH_UBUNTU_IMAGE_NAME).await;
    fake::create(&mock, 1).await;
    fake::instances(&mock, serde_json::json!([])).await;
    fake::delete(&mock, "inst-1", 1).await;

    let miss = run_witness(&fake::ovh(&mock), &reqwest::Client::new(), &fake::cfg()).await;

    assert_no_mismatch(&miss);
}

#[tokio::test]
async fn witness_sweeps_only_its_own_leftovers() {
    let mock = wiremock::MockServer::start().await;
    fake::far_end(&mock, OVH_UBUNTU_IMAGE_NAME).await;
    fake::create(&mock, 1).await;
    fake::instances(
        &mock,
        serde_json::json!([
            {"id": "inst-1", "name": format!("{NAME_PREFIX}9"), "ipAddresses": []},
            {"id": "old-1", "name": format!("{NAME_PREFIX}1"), "ipAddresses": []},
            {"id": "users-box", "name": "nest", "ipAddresses": []},
        ]),
    )
    .await;
    fake::delete(&mock, "inst-1", 1).await;
    fake::delete(&mock, "old-1", 1).await;
    fake::delete(&mock, "users-box", 0).await;

    let miss = run_witness(&fake::ovh(&mock), &reqwest::Client::new(), &fake::cfg()).await;

    assert_no_mismatch(&miss);
}

#[tokio::test]
async fn witness_reports_a_missing_image_and_creates_nothing() {
    let mock = wiremock::MockServer::start().await;
    fake::far_end(&mock, "Ubuntu 26.04 LTS").await;
    fake::create(&mock, 0).await;

    let miss = run_witness(&fake::ovh(&mock), &reqwest::Client::new(), &fake::cfg()).await;

    assert!(
        miss.iter().any(|m| m.starts_with("image name: pinned")),
        "the missing image is a recorded mismatch: {miss:?}"
    );
}
