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
//! **Where the look reads from.** The nest the app has just signed in to —
//! its `fauna.nest.info` reply's `newest_release` — so the unasked look
//! contacts no third party; a nest that says nothing means no notice, never a
//! fall-back to the release host (`installers/README.md` § Knowing a newer
//! version is out → *The nest's newest-release field*). The asked check
//! always reads the release host. [`look_at_sign_in_over_http`] is the
//! release-host look macOS and windows still reach through `fauna-ffi` until
//! each moves to the nest-reading face; nothing else may call it.

use std::future::Future;

use fauna_protocol::RpcRequester;
#[cfg(test)]
use fauna_protocol::discovery::NestInfoReply;

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

/// The once-per-sign-in look: whether the nest the app has just signed in to
/// knows of a release newer than `current`, as that release's tag (`v0.2.0`,
/// the spelling the notice and [`fauna_core::version::release_page_url`]
/// take) — or `None` when it does not, when it says nothing (an older nest,
/// its update check off), or when the read failed (silent by rule).
///
/// Each app calls this once from its post-sign-in hook, with that sign-in's
/// nest, and paints the same notice its asked check paints. One read of
/// `fauna.nest.info` and nothing else: no request reaches the release host —
/// and that read is the connection's shared one
/// ([`fauna_protocol::read_nest_info`]), so a settings landing loaded on the
/// same connection draws no second.
pub async fn look_at_sign_in<R: RpcRequester>(nest: &R, current: &str) -> Option<String> {
    match fauna_protocol::read_nest_info(nest).await {
        Ok(reply) => newer_release_from_nest(current, reply.newest_release.as_deref()),
        Err(e) => {
            tracing::debug!("[update_look] sign-in look failed silently: {e}");
            None
        }
    }
}

/// The pure half of [`look_at_sign_in`]: the tag to paint for the newest
/// release a nest reported, when it is newer than `current`.
pub fn newer_release_from_nest(current: &str, newest_release: Option<&str>) -> Option<String> {
    let newest = newest_release?.trim_start_matches('v');
    fauna_core::version::is_newer(current, newest).then(|| format!("v{newest}"))
}

/// The notice an app signed in to several nests keeps: the newer of the tag
/// it already shows and the one a later sign-in's look found, so within one
/// run of the app the notice only ever moves up (`installers/README.md`
/// § Knowing a newer version is out → *Several nests*).
pub fn keep_the_newer(shown: Option<String>, found: Option<String>) -> Option<String> {
    match (shown, found) {
        (Some(shown), Some(found))
            if fauna_core::version::is_newer(
                shown.trim_start_matches('v'),
                found.trim_start_matches('v'),
            ) =>
        {
            Some(found)
        }
        (Some(shown), _) => Some(shown),
        (None, found) => found,
    }
}

/// The release-host look — **interim**: the sign-in look macOS and windows
/// still make through `fauna-ffi` until each moves to [`look_at_sign_in`].
/// Whether a release newer than `current` is out, as its feed tag, or `None`
/// (silent by rule). `fetch` produces the feed body — in production
/// [`fetch_latest_release`] (see [`look_at_sign_in_over_http`]).
pub async fn look_at_feed_at_sign_in<F, Fut, E>(current: &str, fetch: F) -> Option<String>
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

/// [`look_at_feed_at_sign_in`] over the real feed — **interim**, for
/// `fauna-ffi`'s release-host face only (macOS and windows, until each moves
/// to [`look_at_sign_in`]).
pub async fn look_at_sign_in_over_http(
    http: &reqwest::Client,
    origin: &str,
    user_agent: &str,
    current: &str,
) -> Option<String> {
    look_at_feed_at_sign_in(current, || fetch_latest_release(http, origin, user_agent)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn look(current: &str, answer: Result<&str, &str>) -> Option<String> {
        let answer = answer.map(str::to_owned).map_err(str::to_owned);
        look_at_feed_at_sign_in(current, || async move { answer }).await
    }

    /// A nest that answers `fauna.nest.info` with `reply` (or fails), and
    /// counts the kinds it was asked — the look must ask exactly that one.
    struct Nest {
        reply: Result<NestInfoReply, &'static str>,
        asked: std::sync::Mutex<Vec<&'static str>>,
    }

    impl Nest {
        fn reporting(newest_release: Option<&str>) -> Self {
            Self {
                reply: Ok(NestInfoReply {
                    newest_release: newest_release.map(str::to_owned),
                    ..Default::default()
                }),
                asked: Default::default(),
            }
        }
    }

    impl RpcRequester for Nest {
        type Error = &'static str;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            self.asked.lock().unwrap().push(kind);
            let reply = self.reply.clone()?;
            let bytes = fauna_protocol::encode_canonical(&reply).unwrap();
            Ok(fauna_protocol::decode_strict(&bytes).unwrap())
        }
    }

    #[tokio::test]
    async fn the_sign_in_look_reads_the_nests_newest_release_and_nothing_else() {
        let nest = Nest::reporting(Some("0.2.0"));
        assert_eq!(
            look_at_sign_in(&nest, "0.1.2").await,
            Some("v0.2.0".to_owned())
        );
        assert_eq!(*nest.asked.lock().unwrap(), vec!["fauna.nest.info"]);
    }

    #[tokio::test]
    async fn a_nest_that_says_nothing_or_nothing_newer_means_no_notice() {
        // An older nest, or one whose update check is off: no field.
        assert_eq!(look_at_sign_in(&Nest::reporting(None), "0.1.2").await, None);
        // At or behind the running version.
        assert_eq!(
            look_at_sign_in(&Nest::reporting(Some("0.1.2")), "0.1.2").await,
            None
        );
        assert_eq!(
            look_at_sign_in(&Nest::reporting(Some("0.1.0")), "0.1.2").await,
            None
        );
        // Not a version at all: no spurious notice.
        assert_eq!(
            look_at_sign_in(&Nest::reporting(Some("soon")), "0.1.2").await,
            None
        );
        // The read failed: silent.
        let unreachable = Nest {
            reply: Err("disconnected"),
            asked: Default::default(),
        };
        assert_eq!(look_at_sign_in(&unreachable, "0.1.2").await, None);
    }

    #[test]
    fn several_nests_keep_the_newest_notice() {
        let v = |s: &str| Some(s.to_owned());
        assert_eq!(keep_the_newer(None, v("v0.2.0")), v("v0.2.0"));
        assert_eq!(keep_the_newer(v("v0.2.0"), v("v0.3.0")), v("v0.3.0"));
        // A later nest that knows less never moves the notice down or away.
        assert_eq!(keep_the_newer(v("v0.3.0"), v("v0.2.0")), v("v0.3.0"));
        assert_eq!(keep_the_newer(v("v0.3.0"), None), v("v0.3.0"));
        assert_eq!(keep_the_newer(None, None), None);
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
