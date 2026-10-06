//! The store conformance suite over web's IndexedDB arm, in a browser
//! (`just wasm-test-check`: `wasm-pack test --headless --firefox`). The same
//! case list every native arm grades (`fauna_account_store::conformance`), so
//! the web replica is held to the one statement of the `StoreBackend`
//! contract.

#![cfg(target_arch = "wasm32")]

use std::sync::atomic::{AtomicU32, Ordering};

use fauna_account_store::conformance::Medium;
use fauna_account_store::indexeddb::IndexedDbBackend;

// No Node.js on the dev machines (the SPA builds with Deno), so the runner
// needs the browser; one configure per test binary.
wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

/// One IndexedDB database per case. The name carries the run's start time as
/// well as a counter, so a case never inherits a store an earlier run of the
/// suite left in the same browser profile.
struct IndexedDbMedium(String);

impl Medium for IndexedDbMedium {
    type Backend = IndexedDbBackend;

    fn fresh() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        Self(format!(
            "fauna-conformance-{}-{}",
            js_sys::Date::now() as u64,
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    async fn open(&self) -> IndexedDbBackend {
        IndexedDbBackend::open(&self.0).await.unwrap()
    }

    async fn segment_files(&self) -> Option<Vec<String>> {
        // Never through `open()`: an open sweeps staging files.
        Some(
            IndexedDbBackend::segment_files_of_unopened_for_test(&self.0)
                .await
                .unwrap(),
        )
    }
}

fauna_account_store::store_conformance_suite!(
    IndexedDbMedium,
    wasm_bindgen_test::wasm_bindgen_test
);

// ── The web arm's own crash window ───────────────────────────────────────────

/// A departure whose row transaction committed but whose OPFS files were not
/// yet swept (the tab closed between the two) is finished by the next open —
/// "atomic or resumable", never a half-departed scope (`nest/common.md`
/// § Client-state recoverability). Observed on the file area itself: a
/// row-only drop would leave the departed bytes in OPFS.
#[wasm_bindgen_test::wasm_bindgen_test]
async fn a_departure_interrupted_before_its_file_sweep_resumes_at_open() {
    use fauna_account_store::conformance::fixtures::*;

    let medium = IndexedDbMedium::fresh();
    let s = store_on(&medium).await;
    let (dat, meta) = real_segment("post", 1, SEG_ACTOR, &[b"one record"]);
    s.adopt_segment(&seg_post_scope(), &dat, &meta)
        .await
        .unwrap()
        .expect("adopted");

    // Exactly what a tab closed between the commit and the sweep leaves.
    let counts = s
        .backend()
        .drop_scope_rows_for_test(&seg_post_scope())
        .await
        .unwrap();
    assert_eq!(counts.segments, 1, "{counts:?}");
    assert_eq!(
        s.backend().segment_files_for_test().await.unwrap().len(),
        2,
        "the .dat/.meta pair outlives the interrupted drop"
    );
    drop(s);

    let reopened = medium.open().await;
    assert!(
        reopened.segment_files_for_test().await.unwrap().is_empty(),
        "opening the store swept the files the interrupted departure owed"
    );
}

// ── A caller that polls late ─────────────────────────────────────────────────

/// A method's transaction does not depend on how promptly its caller polls
/// it. The account driver serves a local command inside a pass's yield point
/// (`fauna_account_plane::account_driver::drive::drive_pass`), and while that
/// command runs the pass is not polled: a read-then-write method the pass had
/// open saw its read answered, nobody ran the continuation, the browser
/// committed the request-less transaction, and the write met
/// `TransactionInactiveError` — on web, every pass whose relay put straddled a
/// command. Polled once, left alone for another method's whole round trip,
/// then awaited: the row lands.
#[wasm_bindgen_test::wasm_bindgen_test]
async fn a_method_its_caller_polls_late_still_runs_inside_its_transaction() {
    use fauna_account_store::backend::StoreBackend;
    use fauna_account_store::conformance::fixtures::*;
    use std::future::Future;
    use std::task::Poll;

    let backend = IndexedDbMedium::fresh().open().await;
    let row = relay_row(7, 1, &[0x11; 32], b"sealed");
    let mut put = std::pin::pin!(backend.relay_put(&row));
    let mut polled = false;
    std::future::poll_fn(|cx| {
        if !std::mem::replace(&mut polled, true) {
            assert!(
                put.as_mut().poll(cx).is_pending(),
                "a relay put is a read then a write — it cannot finish in one poll"
            );
        }
        Poll::Ready(())
    })
    .await;
    // The caller is busy elsewhere, as a select arm's handler is.
    backend.meta_put("elsewhere", b"1").await.unwrap();
    assert!(backend.meta_get("elsewhere").await.unwrap().is_some());
    put.await
        .expect("the put ran to its commit while its caller was elsewhere");
    let held = backend
        .relay_rows_of_writer(&row.scope, &row.item_class, &row.writer)
        .await
        .unwrap();
    assert_eq!(held.len(), 1, "the relay row landed: {held:?}");
}

// ── The store root's web leg ─────────────────────────────────────────────────

/// One store per actor: the name is the root plus the actor's one lowercase
/// hex spelling, and a string that is not an actor id is refused.
#[wasm_bindgen_test::wasm_bindgen_test]
fn the_store_name_is_the_actors_one_spelling_under_the_root() {
    use fauna_account_store::root::StoreRoot;
    let root = StoreRoot::platform();
    let upper = "AB".repeat(32);
    assert_eq!(
        root.store_name(&upper).unwrap(),
        format!("fauna-account-store/{}", "ab".repeat(32))
    );
    assert_eq!(
        root.store_name(&upper).unwrap(),
        root.store_name(&"ab".repeat(32)).unwrap()
    );
    assert!(root.store_name("not-an-actor").is_err());
    assert!(root.store_name(&"ab".repeat(31)).is_err());
}

// ── The engine lock's web leg (Web Locks) ────────────────────────────────────

use fauna_account_store::locks::{EngineLock, EngineLockOutcome};

/// The role is won once: a second try on the same store is `Refused` while the
/// first holds it, another store's role is independent, and a release hands
/// the role on — the native leg's contract over the browser's lock manager.
#[wasm_bindgen_test::wasm_bindgen_test]
async fn the_engine_role_is_held_once_per_store_and_released_on_demand() {
    let store = IndexedDbMedium::fresh().0;
    let EngineLockOutcome::Held(first) = EngineLock::try_acquire(&store).await else {
        panic!("a fresh store's role is free");
    };
    assert!(
        matches!(
            EngineLock::try_acquire(&store).await,
            EngineLockOutcome::Refused
        ),
        "a held role refuses a second try — never waits for it"
    );
    let other = IndexedDbMedium::fresh().0;
    assert!(
        matches!(
            EngineLock::try_acquire(&other).await,
            EngineLockOutcome::Held(_)
        ),
        "another store's role is its own"
    );
    first.release().await;
    assert!(
        matches!(
            EngineLock::try_acquire(&store).await,
            EngineLockOutcome::Held(_)
        ),
        "released, the role is winnable again"
    );
}

/// The seed-leg role is a second lock with the same mechanics, independent of
/// the engine role on the same store (`account-runtime.md` § Multi-instance
/// concurrency → *The seed-leg role*, part 1).
#[wasm_bindgen_test::wasm_bindgen_test]
async fn the_seed_leg_role_is_held_once_per_store_beside_the_engine_role() {
    use fauna_account_store::locks::{SeedLegLock, SeedLegLockOutcome};

    let store = IndexedDbMedium::fresh().0;
    let EngineLockOutcome::Held(_engine) = EngineLock::try_acquire(&store).await else {
        panic!("a fresh store's engine role is free");
    };
    let SeedLegLockOutcome::Held(first) = SeedLegLock::try_acquire(&store).await else {
        panic!("the engine role's holder does not hold the seed-leg role's lock");
    };
    assert!(
        matches!(
            SeedLegLock::try_acquire(&store).await,
            SeedLegLockOutcome::Refused
        ),
        "a held role refuses a second try — never waits for it"
    );
    first.release().await;
    assert!(
        matches!(
            SeedLegLock::try_acquire(&store).await,
            SeedLegLockOutcome::Held(_)
        ),
        "released, the role is winnable again"
    );
}

/// `open_existing` never mints a store (`nest/box-recovery.md` § The
/// plane-era recovery floor, *(b)*: the local read creates nothing). The
/// second refusal is the proof: had the first created the database, the
/// second would find it at the current version and open it.
#[wasm_bindgen_test::wasm_bindgen_test]
async fn open_existing_creates_no_database_and_opens_one_that_exists() {
    let name = IndexedDbMedium::fresh().0;
    assert!(
        IndexedDbBackend::open_existing(&name)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        IndexedDbBackend::open_existing(&name)
            .await
            .unwrap()
            .is_none(),
        "the first refusal left no database behind"
    );
    let created = IndexedDbBackend::open(&name).await.unwrap();
    assert!(
        IndexedDbBackend::open_existing(&name)
            .await
            .unwrap()
            .is_some(),
        "an existing store opens"
    );
    drop(created);
}

/// A delete blocked behind a connection that will not close answers `Err`
/// instead of waiting for it (`apps/account-scoping.md` § The scoping taxonomy
/// → *Erasure follows scope*, the web paragraph's decision 4), leaves the
/// store whole, and a delete after the holder is gone succeeds. The holder is
/// a plain connection with no `versionchange` handler — what another build's
/// tab is to this one.
#[wasm_bindgen_test::wasm_bindgen_test]
async fn a_delete_another_connection_blocks_answers_instead_of_waiting() {
    use wasm_bindgen::JsCast;

    let name = IndexedDbMedium::fresh().0;
    drop(IndexedDbBackend::open(&name).await.unwrap());

    let factory: web_sys::IdbFactory = js_sys::Reflect::get(&js_sys::global(), &"indexedDB".into())
        .unwrap()
        .unchecked_into();
    let request = factory.open(&name).unwrap();
    wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |resolve, reject| {
        request.set_onsuccess(Some(&resolve));
        request.set_onerror(Some(&reject));
    }))
    .await
    .expect("the holding connection opens");
    let holder: web_sys::IdbDatabase = request.result().unwrap().unchecked_into();

    let blocked = IndexedDbBackend::delete(&name)
        .await
        .expect_err("a delete behind a connection that will not close is an error");
    assert!(
        format!("{blocked:#}").contains("blocked"),
        "the error names the block: {blocked:#}"
    );

    holder.close();
    // The browser kept the blocked request queued and runs it now; this one
    // queues behind it and finds nothing left, which deletes cleanly.
    IndexedDbBackend::delete(&name)
        .await
        .expect("with the holder gone the delete succeeds");
    assert!(
        IndexedDbBackend::open_existing(&name)
            .await
            .unwrap()
            .is_none(),
        "no store is left"
    );
}
