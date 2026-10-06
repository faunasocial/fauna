//! DNS conformance harness — pins per-provider adapter behavior using wiremock.
//!
//! Scope: focused on the two known-buggy paths the survey identified
//! (Gandi MX-priority handling, Namecheap long-TXT splitting). Each test
//! either succeeds (correct behavior) or fails-by-design (bug present).
//! Task 10.5 fixes the failures; this harness is the gating signal.
//!
//! The other three DNS adapters (Cloudflare, Porkbun, Hetzner) already have
//! response-shape unit tests in their respective modules; their request-side
//! behavior is exercised end-to-end by the orchestrator integration tests
//! and per-app e2e suites, so re-asserting them here would be churn.
//!
//! Exception — the **TLSA create** section (Slice 5b.5): the floor-MX DANE pin
//! is *new* per-provider request behavior, not a re-assertion. Cloudflare needs
//! a structured `data` object (a `content` string is rejected), so that body
//! shape is pinned here; Gandi/Porkbun verbatim passthrough is pinned alongside.
//! (Namecheap has no TLSA type — its rejection is a unit test in the adapter;
//! Hetzner passthrough is a `wire_value` unit test.)

use fauna_provisioning::dkim::generate_rsa_2048;
use fauna_provisioning::dns::DnsProvider;
use fauna_provisioning::dns::DnsRecord;
use fauna_provisioning::dns::cloudflare::Cloudflare;
use fauna_provisioning::dns::gandi::Gandi;
use fauna_provisioning::dns::hetzner::HetznerDns;
use fauna_provisioning::dns::namecheap::Namecheap;
use fauna_provisioning::dns::porkbun::Porkbun;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// Gandi: MX-priority handling
// ---------------------------------------------------------------------------

/// Gandi expects MX records as `rrset_values: ["10 mail.example.com"]` —
/// priority is encoded into the value, not a separate field. Today's adapter
/// ignores `record.priority` entirely. Once Task 10.5 lands, the body should
/// include the prefixed value.
#[tokio::test]
async fn gandi_mx_priority_is_prefixed_into_value() {
    let mock = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/domain/domains/example.com/records"))
        .and(body_partial_json(serde_json::json!({
            "rrset_type": "MX",
            "rrset_name": "@",
            "rrset_values": ["10 mail.example.com"],
            "rrset_ttl": 300,
        })))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&mock)
        .await;

    let gandi = Gandi::with_base_url("test-token".into(), mock.uri());
    let record = DnsRecord {
        record_type: "MX".into(),
        name: "@".into(),
        value: "mail.example.com".into(),
        ttl: 300,
        priority: Some(10),
    };

    let client = reqwest::Client::new();
    let result = gandi.create_record(&client, "example.com", &record).await;

    assert!(
        result.is_ok(),
        "expected MX create to succeed; got {:?}. The adapter is most likely \
         dropping `record.priority`. Task 10.5 fixes this.",
        result
    );
}

/// Gandi's non-MX records pass through unchanged.
#[tokio::test]
async fn gandi_short_txt_passes_through() {
    let mock = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/domain/domains/example.com/records"))
        .and(header("authorization", "Bearer test-token"))
        .and(body_partial_json(serde_json::json!({
            "rrset_type": "TXT",
            "rrset_name": "@",
            "rrset_values": ["v=spf1 a mx -all"],
            "rrset_ttl": 300,
        })))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&mock)
        .await;

    let gandi = Gandi::with_base_url("test-token".into(), mock.uri());
    let record = DnsRecord {
        record_type: "TXT".into(),
        name: "@".into(),
        value: "v=spf1 a mx -all".into(),
        ttl: 300,
        priority: None,
    };

    let client = reqwest::Client::new();
    let result = gandi.create_record(&client, "example.com", &record).await;
    assert!(result.is_ok(), "TXT create failed: {:?}", result);
}

// ---------------------------------------------------------------------------
// Namecheap: long-TXT splitting
// ---------------------------------------------------------------------------

/// A short TXT record sends a single Address parameter with index `n=1`
/// (no existing hosts in the mock).
#[tokio::test]
async fn namecheap_short_txt_single_host_entry() {
    let mock = MockServer::start().await;

    // First call: getHosts returns no existing records.
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.dns.getHosts"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EMPTY_HOSTS_XML))
        .mount(&mock)
        .await;

    // Second call: setHosts with single Address1 holding the full value.
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.dns.setHosts"))
        .and(query_param("HostName1", "@"))
        .and(query_param("RecordType1", "TXT"))
        .and(query_param("Address1", "v=spf1 a mx -all"))
        .and(query_param("TTL1", "300"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SET_HOSTS_OK_XML))
        .expect(1)
        .mount(&mock)
        .await;

    let namecheap = Namecheap::with_base_url("u".into(), "k".into(), mock.uri());
    let record = DnsRecord {
        record_type: "TXT".into(),
        name: "@".into(),
        value: "v=spf1 a mx -all".into(),
        ttl: 300,
        priority: None,
    };

    let client = reqwest::Client::new();
    let result = namecheap
        .create_record(&client, "example.com", &record)
        .await;
    assert!(result.is_ok(), "short TXT create failed: {:?}", result);
}

/// Namecheap's `Address{n}` field has a 255-byte hard limit. A real DKIM TXT
/// value (traced through the actual `dkim.rs` keygen, not a synthetic
/// approximation — it comes out to 410 chars, splitting 255 + 155) must split
/// into multiple host entries with the same `HostName{n}`, each chunk fitting
/// in 255 bytes. Today's adapter sends one oversize `Address1` and the
/// registrar rejects it.
#[tokio::test]
async fn namecheap_long_txt_splits_into_multiple_hosts() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.dns.getHosts"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EMPTY_HOSTS_XML))
        .mount(&mock)
        .await;

    // A real DKIM TXT value — not a synthetic approximation — including
    // base64's `+`/`/`/`=` alphabet, which "A".repeat(400) never exercised
    // through Namecheap's query-string transport.
    let big: String = generate_rsa_2048().expect("dkim keygen").public_dns_value;
    assert!(
        big.len() > 255,
        "expected a real DKIM value to exceed the 255-byte chunk threshold"
    );
    let chunk_a: String = big[..255].to_string();
    let chunk_b: String = big[255..].to_string();

    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.dns.setHosts"))
        // Two host entries with same HostName, sequential indices, each
        // Address chunk ≤ 255 bytes.
        .and(query_param("HostName1", "dkim._domainkey"))
        .and(query_param("RecordType1", "TXT"))
        .and(query_param("Address1", chunk_a.as_str()))
        .and(query_param("HostName2", "dkim._domainkey"))
        .and(query_param("RecordType2", "TXT"))
        .and(query_param("Address2", chunk_b.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_string(SET_HOSTS_OK_XML))
        .expect(1)
        .mount(&mock)
        .await;

    let namecheap = Namecheap::with_base_url("u".into(), "k".into(), mock.uri());
    let record = DnsRecord {
        record_type: "TXT".into(),
        name: "dkim._domainkey".into(),
        value: big,
        ttl: 300,
        priority: None,
    };

    let client = reqwest::Client::new();
    let result = namecheap
        .create_record(&client, "example.com", &record)
        .await;

    assert!(
        result.is_ok(),
        "expected long-TXT splitting; got {:?}. Today's adapter sends a \
         single oversize Address1 — Task 10.5 adds split_long_txt().",
        result
    );
}

/// MX records on Namecheap continue to pass through `MXPref{n}` unchanged.
#[tokio::test]
async fn namecheap_mx_priority_is_passed_through() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.dns.getHosts"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EMPTY_HOSTS_XML))
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.dns.setHosts"))
        .and(query_param("HostName1", "@"))
        .and(query_param("RecordType1", "MX"))
        .and(query_param("Address1", "mail.example.com"))
        .and(query_param("MXPref1", "10"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SET_HOSTS_OK_XML))
        .expect(1)
        .mount(&mock)
        .await;

    let namecheap = Namecheap::with_base_url("u".into(), "k".into(), mock.uri());
    let record = DnsRecord {
        record_type: "MX".into(),
        name: "@".into(),
        value: "mail.example.com".into(),
        ttl: 300,
        priority: Some(10),
    };

    let client = reqwest::Client::new();
    let result = namecheap
        .create_record(&client, "example.com", &record)
        .await;
    assert!(result.is_ok(), "MX create failed: {:?}", result);
}

// ---------------------------------------------------------------------------
// Hetzner: Hetzner Cloud API (RRset model) — pins the request shapes after the
// migration off the dead `dns.hetzner.com/api/v1` standalone API. Bearer auth,
// value-based create/find/delete mapped onto RRset create + add/remove-records
// actions.
// ---------------------------------------------------------------------------

/// `verify` hits `GET /zones?per_page=50` with Bearer auth and maps integer
/// zone ids to strings.
#[tokio::test]
async fn hetzner_verify_uses_bearer_and_lists_zones() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/zones"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "zones": [
                {"id": 42, "name": "example.com"},
                {"id": 43, "name": "another.org"},
            ],
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let hetzner = HetznerDns::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let zones = hetzner.verify(&client).await.expect("verify failed");

    assert_eq!(zones.len(), 2);
    assert_eq!(zones[0].id, "42");
    assert_eq!(zones[0].name, "example.com");
}

/// When the RRset does not exist, `create_record` creates it via
/// `POST /zones/{zone}/rrsets` (name/type/ttl/value in the body).
#[tokio::test]
async fn hetzner_create_record_creates_rrset_when_absent() {
    let mock = MockServer::start().await;

    // Existence probe: no matching RRset yet.
    Mock::given(method("GET"))
        .and(path("/zones/example.com/rrsets"))
        .and(query_param("name", "_dmarc"))
        .and(query_param("type", "TXT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "rrsets": [] })))
        .mount(&mock)
        .await;

    // Create the RRset with our record.
    Mock::given(method("POST"))
        .and(path("/zones/example.com/rrsets"))
        .and(header("authorization", "Bearer test-token"))
        .and(body_partial_json(serde_json::json!({
            "name": "_dmarc",
            "type": "TXT",
            "ttl": 300,
            // Hetzner Cloud requires TXT values zone-file-quoted (the adapter
            // wraps the bare seam value `v=DMARC1; p=reject`).
            "records": [{ "value": "\"v=DMARC1; p=reject\"" }],
        })))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&mock)
        .await;

    let hetzner = HetznerDns::with_base_url("test-token".into(), mock.uri());
    let record = DnsRecord {
        record_type: "TXT".into(),
        name: "_dmarc".into(),
        value: "v=DMARC1; p=reject".into(),
        ttl: 300,
        priority: None,
    };
    let client = reqwest::Client::new();
    hetzner
        .create_record(&client, "example.com", &record)
        .await
        .expect("create (absent rrset) failed");
}

/// When the RRset already holds other values, `create_record` adds ours via
/// the `add_records` action (not a whole-RRset replace).
#[tokio::test]
async fn hetzner_create_record_adds_to_existing_rrset() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/zones/example.com/rrsets"))
        .and(query_param("name", "@"))
        .and(query_param("type", "TXT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "rrsets": [{
                "id": "@/TXT",
                "name": "@",
                "type": "TXT",
                "ttl": 300,
                // Hetzner returns TXT zone-file-quoted.
                "records": [{ "value": "\"v=spf1 -all\"" }],
            }],
        })))
        .mount(&mock)
        .await;

    Mock::given(method("POST"))
        .and(path("/zones/example.com/rrsets/@/TXT/actions/add_records"))
        .and(header("authorization", "Bearer test-token"))
        .and(body_partial_json(serde_json::json!({
            "records": [{ "value": "\"keybase-site-verification=abc\"" }],
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "action": { "id": 1, "status": "success" },
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let hetzner = HetznerDns::with_base_url("test-token".into(), mock.uri());
    let record = DnsRecord {
        record_type: "TXT".into(),
        name: "@".into(),
        value: "keybase-site-verification=abc".into(),
        ttl: 300,
        priority: None,
    };
    let client = reqwest::Client::new();
    hetzner
        .create_record(&client, "example.com", &record)
        .await
        .expect("create (existing rrset) failed");
}

/// A same-value `create_record` is a no-op — no create/add request is sent
/// (only the existence probe). The POST mocks below `.expect(0)`-by-absence:
/// any stray POST hits no mock and wiremock 404s → the call would error.
#[tokio::test]
async fn hetzner_create_record_is_idempotent_when_value_present() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/zones/example.com/rrsets"))
        .and(query_param("name", "_acme-challenge"))
        .and(query_param("type", "TXT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "rrsets": [{
                "id": "_acme-challenge/TXT",
                "name": "_acme-challenge",
                "type": "TXT",
                "ttl": 60,
                // Stored zone-file-quoted, matching the adapter's quoted
                // wire_value so the same-value create is a genuine no-op.
                "records": [{ "value": "\"token-xyz\"" }],
            }],
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let hetzner = HetznerDns::with_base_url("test-token".into(), mock.uri());
    let record = DnsRecord {
        record_type: "TXT".into(),
        name: "_acme-challenge".into(),
        value: "token-xyz".into(),
        ttl: 60,
        priority: None,
    };
    let client = reqwest::Client::new();
    hetzner
        .create_record(&client, "example.com", &record)
        .await
        .expect("idempotent create should succeed without a write");
}

/// MX records encode priority into the value (`"10 mail.example.com"`).
#[tokio::test]
async fn hetzner_create_mx_encodes_priority_in_value() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/zones/example.com/rrsets"))
        .and(query_param("name", "@"))
        .and(query_param("type", "MX"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "rrsets": [] })))
        .mount(&mock)
        .await;

    Mock::given(method("POST"))
        .and(path("/zones/example.com/rrsets"))
        .and(body_partial_json(serde_json::json!({
            "type": "MX",
            "records": [{ "value": "10 mail.example.com" }],
        })))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&mock)
        .await;

    let hetzner = HetznerDns::with_base_url("test-token".into(), mock.uri());
    let record = DnsRecord {
        record_type: "MX".into(),
        name: "@".into(),
        value: "mail.example.com".into(),
        ttl: 300,
        priority: Some(10),
    };
    let client = reqwest::Client::new();
    hetzner
        .create_record(&client, "example.com", &record)
        .await
        .expect("MX create failed");
}

/// `find_records` lists the RRset and flattens its record values, tagging each
/// with the requested name/type (the query already filtered to them).
#[tokio::test]
async fn hetzner_find_records_maps_rrset_values() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/zones/example.com/rrsets"))
        .and(query_param("name", "@"))
        .and(query_param("type", "TXT"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "rrsets": [{
                "id": "@/TXT",
                "name": "@",
                "type": "TXT",
                "ttl": 300,
                // Hetzner returns TXT zone-file-quoted; find_records unwraps it
                // back to the bare seam value the orchestrator compares.
                "records": [
                    { "value": "\"v=spf1 -all\"" },
                    { "value": "\"second-value\"" },
                ],
            }],
        })))
        .mount(&mock)
        .await;

    let hetzner = HetznerDns::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let found = hetzner
        .find_records(&client, "example.com", "@", "TXT")
        .await
        .expect("find_records failed");

    assert_eq!(found.len(), 2);
    assert_eq!(found[0].record_type, "TXT");
    assert_eq!(found[0].name, "@");
    assert_eq!(found[0].value, "v=spf1 -all");
    assert_eq!(found[0].ttl, 300);
    assert_eq!(found[1].value, "second-value");
}

/// `find_records` returns an empty Vec when the RRset does not exist.
#[tokio::test]
async fn hetzner_find_records_empty_when_absent() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/zones/example.com/rrsets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "rrsets": [] })))
        .mount(&mock)
        .await;

    let hetzner = HetznerDns::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let found = hetzner
        .find_records(&client, "example.com", "www", "A")
        .await
        .expect("find_records failed");
    assert!(found.is_empty());
}

/// `delete_record` removes a single value via the `remove_records` action.
/// Uses an apex `@` name to also exercise `@` in the request path.
#[tokio::test]
async fn hetzner_delete_record_posts_remove_action() {
    let mock = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path(
            "/zones/example.com/rrsets/_acme-challenge/TXT/actions/remove_records",
        ))
        .and(header("authorization", "Bearer test-token"))
        .and(body_partial_json(serde_json::json!({
            // TXT removal matches the stored zone-file-quoted value.
            "records": [{ "value": "\"token-to-tear-down\"" }],
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "action": { "id": 2, "status": "success" },
        })))
        .expect(1)
        .mount(&mock)
        .await;

    let hetzner = HetznerDns::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    hetzner
        .delete_record(
            &client,
            "example.com",
            "_acme-challenge",
            "TXT",
            "token-to-tear-down",
        )
        .await
        .expect("delete_record failed");
}

// ---------------------------------------------------------------------------
// delete_record — value-based teardown of `_acme-challenge` TXT (and any
// managed record). Each provider maps the value-based contract onto its own
// API: Cloudflare/Porkbun resolve a record id then delete it; Gandi reduces
// the RRset (DELETE if empty, PUT the survivors otherwise); Namecheap rewrites
// the host set minus the target. An already-absent record is a no-op success.
// ---------------------------------------------------------------------------

/// The owner name as it reaches an adapter, per the seam's contract
/// (`DnsProvider::record_names_relative_to_zone`): **zone-relative**, because
/// the caller has already relativized. Every provider except Cloudflare.
const ACME_NAME: &str = "_acme-challenge";
/// The Cloudflare form of the same owner: Cloudflare's v4 API consumes and
/// returns fully-qualified names, so it is the one adapter that returns `false`
/// and receives the FQDN untouched.
const ACME_NAME_QUALIFIED: &str = "_acme-challenge.example.com";
const ACME_VALUE: &str = "token-abc";

/// Cloudflare deletes by record id: list `(name, type)`, match on content,
/// then `DELETE /zones/{zone}/dns_records/{id}`.
#[tokio::test]
async fn cloudflare_delete_resolves_id_then_deletes() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/zones/zone1/dns_records"))
        .and(query_param("name", ACME_NAME_QUALIFIED))
        .and(query_param("type", "TXT"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"success":true,"result":[{"id":"rec1","type":"TXT","name":"_acme-challenge.example.com","content":"token-abc","ttl":120}],"errors":[]}"#,
        ))
        .mount(&mock)
        .await;

    Mock::given(method("DELETE"))
        .and(path("/zones/zone1/dns_records/rec1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"success":true,"errors":[]}"#))
        .expect(1)
        .mount(&mock)
        .await;

    let cf = Cloudflare::with_base_url("tok".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = cf
        .delete_record(&client, "zone1", ACME_NAME_QUALIFIED, "TXT", ACME_VALUE)
        .await;
    assert!(result.is_ok(), "delete failed: {:?}", result);
}

/// An already-absent record is a no-op success — no DELETE is issued (the
/// missing DELETE mock would 404 and fail the call if one were attempted).
#[tokio::test]
async fn cloudflare_delete_absent_is_noop() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/zones/zone1/dns_records"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"success":true,"result":[],"errors":[]}"#),
        )
        .mount(&mock)
        .await;

    let cf = Cloudflare::with_base_url("tok".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = cf
        .delete_record(&client, "zone1", ACME_NAME_QUALIFIED, "TXT", ACME_VALUE)
        .await;
    assert!(
        result.is_ok(),
        "absent delete should be a no-op: {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// TLSA (DANE) create — the floor-MX `_25._tcp.mail.<primary>` pin. Cloudflare
// needs a structured `data` object (a `content` string is rejected); the other
// three managed providers pass the `"<usage> <selector> <matching> <hex>"`
// RDATA through verbatim (Namecheap has no TLSA type — its rejection is a unit
// test in the adapter). One source for the per-provider TLSA decision (Slice
// 5b.5; research 2026-06-07).
// ---------------------------------------------------------------------------

/// Zone-relative owner (Gandi, Porkbun) — see `ACME_NAME`.
const TLSA_NAME: &str = "_25._tcp.mail";
/// Fully-qualified owner (Cloudflare) — see `ACME_NAME_QUALIFIED`.
const TLSA_NAME_QUALIFIED: &str = "_25._tcp.mail.example.com";
const TLSA_RDATA: &str = "3 1 1 abcdef0123456789";

fn tlsa_record() -> DnsRecord {
    DnsRecord {
        record_type: "TLSA".into(),
        name: TLSA_NAME.into(),
        value: TLSA_RDATA.into(),
        ttl: 3600,
        priority: None,
    }
}

/// The same record as an adapter that opted out of relativization sees it.
fn tlsa_record_qualified() -> DnsRecord {
    DnsRecord {
        name: TLSA_NAME_QUALIFIED.into(),
        ..tlsa_record()
    }
}

/// Cloudflare TLSA create posts a structured `data:{usage,selector,
/// matching_type,certificate}` object parsed from the RDATA — NOT a `content`
/// string (which Cloudflare rejects for TLSA).
#[tokio::test]
async fn cloudflare_tlsa_create_posts_structured_data() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/zones/zone1/dns_records"))
        .and(body_partial_json(serde_json::json!({
            "type": "TLSA",
            "name": TLSA_NAME_QUALIFIED,
            "data": {
                "usage": 3, "selector": 1, "matching_type": 1,
                "certificate": "abcdef0123456789",
            },
        })))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"success":true,"errors":[]}"#))
        .expect(1)
        .mount(&mock)
        .await;
    let cf = Cloudflare::with_base_url("tok".into(), mock.uri());
    let result = cf
        .create_record(&reqwest::Client::new(), "zone1", &tlsa_record_qualified())
        .await;
    assert!(result.is_ok(), "TLSA create failed: {:?}", result);
}

/// Gandi TLSA passes the RDATA through as a single `rrset_values` element (the
/// same array contract as every other type, priority-less).
#[tokio::test]
async fn gandi_tlsa_passes_rdata_in_rrset_values() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/domain/domains/example.com/records"))
        .and(body_partial_json(serde_json::json!({
            "rrset_type": "TLSA",
            "rrset_name": TLSA_NAME,
            "rrset_values": [TLSA_RDATA],
        })))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&mock)
        .await;
    let gandi = Gandi::with_base_url("test-token".into(), mock.uri());
    let result = gandi
        .create_record(&reqwest::Client::new(), "example.com", &tlsa_record())
        .await;
    assert!(result.is_ok(), "TLSA create failed: {:?}", result);
}

/// Porkbun TLSA passes the RDATA through as `content`, priority-less.
#[tokio::test]
async fn porkbun_tlsa_passes_rdata_in_content() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/dns/create/example.com"))
        .and(body_partial_json(serde_json::json!({
            "type": "TLSA",
            "name": TLSA_NAME,
            "content": TLSA_RDATA,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"status":"SUCCESS"}"#))
        .expect(1)
        .mount(&mock)
        .await;
    let pb = Porkbun::with_base_url("k".into(), "s".into(), mock.uri());
    let result = pb
        .create_record(&reqwest::Client::new(), "example.com", &tlsa_record())
        .await;
    assert!(result.is_ok(), "TLSA create failed: {:?}", result);
}

/// **Porkbun spells the apex as a BLANK name, never `@`.** Its API docs say the
/// create `name` is the "Subdomain for the record (e.g. 'www', '*' for wildcard,
/// blank for root)" and `retrieveByNameType`'s `subdomain` is "Omit or leave
/// empty for root domain records" — `@` is not an accepted spelling anywhere.
///
/// The orchestrator relativizes every name through `dns_record_name` before the
/// adapter sees it, and that emits the *shared* apex sentinel `@`. Porkbun's own
/// `String::new()` apex branch keys on `name == zone_id`, which a pre-relativized
/// `@` never matches — so the branch was unreachable and `@` went out on the
/// wire as a literal label, creating apex records at `@.example.com`.
#[tokio::test]
async fn porkbun_create_apex_sends_a_blank_name() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/dns/create/example.com"))
        .and(body_partial_json(serde_json::json!({
            "type": "A",
            "name": "",
            "content": "1.2.3.4",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"status":"SUCCESS"}"#))
        .expect(1)
        .mount(&mock)
        .await;

    let pb = Porkbun::with_base_url("k".into(), "s".into(), mock.uri());
    let record = DnsRecord {
        record_type: "A".into(),
        name: "@".into(),
        value: "1.2.3.4".into(),
        ttl: 300,
        priority: None,
    };
    pb.create_record(&reqwest::Client::new(), "example.com", &record)
        .await
        .expect("apex create failed");
}

/// The find half of the same contract: a blank subdomain means the URL drops the
/// segment entirely (`/dns/retrieveByNameType/{zone}/{type}`), which is how
/// Porkbun addresses the root. A literal `@` segment addresses a label named `@`.
#[tokio::test]
async fn porkbun_find_apex_omits_the_subdomain_segment() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/dns/retrieveByNameType/example.com/TXT"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"status":"SUCCESS","records":[{"id":"1","name":"example.com","type":"TXT","content":"v=spf1 -all","ttl":"300"}]}"#,
        ))
        .expect(1)
        .mount(&mock)
        .await;

    let pb = Porkbun::with_base_url("k".into(), "s".into(), mock.uri());
    let found = pb
        .find_records(&reqwest::Client::new(), "example.com", "@", "TXT")
        .await
        .expect("apex find failed");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].value, "v=spf1 -all");
}

// ---------------------------------------------------------------------------
// The seam's owner-name contract: `name` arrives ALREADY zone-relative
// ---------------------------------------------------------------------------
//
// `DnsProvider::record_names_relative_to_zone` makes the *caller* relativize
// (`orchestrator::dns_record_name`, and `fauna-client-dns`'s `owner_name` for
// the managed-DNS path). Adapters consume the result verbatim; they must not
// re-derive it.
//
// This is not a style preference — it is the only contract the seam can
// express. Adapters receive `zone_id`, never the zone *name*, and Hetzner's
// `zone_id` is an opaque integer, so a Hetzner adapter physically cannot strip
// a `.<zone>` suffix. Gandi/Namecheap/Porkbun could only ever *appear* to,
// because those three happen to use the zone name as their id.
//
// Defensive re-relativization is therefore actively harmful: it makes a caller
// that forgets to relativize look correct on three providers while silently
// breaking the two that cannot defend. That exact asymmetry caused a
// production outage on 2026-07-24 — `RpcDnsProvider` passed fully-qualified
// owners, and Hetzner's RRset API stored every record at a doubled name
// (`_acme-challenge.example.com.example.com.`), API-visible but never resolvable,
// failing ACME twice (`docs/goal/architecture/nest/tls-certificates.md`
// § the 2026-07-24 dual defect).
//
// The tests below pin the other direction of the same contract: a *legitimate*
// relative owner that happens to end with the zone name must reach the wire
// whole. A suffix strip corrupts it — `www.example.com` within zone
// `example.com` addresses the host `www.example.com.example.com`, and stripping
// silently retargets the write at `www.example.com`.

/// Gandi: the rrset path segment is the caller's name verbatim.
#[tokio::test]
async fn gandi_does_not_re_relativize_a_relative_owner_ending_in_the_zone() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/domain/domains/example.com/records/www.example.com/A"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"rrset_name":"www.example.com","rrset_type":"A","rrset_values":["1.2.3.4"],"rrset_ttl":300}"#,
        ))
        .expect(1)
        .mount(&mock)
        .await;

    let gandi = Gandi::with_base_url("test-token".into(), mock.uri());
    let found = gandi
        .find_records(
            &reqwest::Client::new(),
            "example.com",
            "www.example.com",
            "A",
        )
        .await
        .expect("find_records");
    assert_eq!(found.len(), 1, "the adapter stripped the owner name");
}

/// Namecheap: the host filter matches the caller's name verbatim.
#[tokio::test]
async fn namecheap_does_not_re_relativize_a_relative_owner_ending_in_the_zone() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.dns.getHosts"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<?xml version="1.0" encoding="utf-8"?>
<ApiResponse Status="OK">
  <CommandResponse Type="namecheap.domains.dns.getHosts">
    <DomainDNSGetHostsResult Domain="example.com" IsUsingOurDNS="true">
      <host HostId="9" Name="www.example.com" Type="A" Address="1.2.3.4" MXPref="0" TTL="300" />
    </DomainDNSGetHostsResult>
  </CommandResponse>
</ApiResponse>"#,
        ))
        .mount(&mock)
        .await;

    let namecheap = Namecheap::with_base_url("u".into(), "k".into(), mock.uri());
    let found = namecheap
        .find_records(
            &reqwest::Client::new(),
            "example.com",
            "www.example.com",
            "A",
        )
        .await
        .expect("find_records");
    assert_eq!(found.len(), 1, "the adapter stripped the owner name");
}

/// Porkbun: the `retrieveByNameType` subdomain segment is the caller's name
/// verbatim (the apex sentinel translation is the *only* transform it applies).
#[tokio::test]
async fn porkbun_does_not_re_relativize_a_relative_owner_ending_in_the_zone() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/dns/retrieveByNameType/example.com/A/www.example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"status":"SUCCESS","records":[{"id":"1","name":"www.example.com.example.com","type":"A","content":"1.2.3.4","ttl":"300"}]}"#,
        ))
        .expect(1)
        .mount(&mock)
        .await;

    let pb = Porkbun::with_base_url("k".into(), "s".into(), mock.uri());
    let found = pb
        .find_records(
            &reqwest::Client::new(),
            "example.com",
            "www.example.com",
            "A",
        )
        .await
        .expect("find_records");
    assert_eq!(found.len(), 1, "the adapter stripped the owner name");
}

/// Porkbun resolves the id via `retrieveByNameType`, matches on content, then
/// `POST /dns/delete/{zone}/{id}`.
#[tokio::test]
async fn porkbun_delete_resolves_id_then_deletes() {
    let mock = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/dns/retrieveByNameType/example.com/TXT/_acme-challenge"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"status":"SUCCESS","records":[{"id":"123","type":"TXT","name":"_acme-challenge.example.com","content":"token-abc","ttl":"120"}]}"#,
        ))
        .mount(&mock)
        .await;

    Mock::given(method("POST"))
        .and(path("/dns/delete/example.com/123"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"status":"SUCCESS"}"#))
        .expect(1)
        .mount(&mock)
        .await;

    let pb = Porkbun::with_base_url("k".into(), "s".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = pb
        .delete_record(&client, "example.com", ACME_NAME, "TXT", ACME_VALUE)
        .await;
    assert!(result.is_ok(), "delete failed: {:?}", result);
}

/// Gandi with a single-value RRset deletes the whole RRset.
#[tokio::test]
async fn gandi_delete_single_value_drops_rrset() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/domain/domains/example.com/records/_acme-challenge/TXT"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"rrset_name":"_acme-challenge","rrset_type":"TXT","rrset_values":["token-abc"],"rrset_ttl":120}"#,
        ))
        .mount(&mock)
        .await;

    Mock::given(method("DELETE"))
        .and(path(
            "/domain/domains/example.com/records/_acme-challenge/TXT",
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&mock)
        .await;

    let gandi = Gandi::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = gandi
        .delete_record(&client, "example.com", ACME_NAME, "TXT", ACME_VALUE)
        .await;
    assert!(result.is_ok(), "delete failed: {:?}", result);
}

/// Gandi with a multi-value RRset PUTs back only the survivors — a whole-RRset
/// DELETE would erase the unrelated value.
#[tokio::test]
async fn gandi_delete_one_of_many_puts_remainder() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/domain/domains/example.com/records/_acme-challenge/TXT"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"rrset_name":"_acme-challenge","rrset_type":"TXT","rrset_values":["token-abc","keepme"],"rrset_ttl":120}"#,
        ))
        .mount(&mock)
        .await;

    Mock::given(method("PUT"))
        .and(path(
            "/domain/domains/example.com/records/_acme-challenge/TXT",
        ))
        .and(body_partial_json(serde_json::json!({
            "rrset_values": ["keepme"],
            "rrset_ttl": 120,
        })))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&mock)
        .await;

    let gandi = Gandi::with_base_url("test-token".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = gandi
        .delete_record(&client, "example.com", ACME_NAME, "TXT", ACME_VALUE)
        .await;
    assert!(result.is_ok(), "put-remainder delete failed: {:?}", result);
}

/// Namecheap has no per-record delete: it reads every host and `setHosts` the
/// survivors. The `_acme-challenge` host is listed FIRST in the fixture, so a
/// failure to drop it would put it at `HostName1` and miss the mock — making
/// this a strict assertion that the target was removed.
#[tokio::test]
async fn namecheap_delete_rewrites_host_set_without_target() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.dns.getHosts"))
        .respond_with(ResponseTemplate::new(200).set_body_string(TWO_HOSTS_ACME_FIRST_XML))
        .mount(&mock)
        .await;

    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.dns.setHosts"))
        .and(query_param("HostName1", "@"))
        .and(query_param("RecordType1", "A"))
        .and(query_param("Address1", "1.2.3.4"))
        .and(query_param("TTL1", "300"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SET_HOSTS_OK_XML))
        .expect(1)
        .mount(&mock)
        .await;

    let namecheap = Namecheap::with_base_url("u".into(), "k".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = namecheap
        .delete_record(&client, "example.com", ACME_NAME, "TXT", ACME_VALUE)
        .await;
    assert!(result.is_ok(), "delete failed: {:?}", result);
}

/// Namecheap delete of an absent value rewrites nothing — no `setHosts` call
/// (the missing setHosts mock would 404 if one were attempted).
#[tokio::test]
async fn namecheap_delete_absent_skips_rewrite() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.dns.getHosts"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ONE_HOST_A_XML))
        .mount(&mock)
        .await;

    let namecheap = Namecheap::with_base_url("u".into(), "k".into(), mock.uri());
    let client = reqwest::Client::new();
    let result = namecheap
        .delete_record(&client, "example.com", ACME_NAME, "TXT", ACME_VALUE)
        .await;
    assert!(
        result.is_ok(),
        "absent delete should skip the rewrite: {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// Fixtures (inline — short enough that separate files would be more friction
// than they're worth at this scope)
// ---------------------------------------------------------------------------

const TWO_HOSTS_ACME_FIRST_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<ApiResponse Status="OK">
  <CommandResponse Type="namecheap.domains.dns.getHosts">
    <DomainDNSGetHostsResult Domain="example.com" IsUsingOurDNS="true">
      <host HostId="1" Name="_acme-challenge" Type="TXT" Address="token-abc" MXPref="0" TTL="120" />
      <host HostId="2" Name="@" Type="A" Address="1.2.3.4" MXPref="0" TTL="300" />
    </DomainDNSGetHostsResult>
  </CommandResponse>
</ApiResponse>"#;

const ONE_HOST_A_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<ApiResponse Status="OK">
  <CommandResponse Type="namecheap.domains.dns.getHosts">
    <DomainDNSGetHostsResult Domain="example.com" IsUsingOurDNS="true">
      <host HostId="2" Name="@" Type="A" Address="1.2.3.4" MXPref="0" TTL="300" />
    </DomainDNSGetHostsResult>
  </CommandResponse>
</ApiResponse>"#;

const EMPTY_HOSTS_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<ApiResponse Status="OK">
  <CommandResponse Type="namecheap.domains.dns.getHosts">
    <DomainDNSGetHostsResult Domain="example.com" IsUsingOurDNS="true">
    </DomainDNSGetHostsResult>
  </CommandResponse>
</ApiResponse>"#;

const SET_HOSTS_OK_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<ApiResponse Status="OK">
  <CommandResponse Type="namecheap.domains.dns.setHosts">
    <DomainDNSSetHostsResult Domain="example.com" IsSuccess="true">
    </DomainDNSSetHostsResult>
  </CommandResponse>
</ApiResponse>"#;

// ---------------------------------------------------------------------------
// Namecheap: API-level errors arrive as HTTP 200
// ---------------------------------------------------------------------------
//
// Namecheap reports API-level failures with **HTTP 200** and an
// `<ApiResponse Status="ERROR">` body — verified empirically against the live
// endpoint 2026-07-22 (an empty `ClientIp` yields `1010105`, a bad key
// `1011102`, both under HTTP 200). An adapter that only checks the HTTP status
// therefore reads every API failure as a success. These tests pin the three
// consequences that matters most.

/// The allowlist rejection. Namecheap echoes the **source** address it actually
/// observed, which is the only reflector this code has: the wizard can name the
/// exact IP the user must allowlist without any STUN server.
const INVALID_REQUEST_IP_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<ApiResponse Status="ERROR" xmlns="http://api.namecheap.com/xml.response">
  <Errors>
    <Error Number="1011150">Invalid request IP: 203.0.113.7</Error>
  </Errors>
  <RequestedCommand />
</ApiResponse>"#;

/// A `setHosts` that failed at the API level must NOT be reported as a
/// successful write. This is the most damaging shape of the bug: the wizard
/// believes the zone was published when nothing was written.
#[tokio::test]
async fn namecheap_failed_set_hosts_is_not_reported_as_success() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.dns.getHosts"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EMPTY_HOSTS_XML))
        .mount(&mock)
        .await;

    // The write fails at the API level — but with HTTP 200.
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.dns.setHosts"))
        .respond_with(ResponseTemplate::new(200).set_body_string(INVALID_REQUEST_IP_XML))
        .mount(&mock)
        .await;

    let namecheap = Namecheap::with_base_url("u".into(), "k".into(), mock.uri());
    let record = DnsRecord {
        record_type: "TXT".into(),
        name: ACME_NAME.into(),
        value: "token".into(),
        ttl: 300,
        priority: None,
    };

    let result = namecheap
        .create_record(&reqwest::Client::new(), "example.com", &record)
        .await;

    assert!(
        result.is_err(),
        "a setHosts rejected by Namecheap must surface as an error, not a silent \
         successful write — the wizard would otherwise believe DNS was published"
    );
}

/// `verify()` must not turn an API-level rejection into "you have no domains".
/// An empty zone list is indistinguishable from a real empty account, so the
/// user is told the wrong thing and has nothing to act on.
#[tokio::test]
async fn namecheap_verify_surfaces_the_ip_to_allowlist() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.getList"))
        .respond_with(ResponseTemplate::new(200).set_body_string(INVALID_REQUEST_IP_XML))
        .mount(&mock)
        .await;

    let namecheap = Namecheap::with_base_url("u".into(), "k".into(), mock.uri());
    let err = namecheap
        .verify(&reqwest::Client::new())
        .await
        .expect_err("an allowlist rejection must not read as an empty domain list");

    let msg = err.to_string();
    assert!(
        msg.contains("203.0.113.7"),
        "the error must name the exact IP Namecheap observed so the user can \
         allowlist it — got: {msg}"
    );
}

/// The self-heal: Namecheap's rejection carries the address it saw, so the
/// adapter retries once with that value. This rescues the case where the user
/// HAS allowlisted their address but the request carried a placeholder — the
/// otherwise-unbreakable loop of "allowlist the IP you already allowlisted".
#[tokio::test]
async fn namecheap_retries_once_with_the_ip_namecheap_echoed() {
    let mock = MockServer::start().await;

    // A request carrying the observed address is accepted...
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.getList"))
        .and(query_param("ClientIp", "203.0.113.7"))
        .respond_with(ResponseTemplate::new(200).set_body_string(DOMAIN_LIST_OK_XML))
        .expect(1)
        .mount(&mock)
        .await;

    // ...anything else is rejected with the echo.
    Mock::given(method("GET"))
        .and(query_param("Command", "namecheap.domains.getList"))
        .respond_with(ResponseTemplate::new(200).set_body_string(INVALID_REQUEST_IP_XML))
        .mount(&mock)
        .await;

    let namecheap = Namecheap::with_base_url("u".into(), "k".into(), mock.uri());
    let zones = namecheap
        .verify(&reqwest::Client::new())
        .await
        .expect("the retry carrying Namecheap's echoed IP must succeed");

    assert_eq!(zones.len(), 1);
    assert_eq!(zones[0].name, "example.com");
}

const DOMAIN_LIST_OK_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<ApiResponse Status="OK" xmlns="http://api.namecheap.com/xml.response">
  <CommandResponse Type="namecheap.domains.getList">
    <DomainGetListResult>
      <Domain Name="example.com" User="u" IsExpired="false"/>
    </DomainGetListResult>
  </CommandResponse>
</ApiResponse>"#;
