//! Self-update library for Fauna binaries.
//!
//! Checks GitHub Releases for new versions, downloads artifacts,
//! verifies Ed25519 signatures + SHA-256 checksums, and performs
//! platform-specific binary replacement.

use std::path::PathBuf;
use std::time::Duration;

pub mod apply;
pub mod github;
pub mod updater;
pub mod verify;

/// Ed25519 public key for release artifact verification.
/// Replace with the actual key bytes before the first signed release.
pub const RELEASE_PUBLIC_KEY: [u8; 32] = [0u8; 32];

/// Configuration for the update system.
#[derive(Debug, Clone)]
pub struct UpdateConfig {
    pub github_repo: &'static str,
    pub current_version: &'static str,
    pub artifact_prefix: &'static str,
    pub check_interval: Duration,
    pub auto_apply: bool,
    pub install_dir: PathBuf,
}

/// Result of an update check or apply operation.
#[derive(Debug, Clone)]
pub enum UpdateStatus {
    /// Nothing newer than the running version is out. `newest` is the newest
    /// published release the check saw (`None` when the repository has
    /// published none) — kept so a caller can report the newest release
    /// whether or not this binary runs it ([`UpdateStatus::newest_release`]).
    UpToDate {
        newest: Option<String>,
    },
    Available {
        version: String,
        release_url: String,
    },
    Downloaded {
        version: String,
        path: PathBuf,
    },
    Applied {
        version: String,
    },
}

impl UpdateStatus {
    /// The newest published release this status knows of (`0.2.0`), whether
    /// or not the running binary is at it — never the running binary's own
    /// version, so reporting it says nothing about whether this binary is
    /// behind (`installers/README.md` § Knowing a newer version is out →
    /// *The nest's newest-release field*).
    pub fn newest_release(&self) -> Option<&str> {
        match self {
            UpdateStatus::UpToDate { newest } => newest.as_deref(),
            UpdateStatus::Available { version, .. }
            | UpdateStatus::Downloaded { version, .. }
            | UpdateStatus::Applied { version } => Some(version),
        }
    }
}

pub use updater::{apply, check, spawn_update_loop};

#[cfg(test)]
mod tests {
    use super::UpdateStatus;

    #[test]
    fn the_newest_release_is_reported_whether_or_not_this_binary_runs_it() {
        let behind = UpdateStatus::Available {
            version: "0.2.0".into(),
            release_url: "https://example.com/v0.2.0".into(),
        };
        let at_it = UpdateStatus::UpToDate {
            newest: Some("0.2.0".into()),
        };
        // Behind or at it, the answer is the release's — the same either way.
        assert_eq!(behind.newest_release(), Some("0.2.0"));
        assert_eq!(at_it.newest_release(), Some("0.2.0"));
        // No release published: nothing to report.
        assert_eq!(
            UpdateStatus::UpToDate { newest: None }.newest_release(),
            None
        );
    }
}
