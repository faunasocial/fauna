//! The agent's `ws-device` notification arm — the desktop's push wake stand-in
//! (`sync-agent.md` § Scope per platform; owner `common.md` § Push
//! Notifications → *Transports*, the `ws-device` ruling).
//!
//! The nest delivers a present `ws-device` row as a `fauna.push.notification`
//! frame to every connection that announced the row's device id — the agent's
//! own account connection, and the app's if it is open. Exactly one process
//! posts the banner: **the agent posts only while no app is attached** over the
//! local IPC seam; an attached app owns the machine's banners under its own
//! focus rule and ignores the frame. "Attached" is a connection lease
//! ([`AttachedApps`]): an app sends `RequestMethod::AttachApp` on a connection it
//! holds for its whole life, and the lease ends when that connection closes,
//! a crash included.
//!
//! The subscription is the APP's, never the agent's: the agent only announces
//! presence (`NestClient::set_push_presence`, in the account host) and posts.
//! A machine with no desktop session has no sink; the agent says so on
//! `ServiceStatusInfo::notification_sink` and the app's control renders its
//! failure line.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fauna_protocol::PushEvent;
use fauna_protocol::push_events::PushNotificationPayload;
use tokio::sync::broadcast;

/// How many apps hold an attachment right now.
#[derive(Default)]
pub struct AttachedApps(AtomicUsize);

impl AttachedApps {
    /// Start one app's lease; it ends when the returned guard drops.
    pub fn attach(self: &Arc<Self>) -> AppAttachment {
        self.0.fetch_add(1, Ordering::SeqCst);
        AppAttachment(Arc::clone(self))
    }

    /// Is any app open on this machine?
    pub fn any(&self) -> bool {
        self.0.load(Ordering::SeqCst) > 0
    }
}

/// One app's attachment lease — held for its IPC connection's lifetime
/// (`fauna_ipc::conn_scope::hold_for_connection`).
pub struct AppAttachment(Arc<AttachedApps>);

impl Drop for AppAttachment {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Where the agent posts a banner.
pub trait NotificationSink: Send + Sync {
    /// Whether a banner can be posted here, without blocking: `None` until
    /// [`Self::probe`] has answered.
    fn availability(&self) -> Option<bool>;
    /// Answer [`Self::availability`] — may block (a session-bus round trip);
    /// called once, off the async runtime.
    fn probe(&self);
    /// Post one banner. Blocking.
    fn post(&self, notification: &PushNotificationPayload) -> Result<(), String>;
}

/// The sink this platform's agent posts through. linux: the freedesktop
/// notification daemon through `notify-rust`, present when a desktop session
/// is (the `keyring_probe` selector tui already trusts — never an env knob).
/// Elsewhere the arm is unbuilt (windows: the WinRT toast, the windows row's;
/// macOS: the mac row's), so the agent honestly reports no sink.
pub fn platform_sink() -> Arc<dyn NotificationSink> {
    #[cfg(target_os = "linux")]
    {
        Arc::new(DesktopSink::default())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Arc::new(NoSink)
    }
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct DesktopSink {
    present: std::sync::OnceLock<bool>,
}

#[cfg(target_os = "linux")]
impl NotificationSink for DesktopSink {
    fn availability(&self) -> Option<bool> {
        self.present.get().copied()
    }

    fn probe(&self) {
        self.present
            .get_or_init(fauna_credential_store::keyring_probe);
    }

    fn post(&self, n: &PushNotificationPayload) -> Result<(), String> {
        notify_rust::Notification::new()
            .summary(&n.title)
            .body(&n.body)
            .appname("Fauna")
            .show()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// A platform whose agent arm is not built: never a sink.
#[cfg_attr(target_os = "linux", allow(dead_code))]
struct NoSink;

impl NotificationSink for NoSink {
    fn availability(&self) -> Option<bool> {
        Some(false)
    }
    fn probe(&self) {}
    fn post(&self, _: &PushNotificationPayload) -> Result<(), String> {
        Err("no notification sink on this platform".into())
    }
}

/// Post every `fauna.push.notification` frame on `pushes` while no app is
/// attached, until the push stream closes (the connection's client is gone).
pub async fn run(
    mut pushes: broadcast::Receiver<PushEvent>,
    attached: Arc<AttachedApps>,
    sink: Arc<dyn NotificationSink>,
) {
    loop {
        let notification = match pushes.recv().await {
            Ok(PushEvent::PushNotification(n)) => n,
            Ok(_) => continue,
            // A burst we fell behind on: the banners are best-effort.
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if attached.any() {
            // The open app received the same frame and owns the banner.
            continue;
        }
        if sink.availability() != Some(true) {
            continue;
        }
        let sink = Arc::clone(&sink);
        let posted = tokio::task::spawn_blocking(move || sink.post(&notification)).await;
        match posted {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("push arm: the banner was not posted: {e}"),
            Err(e) => tracing::warn!("push arm: the post task failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeSink(Mutex<Vec<String>>);
    impl NotificationSink for FakeSink {
        fn availability(&self) -> Option<bool> {
            Some(true)
        }
        fn probe(&self) {}
        fn post(&self, n: &PushNotificationPayload) -> Result<(), String> {
            self.0.lock().unwrap().push(n.title.clone());
            Ok(())
        }
    }

    fn frame(title: &str) -> PushEvent {
        PushEvent::PushNotification(PushNotificationPayload {
            title: title.into(),
            body: "body".into(),
            url: "/app/inbox".into(),
            extra: Default::default(),
        })
    }

    /// Feed `events` through the arm and return what it posted. Closing the
    /// channel ends the arm, so the run is deterministic: no settle sleep.
    async fn posted(
        events: Vec<PushEvent>,
        attached: Arc<AttachedApps>,
        sink: Arc<FakeSink>,
    ) -> Vec<String> {
        let (tx, rx) = broadcast::channel(16);
        for e in events {
            tx.send(e).unwrap();
        }
        drop(tx);
        run(rx, attached, Arc::clone(&sink) as Arc<dyn NotificationSink>).await;
        sink.0.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn with_no_app_attached_a_frame_is_posted_once() {
        let posts = posted(vec![frame("New message")], Arc::default(), Arc::default()).await;
        assert_eq!(posts, vec!["New message".to_string()]);
    }

    #[tokio::test]
    async fn while_an_app_is_attached_nothing_is_posted() {
        let attached = Arc::<AttachedApps>::default();
        let _lease = attached.attach();
        let posts = posted(vec![frame("New message")], attached, Arc::default()).await;
        assert!(posts.is_empty(), "the open app owns the banner: {posts:?}");
    }

    #[tokio::test]
    async fn once_the_app_detaches_the_agent_posts_again() {
        let attached = Arc::<AttachedApps>::default();
        drop(attached.attach());
        assert!(!attached.any());
        let posts = posted(vec![frame("Group invite")], attached, Arc::default()).await;
        assert_eq!(posts, vec!["Group invite".to_string()]);
    }
}
