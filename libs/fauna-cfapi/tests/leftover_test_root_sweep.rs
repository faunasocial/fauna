//! The dev fleet's leftover-root sweep (`fauna_cfapi::test_roots`), live: a
//! `Fauna!` shell + filter root in a `tempfile` dir under the user's temp dir
//! that no live test holds is removed whole, its tempdir with it; one
//! registered while a test holds `SHELL_TEST_MUTEX` is left exactly as it was;
//! and while the hold harness holds `HOLD_MUTEX` the sweep touches nothing.
//!
//! ```text
//! cmd /c "scripts\cargo-win.cmd test -p fauna-cfapi --test leftover_test_root_sweep"
//! ```
//!
//! The sweep it runs is the real one, over the whole machine: any other leftover
//! test root on the box goes with this test's own, as it would in the merge-gate
//! check's pass.

#![cfg(all(windows, any(debug_assertions, feature = "test-helpers")))]

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fauna_cfapi::test_roots::{
    HOLD_MUTEX, SHELL_TEST_MUTEX, Sweep, sweep_leftover_test_roots, user_temp_dirs,
};
use fauna_ipc::test_support::NamedMutexTestGuard;

/// Long enough to outlast another checkout's cfapi test holding the lock.
const PATIENT: Duration = Duration::from_secs(300);

/// A shell (and so filter) registration of `<tempdir>\root`, as a live test makes one.
fn register_in(tmp: &Path, account: &str) -> (String, PathBuf) {
    let root = tmp.join("root");
    std::fs::create_dir_all(&root).expect("mkdir root");
    let id = fauna_cfapi::register_sync_root_with_shell(
        &root.to_string_lossy(),
        "Fauna – sweep test",
        account,
    )
    .expect("register with shell");
    (id, root)
}

fn listed(id: &str) -> bool {
    fauna_cfapi::list_shell_sync_roots()
        .expect("list shell roots")
        .iter()
        .any(|r| r.id == id)
}

fn filter_registered(root: &Path) -> bool {
    fauna_cfapi::is_filter_registered(&root.to_string_lossy()).expect("filter query")
}

/// One test, three phases in order: each phase's lock state would change the
/// others' outcome if libtest ran them side by side.
#[test]
fn a_sweep_removes_an_unheld_test_root_and_spares_a_held_one() {
    // The sweep only looks where tempfile puts a test's root.
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(
        fauna_cfapi::test_roots::test_root_tempdir(
            &tmp.path().join("root").to_string_lossy(),
            &user_temp_dirs()
        )
        .is_some(),
        "this test's tempdir {} is not where the sweep looks — {:?}",
        tmp.path().display(),
        user_temp_dirs()
    );

    // ── 1. A killed run's root: registered under the lock, the lock gone, the
    //       registration and the tempdir left behind. ──
    let leftover_dir = tmp.keep();
    let (leftover_id, leftover_root) = {
        let _serial = NamedMutexTestGuard::acquire(SHELL_TEST_MUTEX);
        register_in(&leftover_dir, "sweep-test-leftover")
    };
    assert!(
        listed(&leftover_id),
        "precondition: the leftover is shell-registered"
    );
    assert!(
        filter_registered(&leftover_root),
        "precondition: the leftover is filter-registered"
    );

    match sweep_leftover_test_roots(PATIENT).expect("sweep") {
        Sweep::Swept(report) => assert!(
            report.removed.contains(&leftover_id),
            "the sweep did not remove the unheld leftover {leftover_id}: {report:?}"
        ),
        held => panic!("no live test holds the leftover, yet the sweep said {held}"),
    }
    assert!(
        !listed(&leftover_id),
        "the leftover's shell entry survived the sweep"
    );
    assert!(
        !leftover_dir.exists(),
        "the leftover's tempdir {} survived the sweep",
        leftover_dir.display()
    );

    // ── 2. A live test's root: registered and held under `SHELL_TEST_MUTEX` on
    //       another thread (the mutex is re-entrant for its owner). ──
    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let holder = thread::spawn(move || {
        let _serial = NamedMutexTestGuard::acquire(SHELL_TEST_MUTEX);
        let tmp = tempfile::tempdir().expect("tempdir");
        let (id, root) = register_in(tmp.path(), "sweep-test-held");
        ready_tx.send((id.clone(), root.clone())).unwrap();
        let _ = done_rx.recv();
        let entry = fauna_cfapi::ShellSyncRoot {
            id,
            path: Some(root.to_string_lossy().into_owned()),
        };
        // Best-effort: a sweep that wrongly took the held root has already
        // unregistered it, and the assertions below say so.
        let _ = fauna_cfapi::unregister_shell_root(&entry);
    });
    let (held_id, held_root) = ready_rx.recv().expect("the holder registered");

    let outcome = sweep_leftover_test_roots(Duration::from_secs(2)).expect("sweep");
    let untouched = listed(&held_id) && filter_registered(&held_root) && held_root.exists();
    done_tx.send(()).unwrap();
    holder.join().expect("holder thread");
    assert!(
        matches!(outcome, Sweep::Held(m) if m == SHELL_TEST_MUTEX),
        "a live test holds {SHELL_TEST_MUTEX}, yet the sweep said {outcome}"
    );
    assert!(untouched, "the sweep touched the held root {held_id}");

    // ── 3. The hold harness: `HOLD_MUTEX` held, `SHELL_TEST_MUTEX` free. ──
    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let hold = thread::spawn(move || {
        let _hold = NamedMutexTestGuard::acquire(HOLD_MUTEX);
        ready_tx.send(()).unwrap();
        let _ = done_rx.recv();
    });
    ready_rx.recv().expect("the hold harness holds");
    let outcome = sweep_leftover_test_roots(PATIENT).expect("sweep");
    done_tx.send(()).unwrap();
    hold.join().expect("hold thread");
    assert!(
        matches!(outcome, Sweep::Held(m) if m == HOLD_MUTEX),
        "the hold harness holds {HOLD_MUTEX}, yet the sweep said {outcome}"
    );
}
