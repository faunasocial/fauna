//! Windows path-resolution shell for the shared MDA supervisor.
//!
//! The cross-platform reconcile logic — spawn/monitor the Go `fauna-mail-bridge`
//! MDA child, gate it on the nest's `imap-enabled`/`caldav-enabled` flags, pin the
//! CalDAV `operator-hatch.toml`, restart on an admin port change — lives in the
//! shared [`fauna_mda_supervisor`] crate (lifted there `2026-06-23`; design tracked
//! internally: the supervision *logic* is shared, the *shell* is per-OS). This module is the
//! Windows shell: it resolves the `%PROGRAMDATA%\Fauna\{bridge,nest}` data dirs +
//! the `.exe` staging path and constructs the [`MdaSupervisor`].

// Re-export so the rest of the Windows service refers to `crate::mda_supervisor::*`.
pub use fauna_mda_supervisor::MdaSupervisor;

use std::path::{Path, PathBuf};

/// Resolve the bridge and nest data directories under a `%PROGRAMDATA%` base.
///
/// Returns `(bridge_data_dir, nest_data_dir)`. The nest dir must match
/// `fauna-nest-service`'s `config::default_data_dir` (`<base>\Fauna\nest`) so the
/// enable flag files line up; the bridge dir is `<base>\Fauna\bridge`.
pub fn resolve_dirs(programdata: &Path) -> (PathBuf, PathBuf) {
    let fauna = programdata.join("Fauna");
    (fauna.join("bridge"), fauna.join("nest"))
}

/// Construct an [`MdaSupervisor`] for production use on Windows: dirs under
/// `%PROGRAMDATA%`, the MDA exe staged beside this service exe, the CalDAV
/// interface the shared [`fauna_mda_supervisor::CALDAV_LISTEN`] constant.
pub fn resolve(nest_endpoint: String) -> MdaSupervisor {
    let base = fauna_ipc::device::programdata_base();
    let (bridge_data_dir, nest_data_dir) = resolve_dirs(&base);
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    MdaSupervisor {
        mda_exe: exe_dir.join("fauna-mail-bridge.exe"),
        bridge_data_dir,
        nest_data_dir,
        nest_endpoint,
        caldav_listen: fauna_mda_supervisor::CALDAV_LISTEN.to_string(),
        log_level: "info".to_string(),
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    #[test]
    fn resolve_pins_the_shared_caldav_listen_constant() {
        let sup = resolve("https://127.0.0.1:443".to_string());
        assert_eq!(sup.caldav_listen, fauna_mda_supervisor::CALDAV_LISTEN);
        assert_eq!(sup.nest_endpoint, "https://127.0.0.1:443");
    }
}

// Windows-only: `resolve_dirs` builds `%PROGRAMDATA%\Fauna\…` and the assertion
// below pins the literal Windows `\` separators, so it is only valid on Windows
// (it would fail on a non-Windows dev machine / CI runner, where `PathBuf::join`
// uses `/`). The function itself is only ever called on Windows.
#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn resolve_dirs_matches_nest_and_bridge_conventions() {
        let base = PathBuf::from(r"C:\ProgramData");
        let (bridge, nest) = resolve_dirs(&base);
        assert_eq!(bridge, PathBuf::from(r"C:\ProgramData\Fauna\bridge"));
        assert_eq!(nest, PathBuf::from(r"C:\ProgramData\Fauna\nest"));
    }
}
