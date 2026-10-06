//! Helper to load the shared device configuration.
//!
//! The device config lives at `%PROGRAMDATA%\Fauna\device.toml` and is
//! shared across all Fauna services. It contains the device ID and
//! nest connection details.

use std::path::Path;

use fauna_ipc::device::DeviceConfig;

/// Load DeviceConfig from the default path (`%PROGRAMDATA%\Fauna\device.toml`).
pub fn load_device_config() -> Option<DeviceConfig> {
    match DeviceConfig::load() {
        Ok(cfg) => {
            tracing::info!(device_id = %cfg.device_id, "device config loaded");
            Some(cfg)
        }
        Err(e) => {
            tracing::warn!("failed to load device config: {e}");
            None
        }
    }
}

/// Load DeviceConfig from a custom path.
pub fn load_device_config_from(path: &Path) -> Option<DeviceConfig> {
    match DeviceConfig::load_from(path) {
        Ok(cfg) => {
            tracing::info!(device_id = %cfg.device_id, path = %path.display(), "device config loaded");
            Some(cfg)
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), "failed to load device config: {e}");
            None
        }
    }
}
