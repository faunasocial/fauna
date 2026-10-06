//! `fauna-sync-agent` binary — the per-user sync+backup agent on every platform:
//! linux, macOS, and the shipped windows `fauna-sync-agent.exe`.
//!
//! A thin shell over [`fauna_sync_agent::run_main`]; all logic lives in the lib.

// GUI-subsystem for shipped (release/dist, debug_assertions-off) Windows builds so
// a per-user logon launch never flashes a console window; debug builds keep the
// console for `--foreground` log viewing (harmless on unix, where the agent runs
// in the user's logon session). The attribute must sit on a binary crate root,
// which is why it is here and not in the lib.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    fauna_sync_agent::run_main()
}
