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
    pub github_token: Option<String>,
}

/// Result of an update check or apply operation.
#[derive(Debug, Clone)]
pub enum UpdateStatus {
    UpToDate,
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

pub use updater::{apply, check, spawn_update_loop};
