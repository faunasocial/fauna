//! Property tests over the stranger-facing pure parsers of `fauna-mail`: the
//! header walkers every inbound message meets before sealing, the DMARC
//! aggregate-report unpack and XML reader (a report arrives from any sender),
//! and the clamd/rspamd reply parsers (a compromised sidecar's reply is
//! stranger input to the bridge). The residual bug class in safe Rust is a
//! panic or an unbounded allocation, so the properties are: never panic, output
//! bounded by input (or by the stated cap), and the fail-closed arms stay
//! fail-closed.

use proptest::prelude::*;

/// Header-ish bytes: mostly printable with the structural bytes over-weighted,
/// so folds, bare CRs, colons and `From`/`X-Fauna-` names turn up often.
fn headerish() -> impl Strategy<Value = Vec<u8>> {
    let atom = prop_oneof![
        3 => any::<u8>().prop_map(|b| vec![b]),
        2 => Just(b"\r\n".to_vec()),
        2 => Just(b"\n".to_vec()),
        1 => Just(b"\r".to_vec()),
        1 => Just(b"\r\n ".to_vec()),
        1 => Just(b"\r\n\t".to_vec()),
        2 => Just(b"From: a@b.example\r\n".to_vec()),
        2 => Just(b"from :x@y.example\n".to_vec()),
        2 => Just(b"X-Fauna-Address-Matched: forged\r\n".to_vec()),
        1 => Just(b"X-Fauna-Forwarded-By: n\r\n".to_vec()),
        1 => Just(b"\r\n\r\n".to_vec()),
    ];
    proptest::collection::vec(atom, 0..40).prop_map(|parts| parts.concat())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn from_field_count_never_panics(raw in headerish()) {
        let _ = crate::from_field::from_field_count(&raw);
    }

    /// A body after the blank line never adds to the count.
    #[test]
    fn from_field_count_ignores_the_body(body in headerish()) {
        let mut raw = b"From: a@b.example\r\nSubject: s\r\n\r\n".to_vec();
        raw.extend_from_slice(&body);
        prop_assert_eq!(crate::from_field::from_field_count(&raw), 1);
    }
}

#[cfg(feature = "received-header")]
proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn strip_fauna_headers_never_panics_and_only_removes(raw in headerish()) {
        let out = crate::received_header::strip_fauna_headers(&raw);
        prop_assert!(out.len() <= raw.len());
    }

    /// No forged `X-Fauna-` stamp survives, except the preserved forward-loop
    /// header (checked on header lines only, ahead of the first blank line).
    #[test]
    fn strip_fauna_headers_leaves_no_forged_stamp(
        names in proptest::collection::vec("[Xx]-[Ff][Aa][Uu][Nn][Aa]-[A-Za-z]{1,12}", 1..5),
    ) {
        let mut raw = Vec::new();
        for n in &names {
            raw.extend_from_slice(format!("{n}: v\r\n").as_bytes());
        }
        raw.extend_from_slice(b"Subject: s\r\n\r\nbody\r\n");
        let out = crate::received_header::strip_fauna_headers(&raw);
        let head = String::from_utf8_lossy(&out);
        for line in head.lines().take_while(|l| !l.is_empty()) {
            let name = line.split(':').next().unwrap_or("").trim();
            prop_assert!(
                !name.to_ascii_lowercase().starts_with("x-fauna-")
                    || name.eq_ignore_ascii_case("X-Fauna-Forwarded-By")
            );
        }
        prop_assert!(String::from_utf8_lossy(&out).contains("Subject: s"));
    }
}

#[cfg(feature = "parser")]
proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn sender_domain_with_envelope_fallback_never_panics(
        raw in headerish(),
        mail_from in "\\PC{0,64}",
    ) {
        let d = crate::envelope::sender_domain_with_envelope_fallback_from_slice(&raw, &mail_from);
        prop_assert!(d.len() <= raw.len().max(mail_from.len()) + 1);
    }

    #[test]
    fn sender_domain_never_panics_on_arbitrary_bytes(
        raw in proptest::collection::vec(any::<u8>(), 0..1024),
    ) {
        let _ = crate::envelope::sender_domain(&raw);
    }
}

#[cfg(feature = "scan")]
proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn clamd_reply_never_panics(reply in "\\PC{0,256}") {
        let _ = crate::scan::clamd_parse_reply(&reply);
    }

    /// Fail-closed: only the exact clean shapes read as clean. Anything with an
    /// ERROR or FOUND marker is never `Clean`.
    #[test]
    fn clamd_error_and_found_are_never_clean(
        pre in "[!-~][ -~]{0,31}",
        post in "[ -~]{0,32}",
    ) {
        use crate::scan::{ClamavVerdict, clamd_parse_reply};
        let found = clamd_parse_reply(&format!("{pre} FOUND"));
        let is_infected = matches!(found, ClamavVerdict::Infected { .. });
        prop_assert!(is_infected);
        let err = clamd_parse_reply(&format!("{pre}ERROR{post}"));
        let not_clean = !matches!(err, ClamavVerdict::Clean);
        prop_assert!(not_clean);
    }

    #[test]
    fn rspamd_reply_never_panics_on_text(json in "\\PC{0,256}", scale in any::<u16>()) {
        let _ = crate::scan::rspamd_parse_reply(&json, scale);
    }

    /// Structured replies: a numeric `score` always parses; a missing or
    /// non-numeric one is an error, never a default score.
    #[test]
    fn rspamd_structured_replies(
        score in -1.0e6f64..1.0e6,
        symbols in proptest::collection::btree_map("[A-Z_]{1,12}", -50.0f64..50.0, 0..8),
        scale in any::<u16>(),
    ) {
        let syms: serde_json::Map<String, serde_json::Value> = symbols
            .iter()
            .map(|(k, s)| (k.clone(), serde_json::json!({ "score": s })))
            .collect();
        let with = serde_json::json!({ "score": score, "symbols": syms }).to_string();
        let got = crate::scan::rspamd_parse_reply(&with, scale).expect("numeric score parses");
        prop_assert_eq!(got.breakdown.len(), symbols.len());
        let sorted = got.breakdown.windows(2).all(|w| w[0].rule <= w[1].rule);
        prop_assert!(sorted);

        let without = serde_json::json!({ "symbols": syms }).to_string();
        prop_assert!(crate::scan::rspamd_parse_reply(&without, scale).is_err());
        let textual = serde_json::json!({ "score": "9.9" }).to_string();
        prop_assert!(crate::scan::rspamd_parse_reply(&textual, scale).is_err());
    }
}

#[cfg(feature = "dmarc-report")]
proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Unpack on stranger bytes: gzip/zip/bare, junk with a real magic prefix
    /// included. Output is bounded by the decompression cap.
    #[test]
    fn unpack_never_panics_and_stays_bounded(
        magic in prop_oneof![
            Just(Vec::<u8>::new()),
            Just(vec![0x1f, 0x8b]),
            Just(b"PK\x03\x04".to_vec()),
        ],
        rest in proptest::collection::vec(any::<u8>(), 0..2048),
    ) {
        let mut body = magic;
        body.extend_from_slice(&rest);
        if let Ok(out) = crate::dmarc_report::unpack(&body) {
            prop_assert!(out.len() <= crate::dmarc_report::MAX_DECOMPRESSED_BYTES);
        }
    }

    #[test]
    fn parse_aggregate_xml_never_panics(xml in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = crate::dmarc_report::parse_aggregate_xml(&xml);
    }

    /// Markup-dense text: tags, attributes, entities, CDATA, comments and
    /// doctypes in arbitrary order stress the strict reader's state machine.
    #[test]
    fn parse_aggregate_xml_never_panics_on_markup_soup(
        parts in proptest::collection::vec(
            prop_oneof![
                Just("<feedback>".to_string()),
                Just("</feedback>".to_string()),
                Just("<record><row><source_ip>192.0.2.1</source_ip><count>1</count></row></record>".to_string()),
                Just("<report_metadata><org_name>o</org_name></report_metadata>".to_string()),
                Just("<![CDATA[".to_string()),
                Just("]]>".to_string()),
                Just("<!--".to_string()),
                Just("-->".to_string()),
                Just("<!DOCTYPE x [<!ENTITY a \"b\">]>".to_string()),
                Just("&amp;&lt;&#x41;&#0;&bogus;".to_string()),
                Just("<a b='c\" d=e>".to_string()),
                "\\PC{0,8}",
            ],
            0..40,
        ),
    ) {
        let xml = parts.concat();
        let _ = crate::dmarc_report::parse_aggregate_xml(xml.as_bytes());
    }
}
