//! The release feed's round trip, and the once-per-sign-in look built on it.
//!
//! **The rule** (`installers/README.md` § Knowing a newer version is out, as
//! amended 2026-10-03): an app that checks for a newer version does two things
//! and no more — it answers when the user asks, and once per sign-in it looks
//! by itself and shows the same notice. Never a timer, never a download, never
//! a self-replacement. This module is the shared half of both: the HTTP round
//! trip to the release feed ([`fetch_latest_release`]), the asked check
//! ([`check_for_newer_release`]) and the sign-in look ([`look_at_sign_in`]),
//! so no app keeps its own copy of any. Rust apps call them directly; windows
//! and macOS reach the same functions through `fauna-ffi`'s `version` module.
//! What the feed
//! looks like and what counts as *newer* stay in `fauna_core::version`.
//!
//! **A failed look is silent.** The asked check reports its own failure where
//! the user pressed the button; the unasked look has nobody waiting on it, so
//! every failure — offline, a rate-limited feed, a body that is not the feed's
//! shape — folds to "nothing to say" (`None`), never an error surface.
//!
//! **Where the look reads from.** The target is the nest the app has just
//! signed in to, which follows the release channel itself; until the nest
//! carries that field the look reads the release host, as the asked check
//! does. When it lands, [`look_at_sign_in`] is the one place that changes.

use std::future::Future;

/// Read the release feed's "newest release" answer off `origin` (production:
/// `fauna_core::version::GITHUB_API_ORIGIN`; under e2e automation, the
/// harness's stub feed) and return its body.
///
/// `user_agent` names the calling app (`fauna-tui/0.1.2`) — the feed's host
/// refuses requests without one. Any HTTP status comes back as a body: a
/// rate-limit or not-found answer is not the feed's shape, and
/// `fauna_core::version::newer_release_from_latest_json` folds it to "no
/// update", so callers never special-case the status.
pub async fn fetch_latest_release(
    http: &reqwest::Client,
    origin: &str,
    user_agent: &str,
) -> Result<String, reqwest::Error> {
    http.get(fauna_core::version::latest_release_api_url(origin))
        .header("User-Agent", user_agent)
        .send()
        .await?
        .text()
        .await
}

/// What the asked check found — the three answers the user who pressed the
/// button is told apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewerReleaseCheck {
    /// A newer release is out: its tag as the feed spells it (`v0.2.0`).
    Newer { tag: String },
    /// The feed answered and nothing newer is out.
    UpToDate,
    /// No answer, or one that is not even JSON: the check failed, and the user
    /// reads it that way rather than as "up to date".
    Failed,
}

/// The asked check: one round trip to the feed, folded into the answer the
/// door paints. Unlike [`look_at_sign_in`], a failure is reported — the user
/// is waiting on it. `fetch` produces the feed body — in production
/// [`fetch_latest_release`] (see [`check_for_newer_release_over_http`]).
pub async fn check_for_newer_release<F, Fut, E>(current: &str, fetch: F) -> NewerReleaseCheck
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<String, E>>,
    E: std::fmt::Display,
{
    let body = match fetch().await {
        Ok(body) => body,
        Err(e) => {
            tracing::warn!("[update_look] release feed unreachable: {e}");
            return NewerReleaseCheck::Failed;
        }
    };
    match fauna_core::version::newer_release_from_latest_json(current, &body) {
        Some(tag) => NewerReleaseCheck::Newer { tag },
        // Not newer, or not the feed's shape: the shared parse folds both to
        // "no update" — but a body that is not even JSON is a failed check,
        // not an up-to-date one.
        None if serde_json::from_str::<serde_json::Value>(&body).is_ok() => {
            NewerReleaseCheck::UpToDate
        }
        None => NewerReleaseCheck::Failed,
    }
}

/// [`check_for_newer_release`] over the real feed: the form every app's
/// "Check for Updates" door calls.
pub async fn check_for_newer_release_over_http(
    http: &reqwest::Client,
    origin: &str,
    user_agent: &str,
    current: &str,
) -> NewerReleaseCheck {
    check_for_newer_release(current, || fetch_latest_release(http, origin, user_agent)).await
}

/// The once-per-sign-in look: whether a release newer than `current` is out,
/// as its tag (`v0.2.0`, the feed's spelling) — or `None` when it is not, or
/// when the look failed in any way (silent by rule).
///
/// Each app calls this once from its post-sign-in hook and paints the same
/// notice its asked check paints. `fetch` produces the feed body — in
/// production [`fetch_latest_release`] (see [`look_at_sign_in_over_http`]);
/// a test hands in its own answer.
pub async fn look_at_sign_in<F, Fut, E>(current: &str, fetch: F) -> Option<String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<String, E>>,
    E: std::fmt::Display,
{
    match fetch().await {
        Ok(body) => fauna_core::version::newer_release_from_latest_json(current, &body),
        Err(e) => {
            tracing::debug!("[update_look] sign-in look failed silently: {e}");
            None
        }
    }
}

/// [`look_at_sign_in`] over the real feed: the form every app's post-sign-in
/// hook calls.
pub async fn look_at_sign_in_over_http(
    http: &reqwest::Client,
    origin: &str,
    user_agent: &str,
    current: &str,
) -> Option<String> {
    look_at_sign_in(current, || fetch_latest_release(http, origin, user_agent)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn look(current: &str, answer: Result<&str, &str>) -> Option<String> {
        let answer = answer.map(str::to_owned).map_err(str::to_owned);
        look_at_sign_in(current, || async move { answer }).await
    }

    async fn ask(current: &str, answer: Result<&str, &str>) -> NewerReleaseCheck {
        let answer = answer.map(str::to_owned).map_err(str::to_owned);
        check_for_newer_release(current, || async move { answer }).await
    }

    #[tokio::test]
    async fn the_asked_check_tells_its_three_answers_apart() {
        assert_eq!(
            ask("0.1.2", Ok(r#"{"tag_name":"v0.2.0"}"#)).await,
            NewerReleaseCheck::Newer {
                tag: "v0.2.0".to_owned()
            }
        );
        assert_eq!(
            ask("0.2.0", Ok(r#"{"tag_name":"v0.2.0"}"#)).await,
            NewerReleaseCheck::UpToDate
        );
        // The feed answered in JSON, just not with a release (a rate limit):
        // nothing newer to report, so up to date — the shared parse's reading.
        assert_eq!(
            ask("0.1.2", Ok(r#"{"message":"API rate limit"}"#)).await,
            NewerReleaseCheck::UpToDate
        );
        // No round trip, or a body that is not JSON at all: the user asked and
        // is told the check failed, never that they are up to date.
        assert_eq!(
            ask("0.1.2", Err("connection refused")).await,
            NewerReleaseCheck::Failed
        );
        assert_eq!(
            ask("0.1.2", Ok("<html>502</html>")).await,
            NewerReleaseCheck::Failed
        );
    }

    #[tokio::test]
    async fn a_newer_release_is_named_by_its_tag() {
        assert_eq!(
            look("0.1.2", Ok(r#"{"tag_name":"v0.2.0"}"#)).await,
            Some("v0.2.0".to_owned())
        );
    }

    #[tokio::test]
    async fn the_same_or_an_older_release_says_nothing() {
        assert_eq!(look("0.2.0", Ok(r#"{"tag_name":"v0.2.0"}"#)).await, None);
        assert_eq!(look("0.3.0", Ok(r#"{"tag_name":"v0.2.0"}"#)).await, None);
    }

    #[tokio::test]
    async fn a_failed_look_says_nothing() {
        // The round trip failed outright.
        assert_eq!(look("0.1.2", Err("connection refused")).await, None);
        // The feed answered, but not with a release (rate limit, not found,
        // an HTML error page).
        assert_eq!(
            look("0.1.2", Ok(r#"{"message":"API rate limit"}"#)).await,
            None
        );
        assert_eq!(look("0.1.2", Ok("<html>502</html>")).await, None);
    }

    /// One HTTP answer from a loopback listener — the production round trip
    /// end to end, its origin the only thing moved.
    async fn serve_once(body: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let n = sock.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]);
            assert!(
                request.starts_with("GET /repos/faunasocial/fauna/releases/latest "),
                "{request}"
            );
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("user-agent: fauna-test/")
            );
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(reply.as_bytes()).await.unwrap();
        });
        origin
    }

    #[tokio::test]
    async fn the_http_look_reads_the_shared_endpoint_on_the_given_origin() {
        let origin = serve_once(r#"{"tag_name":"v9.9.9"}"#).await;
        let http = reqwest::Client::new();
        assert_eq!(
            look_at_sign_in_over_http(&http, &origin, "fauna-test/0", "0.1.2").await,
            Some("v9.9.9".to_owned())
        );
    }

    #[tokio::test]
    async fn the_http_check_reads_the_shared_endpoint_and_fails_out_loud() {
        let origin = serve_once(r#"{"tag_name":"v9.9.9"}"#).await;
        let http = reqwest::Client::new();
        assert_eq!(
            check_for_newer_release_over_http(&http, &origin, "fauna-test/0", "0.1.2").await,
            NewerReleaseCheck::Newer {
                tag: "v9.9.9".to_owned()
            }
        );
        let closed = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            format!("http://{}", l.local_addr().unwrap())
        };
        assert_eq!(
            check_for_newer_release_over_http(&http, &closed, "fauna-test/0", "0.1.2").await,
            NewerReleaseCheck::Failed
        );
    }

    #[tokio::test]
    async fn the_http_look_is_silent_when_nothing_answers() {
        // Bind and drop: the port is closed by the time the look dials it.
        let origin = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            format!("http://{}", l.local_addr().unwrap())
        };
        let http = reqwest::Client::new();
        assert_eq!(
            look_at_sign_in_over_http(&http, &origin, "fauna-test/0", "0.1.2").await,
            None
        );
    }
}
