//! Integration test for the shared cross-OS desktop nest serve-and-restart loop
//! (`fauna_nest::desktop_serve::run_serve_loop`) — the loop both the Windows
//! `fauna-nest-service` (SCM) and the macOS `fauna-nest-daemon` (LaunchDaemon)
//! shells delegate to (priority #2/#3 — they share one nest-construction +
//! serve loop instead of each reimplementing it).
//!
//! This is the regression guard for the lift: it spins the *actual* shared loop
//! against a throwaway data dir, confirms the nest it constructs **serves** on
//! the bound port (the full `start_server` path — claim seeding, identity,
//! services.json, TLS floor, router), then confirms a shutdown signal makes the
//! loop **return cleanly** (`Ok(())`).
//!
//! `serve_loop_rebinds_onto_an_admin_changed_serving_port` drives the live
//! serving-port **restart** end-to-end (flag poll → abort → re-enter
//! `start_server` → re-bind on the admin's new port), so the residual is now only
//! the OS-shell wiring itself (Windows SCM / macOS launchd), not the loop. The
//! restart *decision* is separately unit-tested in `fauna-nest-supervisor`.

use std::net::SocketAddr;
use std::time::Duration;

use fauna_nest::desktop_serve::{ServeLoopConfig, run_serve_loop};

/// The shared loop constructs a real nest, serves it on the bound (ephemeral)
/// port, and returns `Ok(())` when signalled to shut down.
#[tokio::test]
async fn shared_serve_loop_serves_then_shuts_down_cleanly() {
    // Plain HTTP so the test can reach the nest over `http://` without TOFU —
    // the same escape the tier_3 binary suite uses (conftest.py). The lifted
    // loop honours it exactly as the Windows loop does.
    // SAFETY: set once at the top of this single-threaded test before the loop
    // (which reads it) is spawned; nothing else mutates the env concurrently.
    unsafe {
        std::env::set_var("FAUNA_INSECURE_DISABLE_TLS", "1");
    }

    let data_dir = tempfile::tempdir().expect("tempdir");

    let (ready_tx, mut ready_rx) = tokio::sync::mpsc::unbounded_channel::<SocketAddr>();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let cfg = ServeLoopConfig {
        // Ephemeral port — `start_server` binds an OS-assigned free port (no
        // serving-port singleton in a fresh DB, so the seed port stands).
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        data_dir: data_dir.path().to_path_buf(),
        default_serving_port: 3000,
        // No co-located internal loopback listener in the test (keeps the test
        // from binding/poking the fixed 127.0.0.1:3000 IPC port).
        internal_loopback_port: None,
    };

    let loop_handle = tokio::spawn(async move {
        run_serve_loop(
            cfg,
            // Direct-bind path (no launchd socket activation): the loop binds
            // `cfg.bind` itself.
            None,
            async move {
                let _ = shutdown_rx.await;
            },
            Some(ready_tx),
        )
        .await
    });

    // The loop reports the bound address on (re)start.
    let addr = tokio::time::timeout(Duration::from_secs(20), ready_rx.recv())
        .await
        .expect("loop bound a listener within 20s")
        .expect("ready channel delivered the bound addr");

    // The constructed nest actually serves: `/internal/router-status` is the
    // unauthenticated health view (returns `healthy: true`).
    let client = reqwest::Client::new();
    let resp = tokio::time::timeout(
        Duration::from_secs(10),
        client
            .get(format!("http://{addr}/internal/router-status"))
            .send(),
    )
    .await
    .expect("health request completed")
    .expect("health request succeeded");
    assert_eq!(resp.status(), 200, "nest serves the health endpoint");
    let body: serde_json::Value = resp.json().await.expect("health body is json");
    assert_eq!(
        body["healthy"].as_bool(),
        Some(true),
        "the served nest reports healthy"
    );

    // A shutdown signal ends the loop cleanly.
    shutdown_tx.send(()).expect("send shutdown");
    let result = tokio::time::timeout(Duration::from_secs(10), loop_handle)
        .await
        .expect("loop returned within 10s of shutdown")
        .expect("loop task did not panic");
    assert!(
        result.is_ok(),
        "loop returns Ok(()) on shutdown: {result:?}"
    );
}

/// An admin serving-port change makes the running loop **re-bind its external
/// listener on the new port**, with no service restart — the live proof of the
/// serve-and-restart arm (`nest/common.md` § Serving ports, desktop-direct).
///
/// The nest cannot hot-rebind a `TcpListener`, so the change is apply-on-restart:
/// the loop's 15 s poll (`SERVING_PORT_POLL`) sees the `<data_dir>/serving-port`
/// value flag diverge from the value this run started on, aborts the server task,
/// and re-enters `start_server`, which boot-resolves the DB `serving_port`
/// singleton (`resolve_serving_bind_addr`) over the bind seed's port.
///
/// The mutation is performed exactly as `fauna.admin.set_serving_port` performs it
/// (`bins/fauna-nest/src/node_policy_handlers.rs::set_serving_port_handler`):
/// (1) persist the authoritative DB singleton, then (2) materialize the
/// supervisor's value flag. Setting only the flag would prove the restart *fires*
/// but not that the new port *binds* — the boot-resolve reads the DB, not the flag.
#[tokio::test]
async fn serve_loop_rebinds_onto_an_admin_changed_serving_port() {
    // SAFETY: idempotent same-value set (see the sibling tests) — plain HTTP so the
    // test reaches the nest over `http://` without TOFU.
    unsafe {
        std::env::set_var("FAUNA_INSECURE_DISABLE_TLS", "1");
    }

    let data_dir = tempfile::tempdir().expect("tempdir");

    // The admin's new choice. Claim a free port the way the loopback test does
    // (bind a probe, capture its OS-assigned port, drop it) so a parallel run on
    // the same host cannot collide on a hard-coded number.
    let new_port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
        probe.local_addr().expect("probe local_addr").port()
    };

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
            None, // direct-bind path (no launchd socket activation)
            async move {
                let _ = shutdown_rx.await;
            },
            Some(ready_tx),
        )
        .await
    });

    // First (re)start: a fresh DB carries no `serving_port` singleton, so the bind
    // seed's ephemeral port stands.
    let first = tokio::time::timeout(Duration::from_secs(20), ready_rx.recv())
        .await
        .expect("loop bound a listener within 20s")
        .expect("ready channel delivered the first bound addr");
    assert_ne!(
        first.port(),
        new_port,
        "precondition: the first bind must not already be the admin's new port \
         (ephemeral collision — re-run)"
    );

    // (1) Persist the authoritative DB singleton. The loop holds its own connection
    // to the same `nest.db`; retry briefly so a transient SQLITE_BUSY against the
    // live nest cannot flake the test.
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

    // (2) Materialize the supervisor's value flag — the on-disk cross-process
    // contract the poll edge-triggers on.
    fauna_nest::mail_enable::set_serving_port_flag(data_dir.path(), new_port)
        .expect("materialize the <data_dir>/serving-port value flag");

    // Within one `SERVING_PORT_POLL` tick the loop aborts + re-enters `start_server`,
    // which boot-resolves the new singleton and binds it. Generous timeout: the poll
    // is 15 s and the re-entered `start_server` rebuilds the whole nest.
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
        "the restarted loop re-bound its external listener on the admin's new \
         serving port (first bind was {first})"
    );

    // And the re-bound nest actually serves there. Retry briefly — `ready` fires as
    // the listener binds, a few ms before the router answers.
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{new_port}/internal/router-status");
    let mut served_healthy = false;
    let mut last_err: Option<String> = None;
    for _ in 0..50 {
        match client.get(&url).send().await {
            Ok(resp) if resp.status() == 200 => {
                let body: serde_json::Value = resp.json().await.expect("health body is json");
                assert_eq!(
                    body["healthy"].as_bool(),
                    Some(true),
                    "the re-bound nest reports healthy on the new serving port"
                );
                served_healthy = true;
                break;
            }
            Ok(resp) => last_err = Some(format!("status {}", resp.status())),
            Err(e) => last_err = Some(e.to_string()),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        served_healthy,
        "the nest never served healthy on the re-bound port {new_port} within 5s \
         (last: {last_err:?})"
    );

    // A shutdown signal still ends the loop cleanly after a restart (the pinned
    // shutdown future survives the re-enter).
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

/// When the caller hands the loop a **pre-bound** listener (the macOS launchd
/// socket-activation path — root pre-binds the privileged `:443` and the
/// non-root `_fauna` daemon inherits the fd), the loop serves the nest on
/// **that** listener's port instead of binding `cfg.bind` itself. This is the
/// regression guard for the `start_server` pre-bound-listener seam (S1) — it
/// proves the seam end-to-end without a real launchd (the live `:443` fd
/// inheritance remains untested: sudo + a real `LaunchDaemon`).
#[tokio::test]
async fn shared_serve_loop_uses_a_pre_bound_activated_listener() {
    // SAFETY: idempotent same-value set (see the sibling test) — plain HTTP so
    // the test reaches the nest over `http://` without TOFU.
    unsafe {
        std::env::set_var("FAUNA_INSECURE_DISABLE_TLS", "1");
    }

    let data_dir = tempfile::tempdir().expect("tempdir");

    // Stand in for launchd's pre-bound socket: bind a real listener on an
    // OS-assigned port and hand its fd to the loop. The `_fauna` daemon would
    // receive this fd from `launch_activate_socket`; the test binds it directly.
    let activated = std::net::TcpListener::bind("127.0.0.1:0").expect("bind activated listener");
    let activated_port = activated.local_addr().expect("activated local_addr").port();

    let (ready_tx, mut ready_rx) = tokio::sync::mpsc::unbounded_channel::<SocketAddr>();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let cfg = ServeLoopConfig {
        // A DISTINCT ephemeral seed: were the seam broken and the loop to bind
        // `cfg.bind` instead of using the pre-bound listener, it would land on a
        // different OS-assigned port and the port assertion below would fail.
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        data_dir: data_dir.path().to_path_buf(),
        default_serving_port: 3000,
        internal_loopback_port: None,
    };

    let loop_handle = tokio::spawn(async move {
        run_serve_loop(
            cfg,
            Some(activated),
            async move {
                let _ = shutdown_rx.await;
            },
            Some(ready_tx),
        )
        .await
    });

    let addr = tokio::time::timeout(Duration::from_secs(20), ready_rx.recv())
        .await
        .expect("loop bound a listener within 20s")
        .expect("ready channel delivered the bound addr");

    // The loop served on the PRE-BOUND listener's port, not a fresh `cfg.bind`.
    assert_eq!(
        addr.port(),
        activated_port,
        "the loop serves on the pre-bound (socket-activated) listener's port, not cfg.bind"
    );

    // And it actually serves there.
    let client = reqwest::Client::new();
    let resp = tokio::time::timeout(
        Duration::from_secs(10),
        client
            .get(format!(
                "http://127.0.0.1:{activated_port}/internal/router-status"
            ))
            .send(),
    )
    .await
    .expect("health request completed")
    .expect("health request succeeded");
    assert_eq!(
        resp.status(),
        200,
        "nest serves the health endpoint on the pre-bound port"
    );

    shutdown_tx.send(()).expect("send shutdown");
    let result = tokio::time::timeout(Duration::from_secs(10), loop_handle)
        .await
        .expect("loop returned within 10s of shutdown")
        .expect("loop task did not panic");
    assert!(
        result.is_ok(),
        "loop returns Ok(()) on shutdown: {result:?}"
    );
}
