//! Watches `services.json` and reconciles systemd user services.
//!
//! The nest writes `services.json` to the data directory on startup.
//! This module watches that file (via inotify) and starts/stops
//! systemd user services to match the declared intent — mirroring
//! macOS SyncDaemonManager and the Docker s6 service watcher.

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// The service intent file format (mirrors `bins/fauna-nest/src/services.rs`).
#[derive(Debug, Deserialize)]
struct ServiceIntent {
    #[allow(dead_code)]
    version: u32,
    services: ServiceFlags,
}

/// Boolean flags for each sidecar service.
///
/// `#[serde(default)]` mirrors the nest producer (`bins/fauna-nest/src/services.rs`):
/// a key this watcher does not reconcile (e.g. `algorithm`, whose sidecar was
/// removed) is ignored, and a flag absent from `services.json` defaults to
/// `false`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
struct ServiceFlags {
    bridge: bool,
}

/// Read and parse services.json, returning None on any error.
fn read_services(path: &Path) -> Option<ServiceIntent> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Start or stop a systemd user service.
fn toggle_service(name: &str, enable: bool) {
    let action = if enable { "start" } else { "stop" };
    let unit = format!("{name}.service");
    let status = std::process::Command::new("systemctl")
        .args(["--user", action, &unit])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => {
            tracing::info!("systemctl --user {action} {unit}: ok");
        }
        Ok(s) => {
            tracing::error!(
                "systemctl --user {action} {unit}: exit {}",
                s.code().unwrap_or(-1)
            );
        }
        Err(e) => {
            tracing::error!("systemctl --user {action} {unit}: {e}");
        }
    }
}

/// Compare current and previous flags, toggling changed services.
fn reconcile(current: &ServiceFlags, previous: &ServiceFlags) {
    if current.bridge != previous.bridge {
        toggle_service("fauna-bridge", current.bridge);
    }
}

/// Spawn a background thread that watches `data_dir/services.json` and
/// reconciles systemd user services when the file changes.
///
/// Returns the join handle. The thread runs until the process exits.
pub fn start(data_dir: PathBuf) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let services_path = data_dir.join("services.json");
        let mut prev_flags = ServiceFlags::default();

        // Initial read — reconcile if the file already exists.
        if let Some(intent) = read_services(&services_path) {
            tracing::info!("Initial read: bridge={}", intent.services.bridge);
            reconcile(&intent.services, &prev_flags);
            prev_flags = intent.services;
        } else {
            tracing::info!(
                "No services.json at {}, waiting for creation",
                services_path.display()
            );
        }

        // Watch the parent directory — services.json may not exist yet,
        // and atomic writes (rename) create new inodes.
        let watch_dir = data_dir.as_path();
        let (tx, rx) = mpsc::channel();
        let mut watcher: RecommendedWatcher =
            match notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
                let _ = tx.send(res);
            }) {
                Ok(w) => w,
                Err(e) => {
                    tracing::error!("Failed to create file watcher: {e}");
                    return;
                }
            };

        if let Err(e) = watcher.watch(watch_dir, RecursiveMode::NonRecursive) {
            tracing::error!("Cannot watch {}: {e}", watch_dir.display());
            // Fall through to polling-only mode via the timeout loop.
        }

        loop {
            match rx.recv_timeout(Duration::from_secs(30)) {
                Ok(Ok(event)) => {
                    // Only react to events on services.json.
                    let dominated = event
                        .paths
                        .iter()
                        .any(|p| p.file_name().map(|n| n == "services.json").unwrap_or(false));
                    if !dominated {
                        continue;
                    }
                    match event.kind {
                        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                            if let Some(intent) = read_services(&services_path)
                                && intent.services != prev_flags
                            {
                                tracing::info!(
                                    "Change detected: bridge={}",
                                    intent.services.bridge,
                                );
                                reconcile(&intent.services, &prev_flags);
                                prev_flags = intent.services;
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!("File watcher error: {e}");
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Periodic fallback re-read (handles missed events, atomic renames).
                    if let Some(intent) = read_services(&services_path)
                        && intent.services != prev_flags
                    {
                        tracing::info!("Periodic re-read: bridge={}", intent.services.bridge);
                        reconcile(&intent.services, &prev_flags);
                        prev_flags = intent.services;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    tracing::warn!("Watcher channel disconnected, exiting");
                    break;
                }
            }
        }
    })
}

/// Resolve the data directory containing services.json.
///
/// Checks the system-level path first (for root-installed nests),
/// then falls back to the XDG data directory (for user-level installs).
pub fn resolve_data_dir() -> Option<PathBuf> {
    let system_path = PathBuf::from("/var/lib/fauna");
    if system_path.join("services.json").exists() {
        return Some(system_path);
    }

    // XDG_DATA_HOME or ~/.local/share
    let xdg_data = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/.local/share")
    });
    let user_path = PathBuf::from(xdg_data).join("fauna");
    if user_path.exists() {
        return Some(user_path);
    }

    // Neither exists yet — return the system path and let the watcher
    // wait for the directory/file to be created.
    if system_path.exists() {
        return Some(system_path);
    }

    None
}
