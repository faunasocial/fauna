//! TLSRPT aggregator + report serialization — docs/goal/behavior/smtp-server.md
//! § TLSRPT outbound reporter. Implements RFC 8460 §4.4 JSON shape.

#![cfg(feature = "outbound")]

use fauna_mail::outbound::mta_sts::{FetchedPolicy, MtaStsLookup, MtaStsMode, MtaStsPolicy};
use fauna_mail::outbound::tlsrpt::{
    AttemptOutcome, CachingTlsrptPolicyFetcher, DomainSnapshot, FailureSnapshot,
    NullOutboundTlsrptRecorder, NullTlsrptPolicyFetcher, OutboundTlsrptRecorder, PolicySnapshot,
    ReportDispatchContext, ReportEnvelope, ReportTransport, TLSRPT_POLICY_CACHE_TTL_SECS,
    TlsrptAggregator, TlsrptPolicy, TlsrptPolicyFetcher, build_mailto_mime, format_utc_date,
    gzip_bytes, parse_rua_uris, policy_for_attempt, populate_transports, rfc2822_utc,
    sample_jitter_offset_secs, seconds_until_next_utc_midnight,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

fn enforce_lookup(mx: &str) -> MtaStsLookup {
    MtaStsLookup::Found(FetchedPolicy {
        id: "id-test".to_string(),
        policy: MtaStsPolicy {
            version: "STSv1".to_string(),
            mode: MtaStsMode::Enforce,
            mx: vec![mx.to_string()],
            max_age_secs: 86_400,
        },
    })
}

fn pol(domain: &str) -> TlsrptPolicy {
    TlsrptPolicy {
        policy_type: "sts".into(),
        policy_string: vec!["version: STSv1".into(), "mode: enforce".into()],
        policy_domain: domain.into(),
    }
}

#[test]
fn aggregator_buckets_by_failure_type() {
    let mut agg = TlsrptAggregator::default();
    let domain = "gmail.com".to_string();

    // 5 successful deliveries.
    for _ in 0..5 {
        agg.record(AttemptOutcome {
            recipient_domain: domain.clone(),
            policy: pol(&domain),
            failure_type: None,
        });
    }
    // 2 STARTTLS-not-supported failures.
    for _ in 0..2 {
        agg.record(AttemptOutcome {
            recipient_domain: domain.clone(),
            policy: pol(&domain),
            failure_type: Some("starttls-not-supported".into()),
        });
    }

    let env = agg.emit_report(&domain, "fauna.example", "report-1", "2026-05-14");
    let json: serde_json::Value = serde_json::from_slice(&env.json).unwrap();

    assert_eq!(json["organization-name"], "fauna.example");
    assert_eq!(json["report-id"], "report-1");
    let policies = json["policies"].as_array().unwrap();
    // One policy bucket — same TlsrptPolicy across all outcomes.
    assert_eq!(policies.len(), 1);
    let summary = &policies[0]["summary"];
    assert_eq!(summary["total-successful-session-count"], 5);
    assert_eq!(summary["total-failure-session-count"], 2);

    let failure_details = policies[0]["failure-details"].as_array().unwrap();
    let starttls_bucket = failure_details
        .iter()
        .find(|b| b["result-type"] == "starttls-not-supported")
        .expect("starttls-not-supported bucket");
    assert_eq!(starttls_bucket["failed-session-count"], 2);
}

#[test]
fn report_for_unrecorded_domain_has_zero_counts() {
    let agg = TlsrptAggregator::default();
    let env = agg.emit_report("yahoo.com", "fauna.example", "r-2", "2026-05-14");
    let json: serde_json::Value = serde_json::from_slice(&env.json).unwrap();
    let policies = json["policies"].as_array().unwrap();
    assert!(
        policies.is_empty(),
        "no recorded outcomes → no policy buckets"
    );
}

#[test]
fn report_envelope_includes_mailto_and_https_transports() {
    let mut agg = TlsrptAggregator::default();
    let domain = "gmail.com".to_string();
    agg.record(AttemptOutcome {
        recipient_domain: domain.clone(),
        policy: pol(&domain),
        failure_type: None,
    });

    let mut env = agg.emit_report(&domain, "fauna.example", "r-3", "2026-05-14");
    env.transports = vec![
        ReportTransport::Mailto {
            rcpt: "tlsrpt@example.com".into(),
            mime_message: Vec::new(),
        },
        ReportTransport::Https {
            uri: "https://tlsrpt.example.com/upload".into(),
            body: Vec::new(),
            content_encoding: "gzip",
        },
    ];

    let names: Vec<&str> = env
        .transports
        .iter()
        .map(|t| match t {
            ReportTransport::Mailto { .. } => "mailto",
            ReportTransport::Https { .. } => "https",
        })
        .collect();
    assert_eq!(names, vec!["mailto", "https"]);
}

#[test]
fn parse_rua_extracts_mailto_and_https() {
    let txt = "v=TLSRPTv1; rua=mailto:tlsrpt@example.com,https://reports.example.com/tlsrpt";
    let uris = parse_rua_uris(txt).expect("parse failed");
    assert_eq!(uris.len(), 2);
    assert_eq!(uris[0], "mailto:tlsrpt@example.com");
    assert_eq!(uris[1], "https://reports.example.com/tlsrpt");
}

#[test]
fn parse_rua_ignores_non_tlsrpt_records() {
    assert!(parse_rua_uris("v=DMARC1; p=none").is_none());
    assert!(parse_rua_uris("not a TLSRPT record").is_none());
}

#[test]
fn report_summary_counts_match_recorded_outcomes() {
    let mut agg = TlsrptAggregator::default();
    let domain = "gmail.com".to_string();
    for _ in 0..10 {
        agg.record(AttemptOutcome {
            recipient_domain: domain.clone(),
            policy: pol(&domain),
            failure_type: None,
        });
    }
    for _ in 0..3 {
        agg.record(AttemptOutcome {
            recipient_domain: domain.clone(),
            policy: pol(&domain),
            failure_type: Some("tlsa-invalid".into()),
        });
    }
    let env = agg.emit_report(&domain, "fauna.example", "r-4", "2026-05-14");
    let json: serde_json::Value = serde_json::from_slice(&env.json).unwrap();
    let summary = &json["policies"][0]["summary"];
    assert_eq!(summary["total-successful-session-count"], 10);
    assert_eq!(summary["total-failure-session-count"], 3);
}

#[test]
fn dump_snapshot_orders_domains_policies_and_failures() {
    let mut agg = TlsrptAggregator::default();
    // gmail.com: one TLSA policy, one success + one validation-failure.
    let gmail_policy = TlsrptPolicy {
        policy_type: "tlsa".into(),
        policy_string: vec!["3 1 1 deadbeef".into()],
        policy_domain: "mx.gmail.com".into(),
    };
    agg.record(AttemptOutcome {
        recipient_domain: "gmail.com".into(),
        policy: gmail_policy.clone(),
        failure_type: None,
    });
    agg.record(AttemptOutcome {
        recipient_domain: "gmail.com".into(),
        policy: gmail_policy.clone(),
        failure_type: Some("validation-failure".into()),
    });
    // aol.com: no-policy-found, one failure with two distinct result-types.
    let aol_policy = TlsrptPolicy {
        policy_type: "no-policy-found".into(),
        policy_string: vec![],
        policy_domain: "aol.com".into(),
    };
    agg.record(AttemptOutcome {
        recipient_domain: "aol.com".into(),
        policy: aol_policy.clone(),
        failure_type: Some("starttls-not-supported".into()),
    });
    agg.record(AttemptOutcome {
        recipient_domain: "aol.com".into(),
        policy: aol_policy.clone(),
        failure_type: Some("certificate-host-mismatch".into()),
    });

    let snap: Vec<DomainSnapshot> = agg.dump_snapshot();

    // Domains sorted alphabetically: aol < gmail.
    assert_eq!(snap.len(), 2);
    assert_eq!(snap[0].domain, "aol.com");
    assert_eq!(snap[1].domain, "gmail.com");

    // aol bucket: one policy, two failure result-types sorted alphabetically.
    assert_eq!(snap[0].policies.len(), 1);
    let aol = &snap[0].policies[0];
    assert_eq!(aol.policy_type, "no-policy-found");
    assert_eq!(aol.policy_domain, "aol.com");
    assert_eq!(aol.total_success, 0);
    assert_eq!(aol.total_failure, 2);
    assert_eq!(
        aol.failures,
        vec![
            FailureSnapshot {
                result_type: "certificate-host-mismatch".into(),
                count: 1
            },
            FailureSnapshot {
                result_type: "starttls-not-supported".into(),
                count: 1
            },
        ]
    );

    // gmail bucket: 1 success + 1 failure.
    let gmail: &PolicySnapshot = &snap[1].policies[0];
    assert_eq!(gmail.policy_type, "tlsa");
    assert_eq!(gmail.policy_string, vec!["3 1 1 deadbeef".to_string()]);
    assert_eq!(gmail.policy_domain, "mx.gmail.com");
    assert_eq!(gmail.total_success, 1);
    assert_eq!(gmail.total_failure, 1);
    assert_eq!(gmail.failures.len(), 1);
    assert_eq!(gmail.failures[0].result_type, "validation-failure");
    assert_eq!(gmail.failures[0].count, 1);
}

#[test]
fn clear_empties_all_buckets() {
    let mut agg = TlsrptAggregator::default();
    agg.record(AttemptOutcome {
        recipient_domain: "gmail.com".into(),
        policy: pol("gmail.com"),
        failure_type: None,
    });
    agg.record(AttemptOutcome {
        recipient_domain: "yahoo.com".into(),
        policy: pol("yahoo.com"),
        failure_type: Some("tlsa-invalid".into()),
    });
    assert_eq!(agg.recorded_domains().len(), 2);

    agg.clear();

    assert!(agg.recorded_domains().is_empty());
    // emit_report on a cleared domain has no policy buckets.
    let env = agg.emit_report("gmail.com", "fauna.example", "r-cleared", "2026-05-14");
    let json: serde_json::Value = serde_json::from_slice(&env.json).unwrap();
    assert!(json["policies"].as_array().unwrap().is_empty());
}

#[test]
fn jitter_offset_in_range() {
    // Per-domain 1-hour jitter to avoid stampeding at 00:00 UTC.
    for _ in 0..1000 {
        let off = sample_jitter_offset_secs(3600);
        assert!((0..3600).contains(&off));
    }
}

fn sample_ctx<'a>() -> ReportDispatchContext<'a> {
    ReportDispatchContext {
        our_domain: "fauna.example",
        recipient_domain: "gmail.com",
        report_id: "20260514.42@fauna.example",
        report_date: "2026-05-14",
        message_id: "tlsrpt-20260514-42",
        rfc2822_date: "Thu, 14 May 2026 00:30:00 +0000",
        boundary: "tlsrpt-boundary-deadbeef",
    }
}

#[test]
fn gzip_bytes_round_trips_through_flate2_decoder() {
    use flate2::read::GzDecoder;
    use std::io::Read;

    let payload = b"{\"organization-name\":\"fauna.example\",\"report-id\":\"r-1\"}";
    let gz = gzip_bytes(payload);
    // Gzip magic bytes: 1f 8b.
    assert_eq!(&gz[..2], &[0x1f, 0x8b], "missing gzip magic");
    let mut decoder = GzDecoder::new(&gz[..]);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).expect("decode gz");
    assert_eq!(out, payload);
}

#[test]
fn build_mailto_mime_includes_rfc8460_headers_and_attachment() {
    let ctx = sample_ctx();
    let gzipped = gzip_bytes(b"{\"organization-name\":\"fauna.example\"}");
    let mime = build_mailto_mime("tlsrpt@example.com", &ctx, &gzipped);
    let text = std::str::from_utf8(&mime).expect("MIME message is UTF-8 safe");

    // Spec line 419: "Subject: Report Domain: <recipient> Submitter: <our-domain> Report-ID: <uuid>".
    assert!(
        text.contains("Subject: Report Domain: gmail.com Submitter: fauna.example Report-ID: 20260514.42@fauna.example"),
        "Subject header missing or malformed: {text}"
    );
    assert!(
        text.contains("From: tlsrpt@fauna.example"),
        "From header missing"
    );
    assert!(text.contains("To: tlsrpt@example.com"), "To header missing");
    assert!(text.contains("MIME-Version: 1.0"), "MIME-Version missing");
    assert!(
        text.contains("Auto-Submitted: auto-generated"),
        "Auto-Submitted (RFC 8460 §5.3) missing"
    );
    assert!(
        text.contains("TLS-Report-Domain: gmail.com"),
        "TLS-Report-Domain (RFC 8460 §5.3) missing"
    );
    assert!(
        text.contains("TLS-Report-Submitter: fauna.example"),
        "TLS-Report-Submitter (RFC 8460 §5.3) missing"
    );

    // Multipart container per RFC 8460 §5.3 with the gzipped report as
    // the attached part.
    assert!(
        text.contains(r#"Content-Type: multipart/report; report-type="tlsrpt""#),
        "multipart/report wrapper missing: {text}"
    );
    assert!(
        text.contains("--tlsrpt-boundary-deadbeef"),
        "boundary not present"
    );
    assert!(
        text.contains("Content-Type: application/tlsrpt+gzip"),
        "report part Content-Type missing"
    );
    assert!(
        text.contains("Content-Transfer-Encoding: base64"),
        "report part must be base64-encoded for 7-bit SMTP transport"
    );
    // RFC 8460 §5.3 filename convention:
    // <submitter>!<recipient>!<start-secs>!<end-secs>!<report-id>.json.gz
    assert!(
        text.contains(r#"filename="fauna.example!gmail.com!"#),
        "RFC 8460 §5.3 filename convention missing"
    );
    assert!(text.contains(".json.gz"), "filename suffix missing");

    // Closing boundary marker.
    assert!(
        text.trim_end().ends_with("--tlsrpt-boundary-deadbeef--"),
        "closing boundary marker missing"
    );
}

#[test]
fn build_mailto_mime_base64_payload_decodes_to_gzipped_input() {
    use base64::Engine;

    let ctx = sample_ctx();
    let payload = b"{\"organization-name\":\"fauna.example\",\"data\":\"x\"}";
    let gzipped = gzip_bytes(payload);
    let mime = build_mailto_mime("tlsrpt@example.com", &ctx, &gzipped);
    let text = std::str::from_utf8(&mime).expect("MIME message is UTF-8");

    // Extract the base64 payload between the last "Content-Transfer-Encoding: base64\r\n\r\n"
    // and the closing boundary.
    let after_b64 = text
        .split("Content-Transfer-Encoding: base64")
        .nth(1)
        .expect("base64 part");
    let after_blank = after_b64
        .split_once("\r\n\r\n")
        .map(|(_, t)| t)
        .expect("blank line before body");
    let b64_body = after_blank
        .split("\r\n--")
        .next()
        .expect("body before closing boundary")
        .replace("\r\n", "");

    let decoded = base64::engine::general_purpose::STANDARD
        .decode(b64_body.trim())
        .expect("decode base64");
    assert_eq!(
        decoded, gzipped,
        "MIME base64 body must round-trip gzipped JSON"
    );
}

#[test]
fn populate_transports_routes_mailto_uri_to_mime_envelope() {
    let mut env = ReportEnvelope {
        json: b"{\"organization-name\":\"fauna.example\"}".to_vec(),
        transports: Vec::new(),
    };
    let ctx = sample_ctx();
    populate_transports(&mut env, &["mailto:tlsrpt@example.com".to_string()], &ctx);
    assert_eq!(env.transports.len(), 1);
    match &env.transports[0] {
        ReportTransport::Mailto { rcpt, mime_message } => {
            assert_eq!(rcpt, "tlsrpt@example.com");
            let s = std::str::from_utf8(mime_message).unwrap();
            assert!(s.contains("To: tlsrpt@example.com"));
            assert!(s.contains(
                "Subject: Report Domain: gmail.com Submitter: fauna.example Report-ID: 20260514.42@fauna.example"
            ));
        }
        other => panic!("expected Mailto transport, got {other:?}"),
    }
}

#[test]
fn populate_transports_routes_https_uri_to_gzipped_body() {
    use flate2::read::GzDecoder;
    use std::io::Read;

    let mut env = ReportEnvelope {
        json: b"{\"organization-name\":\"fauna.example\"}".to_vec(),
        transports: Vec::new(),
    };
    let ctx = sample_ctx();
    populate_transports(
        &mut env,
        &["https://tlsrpt.example.com/upload".to_string()],
        &ctx,
    );
    assert_eq!(env.transports.len(), 1);
    match &env.transports[0] {
        ReportTransport::Https {
            uri,
            body,
            content_encoding,
        } => {
            assert_eq!(uri, "https://tlsrpt.example.com/upload");
            assert_eq!(*content_encoding, "gzip");
            // Body must be the gzipped JSON.
            let mut decoder = GzDecoder::new(&body[..]);
            let mut out = Vec::new();
            decoder.read_to_end(&mut out).expect("decode gz");
            assert_eq!(out, env.json);
        }
        other => panic!("expected Https transport, got {other:?}"),
    }
}

#[test]
fn populate_transports_skips_unknown_schemes() {
    let mut env = ReportEnvelope {
        json: b"{}".to_vec(),
        transports: Vec::new(),
    };
    let ctx = sample_ctx();
    populate_transports(
        &mut env,
        &[
            "ftp://reports.example.com/tlsrpt".to_string(),
            "mailto:tlsrpt@example.com".to_string(),
            "http://insecure.example.com/upload".to_string(),
            "https://secure.example.com/upload".to_string(),
        ],
        &ctx,
    );
    // ftp + http skipped; mailto + https kept.
    assert_eq!(env.transports.len(), 2);
    let kinds: Vec<&str> = env
        .transports
        .iter()
        .map(|t| match t {
            ReportTransport::Mailto { .. } => "mailto",
            ReportTransport::Https { .. } => "https",
        })
        .collect();
    assert_eq!(kinds, vec!["mailto", "https"]);
}

#[test]
fn populate_transports_handles_mailto_with_query_params() {
    // RFC 6068 mailto: URIs may carry "?subject=...&body=..." — strip
    // them for the rcpt; we generate our own headers.
    let mut env = ReportEnvelope {
        json: b"{}".to_vec(),
        transports: Vec::new(),
    };
    let ctx = sample_ctx();
    populate_transports(
        &mut env,
        &["mailto:tlsrpt@example.com?subject=ignored".to_string()],
        &ctx,
    );
    assert_eq!(env.transports.len(), 1);
    match &env.transports[0] {
        ReportTransport::Mailto { rcpt, .. } => {
            assert_eq!(rcpt, "tlsrpt@example.com");
        }
        other => panic!("expected Mailto, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// aggregator-record hook in the delivery path.
//
// `policy_for_attempt` builds the `TlsrptPolicy` for a per-host TLS attempt;
// the recorder trait is what `fauna_bridge_smtp::outbound::deliver_to_host`
// holds onto so the production aggregator (constructed in
// `legacy_smtp_outbound::spawn`) sees real per-attempt outcomes instead of
// staying empty.
// ---------------------------------------------------------------------------

#[test]
fn policy_for_attempt_returns_tlsa_when_dane_strings_present() {
    let policy = policy_for_attempt(
        "gmail.com",
        "gmail-smtp-in.l.google.com",
        &["3 1 1 deadbeef".to_string(), "2 0 1 cafef00d".to_string()],
        &MtaStsLookup::NotPublished,
    );
    assert_eq!(policy.policy_type, "tlsa");
    // policy-domain is the MX host that published the TLSA records
    // (RFC 7672 / RFC 8460 §4.4).
    assert_eq!(policy.policy_domain, "gmail-smtp-in.l.google.com");
    assert_eq!(policy.policy_string.len(), 2);
    assert!(policy.policy_string.iter().any(|s| s == "3 1 1 deadbeef"));
    assert!(policy.policy_string.iter().any(|s| s == "2 0 1 cafef00d"));
}

#[test]
fn policy_for_attempt_returns_no_policy_found_without_dane() {
    let policy = policy_for_attempt(
        "yahoo.com",
        "mta5.am0.yahoodns.net",
        &[],
        &MtaStsLookup::NotPublished,
    );
    assert_eq!(policy.policy_type, "no-policy-found");
    // No DANE + no STS → policy-domain is the recipient domain, not
    // the MX host.
    assert_eq!(policy.policy_domain, "yahoo.com");
    assert!(policy.policy_string.is_empty());
}

#[test]
fn policy_for_attempt_returns_sts_when_only_sts_lookup_present() {
    // RFC 8460 §4.3: `policy_type="sts"` attribution when an MTA-STS
    // policy was fetched + parsed for the recipient domain but no DANE
    // pin was applied at this attempt. `policy_domain` is the recipient
    // domain (the STS owner), not the MX host.
    let lookup = enforce_lookup("mx1.example.com");
    let MtaStsLookup::Found(ref fp) = lookup else {
        unreachable!()
    };
    let sts_strings = fp.policy.policy_strings();

    let policy = policy_for_attempt("example.com", "mx1.example.com", &[], &lookup);
    assert_eq!(policy.policy_type, "sts");
    assert_eq!(policy.policy_domain, "example.com");
    assert_eq!(policy.policy_string, sts_strings);
}

#[test]
fn policy_for_attempt_prefers_tlsa_over_sts() {
    // Per RFC 8460 §4.3 precedence: TLSA (DANE) wins over STS. The
    // attempt attributes to whichever pin was actually applied; DANE
    // pin takes effect at the TLS layer regardless of an advertised
    // STS policy.
    let tlsa = vec!["3 1 1 deadbeef".to_string()];
    let policy = policy_for_attempt(
        "example.com",
        "mx.example.com",
        &tlsa,
        &enforce_lookup("mx.example.com"),
    );
    assert_eq!(policy.policy_type, "tlsa");
    assert_eq!(policy.policy_domain, "mx.example.com");
    assert_eq!(policy.policy_string, tlsa);
}

// ---------------------------------------------------------------------------
// `policy_for_attempt` distinguishes the four
// `MtaStsLookup` variants. `FetchError` and `Invalid` attribute as
// `policy_type="sts"` with empty `policy_string` (the policy body
// wasn't usable, but TLSRPT still wants the per-attempt audit trail).
// `NotPublished` falls through to `no-policy-found`. `Found` carries the
// policy_strings projection.
// ---------------------------------------------------------------------------

#[test]
fn policy_for_attempt_attributes_sts_for_fetch_error_with_empty_strings() {
    let policy = policy_for_attempt(
        "example.com",
        "mx.example.com",
        &[],
        &MtaStsLookup::FetchError,
    );
    assert_eq!(policy.policy_type, "sts");
    assert_eq!(policy.policy_domain, "example.com");
    assert!(policy.policy_string.is_empty());
}

#[test]
fn policy_for_attempt_attributes_sts_for_invalid_with_empty_strings() {
    let policy = policy_for_attempt("example.com", "mx.example.com", &[], &MtaStsLookup::Invalid);
    assert_eq!(policy.policy_type, "sts");
    assert_eq!(policy.policy_domain, "example.com");
    assert!(policy.policy_string.is_empty());
}

#[test]
fn mutex_aggregator_implements_recorder_and_forwards_into_buckets() {
    let agg: std::sync::Mutex<TlsrptAggregator> =
        std::sync::Mutex::new(TlsrptAggregator::default());
    let outcome = AttemptOutcome {
        recipient_domain: "gmail.com".into(),
        policy: policy_for_attempt(
            "gmail.com",
            "gmail-smtp-in.l.google.com",
            &["3 1 1 deadbeef".into()],
            &MtaStsLookup::NotPublished,
        ),
        failure_type: None,
    };

    // Trait-object call to prove `&dyn OutboundTlsrptRecorder` works.
    let recorder: &dyn OutboundTlsrptRecorder = &agg;
    recorder.record(outcome);

    let inner = agg.into_inner().unwrap();
    assert_eq!(inner.recorded_domains(), vec!["gmail.com".to_string()]);
}

#[test]
fn arc_mutex_aggregator_can_be_handed_as_dyn_recorder() {
    // Production shape: `Arc<Mutex<TlsrptAggregator>>` is cloned into both
    // the send-fn closure and the daily-dispatch task. The closure
    // dereferences to `&Mutex<TlsrptAggregator>`, which the trait impl
    // covers — verify that explicitly.
    let agg: std::sync::Arc<std::sync::Mutex<TlsrptAggregator>> =
        std::sync::Arc::new(std::sync::Mutex::new(TlsrptAggregator::default()));
    let clone = agg.clone();
    let outcome = AttemptOutcome {
        recipient_domain: "example.com".into(),
        policy: policy_for_attempt(
            "example.com",
            "mx.example.com",
            &[],
            &MtaStsLookup::NotPublished,
        ),
        failure_type: Some("starttls-not-supported".into()),
    };
    (&*clone as &dyn OutboundTlsrptRecorder).record(outcome);

    let domains = agg.lock().unwrap().recorded_domains();
    assert_eq!(domains, vec!["example.com".to_string()]);
}

#[test]
fn null_recorder_is_a_noop() {
    // Used by tests that just want `deliver_raw`'s recorder argument
    // satisfied without observing the recording.
    let r = NullOutboundTlsrptRecorder;
    let outcome = AttemptOutcome {
        recipient_domain: "ignored.example".into(),
        policy: policy_for_attempt(
            "ignored.example",
            "mx.ignored.example",
            &[],
            &MtaStsLookup::NotPublished,
        ),
        failure_type: None,
    };
    (&r as &dyn OutboundTlsrptRecorder).record(outcome);
    // Nothing to assert beyond "doesn't panic" — but exercise the type
    // path so the trait object signature compiles.
}

// ---------------------------------------------------------------------------
// R2 (account-data-plane.md § The ratified decisions) — recipient TLSRPT-policy fetcher (`_smtp._tls.<domain>` rua lookup) +
// fixed-24 h cache. The daily emitter consults this once per recorded
// recipient domain. docs/goal/behavior/smtp-server.md § TLSRPT outbound
// reporter ("Fetch the recipient's TLSRPT policy ... cached for 24 h").
// ---------------------------------------------------------------------------

struct CountingTlsrptFetcher {
    calls: AtomicUsize,
    rua: Option<Vec<String>>,
}

impl CountingTlsrptFetcher {
    fn new(rua: Option<Vec<String>>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            rua,
        }
    }
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl TlsrptPolicyFetcher for CountingTlsrptFetcher {
    async fn lookup(&self, _domain: &str) -> anyhow::Result<Option<Vec<String>>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(self.rua.clone())
    }
}

fn make_clock() -> (Arc<AtomicI64>, Arc<dyn Fn() -> i64 + Send + Sync>) {
    let now = Arc::new(AtomicI64::new(0));
    let now_clone = now.clone();
    let clock: Arc<dyn Fn() -> i64 + Send + Sync> =
        Arc::new(move || now_clone.load(Ordering::Relaxed));
    (now, clock)
}

#[tokio::test]
async fn null_tlsrpt_fetcher_reports_no_policy() {
    let f = NullTlsrptPolicyFetcher;
    assert_eq!(f.lookup("example.com").await.unwrap(), None);
}

#[tokio::test]
async fn caching_tlsrpt_fetcher_holds_within_ttl() {
    let (_clock_state, clock) = make_clock();
    let inner = CountingTlsrptFetcher::new(Some(vec!["mailto:tlsrpt@example.com".to_string()]));
    let cache = CachingTlsrptPolicyFetcher::new(inner, clock);

    let first = cache.lookup("example.com").await.unwrap();
    assert_eq!(first, Some(vec!["mailto:tlsrpt@example.com".to_string()]));
    // Second lookup within TTL — served from cache, inner not re-called.
    let _ = cache.lookup("example.com").await.unwrap();
    assert_eq!(
        cache.inner().call_count(),
        1,
        "cache should hold within TTL"
    );
}

#[tokio::test]
async fn caching_tlsrpt_fetcher_refetches_after_ttl() {
    let (clock_state, clock) = make_clock();
    let inner = CountingTlsrptFetcher::new(Some(vec!["https://r.example.com/v1".to_string()]));
    let cache = CachingTlsrptPolicyFetcher::new(inner, clock);

    cache.lookup("example.com").await.unwrap();
    // Past the 24 h TTL — inner is re-queried.
    clock_state.store(TLSRPT_POLICY_CACHE_TTL_SECS + 1, Ordering::Relaxed);
    cache.lookup("example.com").await.unwrap();
    assert_eq!(cache.inner().call_count(), 2);
}

#[tokio::test]
async fn caching_tlsrpt_fetcher_caches_negative_result() {
    let (_clock_state, clock) = make_clock();
    let inner = CountingTlsrptFetcher::new(None);
    let cache = CachingTlsrptPolicyFetcher::new(inner, clock);

    assert_eq!(cache.lookup("nopolicy.example").await.unwrap(), None);
    assert_eq!(cache.lookup("nopolicy.example").await.unwrap(), None);
    assert_eq!(
        cache.inner().call_count(),
        1,
        "negative result should be cached for the TTL window too"
    );
}

// ---------------------------------------------------------------------------
// N2 — daily-emit scheduling + date helpers (UTC, derived from the
// test-hook-overridable epoch, never the wall clock).
// ---------------------------------------------------------------------------

#[test]
fn seconds_until_midnight_boundary_cases() {
    // Exactly midnight → a full day (never fire twice in one UTC day).
    assert_eq!(seconds_until_next_utc_midnight(0), 86_400);
    // One second before midnight → 1 s.
    assert_eq!(seconds_until_next_utc_midnight(86_400 - 1), 1);
    // One second past midnight → 86399 s.
    assert_eq!(seconds_until_next_utc_midnight(86_401), 86_399);
}

#[test]
fn format_utc_date_matches_epoch() {
    assert_eq!(format_utc_date(0), "1970-01-01");
    // epoch 1779840000 = 20600 days = 2026-05-27 00:00:00 UTC.
    assert_eq!(format_utc_date(1_779_840_000), "2026-05-27");
}

#[test]
fn rfc2822_utc_formats_epoch_and_tod() {
    assert_eq!(rfc2822_utc(0), "Thu, 01 Jan 1970 00:00:00 +0000");
    // 2026-05-27 00:00:00 UTC was a Wednesday.
    assert_eq!(
        rfc2822_utc(1_779_840_000),
        "Wed, 27 May 2026 00:00:00 +0000"
    );
    // +1h2m3s into that day exercises the time-of-day split.
    assert_eq!(
        rfc2822_utc(1_779_840_000 + 3_723),
        "Wed, 27 May 2026 01:02:03 +0000"
    );
}
