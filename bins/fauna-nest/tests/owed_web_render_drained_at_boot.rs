//! tier_3: **a nest boot renders every site a render is still owed to.**
//!
//! Proof obligation (`docs/goal/behavior/web-content-hosting.md` § Routing,
//! render, serving → *A revoke is durable*): a door that removes content from a
//! rendered site records the owed render in its own transaction, and a nest
//! that stopped before rendering must honour that marker on its way back up —
//! nothing else renders on a schedule, so without the boot drain the removed
//! page serves until some unrelated trigger happens to re-render the site.
//!
//! Written against the **whole boot**, like `index_survives_nest_restart.rs`:
//! it drives the real shared serve loop
//! (`fauna_nest::desktop_serve::run_serve_loop`, the same construction +
//! `start_server` path every deployment runs) over a data dir that holds the
//! torn state at rest, then re-opens the nest's own `nest.db`. A test calling
//! `WebContentService::drain_owed_renders` directly could not see the drain
//! being dropped from the boot path; this one reddens when it is.
//!
//! Red before the drain is wired into `start_server`, green after.
//!
//! The same drain owes a second thing, and this boot pins it too: a site an
//! earlier failed render took dark gets its restore attempted (§ *A blanked
//! site is owed its restore*). The restore retry is started by the drain
//! itself, so this is the one piece of boot wiring either rests on.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use fauna_core::data::ContentHash;
use fauna_nest::blob_store::{BlobStoreBackend, DiskBlobStore};
use fauna_nest::db::CacheDb;
use fauna_nest::desktop_serve::{ServeLoopConfig, run_serve_loop};

#[tokio::test]
async fn a_boot_renders_the_site_a_torn_revoke_left_owed() {
    // Plain HTTP so the loop needs no TLS floor material — the same escape the
    // sibling serve-loop tests use.
    // SAFETY: set once at the top of this single-threaded test before the loop
    // (which reads it) is spawned; nothing else mutates the env concurrently.
    unsafe {
        std::env::set_var("FAUNA_INSECURE_DISABLE_TLS", "1");
    }

    let data_dir = tempfile::tempdir().expect("tempdir");
    let db_path = data_dir.path().join("nest.db");
    let blob_dir = data_dir.path().join("blobs");
    std::fs::create_dir_all(&blob_dir).expect("blob dir");

    // ---- Arrange: the torn state at rest. A rendered page for a post the
    // author has since unpublished — the unpublish's own transaction wrote the
    // owed-render marker (the real writer, not a hand-inserted row), and the
    // nest stopped before the render that would have dropped the page.
    let actor = [0xAB; 32];
    let post = [0xCD; 32];
    let blanked = [0xEF; 32];
    {
        let db = Arc::new(CacheDb::open(&db_path).expect("open nest.db"));
        let store: Arc<dyn BlobStoreBackend> =
            Arc::new(DiskBlobStore::new(&blob_dir).expect("blob store"));
        let page = b"<html>the page of a post its author withdrew</html>";
        let hash: [u8; 32] = *blake3::hash(page).as_bytes();
        store
            .put(&ContentHash::from_digest_raw(hash), page)
            .await
            .expect("put rendered page");
        let render = db
            .begin_web_render(&actor)
            .await
            .expect("open the staging render's claim");
        db.upsert_web_rendered(&render, "post/withdrawn.html", &hash, "text/html")
            .await
            .expect("stage the rendered page");
        db.publish_web_post(&actor, &post, "withdrawn")
            .await
            .expect("publish");
        db.unpublish_web_post(&actor, &post)
            .await
            .expect("unpublish");
        assert_eq!(
            db.list_web_render_owed().await.expect("list owed"),
            vec![actor],
            "precondition: the unpublish's transaction recorded the owed render"
        );
        // A second site, one an earlier failed render took dark: the
        // fail-closed clear's own writer left its restore owed (§ *A blanked
        // site is owed its restore*).
        db.clear_web_rendered_owing_restore(&blanked)
            .await
            .expect("the fail-closed clear");
        assert_eq!(
            db.list_web_restore_owed().await.expect("list restores"),
            vec![blanked],
            "precondition: the clear recorded the owed restore"
        );
    }

    // ---- Act: a real nest boot over that data dir, then a clean shutdown.
    let (ready_tx, mut ready_rx) = tokio::sync::mpsc::unbounded_channel::<SocketAddr>();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let cfg = ServeLoopConfig {
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        data_dir: data_dir.path().to_path_buf(),
        default_serving_port: 3000,
        internal_loopback_port: None,
    };
    let loop_handle = tokio::spawn(async move {
        run_serve_loop(
            cfg,
            None,
            async move {
                let _ = shutdown_rx.await;
            },
            Some(ready_tx),
        )
        .await
    });

    // The loop reports its bound address once the nest is serving — i.e. once
    // every boot reconcile ahead of `axum::serve` has run.
    tokio::time::timeout(Duration::from_secs(60), ready_rx.recv())
        .await
        .expect("nest booted and bound a listener")
        .expect("ready channel delivered the bound addr");

    shutdown_tx.send(()).expect("send shutdown");
    tokio::time::timeout(Duration::from_secs(30), loop_handle)
        .await
        .expect("serve loop returned after shutdown")
        .expect("serve loop task did not panic")
        .expect("serve loop returned Ok(())");

    // ---- Assert: the withdrawn page is gone and the marker is discharged.
    let db = Arc::new(CacheDb::open(&db_path).expect("re-open nest.db"));
    assert!(
        db.get_web_rendered(&actor, "post/withdrawn.html")
            .await
            .expect("read rendered page")
            .is_none(),
        "the boot left a withdrawn post's rendered page in place — the owed render was \
         never drained, so the page serves until some unrelated trigger re-renders the site"
    );
    assert!(
        db.list_web_render_owed()
            .await
            .expect("list owed")
            .is_empty(),
        "the boot drain rendered the site and must discharge its marker"
    );
    assert!(
        db.list_web_restore_owed()
            .await
            .expect("list restores")
            .is_empty(),
        "the boot made no render attempt for a site a failed render had taken dark — its \
         restore is still owed, so the site stays blank until its author happens to publish"
    );
}
