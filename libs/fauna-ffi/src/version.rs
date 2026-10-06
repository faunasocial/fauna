//! UniFFI façade for the shared release-version comparison
//! ([`fauna_core::version::is_newer`]).
//!
//! The canonical "is a newer release available?" semver check. The Rust-native
//! Linux app calls `fauna_core::version::is_newer` directly; this export gives
//! Apple / Windows / Android the identical, spec-correct compare so no client
//! re-derives semver per platform (priority #2/#1 — the update-checker analog of
//! `handle::validate_handle`). Replaces the hand-rolled tuple parsers that
//! mishandled pre-release tags and build metadata (e.g. the windows
//! `UpdateService.IsNewer`, whose lenient int-tuple treated `1.0.0-beta` as equal
//! to `1.0.0`). See `libs/fauna-core/src/version.rs`.
//!
//! Also the FFI face of the update notice's two shared calls
//! (`installers/README.md` § Knowing a newer version is out):
//! [`check_for_newer_release`] (the asked check) and [`look_at_sign_in`] (the
//! once-per-sign-in look), both over `fauna_client::update_look`, plus
//! [`release_feed_origin`] — so windows and macOS keep neither a feed URL nor a
//! round trip of their own. The records crossing the boundary are this
//! module's own ([`NewerRelease`], [`NewerReleaseCheck`]).

/// UniFFI face of [`fauna_core::version::is_newer`]. Returns `true` when
/// `candidate` is a strictly newer release than `current`. Both are semver
/// strings without a leading `v` (callers strip it); a parse failure on either
/// side returns `false` — never prompt an update on an unparseable version.
#[uniffi::export]
pub fn is_newer(current: String, candidate: String) -> bool {
    fauna_core::version::is_newer(&current, &candidate)
}

/// This crate's own build version — the workspace version every workspace
/// member shares via `version.workspace = true`, `fauna-sync-agent` (the
/// local sync-agent binary) included. A Rust-native app compares its own
/// `env!("CARGO_PKG_VERSION")` against `GetServiceStatus.version` to derive
/// [`AgentHealthState`](https://docs.rs/fauna-client-sync) (`sync-agent.md`
/// § Local agent health); a non-Rust client (Windows) has no such constant of
/// its own — its C# assembly version is MSBuild-derived and does NOT track the
/// Cargo workspace version — so it needs this export to make the identical
/// comparison instead of drifting onto its own (unrelated) version string.
#[uniffi::export]
pub fn fauna_ffi_build_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// The release feed's production origin
/// ([`fauna_core::version::GITHUB_API_ORIGIN`]) — what [`check_for_newer_release`]
/// and [`look_at_sign_in`] are called with outside e2e automation, so no app
/// spells a feed URL of its own. Under e2e, each app's compile-gated
/// `FAUNA_E2E_RELEASE_FEED_URL` seam passes the harness's stub feed instead.
#[uniffi::export]
pub fn release_feed_origin() -> String {
    fauna_core::version::GITHUB_API_ORIGIN.to_owned()
}

/// A newer release, as the update notice paints it: "version `version` is out,
/// get it at `release_page_url`".
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NewerRelease {
    /// The tag as the feed spells it (`v0.2.0`).
    pub tag: String,
    /// The bare version (`0.2.0`) — the tag without its leading `v`.
    pub version: String,
    /// The release page every channel's artifacts hang off
    /// ([`fauna_core::version::release_page_url`]).
    pub release_page_url: String,
}

impl NewerRelease {
    fn from_tag(tag: String) -> Self {
        Self {
            version: tag.trim_start_matches('v').to_owned(),
            release_page_url: fauna_core::version::release_page_url(&tag),
            tag,
        }
    }
}

/// UniFFI face of [`fauna_client::update_look::NewerReleaseCheck`]: what the
/// asked check found.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum NewerReleaseCheck {
    /// A newer release is out.
    Newer { release: NewerRelease },
    /// The feed answered and nothing newer is out.
    UpToDate,
    /// No answer, or one that is not even JSON: tell the user the check failed.
    Failed,
}

impl From<fauna_client::update_look::NewerReleaseCheck> for NewerReleaseCheck {
    fn from(check: fauna_client::update_look::NewerReleaseCheck) -> Self {
        use fauna_client::update_look::NewerReleaseCheck as Shared;
        match check {
            Shared::Newer { tag } => Self::Newer {
                release: NewerRelease::from_tag(tag),
            },
            Shared::UpToDate => Self::UpToDate,
            Shared::Failed => Self::Failed,
        }
    }
}

/// The asked check — the "Check for Updates" door's one round trip — over
/// [`fauna_client::update_look::check_for_newer_release_over_http`], against
/// this build's own version. `feed_origin` is [`release_feed_origin`] in
/// production; `user_agent` names the calling app (`fauna-windows/0.1.2`).
///
/// Runs on the crate's tokio runtime (`fauna_uniffi_async::export`): the
/// reqwest round trip panics without a reactor.
#[fauna_uniffi_async::export]
pub async fn check_for_newer_release(feed_origin: String, user_agent: String) -> NewerReleaseCheck {
    fauna_client::update_look::check_for_newer_release_over_http(
        &reqwest::Client::new(),
        &feed_origin,
        &user_agent,
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .into()
}

/// The once-per-sign-in look, called from each app's post-sign-in hook, over
/// [`fauna_client::update_look::look_at_sign_in_over_http`]: the newer release
/// to paint the same notice for, or `None` — nothing newer, or a failed look
/// (silent by rule). Same arguments as [`check_for_newer_release`].
#[fauna_uniffi_async::export]
pub async fn look_at_sign_in(feed_origin: String, user_agent: String) -> Option<NewerRelease> {
    fauna_client::update_look::look_at_sign_in_over_http(
        &reqwest::Client::new(),
        &feed_origin,
        &user_agent,
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .map(NewerRelease::from_tag)
}

#[cfg(test)]
mod tests {
    use super::{
        NewerRelease, NewerReleaseCheck, check_for_newer_release, fauna_ffi_build_version,
        is_newer, look_at_sign_in, release_feed_origin,
    };

    #[test]
    fn the_feed_origin_is_the_shared_production_one() {
        assert_eq!(
            release_feed_origin(),
            fauna_core::version::GITHUB_API_ORIGIN
        );
    }

    #[test]
    fn a_newer_release_carries_its_bare_version_and_release_page() {
        assert_eq!(
            NewerRelease::from_tag("v9.9.9".into()),
            NewerRelease {
                tag: "v9.9.9".into(),
                version: "9.9.9".into(),
                release_page_url: fauna_core::version::release_page_url("v9.9.9"),
            }
        );
    }

    /// Serve one HTTP answer from a loopback listener; return its origin.
    async fn serve_once(body: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf).await.unwrap();
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(reply.as_bytes()).await.unwrap();
        });
        origin
    }

    /// An origin nothing listens on: bound, then dropped.
    async fn closed_origin() -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", l.local_addr().unwrap())
    }

    #[tokio::test]
    async fn the_asked_check_tells_newer_up_to_date_and_failed_apart() {
        let newer = serve_once(r#"{"tag_name":"v9.9.9"}"#).await;
        assert_eq!(
            check_for_newer_release(newer, "fauna-test/0".into()).await,
            NewerReleaseCheck::Newer {
                release: NewerRelease::from_tag("v9.9.9".into())
            }
        );
        let same = serve_once(concat!(
            r#"{"tag_name":"v"#,
            env!("CARGO_PKG_VERSION"),
            r#""}"#
        ))
        .await;
        assert_eq!(
            check_for_newer_release(same, "fauna-test/0".into()).await,
            NewerReleaseCheck::UpToDate
        );
        assert_eq!(
            check_for_newer_release(closed_origin().await, "fauna-test/0".into()).await,
            NewerReleaseCheck::Failed
        );
    }

    #[tokio::test]
    async fn the_sign_in_look_names_a_newer_release_and_is_silent_on_failure() {
        let newer = serve_once(r#"{"tag_name":"v9.9.9"}"#).await;
        assert_eq!(
            look_at_sign_in(newer, "fauna-test/0".into()).await,
            Some(NewerRelease::from_tag("v9.9.9".into()))
        );
        assert_eq!(
            look_at_sign_in(closed_origin().await, "fauna-test/0".into()).await,
            None
        );
    }

    /// This crate's build version must match the workspace version verbatim
    /// (`Cargo.toml`'s `[workspace.package].version`) — it's what makes the
    /// windows Running/RestartPending comparison against `fauna-sync-agent`'s
    /// own `env!("CARGO_PKG_VERSION")` correct, since both crates inherit it
    /// via `version.workspace = true`.
    #[test]
    fn build_version_matches_the_workspace_version() {
        assert_eq!(fauna_ffi_build_version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn forwards_to_shared_spec_correct_compare() {
        // Ordinary newer cases (a lenient tuple parser gets these right too).
        assert!(is_newer("1.2.3".into(), "1.2.4".into()));
        // Numeric (not lexical) component compare.
        assert!(is_newer("1.9.0".into(), "1.10.0".into()));
        // Pre-release ordering — the gap in the hand-rolled per-app parsers:
        // a pre-release is OLDER than its release.
        assert!(is_newer("1.0.0-beta".into(), "1.0.0".into()));
        assert!(!is_newer("1.0.0".into(), "1.0.0-beta".into()));
        // Build metadata is ignored by precedence.
        assert!(!is_newer("1.2.3".into(), "1.2.3+build.9".into()));
        // Unparseable (forgotten `v` strip / garbage) -> not newer.
        assert!(!is_newer("1.2.3".into(), "v1.2.4".into()));
        assert!(!is_newer("1.2.3".into(), "not-a-version".into()));
    }
}
