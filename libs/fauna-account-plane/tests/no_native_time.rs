//! The plane crate names no thread, no timer and no monotonic clock —
//! pinned textually, because `cargo check --target wasm32-unknown-unknown`
//! cannot catch it: `std::thread::spawn`, `tokio::time` and
//! `Instant::now()` all type-check on wasm32 and fail only at run time
//! (`docs/goal/architecture/account-data-plane.md` § The client-side
//! lifecycle → *The trigger fired*, ruling (2): the driver reaches time only
//! through `fauna_sleep::sleep` and the wall clock,
//! `fauna_core::data::Timestamp`). The shape is the workspace's wall-clock gate's
//! — a line scan with comments skipped and no `#[cfg(test)]` exemption — as a
//! Rust test inside the crate it guards, so it runs with the crate's own
//! suite on every machine.
//!
//! `std::thread::current()` (a test's thread id) is not a hazard and not
//! banned; a spawn or a builder is. `std::time::Duration` is a plain value
//! and stays.

use std::path::Path;

/// Each banned token with why — the message a red names.
const BANNED: &[(&str, &str)] = &[
    (
        "std::thread::spawn",
        "a thread — the host's, never the driver's",
    ),
    (
        "std::thread::Builder",
        "a thread — the host's, never the driver's",
    ),
    (
        "thread::spawn(",
        "a thread — the host's, never the driver's",
    ),
    (
        "tokio::time",
        "tokio's timer does not build for wasm32 — use `fauna_sleep::sleep`",
    ),
    (
        "tokio::runtime",
        "a runtime — the host's, never the driver's",
    ),
    (
        "std::time::Instant",
        "`Instant::now()` panics on wasm32 — state durations against `Timestamp`",
    ),
    (
        "time::Instant",
        "`Instant::now()` panics on wasm32 — state durations against `Timestamp`",
    ),
    (
        "Instant::now(",
        "`Instant::now()` panics on wasm32 — state durations against `Timestamp`",
    ),
    (
        "SystemTime",
        "`SystemTime::now()` panics on wasm32 — read `fauna_core::data::Timestamp`",
    ),
];

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read the src tree") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn the_plane_crate_names_no_thread_timer_or_monotonic_clock() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    files.sort();
    assert!(
        files.len() > 20,
        "the scan found only {} files under {}",
        files.len(),
        src.display()
    );

    let mut findings = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read a source file");
        for (n, line) in text.lines().enumerate() {
            let code = line.trim_start();
            // Prose about the hazard is not the hazard.
            if code.starts_with("//") {
                continue;
            }
            for (token, why) in BANNED {
                if code.contains(token) {
                    findings.push(format!(
                        "{}:{}: `{token}` — {why}\n    {}",
                        file.strip_prefix(&src).unwrap_or(file).display(),
                        n + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        findings.is_empty(),
        "the plane crate compiles for wasm32 and must reach time only through \
         `fauna_sleep::sleep` and `fauna_core::data::Timestamp`; found:\n{}",
        findings.join("\n")
    );
}
