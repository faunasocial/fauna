//! Configuration helpers for the FaunaNest Windows Service.
//!
//! Manages the nest data directory (`%PROGRAMDATA%\Fauna\nest`) and the
//! shared device configuration (`%PROGRAMDATA%\Fauna\device.toml`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fauna_ipc::device::DeviceConfig;
use fauna_protocol::node_policy::CANONICAL_INTERNAL_LOOPBACK_PORT;

/// Default nest data directory: `%PROGRAMDATA%\Fauna\nest`.
pub fn default_data_dir() -> PathBuf {
    fauna_ipc::device::programdata_base()
        .join("Fauna")
        .join("nest")
}

// NB: the shared device-config path is derived inline in `run_service_loop` as
// `<data_dir>.parent()/device.toml` so it honours a `--data-dir` override; a
// hard-coded `%PROGRAMDATA%\Fauna\device.toml` helper would silently ignore that
// override, so none exists here.

/// Load the shared device config, or create it with sensible defaults if it
/// does not yet exist.
///
/// `nest_port` is the FIXED internal-loopback port the co-located bridge + app
/// dial ([`CANONICAL_INTERNAL_LOOPBACK_PORT`] = 3000) — it never moves when the
/// admin changes the external serving_port, so a port change can't strand same-box
/// clients (nest/common.md § Same-box reach). An existing config is loaded as
/// written — the generation below is the only place its `nest_port` is chosen.
///
/// Generated defaults:
/// - `device_id`: 16 random hex bytes
/// - `nest_port`: [`CANONICAL_INTERNAL_LOOPBACK_PORT`] (3000)
/// - `auth_token`: 32 random hex bytes
pub fn load_or_init_device_config(path: &Path) -> Result<DeviceConfig> {
    if path.exists() {
        let cfg = DeviceConfig::load_from(path)
            .with_context(|| format!("loading device config from {}", path.display()))?;
        tracing::info!(device_id = %cfg.device_id, "loaded existing device config");
        return Ok(cfg);
    }

    tracing::info!(path = %path.display(), "device config not found, generating defaults");

    // Generate random device_id (16 bytes = 32 hex chars)
    let mut device_id_bytes = [0u8; 16];
    getrandom::fill(&mut device_id_bytes).expect("getrandom failed");
    let device_id = hex::encode(device_id_bytes);

    // Generate random auth_token (32 bytes = 64 hex chars)
    let mut token_bytes = [0u8; 32];
    getrandom::fill(&mut token_bytes).expect("getrandom failed");
    let auth_token = hex::encode(token_bytes);

    let cfg = DeviceConfig {
        device_id,
        // Same-box IPC dials the nest's FIXED internal-loopback listener
        // (127.0.0.1:CANONICAL_INTERNAL_LOOPBACK_PORT); the nest also binds it
        // alongside its external serving_port listener (nest/common.md
        // § Same-box reach).
        nest_url: "https://127.0.0.1".to_string(),
        nest_port: CANONICAL_INTERNAL_LOOPBACK_PORT,
        auth_token: Some(auth_token),
    };

    cfg.save_to(path)
        .with_context(|| format!("saving device config to {}", path.display()))?;
    tracing::info!(device_id = %cfg.device_id, "generated new device config");

    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_data_dir_contains_nest() {
        let dir = default_data_dir();
        assert!(dir.to_string_lossy().contains("nest"));
    }

    #[test]
    fn load_or_init_creates_file() {
        let dir = std::env::temp_dir().join("fauna-nest-svc-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("device.toml");

        let cfg = load_or_init_device_config(&path).unwrap();
        assert!(!cfg.device_id.is_empty());
        // The generated default points same-box IPC at the nest's FIXED
        // internal-loopback listener (127.0.0.1:CANONICAL_INTERNAL_LOOPBACK_PORT =
        // 3000), which survives an admin serving_port change (nest/common.md
        // § Same-box reach).
        assert_eq!(cfg.nest_port, 3000);
        assert_eq!(cfg.nest_url, "https://127.0.0.1");
        assert!(cfg.auth_token.is_some());
        assert!(path.exists());

        // Second call loads existing config
        let cfg2 = load_or_init_device_config(&path).unwrap();
        assert_eq!(cfg.device_id, cfg2.device_id);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
