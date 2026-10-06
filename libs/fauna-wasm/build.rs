//! Link this chunk with a 16 MiB wasm stack (the linker's default is 1 MiB).
//!
//! This chunk hosts the account runtime (`src/account_runtime.rs` →
//! `fauna_account_plane::web_host`), and the driver's deepest chain — the
//! serve loop polling a pass that runs a first-need generation mint, whose
//! escrow wrap is an X-Wing seal — runs on the browser's one wasm stack, where
//! the native host gets a store thread of its own. Measured 2026-09-29: the
//! reception-key round-trip proof (`account_runtime::tests`) traps with
//! `RuntimeError: index out of bounds` (the stack pointer run off linear
//! memory) at 1 MiB in a debug build and passes at 16 MiB; the minimum
//! between was not bisected. A trap is not a recoverable error on web — it
//! leaves the instance's stack pointer unrestored — so the margin is sized
//! for the unoptimized build, and a release build keeps it as headroom.
//! Scoped here, not in a shared RUSTFLAGS, so no other chunk and no other
//! crate's cache changes (`account-client-lifecycle.md` § Implementation
//! status today, the web host).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        println!("cargo:rustc-link-arg=-zstack-size=16777216");
    }
}
