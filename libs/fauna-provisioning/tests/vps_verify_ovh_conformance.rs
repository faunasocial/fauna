//! OVH `verify` conformance — a location is `<project>/<region>`
//! (`docs/goal/architecture/provisioning/registry.md` § Server offers follow
//! the location → *OVH: a location is a project and a region*): every `UP`
//! region of type `region` in every project the credential sees, named
//! `<datacenterLocation> (<region>)`, prefixed by the project's description
//! only when more than one project is seen.

use fauna_provisioning::vps::VpsProvider;
use fauna_provisioning::vps::ovh::Ovh;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn serve(mock: &MockServer, at: &str, body: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(mock)
        .await;
}

fn region(name: &str, kind: &str, status: &str, city: &str, country: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name, "type": kind, "status": status,
        "datacenterLocation": city, "countryCode": country
    })
}

async fn mount_account(mock: &MockServer, projects: &[&str]) {
    serve(mock, "/1.0/auth/time", serde_json::json!(1_700_000_000)).await;
    serve(mock, "/1.0/me", serde_json::json!({"ovhSubsidiary": "FR"})).await;
    serve(mock, "/1.0/cloud/project", serde_json::json!(projects)).await;
}

fn ovh(mock: &MockServer) -> Ovh {
    Ovh::with_base_url("k".into(), "s".into(), "c".into(), mock.uri())
}

#[tokio::test]
async fn ovh_verify_lists_up_regions_of_one_project_unprefixed() {
    let mock = MockServer::start().await;
    mount_account(&mock, &["proj-1"]).await;
    serve(
        &mock,
        "/1.0/cloud/project/proj-1/region",
        serde_json::json!(["GRA11", "BRU1LZ", "SBG5", "PAR3AZ"]),
    )
    .await;
    let r = "/1.0/cloud/project/proj-1/region";
    serve(
        &mock,
        &format!("{r}/GRA11"),
        region("GRA11", "region", "UP", "Gravelines", "fr"),
    )
    .await;
    serve(
        &mock,
        &format!("{r}/BRU1LZ"),
        region("BRU1LZ", "localzone", "UP", "Brussels", "be"),
    )
    .await;
    serve(
        &mock,
        &format!("{r}/SBG5"),
        region("SBG5", "region", "DOWN", "Strasbourg", "fr"),
    )
    .await;
    serve(
        &mock,
        &format!("{r}/PAR3AZ"),
        region("PAR3AZ", "region-3-az", "UP", "Paris", "fr"),
    )
    .await;

    let locations = ovh(&mock).verify(&reqwest::Client::new()).await.unwrap();

    assert_eq!(
        locations.len(),
        1,
        "localzone, region-3-az and DOWN are excluded"
    );
    assert_eq!(locations[0].id, "proj-1/GRA11");
    assert_eq!(locations[0].name, "Gravelines (GRA11)");
    assert_eq!(locations[0].city, "Gravelines");
    assert_eq!(locations[0].country, "fr");
}

#[tokio::test]
async fn ovh_verify_prefixes_the_project_when_several_are_seen() {
    let mock = MockServer::start().await;
    mount_account(&mock, &["proj-1", "proj-2"]).await;
    serve(
        &mock,
        "/1.0/cloud/project/proj-1",
        serde_json::json!({"description": "Home"}),
    )
    .await;
    serve(
        &mock,
        "/1.0/cloud/project/proj-2",
        serde_json::json!({"description": "Work"}),
    )
    .await;
    serve(
        &mock,
        "/1.0/cloud/project/proj-1/region",
        serde_json::json!(["GRA11"]),
    )
    .await;
    serve(
        &mock,
        "/1.0/cloud/project/proj-2/region",
        serde_json::json!(["BHS5"]),
    )
    .await;
    serve(
        &mock,
        "/1.0/cloud/project/proj-1/region/GRA11",
        region("GRA11", "region", "UP", "Gravelines", "fr"),
    )
    .await;
    serve(
        &mock,
        "/1.0/cloud/project/proj-2/region/BHS5",
        region("BHS5", "region", "UP", "Beauharnois", "ca"),
    )
    .await;

    let locations = ovh(&mock).verify(&reqwest::Client::new()).await.unwrap();

    let got: Vec<(&str, &str)> = locations
        .iter()
        .map(|l| (l.id.as_str(), l.name.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("proj-1/GRA11", "Home · Gravelines (GRA11)"),
            ("proj-2/BHS5", "Work · Beauharnois (BHS5)"),
        ]
    );
}

#[tokio::test]
async fn ovh_verify_rejects_a_key_that_cannot_read_the_account() {
    let mock = MockServer::start().await;
    serve(&mock, "/1.0/auth/time", serde_json::json!(1_700_000_000)).await;
    Mock::given(method("GET"))
        .and(path("/1.0/me"))
        .respond_with(ResponseTemplate::new(403).set_body_string(r#"{"message":"denied"}"#))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/1.0/cloud/project"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(["proj-1"])))
        .expect(0)
        .mount(&mock)
        .await;

    assert!(ovh(&mock).verify(&reqwest::Client::new()).await.is_err());
}
