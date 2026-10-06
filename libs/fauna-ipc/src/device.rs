//! Shared device configuration used by all Fauna services.
//!
//! Each service reads `DeviceConfig` to discover its device identity
//! and the Nest server it should connect to.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Device-level configuration shared across all Fauna services.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    pub device_id: String,
    #[serde(default = "default_nest_url")]
    pub nest_url: String,
    #[serde(default = "default_nest_port")]
    pub nest_port: u16,
    #[serde(default)]
    pub auth_token: Option<String>,
}

fn default_nest_url() -> String {
    // Same-box IPC scheme + host; the port is `default_nest_port()`. The local
    // nest serves HTTPS from its always-live self-signed floor on a FIXED
    // 127.0.0.1 internal-loopback listener it binds for co-located IPC (alongside
    // its external serving_port listener), so the co-located bridge + app dial a
    // port that never moves when the admin changes serving_port (nest/common.md
    // § Same-box reach).
    "https://127.0.0.1".to_string()
}

fn default_nest_port() -> u16 {
    // The FIXED internal-loopback port for co-located IPC — kept in sync with
    // `fauna_protocol::node_policy::CANONICAL_INTERNAL_LOOPBACK_PORT` (= 3000).
    // Hardcoded rather than referencing that constant so this leaf IPC crate
    // need not depend on the WS-RPC transport crate (`fauna-protocol`); it is
    // also consumed by the lean Windows shell extension + sync/bridge services,
    // which should not compile the transport stack for one port number. The
    // `fauna-nest-service` WRITER references the constant directly (it already
    // depends on `fauna-protocol`); this serde default is the matching fallback
    // when `nest_port` is absent from device.toml (the writer always sets it).
    3000
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            device_id: String::new(),
            nest_url: default_nest_url(),
            nest_port: default_nest_port(),
            auth_token: None,
        }
    }
}

/// The `%PROGRAMDATA%` base directory (falls back to the literal
/// `C:\ProgramData` when the env var is unset) — the single home for every
/// Fauna Windows service that resolves a machine-wide, not per-user, data
/// root. Nest
/// and Bridge run as machine service accounts (`NT SERVICE\Fauna*`) that
/// cannot read a per-user `%LOCALAPPDATA%`, so this is the machine-wide
/// counterpart to `fauna_sync_engine::root::platform_state_base()`'s
/// per-user `%LOCALAPPDATA%` root — a deliberately separate home, not
/// merged with it: the two roots serve different account models (machine
/// service vs. per-user agent) and must not resolve through one another.
///
/// Deliberately NOT `#[cfg(windows)]`: the body is portable (an env-var read
/// with a string fallback, no Windows syscall), and the windows-service
/// crates that call it (`fauna-nest-service`, `fauna-bridge-service`) are
/// plain workspace members compiled on every platform by the cheap/heavy
/// merge gates — gating this one shared definition, while every caller
/// stayed ungated, is a hard compile error off Windows the moment a new
/// caller lands. Its answer is only
/// ever *meaningful* on Windows; that is a semantic fact for the caller to
/// mind, not a reason to make the function itself uncompileable elsewhere.
pub fn programdata_base() -> PathBuf {
    let base = std::env::var("PROGRAMDATA").unwrap_or_else(|_| r"C:\ProgramData".to_string());
    PathBuf::from(base)
}

impl DeviceConfig {
    /// Returns the full base URL for Nest API calls, e.g. `https://127.0.0.1:443`.
    pub fn nest_base_url(&self) -> String {
        format!("{}:{}", self.nest_url, self.nest_port)
    }

    /// Default config file path: `%PROGRAMDATA%\Fauna\device.toml` on Windows,
    /// `~/.config/fauna/device.toml` on other platforms.
    ///
    /// `device.toml` is **Nest↔Bridge** machine-local IPC: `fauna-nest-service`
    /// is the writer (it generates the file under `%PROGRAMDATA%\Fauna\`), and
    /// `fauna-bridge-service` is the reader. The **per-user sync agent does NOT read `device.toml`** — it is
    /// provisioned live by the desktop app (it sources its nest URL from the live
    /// provision), so it never resolves this path.
    ///
    /// On Windows this is a machine-wide location, NOT a per-user one: Nest and
    /// Bridge run as machine service accounts (`NT SERVICE\Fauna*`) that cannot
    /// read a per-user `%LOCALAPPDATA%`, so both must resolve the same path. See
    /// `apps/fauna-windows/installer/README.md` (*Data directories*) and
    /// `fauna-nest-service/src/config.rs`.
    pub fn default_path() -> PathBuf {
        #[cfg(windows)]
        {
            programdata_base().join("Fauna").join("device.toml")
        }
        #[cfg(not(windows))]
        {
            let base = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                format!("{home}/.config")
            });
            PathBuf::from(base).join("fauna").join("device.toml")
        }
    }

    /// Load from the default path.
    pub fn load() -> Result<Self, DeviceConfigError> {
        Self::load_from(&Self::default_path())
    }

    /// Load from a specific path.
    pub fn load_from(path: &std::path::Path) -> Result<Self, DeviceConfigError> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| DeviceConfigError::Io(path.to_path_buf(), e))?;
        let config: DeviceConfig = toml::from_str(&contents)
            .map_err(|e| DeviceConfigError::Parse(path.to_path_buf(), Box::new(e)))?;
        Ok(config)
    }

    /// Save to the default path (creates parent directories).
    pub fn save(&self) -> Result<(), DeviceConfigError> {
        self.save_to(&Self::default_path())
    }

    /// Save to a specific path (creates parent directories).
    pub fn save_to(&self, path: &std::path::Path) -> Result<(), DeviceConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| DeviceConfigError::Io(parent.to_path_buf(), e))?;
        }
        let contents =
            toml::to_string_pretty(self).map_err(|e| DeviceConfigError::Serialize(Box::new(e)))?;
        std::fs::write(path, contents).map_err(|e| DeviceConfigError::Io(path.to_path_buf(), e))?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DeviceConfigError {
    #[error("I/O error at {0}: {1}")]
    Io(PathBuf, std::io::Error),
    // The `toml` error types are large (>128 bytes); box them so the enum — and
    // every `Result<_, DeviceConfigError>` returned below — stays small
    // (clippy::result_large_err).
    #[error("failed to parse config at {0}: {1}")]
    Parse(PathBuf, Box<toml::de::Error>),
    #[error("failed to serialize config: {0}")]
    Serialize(Box<toml::ser::Error>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let cfg = DeviceConfig::default();
        assert_eq!(cfg.nest_url, "https://127.0.0.1");
        // The same-box co-located dial port is the FIXED internal-loopback port
        // (CANONICAL_INTERNAL_LOOPBACK_PORT = 3000), which never moves when the admin
        // changes the external serving_port (nest/common.md § Same-box reach).
        assert_eq!(cfg.nest_port, 3000);
        assert_eq!(cfg.nest_base_url(), "https://127.0.0.1:3000");
    }

    #[test]
    fn round_trip_toml() {
        let cfg = DeviceConfig {
            device_id: "test-device-01".into(),
            nest_url: "https://nest.example.com".into(),
            nest_port: 8080,
            auth_token: Some("secret".into()),
        };
        let serialized = toml::to_string_pretty(&cfg).unwrap();
        let deserialized: DeviceConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.device_id, "test-device-01");
        assert_eq!(deserialized.nest_port, 8080);
        assert_eq!(deserialized.auth_token.as_deref(), Some("secret"));
    }

    #[cfg(windows)]
    #[test]
    fn default_path_is_under_programdata_not_localappdata() {
        // The device config is shared by Nest + Bridge, which run as machine
        // service accounts (NT SERVICE\Fauna*) that cannot read a per-user
        // %LOCALAPPDATA%. fauna-nest-service writes it to
        // %PROGRAMDATA%\Fauna\device.toml, and the Bridge reader must resolve that
        // same machine-wide path or it finds nothing. The per-user sync agent no longer reads device.toml.
        let programdata = std::env::var("PROGRAMDATA").expect("PROGRAMDATA set on Windows");
        let expected = PathBuf::from(&programdata)
            .join("Fauna")
            .join("device.toml");
        assert_eq!(
            DeviceConfig::default_path(),
            expected,
            "device.toml must resolve under %PROGRAMDATA% to match the nest-service writer"
        );
    }

    #[test]
    fn save_and_load() {
        let dir = std::env::temp_dir().join("fauna-ipc-test-device");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("device.toml");

        let cfg = DeviceConfig {
            device_id: "roundtrip".into(),
            ..Default::default()
        };
        cfg.save_to(&path).unwrap();
        let loaded = DeviceConfig::load_from(&path).unwrap();
        assert_eq!(loaded.device_id, "roundtrip");
        assert_eq!(loaded.nest_port, 3000);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
