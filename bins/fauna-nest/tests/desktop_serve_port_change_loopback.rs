//! Integration test for **same-box reach across an admin serving-port change**:
//! the co-located IPC loopback listener (`127.0.0.1:<canonical>`) must stay
//! reachable when the admin moves the *external* serving port and the loop
//! restarts to re-bind it (`docs/goal/architecture/nest/common.md` § Serving ports
//! → § Same-box reach).
//!
//! This is the composition of the two sibling serve-loop proofs:
//! `desktop_serve_loopback.rs` (the loopback binds and serves) and
//! `desktop_serve_loop.rs::serve_loop_rebinds_onto_an_admin_changed_serving_port`
//! (the external listener moves on an admin change). Neither covers the claim that
//! actually matters to a desktop install: **the external listener moves, the fixed
//! loopback does not** — so the co-located MDA bridge (`--nest-endpoint
//! https://127.0.0.1:3000`, `libs/fauna-mda-supervisor`) and the same-box app
//! (`FaunaApp DefaultNestPort`) keep reaching the nest, with no strand and no
//! manual service action.
//!
//! It lives in its **own** test binary (Cargo runs each `tests/*.rs` in a separate
//! process) because the loop mutates the process-global `FAUNA_INTERNAL_LOOPBACK_PORT`
//! env — the same reason `desktop_serve_loopback.rs` is separate from
//! `desktop_serve_loop.rs`.
//!
//! Residual after this test: only the OS-shell wiring that *drives* the loop (the
//! Windows SCM `fauna-nest-service` and its real `device.toml`), not the loop.

use std::net::SocketAddr;
use std::time::Duration;

use fauna_nest::desktop_serve::{ServeLoopConfig, run_serve_loop};

/// Claim a free loopback port the way production picks the fixed `3000`: bind a
/// throwaway listener to take an OS-assigned port, capture it, drop it.
fn free_port() -> u16 {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
    probe.local_addr().expect("probe local_addr").port()
}

/// Poll `127.0.0.1:<port>/internal/router-status` until it reports healthy, or
/// give up. Returns the last error for a diagnostic assertion message.
async fn serves_healthy(client: &reqwest::Client, port: u16) -> Result<(), String> {
    let url = format!("http://127.0.0.1:{port}/internal/router-status");
    let mut last_err = "never attempted".to_string();
    for _ in 0..80 {
        match client.get(&url).send().await {
            Ok(resp) if resp.status() == 200 => {
                let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
                if body["healthy"].as_bool() == Some(true) {
                    return Ok(());
                }
                last_err = format!("status 200 but healthy={:?}", body["healthy"]);
            }
            Ok(resp) => last_err = format!("status {}", resp.status()),
            Err(e) => last_err = e.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(last_err)
}

/// The admin moves the external serving port; the **fixed co-located loopback
/// survives the restart**, so every same-box dialer keeps reaching the nest.
#[tokio::test]
async fn colocated_loopback_survives_an_admin_serving_port_change() {
    // SAFETY: set once at the top of this single-test binary before the loop reads
    // it; nothing else mutates the env concurrently (own process — see module doc).
    unsafe {
        std::env::set_var("FAUNA_INSECURE_DISABLE_TLS", "1");
    }

    // Two distinct free ports: the fixed co-located loopback, and the admin's new
    // external choice. They must differ — `resolve_internal_loopback_addr`'s
    // equal-port guard suppresses the loopback when it equals the external bind,
    // which would make the post-restart assertion vacuous.
    let loopback_port = free_port();
    let mut new_port = free_port();
    while new_port == loopback_port {
        new_port = free_port();
    }

    let data_dir = tempfile::tempdir().expect("tempdir");

    let (ready_tx, mut ready_rx) = tokio::sync::mpsc::unbounded_channel::<SocketAddr>();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let cfg = ServeLoopConfig {
        // Ephemeral external bind; the admin's choice arrives later via the DB.
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        data_dir: data_dir.path().to_path_buf(),
        default_serving_port: 3000,
        // The desktop shells' shape: ask for the co-located IPC loopback listener
        // (production passes the fixed 3000; we pass an ephemeral).
        internal_loopback_port: Some(loopback_port),
    };

    let loop_handle = tokio::spawn(async move {
        run_serve_loop(
            cfg,
            None, // direct-bind path (no launchd socket activation)
            async move {
                let _ = shutdown_rx.await;
            },
            Some(ready_tx),
        )
        .await
    });

    // First (re)start: fresh DB carries no `serving_port` singleton, so the bind
    // seed's ephemeral port stands.
    let first = tokio::time::timeout(Duration::from_secs(20), ready_rx.recv())
        .await
        .expect("loop bound a listener within 20s")
        .expect("ready channel delivered the first bound addr");
    assert_ne!(
        first.port(),
        loopback_port,
        "external bind reused the loopback port; the equal-port guard suppressed the \
         co-located listener (re-run — ~0 probability ephemeral collision)"
    );
    assert_ne!(
        first.port(),
        new_port,
        "precondition: the first bind must not already be the admin's new port (re-run)"
    );

    let client = reqwest::Client::new();

    // Precondition: the co-located loopback the MDA bridge dials is up.
    serves_healthy(&client, loopback_port)
        .await
        .unwrap_or_else(|e| {
            panic!("precondition: co-located loopback serves before the change: {e}")
        });

    // The admin action, exactly as `fauna.admin.set_serving_port` performs it
    // (`node_policy_handlers.rs::set_serving_port_handler`): (1) persist the
    // authoritative DB singleton the boot-resolve reads, then (2) materialize the
    // `<data_dir>/serving-port` value flag the supervisor poll edge-triggers on.
    let db_path = data_dir.path().join("nest.db");
    let mut set_err: Option<String> = None;
    let mut persisted = false;
    for _ in 0..25 {
        match fauna_nest::db::CacheDb::open(&db_path) {
            Ok(db) => match db.set_serving_port(new_port).await {
                Ok(()) => {
                    persisted = true;
                    break;
                }
                Err(e) => set_err = Some(e.to_string()),
            },
            Err(e) => set_err = Some(e.to_string()),
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        persisted,
        "persist the serving_port singleton the boot-resolve reads (last: {set_err:?})"
    );
    fauna_nest::mail_enable::set_serving_port_flag(data_dir.path(), new_port)
        .expect("materialize the <data_dir>/serving-port value flag");

    // The loop aborts + re-enters `start_server`, which boot-resolves the new
    // singleton and re-binds the EXTERNAL listener there.
    let second = tokio::time::timeout(Duration::from_secs(90), ready_rx.recv())
        .await
        .expect(
            "the loop re-bound within 90s of the serving-port change \
             (the flag-poll restart never fired)",
        )
        .expect("ready channel delivered the second bound addr");
    assert_eq!(
        second.port(),
        new_port,
        "the external listener moved to the admin's new serving port"
    );
    serves_healthy(&client, new_port)
        .await
        .unwrap_or_else(|e| panic!("the re-bound external listener serves on {new_port}: {e}"));

    // THE POINT: the fixed co-located loopback did NOT move. Every same-box dialer
    // (the MDA bridge via `device.toml.nest_port`, the app via `DefaultNestPort`)
    // still reaches the nest on it — no strand across the serving-port change.
    // A re-bind race on this port (the restart drops the old listener and re-binds
    // the same address) is absorbed by the retry inside `serves_healthy`.
    serves_healthy(&client, loopback_port)
        .await
        .unwrap_or_else(|e| {
            panic!(
                "SAME-BOX STRAND: the fixed co-located loopback (127.0.0.1:{loopback_port}) \
             stopped serving after the external serving port moved to {new_port}. The \
             co-located MDA bridge and app dial this port and would lose the nest: {e}"
            )
        });

    // A shutdown signal still ends the loop cleanly after a restart.
    shutdown_tx.send(()).expect("send shutdown");
    let result = tokio::time::timeout(Duration::from_secs(10), loop_handle)
        .await
        .expect("loop returned within 10s of shutdown")
        .expect("loop task did not panic");
    assert!(
        result.is_ok(),
        "loop returns Ok(()) on shutdown after a serving-port restart: {result:?}"
    );
}
