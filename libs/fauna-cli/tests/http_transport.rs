//! tier_1: the loopback streamable-HTTP transport (`apps/cli.md` § The device
//! shell) against a mocked principal session — the same method layer as stdio,
//! and the DNS-rebinding refusals that are the listener's own defence (loopback
//! is the trust boundary, so `Host` and `Origin` are what it checks).
#![cfg(feature = "device")]

use std::sync::Arc;

use fauna_cli::tools::ConnectStarted;
use fauna_cli::{Host, Server, SessionUnavailable, ToolError, http};
use fauna_client_testkit::RejectingRequester;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct FakeHost;

impl Host for FakeHost {
    type Rpc = Arc<RejectingRequester>;

    async fn session(&self, _account: Option<&str>) -> Result<Self::Rpc, SessionUnavailable> {
        Err(SessionUnavailable::NotConnected)
    }

    async fn account(&self, _account: Option<&str>) -> Result<String, SessionUnavailable> {
        Err(SessionUnavailable::NotConnected)
    }

    async fn redial(
        &self,
        _account: Option<&str>,
        _lost: &Self::Rpc,
    ) -> Result<Self::Rpc, SessionUnavailable> {
        Err(SessionUnavailable::NotConnected)
    }

    async fn connect(&self, handle: &str, _admin: bool) -> Result<ConnectStarted, ToolError> {
        Ok(ConnectStarted {
            account: handle.into(),
            user_code: "WXYZ-1234".into(),
            nest: format!("https://{handle}"),
            expires_in: 600,
            custody: None,
        })
    }

    async fn setup_status(
        &self,
        _account: Option<&str>,
    ) -> Result<fauna_protocol::discovery::SetupStatusReply, ToolError> {
        Err(ToolError::new("no setup status in this fake"))
    }
}

/// One raw HTTP/1.1 exchange: the status code and the body. `host` and `origin`
/// are sent as given (`host: None` omits the header).
async fn exchange(
    port: u16,
    method: &str,
    path: &str,
    host: Option<&str>,
    origin: Option<&str>,
    body: &str,
) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut request = format!("{method} {path} HTTP/1.1\r\n");
    if let Some(host) = host {
        request.push_str(&format!("Host: {host}\r\n"));
    }
    if let Some(origin) = origin {
        request.push_str(&format!("Origin: {origin}\r\n"));
    }
    request.push_str(&format!(
        "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    ));
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, body.to_owned())
}

/// Run `script(port)` against a live listener on the current task (the host's
/// futures are not `Send`, so the server is selected against, never spawned).
async fn with_listener<F, Fut>(script: F)
where
    F: FnOnce(u16) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let server = Server::new(FakeHost);
    let listener = http::bind().await.unwrap();
    let port = listener.local_addr().unwrap().port();
    assert_eq!(
        http::url(&listener).unwrap(),
        format!("http://127.0.0.1:{port}/mcp")
    );
    tokio::select! {
        served = http::serve(&server, listener) => panic!("the listener ended: {served:?}"),
        () = script(port) => {}
    }
}

fn rpc(id: u32, method: &str, params: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string()
}

#[tokio::test]
async fn a_loopback_host_gets_the_same_catalog_over_http() {
    with_listener(|port| async move {
        let host = format!("127.0.0.1:{port}");
        let (status, body) = exchange(
            port,
            "POST",
            "/mcp",
            Some(&host),
            None,
            &rpc(1, "initialize", json!({ "protocolVersion": "2025-06-18" })),
        )
        .await;
        assert_eq!(status, 200);
        let init: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "fauna-cli");

        let (status, body) = exchange(
            port,
            "POST",
            "/mcp",
            Some(&host),
            None,
            &rpc(2, "tools/list", json!({})),
        )
        .await;
        assert_eq!(status, 200);
        let listed: Value = serde_json::from_str(&body).unwrap();
        assert!(
            listed["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == "connect"),
            "{listed}"
        );

        // A tool before any grant is the stdio answer, verbatim.
        let (_, body) = exchange(
            port,
            "POST",
            "/mcp",
            Some(&format!("localhost:{port}")),
            Some("http://localhost:3000"),
            &rpc(
                3,
                "tools/call",
                json!({ "name": "feed_local", "arguments": {} }),
            ),
        )
        .await;
        let called: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(called["result"]["isError"], true, "{called}");
        assert!(
            called["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("call connect"),
            "{called}"
        );

        // A notification takes no answer body.
        let (status, body) = exchange(
            port,
            "POST",
            "/mcp",
            Some(&host),
            None,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        )
        .await;
        assert_eq!((status, body.as_str()), (202, ""));
    })
    .await;
}

#[tokio::test]
async fn a_foreign_host_or_origin_never_reaches_the_method_layer() {
    with_listener(|port| async move {
        let ok_host = format!("127.0.0.1:{port}");
        let ping = rpc(1, "ping", json!({}));
        // DNS rebinding: the page's origin is foreign, or the Host is a name
        // that merely resolved to loopback, or it is missing.
        for (host, origin) in [
            (Some(ok_host.as_str()), Some("https://evil.example")),
            (Some(ok_host.as_str()), Some("null")),
            (
                Some(ok_host.as_str()),
                Some("http://localhost.evil.example"),
            ),
            (Some(format!("evil.example:{port}").as_str()), None),
            (Some("127.0.0.1:1"), None),
            (None, None),
        ] {
            let (status, body) = exchange(port, "POST", "/mcp", host, origin, &ping).await;
            assert_eq!(status, 403, "host {host:?}, origin {origin:?}: {body}");
            assert!(
                !body.contains("jsonrpc"),
                "refused before the method layer: {body}"
            );
        }
        // Wrong path and wrong verbs are plain HTTP refusals.
        assert_eq!(
            exchange(port, "POST", "/other", Some(&ok_host), None, &ping)
                .await
                .0,
            404
        );
        assert_eq!(
            exchange(port, "GET", "/mcp", Some(&ok_host), None, "")
                .await
                .0,
            405
        );
        assert_eq!(
            exchange(port, "DELETE", "/mcp", Some(&ok_host), None, "")
                .await
                .0,
            405
        );
    })
    .await;
}
