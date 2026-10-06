//! Service intent file — declares which sidecar services should be running.
//!
//! The nest writes `services.json` to the data directory on startup and
//! updates it when the admin enables or disables features. Platform-specific
//! supervisors (s6, launchd, SCM, test harness) read the file and reconcile
//! running processes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The service intent file format (services.json).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceIntent {
    pub version: u32,
    pub services: ServiceFlags,
}

/// Boolean flags for each sidecar service.
///
/// `#[serde(default)]` so the shape can evolve without breaking old/new
/// `services.json` files: a removed flag (a stray `dns` key from before the
/// nest-side DNS retirement) is ignored on read, and a flag absent from an
/// older file defaults to its `Default` value.
///
/// `pairing` is the admin nest-level pairing-policy knob (per-user
/// multi-homing) and defaults **on** — both via the manual `Default` impl below
/// (fresh `services.json`) and the `default_pairing_on` field default (a file
/// missing the key reads back as enabled — the `Default` value, never silently
/// disabling pairing). `bridge` stays default-off. The retired `algorithm`
/// flag (it gated the sidecar removed 2026-10-01) and the retired `iroh_relay`
/// flag (the relay sidecar's gate until 2026-10-03 — the image now always runs
/// the relay and the nest reads the live connection instead,
/// `discovery_core::relay_sidecar_connected`) are no fields: a `services.json`
/// still carrying either key loads, the key is ignored, and the next write
/// drops it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceFlags {
    pub bridge: bool,
    #[serde(default = "default_pairing_on")]
    pub pairing: bool,
}

fn default_pairing_on() -> bool {
    true
}

impl Default for ServiceFlags {
    fn default() -> Self {
        Self {
            bridge: false,
            pairing: default_pairing_on(),
        }
    }
}

impl Default for ServiceIntent {
    fn default() -> Self {
        Self {
            version: 1,
            services: ServiceFlags::default(),
        }
    }
}

impl ServiceIntent {
    /// Read the intent file from disk.
    pub fn read_from(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let intent: ServiceIntent = serde_json::from_str(&contents)?;
        Ok(intent)
    }

    /// Write the intent file to disk atomically (write-to-temp then rename).
    pub fn write_to(&self, path: &Path) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Ensure a services.json file exists. If missing, create the default
/// (all services disabled). If it already exists, leave it alone.
pub fn ensure_services_json(path: &Path) {
    if path.exists() {
        tracing::info!("Service intent file exists at {}", path.display());
        return;
    }
    let intent = ServiceIntent::default();
    if let Err(e) = intent.write_to(path) {
        tracing::error!("Failed to write services.json to {}: {e}", path.display());
        return;
    }
    tracing::info!("Service intent file created at {}", path.display());
}

/// Resolve the path to services.json from the database path.
pub fn services_json_path(db_path: &str) -> PathBuf {
    let data_dir = Path::new(db_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("/data"));
    data_dir.join("services.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn creates_default_services_json() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("services.json");
        ensure_services_json(&path);
        assert!(path.exists());
        let intent = ServiceIntent::read_from(&path).unwrap();
        assert_eq!(intent.version, 1);
        assert!(!intent.services.bridge);
        // The admin pairing knob defaults ON.
        assert!(intent.services.pairing);
    }

    #[test]
    fn pairing_defaults_on_for_pre_flag_services_json() {
        // A services.json written before the `pairing` flag existed must read
        // back with pairing enabled (default-on), never silently disabled. It
        // also still carries the retired `algorithm` and `iroh_relay` keys,
        // which must load and be ignored.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("services.json");
        std::fs::write(
            &path,
            r#"{"version":1,"services":{"bridge":true,"algorithm":true,"iroh_relay":true}}"#,
        )
        .unwrap();
        let intent = ServiceIntent::read_from(&path).unwrap();
        assert!(intent.services.bridge);
        assert!(intent.services.pairing);
    }

    #[test]
    fn preserves_existing_services_json() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("services.json");
        let mut intent = ServiceIntent::default();
        intent.services.bridge = true;
        intent.write_to(&path).unwrap();

        // ensure should not overwrite
        ensure_services_json(&path);
        let reloaded = ServiceIntent::read_from(&path).unwrap();
        assert!(reloaded.services.bridge);
    }

    #[test]
    fn write_and_read_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("services.json");
        let mut intent = ServiceIntent::default();
        intent.services.bridge = true;
        intent.write_to(&path).unwrap();

        let reloaded = ServiceIntent::read_from(&path).unwrap();
        assert!(reloaded.services.bridge);
    }

    #[test]
    fn update_service_flag() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("services.json");
        let intent = ServiceIntent::default();
        intent.write_to(&path).unwrap();

        // Simulate what the handler does
        let mut loaded = ServiceIntent::read_from(&path).unwrap();
        assert!(!loaded.services.bridge);
        loaded.services.bridge = true;
        loaded.write_to(&path).unwrap();

        let reloaded = ServiceIntent::read_from(&path).unwrap();
        assert!(reloaded.services.bridge);
        assert!(reloaded.services.pairing);
    }
}
