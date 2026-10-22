//! SIGTERM drains live splices instead of cutting them.
//!
//! On `docker stop` the nest's graceful shutdown sends every connected app a WS
//! 1001 and drains, and on a deployed box that close frame leaves through this
//! router's `:443` splice (`transport-connection.md` § Graceful shutdown). The
//! s6 graph stops the nest before the router, but any shutdown that signals
//! both at once (s6-overlay's final sweep, a hand-run `kill`) must not cut a
//! splice whose backend is still saying goodbye: on SIGTERM the router stops
//! accepting, keeps every live splice until its backend closes (bounded by its
//! drain grace), and only then exits. A splice whose backend has closed is over
//! for a draining router even while its client keeps its own half open — the
//! goodbye has been delivered, and waiting on the client would spend the whole
//! drain grace of the container's stop budget (`installers/docker.md`
//! § s6-overlay Services). This drives the real binary.
//!
//! Linux-only: it signals the process with `kill -TERM`; the deployment target
//! (the nest Docker image) is Linux.

#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Generous ceiling for each wait below. A green run pays only the real time
/// (every poll exits as soon as its condition holds); the budget exists so a
/// loaded machine fails slowly rather than flakily.
const BUDGET: Duration = Duration::from_secs(30);

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
fn sigterm_keeps_a_live_splice_until_its_backend_closes() {
    // A backend that accepts one connection, reports it, then waits to be told
    // to send its goodbye and close — the nest's 1001-then-exit, in miniature.
    let backend = TcpListener::bind("127.0.0.1:0").expect("binding fake backend");
    let backend_addr = backend.local_addr().unwrap();
    let (accepted_tx, accepted_rx) = mpsc::channel::<()>();
    let (goodbye_tx, goodbye_rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let (mut conn, _) = backend.accept().expect("backend accept");
        // Read what the client sent, as the nest reads every request: closing
        // with unread bytes would send a RST, which ends a splice on its own and
        // hides whether the router waits on the client's half after a clean FIN.
        let mut record = [0u8; 10];
        conn.read_exact(&mut record)
            .expect("backend reading the client's record");
        accepted_tx.send(()).unwrap();
        goodbye_rx.recv().unwrap();
        conn.write_all(b"goodbye").unwrap();
        // Dropping `conn` closes the backend side: the splice's natural end.
    });

    let listen_port = free_port();
    let mut router = RouterProcess(
        Command::new(env!("CARGO_BIN_EXE_fauna-sni-router"))
            .arg("--listen")
            .arg(format!("127.0.0.1:{listen_port}"))
            .arg("--default")
            .arg(backend_addr.to_string())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawning fauna-sni-router"),
    );
    // Drained for the child's whole life (its log lines go to stdout); read at
    // the end to tell which way the drain ended.
    let mut stdout = router.0.stdout.take().unwrap();
    let log = std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        out
    });

    let deadline = Instant::now() + BUDGET;
    let mut client = loop {
        match TcpStream::connect(("127.0.0.1", listen_port)) {
            Ok(s) => break s,
            Err(e) => {
                assert!(
                    Instant::now() < deadline,
                    "router never accepted connections within {BUDGET:?}: {e}"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    // A minimal TLS record so the router stops peeking, resolves the default
    // backend and enters its splice.
    client
        .write_all(&[0x16, 0x03, 0x01, 0x00, 0x05, 0x01, 0x02, 0x03, 0x04, 0x05])
        .expect("sending a TLS record header");
    accepted_rx
        .recv_timeout(BUDGET)
        .expect("the router never spliced the connection to the backend");

    let status = Command::new("kill")
        .args(["-TERM", &router.0.id().to_string()])
        .status()
        .expect("running kill");
    assert!(status.success(), "kill -TERM failed: {status}");

    // The listener closes: a new connection is refused, while the router lives.
    let deadline = Instant::now() + BUDGET;
    loop {
        if let Some(exit) = router.0.try_wait().unwrap() {
            panic!(
                "the router exited ({exit}) on SIGTERM with a splice still live — the backend's \
                 goodbye (the nest's WS 1001) would never reach the client"
            );
        }
        if TcpStream::connect(("127.0.0.1", listen_port)).is_err() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the router still accepts new connections {BUDGET:?} after SIGTERM"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // The backend says goodbye and closes; the client receives every byte of it
    // and then keeps its own half of the connection open, as an app that has
    // not yet answered the close does.
    goodbye_tx.send(()).unwrap();
    client.set_read_timeout(Some(BUDGET)).unwrap();
    let mut received = Vec::new();
    client
        .read_to_end(&mut received)
        .expect("reading the backend's goodbye through the draining router");
    assert_eq!(
        received, b"goodbye",
        "the draining router must carry the backend's last bytes, then its close"
    );

    // With no splice left, the router exits cleanly on its own.
    let deadline = Instant::now() + BUDGET;
    let exit = loop {
        if let Some(exit) = router.0.try_wait().unwrap() {
            break exit;
        }
        assert!(
            Instant::now() < deadline,
            "the router did not exit within {BUDGET:?} of its last splice ending"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(exit.success(), "the drained router must exit 0, got {exit}");
    let log = log.join().unwrap();
    assert!(
        log.contains("every splice drained; exiting"),
        "the router must count the splice drained once its backend closed, not wait out its \
         drain grace on the client's open half; its log:\n{log}"
    );
    drop(client);
}
