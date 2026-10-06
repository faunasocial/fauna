//! Wire-contract round-trip tests for [`ReqwestNestContentApi`] against
//! `wiremock` — bearer attach, the 401-reactive `notify_401()` + one retry,
//! the structured `{"error": ...}` parsing, and the `get_with_query` /
//! `patch_json` verbs the bridge daemon needs. Parametrized over the
//! [`BearerSource`] impls:
//!
//! - [`StaticBearer`] — proves `send` / `into_body` work with a source that
//!   can't recover (a 401 surfaces after exactly one retry — the "never an
//!   infinite retry" guarantee).
//! - [`LaunchMachineBearer`] (feature `launch-machine`) — the four tests
//!   re-homed from `apps/fauna-linux/src/nest_content_api/reqwest_impl.rs`'s
//!   `#[cfg(test)]` module: success-no-refresh / 401→notify→retry /
//!   401-then-failed-refresh→surface-401 / structured-error parse.

use fauna_nest_http::{ApiError, NestContentApi, ReqwestNestContentApi, StaticBearer};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Far enough in the future that the 60 s pre-expiry buffer keeps a cached
/// bearer (we want the content endpoints to see the bearer, not a proactive
/// `/auth/token` refresh).
const FAR_FUTURE_SECS: u64 = 4_000_000_000; // ~year 2096

fn api<B: fauna_nest_http::BearerSource>(server: &MockServer, b: B) -> ReqwestNestContentApi<B> {
    ReqwestNestContentApi::new(server.uri(), reqwest::Client::new(), b)
}

// ===========================================================================
// StaticBearer — generality of send/into_body + the new verbs
// ===========================================================================

#[tokio::test]
async fn static_bearer_get_attaches_and_returns_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/quota"))
        .and(header("authorization", "Bearer t0"))
        .respond_with(ResponseTemplate::new(200).set_body_string("quota-ok"))
        .mount(&server)
        .await;
    let body = api(&server, StaticBearer("t0".into()))
        .get("/api/v1/quota")
        .await
        .expect("2xx → body");
    assert_eq!(&body[..], b"quota-ok");
}

#[tokio::test]
async fn static_bearer_get_with_query_percent_encodes_params() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .and(wiremock::matchers::query_param("q", "hello world"))
        .and(wiremock::matchers::query_param("limit", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server)
        .await;
    let body = api(&server, StaticBearer("t0".into()))
        .get_with_query("/api/v1/search", &[("q", "hello world"), ("limit", "20")])
        .await
        .expect("2xx → body");
    assert_eq!(&body[..], b"[]");
}

#[tokio::test]
async fn static_bearer_patch_json_sends_body() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/events/ev1"))
        .and(body_partial_json(
            serde_json::json!({ "summary": "Updated" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    let body = api(&server, StaticBearer("t0".into()))
        .patch_json(
            "/api/events/ev1",
            &serde_json::json!({ "summary": "Updated" }),
        )
        .await
        .expect("2xx → body");
    assert_eq!(&body[..], b"{}");
}

#[tokio::test]
async fn static_bearer_structured_error_is_parsed() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/profile/handle"))
        .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
            "error": "handle taken"
        })))
        .mount(&server)
        .await;
    let err = api(&server, StaticBearer("t0".into()))
        .put_json(
            "/api/v1/profile/handle",
            &serde_json::json!({ "handle": "alice" }),
        )
        .await
        .expect_err("409 → Status error");
    assert_eq!(
        err,
        ApiError::Status {
            code: 409,
            message: "handle taken".into()
        }
    );
}

/// The doc-promised off-contract fallback (`NestErrorBody` parse fails → raw
/// body verbatim): a reverse-proxy/CDN in front of a nest can answer a non-2xx
/// with HTML instead of the nest's own `{"error": ...}` shape. Every other
/// error test in this file sends well-formed nest JSON — this is the only one
/// that exercises the fallback arm of `into_body`.
#[tokio::test]
async fn static_bearer_off_contract_body_falls_back_to_raw_text() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/quota"))
        .respond_with(
            ResponseTemplate::new(502)
                .set_body_string("<html><body>Bad Gateway</body></html>")
                .insert_header("content-type", "text/html"),
        )
        .mount(&server)
        .await;
    let err = api(&server, StaticBearer("t0".into()))
        .get("/api/v1/quota")
        .await
        .expect_err("502 HTML → Status error, raw body as message");
    assert_eq!(
        err,
        ApiError::Status {
            code: 502,
            message: "<html><body>Bad Gateway</body></html>".into()
        }
    );
}

/// The same fallback arm, empty-body edge case (a proxy timeout / bare status
/// line with no body at all) — `into_body` must not panic or hang, and must
/// report an empty message rather than treating an empty body as "parse
/// succeeded with no message".
#[tokio::test]
async fn static_bearer_off_contract_empty_body_falls_back_to_empty_message() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/quota"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let err = api(&server, StaticBearer("t0".into()))
        .get("/api/v1/quota")
        .await
        .expect_err("503 empty body → Status error, empty message");
    assert_eq!(
        err,
        ApiError::Status {
            code: 503,
            message: String::new()
        }
    );
}

#[tokio::test]
async fn static_bearer_401_retries_once_then_surfaces_the_status() {
    let server = MockServer::start().await;
    // The source can't recover (StaticBearer.notify_401 is a no-op) — `send`
    // still retries exactly once with the same token, then gives up; the
    // first 401 becomes an `ApiError::Status`.
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .and(header("authorization", "Bearer stale"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "token expired"
        })))
        .expect(2) // initial + the one retry — never an infinite loop
        .mount(&server)
        .await;
    let err = api(&server, StaticBearer("stale".into()))
        .get("/api/v1/account")
        .await
        .expect_err("a 401 we can't recover from is still an error");
    assert_eq!(
        err,
        ApiError::Status {
            code: 401,
            message: "token expired".into()
        }
    );
}

#[tokio::test]
async fn static_bearer_post_multipart_blob_sends_sidecar_and_bytes_parts() {
    // The encrypted-mode blob-ingest wire shape: `multipart/form-data` with
    // exactly two parts — `sidecar` (DAG-CBOR `UploadSidecar`,
    // `application/cbor`) + `bytes` (sealed primary bytes,
    // `application/octet-stream`). Mirrors the nest's `parse_multipart_upload`
    // contract (`bins/fauna-nest/src/blob_routes.rs`).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/blob"))
        .and(header("authorization", "Bearer t0"))
        .respond_with(ResponseTemplate::new(201).set_body_string(r#"{"hash":"deadbeef"}"#))
        .mount(&server)
        .await;

    let sidecar_cbor = vec![0xA1u8, 0x00, 0x01]; // arbitrary stand-in CBOR
    let sealed_bytes = vec![0x10u8, 0x20, 0x30, 0x40];
    let body = api(&server, StaticBearer("t0".into()))
        .post_multipart_blob("/api/v1/blob", sidecar_cbor.clone(), sealed_bytes.clone())
        .await
        .expect("2xx → body");
    assert_eq!(&body[..], br#"{"hash":"deadbeef"}"#);

    // Inspect the wire: multipart/form-data, both named parts, the sidecar
    // declares application/cbor.
    let reqs = server
        .received_requests()
        .await
        .expect("request recording enabled");
    assert_eq!(reqs.len(), 1);
    let ct = reqs[0]
        .headers
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.starts_with("multipart/form-data"), "content-type: {ct}");
    let raw = String::from_utf8_lossy(&reqs[0].body);
    assert!(
        raw.contains("name=\"sidecar\""),
        "missing sidecar part:\n{raw}"
    );
    assert!(raw.contains("name=\"bytes\""), "missing bytes part:\n{raw}");
    assert!(
        raw.contains("application/cbor"),
        "sidecar part should declare application/cbor:\n{raw}"
    );
}

#[tokio::test]
async fn static_bearer_head_has_c2pa_reads_the_header_with_no_body() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/api/v1/blob/deadbeef"))
        .and(header("authorization", "Bearer t0"))
        .respond_with(ResponseTemplate::new(200).insert_header("x-c2pa", "true"))
        .mount(&server)
        .await;

    let has_c2pa = api(&server, StaticBearer("t0".into()))
        .head_has_c2pa("/api/v1/blob/deadbeef")
        .await
        .expect("2xx → the header read");
    assert!(has_c2pa);
}

#[tokio::test]
async fn static_bearer_head_has_c2pa_false_when_header_is_false_or_absent() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/api/v1/blob/false-hash"))
        .respond_with(ResponseTemplate::new(200).insert_header("x-c2pa", "false"))
        .mount(&server)
        .await;
    Mock::given(method("HEAD"))
        .and(path("/api/v1/blob/no-header-hash"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let api = api(&server, StaticBearer("t0".into()));
    assert!(!api.head_has_c2pa("/api/v1/blob/false-hash").await.unwrap());
    assert!(
        !api.head_has_c2pa("/api/v1/blob/no-header-hash")
            .await
            .unwrap()
    );
}

// ===========================================================================
// LaunchMachineBearer — re-homed from nest_content_api/reqwest_impl.rs's tests
// ===========================================================================

#[cfg(feature = "launch-machine")]
mod launch_machine {
    use super::*;
    use std::sync::Arc;

    use fauna_launch_machine::{
        InMemoryPersistence, LaunchMachine, LaunchPhase, MockAuthConnector, NullObserver,
        SilentChallengeOutcome,
    };
    use fauna_nest_http::LaunchMachineBearer;
    use fauna_protocol::auth::VerifyReply;

    fn verify_reply(token: &str) -> VerifyReply {
        verify_reply_with_id(token, &"0".repeat(16))
    }

    fn verify_reply_with_id(token: &str, token_id: &str) -> VerifyReply {
        VerifyReply {
            token: token.into(),
            token_id: token_id.into(),
            handle: "alice".into(),
            domain: "nest.example".into(),
            tier: "free".into(),
            expires_at: FAR_FUTURE_SECS,
            // Anchored on this clock at receipt: a full hour, the nest's TTL.
            expires_in: 3600,
            ..Default::default()
        }
    }

    /// A scripted connector that lands the machine `Online` holding the bearer
    /// `"stale.bearer"` (a successful silent challenge), with the given refresh
    /// outcomes queued for the 401-reactive path — the same silent-challenge
    /// ceremony, so one mock queue serves launch and refresh. The auth
    /// ceremonies are WS-RPC, so they're driven through `MockAuthConnector`
    /// rather than HTTP-mocked on the wiremock server (which only serves the
    /// still-HTTP content endpoints below).
    fn online_connector(refreshes: Vec<SilentChallengeOutcome>) -> MockAuthConnector {
        let mut c = MockAuthConnector::new().push_silent_challenge(
            SilentChallengeOutcome::Success(verify_reply("stale.bearer")),
        );
        for r in refreshes {
            c = c.push_silent_challenge(r);
        }
        c
    }

    /// Drive a fresh `LaunchMachine` to `Online` over the scripted `connector`
    /// so it holds the bearer `"stale.bearer"`. Mirrors
    /// `silent_challenge.rs`'s `MockAuthConnector` pattern.
    async fn machine_online(connector: MockAuthConnector) -> Arc<LaunchMachine> {
        let p = Arc::new(
            InMemoryPersistence::new()
                .with_identity([0x42u8; 32].to_vec())
                .with_nest_url("https://nest.example"),
        );
        let m = LaunchMachine::new_with_connector(Arc::new(NullObserver), p, Arc::new(connector));
        m.start().await;
        assert_eq!(m.snapshot().phase, LaunchPhase::Online);
        assert_eq!(m.current_bearer().as_deref(), Some("stale.bearer"));
        m
    }

    #[tokio::test]
    async fn success_returns_body_without_refreshing() {
        let server = MockServer::start().await;
        let m = machine_online(online_connector(vec![])).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/quota"))
            .and(header("authorization", "Bearer stale.bearer"))
            .respond_with(ResponseTemplate::new(200).set_body_string("quota-ok"))
            .mount(&server)
            .await;
        // No token-refresh outcome scripted — a stray refresh would land the
        // machine Offline; assert the bearer is untouched to be sure it never
        // happened.
        let body = api(&server, LaunchMachineBearer(Arc::clone(&m)))
            .get("/api/v1/quota")
            .await
            .expect("2xx → body");
        assert_eq!(&body[..], b"quota-ok");
        assert_eq!(m.current_bearer().as_deref(), Some("stale.bearer"));
    }

    #[tokio::test]
    async fn http_401_triggers_notify_and_retries_with_fresh_bearer() {
        let server = MockServer::start().await;
        // 401-reactive refresh mints "fresh.bearer" over the silent challenge.
        let m = machine_online(online_connector(vec![SilentChallengeOutcome::Success(
            verify_reply_with_id("fresh.bearer", "freshbearer00001"),
        )]))
        .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/channel/abc"))
            .and(header("authorization", "Bearer stale.bearer"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": "token expired"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/channel/abc"))
            .and(header("authorization", "Bearer fresh.bearer"))
            .respond_with(ResponseTemplate::new(200).set_body_string("posted"))
            .mount(&server)
            .await;
        let body = api(&server, LaunchMachineBearer(Arc::clone(&m)))
            .post_bytes(
                "/api/v1/channel/abc",
                "application/octet-stream",
                b"ciphertext".to_vec(),
            )
            .await
            .expect("retry with the fresh bearer → 2xx");
        assert_eq!(&body[..], b"posted");
        assert_eq!(m.current_bearer().as_deref(), Some("fresh.bearer"));
    }

    /// The chunk store's write shape: an octet-stream body under the
    /// `X-Content-Hash` store key, with the session bearer — the wire the sync
    /// engine's `upload_chunk` sends and the Media page's content-keyed
    /// upload reaches through this API.
    #[tokio::test]
    async fn post_bytes_keyed_sends_the_store_key_header_and_the_bearer() {
        let server = MockServer::start().await;
        let m = machine_online(online_connector(vec![])).await;
        Mock::given(method("POST"))
            .and(path("/api/v1/chunks"))
            .and(header("authorization", "Bearer stale.bearer"))
            .and(header("x-content-hash", "ab".repeat(32).as_str()))
            .and(header("content-type", "application/octet-stream"))
            .respond_with(ResponseTemplate::new(201).set_body_string(r#"{"hash":"..."}"#))
            .mount(&server)
            .await;
        let body = api(&server, LaunchMachineBearer(Arc::clone(&m)))
            .post_bytes_keyed("/api/v1/chunks", &"ab".repeat(32), b"ciphertext".to_vec())
            .await
            .expect("the keyed POST reaches the chunk route with its header");
        assert_eq!(&body[..], br#"{"hash":"..."}"#);
    }

    #[tokio::test]
    async fn http_401_then_failed_refresh_surfaces_status_error() {
        let server = MockServer::start().await;
        // The 401-reactive refresh fails terminally (the account is gone —
        // `NotRegistered` → terminal `Offline`). `send` must surface the
        // original content-endpoint 401 as `ApiError::Status`, not loop — and
        // the bearer's transient-recovery retry must NOT fire (it self-guards
        // to the transient case).
        let m = machine_online(online_connector(vec![
            SilentChallengeOutcome::NotRegistered,
        ]))
        .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/quota"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": "token expired"
            })))
            .mount(&server)
            .await;
        let err = api(&server, LaunchMachineBearer(Arc::clone(&m)))
            .get("/api/v1/quota")
            .await
            .expect_err("a 401 we can't recover from is still an error");
        assert_eq!(
            err,
            ApiError::Status {
                code: 401,
                message: "token expired".into()
            }
        );
        assert!(m.current_bearer().is_none());
    }

    #[tokio::test]
    async fn non_success_parses_structured_error() {
        let server = MockServer::start().await;
        let m = machine_online(online_connector(vec![])).await;
        Mock::given(method("PUT"))
            .and(path("/api/v1/profile/handle"))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "error": "handle taken"
            })))
            .mount(&server)
            .await;
        let err = api(&server, LaunchMachineBearer(m))
            .put_json(
                "/api/v1/profile/handle",
                &serde_json::json!({ "handle": "alice" }),
            )
            .await
            .expect_err("409 → Status error");
        assert_eq!(
            err,
            ApiError::Status {
                code: 409,
                message: "handle taken".into()
            }
        );
    }
}

// ===========================================================================
// The error body is bounded — a responder does not choose our allocation
// ===========================================================================

/// A non-2xx body far past the cap comes back bounded, with the status intact.
/// The responder can be a nest someone else runs — a conversation's home nest
/// is chosen by whoever created the room — so its error body must not decide
/// how much this client reads, holds and passes on.
#[tokio::test]
async fn an_oversized_error_body_is_read_only_up_to_the_cap() {
    let server = MockServer::start().await;
    let huge = "A".repeat(fauna_nest_http::content::MAX_ERROR_BODY_BYTES * 64);
    Mock::given(method("GET"))
        .and(path("/api/v1/quota"))
        .respond_with(ResponseTemplate::new(400).set_body_string(huge))
        .mount(&server)
        .await;
    let err = api(&server, StaticBearer("t0".into()))
        .get("/api/v1/quota")
        .await
        .expect_err("4xx → Status");
    match err {
        ApiError::Status { code, message } => {
            assert_eq!(code, 400, "the status still reaches the caller");
            assert!(
                message.len() <= fauna_nest_http::content::MAX_ERROR_BODY_BYTES,
                "the error body must be capped, got {} bytes",
                message.len()
            );
        }
        other => panic!("expected Status, got {other:?}"),
    }
}
