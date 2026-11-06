//! `fauna-tui` — the binary. The app is the `fauna_tui` library; this crate
//! only expands the build-commit stamp, the one input that changes at every
//! commit, so that only this unit recompiles when `HEAD` moves (`lib.rs`'s
//! crate doc; `docs/goal/ui/status.md` § State & data shape, the build leg).

fn main() -> std::io::Result<()> {
    fauna_tui::start(fauna_build_commit::commit!())
}
