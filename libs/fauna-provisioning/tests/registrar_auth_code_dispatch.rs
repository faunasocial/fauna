//! The transfer-authorization-code capability, as **data** — the seam the
//! retire view branches on so no app ever spells a provider id
//! (`docs/goal/behavior/nest-retirement.md` § Transfer authorization code).
//!
//! Two halves that must never drift apart:
//!
//! 1. `supports_auth_code(id)` — the boolean a page asks *before* it builds a
//!    dispatcher, to decide between a *get the code* button and a *fetch it
//!    from your registrar's dashboard* note.
//! 2. `RegistrarDispatch::auth_code()` — the call itself, `Ok(None)` when the
//!    adapter has no such call.
//!
//! A drift between them is a user-visible bug in either direction: a button
//! that always fails, or a door the spec calls mandatory quietly hidden. The
//! bijection test below is what stops a new registrar adapter landing one.

use fauna_provisioning::dispatch::{
    Credentials, RegistrarDispatch, parse_credentials, registrar, supports_auth_code,
};
use fauna_provisioning::registrar::bundled::AuthCode;
use fauna_provisioning::{PROVIDERS, ProviderId};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A credentials bag holding every field the registry lists for `id` — the
/// same shape `dispatch_registry_bijection.rs` builds, and for the same
/// reason: sourcing keys from `FieldMeta` means a renamed field fails loudly
/// here instead of reading as an absent credential.
fn full_creds(id: ProviderId) -> Credentials {
    let meta = PROVIDERS
        .iter()
        .find(|p| p.id == id)
        .expect("every ProviderId has a PROVIDERS row");
    let body = meta
        .fields
        .iter()
        .map(|f| {
            let value = if f.id == "base-url" {
                "https://provider.test".to_string()
            } else {
                format!("test-{}", f.id)
            };
            format!("\"{}\":\"{}\"", f.id, value)
        })
        .collect::<Vec<_>>()
        .join(",");
    parse_credentials(&format!("{{{body}}}")).expect("registry field ids form valid JSON keys")
}

#[tokio::test]
async fn supports_auth_code_agrees_with_what_dispatch_actually_serves() {
    // A base URL that resolves nowhere: a registrar WITHOUT an auth-code call
    // must answer `Ok(None)` without ever reaching the network, so this URL is
    // never dialled on the `false` arms. The `true` arm is covered by the
    // wiremock tests below; here it is only asked whether it *would* try.
    let mut mismatches = Vec::new();

    for meta in PROVIDERS {
        let id = meta.id;
        let claims = supports_auth_code(id);

        let Some(dispatch) = registrar(id, full_creds(id)) else {
            // No registrar adapter at all — it cannot have an auth-code call.
            if claims {
                mismatches.push(format!(
                    "{}: supports_auth_code says true but has no registrar dispatcher",
                    id.as_str()
                ));
            }
            continue;
        };

        if claims {
            // Proven against a live mock below; nothing to dial here.
            continue;
        }

        match dispatch.auth_code(&reqwest::Client::new(), "example.test").await {
            Ok(None) => {}
            other => mismatches.push(format!(
                "{}: supports_auth_code says false, but auth_code() did not answer Ok(None) — got {}",
                id.as_str(),
                match other {
                    Ok(Some(_)) => "a code".to_string(),
                    Err(e) => format!("an error ({e:?}) — it reached the network"),
                    Ok(None) => unreachable!(),
                }
            )),
        }
    }

    assert!(
        mismatches.is_empty(),
        "supports_auth_code and RegistrarDispatch::auth_code disagree:\n  {}",
        mismatches.join("\n  ")
    );
}

#[test]
fn only_the_bundled_provider_claims_an_auth_code_today() {
    // The spec makes the code a *mandatory* exit guarantee of the open bundled
    // API (`bundled-provider-api.md` § Exit guarantee 1); no other registrar
    // adapter has one, so every other provider's row carries a note instead.
    // A new `true` here is a deliberate act, not a drive-by.
    let claiming: Vec<&str> = PROVIDERS
        .iter()
        .filter(|m| supports_auth_code(m.id))
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(claiming, vec!["bundled"], "unexpected auth-code claimants");
}

/// Both arms reach the caller intact through dispatch — `200` with the code,
/// and `202 available_after` under a registry transfer lock. The `202` is
/// **never a refusal**: the view shows the date the lock lifts.
#[tokio::test]
async fn dispatch_carries_the_ready_arm() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/domains/example.test/auth-code"))
        .and(header("authorization", "Bearer test-api-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "auth_code": "XFER-123" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let creds = parse_credentials(&format!(
        r#"{{"base-url":"{}","api-token":"test-api-token"}}"#,
        server.uri()
    ))
    .unwrap();
    let dispatch: RegistrarDispatch =
        registrar(ProviderId::Bundled, creds).expect("bundled builds a registrar");

    let got = dispatch
        .auth_code(&reqwest::Client::new(), "example.test")
        .await
        .unwrap();
    assert_eq!(got, Some(AuthCode::Ready("XFER-123".into())));
}

#[tokio::test]
async fn dispatch_carries_the_available_after_arm() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/domains/locked.test/auth-code"))
        .respond_with(
            ResponseTemplate::new(202)
                .set_body_json(serde_json::json!({ "available_after": "2026-11-18T00:00:00Z" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let creds = parse_credentials(&format!(
        r#"{{"base-url":"{}","api-token":"test-api-token"}}"#,
        server.uri()
    ))
    .unwrap();
    let dispatch = registrar(ProviderId::Bundled, creds).expect("bundled builds a registrar");

    let got = dispatch
        .auth_code(&reqwest::Client::new(), "locked.test")
        .await
        .unwrap();
    assert_eq!(
        got,
        Some(AuthCode::AvailableAfter("2026-11-18T00:00:00Z".into())),
        "a registry lock reports when it lifts — it is never a refusal"
    );
}
