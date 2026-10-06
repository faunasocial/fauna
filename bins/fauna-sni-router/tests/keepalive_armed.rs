//! Regression lock for the 2026-07-31 example.com `:443` outage.
//!
//! The unit test in `fauna-conn-limit` proves `arm_dead_peer_detection` sets the
//! right socket options. That is necessary but not sufficient: what actually
//! broke — and what can silently break again — is the **accept loop forgetting
//! to call it**, leaving spliced connections that outlive their peers and burn
//! per-IP permits forever. So this drives the real binary and observes the real
//! accepted socket, in the spliced state the leak happens in.
//!
//! Linux-only because it reads the kernel's per-socket timer through `ss`; the
//! deployment target (the nest Docker image) is Linux, and the option-level
//! assertion in `fauna-conn-limit` covers every platform.

#![cfg(target_os = "linux")]

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Generous ceiling for the router process to bind its listener. A green run
/// pays only the real startup time (the poll exits as soon as it connects); the
/// budget exists so a loaded machine fails slowly rather than flakily.
const ROUTER_START_BUDGET: Duration = Duration::from_secs(30);

/// A free localhost port. Bind-then-drop: another process could in principle
/// claim it in the gap, which would surface as a bind failure in the router's
/// output, not as a wrong-socket assertion.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("binding an ephemeral port")
        .local_addr()
        .unwrap()
        .port()
}

/// Kills the router on drop so a failing assertion can't leak the process.
struct RouterProcess(Child);

impl Drop for RouterProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn accept_loop_arms_dead_peer_detection_on_spliced_connections() {
    // A backend that accepts and then holds, so the router's splice stays up
    // for the duration of the observation.
    let backend = TcpListener::bind("127.0.0.1:0").expect("binding fake backend");
    let backend_addr = backend.local_addr().unwrap();
    std::thread::spawn(move || {
        for conn in backend.incoming() {
            // Park the accepted socket; dropping it would tear the splice down.
            std::mem::forget(conn);
        }
    });

    let listen_port = free_port();
    let router = RouterProcess(
        Command::new(env!("CARGO_BIN_EXE_fauna-sni-router"))
            .arg("--listen")
            .arg(format!("127.0.0.1:{listen_port}"))
            .arg("--default")
            .arg(backend_addr.to_string())
            .spawn()
            .expect("spawning fauna-sni-router"),
    );

    // Deadline-poll until the router is listening.
    let deadline = Instant::now() + ROUTER_START_BUDGET;
    let mut client = loop {
        match TcpStream::connect(("127.0.0.1", listen_port)) {
            Ok(s) => break s,
            Err(e) => {
                assert!(
                    Instant::now() < deadline,
                    "router never accepted connections within {ROUTER_START_BUDGET:?}: {e}"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };

    // A minimal well-formed TLS record so the router stops peeking, resolves a
    // backend (no SNI → the default) and enters `copy_bidirectional`. That
    // spliced state is the one connections leaked in.
    client
        .write_all(&[0x16, 0x03, 0x01, 0x00, 0x05, 0x01, 0x02, 0x03, 0x04, 0x05])
        .expect("sending a TLS record header");
    client.flush().unwrap();

    // The router's accepted socket is the established one whose LOCAL port is
    // the listen port. `ss -o` prints each socket's kernel timer; a socket with
    // SO_KEEPALIVE armed and idle carries a `keepalive` timer, and one without
    // it carries none. Poll: the splice is set up asynchronously.
    let deadline = Instant::now() + ROUTER_START_BUDGET;
    let observed = loop {
        let out = Command::new("ss")
            .args([
                "-tino",
                "state",
                "established",
                &format!("( sport = :{listen_port} )"),
            ])
            .output()
            .expect("running `ss` (iproute2) to read the socket's kernel timer");
        let listing = String::from_utf8_lossy(&out.stdout).into_owned();
        // Guard against a vacuous pass: the word must appear on a listing that
        // actually contains our socket, not on an empty one.
        if listing.contains(&format!(":{listen_port}")) && listing.contains("keepalive") {
            break listing; // armed — the accept loop called it
        }
        assert!(
            Instant::now() < deadline,
            "the router's accepted socket on :{listen_port} carries no keepalive timer, so a peer \
             that vanishes without a clean close would hold its per-IP permit forever (the \
             2026-07-31 example.com :443 outage). `ss -tino` said:\n{listing}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    // The countdown the kernel prints is our configured idle period, so this
    // also pins the constant end-to-end rather than just "some keepalive".
    assert!(
        observed.contains("keepalive,1min59sec") || observed.contains("keepalive,2min"),
        "expected the kernel to count down fauna_conn_limit::KEEPALIVE_IDLE (120s), got:\n{observed}"
    );

    drop(client);
    drop(router);
}
