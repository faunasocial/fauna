//! `NestRetireMachine` unit tests, driven against wiremock through the generic
//! bundled provider — the one provider carrying all three capabilities (VPS,
//! DNS, registrar), so one fake serves the listing, the attribution read, the
//! cleanup and the transfer code without a second adapter in the picture.
//!
//! The properties under test are the ones a wrong answer makes expensive:
//! attribution never guessing, DNS running before the server, a failed DNS
//! step stopping *before* the box is destroyed, and a re-run converging.

use super::*;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BOX_IPV4: &str = "203.0.113.5";

fn me_json(zones: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "api_version": 1,
        "account": { "id": "acct-1" },
        "zones": zones,
        "locations": [{ "id": "eu-1", "name": "Europe 1", "city": "Falkenstein", "country": "DE" }]
    })
}

fn one_zone() -> serde_json::Value {
    serde_json::json!([{ "id": "z-1", "name": "example.test" }])
}

fn server_json(name: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "s-1",
        "name": name,
        "ipv4": BOX_IPV4,
        "status": "running",
        "labels": { "managed-by": "fauna" }
    })
}

/// `GET /v1/me` (zones + locations) and the marker-filtered server list.
async fn mount_account(mock: &MockServer, zones: serde_json::Value, name: &str) {
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(me_json(zones)))
        .mount(mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/servers"))
        .and(query_param("label", "managed-by=fauna"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "servers": [server_json(name)] })),
        )
        .mount(mock)
        .await;
}

async fn mount_ptr(mock: &MockServer, ptr: Option<&str>) {
    Mock::given(method("GET"))
        .and(path("/v1/servers/s-1/ptr"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ptr": ptr })))
        .mount(mock)
        .await;
}

/// The apex `A` the attribution check reads. `value` is what it reports.
async fn mount_apex_a(mock: &MockServer, value: &str) {
    Mock::given(method("GET"))
        .and(path("/v1/zones/z-1/records"))
        .and(query_param("name", "@"))
        .and(query_param("type", "A"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "records": [{ "id": "r-apex", "type": "A", "name": "@", "value": value, "ttl": 300 }]
        })))
        .mount(mock)
        .await;
}

/// Every other `find_records` the cleanup walks — empty, so the plan is a
/// no-op beyond the apex.
async fn mount_empty_records(mock: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/v1/zones/z-1/records"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "records": [] })),
        )
        .mount(mock)
        .await;
}

fn machine(mock: &MockServer, inputs: RetireInputs) -> Arc<NestRetireMachine> {
    let m = NestRetireMachine::new_with_inputs(inputs);
    m.select_provider("bundled".into());
    m.set_credential_field("base-url".into(), mock.uri());
    m.set_credential_field("api-token".into(), "tok".into());
    m
}

// ---------------------------------------------------------------------------
// Attribution (§ Box → domain attribution)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ptr_attributes_the_domain_when_the_apex_a_matches() {
    let mock = MockServer::start().await;
    mount_account(&mock, one_zone(), "example-test").await;
    // Provisioning sets the PTR to `mail.<domain>`; the leading label is
    // stripped and the result verified.
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;

    let snap = m.snapshot();
    assert_eq!(snap.phase, RetirePhase::List);
    assert_eq!(snap.servers.len(), 1);
    assert_eq!(snap.servers[0].domain.as_deref(), Some("example.test"));
}

#[tokio::test]
async fn a_passed_in_candidate_attributes_when_the_ptr_is_absent() {
    let mock = MockServer::start().await;
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, None).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;

    let m = machine(
        &mock,
        RetireInputs {
            candidate_domains: vec!["example.test".into()],
            ..Default::default()
        },
    );
    m.verify().await;

    assert_eq!(
        m.snapshot().servers[0].domain.as_deref(),
        Some("example.test"),
        "candidate (2) attributes once it verifies"
    );
}

#[tokio::test]
async fn a_dashed_name_collision_never_attributes_unverified() {
    let mock = MockServer::start().await;
    // The zone's dashed form equals the server name, so candidate (3) offers
    // it — but the apex `A` points somewhere else entirely, so it must NOT be
    // attributed. This is the whole reason the name alone never attributes:
    // `my-site.example.test` and `my.site.example.test` share one dashed name.
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, None).await;
    mount_apex_a(&mock, "198.51.100.99").await;
    mount_empty_records(&mock).await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;

    let row = &m.snapshot().servers[0];
    assert_eq!(
        row.domain, None,
        "an unverified candidate is never attributed"
    );
    assert_eq!(
        row.transfer_code,
        TransferCodeState::Unsupported,
        "no verified domain means no transfer-code affordance"
    );
}

#[tokio::test]
async fn an_unattributed_box_is_still_deletable_and_cleans_up_no_dns() {
    let mock = MockServer::start().await;
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, None).await;
    mount_apex_a(&mock, "198.51.100.99").await;
    mount_empty_records(&mock).await;
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&mock)
        .await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    m.select("s-1".into());
    m.begin_confirm();
    assert!(
        m.snapshot().dns_plan.is_empty(),
        "no verified domain → nothing is planned for removal"
    );
    m.set_confirm_name("example-test".into());
    m.confirm().await;

    let snap = m.snapshot();
    assert_eq!(snap.phase, RetirePhase::Done);
    assert!(matches!(snap.steps[0].state, StepState::Skipped { .. }));
    assert_eq!(snap.steps[1].state, StepState::Done);
}

// ---------------------------------------------------------------------------
// The public-DNS arm (§ Box → domain attribution: "else public DNS from the
// client") — what verifies a domain when no DNS credential can read its zone
// ---------------------------------------------------------------------------

/// The public resolver's answer for `domain`'s apex `A`. `expect` pins how
/// often the machine may ask.
async fn mount_public_a(
    mock: &MockServer,
    domain: &str,
    value: &str,
    expect: impl Into<wiremock::Times>,
) {
    Mock::given(method("GET"))
        .and(path("/dns-query"))
        .and(query_param("name", domain))
        .and(query_param("type", "A"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Status": 0,
            "Answer": [{ "name": format!("{domain}."), "type": 1, "TTL": 300, "data": value }]
        })))
        .expect(expect)
        .mount(mock)
        .await;
}

fn with_public_dns(mock: &MockServer) -> RetireInputs {
    RetireInputs {
        doh_base_url: Some(mock.uri()),
        ..Default::default()
    }
}

#[tokio::test]
async fn public_dns_attributes_when_no_dns_credential_is_usable() {
    let mock = MockServer::start().await;
    // An account that reports no zones is the VPS-only shape (four of the six
    // providers): the entered token can list and delete boxes, read no DNS.
    mount_account(&mock, serde_json::json!([]), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_public_a(&mock, "example.test", BOX_IPV4, 1..).await;
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&mock)
        .await;

    let m = machine(&mock, with_public_dns(&mock));
    m.verify().await;
    assert_eq!(
        m.snapshot().servers[0].domain.as_deref(),
        Some("example.test"),
        "the public apex A equals the box's IPv4 — verified, so attributed"
    );

    // Attributed but with no credential: nothing can be removed for the
    // person, so the by-hand list is the whole DNS output (§ DNS cleanup).
    m.select("s-1".into());
    m.begin_confirm();
    let snap = m.snapshot();
    assert!(snap.dns_plan.is_empty(), "no credential → no removals");
    assert!(
        !snap.leftover_records.is_empty(),
        "the by-hand list is what an attributed, credential-less row gets"
    );

    // And it is not just the four harmless-stale TXT: the records that point
    // at the address the provider is about to re-issue lead the list, each
    // with the value that says which entry to delete (§ DNS cleanup).
    let apex_at = snap
        .leftover_records
        .iter()
        .position(|l| l.name == "example.test" && l.record_type == "A")
        .expect("the apex A still points at the box being destroyed");
    let apex = &snap.leftover_records[apex_at];
    assert_eq!(apex.value, BOX_IPV4);
    assert_eq!(apex.kind, LeftoverKind::PointsAtBox);

    let first_txt = snap
        .leftover_records
        .iter()
        .position(|l| l.record_type == "TXT")
        .expect("the TXT are still listed");
    assert!(
        apex_at < first_txt,
        "the takeover record must not sit below the harmless ones: {:?}",
        snap.leftover_records
    );
    assert!(
        snap.leftover_records[first_txt..]
            .iter()
            .all(|l| l.kind == LeftoverKind::SharedName),
        "nothing urgent hides under the TXT: {:?}",
        snap.leftover_records
    );

    m.set_confirm_name("example-test".into());
    m.confirm().await;
    let snap = m.snapshot();
    assert_eq!(
        snap.steps[0].state,
        StepState::Skipped {
            why: "no_dns_credential".into()
        }
    );
    assert_eq!(snap.steps[1].state, StepState::Done);
}

#[tokio::test]
async fn public_dns_pointing_elsewhere_never_attributes() {
    let mock = MockServer::start().await;
    mount_account(&mock, serde_json::json!([]), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_public_a(&mock, "example.test", "198.51.100.99", 1..).await;

    let m = machine(&mock, with_public_dns(&mock));
    m.verify().await;
    assert_eq!(
        m.snapshot().servers[0].domain,
        None,
        "the domain resolves to someone else's address — attribution never guesses"
    );
}

#[tokio::test]
async fn an_unreachable_public_resolver_leaves_the_row_unattributed() {
    let mock = MockServer::start().await;
    mount_account(&mock, serde_json::json!([]), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    Mock::given(method("GET"))
        .and(path("/dns-query"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&mock)
        .await;

    let m = machine(&mock, with_public_dns(&mock));
    m.verify().await;
    let snap = m.snapshot();
    assert_eq!(snap.phase, RetirePhase::List, "the list still renders");
    assert_eq!(
        snap.servers[0].domain, None,
        "could not read = not verified"
    );
}

#[tokio::test]
async fn a_domain_outside_every_zone_verifies_publicly_but_plans_no_removals() {
    let mock = MockServer::start().await;
    // The credential is usable — it reports a zone — but not one covering the
    // box's domain. Public DNS verifies the attribution; the DNS step cannot
    // touch a zone the credential does not hold, so the confirm must promise
    // nothing: a summary listing removals that then never run is a lie told
    // at the one moment the person is reading carefully.
    mount_account(
        &mock,
        serde_json::json!([{ "id": "z-9", "name": "unrelated.test" }]),
        "example-test",
    )
    .await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_public_a(&mock, "example.test", BOX_IPV4, 1..).await;
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-1"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&mock)
        .await;

    let m = machine(&mock, with_public_dns(&mock));
    m.verify().await;
    assert_eq!(
        m.snapshot().servers[0].domain.as_deref(),
        Some("example.test")
    );

    m.select("s-1".into());
    m.begin_confirm();
    let snap = m.snapshot();
    assert!(
        snap.dns_plan.is_empty(),
        "no covering zone → the summary promises no removals: {:?}",
        snap.dns_plan
    );
    assert!(!snap.leftover_records.is_empty());

    m.set_confirm_name("example-test".into());
    m.confirm().await;
    assert_eq!(
        m.snapshot().steps[0].state,
        StepState::Skipped {
            why: "no_zone_for_domain".into()
        }
    );
}

#[tokio::test]
async fn the_dns_provider_outranks_public_dns_when_it_holds_the_zone() {
    let mock = MockServer::start().await;
    // The provider holding the zone is authoritative and current; a public
    // resolver may still be serving a cached answer. A provider "no" is final.
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, "198.51.100.99").await;
    mount_empty_records(&mock).await;
    mount_public_a(&mock, "example.test", BOX_IPV4, 0).await;

    let m = machine(&mock, with_public_dns(&mock));
    m.verify().await;
    assert_eq!(m.snapshot().servers[0].domain, None);
}

// ---------------------------------------------------------------------------
// A box serving several domains (§ DNS cleanup — several domains): every
// other domain verified to point at the box is planned too, domain by domain,
// through the credential whose zone covers it — else on the by-hand list.
// ---------------------------------------------------------------------------

fn two_zones() -> serde_json::Value {
    serde_json::json!([
        { "id": "z-1", "name": "example.test" },
        { "id": "z-2", "name": "two.test" }
    ])
}

/// The apex `A` of a second zone, with its own record id so a delete of it is
/// distinguishable from the first zone's.
async fn mount_zone_apex_a(mock: &MockServer, zone_id: &str, record_id: &str, value: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/v1/zones/{zone_id}/records")))
        .and(query_param("name", "@"))
        .and(query_param("type", "A"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "records": [{ "id": record_id, "type": "A", "name": "@", "value": value, "ttl": 300 }]
        })))
        .mount(mock)
        .await;
}

async fn mount_zone_empty_records(mock: &MockServer, zone_id: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/v1/zones/{zone_id}/records")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "records": [] })),
        )
        .mount(mock)
        .await;
}

#[tokio::test]
async fn a_second_domain_verified_through_the_credential_is_planned_and_removed() {
    let mock = MockServer::start().await;
    // The credential holds two zones; both apexes point at the box. The PTR
    // names the first as the mail host's domain, so it is the attributed one;
    // the second is not named by anything but its own apex `A` — which is
    // exactly the record the takeover is made of.
    mount_account(&mock, two_zones(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;
    mount_zone_apex_a(&mock, "z-2", "r-apex-2", BOX_IPV4).await;
    mount_zone_empty_records(&mock, "z-2").await;
    Mock::given(method("DELETE"))
        .and(path("/v1/zones/z-1/records/r-apex"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1..)
        .mount(&mock)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/zones/z-2/records/r-apex-2"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1..)
        .mount(&mock)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&mock)
        .await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    let row = m.snapshot().servers[0].clone();
    assert_eq!(
        row.domain.as_deref(),
        Some("example.test"),
        "the PTR's domain stays attributed"
    );
    assert_eq!(
        row.secondary_domains,
        vec!["two.test".to_string()],
        "the other zone's apex points at the box, so it joins the row"
    );

    m.select("s-1".into());
    m.begin_confirm();
    let plan = m.snapshot().dns_plan;
    assert!(
        plan.iter()
            .any(|l| l.name == "two.test" && l.record_type == "A" && l.value == BOX_IPV4),
        "the confirm summary names the second apex A, value-scoped: {plan:?}"
    );
    assert!(
        plan.iter().any(|l| l.name == "two.test"
            && l.record_type == "MX"
            && l.value == "mail.example.test"),
        "and its MX, scoped to the attributed domain's mail host: {plan:?}"
    );
    assert!(
        plan.iter()
            .all(|l| l.record_type != "TLSA" || l.name == "_25._tcp.mail.example.test"),
        "the floor TLSA is planned under the attributed mail host only: {plan:?}"
    );

    m.set_confirm_name("example-test".into());
    m.confirm().await;
    let snap = m.snapshot();
    assert_eq!(
        snap.steps[0].state,
        StepState::Done,
        "DNS ran across both zones"
    );
    assert_eq!(snap.steps[1].state, StepState::Done);
    // The `expect(1..)` on the z-2 delete is verified when the mock drops.
}

#[tokio::test]
async fn a_record_the_person_repointed_is_never_removed_even_at_a_planned_name() {
    let mock = MockServer::start().await;
    // Value-scoping is what makes the whole name set safe on every domain
    // (§ DNS cleanup — several domains): the second zone's apex still points
    // at the box and goes, but its `mail.` `A` was repointed elsewhere — same
    // name, same type as a planned removal, a different value — and must
    // survive the run.
    mount_account(&mock, two_zones(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;
    mount_zone_apex_a(&mock, "z-2", "r-apex-2", BOX_IPV4).await;
    Mock::given(method("GET"))
        .and(path("/v1/zones/z-2/records"))
        .and(query_param("name", "mail"))
        .and(query_param("type", "A"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "records": [{ "id": "r-mail-2", "type": "A", "name": "mail", "value": "198.51.100.99", "ttl": 300 }]
        })))
        .expect(1..)
        .mount(&mock)
        .await;
    mount_zone_empty_records(&mock, "z-2").await;
    Mock::given(method("DELETE"))
        .and(path("/v1/zones/z-2/records/r-apex-2"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1..)
        .mount(&mock)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/zones/z-2/records/r-mail-2"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&mock)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&mock)
        .await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    assert_eq!(
        m.snapshot().servers[0].secondary_domains,
        vec!["two.test".to_string()]
    );
    m.select("s-1".into());
    m.begin_confirm();
    m.set_confirm_name("example-test".into());
    m.confirm().await;
    let snap = m.snapshot();
    assert_eq!(snap.steps[0].state, StepState::Done);
    assert_eq!(snap.steps[1].state, StepState::Done);
    // The `expect(0)` on the repointed record is verified when the mock drops.
}

#[tokio::test]
async fn a_zone_whose_apex_points_elsewhere_never_joins_the_row() {
    let mock = MockServer::start().await;
    // Holding a zone is not pointing at the box: the set is verified
    // value-first, never swept by name. The second zone resolves to someone
    // else's address and must be left alone entirely.
    mount_account(&mock, two_zones(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;
    mount_zone_apex_a(&mock, "z-2", "r-apex-2", "198.51.100.99").await;
    mount_zone_empty_records(&mock, "z-2").await;
    Mock::given(method("DELETE"))
        .and(path("/v1/zones/z-2/records/r-apex-2"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&mock)
        .await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    let row = m.snapshot().servers[0].clone();
    assert_eq!(row.domain.as_deref(), Some("example.test"));
    assert!(
        row.secondary_domains.is_empty(),
        "a zone pointing elsewhere is not the box's: {:?}",
        row.secondary_domains
    );

    m.select("s-1".into());
    m.begin_confirm();
    let snap = m.snapshot();
    assert!(
        snap.dns_plan.iter().all(|l| !l.name.ends_with("two.test")),
        "nothing of the other zone is planned: {:?}",
        snap.dns_plan
    );
    assert!(
        snap.leftover_records
            .iter()
            .all(|l| !l.name.ends_with("two.test")),
        "nor listed by hand: {:?}",
        snap.leftover_records
    );
}

#[tokio::test]
async fn an_app_passed_second_domain_outside_every_zone_lands_on_the_by_hand_list() {
    let mock = MockServer::start().await;
    // The signed-in app knows the nest served `two.test` too, but no usable
    // credential holds that zone: public DNS verifies it points at the box,
    // and since nothing can remove it *for* the person, its apex leads the
    // by-hand list beside the attributed domain's automatic removals.
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;
    mount_public_a(&mock, "two.test", BOX_IPV4, 1..).await;

    let m = machine(
        &mock,
        RetireInputs {
            candidate_domains: vec!["two.test".into()],
            ..with_public_dns(&mock)
        },
    );
    m.verify().await;
    let row = m.snapshot().servers[0].clone();
    assert_eq!(row.domain.as_deref(), Some("example.test"));
    assert_eq!(row.secondary_domains, vec!["two.test".to_string()]);

    m.select("s-1".into());
    m.begin_confirm();
    let snap = m.snapshot();
    assert!(
        snap.dns_plan
            .iter()
            .any(|l| l.name == "example.test" && l.record_type == "A"),
        "the held zone's apex is removed automatically: {:?}",
        snap.dns_plan
    );
    assert!(
        snap.dns_plan.iter().all(|l| !l.name.ends_with("two.test")),
        "the summary never promises a removal in a zone no credential holds: {:?}",
        snap.dns_plan
    );
    let two = snap
        .leftover_records
        .iter()
        .find(|l| l.name == "two.test" && l.record_type == "A")
        .expect("the second apex A is on the by-hand list");
    assert_eq!(two.value, BOX_IPV4);
    assert_eq!(two.kind, LeftoverKind::PointsAtBox);
}

#[tokio::test]
async fn a_second_domain_added_after_the_page_opened_joins_the_row_too() {
    let mock = MockServer::start().await;
    // The admin entry learns the nest's local-domain list after the page
    // opened (`add_candidate_domains`). That list is the other-verified-domain
    // scan's app-passed half as much as it is an attribution candidate: a
    // domain handed over late must still be verified and listed, or its apex
    // stays on the released address.
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;
    mount_public_a(&mock, "two.test", BOX_IPV4, 1..).await;

    let m = machine(&mock, with_public_dns(&mock));
    m.add_candidate_domains(vec!["two.test".into()]);
    m.verify().await;
    let row = m.snapshot().servers[0].clone();
    assert_eq!(row.domain.as_deref(), Some("example.test"));
    assert_eq!(
        row.secondary_domains,
        vec!["two.test".to_string()],
        "a late-added domain pointing at the box is one of its other verified domains"
    );

    m.select("s-1".into());
    m.begin_confirm();
    let snap = m.snapshot();
    let two = snap
        .leftover_records
        .iter()
        .find(|l| l.name == "two.test" && l.record_type == "A")
        .expect("the late-added domain's apex A is on the by-hand list");
    assert_eq!(two.value, BOX_IPV4);
    assert_eq!(two.kind, LeftoverKind::PointsAtBox);
}

#[tokio::test]
async fn the_held_credential_is_asked_when_the_entered_token_does_not_cover_the_domain() {
    // § Credential stance: the credential is the first, in preference order,
    // whose zones *cover the domain* — not the first that reports any zone.
    // An entered Hetzner-shaped token holding an unrelated zone must not
    // shadow the held `fauna.state.dns` credential that actually holds the
    // box's zone.
    let entered = MockServer::start().await;
    let held = MockServer::start().await;
    mount_account(
        &entered,
        serde_json::json!([{ "id": "z-7", "name": "unrelated.test" }]),
        "example-test",
    )
    .await;
    mount_ptr(&entered, Some("mail.example.test")).await;
    mount_zone_empty_records(&entered, "z-7").await;
    // The public resolver is never consulted for a domain a credential holds.
    mount_public_a(&entered, "example.test", BOX_IPV4, 0).await;

    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(me_json(one_zone())))
        .expect(1..)
        .mount(&held)
        .await;
    mount_apex_a(&held, BOX_IPV4).await;
    mount_empty_records(&held).await;
    Mock::given(method("DELETE"))
        .and(path("/v1/zones/z-1/records/r-apex"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1..)
        .mount(&held)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&entered)
        .await;

    let held_dns = HeldDnsCredential {
        provider_id: "bundled".into(),
        fields: BTreeMap::from([
            ("base-url".to_string(), SecretString::from(held.uri())),
            (
                "api-token".to_string(),
                SecretString::from("held-tok".to_string()),
            ),
        ]),
    };
    let m = machine(
        &entered,
        RetireInputs {
            held_dns: vec![held_dns],
            ..with_public_dns(&entered)
        },
    );
    m.verify().await;
    assert_eq!(
        m.snapshot().servers[0].domain.as_deref(),
        Some("example.test")
    );

    m.select("s-1".into());
    m.begin_confirm();
    let plan = m.snapshot().dns_plan;
    assert!(
        plan.iter()
            .any(|l| l.name == "example.test" && l.record_type == "A" && l.value == BOX_IPV4),
        "the held credential covers the zone, so the cleanup is offered: {plan:?}"
    );

    m.set_confirm_name("example-test".into());
    m.confirm().await;
    let snap = m.snapshot();
    assert_eq!(
        snap.steps[0].state,
        StepState::Done,
        "DNS ran through the held credential"
    );
    assert_eq!(snap.steps[1].state, StepState::Done);
}

fn bundled_held(base: &str, token: &str) -> HeldDnsCredential {
    HeldDnsCredential {
        provider_id: "bundled".into(),
        fields: BTreeMap::from([
            ("base-url".to_string(), SecretString::from(base.to_string())),
            (
                "api-token".to_string(),
                SecretString::from(token.to_string()),
            ),
        ]),
    }
}

#[tokio::test]
async fn every_held_credential_is_asked_and_one_set_after_open_counts() {
    // `fauna.state.dns` holds one credential per (provider, set of zones), so
    // the app passes them all: the domain is cleaned through whichever one's
    // zone covers it, however far down the list (§ Credential stance). The
    // admin entry reads the live config after the page opened, so the list
    // set then is the one `verify` uses.
    let entered = MockServer::start().await;
    let first = MockServer::start().await;
    let holder = MockServer::start().await;
    mount_account(
        &entered,
        serde_json::json!([{ "id": "z-7", "name": "unrelated.test" }]),
        "example-test",
    )
    .await;
    mount_ptr(&entered, Some("mail.example.test")).await;
    mount_zone_empty_records(&entered, "z-7").await;
    mount_public_a(&entered, "example.test", BOX_IPV4, 0).await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(me_json(
            serde_json::json!([{ "id": "z-9", "name": "other.test" }]),
        )))
        .mount(&first)
        .await;
    mount_zone_empty_records(&first, "z-9").await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(me_json(one_zone())))
        .expect(1..)
        .mount(&holder)
        .await;
    mount_apex_a(&holder, BOX_IPV4).await;
    mount_empty_records(&holder).await;

    let m = machine(&entered, with_public_dns(&entered));
    m.set_held_dns(vec![
        bundled_held(&first.uri(), "first-tok"),
        bundled_held(&holder.uri(), "holder-tok"),
    ]);
    m.verify().await;
    assert_eq!(
        m.snapshot().servers[0].domain.as_deref(),
        Some("example.test")
    );
    m.select("s-1".into());
    m.begin_confirm();
    let plan = m.snapshot().dns_plan;
    assert!(
        plan.iter()
            .any(|l| l.name == "example.test" && l.record_type == "A" && l.value == BOX_IPV4),
        "the second held credential covers the zone, so the cleanup is offered: {plan:?}"
    );
}

#[tokio::test]
async fn a_candidate_added_after_open_attributes_like_a_passed_in_one() {
    // The admin entry learns the nest's local-domain list after the page
    // opened (§ DNS cleanup → *Several domains* (1)); added then, it is a
    // candidate exactly as one passed at construction.
    let mock = MockServer::start().await;
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, None).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;

    let m = machine(&mock, RetireInputs::default());
    m.add_candidate_domains(vec!["Example.Test.".into(), " ".into()]);
    m.add_candidate_domains(vec!["example.test".into()]);
    m.verify().await;
    assert_eq!(
        m.snapshot().servers[0].domain.as_deref(),
        Some("example.test"),
        "a late candidate attributes once it verifies"
    );
}

#[test]
fn verify_waits_for_inputs_the_app_announced() {
    let m = NestRetireMachine::new_with_inputs(RetireInputs::default());
    m.select_provider("hetzner".into());
    m.set_credential_field("api-token".into(), "tok".into());
    assert!(m.can_verify());
    m.expect_app_inputs();
    assert!(
        !m.can_verify(),
        "a verify ahead of the admin's own credentials would clean less than they allow"
    );
    m.app_inputs_landed();
    assert!(m.can_verify());
}

#[test]
fn the_binding_faces_pair_held_credentials_by_index() {
    let held = held_dns_from_json(
        vec!["hetzner".into(), "cloudflare".into(), "orphan".into()],
        vec![r#"{"api-token":"h"}"#.into(), "not json".into()],
    );
    assert_eq!(held.len(), 2, "a provider id without its bag is dropped");
    assert_eq!(held[0].provider_id, "hetzner");
    assert_eq!(held[0].fields["api-token"].as_str(), "h");
    assert!(
        held[1].fields.is_empty(),
        "an unparsable bag carries no fields"
    );
}

#[test]
fn every_stored_dns_credential_projects_to_a_held_one() {
    use fauna_core::data::{DnsConfig, DnsProviderCredential};
    let cred = |id: &str, tok: &str| DnsProviderCredential {
        provider_id: id.into(),
        fields: vec![("api-token".into(), SecretString::from(tok.to_string()))],
        zones: vec![],
        label: String::new(),
        created_at: 0,
    };
    let dns = DnsConfig {
        credentials: vec![cred("hetzner", "a"), cred("porkbun", "b")],
        ..Default::default()
    };
    let held = HeldDnsCredential::all_from(&dns);
    assert_eq!(
        held.iter()
            .map(|h| h.provider_id.as_str())
            .collect::<Vec<_>>(),
        ["hetzner", "porkbun"]
    );
    assert_eq!(held[1].fields["api-token"].as_str(), "b");
}

#[tokio::test]
async fn an_aaaa_at_the_boxs_ipv6_is_planned_and_removed_before_the_server() {
    let mock = MockServer::start().await;
    const BOX_IPV6: &str = "2001:db8::5";
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(me_json(one_zone())))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/servers"))
        .and(query_param("label", "managed-by=fauna"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "servers": [{
                "id": "s-1",
                "name": "example-test",
                "ipv4": BOX_IPV4,
                "ipv6": BOX_IPV6,
                "status": "running",
                "labels": { "managed-by": "fauna" }
            }]
        })))
        .mount(&mock)
        .await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    Mock::given(method("GET"))
        .and(path("/v1/zones/z-1/records"))
        .and(query_param("name", "@"))
        .and(query_param("type", "AAAA"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "records": [{ "id": "r-apex6", "type": "AAAA", "name": "@", "value": BOX_IPV6, "ttl": 300 }]
        })))
        .mount(&mock)
        .await;
    mount_empty_records(&mock).await;
    Mock::given(method("DELETE"))
        .and(path("/v1/zones/z-1/records/r-apex6"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/zones/z-1/records/r-apex"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&mock)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&mock)
        .await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    assert_eq!(m.snapshot().servers[0].ipv6.as_deref(), Some(BOX_IPV6));

    m.select("s-1".into());
    m.begin_confirm();
    let plan = m.snapshot().dns_plan;
    assert!(
        plan.iter()
            .any(|l| l.name == "example.test" && l.record_type == "AAAA" && l.value == BOX_IPV6),
        "the box has a v6, so its AAAA is planned too: {plan:?}"
    );

    m.set_confirm_name("example-test".into());
    m.confirm().await;
    let snap = m.snapshot();
    assert_eq!(snap.steps[0].state, StepState::Done);
    assert_eq!(snap.steps[1].state, StepState::Done);
}

// ---------------------------------------------------------------------------
// The typed-name gate (§ Confirm shape)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_confirm_button_enables_only_on_an_exact_name_match() {
    let mock = MockServer::start().await;
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    m.select("s-1".into());
    m.begin_confirm();

    assert!(!m.snapshot().confirm_enabled, "starts disabled");
    for wrong in ["example", "example-tes", "Example-Test", "example-test "] {
        m.set_confirm_name(wrong.into());
        assert!(
            !m.snapshot().confirm_enabled,
            "{wrong:?} is not an exact match"
        );
    }
    m.set_confirm_name("example-test".into());
    assert!(m.snapshot().confirm_enabled);
}

#[tokio::test]
async fn confirm_refuses_to_run_until_the_name_is_typed() {
    let mock = MockServer::start().await;
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;
    // Never called: `expect(0)` is the assertion.
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&mock)
        .await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    m.select("s-1".into());
    m.begin_confirm();
    m.confirm().await;

    assert_eq!(m.snapshot().phase, RetirePhase::Confirm, "still on confirm");
}

// ---------------------------------------------------------------------------
// DNS first, then the server (§ DNS cleanup — scope and order)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_dns_step_removes_the_apex_a_before_the_server_is_deleted() {
    let mock = MockServer::start().await;
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;
    Mock::given(method("DELETE"))
        .and(path("/v1/zones/z-1/records/r-apex"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1..)
        .mount(&mock)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&mock)
        .await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    m.select("s-1".into());
    m.begin_confirm();

    let plan = m.snapshot().dns_plan;
    assert!(
        plan.iter()
            .any(|l| l.name == "example.test" && l.record_type == "A" && l.value == BOX_IPV4),
        "the confirm summary states the value-scoped plan: {plan:?}"
    );

    m.set_confirm_name("example-test".into());
    m.confirm().await;

    let snap = m.snapshot();
    assert_eq!(snap.steps[0].state, StepState::Done, "DNS ran");
    assert_eq!(snap.steps[1].state, StepState::Done, "then the server");
    assert_eq!(snap.phase, RetirePhase::Done);
    assert!(snap.servers.is_empty(), "the retired box leaves the list");
}

#[tokio::test]
async fn a_failed_dns_step_stops_before_the_server_and_offers_force() {
    let mock = MockServer::start().await;
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    // Every *other* record read fails — the cleanup cannot complete.
    Mock::given(method("GET"))
        .and(path("/v1/zones/z-1/records"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&mock)
        .await;
    // The box must NOT be destroyed: deleting the server first and failing
    // before the DNS step strands `A` records on an address the provider will
    // hand to a stranger.
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&mock)
        .await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    m.select("s-1".into());
    m.begin_confirm();
    m.set_confirm_name("example-test".into());
    m.confirm().await;

    let snap = m.snapshot();
    assert!(
        matches!(snap.steps[0].state, StepState::Failed { .. }),
        "DNS failed: {:?}",
        snap.steps[0].state
    );
    assert_eq!(
        snap.steps[1].state,
        StepState::Pending,
        "the server was never touched"
    );
    assert!(
        snap.force_server_offered,
        "retry or delete-anyway is offered"
    );
    assert!(snap.error.is_some());
}

#[tokio::test]
async fn force_server_deletes_the_box_despite_the_failed_dns_step() {
    let mock = MockServer::start().await;
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    Mock::given(method("GET"))
        .and(path("/v1/zones/z-1/records"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&mock)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&mock)
        .await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    m.select("s-1".into());
    m.begin_confirm();
    m.set_confirm_name("example-test".into());
    m.confirm().await;
    assert!(m.snapshot().force_server_offered);

    m.force_server().await;

    let snap = m.snapshot();
    assert_eq!(snap.steps[1].state, StepState::Done);
    assert_eq!(snap.phase, RetirePhase::Done);
    assert!(
        matches!(snap.steps[0].state, StepState::Skipped { .. }),
        "the DNS step is recorded as forced past, not as done: {:?}",
        snap.steps[0].state
    );
}

#[tokio::test]
async fn a_re_run_after_a_crash_between_the_steps_converges() {
    let mock = MockServer::start().await;
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;
    Mock::given(method("DELETE"))
        .and(path("/v1/zones/z-1/records/r-apex"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&mock)
        .await;
    // The crash: the server delete fails the first time, then the box turns
    // out to be already gone (404) — which `delete_server` treats as success,
    // so the re-run finishes rather than raising.
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-1"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-1"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&mock)
        .await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    m.select("s-1".into());
    m.begin_confirm();
    m.set_confirm_name("example-test".into());
    m.confirm().await;

    assert!(
        matches!(m.snapshot().steps[1].state, StepState::Failed { .. }),
        "first run died at the server step"
    );

    // Re-run: the DNS pass is idempotent (the records are already gone) and
    // the server delete converges on 404-is-success.
    m.retry().await;

    let snap = m.snapshot();
    assert_eq!(snap.steps[0].state, StepState::Done);
    assert_eq!(snap.steps[1].state, StepState::Done);
    assert_eq!(snap.phase, RetirePhase::Done);
}

// ---------------------------------------------------------------------------
// The typed name binds the run to one server (§ Confirm shape)
// ---------------------------------------------------------------------------

/// Two boxes in one account — `box-a` (`s-1`, attributed to `example.test`)
/// and `box-b` (`s-2`, unattributed). On OVH every listed row is unmarked and
/// the typed name is the only guard for any of them. `box-b`'s server `DELETE`
/// is mounted with `expect(0)`: every test below asserts that no path reaches
/// it without its name being typed.
async fn mount_two_boxes(mock: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(me_json(one_zone())))
        .mount(mock)
        .await;
    let box_b = serde_json::json!({
        "id": "s-2",
        "name": "box-b",
        "ipv4": "203.0.113.9",
        "status": "running",
        "labels": { "managed-by": "fauna" }
    });
    Mock::given(method("GET"))
        .and(path("/v1/servers"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "servers": [server_json("box-a"), box_b] })),
        )
        .mount(mock)
        .await;
    mount_ptr(mock, Some("mail.example.test")).await;
    Mock::given(method("GET"))
        .and(path("/v1/servers/s-2/ptr"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ptr": null })))
        .mount(mock)
        .await;
    mount_apex_a(mock, BOX_IPV4).await;
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-2"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(mock)
        .await;
}

/// Sign in again after a `cancel` — the page's second visit.
async fn revisit(m: &NestRetireMachine, mock: &MockServer) {
    m.select_provider("bundled".into());
    m.set_credential_field("base-url".into(), mock.uri());
    m.set_credential_field("api-token".into(), "tok".into());
    m.verify().await;
}

#[tokio::test]
async fn force_server_from_the_list_deletes_nothing() {
    let mock = MockServer::start().await;
    mount_two_boxes(&mock).await;
    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    assert_eq!(m.snapshot().phase, RetirePhase::List);

    m.select("s-2".into());
    m.force_server().await;

    assert_eq!(
        m.snapshot().phase,
        RetirePhase::List,
        "no run starts without a confirmed name"
    );
}

#[tokio::test]
async fn retry_from_the_list_deletes_nothing() {
    let mock = MockServer::start().await;
    mount_two_boxes(&mock).await;
    let m = machine(&mock, RetireInputs::default());
    m.verify().await;

    m.select("s-2".into());
    m.retry().await;

    assert_eq!(
        m.snapshot().phase,
        RetirePhase::List,
        "no run starts without a confirmed name"
    );
}

#[tokio::test]
async fn the_offer_made_for_one_box_never_deletes_another() {
    let mock = MockServer::start().await;
    mount_two_boxes(&mock).await;
    // box-a's cleanup fails, so *delete the server anyway* is offered for it.
    Mock::given(method("GET"))
        .and(path("/v1/zones/z-1/records"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&mock)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&mock)
        .await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    m.select("s-1".into());
    m.begin_confirm();
    m.set_confirm_name("box-a".into());
    m.confirm().await;
    assert!(m.snapshot().force_server_offered, "offered for box-a");

    // Re-targeting mid-run is refused; the offer still acts on box-a only.
    m.select("s-2".into());
    assert_eq!(m.snapshot().selected.as_deref(), Some("s-1"));
    m.force_server().await;

    let snap = m.snapshot();
    assert_eq!(snap.phase, RetirePhase::Done);
    assert_eq!(
        snap.servers
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>(),
        ["box-b"],
        "box-a retired, box-b untouched"
    );
}

#[tokio::test]
async fn force_server_is_refused_unless_it_was_offered() {
    let mock = MockServer::start().await;
    mount_two_boxes(&mock).await;
    Mock::given(method("DELETE"))
        .and(path("/v1/zones/z-1/records/r-apex"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&mock)
        .await;
    mount_empty_records(&mock).await;
    // The run dies at the server step, not the DNS step: retry is the way
    // on, and *delete anyway* (which skips the DNS step) is not offered.
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-1"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .expect(1)
        .mount(&mock)
        .await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    m.select("s-1".into());
    m.begin_confirm();
    m.set_confirm_name("box-a".into());
    m.confirm().await;
    let snap = m.snapshot();
    assert!(matches!(snap.steps[1].state, StepState::Failed { .. }));
    assert!(!snap.force_server_offered);

    m.force_server().await;

    assert!(
        matches!(m.snapshot().steps[1].state, StepState::Failed { .. }),
        "an un-offered force does not run"
    );
}

#[tokio::test]
async fn cancel_disarms_the_run() {
    let mock = MockServer::start().await;
    mount_two_boxes(&mock).await;
    Mock::given(method("GET"))
        .and(path("/v1/zones/z-1/records"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&mock)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&mock)
        .await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    m.select("s-1".into());
    m.begin_confirm();
    m.set_confirm_name("box-a".into());
    m.confirm().await;
    assert!(m.snapshot().force_server_offered);

    // Leave the page and come back with the same provider: the arm made in
    // the last visit does not survive into this one.
    m.cancel();
    revisit(&m, &mock).await;
    m.select("s-1".into());
    m.force_server().await;
    m.retry().await;

    assert_eq!(m.snapshot().phase, RetirePhase::List);
}

// ---------------------------------------------------------------------------
// A partly-failed DNS step says what is left (§ DNS cleanup — scope and order)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_partly_failed_dns_step_lists_what_was_not_removed_after_force() {
    let mock = MockServer::start().await;
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    // The apex `A` goes; the `MX` read fails part way through the walk.
    Mock::given(method("DELETE"))
        .and(path("/v1/zones/z-1/records/r-apex"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1..)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/zones/z-1/records"))
        .and(query_param("type", "MX"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&mock)
        .await;
    mount_empty_records(&mock).await;
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&mock)
        .await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    m.select("s-1".into());
    m.begin_confirm();
    assert!(
        m.snapshot().dns_plan.iter().all(|l| !l.removed),
        "nothing is removed before the run"
    );
    m.set_confirm_name("example-test".into());
    m.confirm().await;
    assert!(m.snapshot().force_server_offered, "the MX read failed");

    let plan = m.snapshot().dns_plan;
    let apex = plan
        .iter()
        .find(|l| l.name == "example.test" && l.record_type == "A")
        .expect("apex A planned");
    assert!(
        apex.removed,
        "the apex A landed before the failure: {plan:?}"
    );
    let mx = plan
        .iter()
        .find(|l| l.record_type == "MX")
        .expect("MX planned");
    assert!(!mx.removed, "the MX did not: {plan:?}");

    m.force_server().await;

    let snap = m.snapshot();
    assert_eq!(snap.phase, RetirePhase::Done);
    assert!(
        !snap
            .leftover_records
            .iter()
            .any(|l| l.name == "example.test" && l.record_type == "A"),
        "the removed apex A is not by-hand work: {:?}",
        snap.leftover_records
    );
    for line in snap.dns_plan.iter().filter(|l| !l.removed) {
        assert!(
            snap.leftover_records.iter().any(|l| l.name == line.name
                && l.record_type == line.record_type
                && l.value == line.value
                && l.kind == LeftoverKind::PointsAtBox),
            "every not-removed line is on the by-hand list, urgent: {line:?} in {:?}",
            snap.leftover_records
        );
    }
    assert!(
        snap.leftover_records
            .iter()
            .any(|l| l.record_type == "MX" && l.kind == LeftoverKind::PointsAtBox),
        "the MX among them: {:?}",
        snap.leftover_records
    );
}

// ---------------------------------------------------------------------------
// The transfer-authorization code (§ Transfer authorization code)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_transfer_code_is_idle_then_shows_the_code() {
    let mock = MockServer::start().await;
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;
    Mock::given(method("GET"))
        .and(path("/v1/domains/example.test/auth-code"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "auth_code": "XFER-123" })),
        )
        .expect(1)
        .mount(&mock)
        .await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    assert_eq!(
        m.snapshot().servers[0].transfer_code,
        TransferCodeState::Idle,
        "the bundled provider supports the call and the domain verified"
    );

    m.select("s-1".into());
    m.fetch_transfer_code().await;
    assert_eq!(
        m.snapshot().servers[0].transfer_code,
        TransferCodeState::Code {
            code: "XFER-123".into()
        }
    );
}

#[tokio::test]
async fn a_registry_lock_reports_when_it_lifts_not_a_refusal() {
    let mock = MockServer::start().await;
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;
    Mock::given(method("GET"))
        .and(path("/v1/domains/example.test/auth-code"))
        .respond_with(
            ResponseTemplate::new(202)
                .set_body_json(serde_json::json!({ "available_after": "2026-11-18T00:00:00Z" })),
        )
        .mount(&mock)
        .await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    m.select("s-1".into());
    m.fetch_transfer_code().await;

    assert_eq!(
        m.snapshot().servers[0].transfer_code,
        TransferCodeState::AvailableAfter {
            when: "2026-11-18T00:00:00Z".into()
        }
    );
}

// ---------------------------------------------------------------------------
// Credential stance (§ Credential stance) and the *current* badge
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_drops_the_token_and_everything_fetched_with_it() {
    let mock = MockServer::start().await;
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;
    Mock::given(method("GET"))
        .and(path("/v1/domains/example.test/auth-code"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "auth_code": "XFER-123" })),
        )
        .mount(&mock)
        .await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    m.select("s-1".into());
    m.fetch_transfer_code().await;
    assert!(matches!(
        m.snapshot().servers[0].transfer_code,
        TransferCodeState::Code { .. }
    ));

    m.cancel();

    let snap = m.snapshot();
    assert_eq!(snap.phase, RetirePhase::Credentials);
    assert!(snap.servers.is_empty(), "the fetched code went with it");
    assert!(snap.selected.is_none());

    // The token is gone: a verify without re-entering it cannot build a
    // dispatcher, so it fails back to the credential form.
    m.select_provider("bundled".into());
    m.verify().await;
    assert_eq!(m.snapshot().phase, RetirePhase::Credentials);
    assert!(m.snapshot().error.is_some());
}

#[tokio::test]
async fn the_current_badge_marks_the_box_this_session_is_signed_into() {
    let mock = MockServer::start().await;
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;

    let m = machine(
        &mock,
        RetireInputs {
            current_ipv4: Some(BOX_IPV4.into()),
            ..Default::default()
        },
    );
    m.verify().await;
    assert!(m.snapshot().servers[0].current);

    let other = machine(
        &mock,
        RetireInputs {
            current_ipv4: Some("198.51.100.1".into()),
            ..Default::default()
        },
    );
    other.verify().await;
    assert!(!other.snapshot().servers[0].current);
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

#[test]
fn value_matches_tolerates_trailing_dots_and_srv_prefixes() {
    assert!(value_matches("mail.example.test.", "mail.example.test"));
    assert!(value_matches("10 mail.example.test", "mail.example.test"));
    assert!(value_matches(
        "0 1 443 mail.example.test.",
        "mail.example.test"
    ));
    assert!(!value_matches("mail.other.test", "mail.example.test"));
}

#[test]
fn zone_id_for_prefers_the_longest_matching_zone() {
    let zones = vec![
        ("example.test".to_string(), "z-parent".to_string()),
        ("sub.example.test".to_string(), "z-child".to_string()),
    ];
    assert_eq!(
        zone_id_for(&zones, "sub.example.test").as_deref(),
        Some("z-child")
    );
    assert_eq!(
        zone_id_for(&zones, "other.example.test").as_deref(),
        Some("z-parent")
    );
    assert_eq!(zone_id_for(&zones, "unrelated.test"), None);
}

// ---------------------------------------------------------------------------
// Hosted sign-in (the bundled provider's `hosted-auth` credential field)
// ---------------------------------------------------------------------------

/// The retire view reuses `vps_config`'s credential controls, so a bundled
/// provider — the one with a transfer code — signs in here exactly as it does
/// in the wizard: the device flow lands its token in the credential bag, and
/// the listing then authenticates with it.
#[tokio::test]
async fn hosted_sign_in_lands_the_token_the_listing_then_uses() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/device"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_code": "dev-1",
            "user_code": "ABCD-EFGH",
            "verification_uri": "https://bundle.example/activate",
            "expires_in": 60,
            "interval": 0
        })))
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "access_token": "tok-final", "token_type": "bearer" }),
        ))
        .mount(&mock)
        .await;
    // The listing answers only the token the sign-in yielded.
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer tok-final",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(me_json(one_zone())))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/servers"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer tok-final",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "servers": [server_json("example-test")] })),
        )
        .mount(&mock)
        .await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;

    let m = NestRetireMachine::new_with_inputs(RetireInputs::default());
    m.select_provider("bundled".into());
    assert!(
        !m.hosted_auth_can_begin("api-token".into()),
        "no base-url yet → the button is dead"
    );
    m.set_credential_field("base-url".into(), mock.uri());
    assert!(m.hosted_auth_can_begin("api-token".into()));

    let prompt = m
        .hosted_auth_begin("api-token".into())
        .await
        .expect("the device request is answered");
    assert_eq!(prompt.user_code, "ABCD-EFGH");
    assert!(
        !m.hosted_auth_can_begin("api-token".into()),
        "mid-flight → not re-pressable"
    );
    m.hosted_auth_wait("api-token".into()).await;
    assert_eq!(
        m.hosted_auth_state("api-token".into()),
        HostedAuthState::Connected
    );

    m.verify().await;
    let snap = m.snapshot();
    assert_eq!(snap.phase, RetirePhase::List, "error: {:?}", snap.error);
    assert_eq!(snap.servers[0].domain.as_deref(), Some("example.test"));
}

/// Leaving the page drops the signed-in token with every other credential —
/// the credential stance is that nothing entered here outlives the visit.
#[tokio::test]
async fn cancel_forgets_the_hosted_sign_in() {
    let m = NestRetireMachine::new_with_inputs(RetireInputs::default());
    m.select_provider("bundled".into());
    m.inner
        .lock()
        .unwrap()
        .hosted_auth
        .insert("api-token".into(), HostedAuthState::Connected);
    m.cancel();
    assert_eq!(
        m.hosted_auth_state("api-token".into()),
        HostedAuthState::Idle
    );
}

// ---------------------------------------------------------------------------
// The view's sentences (§ Confirm shape, § DNS cleanup) and the current box
// ---------------------------------------------------------------------------

fn keys(lines: &[LocalizedText]) -> Vec<&str> {
    lines.iter().map(|l| l.key.as_str()).collect()
}

/// The summary names the server, the provider (as a display key), the
/// address, the domain, the unrecoverable loss and the DNS plan — every fact
/// § Confirm shape requires before the person types the name.
#[tokio::test]
async fn the_confirm_summary_names_what_dies_and_the_dns_plan() {
    let mock = MockServer::start().await;
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    assert!(
        m.confirm_summary().is_empty(),
        "nothing selected, nothing to summarize"
    );
    m.select("s-1".into());
    m.begin_confirm();

    let summary = m.confirm_summary();
    assert_eq!(
        keys(&summary)[..4],
        [
            "onboarding.retire.confirm_what",
            "onboarding.retire.confirm_domain",
            "onboarding.retire.confirm_destroyed",
            "onboarding.retire.confirm_dns_removals",
        ]
    );
    let what = &summary[0].args;
    assert_eq!(what["name"], "example-test");
    assert_eq!(what["address"], BOX_IPV4);
    assert_eq!(what["provider"], "provisioning.bundled.name");
    assert!(summary[3].args["records"].contains(BOX_IPV4));
    assert!(
        !keys(&summary).contains(&"onboarding.retire.confirm_current"),
        "not this session's box → no current-box sentence"
    );
}

/// The badge follows an address the app learns after the page opened.
#[tokio::test]
async fn a_late_current_address_marks_the_listed_row() {
    let mock = MockServer::start().await;
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    assert!(!m.snapshot().servers[0].current);
    m.set_current_ipv4(Some(BOX_IPV4.into()));
    assert!(m.snapshot().servers[0].current);
    m.select("s-1".into());
    m.begin_confirm();
    assert!(keys(&m.confirm_summary()).contains(&"onboarding.retire.confirm_current"));
    m.set_current_ipv4(None);
    assert!(!m.snapshot().servers[0].current);
}

/// Done still names the deleted row, and the by-hand list reads the urgent
/// records first.
#[tokio::test]
async fn done_keeps_the_retired_row_and_lists_what_is_left() {
    let mock = MockServer::start().await;
    mount_account(&mock, one_zone(), "example-test").await;
    mount_ptr(&mock, Some("mail.example.test")).await;
    mount_apex_a(&mock, BOX_IPV4).await;
    mount_empty_records(&mock).await;
    Mock::given(method("DELETE"))
        .and(path("/v1/zones/z-1/records/r-apex"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&mock)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/servers/s-1"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&mock)
        .await;

    let m = machine(&mock, RetireInputs::default());
    m.verify().await;
    m.select("s-1".into());
    m.begin_confirm();
    m.set_confirm_name("example-test".into());
    m.confirm().await;

    let snap = m.snapshot();
    assert_eq!(snap.phase, RetirePhase::Done, "error: {:?}", snap.error);
    assert_eq!(
        snap.retired.as_ref().map(|r| r.name.as_str()),
        Some("example-test")
    );
    let lines = m.leftover_lines();
    assert!(!lines.is_empty());
    assert!(
        lines
            .iter()
            .all(|l| l.key == "onboarding.retire.leftover_shared_name"),
        "every value-scoped record was removable, so only the shared-name TXT remain: {lines:?}"
    );
}

#[test]
fn a_skipped_step_says_why_and_the_status_maps_onto_provisioning_progress() {
    let m = NestRetireMachine::new_with_inputs(RetireInputs::default());
    let note = m
        .step_note(StepState::Skipped {
            why: "no_verified_domain".into(),
        })
        .expect("a skipped step carries its reason");
    assert_eq!(
        note.key,
        "onboarding.retire.step_skipped_no_verified_domain"
    );
    assert!(
        fauna_i18n::strings::lookup(&note.key).is_some(),
        "every skip code the machine emits has a sentence"
    );
    for why in [
        "no_dns_credential",
        "no_zone_for_domain",
        "dns_step_forced_past",
    ] {
        let key = format!("onboarding.retire.step_skipped_{why}");
        assert!(
            fauna_i18n::strings::lookup(&key).is_some(),
            "{key} is missing"
        );
    }
    assert_eq!(m.step_note(StepState::Done), None);
    assert_eq!(
        StepState::Failed { cause: "x".into() }.progress_status(),
        fauna_provisioning::progress::StepStatus::Failed
    );
}
