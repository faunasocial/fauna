//! The **one platform-transport seam** for the sync agent's local control
//! plane — `sync-agent.md` § Consumers: *"the client resolves the platform
//! transport through one `fauna_ipc` endpoint seam: the per-user unix socket on
//! linux/macOS, the per-SID named pipe on windows"*.
//!
//! [`SyncPipeClient`] has carried both transports for a long time
//! ([`connect_socket`](SyncPipeClient::connect_socket) /
//! [`connect_pipe_to`](SyncPipeClient::connect_pipe_to)) — what was missing was
//! a single value a *client* can resolve once at construction and connect from
//! on every platform, so control-plane code stops being `#[cfg(unix)]` merely
//! because it holds a `PathBuf`. [`AgentEndpoint`] is that value.
//!
//! Consumers hold the endpoint, never the path/name: `fauna_client_sync::agent`
//! (linux GTK + fauna-tui directly, FaunaKit/macOS and the windows app through
//! the `fauna-ffi` adapter) resolves [`AgentEndpoint::default_for_user`] once
//! and calls [`AgentEndpoint::connect`] per exchange.
//!
//! Gated `#[cfg(any(unix, windows))]` like [`crate::convergence`]: wasm has no
//! local agent at all.

use std::ffi::OsStr;
use std::io;

use crate::sync_pipe_client::SyncPipeClient;

/// The harness pipe-name override (convention 15's `FAUNA_E2E_*` runtime gate
/// family) — a per-launch pipe **leaf**, e.g. `fauna-sync-e2e-tui-4711`.
///
/// Windows rendezvouses on `\\.\pipe\fauna-sync.<SID>`, a machine-global kernel
/// name keyed by the OS user, so — unlike the unix clients, whose socket is
/// path-derived and therefore isolated for free by a private
/// `HOME`/`XDG_RUNTIME_DIR` — an isolated e2e launch has no way to shift it
/// except by naming it. Without this a test drives whatever holds the real
/// per-user pipe: the box's installed agent, or a sibling dev checkout's
/// running as the same user out of its own build tree.
///
/// **Only [`e2e_pipe_override`] may read it**, and that read is compiled out of
/// production builds — the name is public because the spawn side composes the
/// matching `--pipe-name` from the same value.
pub const E2E_PIPE_ENV: &str = "FAUNA_E2E_SYNC_PIPE";

/// The harness pipe override this build honours: the [`E2E_PIPE_ENV`] leaf in a
/// test-capable build.
///
/// Gated as a pair with the production twin below, the shape the spawn side
/// already uses for its own half of this override
/// (`fauna_client_sync::agent_spawner::pinned_from_env`). **The gate is the
/// security boundary, not the env read** (e2e-conventions.md convention 15): an
/// ungated read makes the pipe an *endpoint redirect* on every released windows
/// app — whoever controls the launch environment names the pipe the client
/// treats as its agent, and what the client then pushes down it is
/// `RequestMethod::ProvisionCapability`, i.e. the owner's `BackupKey`, the
/// per-folder content keys and a renewable nest bearer (`crate::sync`). That
/// is key-material disclosure, not merely a falsifiable status read.
#[cfg(all(windows, any(test, debug_assertions, feature = "test-helpers")))]
fn e2e_pipe_override() -> Option<String> {
    pipe_name_from_env(std::env::var_os(E2E_PIPE_ENV).as_deref())
}

/// Production twin: a plain `--release` build consults no harness environment
/// and resolves only the per-SID pipe.
///
/// A release-profile e2e build keeps the override through the crate's
/// `test-helpers` feature, which `fauna-client-sync/test-helpers` forwards (and
/// linux/tui `e2e-agent` + fauna-ffi `test-helpers` forward in turn) — so this
/// twin is reached only by a build that ships.
#[cfg(all(windows, not(any(test, debug_assertions, feature = "test-helpers"))))]
fn e2e_pipe_override() -> Option<String> {
    None
}

/// Turn a raw [`E2E_PIPE_ENV`] value into the full `\\.\pipe\<leaf>` name.
///
/// **Blank is NOT set.** An empty env var is how a harness "unsets" one, and
/// falling through to a pipe literally named `""` would make every connect fail
/// in a way that reads as *"no agent is running"* — the misdiagnosis this
/// convention exists to prevent. Mirrors the C# `SyncServicePipeClient.PipeName`
/// reading exactly, which is what keeps a windows tui and the windows app
/// agreeing on the pipe a single harness launch pinned.
///
/// Pure — testable on any platform, like [`crate::sync::pipe_name_for_sid`].
pub fn pipe_name_from_env(raw: Option<&OsStr>) -> Option<String> {
    let raw = raw?;
    if raw.is_empty() {
        return None;
    }
    Some(format!(r"\\.\pipe\{}", raw.to_string_lossy()))
}

/// Where this user's `fauna-sync-agent` listens, in the shape its platform
/// transport takes: a filesystem path on unix, a named-pipe name on windows.
///
/// Resolve once (`default_for_user`), connect many times (`connect`) — the
/// resolution reads the environment, and a client that re-read it per exchange
/// could split a single session across two agents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEndpoint {
    /// The per-user `0600` unix socket in its `0700` dir
    /// (`crate::unix_transport::default_socket_path`).
    #[cfg(unix)]
    Unix(std::path::PathBuf),
    /// The full `\\.\pipe\…` name — per-SID in production, a per-launch leaf
    /// under [`E2E_PIPE_ENV`].
    #[cfg(windows)]
    Pipe(String),
}

impl AgentEndpoint {
    /// This user's agent endpoint for this platform.
    ///
    /// - **unix** — [`crate::unix_transport::default_socket_path`] (macOS
    ///   Application Support / linux `$XDG_RUNTIME_DIR`, Flatpak-aware).
    /// - **windows** — [`crate::sync::current_user_pipe_name`]
    ///   (`\\.\pipe\fauna-sync.<SID>`), or the [`E2E_PIPE_ENV`] leaf in a
    ///   test-capable build ([`e2e_pipe_override`] — compiled out of a release
    ///   artifact, so a shipped client resolves the per-SID pipe and nothing
    ///   else).
    pub fn default_for_user() -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self::Unix(crate::unix_transport::default_socket_path()?))
        }
        #[cfg(windows)]
        {
            match e2e_pipe_override() {
                Some(name) => Ok(Self::Pipe(name)),
                None => Ok(Self::Pipe(crate::sync::current_user_pipe_name()?)),
            }
        }
    }

    /// Open one connection to the agent.
    ///
    /// Blocking on both platforms and requiring **no tokio runtime** (the
    /// windows arm drives the pipe's `OVERLAPPED` halves internally), so a GTK
    /// main loop, a fauna-tui render thread, or a `spawn_blocking` closure can
    /// all call it directly.
    ///
    /// A missing agent surfaces as an ordinary connect error — on windows the
    /// open of a nonexistent pipe name fails fast, so there is deliberately no
    /// exists-probe here (.NET's `ConnectAsync` blocks instead, which is why the
    /// C# client needed a separate `PipeIsServed`).
    pub fn connect(&self) -> io::Result<SyncPipeClient> {
        match self {
            #[cfg(unix)]
            Self::Unix(path) => SyncPipeClient::connect_socket(path),
            #[cfg(windows)]
            Self::Pipe(name) => SyncPipeClient::connect_pipe_to(name),
        }
    }
}

impl std::fmt::Display for AgentEndpoint {
    /// The endpoint as a log line names it — a client reporting *"agent
    /// unreachable"* should say **which** endpoint it could not reach.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(unix)]
            Self::Unix(path) => write!(f, "{}", path.display()),
            #[cfg(windows)]
            Self::Pipe(name) => write!(f, "{name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The harness leaf becomes the full Win32 pipe path — the client's half of
    /// the same translation the spawner does when it passes `--pipe-name` to the
    /// agent. Both sides must agree or an isolated launch talks to nothing.
    #[test]
    fn a_leaf_env_value_resolves_to_the_full_pipe_name() {
        assert_eq!(
            pipe_name_from_env(Some(OsStr::new("fauna-sync-e2e-tui-4711"))),
            Some(r"\\.\pipe\fauna-sync-e2e-tui-4711".to_string())
        );
    }

    /// Blank is not set: an empty env var is how a harness clears one, and a
    /// pipe named `""` would fail every connect as an indistinguishable
    /// "no agent running".
    #[test]
    fn an_unset_or_blank_env_value_is_not_an_override() {
        assert_eq!(pipe_name_from_env(None), None);
        assert_eq!(pipe_name_from_env(Some(OsStr::new(""))), None);
    }

    /// Windows: no override → the production per-SID name. Pinned as a *shape*
    /// (`\\.\pipe\fauna-sync.S-…`) rather than a literal, since the SID is the
    /// running user's — the same assertion the C# `SyncPipeNameTests` makes.
    #[cfg(windows)]
    #[test]
    fn the_windows_default_is_the_per_sid_pipe() {
        // Read the ambient environment rather than mutating it: env writes race
        // every other test in the process (and are `unsafe` in edition 2024).
        // Under a harness-pinned launch the override is the correct answer, so
        // assert against whichever arm the environment selects.
        let endpoint = AgentEndpoint::default_for_user().expect("resolve endpoint");
        let AgentEndpoint::Pipe(name) = endpoint;
        // Asked through the same door production uses, so a build whose gate
        // compiled out the override is asserted against the per-SID arm.
        match e2e_pipe_override() {
            Some(expected) => assert_eq!(name, expected),
            None => assert!(
                name.starts_with(r"\\.\pipe\fauna-sync.S-"),
                "expected the per-SID production pipe, got: {name}"
            ),
        }
    }

    /// Unix: the endpoint is exactly what the socket-path resolver says — the
    /// substitution must be behavior-preserving for linux/macOS, which this
    /// crate's own transport tests then cover end to end.
    ///
    /// Compared as `Result`s so the assertion holds on a box with no
    /// `XDG_RUNTIME_DIR` (where both sides must refuse identically) as well as
    /// on a real logon session.
    #[cfg(unix)]
    #[test]
    fn the_unix_endpoint_is_the_default_socket_path() {
        let resolved = AgentEndpoint::default_for_user();
        let expected = crate::unix_transport::default_socket_path();
        match (resolved, expected) {
            (Ok(AgentEndpoint::Unix(got)), Ok(want)) => assert_eq!(got, want),
            (Err(_), Err(_)) => {}
            (got, want) => panic!("endpoint {got:?} disagrees with socket path {want:?}"),
        }
    }

    /// The log-line accessor renders the endpoint a human can act on (the path
    /// to stat, or the pipe name to look for).
    #[test]
    fn display_renders_the_endpoint_for_a_log_line() {
        #[cfg(unix)]
        {
            let e = AgentEndpoint::Unix(std::path::PathBuf::from("/run/user/1000/fauna/a.sock"));
            assert_eq!(e.to_string(), "/run/user/1000/fauna/a.sock");
        }
        #[cfg(windows)]
        {
            let e = AgentEndpoint::Pipe(r"\\.\pipe\fauna-sync.S-1-5-21-1-2-3-1001".to_string());
            assert_eq!(e.to_string(), r"\\.\pipe\fauna-sync.S-1-5-21-1-2-3-1001");
        }
    }
}
