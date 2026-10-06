//! Per-session scoped bearer tokens for sidecar services.
//!
//! At startup the nest generates a random token for each sidecar, writes it to
//! the data directory (one file per sidecar, mode 0600), and keeps a
//! token → scopes map in memory.  Sidecar processes receive their token via
//! the `FAUNA_SIDECAR_TOKEN` environment variable and present it to the nest:
//! as `Authorization: Bearer <token>` on HTTP, or in the `fauna.sidecar.hello`
//! handshake on the internal sidecar WS-RPC channel.

use std::collections::HashMap;
use std::path::Path;

/// Scopes that a sidecar token may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SidecarScope {
    Bridge,
    Dns,
    /// The self-hosted iroh P2P relay sidecar (`bins/fauna-iroh-relay`). Its
    /// channel (`/internal/relay/ws`) carries the relay's sealed-TLS-cert fetch:
    /// the relay attests its X25519 in the `fauna.sidecar.hello` and the nest
    /// seals the `relay.<apex>` cert to it (the relay never reads `/data/acme`).
    Relay,
}

impl SidecarScope {
    /// The wire string a sidecar declares in its `fauna.sidecar.hello`
    /// handshake (`SidecarHello::scope`) — also the per-sidecar token-file
    /// suffix (`sidecar-token-{as_str}`).
    pub fn as_str(self) -> &'static str {
        match self {
            SidecarScope::Bridge => "bridge",
            SidecarScope::Dns => "dns",
            SidecarScope::Relay => "relay",
        }
    }

    /// Parse a wire scope string back to a `SidecarScope`; `None` for an
    /// unknown scope (a malformed handshake → rejected).
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "bridge" => Some(SidecarScope::Bridge),
            "dns" => Some(SidecarScope::Dns),
            "relay" => Some(SidecarScope::Relay),
            _ => None,
        }
    }
}

/// Description of a sidecar: its name (used for the token filename) and scopes.
struct SidecarDef {
    name: &'static str,
    scopes: &'static [SidecarScope],
}

const SIDECARS: &[SidecarDef] = &[
    SidecarDef {
        name: "bridge",
        scopes: &[SidecarScope::Bridge],
    },
    SidecarDef {
        name: "dns",
        scopes: &[SidecarScope::Dns],
    },
    SidecarDef {
        name: "relay",
        scopes: &[SidecarScope::Relay],
    },
];

/// Generate sidecar tokens, write them to disk, and return a token → scopes map.
///
/// Each token is a 32-byte random value hex-encoded to 64 characters.  The file
/// `{data_dir}/sidecar-token-{name}` is written with mode 0600 (owner read/write
/// only).
pub fn generate_sidecar_tokens(
    data_dir: &Path,
) -> anyhow::Result<HashMap<String, Vec<SidecarScope>>> {
    let mut map = HashMap::new();
    for sidecar in SIDECARS {
        let bytes: [u8; 32] = rand::random();
        let token = hex::encode(bytes);

        let path = data_dir.join(format!("sidecar-token-{}", sidecar.name));
        crate::deployment_key::write_secret_file_0600(&path, token.as_bytes())?;

        tracing::info!(
            "Wrote sidecar token for '{}' to {}",
            sidecar.name,
            path.display()
        );

        map.insert(token, sidecar.scopes.to_vec());
    }

    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_unique_tokens_for_each_sidecar() {
        let dir = tempfile::TempDir::new().unwrap();
        let map = generate_sidecar_tokens(dir.path()).unwrap();

        // Should have one entry per sidecar
        assert_eq!(map.len(), SIDECARS.len());

        // All tokens should be unique and 64-char hex
        let tokens: Vec<&String> = map.keys().collect();
        for token in &tokens {
            assert_eq!(token.len(), 64, "token should be 64 hex chars");
            assert!(hex::decode(token).is_ok(), "token should be valid hex");
        }
        // Check uniqueness
        let unique: std::collections::HashSet<&String> = tokens.iter().copied().collect();
        assert_eq!(unique.len(), tokens.len(), "all tokens should be unique");
    }

    #[test]
    fn writes_token_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let map = generate_sidecar_tokens(dir.path()).unwrap();

        for sidecar in SIDECARS {
            let path = dir.path().join(format!("sidecar-token-{}", sidecar.name));
            assert!(
                path.exists(),
                "token file should exist for {}",
                sidecar.name
            );
            let contents = std::fs::read_to_string(&path).unwrap();
            assert!(
                map.contains_key(&contents),
                "file contents should match a token in the map"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn token_files_have_restricted_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        generate_sidecar_tokens(dir.path()).unwrap();

        for sidecar in SIDECARS {
            let path = dir.path().join(format!("sidecar-token-{}", sidecar.name));
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "token file for {} should be mode 0600",
                sidecar.name
            );
        }
    }

    #[test]
    fn scope_wire_string_round_trips_and_matches_token_filenames() {
        for scope in [SidecarScope::Bridge, SidecarScope::Dns, SidecarScope::Relay] {
            assert_eq!(SidecarScope::from_wire(scope.as_str()), Some(scope));
        }
        assert_eq!(SidecarScope::from_wire("nope"), None);
        // The wire string is also the token-file suffix the minting loop uses.
        for sidecar in SIDECARS {
            assert!(
                SidecarScope::from_wire(sidecar.name).is_some(),
                "token-file name `{}` must map to a wire scope",
                sidecar.name
            );
        }
    }

    #[test]
    fn scope_mapping_is_correct() {
        let dir = tempfile::TempDir::new().unwrap();
        let map = generate_sidecar_tokens(dir.path()).unwrap();

        // Read each token file and verify its scopes
        let bridge_token =
            std::fs::read_to_string(dir.path().join("sidecar-token-bridge")).unwrap();
        assert_eq!(map[&bridge_token], vec![SidecarScope::Bridge]);

        let dns_token = std::fs::read_to_string(dir.path().join("sidecar-token-dns")).unwrap();
        assert_eq!(map[&dns_token], vec![SidecarScope::Dns]);

        let relay_token = std::fs::read_to_string(dir.path().join("sidecar-token-relay")).unwrap();
        assert_eq!(map[&relay_token], vec![SidecarScope::Relay]);
    }
}
