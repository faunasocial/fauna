//! tier_1: a principal session the nest dropped (`apps/cli.md` § The device
//! shell → *A dropped session*) and the account a call names, against a fake
//! [`Host`] whose first session drops every request with a lost connection.
//! A read is redialled and run again on the new session; a write is never
//! retried blind — the catalog's `read_only` annotation decides, so an
//! idempotent write is not retried either.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use fauna_cli::tools::{ConnectStarted, WRITE_NOT_RETRIED};
use fauna_cli::{Host, Server, SessionUnavailable, ToolError};
use fauna_client_testkit::{ClassifiedError, RejectingRequester, block_on};
use fauna_protocol::post_author::{KIND_CREATE, PostAuthorCreateReply};
use fauna_protocol::{RpcError, RpcErrorClass, RpcRequester};
use serde_json::{Value, json};

#[derive(Debug)]
enum DropError {
    /// The socket went away under the request.
    Lost,
    Answered(ClassifiedError),
}

impl core::fmt::Display for DropError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DropError::Lost => write!(f, "connection lost"),
            DropError::Answered(e) => write!(f, "{e}"),
        }
    }
}

impl RpcErrorClass for DropError {
    fn as_rpc_error(&self) -> Option<&RpcError> {
        match self {
            DropError::Lost => None,
            DropError::Answered(e) => e.as_rpc_error(),
        }
    }

    fn is_rejection(&self) -> bool {
        self.as_rpc_error().is_some()
    }

    fn is_connection_lost(&self) -> bool {
        matches!(self, DropError::Lost)
    }
}

/// A session that either drops every request (`dropped`) or answers from its
/// table; every kind it was asked, either way.
struct Session {
    dropped: bool,
    table: RejectingRequester,
    asked: Mutex<Vec<&'static str>>,
}

impl RpcRequester for Session {
    type Error = DropError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.asked.lock().unwrap().push(kind);
        if self.dropped {
            return Err(DropError::Lost);
        }
        self.table
            .request(kind, payload)
            .await
            .map_err(DropError::Answered)
    }
}

/// The fake shell: its live session drops until a redial replaces it with one
/// answering the same table.
struct FakeHost {
    live: Mutex<Arc<Session>>,
    healthy: fn() -> RejectingRequester,
    redials: AtomicUsize,
    /// The `account` each call named, in order.
    named: Mutex<Vec<Option<String>>>,
}

impl FakeHost {
    fn new(healthy: fn() -> RejectingRequester) -> Self {
        Self {
            live: Mutex::new(Arc::new(Session {
                dropped: true,
                table: healthy(),
                asked: Mutex::new(vec![]),
            })),
            healthy,
            redials: AtomicUsize::new(0),
            named: Mutex::new(vec![]),
        }
    }

    fn asked(&self) -> Vec<&'static str> {
        self.live.lock().unwrap().asked.lock().unwrap().clone()
    }
}

impl Host for FakeHost {
    type Rpc = Arc<Session>;

    async fn session(&self, account: Option<&str>) -> Result<Self::Rpc, SessionUnavailable> {
        self.named.lock().unwrap().push(account.map(str::to_owned));
        Ok(self.live.lock().unwrap().clone())
    }

    async fn account(&self, _account: Option<&str>) -> Result<String, SessionUnavailable> {
        Ok("a1".repeat(32))
    }

    async fn redial(
        &self,
        account: Option<&str>,
        lost: &Self::Rpc,
    ) -> Result<Self::Rpc, SessionUnavailable> {
        self.named.lock().unwrap().push(account.map(str::to_owned));
        let mut live = self.live.lock().unwrap();
        if Arc::ptr_eq(&live, lost) {
            self.redials.fetch_add(1, Ordering::SeqCst);
            *live = Arc::new(Session {
                dropped: false,
                table: (self.healthy)(),
                asked: Mutex::new(vec![]),
            });
        }
        Ok(live.clone())
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

fn call(server: &Server<FakeHost>, name: &str, arguments: Value) -> Value {
    let message = json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                          "params": { "name": name, "arguments": arguments } });
    block_on(server.handle(&message.to_string())).expect("answered")["result"].clone()
}

fn feeds() -> RejectingRequester {
    #[derive(serde::Serialize)]
    struct Feeds {
        feeds: Vec<()>,
    }
    RejectingRequester::new().reply("fauna.feed.list", &Feeds { feeds: vec![] })
}

#[test]
fn a_read_whose_session_dropped_is_redialled_and_run_again() {
    let s = Server::new(FakeHost::new(feeds));
    let r = call(&s, "feed_list", json!({}));
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["feeds"], json!([]));
    assert_eq!(s.host().redials.load(Ordering::SeqCst), 1);
    assert_eq!(s.host().asked(), vec!["fauna.feed.list"]);
}

#[test]
fn a_write_whose_session_dropped_is_redialled_but_never_retried() {
    fn created() -> RejectingRequester {
        RejectingRequester::new().reply(
            KIND_CREATE,
            &PostAuthorCreateReply {
                post_id: "ab".repeat(32),
                ..Default::default()
            },
        )
    }
    let s = Server::new(FakeHost::new(created));
    let r = call(&s, "post_create", json!({ "text": "hello" }));
    assert_eq!(r["isError"], true, "{r}");
    assert_eq!(r["content"][0]["text"], WRITE_NOT_RETRIED);
    // Redialled, so the next call has a session — but the new session was
    // never asked to post.
    assert_eq!(s.host().redials.load(Ordering::SeqCst), 1);
    assert!(s.host().asked().is_empty());
    let again = call(&s, "post_create", json!({ "text": "hello" }));
    assert_eq!(again["structuredContent"]["post_id"], "ab".repeat(32));
}

#[test]
fn an_idempotent_write_is_still_a_write() {
    fn marked() -> RejectingRequester {
        use fauna_protocol::notifications::NotifMarkReadReply;
        RejectingRequester::new().reply(
            "fauna.notifications.mark_read",
            &NotifMarkReadReply {
                marked_read: 2,
                extra: Default::default(),
            },
        )
    }
    let tool = fauna_cli::catalog::tool("notifications_mark_read").unwrap();
    assert!(tool.annotations.idempotent && !tool.annotations.read_only);
    let s = Server::new(FakeHost::new(marked));
    let r = call(&s, "notifications_mark_read", json!({}));
    assert_eq!(r["content"][0]["text"], WRITE_NOT_RETRIED);
    assert!(s.host().asked().is_empty());
}

#[test]
fn a_rejection_is_the_nests_answer_and_redials_nothing() {
    fn refusing() -> RejectingRequester {
        RejectingRequester::new().reject(
            "fauna.feed.list",
            RpcError::new("fauna.auth.permission_denied", "errors.permission_denied"),
        )
    }
    let host = FakeHost::new(refusing);
    *host.live.lock().unwrap() = Arc::new(Session {
        dropped: false,
        table: refusing(),
        asked: Mutex::new(vec![]),
    });
    let s = Server::new(host);
    let r = call(&s, "feed_list", json!({}));
    assert_eq!(r["isError"], true);
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("fauna.auth.permission_denied")
    );
    assert_eq!(s.host().redials.load(Ordering::SeqCst), 0);
}

#[test]
fn a_resource_read_whose_session_dropped_is_read_again() {
    fn profile() -> RejectingRequester {
        use fauna_core::data::{InboxMode, Profile, Timestamp};
        use fauna_core::identity::ActorKeypair;
        use fauna_protocol::profile::ProfileGetReply;
        let kp = ActorKeypair::generate();
        let profile = Profile {
            actor_id: kp.actor_id(),
            display_name: Some("Alice".into()),
            bio: None,
            avatar: None,
            banner: None,
            links: vec![],
            nests: vec![],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: InboxMode::Open,
            recovery_head: None,
            audience: fauna_core::data::ProfileAudience::ThisNest,
            updated_at: Timestamp(0),
        };
        let body = fauna_client_profile::build_profile(&kp, &profile).expect("signed profile");
        RejectingRequester::new().reply(
            "fauna.profile.get",
            &ProfileGetReply {
                body: fauna_protocol::ByteBuf::from(body),
                ..Default::default()
            },
        )
    }
    let s = Server::new(FakeHost::new(profile));
    let message = json!({ "jsonrpc": "2.0", "id": 9, "method": "resources/read",
                          "params": { "uri": "nest://profile?account=alice.example" } });
    let r = block_on(s.handle(&message.to_string())).unwrap();
    assert!(r.get("error").is_none(), "{r}");
    assert_eq!(s.host().redials.load(Ordering::SeqCst), 1);
    assert_eq!(
        s.host().named.lock().unwrap()[0].as_deref(),
        Some("alice.example")
    );
}

#[test]
fn a_call_hands_its_account_to_the_shell() {
    let s = Server::new(FakeHost::new(feeds));
    call(&s, "feed_list", json!({ "account": "bob.example" }));
    call(&s, "feed_list", json!({}));
    let named = s.host().named.lock().unwrap().clone();
    // The first call and its redial name bob; the second names no one.
    assert_eq!(
        named,
        vec![Some("bob.example".into()), Some("bob.example".into()), None]
    );
    let bad = call(&s, "feed_list", json!({ "account": 7 }));
    assert_eq!(bad["isError"], true);
}
