//! tier_1 for [`fauna_client::RangedNestReader`] — the one pinned leg every
//! native inline player reads from (security.md § Transport trust → *The
//! native-player leg*; render-model.md § D6c → *Inline playback*, answer (1)).
//!
//! Driven against the shared self-signed TLS fixture server
//! (`common::spawn_self_signed_server`) through the PRODUCTION trust path: the
//! reader builds its own `pinned_http_client`, which reads the process-global
//! SPKI pin store, and each test seeds that store the way a graduated WS
//! handshake would (`trust::pin_spki_for_test`). Every fixture port is its own
//! pin key, so the tests share the global store without colliding.
//!
//! The fixture is loopback, which makes the trust half the STRICTER case: the
//! reqwest leg has no loopback carve-out (that waiver lives only in the platform
//! HTTP callbacks — `is_loopback_authority`), so a same-box self-signed nest is
//! read iff its SPKI is pinned, exactly like a LAN one.

mod common;

use std::sync::{Arc, Mutex};

use fauna_client::RangedNestReader;
use fauna_client::trust::pin_spki_for_test;

const LEN: usize = 1000;
const PATH: &str = "/api/v1/blob/abc123";

fn body() -> Vec<u8> {
    (0..LEN).map(|i| (i % 251) as u8).collect()
}

/// How the fixture answers a GET.
#[derive(Clone, Copy)]
enum Server {
    /// Honours `Range`: 206 + `Content-Range: bytes a-b/1000`, 416 past the end.
    Ranged,
    /// Honours `Range` but names no total (`bytes a-b/*`) — the media proxy's
    /// absent-upstream-length relay.
    RangedNoTotal,
    /// Ignores `Range`: a whole-body 200 with `Content-Length`.
    NotRanged,
}

fn parse_range(head: &str) -> Option<(usize, Option<usize>)> {
    let line = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("range:"))?;
    let spec = line.split_once("bytes=")?.1.trim();
    let (a, b) = spec.split_once('-')?;
    Some((a.parse().ok()?, b.parse().ok()))
}

fn respond(kind: Server, head: &str) -> Vec<u8> {
    let body = body();
    let ctype = "Content-Type: video/webm\r\n";
    let range = parse_range(head);
    match (kind, range) {
        (Server::NotRanged, _) | (_, None) => {
            let mut r = format!(
                "HTTP/1.1 200 OK\r\n{ctype}Content-Length: {LEN}\r\nConnection: close\r\n\r\n"
            )
            .into_bytes();
            r.extend_from_slice(&body);
            r
        }
        (kind, Some((a, _))) if a >= LEN => {
            let total = match kind {
                Server::Ranged => LEN.to_string(),
                _ => "*".to_string(),
            };
            format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .into_bytes()
        }
        (kind, Some((a, b))) => {
            let end = b.unwrap_or(LEN - 1).min(LEN - 1);
            let total = match kind {
                Server::Ranged => LEN.to_string(),
                _ => "*".to_string(),
            };
            let slice = &body[a..=end];
            let mut r = format!(
                "HTTP/1.1 206 Partial Content\r\n{ctype}Content-Range: bytes {a}-{end}/{total}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                slice.len()
            )
            .into_bytes();
            r.extend_from_slice(slice);
            r
        }
    }
}

/// A fixture of `kind`, its SPKI, and the log of every request head it saw.
async fn fixture(kind: Server) -> (String, [u8; 32], Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let (port, spki) = common::spawn_self_signed_server(move |head| {
        log.lock().unwrap().push(head.to_string());
        respond(kind, head)
    })
    .await;
    (format!("https://127.0.0.1:{port}"), spki, seen)
}

/// A pinned fixture of `kind`, already opened.
async fn opened(kind: Server) -> (RangedNestReader, Arc<Mutex<Vec<String>>>) {
    let (url, spki, seen) = fixture(kind).await;
    pin_spki_for_test(&url, spki);
    let reader = RangedNestReader::open(&url, PATH)
        .await
        .expect("a pinned self-signed nest opens");
    (reader, seen)
}

#[tokio::test]
async fn open_probes_len_and_content_type() {
    let (reader, seen) = opened(Server::Ranged).await;
    assert_eq!(reader.len(), Some(LEN as u64));
    assert_eq!(reader.content_type(), Some("video/webm"));
    assert!(reader.supports_ranges());
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "open is one request");
    assert!(seen[0].starts_with(&format!("GET {PATH} ")));
    assert!(seen[0].to_ascii_lowercase().contains("range: bytes=0-0"));
}

#[tokio::test]
async fn read_at_returns_the_window_at_an_offset() {
    let (reader, seen) = opened(Server::Ranged).await;
    assert_eq!(reader.read_at(100, 50).await.unwrap(), body()[100..150]);
    let seen = seen.lock().unwrap();
    assert!(
        seen[1]
            .to_ascii_lowercase()
            .contains("range: bytes=100-149")
    );
}

#[tokio::test]
async fn read_at_truncates_at_eof() {
    let (reader, _) = opened(Server::Ranged).await;
    assert_eq!(reader.read_at(990, 50).await.unwrap(), body()[990..]);
}

#[tokio::test]
async fn read_at_past_the_end_is_empty() {
    let (reader, seen) = opened(Server::Ranged).await;
    assert!(reader.read_at(LEN as u64, 10).await.unwrap().is_empty());
    assert_eq!(
        seen.lock().unwrap().len(),
        2,
        "the 416 was asked, not assumed"
    );
}

#[tokio::test]
async fn zero_length_read_issues_no_request() {
    let (reader, seen) = opened(Server::Ranged).await;
    assert!(reader.read_at(10, 0).await.unwrap().is_empty());
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_non_ranging_server_is_read_correctly() {
    let (reader, _) = opened(Server::NotRanged).await;
    assert_eq!(reader.len(), Some(LEN as u64), "the 200's Content-Length");
    assert_eq!(reader.content_type(), Some("video/webm"));
    assert!(!reader.supports_ranges());
    assert_eq!(reader.read_at(300, 25).await.unwrap(), body()[300..325]);
    assert_eq!(reader.read_at(995, 25).await.unwrap(), body()[995..]);
    assert!(reader.read_at(LEN as u64, 5).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_range_with_no_total_reports_unknown_len() {
    let (reader, _) = opened(Server::RangedNoTotal).await;
    assert_eq!(reader.len(), None);
    assert!(reader.supports_ranges());
    assert_eq!(reader.read_at(500, 20).await.unwrap(), body()[500..520]);
    assert!(reader.read_at(LEN as u64 + 7, 20).await.unwrap().is_empty());
}

#[tokio::test]
async fn every_request_is_ranged() {
    let (reader, seen) = opened(Server::Ranged).await;
    reader.read_at(0, 10).await.unwrap();
    reader.read_at(700, 300).await.unwrap();
    for head in seen.lock().unwrap().iter() {
        assert!(
            head.to_ascii_lowercase().contains("range: bytes="),
            "a whole-body GET was issued: {head}"
        );
    }
}

// ── The trust half: the reader is the pinned leg, nothing weaker ──

#[tokio::test]
async fn an_unpinned_self_signed_nest_is_refused() {
    let (url, _spki, seen) = fixture(Server::Ranged).await;
    assert!(RangedNestReader::open(&url, PATH).await.is_err());
    assert!(
        seen.lock().unwrap().is_empty(),
        "no request reached the box"
    );
}

#[tokio::test]
async fn a_disagreeing_pin_is_refused() {
    let (url, _spki, seen) = fixture(Server::Ranged).await;
    pin_spki_for_test(&url, [0xAA; 32]);
    assert!(RangedNestReader::open(&url, PATH).await.is_err());
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_pin_is_read_per_handshake() {
    // Pinned after open → every later read still goes through; the pin store
    // is consulted per handshake, not frozen into the reader at open.
    let (url, spki, _) = fixture(Server::Ranged).await;
    pin_spki_for_test(&url, spki);
    let reader = RangedNestReader::open(&url, PATH).await.unwrap();
    pin_spki_for_test(&url, [0xBB; 32]);
    assert!(
        reader.read_at(0, 10).await.is_err(),
        "a re-pinned host refuses the old cert"
    );
}
