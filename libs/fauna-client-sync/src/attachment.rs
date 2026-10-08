//! The app's half of the agent's attachment lease — "this app is open on this
//! machine" (`sync-agent.md` § Scope per platform, the push wake stand-in;
//! `fauna_ipc::sync::RequestMethod::AttachApp`).
//!
//! An open desktop app owns the machine's banners under its own focus rule, so
//! the agent's `ws-device` notification arm stays silent while any app is
//! attached (`common.md` § Push Notifications → *Transports*). Every desktop
//! app — tui, linux, windows — holds one attachment for its whole process
//! life: [`attach_app`] starts a thread that opens a connection to the agent,
//! sends `AttachApp`, and keeps it open. If the agent restarts, the connection
//! breaks and the thread re-attaches to the new one.
//!
//! The lease ends when the app's process does: the agent sees its socket close
//! on a clean exit and a crash alike. Dropping the [`AppAttachment`] handle
//! stops re-attaching; the connection it already holds stays open until the
//! process exits (the shared blocking client's reader thread holds the socket),
//! which is the lease's intended meaning.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use fauna_ipc::endpoint::AgentEndpoint;
use fauna_ipc::sync::{RequestMethod, ResponseResult};

/// How often a held attachment checks that its connection is still alive.
const ALIVE_POLL: Duration = Duration::from_secs(1);
/// The re-attach backoff after a failed attempt (agent not up yet, or a
/// dropped connection).
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);

/// A running attachment. Dropping it stops re-attaching.
pub struct AppAttachment {
    stop: Arc<AtomicBool>,
}

impl Drop for AppAttachment {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Who is attaching: the app's name (`"tui"`, `"linux"`, `"windows"`) and, on
/// a platform whose banners are posted under an app identity, that identity
/// (windows: the AUMID of the app's own toast registration — the agent posts
/// its toasts under it while the app is closed; `None` elsewhere).
#[derive(Debug, Clone)]
pub struct AttachingApp {
    pub app: String,
    pub notification_identity: Option<String>,
}

/// Attach `app` to the agent at `endpoint` for as long as this process lives.
/// Never blocks the caller.
pub fn attach_app(endpoint: AgentEndpoint, app: AttachingApp) -> AppAttachment {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let spawned = std::thread::Builder::new()
        .name("fauna-agent-attach".into())
        .spawn(move || run(&endpoint, &app, &thread_stop));
    if let Err(e) = spawned {
        tracing::warn!("agent attachment: could not start: {e}");
    }
    AppAttachment { stop }
}

fn run(endpoint: &AgentEndpoint, app: &AttachingApp, stop: &AtomicBool) {
    let mut retry = RETRY_MIN;
    while !stop.load(Ordering::SeqCst) {
        match attach_once(endpoint, app) {
            Ok(client) => {
                retry = RETRY_MIN;
                while client.is_alive() && !stop.load(Ordering::SeqCst) {
                    std::thread::sleep(ALIVE_POLL);
                }
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                tracing::debug!("agent attachment: connection lost; re-attaching");
            }
            Err(e) => {
                tracing::debug!("agent attachment: not attached ({e}); retrying");
                std::thread::sleep(retry);
                retry = (retry * 2).min(RETRY_MAX);
            }
        }
    }
}

fn attach_once(
    endpoint: &AgentEndpoint,
    app: &AttachingApp,
) -> Result<fauna_ipc::sync_pipe_client::SyncPipeClient, String> {
    let client = endpoint.connect().map_err(|e| e.to_string())?;
    let reply = client
        .request(RequestMethod::AttachApp {
            app: Some(app.app.clone()),
            notification_identity: app.notification_identity.clone(),
        })
        .map_err(|e| e.to_string())?;
    match reply.result {
        ResponseResult::Ok(_) => Ok(client),
        ResponseResult::Err(e) => Err(e),
    }
}
