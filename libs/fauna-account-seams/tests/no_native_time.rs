//! The seams crate names no thread, no timer and no monotonic clock —
//! pinned textually, because `cargo check --target wasm32-unknown-unknown`
//! cannot catch it: `std::thread::spawn`, `tokio::time` and
//! `Instant::now()` all type-check on wasm32 and fail only at run time. The
//! plane crate's `tests/no_native_time.rs` is the shape and the reason
//! (`docs/goal/architecture/account-client-lifecycle.md` § The client-side
//! lifecycle → *The trigger fired*, ruling (2): time only through
//! `fauna_sleep::sleep` and the wall clock); this crate sits above the plane
//! and web hosts it the same way, so it keeps the same ban. One token the
//! plane bans is deliberately absent here: `tokio::runtime`, which the native
//! arm of `spawner::TaskSpawner` names — behind `cfg(not(target_arch =
//! "wasm32"))`, the one place a runtime handle is spelled, and the reason
//! the crate exists at all.
//!
//! `std::time::Duration` is a plain value and stays.

use std::path::Path;

/// Each banned token with why — the message a red names.
const BANNED: &[(&str, &str)] = &[
    (
        "std::thread::spawn",
        "a thread — the host's, never the seams'",
    ),
    (
        "std::thread::Builder",
        "a thread — the host's, never the seams'",
    ),
    ("thread::spawn(", "a thread — the host's, never the seams'"),
    (
        "tokio::time",
        "tokio's timer does not build for wasm32 — use `fauna_sleep::sleep`",
    ),
    (
        "tokio::spawn(",
        "a runtime — the host's, through `TaskSpawner`, never the seams'",
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
fn the_seams_crate_names_no_thread_timer_or_monotonic_clock() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    files.sort();
    assert!(
        files.len() >= 5,
        "the scan found only {} files under {}",
        files.len(),
        src.display()
    );
    let mut reds = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read a source file");
        for (line_no, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            for (token, why) in BANNED {
                if code.contains(token) {
                    reds.push(format!(
                        "{}:{}: `{token}` — {why}",
                        file.strip_prefix(&src).unwrap_or(file).display(),
                        line_no + 1
                    ));
                }
            }
        }
    }
    assert!(
        reds.is_empty(),
        "the seams crate must stay wasm-capable:\n{}",
        reds.join("\n")
    );
}
