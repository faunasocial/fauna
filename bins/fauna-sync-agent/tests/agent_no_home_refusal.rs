//! The agent **refuses to start without an absolute home** — the real binary,
//! a real exit status, a real stderr
//! (`docs/goal/architecture/account-data-plane.md` § The account store → Placement).
//!
//! The per-user state base (`config.toml`, per-set state DBs, logs, the account
//! store, the pin store) used to fall back to world-writable `/tmp` when `HOME`
//! was missing, empty or relative, where another local account could pre-create
//! it. The agent now refuses: it exits non-zero with the reason on stderr and
//! creates nothing under the old fallback.
//!
//! The discriminators are the exit status (a build that still derives `/tmp`
//! would serve and never exit on its own — the wait would time out) and the
//! absence of the old fallback directories.
//!
//! A relative `HOME` is the case that reaches the refusal on EVERY unix (macOS
//! `dirs::home_dir` resolves an unset `HOME` through the passwd database, so only
//! a relative one gets there); linux additionally covers the unset and empty
//! forms.

#![cfg(unix)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Far above a refusal, which is immediate; a green run never approaches it.
const EXIT_BUDGET: Duration = Duration::from_secs(30);

fn home_values() -> Vec<Option<&'static str>> {
    let mut v = vec![Some("relative-home")];
    if !cfg!(target_os = "macos") {
        v.push(None);
        v.push(Some(""));
    }
    v
}

#[test]
fn the_agent_binary_refuses_a_missing_empty_or_relative_home_and_creates_nothing_in_tmp() {
    // The old fallback bases. If a stale one already exists (another run, another
    // user's leftovers) the "creates nothing" half cannot be attributed, so only
    // the exit/stderr halves assert then.
    let old_fallbacks = [
        std::path::Path::new("/tmp/.config/fauna"),
        std::path::Path::new("/tmp/Library/Application Support/Fauna"),
    ];
    let preexisting: Vec<bool> = old_fallbacks.iter().map(|p| p.exists()).collect();

    for home in home_values() {
        let cwd = tempfile::tempdir().unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fauna-sync-agent"));
        cmd.env_clear()
            .env("RUST_LOG", "info")
            .current_dir(cwd.path())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(h) = home {
            cmd.env("HOME", h);
        }
        let mut child = cmd.spawn().expect("spawn fauna-sync-agent");

        let deadline = Instant::now() + EXIT_BUDGET;
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("agent with HOME={home:?} kept running instead of refusing");
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let mut stderr = String::new();
        std::io::Read::read_to_string(child.stderr.as_mut().unwrap(), &mut stderr).unwrap();

        assert!(
            !status.success(),
            "HOME={home:?}: the agent must exit non-zero, got {status:?}"
        );
        assert!(
            stderr.contains("refusing to place per-user Fauna state"),
            "HOME={home:?}: the refusal reason must be logged, stderr was: {stderr}"
        );
        assert_eq!(
            std::fs::read_dir(cwd.path()).unwrap().count(),
            0,
            "HOME={home:?}: the refused agent must create nothing in its cwd"
        );
    }

    for (path, existed) in old_fallbacks.iter().zip(preexisting) {
        if !existed {
            assert!(
                !path.exists(),
                "the refused agent created the old fallback {}",
                path.display()
            );
        }
    }
}
