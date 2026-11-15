//! A refusal the authorization server writes **before it reads the body** must
//! reach a client that asked the server to close the connection
//! (`authorization-server.md` § As built → the `/oauth/par` mount: "a fresh nonce
//! on every response", so a client "recovers from the very response that told it
//! so — including its first request").
//!
//! ## The fault this pins
//!
//! The request-taking OAuth endpoints (`/oauth/par`, `/oauth/token`,
//! `/oauth/revoke`, the two polled consent starts) run the DPoP gate before the
//! body is read, deliberately. A correct client's FIRST request has no nonce, so
//! it is always refused there — on the head alone. A client sending
//! `Connection: close` (Python's urllib always does, and the e2e suite's OAuth
//! client is urllib) whose form reaches the nest after its head — a loaded
//! client, or one that writes the two apart — then finds the server already
//! answered and closed: the form meets a closed socket, which answers it with a
//! TCP RST. On Windows an RST makes the client's receive fail
//! (`WSAECONNABORTED` 10053, `WSAECONNRESET` 10054) and discards the reply it had
//! not yet read, so the client never sees the refusal or the nonce it needs to
//! retry. Measured on Windows: the windows consent test red 2 of 3 at its first PAR
//! post (2026-11-06); through `serve_plain`, a client sending its form 20 ms
//! after the head lost the reply in 196 of 200 posts with no drain, in none of
//! 200 with it (2026-11-13). The Go bridge met the same fault and drains its
//! listeners (`bins/fauna-bridges/internal/httpdrain`); the nest's twin is
//! [`fauna_nest::http_drain`].
//!
//! ## What it asserts
//!
//! Through the nest's real plain-HTTP serving path (`fauna_nest::serve_plain`,
//! the accept loop every listener shares, over the real `build_router`): each
//! endpoint, posted a form [`FORM_GAP`] after its head with `Connection: close`
//! and no proof, answers its OAuth refusal with a `DPoP-Nonce` — and the
//! connection then ENDS CLEANLY (a read of zero bytes), never in a reset. The
//! reset is the observable on every OS: Linux keeps the reply but then fails the
//! next read with `ECONNRESET`; Windows loses the reply too. Each endpoint is
//! posted [`ROUNDS`] times, so a red is not a one-off.
//!
//! ## Timing (convention 14)
//!
//! [`FORM_GAP`] is not a wait-then-assert: it builds the slow client the fault
//! needs. The fixed server's pass does not depend on its length — the drain
//! waits for the form however late it comes, up to `http_drain::DRAIN_TIMEOUT`,
//! far above any gap a loaded box adds — only an unfixed server's failure rate
//! does. Every read is a positive wait for a state the server must reach (its
//! reply and its close), bounded by a generous [`BUDGET`] that only turns a hang
//! into a failure.
//!
//! Tier 1 (`testing.md` § The four-tier taxonomy): in-process nest, loopback.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;

/// Every request-taking OAuth endpoint whose DPoP gate runs before its body.
const PATHS: [&str; 5] = [
    "/oauth/par",
    "/oauth/token",
    "/oauth/revoke",
    "/oauth/device_authorization",
    "/oauth/bc-authorize",
];

/// How often each endpoint is posted: enough that a red is not a one-off, well
/// under the loopback budget the endpoints share
/// (`oauth_as_rate_limit::OAUTH_LOOPBACK_LIMIT`).
const ROUNDS: usize = 20;

/// The form's size: about a real first PAR form's.
const FORM_BYTES: usize = 512;

/// How long after its head the client sends its form — the slow client the
/// fault needs (see the module docs, *Timing*).
const FORM_GAP: Duration = Duration::from_millis(20);

/// Bounds a read that must complete; never an assertion on how long it took.
const BUDGET: Duration = Duration::from_secs(30);

async fn serve() -> SocketAddr {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    // A claimed domain, so the endpoints answer as an issuer (the gate and its
    // nonce) rather than with the domainless `503`.
    state
        .identity_domain
        .store(Some(Arc::new("nest.example".to_string())));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // spawn-ok(test): the in-process nest's accept loop; the test binary
    // exiting is its teardown.
    tokio::spawn(fauna_nest::serve_plain(
        listener,
        fauna_nest::build_router(state).into_make_service(),
        fauna_conn_limit::PerIpConnLimit::with_loopback_ceiling(
            fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP,
            fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP,
        ),
    ));
    addr
}

/// What one POST saw: the bytes that arrived, and how the exchange ended (the
/// first failure, a write's or a read's).
struct Seen {
    bytes: Vec<u8>,
    end: Result<(), std::io::Error>,
}

impl Seen {
    fn head(&self) -> String {
        let text = String::from_utf8_lossy(&self.bytes);
        text.split("\r\n\r\n")
            .next()
            .unwrap_or_default()
            .to_string()
    }

    fn status(&self) -> Option<u16> {
        let head = self.head();
        let line = head.lines().next()?;
        line.split_whitespace().nth(1)?.parse().ok()
    }

    fn has_nonce(&self) -> bool {
        self.head()
            .lines()
            .any(|l| l.to_ascii_lowercase().starts_with("dpop-nonce:"))
    }

    fn clean(&self) -> bool {
        self.status().is_some_and(|s| (400..500).contains(&s))
            && self.has_nonce()
            && self.end.is_ok()
    }

    fn describe(&self) -> String {
        match &self.end {
            Ok(()) => format!(
                "status {:?}, nonce {}, clean close",
                self.status(),
                self.has_nonce()
            ),
            Err(e) => format!(
                "status {:?}, nonce {}, {} byte(s) then {:?} ({e})",
                self.status(),
                self.has_nonce(),
                self.bytes.len(),
                e.kind()
            ),
        }
    }
}

/// POST a form with `Connection: close` and no DPoP proof, the form
/// [`FORM_GAP`] after the head, then read until the server closes. A failed
/// write is kept as the end and the reply is still read for, since what the
/// client can recover after it is the question.
fn post(addr: SocketAddr, path: &str) -> Seen {
    let form = format!(
        "client_id=https%3A%2F%2Fapp.example%2Fclient.json&pad={}",
        "a".repeat(FORM_BYTES)
    );
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: nest.example\r\nContent-Type: application/x-www-form-urlencoded\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        form.len()
    );
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(BUDGET)).unwrap();
    let mut end = stream.write_all(head.as_bytes()).and_then(|()| {
        // The slow client the fault needs, not a wait before an assertion
        // (module docs, *Timing*).
        std::thread::sleep(FORM_GAP);
        stream.write_all(form.as_bytes())
    });
    let mut bytes = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                if end.is_ok() {
                    end = Err(e);
                }
                break;
            }
        }
    }
    Seen { bytes, end }
}

/// The headline: on every gate-before-body endpoint, the refusal and its nonce
/// reach a `Connection: close` client whose form came after its head, and the
/// close is a clean end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_before_the_body_reaches_a_closing_client_with_its_nonce() {
    let addr = serve().await;
    let mut failures = Vec::new();
    for path in PATHS {
        let seen = tokio::task::spawn_blocking(move || {
            (0..ROUNDS).map(|_| post(addr, path)).collect::<Vec<_>>()
        })
        .await
        .unwrap();
        let bad: Vec<String> = seen
            .iter()
            .filter(|s| !s.clean())
            .map(Seen::describe)
            .collect();
        if !bad.is_empty() {
            failures.push(format!(
                "{path}: {} of {ROUNDS} posts did not end in a refusal with its nonce and a clean close — first: {}",
                bad.len(),
                bad[0]
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "the server closed before the client's form was in, so the form met a closed socket and the \
         close was a reset (on Windows the reply is lost with it):\n{}",
        failures.join("\n")
    );
}
