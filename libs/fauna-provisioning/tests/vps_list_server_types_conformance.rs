//! `list_server_types` conformance — the offers follow the chosen location
//! (`docs/goal/architecture/provisioning/registry.md` § Server offers follow
//! the location). Each adapter answers the curated offers as the given
//! location sells them: Hetzner prices that location's `prices` entry,
//! DigitalOcean and Vultr drop what the region does not sell, Linode's types
//! are account-wide, and OVH joins the region's flavors to the public
//! catalogue's monthly price.

use fauna_provisioning::vps::VpsProvider;
use fauna_provisioning::vps::digitalocean::DigitalOcean;
use fauna_provisioning::vps::hetzner::Hetzner;
use fauna_provisioning::vps::linode::Linode;
use fauna_provisioning::vps::ovh::Ovh;
use fauna_provisioning::vps::vultr::Vultr;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn serve(mock: &MockServer, at: &str, body: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(mock)
        .await;
}

fn ids(types: &[fauna_provisioning::vps::ServerTypeInfo]) -> Vec<&str> {
    types.iter().map(|t| t.id.as_str()).collect()
}

// --------------------------------------------------------------------------
// Hetzner
// --------------------------------------------------------------------------

fn hetzner_types() -> serde_json::Value {
    serde_json::json!({"server_types": [{
        "name": "cx23", "cores": 2, "memory": 4.0, "disk": 40,
        "prices": [
            {"location": "fsn1", "price_monthly": {"gross": "4.51"}},
            {"location": "hel1", "price_monthly": {"gross": "5.17"}}
        ]
    }]})
}

#[tokio::test]
async fn hetzner_prices_the_chosen_location() {
    let mock = MockServer::start().await;
    serve(&mock, "/server_types", hetzner_types()).await;
    let h = Hetzner::with_base_url("t".into(), mock.uri());
    let client = reqwest::Client::new();

    let fsn = h
        .list_server_types(&client, &["cx23"], "fsn1")
        .await
        .unwrap();
    let hel = h
        .list_server_types(&client, &["cx23"], "hel1")
        .await
        .unwrap();

    assert_eq!(fsn[0].price_monthly_cents, 451);
    assert_eq!(hel[0].price_monthly_cents, 517);
}

#[tokio::test]
async fn hetzner_without_an_entry_for_the_location_falls_back_to_the_first() {
    let mock = MockServer::start().await;
    serve(&mock, "/server_types", hetzner_types()).await;
    let h = Hetzner::with_base_url("t".into(), mock.uri());

    let types = h
        .list_server_types(&reqwest::Client::new(), &["cx23"], "ash")
        .await
        .unwrap();

    assert_eq!(types[0].price_monthly_cents, 451);
}

// --------------------------------------------------------------------------
// DigitalOcean
// --------------------------------------------------------------------------

#[tokio::test]
async fn digitalocean_drops_a_size_the_region_does_not_sell() {
    let mock = MockServer::start().await;
    serve(
        &mock,
        "/sizes",
        serde_json::json!({"sizes": [
            {"slug": "s-1vcpu-2gb", "vcpus": 1, "memory": 2048, "disk": 50,
             "price_monthly": 12.0, "available": true, "regions": ["nyc3", "ams3"]},
            {"slug": "s-2vcpu-4gb", "vcpus": 2, "memory": 4096, "disk": 80,
             "price_monthly": 24.0, "available": true, "regions": ["nyc3"]}
        ]}),
    )
    .await;
    let d = DigitalOcean::with_base_url("t".into(), mock.uri());
    let curated = ["s-1vcpu-2gb", "s-2vcpu-4gb"];
    let client = reqwest::Client::new();

    let nyc = d
        .list_server_types(&client, &curated, "nyc3")
        .await
        .unwrap();
    let ams = d
        .list_server_types(&client, &curated, "ams3")
        .await
        .unwrap();

    assert_eq!(ids(&nyc), ["s-1vcpu-2gb", "s-2vcpu-4gb"]);
    assert_eq!(ids(&ams), ["s-1vcpu-2gb"]);
}

// --------------------------------------------------------------------------
// Vultr
// --------------------------------------------------------------------------

#[tokio::test]
async fn vultr_drops_a_plan_the_location_does_not_sell() {
    let mock = MockServer::start().await;
    serve(
        &mock,
        "/plans",
        serde_json::json!({"plans": [
            {"id": "vc2-1c-2gb", "vcpu_count": 1, "ram": 2048, "disk": 55,
             "monthly_cost": 10.0, "locations": ["ewr", "fra"]},
            {"id": "vc2-2c-4gb", "vcpu_count": 2, "ram": 4096, "disk": 80,
             "monthly_cost": 20.0, "locations": ["ewr"]}
        ]}),
    )
    .await;
    let v = Vultr::with_base_url("t".into(), mock.uri());
    let curated = ["vc2-1c-2gb", "vc2-2c-4gb"];
    let client = reqwest::Client::new();

    let ewr = v.list_server_types(&client, &curated, "ewr").await.unwrap();
    let fra = v.list_server_types(&client, &curated, "fra").await.unwrap();

    assert_eq!(ids(&ewr), ["vc2-1c-2gb", "vc2-2c-4gb"]);
    assert_eq!(ids(&fra), ["vc2-1c-2gb"]);
}

// --------------------------------------------------------------------------
// Linode — types are account-wide, the location changes nothing
// --------------------------------------------------------------------------

#[tokio::test]
async fn linode_types_do_not_depend_on_the_location() {
    let mock = MockServer::start().await;
    serve(
        &mock,
        "/linode/types",
        serde_json::json!({"data": [
            {"id": "g6-standard-1", "vcpus": 1, "memory": 2048, "disk": 51200,
             "price": {"monthly": 12.0}}
        ]}),
    )
    .await;
    let l = Linode::with_base_url("t".into(), mock.uri());
    let client = reqwest::Client::new();

    let a = l
        .list_server_types(&client, &["g6-standard-1"], "us-east")
        .await
        .unwrap();
    let b = l
        .list_server_types(&client, &["g6-standard-1"], "eu-central")
        .await
        .unwrap();

    assert_eq!(ids(&a), ids(&b));
    assert_eq!(a[0].price_monthly_cents, b[0].price_monthly_cents);
}

// --------------------------------------------------------------------------
// OVH — the region's flavors joined to the public catalogue
// --------------------------------------------------------------------------

#[tokio::test]
async fn ovh_joins_the_region_flavors_to_the_catalogue_price() {
    let mock = MockServer::start().await;
    serve(&mock, "/1.0/auth/time", serde_json::json!(1_700_000_000)).await;
    serve(&mock, "/1.0/me", serde_json::json!({"ovhSubsidiary": "IE"})).await;
    Mock::given(method("GET"))
        .and(path("/1.0/cloud/project/proj-1/flavor"))
        .and(query_param("region", "GRA11"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            // The windows twin of d2-2 is never the offer.
            {"id": "f-win", "name": "d2-2", "osType": "windows", "available": true,
             "vcpus": 1, "ram": 2, "disk": 25, "planCodes": {"monthly": "d2-2.win.monthly"}},
            {"id": "f-lin", "name": "d2-2", "osType": "linux", "available": true,
             "vcpus": 1, "ram": 2, "disk": 25, "planCodes": {"monthly": "d2-2.monthly"}},
            // In the region, but with no monthly plan — not offered.
            {"id": "f-b27", "name": "b2-7", "osType": "linux", "available": true,
             "vcpus": 2, "ram": 7, "disk": 50, "planCodes": {"monthly": null}},
            // Monthly plan, but the catalogue does not carry it — not offered.
            {"id": "f-d28", "name": "d2-8", "osType": "linux", "available": true,
             "vcpus": 4, "ram": 8, "disk": 50, "planCodes": {"monthly": "d2-8.monthly"}}
            // d2-4 is absent from the region — not offered.
        ])))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/1.0/order/catalog/public/cloud"))
        .and(query_param("ovhSubsidiary", "IE"))
        .and(header_exists_not("x-ovh-signature"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "locale": {"currencyCode": "EUR", "subsidiary": "IE"},
            "plans": [],
            "addons": [
                {"planCode": "d2-2.win.monthly", "pricings": [
                    {"interval": 1, "intervalUnit": "month", "price": 999000000, "tax": 0}]},
                {"planCode": "d2-2.monthly", "pricings": [
                    {"interval": 1, "intervalUnit": "month", "price": 571000000, "tax": 131330000}]}
            ]
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let ovh = Ovh::with_base_url("k".into(), "s".into(), "c".into(), mock.uri());
    let types = ovh
        .list_server_types(
            &reqwest::Client::new(),
            &["d2-2", "d2-4", "d2-8", "b2-7"],
            "proj-1/GRA11",
        )
        .await
        .unwrap();

    assert_eq!(ids(&types), ["d2-2"]);
    let d22 = &types[0];
    assert_eq!((d22.vcpu, d22.mem_gb, d22.disk_gb), (1, 2.0, 25));
    assert_eq!(
        d22.price_monthly_cents, 702,
        "gross: price + tax, micro-cents → cents"
    );
    assert_eq!(d22.currency, "EUR");
}

#[tokio::test]
async fn ovh_location_without_a_project_is_an_error() {
    let ovh = Ovh::with_base_url(
        "k".into(),
        "s".into(),
        "c".into(),
        "http://127.0.0.1:9".into(),
    );
    assert!(
        ovh.list_server_types(&reqwest::Client::new(), &["d2-2"], "GRA11")
            .await
            .is_err()
    );
}

/// Matches a request that does not carry `name` — the catalogue is public, so
/// its fetch is unsigned.
fn header_exists_not(name: &'static str) -> impl wiremock::Match {
    move |req: &wiremock::Request| !req.headers.contains_key(name)
}
