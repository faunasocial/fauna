use fauna_mail::auth::verify_inbound_with;
use fauna_mail::auth::{
    ArcVerdict, AuthVerdicts, DkimVerdict, DmarcPolicy, DmarcVerdict, SpfVerdict,
};

#[test]
fn auth_verdicts_default_is_all_none() {
    let v = AuthVerdicts::default();
    assert!(matches!(v.dkim, DkimVerdict::None));
    assert!(matches!(v.spf, SpfVerdict::None));
    assert!(matches!(v.dmarc, DmarcVerdict::None));
    assert!(matches!(v.arc, ArcVerdict::None));
}

// Pins the adjacently-tagged JSON shape that fauna-protocol's wire
// AuthVerdicts and the Go-side wsrpc mirror have to match byte-for-byte.
// JSON instead of CBOR so the assertion is human-readable; the wire is
// CBOR but serde emits the same `{"kind":..., "data":...}` map shape in
// either codec.
#[test]
fn verdict_enums_serialize_with_kind_data_tag() {
    let pass_unit = serde_json::to_string(&DkimVerdict::Pass).unwrap();
    assert_eq!(pass_unit, r#"{"kind":"pass"}"#);

    let dkim_fail = serde_json::to_string(&DkimVerdict::Fail {
        reason: "bad sig".into(),
    })
    .unwrap();
    assert_eq!(dkim_fail, r#"{"kind":"fail","data":{"reason":"bad sig"}}"#);

    let dmarc_fail = serde_json::to_string(&DmarcVerdict::Fail {
        policy: DmarcPolicy::Quarantine,
    })
    .unwrap();
    assert_eq!(
        dmarc_fail,
        r#"{"kind":"fail","data":{"policy":"quarantine"}}"#
    );

    let spf_softfail = serde_json::to_string(&SpfVerdict::SoftFail).unwrap();
    assert_eq!(spf_softfail, r#"{"kind":"soft_fail"}"#);

    let arc_perm = serde_json::to_string(&ArcVerdict::PermError).unwrap();
    assert_eq!(arc_perm, r#"{"kind":"perm_error"}"#);
}

#[test]
fn auth_verdicts_round_trips_through_json() {
    let v = AuthVerdicts {
        dkim: DkimVerdict::Fail {
            reason: "bad sig".into(),
        },
        spf: SpfVerdict::Pass,
        dmarc: DmarcVerdict::Fail {
            policy: DmarcPolicy::Reject,
        },
        arc: ArcVerdict::None,
    };
    let s = serde_json::to_string(&v).unwrap();
    let back: AuthVerdicts = serde_json::from_str(&s).unwrap();
    assert_eq!(v, back);
}

const NO_AUTH_EML: &[u8] = b"\
From: alice@example.com\r\n\
To: bob@example.com\r\n\
Subject: No DKIM here\r\n\
Date: Wed, 7 May 2026 12:00:00 +0000\r\n\
Message-ID: <no-dkim-001@example.com>\r\n\
\r\n\
No DKIM-Signature header so DKIM should be 'None'.\r\n";

/// A `MessageAuthenticator` whose ONLY nameserver is the shared in-process
/// responder, which answers NXDOMAIN to every query (the served name is one no
/// lookup here asks for).
///
/// The production `verify_inbound` builds its authenticator from the system
/// resolver config, so a test through it does live SPF/DMARC lookups: its verdict
/// depends on whether the box can reach DNS, which is convention 14's defect
/// (docs/goal/architecture/e2e-latency-independent-assertions.md), and on Windows
/// the resolver's wildcard-bound UDP socket raised a firewall prompt per run.
/// The client socket is bound to loopback too, so even that prompt cannot fire.
async fn hermetic_authenticator() -> mail_auth::MessageAuthenticator {
    use hickory_resolver::config::{
        ConnectionConfig, NameServerConfig, ResolverConfig, ResolverOpts,
    };

    let ns = fauna_core::authoritative_dns::spawn_responder("never-queried.invalid.", "").await;
    let mut udp = ConnectionConfig::udp();
    udp.port = ns.port();
    udp.bind_addr = Some(std::net::SocketAddr::from(([127, 0, 0, 1], 0)));
    let server = NameServerConfig::new(ns.ip(), true, vec![udp]);
    let config = ResolverConfig::from_parts(None, vec![], vec![server]);
    mail_auth::MessageAuthenticator::new(config, ResolverOpts::default())
        .expect("hermetic authenticator")
}

// No DKIM signature, no ARC set, and no SPF or DMARC record anywhere the
// resolver can see: every verdict is None, deterministically.
#[tokio::test]
async fn verify_inbound_message_without_auth_records_is_all_none() {
    let authenticator = hermetic_authenticator().await;
    let verdicts = verify_inbound_with(
        &authenticator,
        NO_AUTH_EML,
        "alice@example.com",
        "192.0.2.1",
        "mail.example.com",
    )
    .await
    .expect("verify should not error");
    assert_eq!(verdicts, AuthVerdicts::default(), "{verdicts:?}");
}

// ── ONE definition, pinned by the type system ──────────
//
// The mail-auth verdict enums used to exist twice — hand-mirrored in
// `fauna-mail::auth` (the producer, UniFFI-exported) and
// `fauna-protocol::bridge_routing` (the wire) — with agreement pinned only by
// the CBOR round-trip tests in `bridge_routing.rs`. A test can only notice
// drift after someone writes it; these assertions make the drift
// unrepresentable, because there is now exactly one type and both paths are
// re-exports of it.
//
// This file is the only place that can hold the pin: `fauna-mail` depends on
// `fauna-protocol` (via `kind-registry`, on by default), while
// `fauna-protocol` cannot depend on `fauna-mail` — so only this side sees both
// names. Each function below is an identity function whose argument and return
// types are spelled through the two different paths; it compiles if and only if
// they name the same type. Before row 163 these failed to compile with E0308.
//
// The canonical home is `fauna-core::mail_auth`. `fauna-protocol` was the wrong
// home despite what the old `TODO: unify these definitions` comment proposed:
// it carries no UniFFI surface by design, and these types must be exported to
// the Go MTA that produces them. `fauna-core` costs neither: it already has
// `uniffi`, is already a dependency of both, and is WASM-safe under
// `default-features = false`.
#[test]
fn the_verdict_types_have_exactly_one_definition() {
    fn dkim(v: fauna_mail::auth::DkimVerdict) -> fauna_protocol::bridge_routing::DkimVerdict {
        v
    }
    fn spf(v: fauna_mail::auth::SpfVerdict) -> fauna_protocol::bridge_routing::SpfVerdict {
        v
    }
    fn dmarc_policy(
        v: fauna_mail::auth::DmarcPolicy,
    ) -> fauna_protocol::bridge_routing::DmarcPolicy {
        v
    }
    fn dmarc(v: fauna_mail::auth::DmarcVerdict) -> fauna_protocol::bridge_routing::DmarcVerdict {
        v
    }
    fn arc(v: fauna_mail::auth::ArcVerdict) -> fauna_protocol::bridge_routing::ArcVerdict {
        v
    }
    fn verdicts(v: fauna_mail::auth::AuthVerdicts) -> fauna_protocol::bridge_routing::AuthVerdicts {
        v
    }

    // Exercise them so the pin is a real test, not only a compile-time claim.
    assert_eq!(dkim(DkimVerdict::Pass), DkimVerdict::Pass);
    assert_eq!(spf(SpfVerdict::SoftFail), SpfVerdict::SoftFail);
    assert_eq!(dmarc_policy(DmarcPolicy::Reject), DmarcPolicy::Reject);
    assert_eq!(
        dmarc(DmarcVerdict::Fail {
            policy: DmarcPolicy::Quarantine
        }),
        DmarcVerdict::Fail {
            policy: DmarcPolicy::Quarantine
        }
    );
    assert_eq!(arc(ArcVerdict::PermError), ArcVerdict::PermError);
    assert_eq!(verdicts(AuthVerdicts::default()), AuthVerdicts::default());
}
