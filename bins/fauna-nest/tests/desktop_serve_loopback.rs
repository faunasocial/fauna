//! Integration test for the shared serve loop's **additive co-located-IPC
//! loopback listener** — the fixed `127.0.0.1:<canonical>` listener the macOS
//! `fauna-nest-daemon` (and the Windows `fauna-nest-service`) asks for via
//! `ServeLoopConfig::internal_loopback_port = Some(..)`, which the loop realizes
//! by setting the `FAUNA_INTERNAL_LOOPBACK_PORT` IPC env that `start_server`
//! reads (`nest/common.md` § Same-box reach).
//!
//! This closes the flow-trace gap that S2 turned out
//! to actually need. The other serve-loop tests (`desktop_serve_loop.rs`) both
//! pass `internal_loopback_port: None`, so they never exercise the loopback the
//! **co-located MDA bridge** dials. This one proves it end-to-end on the
//! mac-runnable path: the loop with `internal_loopback_port: Some(P)` binds a
//! **reachable** `127.0.0.1:P` listener that serves the SAME nest as the external
//! listener — i.e. the endpoint the bridge supervisor hands the Go MDA
//! (`--nest-endpoint https://127.0.0.1:3000`, `libs/fauna-mda-supervisor`) is real.
//!
//! It lives in its **own** test binary (Cargo runs each `tests/*.rs` in a separate
//! process) precisely because it mutates the process-global
//! `FAUNA_INTERNAL_LOOPBACK_PORT` env via the loop — keeping it out of the
//! `desktop_serve_loop.rs` binary means the two `None` tests there can't observe a
//! loopback listener they don't expect.
//!
//! ## S2 scope note — why this is a *confirm*, not new wiring
//!
//! S2 was framed as "wire the macOS `device.toml` and a worker key like Windows".
//! Investigation refuted that premise: the co-located MDA worker-authenticates via
//! **service-user enrollment** (it presents its own Ed25519 keypair at
//! `<bridge>/keys/mda.key` and the nest resolves its `BridgeMda` role from the
//! enrollment row) — the cross-OS path Linux/Docker already use, with no
//! `device.toml` and no shared secret. Worker authorization belongs to an entirely
//! separate subsystem (the optional **sidecar storage worker** for a *public* nest
//! scaling beyond one machine — `nest/worker.md`; `WorkerClient` has no production
//! caller, the standalone worker binary is a placeholder), which a personal desktop
//! nest never runs, so the serve config carries no worker state. The only
//! thing the co-located MDA needs from the daemon is a reachable fixed loopback —
//! which the daemon already requests (`fauna-nest-daemon` `serve_config()` →
//! `internal_loopback_port: Some(3000)`). This test is that confirmation.

use std::net::SocketAddr;
use std::time::Duration;

use fauna_nest::desktop_serve::{ServeLoopConfig, run_serve_loop};

/// With `internal_loopback_port: Some(P)`, the loop binds — *alongside* its
/// external listener — a reachable `127.0.0.1:P` listener serving the same nest.
/// This is the endpoint the co-located MDA bridge dials.
#[tokio::test]
async fn serve_loop_binds_a_reachable_colocated_loopback_listener() {
    // Plain HTTP so the test reaches the nest over `http://` without TOFU — the
    // same escape the tier_3 binary suite uses (conftest.py); the loop honours it.
    // SAFETY: set once at the top of this single-test binary before the loop reads
    // it; nothing else mutates the env concurrently (own process — see module doc).
    unsafe {
        std::env::set_var("FAUNA_INSECURE_DISABLE_TLS", "1");
    }

    // Pick a free loopback port the way production picks the FIXED 3000: bind a
    // throwaway listener to claim an OS-assigned free port, capture it, drop it.
    // Using an ephemeral (not the literal 3000) keeps the test from colliding with
    // a real co-located nest or a parallel run on the same host.
    let loopback_port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
        probe.local_addr().expect("probe local_addr").port()
    };

    let data_dir = tempfile::tempdir().expect("tempdir");

    let (ready_tx, mut ready_rx) = tokio::sync::mpsc::unbounded_channel::<SocketAddr>();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let cfg = ServeLoopConfig {
        // Ephemeral external bind — a fresh OS-assigned port distinct from the
        // loopback one (the loopback only binds when its port != the external's;
        // `resolve_internal_loopback_addr`'s equal-port guard). A `:0` external + a
        // just-freed `loopback_port` are independent allocations.
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        data_dir: data_dir.path().to_path_buf(),
        default_serving_port: 3000,
        // The macOS daemon's `serve_config()` shape: ask for the co-located
        // loopback listener (it passes `Some(3000)`; we pass an ephemeral).
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

    // The loop reports the EXTERNAL bound address on (re)start.
    let external_addr = tokio::time::timeout(Duration::from_secs(20), ready_rx.recv())
        .await
        .expect("loop bound a listener within 20s")
        .expect("ready channel delivered the bound addr");

    // Precondition: the external ephemeral didn't (cosmically unlikely) reuse the
    // just-freed loopback port — else the equal-port guard would suppress the
    // loopback listener and this test would be vacuous. A clear diagnostic beats a
    // confusing connect timeout.
    assert_ne!(
        external_addr.port(),
        loopback_port,
        "external bind reused the loopback port; the equal-port guard suppressed the \
         co-located listener (re-run — this is a ~0 probability ephemeral collision)"
    );

    let client = reqwest::Client::new();

    // The external listener serves the nest (sanity — same as the sibling test).
    let ext = tokio::time::timeout(
        Duration::from_secs(10),
        client
            .get(format!("http://{external_addr}/internal/router-status"))
            .send(),
    )
    .await
    .expect("external health request completed")
    .expect("external health request succeeded");
    assert_eq!(ext.status(), 200, "external listener serves the nest");

    // THE POINT: the co-located loopback listener is bound on the requested port
    // and serves the SAME nest (the endpoint the MDA bridge dials). Retry briefly —
    // the loopback listener is spawned just after the external one binds and the
    // `ready` signal fires, so it can lag by a few ms.
    let loopback_url = format!("http://127.0.0.1:{loopback_port}/internal/router-status");
    let mut last_err: Option<String> = None;
    let mut served_healthy = false;
    for _ in 0..50 {
        match client.get(&loopback_url).send().await {
            Ok(resp) if resp.status() == 200 => {
                let body: serde_json::Value =
                    resp.json().await.expect("loopback health body is json");
                assert_eq!(
                    body["healthy"].as_bool(),
                    Some(true),
                    "the co-located loopback listener reports healthy"
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
        "the co-located IPC loopback listener (127.0.0.1:{loopback_port}) the MDA bridge \
         dials never served healthy within 5s (last: {last_err:?})"
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
