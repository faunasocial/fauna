//! A tiny generic helper for the "state machine holds `Mutex<T>`, `snapshot()`
//! returns a clone of the locked value" idiom every `fauna-client-*` UniFFI
//! state machine uses for its polling getter.

use std::sync::Mutex;

/// Lock `m`, clone the value `project` returns a reference to, and return the
/// clone. Panics if the mutex is poisoned (mirrors every existing call site —
/// a poisoned client-state mutex means a prior update panicked mid-mutation,
/// unrecoverable for that machine instance, so surfacing the panic rather
/// than silently returning stale/default state is the right failure mode).
///
/// Covers both shapes `fauna-client-*` state machines use: `Mutex<XSnapshot>`
/// directly (`project` is the identity closure, `|s| s`) and `Mutex<Inner>`
/// where `Inner` carries a `snapshot: XSnapshot` field alongside other
/// machine-private state (`project` extracts the field, `|i| &i.snapshot`).
pub fn clone_locked<T, R: Clone>(m: &Mutex<T>, project: impl FnOnce(&T) -> &R) -> R {
    project(&m.lock().expect("snapshot mutex")).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_the_locked_value_directly() {
        let m = Mutex::new(42i32);
        assert_eq!(clone_locked(&m, |v| v), 42);
    }

    #[test]
    fn clones_a_projected_field() {
        struct Inner {
            snapshot: String,
            _other_state: u8,
        }
        let m = Mutex::new(Inner {
            snapshot: "hello".to_string(),
            _other_state: 7,
        });
        assert_eq!(clone_locked(&m, |i| &i.snapshot), "hello");
    }

    #[test]
    #[should_panic(expected = "snapshot mutex")]
    fn panics_on_a_poisoned_mutex() {
        let m = std::sync::Arc::new(Mutex::new(0i32));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _guard = m2.lock().unwrap();
            panic!("poison it");
        })
        .join();
        clone_locked(&m, |v| v);
    }
}
