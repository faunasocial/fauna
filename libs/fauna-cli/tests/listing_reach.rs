//! tier_1: what the shell lists follows the held connections' granted scope
//! (`apps/cli.md` § One catalog, many front-ends → *A tool that cannot run*,
//! rule 2): the whole catalog before any connection, only what a grant
//! reaches once one is held, each change announced on stdio with
//! `notifications/tools/list_changed`; the streamable-HTTP shell, which has no
//! event stream, keeps the whole catalog. And an admin connect that waits is
//! told apart from a member's.
#![cfg(feature = "device")]

use std::sync::{Arc, Mutex};

use fauna_cli::catalog::consent_scope;
use fauna_cli::reach::{is_admin_tool, parse_scope};
use fauna_cli::tools::ConnectStarted;
use fauna_cli::{CATALOG, Host, Reach, Server, SessionUnavailable, ToolError};
use fauna_client_testkit::RejectingRequester;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Notify;

/// A shell whose reach the test sets, waking the transport as a landed
/// connection does.
struct ReachHost {
    reach: Mutex<Reach>,
    changed: Notify,
    pending: Option<SessionUnavailable>,
}

impl ReachHost {
    fn new(reach: Reach) -> Self {
        Self {
            reach: Mutex::new(reach),
            changed: Notify::new(),
            pending: None,
        }
    }

    fn set(&self, reach: Reach) {
        *self.reach.lock().unwrap() = reach;
        self.changed.notify_one();
    }
}

impl Host for ReachHost {
    type Rpc = Arc<RejectingRequester>;

    async fn session(&self, _account: Option<&str>) -> Result<Self::Rpc, SessionUnavailable> {
        match &self.pending {
            Some(why) => Err(why.clone()),
            None => Err(SessionUnavailable::NotConnected),
        }
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
        Ok(Default::default())
    }

    async fn reach(&self) -> Reach {
        self.reach.lock().unwrap().clone()
    }

    async fn reach_changed(&self) {
        self.changed.notified().await
    }

    fn conversations_door(&self) -> bool {
        true
    }
}

fn member_grant() -> Reach {
    Reach::of([Some(parse_scope(&consent_scope(false)))])
}

fn names(listed: &Value) -> Vec<String> {
    listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

fn admin_names() -> Vec<&'static str> {
    CATALOG
        .iter()
        .filter(|t| is_admin_tool(t))
        .map(|t| t.name)
        .collect()
}

/// Serve `server` over an in-memory stdio pair; the test writes lines and
/// reads every line the server wrote.
struct Stdio {
    to_server: tokio::io::DuplexStream,
    from_server: tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
}

impl Stdio {
    async fn send(&mut self, message: Value) {
        let mut line = message.to_string();
        line.push('\n');
        self.to_server.write_all(line.as_bytes()).await.unwrap();
    }

    async fn next(&mut self) -> Value {
        let line = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.from_server.next_line(),
        )
        .await
        .expect("the server wrote a line in time")
        .unwrap()
        .expect("the stream is open");
        serde_json::from_str(&line).unwrap()
    }
}

fn stdio(server: Arc<Server<ReachHost>>) -> Stdio {
    let (to_server, server_in) = tokio::io::duplex(1 << 20);
    let (server_out, from_server) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        fauna_cli::stdio::serve(&server, BufReader::new(server_in), server_out)
            .await
            .unwrap();
    });
    Stdio {
        to_server,
        from_server: BufReader::new(from_server).lines(),
    }
}

#[tokio::test]
async fn before_a_connection_stdio_lists_the_whole_catalog_and_advertises_list_changed() {
    let server = Arc::new(Server::announcing(ReachHost::new(Reach::Whole)));
    let mut io = stdio(server);
    io.send(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": { "protocolVersion": "2025-06-18" } }))
        .await;
    let init = io.next().await;
    assert_eq!(init["result"]["capabilities"]["tools"]["listChanged"], true);
    io.send(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
        .await;
    assert_eq!(names(&io.next().await).len(), CATALOG.len());
}

#[tokio::test]
async fn a_connection_granting_no_admin_arm_drops_every_admin_tool_and_is_announced() {
    let server = Arc::new(Server::announcing(ReachHost::new(Reach::Whole)));
    let mut io = stdio(server.clone());
    io.send(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }))
        .await;
    io.next().await;

    // The approval lands in the background, between two requests.
    server.host().set(member_grant());
    let note = io.next().await;
    assert_eq!(note["method"], "notifications/tools/list_changed", "{note}");
    assert!(note.get("id").is_none(), "a notification carries no id");

    io.send(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
        .await;
    let listed = names(&io.next().await);
    for admin in admin_names() {
        assert!(!listed.iter().any(|n| n == admin), "{admin} is listed");
    }
    assert!(listed.iter().any(|n| n == "connect"));
    assert!(listed.iter().any(|n| n == "feed_list"));

    // The connection ends (revoked, failed): the listing is whole again,
    // announced the same way.
    server.host().set(Reach::Whole);
    assert_eq!(
        io.next().await["method"],
        "notifications/tools/list_changed"
    );
}

#[tokio::test]
async fn a_reach_change_that_lists_the_same_tools_is_not_announced() {
    let server = Arc::new(Server::announcing(ReachHost::new(member_grant())));
    let mut io = stdio(server.clone());
    server.host().set(member_grant());
    io.send(json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }))
        .await;
    let answer = io.next().await;
    assert_eq!(answer["id"], 1, "the ping's answer comes first: {answer}");
}

#[tokio::test]
async fn the_http_shell_keeps_the_whole_catalog_listed() {
    let server = Server::new(ReachHost::new(member_grant()));
    let init = server
        .handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }).to_string())
        .await
        .unwrap();
    assert_eq!(
        init["result"]["capabilities"]["tools"]["listChanged"],
        false
    );
    let listed = server
        .handle(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }).to_string())
        .await
        .unwrap();
    assert_eq!(names(&listed).len(), CATALOG.len());
}

/// An admin connect that waits is told apart from a member's: its answer is
/// the waiting sentence with its code, then the way out — connecting without
/// the admin arms.
#[tokio::test]
async fn an_admin_connect_that_waits_answers_its_code() {
    let mut host = ReachHost::new(Reach::Whole);
    host.pending = Some(SessionUnavailable::PendingAdmin {
        user_code: "ADMN-1234".into(),
    });
    let r = Server::new(host)
        .handle(
            &json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                     "params": { "name": "feed_local", "arguments": {} } })
            .to_string(),
        )
        .await
        .unwrap();
    let text = r["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(r["result"]["isError"], true);
    assert!(text.contains("ADMN-1234"), "{text}");
    assert!(
        text.starts_with("Waiting for the user to approve"),
        "{text}"
    );
    assert!(text.contains("without admin: true"), "{text}");
    assert!(text.contains("does not administer its server"), "{text}");
}
