//! Push notifications on linux — the `ws-device` transport (`common.md` § Push
//! Notifications → *Transports*): linux subscribes this install's row, the
//! per-user sync agent posts the desktop notification while the app is closed,
//! and an open app owns its own banners and ignores the frame (the agent's arm
//! stands down while the app holds its attachment lease,
//! `sync_agent::install`).
//!
//! Everything stateful is the shared `fauna_client_push::registration` machine
//! (the install intent bit, the which-actor record, the leave-shape drops); this
//! module only names linux's inputs to it, as tui's `push.rs` does:
//!
//! * **the device id** — [`crate::sync::device_id`], the same value the sync
//!   agent's capability is minted from (`sync_agent::install`), so the id this
//!   row is keyed under, the id this app's connection announces and the id the
//!   agent announces are one value by construction;
//! * **the intent store** — one file beside `app-settings.json` in the
//!   install-scoped config base, never an account scope (`account-scoping.md`
//!   class 2), so no sign-out erase touches it.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use fauna_client::NestClient;
use fauna_client_push::registration::{
    FileIntentStore, IntentStore, PushIntent, PushRegistration, StandingFailure,
    ws_device_subscription,
};

/// The intent file's name under the install-scoped config base.
const INTENT_FILE: &str = "push-intent.cbor";

/// How long a leave gesture waits for its row drop before going on — the
/// gesture completes offline (tui's `PUSH_DROP_BUDGET`).
const PUSH_DROP_BUDGET: Duration = Duration::from_secs(3);

/// linux's registration: the shared machine over the live connection.
type LinuxPushRegistration = PushRegistration<Arc<NestClient>, FileIntentStore>;

/// The signed-in session the Settings control and the leave gesture act for.
#[derive(Clone)]
struct PushSession {
    nest: Arc<NestClient>,
    actor_hex: String,
    /// This install's device id for the actor, read at session start.
    device_id: String,
    rt: tokio::runtime::Handle,
}

thread_local! {
    /// The live session (GTK main thread only), set at the post-auth hook and
    /// taken at teardown.
    static SESSION: RefCell<Option<PushSession>> = const { RefCell::new(None) };
    /// The last `GetServiceStatus` reading: `(agent_running, notification_sink)`.
    static AGENT: RefCell<(bool, Option<bool>)> = const { RefCell::new((false, None)) };
    /// Repaints the Settings control's standing line after a new reading.
    static REPAINT: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

/// The install-scoped intent store, or `None` with no config base.
fn intent_store() -> Option<FileIntentStore> {
    let mut path = crate::window_state::dirs_config()?;
    path.push("fauna");
    path.push(INTENT_FILE);
    Some(FileIntentStore::new(path))
}

/// The stored record (what the toggle renders) — not opted in when unreadable.
pub(crate) fn stored_intent() -> PushIntent {
    intent_store().map(|s| s.load()).unwrap_or_default()
}

/// This install's registration for `actor_hex` over `nest`, or `None` when the
/// device id cannot be read (a failure the caller surfaces, never a guessed id:
/// an id that matches no row would make the nest dial elsewhere).
///
/// The device id is the session's, read once at its start: `sync::device_id`
/// follows the ACTIVE account, and a switch writes the registry before the
/// teardown, so a leave gesture re-reading it would key the drop under the
/// incoming account's id and miss the leaving row.
fn registration(
    nest: Arc<NestClient>,
    actor_hex: &str,
    device_id: &str,
) -> Option<LinuxPushRegistration> {
    Some(PushRegistration::new(
        nest,
        intent_store()?,
        actor_hex,
        device_id.to_string(),
    ))
}

/// At sign-in (launch, switch-in): remember the session, announce this device
/// on every connection, and re-arm the row when this install opted in — never
/// opting it in. Best-effort: a failure is logged, the session proceeds.
pub(crate) fn on_session_start(
    nest: Arc<NestClient>,
    actor_hex: String,
    rt: tokio::runtime::Handle,
) {
    let Ok(device_id) = crate::sync::device_id().map(|id| fauna_core::hex32::encode(&id)) else {
        tracing::warn!("push: no device id for this account; not announcing");
        SESSION.with(|s| *s.borrow_mut() = None);
        return;
    };
    SESSION.with(|s| {
        *s.borrow_mut() = Some(PushSession {
            nest: Arc::clone(&nest),
            actor_hex: actor_hex.clone(),
            device_id: device_id.clone(),
            rt: rt.clone(),
        })
    });
    rt.spawn(async move {
        nest.set_push_presence(device_id.clone()).await;
        if let Some(reg) = registration(nest, &actor_hex, &device_id)
            && let Err(e) = reg.rearm(ws_device_subscription(reg.device_id())).await
        {
            tracing::warn!("push: re-arm failed: {e}");
        }
    });
}

/// A leave gesture (switch-out, sign-out, factory reset): the leaving actor's
/// row drop, bounded and best-effort, plus the runtime to drive it on —
/// `None` with no session. Takes the session, so a second leave finds nothing.
/// Issued over the leaving session's own client, which is why the caller
/// sequences it ahead of anything that disconnects or erases.
pub(crate) fn take_leave() -> Option<(
    tokio::runtime::Handle,
    std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
)> {
    let session = SESSION.with(|s| s.borrow_mut().take())?;
    let rt = session.rt.clone();
    Some((
        rt,
        Box::pin(async move {
            let drop = async {
                if let Some(reg) =
                    registration(session.nest, &session.actor_hex, &session.device_id)
                    && let Err(e) = reg.drop_actor_row().await
                {
                    tracing::warn!("push: dropping the leaving account's row failed: {e}");
                }
            };
            if tokio::time::timeout(PUSH_DROP_BUDGET, drop).await.is_err() {
                tracing::warn!("push: dropping the leaving account's row timed out");
            }
        }),
    ))
}

/// The Settings toggle: on → enable, off → disable, on the session's runtime;
/// `done` runs on the GTK thread with the stored record after the attempt (a
/// failed enable leaves it off, a disable clears it even when the nest is
/// unreachable) and the error, if any, for the inline line.
pub(crate) fn set_opt_in(on: bool, done: impl FnOnce(PushIntent, Option<String>) + 'static) {
    let Some(session) = SESSION.with(|s| s.borrow().clone()) else {
        done(stored_intent(), Some("not signed in".into()));
        return;
    };
    let (tx, rx) = async_channel::bounded(1);
    session.rt.spawn(async move {
        let result = match registration(session.nest, &session.actor_hex, &session.device_id) {
            Some(reg) => {
                let r = if on {
                    reg.enable(ws_device_subscription(reg.device_id())).await
                } else {
                    reg.disable().await
                };
                (reg.intent(), r.err().map(|e| e.to_string()))
            }
            None => (
                stored_intent(),
                Some("this device's id could not be read".into()),
            ),
        };
        let _ = tx.send(result).await;
    });
    gtk::glib::spawn_future_local(async move {
        if let Ok((intent, error)) = rx.recv().await {
            done(intent, error);
        }
    });
}

/// Record the agent's latest health reading (the 10 s `sync-agent-status` poll)
/// and repaint the control's standing line.
pub(crate) fn note_agent_status(
    result: &Result<
        fauna_ipc::sync::ServiceStatusInfo,
        fauna_client_sync::agent::AgentControlError,
    >,
) {
    let reading = match result {
        Ok(status) => (true, status.notification_sink),
        Err(_) => (false, None),
    };
    AGENT.with(|a| *a.borrow_mut() = reading);
    if let Some(repaint) = REPAINT.with(|r| r.borrow().clone()) {
        repaint();
    }
}

/// Register the control's repaint hook (the Account page owns the widgets).
pub(crate) fn set_repaint(repaint: Rc<dyn Fn()>) {
    REPAINT.with(|r| *r.borrow_mut() = Some(repaint));
}

/// The inline line the control paints with nothing failing in-flight — the
/// shared rule (`fauna_client_push::registration::standing_failure`: shown only
/// while this install is opted in, the unreachable agent first) mapped to the
/// shared strings.
pub(crate) fn standing_failure(opted_in: bool) -> Option<&'static str> {
    use fauna_i18n::strings::settings::push_notifications as t;
    let (running, sink) = AGENT.with(|a| *a.borrow());
    fauna_client_push::registration::standing_failure(opted_in, running, sink).map(|f| match f {
        StandingFailure::AgentUnreachable => t::AGENT_UNREACHABLE,
        StandingFailure::NoSink => t::NO_SINK,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_i18n::strings::settings::push_notifications as t;

    #[test]
    fn standing_line_follows_the_shared_rule() {
        AGENT.with(|a| *a.borrow_mut() = (false, None));
        assert_eq!(standing_failure(false), None, "never while opted out");
        assert_eq!(standing_failure(true), Some(t::AGENT_UNREACHABLE));
        AGENT.with(|a| *a.borrow_mut() = (true, Some(false)));
        assert_eq!(standing_failure(true), Some(t::NO_SINK));
        AGENT.with(|a| *a.borrow_mut() = (true, Some(true)));
        assert_eq!(standing_failure(true), None);
    }

    #[test]
    fn leave_without_a_session_is_nothing() {
        SESSION.with(|s| *s.borrow_mut() = None);
        assert!(take_leave().is_none());
    }
}
