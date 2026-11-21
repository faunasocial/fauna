//! Connection-scoped state for the async IPC servers: a request handler can
//! tie a value's lifetime to the client connection the request rode in on.
//!
//! The server loop ([`crate::frame_io::handle_conn`]) runs every handler
//! inside its connection's scope and drops the scope when the client closes
//! the connection — a clean exit or a crash alike, since the kernel closes the
//! socket or pipe either way. That is what an *attachment* needs: the sync
//! agent counts an app as open for as long as the connection that sent
//! [`RequestMethod::AttachApp`](crate::sync::RequestMethod::AttachApp) stays
//! open, with no heartbeat to tune and no window in which a closed app still
//! reads as attached (`sync-agent.md` § Scope per platform, the push wake
//! stand-in).
//!
//! The handler signature stays `Fn(Req) -> F`: the scope travels as a tokio
//! task-local, so neither transport nor any existing handler changes. A value
//! held from a task the handler *spawned* is outside the scope and is refused.

use std::any::Any;
use std::sync::{Arc, Mutex};

tokio::task_local! {
    static CONNECTION: Arc<ConnectionScope>;
}

/// The values one connection holds until it closes.
#[derive(Default)]
pub(crate) struct ConnectionScope {
    held: Mutex<Held>,
}

#[derive(Default)]
struct Held {
    values: Vec<Box<dyn Any + Send>>,
    closed: bool,
}

impl ConnectionScope {
    /// Run `fut` inside this connection's scope.
    pub(crate) async fn run<F: std::future::Future>(self: &Arc<Self>, fut: F) -> F::Output {
        CONNECTION.scope(Arc::clone(self), fut).await
    }

    /// The connection is over: drop every held value now, and refuse any
    /// later hold. A handler still running when its client hung up keeps
    /// this scope alive (it runs on, detached), but what the connection held
    /// must not outlive the connection itself.
    pub(crate) fn close(&self) {
        let values = {
            let mut held = self.lock();
            held.closed = true;
            std::mem::take(&mut held.values)
        };
        drop(values); // outside the lock: a value's Drop may itself take locks
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Held> {
        self.held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Keep `value` alive until the client closes the connection the current
/// request arrived on, then drop it. Returns `false` — and drops `value` at
/// once — when called outside a served connection (a unit test calling a
/// handler directly, or a task the handler spawned), or after that client has
/// already hung up.
pub fn hold_for_connection<T: Send + 'static>(value: T) -> bool {
    let mut value = Some(value);
    let held = CONNECTION
        .try_with(|scope| {
            let mut held = scope.lock();
            if held.closed {
                return false;
            }
            if let Some(v) = value.take() {
                held.values.push(Box::new(v));
            }
            true
        })
        .unwrap_or(false);
    drop(value); // refused: dropped now, outside the lock
    held
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Flag(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Flag {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn a_held_value_lives_exactly_as_long_as_its_scope() {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let scope = Arc::new(ConnectionScope::default());
        let flag = Flag(Arc::clone(&dropped));
        assert!(scope.run(async move { hold_for_connection(flag) }).await);
        assert!(!dropped.load(std::sync::atomic::Ordering::SeqCst));
        drop(scope);
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_closed_scope_drops_what_it_held_and_refuses_more() {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let scope = Arc::new(ConnectionScope::default());
        let flag = Flag(Arc::clone(&dropped));
        assert!(scope.run(async move { hold_for_connection(flag) }).await);
        scope.close();
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
        let late = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Flag(Arc::clone(&late));
        assert!(!scope.run(async move { hold_for_connection(flag) }).await);
        assert!(late.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn outside_a_connection_nothing_is_held() {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert!(!hold_for_connection(Flag(Arc::clone(&dropped))));
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
    }
}
