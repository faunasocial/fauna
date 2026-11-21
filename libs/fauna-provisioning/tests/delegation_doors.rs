//! The two delegation doors — create a zone at a DNS provider, set a domain's
//! nameservers at a registrar — pinned against wiremock stand-ins, plus the
//! bijection that holds each provider's declaration in `i18n/providers.yaml`
//! (`dns_creates_zones`, `registrar_sets_nameservers`) to the adapter.
//!
//! Owner: `docs/goal/architecture/provisioning/registry.md` § Delegation doors.
//! The request shapes are read off each provider's published API reference;
//! the ones that section lists as not yet witnessed by a live credential are
//! pinned here as written, so a live witness that finds a different shape
//! changes this file and the goal doc's table together.

use fauna_provisioning::dispatch::{Credentials, dns_provider, parse_credentials, registrar};
use fauna_provisioning::dns::DnsProvider;
use fauna_provisioning::dns::cloudflare::Cloudflare;
use fauna_provisioning::dns::hetzner::HetznerDns;
use fauna_provisioning::error::ProvisionError;
use fauna_provisioning::registrar::Registrar;
use fauna_provisioning::registrar::gandi::GandiRegistrar;
use fauna_provisioning::registrar::namecheap::NamecheapRegistrar;
use fauna_provisioning::registrar::porkbun::PorkbunRegistrar;
use fauna_provisioning::registrar::spaceship::SpaceshipRegistrar;
use fauna_provisioning::{Capability, PROVIDERS, ProviderId};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ns() -> Vec<String> {
    vec!["ns1.dns.example".to_string(), "ns2.dns.example".to_string()]
}

// ---------------------------------------------------------------------------
// The zone door — Hetzner
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hetzner_create_zone_posts_a_primary_zone_and_reads_the_assigned_nameservers() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/zones"))
        .and(header("authorization", "Bearer tok"))
        .and(body_json(json!({"name": "example.net", "mode": "primary"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "zone": {
                "id": 4711,
                "name": "example.net",
                "mode": "primary",
                "authoritative_nameservers": {
                    "assigned": ["hydrogen.ns.hetzner.example.", "oxygen.ns.hetzner.example.", "helium.ns.hetzner.example."],
                    "delegated": [],
                    "delegation_status": "unregistered"
                }
            },
            "action": {"id": 1, "command": "create_zone", "status": "running"}
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let dns = HetznerDns::with_base_url("tok".into(), mock.uri());
    let zone = dns
        .create_zone(&reqwest::Client::new(), "example.net")
        .await
        .expect("zone created");

    assert_eq!(zone.id, "4711");
    assert_eq!(zone.name, "example.net");
    assert_eq!(
        zone.nameservers,
        vec![
            "hydrogen.ns.hetzner.example.".to_string(),
            "oxygen.ns.hetzner.example.".to_string(),
            "helium.ns.hetzner.example.".to_string()
        ]
    );
}

#[tokio::test]
async fn hetzner_create_zone_surfaces_the_providers_refusal() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/zones"))
        .respond_with(ResponseTemplate::new(409).set_body_string(
            r#"{"error":{"code":"uniqueness_error","message":"zone already exists"}}"#,
        ))
        .mount(&mock)
        .await;

    let dns = HetznerDns::with_base_url("tok".into(), mock.uri());
    let err = dns
        .create_zone(&reqwest::Client::new(), "example.net")
        .await
        .expect_err("a refused create is an error");
    match err {
        ProvisionError::Provider { status, body } => {
            assert_eq!(status, 409);
            assert!(body.contains("uniqueness_error"), "{body}");
        }
        other => panic!("expected Provider error, got {other:?}"),
    }
}

#[tokio::test]
async fn hetzner_verify_fills_each_zones_assigned_nameservers() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/zones"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "zones": [
                {"id": 1, "name": "a.example", "authoritative_nameservers": {"assigned": ["ns1.h.example."]}},
                {"id": 2, "name": "b.example"}
            ]
        })))
        .mount(&mock)
        .await;

    let zones = HetznerDns::with_base_url("tok".into(), mock.uri())
        .verify(&reqwest::Client::new())
        .await
        .expect("listed");
    assert_eq!(zones[0].nameservers, vec!["ns1.h.example.".to_string()]);
    assert!(zones[1].nameservers.is_empty());
}

// ---------------------------------------------------------------------------
// The zone door — Cloudflare
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cloudflare_create_zone_names_the_account_and_reads_name_servers() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/zones"))
        .and(header("authorization", "Bearer tok"))
        .and(body_json(json!({
            "name": "example.net",
            "account": {"id": "acct-1"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "errors": [],
            "result": {
                "id": "zone-9",
                "name": "example.net",
                "status": "pending",
                "name_servers": ["ada.ns.cloudflare.example", "bob.ns.cloudflare.example"]
            }
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let dns = Cloudflare::with_base_url("tok".into(), mock.uri()).with_account_id("acct-1".into());
    let zone = dns
        .create_zone(&reqwest::Client::new(), "example.net")
        .await
        .expect("zone created");
    assert_eq!(zone.id, "zone-9");
    assert_eq!(zone.name, "example.net");
    assert_eq!(
        zone.nameservers,
        vec![
            "ada.ns.cloudflare.example".to_string(),
            "bob.ns.cloudflare.example".to_string()
        ]
    );
}

#[tokio::test]
async fn cloudflare_create_zone_surfaces_success_false() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/zones"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": false,
            "errors": [{"code": 1061, "message": "example.net already exists"}],
            "result": null
        })))
        .mount(&mock)
        .await;

    let dns = Cloudflare::with_base_url("tok".into(), mock.uri()).with_account_id("acct-1".into());
    let err = dns
        .create_zone(&reqwest::Client::new(), "example.net")
        .await
        .expect_err("success:false is an error");
    assert!(err.to_string().contains("already exists"), "{err}");
}

#[tokio::test]
async fn cloudflare_create_zone_without_an_account_id_fails_before_the_network() {
    let mock = MockServer::start().await;
    let dns = Cloudflare::with_base_url("tok".into(), mock.uri());
    let err = dns
        .create_zone(&reqwest::Client::new(), "example.net")
        .await
        .expect_err("no account id, no create");
    assert!(err.to_string().contains("account-id"), "{err}");
    assert!(mock.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn cloudflare_verify_fills_each_zones_name_servers() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/zones"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "errors": [],
            "result": [
                {"id": "z1", "name": "a.example", "name_servers": ["ada.ns.cloudflare.example"]},
                {"id": "z2", "name": "b.example"}
            ]
        })))
        .mount(&mock)
        .await;

    let zones = Cloudflare::with_base_url("tok".into(), mock.uri())
        .verify(&reqwest::Client::new())
        .await
        .expect("listed");
    assert_eq!(
        zones[0].nameservers,
        vec!["ada.ns.cloudflare.example".to_string()]
    );
    assert!(zones[1].nameservers.is_empty());
}

// ---------------------------------------------------------------------------
// The nameserver door — Porkbun
// ---------------------------------------------------------------------------

#[tokio::test]
async fn porkbun_set_nameservers_posts_update_ns_with_the_list() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/domain/updateNs/example.net"))
        .and(body_json(json!({
            "apikey": "k",
            "secretapikey": "s",
            "ns": ["ns1.dns.example", "ns2.dns.example"]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "SUCCESS"})))
        .expect(1)
        .mount(&mock)
        .await;

    PorkbunRegistrar::with_base_url("k".into(), "s".into(), mock.uri())
        .set_nameservers(&reqwest::Client::new(), "example.net", &ns())
        .await
        .expect("nameservers set");
}

#[tokio::test]
async fn porkbun_set_nameservers_surfaces_status_error() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/domain/updateNs/example.net"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "status": "ERROR",
            "message": "Domain is not opted in to API access."
        })))
        .mount(&mock)
        .await;

    let err = PorkbunRegistrar::with_base_url("k".into(), "s".into(), mock.uri())
        .set_nameservers(&reqwest::Client::new(), "example.net", &ns())
        .await
        .expect_err("ERROR status is an error");
    assert!(err.to_string().contains("not opted in"), "{err}");
}

// ---------------------------------------------------------------------------
// The nameserver door — Gandi
// ---------------------------------------------------------------------------

#[tokio::test]
async fn gandi_set_nameservers_puts_the_list() {
    let mock = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/domain/domains/example.net/nameservers"))
        .and(header("authorization", "Bearer pat"))
        .and(body_json(
            json!({"nameservers": ["ns1.dns.example", "ns2.dns.example"]}),
        ))
        .respond_with(
            ResponseTemplate::new(202).set_body_json(json!({"message": "Nameservers updated"})),
        )
        .expect(1)
        .mount(&mock)
        .await;

    GandiRegistrar::with_base_url("pat".into(), mock.uri())
        .set_nameservers(&reqwest::Client::new(), "example.net", &ns())
        .await
        .expect("nameservers set");
}

#[tokio::test]
async fn gandi_set_nameservers_surfaces_the_providers_refusal() {
    let mock = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/domain/domains/example.net/nameservers"))
        .respond_with(ResponseTemplate::new(403).set_body_string(
            r#"{"code":403,"message":"Access was denied to this resource.","object":"HTTPForbidden","cause":"Forbidden"}"#,
        ))
        .mount(&mock)
        .await;

    let err = GandiRegistrar::with_base_url("pat".into(), mock.uri())
        .set_nameservers(&reqwest::Client::new(), "example.net", &ns())
        .await
        .expect_err("403 is an error");
    assert!(
        matches!(err, ProvisionError::Provider { status: 403, .. }),
        "{err:?}"
    );
}

// ---------------------------------------------------------------------------
// The nameserver door — Namecheap
// ---------------------------------------------------------------------------

#[tokio::test]
async fn namecheap_set_nameservers_calls_set_custom_with_sld_tld_and_a_comma_list() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.dns.setCustom"))
        .and(query_param("SLD", "example"))
        .and(query_param("TLD", "net"))
        .and(query_param("Nameservers", "ns1.dns.example,ns2.dns.example"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<?xml version="1.0" encoding="utf-8"?>
<ApiResponse Status="OK" xmlns="http://api.namecheap.com/xml.response">
  <CommandResponse Type="namecheap.domains.dns.setCustom"><DomainDNSSetCustomResult Domain="example.net" Update="true"/></CommandResponse>
</ApiResponse>"#,
        ))
        .expect(1)
        .mount(&mock)
        .await;

    NamecheapRegistrar::with_base_url("user".into(), "key".into(), mock.uri())
        .set_nameservers(&reqwest::Client::new(), "example.net", &ns())
        .await
        .expect("nameservers set");
}

/// Namecheap answers an API-level failure with HTTP 200 and `Status="ERROR"`:
/// trusting the transport status would report a delegation that never happened.
#[tokio::test]
async fn namecheap_set_nameservers_reads_an_http_200_error_envelope_as_a_failure() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.dns.setCustom"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<?xml version="1.0" encoding="utf-8"?>
<ApiResponse Status="ERROR" xmlns="http://api.namecheap.com/xml.response">
  <Errors><Error Number="2019166">Domain not found</Error></Errors>
</ApiResponse>"#,
        ))
        .mount(&mock)
        .await;

    let err = NamecheapRegistrar::with_base_url("user".into(), "key".into(), mock.uri())
        .set_nameservers(&reqwest::Client::new(), "example.net", &ns())
        .await
        .expect_err("a Status=ERROR envelope is an error");
    assert!(err.to_string().contains("Domain not found"), "{err}");
}

/// A refused update reported as `Update="false"` inside an OK envelope is a
/// failure too (the published example spells the attribute `Update`; the
/// adapter reads `Updated` as well until a live witness settles it).
#[tokio::test]
async fn namecheap_set_nameservers_reads_update_false_as_a_failure() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.dns.setCustom"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<?xml version="1.0" encoding="utf-8"?>
<ApiResponse Status="OK" xmlns="http://api.namecheap.com/xml.response">
  <CommandResponse Type="namecheap.domains.dns.setCustom"><DomainDNSSetCustomResult Domain="example.net" Update="false"/></CommandResponse>
</ApiResponse>"#,
        ))
        .mount(&mock)
        .await;

    let err = NamecheapRegistrar::with_base_url("user".into(), "key".into(), mock.uri())
        .set_nameservers(&reqwest::Client::new(), "example.net", &ns())
        .await
        .expect_err("Update=false is an error");
    assert!(err.to_string().contains("example.net"), "{err}");
}

// ---------------------------------------------------------------------------
// Declaration ⇔ adapter
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The nameserver door — Spaceship
// ---------------------------------------------------------------------------

#[tokio::test]
async fn spaceship_set_nameservers_puts_a_custom_host_list() {
    let mock = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/domains/example.net/nameservers"))
        .and(header("x-api-key", "k"))
        .and(header("x-api-secret", "s"))
        .and(body_json(json!({
            "provider": "custom",
            "hosts": ["ns1.dns.example", "ns2.dns.example"]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "provider": "custom",
            "hosts": ["ns1.dns.example", "ns2.dns.example"]
        })))
        .expect(1)
        .mount(&mock)
        .await;

    // A root dot on a nameserver is dropped: Spaceship's host pattern refuses it.
    let with_dots: Vec<String> = ns().into_iter().map(|h| format!("{h}.")).collect();
    SpaceshipRegistrar::with_base_url("k".into(), "s".into(), mock.uri())
        .set_nameservers(&reqwest::Client::new(), "example.net", &with_dots)
        .await
        .expect("nameservers set");
}

#[tokio::test]
async fn spaceship_set_nameservers_surfaces_the_providers_refusal() {
    let mock = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/domains/example.net/nameservers"))
        .respond_with(
            ResponseTemplate::new(422)
                .set_body_string(r#"{"detail":"Nameservers are invalid","data":[]}"#),
        )
        .mount(&mock)
        .await;

    let err = SpaceshipRegistrar::with_base_url("k".into(), "s".into(), mock.uri())
        .set_nameservers(&reqwest::Client::new(), "example.net", &ns())
        .await
        .expect_err("a refused update is an error");
    match err {
        ProvisionError::Provider { status, body } => {
            assert_eq!(status, 422);
            assert!(body.contains("Nameservers are invalid"), "{body}");
        }
        other => panic!("expected Provider error, got {other:?}"),
    }
}

/// Spaceship takes 2 to 12 hosts; a list outside that is refused before any
/// request, with a message that says so.
#[tokio::test]
async fn spaceship_set_nameservers_refuses_a_list_outside_two_to_twelve() {
    let mock = MockServer::start().await;
    let reg = SpaceshipRegistrar::with_base_url("k".into(), "s".into(), mock.uri());
    let thirteen: Vec<String> = (1..=13).map(|i| format!("ns{i}.dns.example")).collect();
    for list in [vec!["ns1.dns.example".to_string()], thirteen] {
        let err = reg
            .set_nameservers(&reqwest::Client::new(), "example.net", &list)
            .await
            .expect_err("refused");
        assert!(err.to_string().contains("between 2 and 12"), "{err}");
    }
    assert!(mock.received_requests().await.unwrap().is_empty());
}

/// A credentials bag holding every field the registry lists for `id`, plus the
/// `api-base` test override pointing a registrar at `base`. Field ids come
/// from the registry so a renamed id shows up here as a refused constructor.
fn creds_for(id: ProviderId, base: &str) -> Credentials {
    let meta = PROVIDERS.iter().find(|p| p.id == id).expect("registry row");
    let mut pairs: Vec<String> = meta
        .fields
        .iter()
        .map(|f| {
            let v = if f.id == "base-url" {
                base.to_string()
            } else {
                format!("test-{}", f.id)
            };
            format!("\"{}\":\"{}\"", f.id, v)
        })
        .collect();
    pairs.push(format!("\"api-base\":\"{base}\""));
    parse_credentials(&format!("{{{}}}", pairs.join(","))).expect("valid JSON")
}

fn is_unsupported(e: &ProvisionError) -> bool {
    matches!(e, ProvisionError::Unsupported { .. })
}

/// Every provider's zone door: declared → the adapter goes to the network
/// (answered here with a 418, so any error but `Unsupported`); undeclared →
/// `Unsupported` with no request sent.
#[tokio::test]
async fn the_zone_door_is_implemented_exactly_where_the_registry_declares_it() {
    let mut failures = Vec::new();
    for meta in PROVIDERS {
        if !meta.capabilities.contains(&Capability::Dns) {
            assert!(
                !meta.dns_creates_zones,
                "{}: a zone door without the dns capability must be refused by the generator",
                meta.id.as_str()
            );
            continue;
        }
        let mock = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(418))
            .mount(&mock)
            .await;
        let dns = dns_provider(meta.id, creds_for(meta.id, &mock.uri()), Some(mock.uri()))
            .unwrap_or_else(|| panic!("{}: dns dispatch from full creds", meta.id.as_str()));
        let result = dns
            .create_zone(&reqwest::Client::new(), "example.net")
            .await;
        let sent = mock.received_requests().await.unwrap().len();
        match (meta.dns_creates_zones, result) {
            (true, Err(e)) if !is_unsupported(&e) && sent > 0 => {}
            (false, Err(e)) if is_unsupported(&e) && sent == 0 => {}
            (declared, got) => failures.push(format!(
                "{}: dns_creates_zones={declared}, create_zone answered {got:?} after {sent} request(s)",
                meta.id.as_str()
            )),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn the_nameserver_door_is_implemented_exactly_where_the_registry_declares_it() {
    let mut failures = Vec::new();
    for meta in PROVIDERS {
        if !meta.capabilities.contains(&Capability::Registrar) {
            assert!(
                !meta.registrar_sets_nameservers,
                "{}: a nameserver door without the registrar capability must be refused by the generator",
                meta.id.as_str()
            );
            continue;
        }
        let mock = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(418))
            .mount(&mock)
            .await;
        let reg = registrar(meta.id, creds_for(meta.id, &mock.uri()))
            .unwrap_or_else(|| panic!("{}: registrar dispatch from full creds", meta.id.as_str()));
        let result = reg
            .set_nameservers(&reqwest::Client::new(), "example.net", &ns())
            .await;
        let sent = mock.received_requests().await.unwrap().len();
        match (meta.registrar_sets_nameservers, result) {
            (true, Err(e)) if !is_unsupported(&e) && sent > 0 => {}
            (false, Err(e)) if is_unsupported(&e) && sent == 0 => {}
            (declared, got) => failures.push(format!(
                "{}: registrar_sets_nameservers={declared}, set_nameservers answered {got:?} after {sent} request(s)",
                meta.id.as_str()
            )),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The doors the goal doc's table declares, so a registry edit that drops or
/// adds one is a deliberate change to both.
#[test]
fn the_registry_declares_the_doors_the_goal_doc_names() {
    let zone: Vec<&str> = PROVIDERS
        .iter()
        .filter(|p| p.dns_creates_zones)
        .map(|p| p.id.as_str())
        .collect();
    let ns: Vec<&str> = PROVIDERS
        .iter()
        .filter(|p| p.registrar_sets_nameservers)
        .map(|p| p.id.as_str())
        .collect();
    let mut zone_sorted = zone.clone();
    zone_sorted.sort();
    let mut ns_sorted = ns.clone();
    ns_sorted.sort();
    assert_eq!(zone_sorted, vec!["cloudflare", "hetzner"]);
    assert_eq!(
        ns_sorted,
        vec!["gandi", "namecheap", "porkbun", "spaceship"]
    );
}

/// Cloudflare's `account-id` is needed by both the registrar and the zone
/// create, so it carries both kinds.
#[test]
fn cloudflare_account_id_is_a_registrar_and_zone_field() {
    let cf = PROVIDERS
        .iter()
        .find(|p| p.id == ProviderId::Cloudflare)
        .unwrap();
    let f = cf.fields.iter().find(|f| f.id == "account-id").unwrap();
    assert_eq!(f.kinds, &[Capability::Registrar, Capability::Zone]);
}
