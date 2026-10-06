//! Windows link-time main-thread stack reserve for the `fauna-tui` binary.
//!
//! The main thread's stack reserve is baked into the PE header at link time and
//! defaults to **1 MiB** on Windows, against **8 MiB** on Linux/macOS. tui runs
//! on all three (`architecture/apps/tui.md`), so it is written and exercised
//! against the unix figure — and a *debug* build's async state machines are far
//! fatter than a release build's, because rustc gives every await point across
//! every match arm its own slot instead of overlapping them.
//!
//! Measured on Windows-arm64 (2026-07-31), one `nav` patch to the Settings page:
//!
//! | build   | peak main-thread stack |
//! |---------|------------------------|
//! | debug   | 837 KiB                |
//! | release |  26 KiB                |
//!
//! The debug figure overflowed the 1 MiB reserve and killed the app with
//! `STATUS_STACK_OVERFLOW` (0xC00000FD) before it could paint, so **every**
//! Settings sub-page was unreachable on windows while every other page had
//! room to spare. Release was never at risk — this is a debug/e2e-build crash,
//! not a shipped-artifact one.
//!
//! The bloat is addressed where it is produced: `app::PageOp::run` boxes each
//! nested `op.run()` future, which cut that peak to 630 KiB. This reserve
//! removes the *platform cliff* underneath it — without it, Windows debug
//! builds run at ~1.6x headroom where unix has ~13x, so the next fat op
//! re-breaks exactly one platform, with a crash three frames removed from its
//! symptom. (That is precisely how this one presented: as an unrelated sync
//! test failing on a dead automation bridge.)
//!
//! Reserve is address space, not committed memory: untouched pages cost
//! nothing on a 64-bit target, so this is free and harmless in release.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // The commit this binary is built from, for the Status page's
    // `status-build-sha` (`ui/status.md` § State & data shape, the build leg) —
    // the shared derivation, so the terminal app, the nest and every later
    // native app agree on what "the build you are running" means.
    fauna_build_commit::emit();
    let is_windows_msvc = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if is_windows_msvc {
        // Matches the Linux/macOS default so the three platforms tui ships on
        // agree on how much stack the app may use.
        println!("cargo:rustc-link-arg-bins=/STACK:8388608");
    }
}
