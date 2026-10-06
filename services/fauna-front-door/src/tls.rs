//! TLS material: the self-signed floor (the door always answers TLS —
//! front-door.md § TLS policy, the nest's valid-else-floor principle over
//! four fixed names), the hot-swap resolver, and the rustls server config.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use fauna_acme_http01::{CERT_FILENAME, KEY_FILENAME, ReloadableCertResolver};

/// Floor-certificate filenames in the state dir (distinct from the ACME
/// output so an issued cert never overwrites the floor, and vice versa).
pub const FLOOR_CERT_FILENAME: &str = "floor-cert.pem";
pub const FLOOR_KEY_FILENAME: &str = "floor-key.pem";

/// Absolute paths of the ACME-issued material in `state_dir`.
pub fn issued_paths(state_dir: &Path) -> (PathBuf, PathBuf) {
    (state_dir.join(CERT_FILENAME), state_dir.join(KEY_FILENAME))
}

/// Generate (once) and return the floor certificate paths.
pub fn ensure_floor(state_dir: &Path, sans: &[String]) -> Result<(PathBuf, PathBuf)> {
    let cert_path = state_dir.join(FLOOR_CERT_FILENAME);
    let key_path = state_dir.join(FLOOR_KEY_FILENAME);
    if !(cert_path.exists() && key_path.exists()) {
        let key = rcgen::KeyPair::generate().context("generate floor key")?;
        let params = rcgen::CertificateParams::new(sans.to_vec()).context("floor cert params")?;
        let cert = params.self_signed(&key).context("self-sign floor cert")?;
        std::fs::create_dir_all(state_dir).context("create state dir")?;
        std::fs::write(&cert_path, cert.pem()).context("write floor cert")?;
        std::fs::write(&key_path, key.serialize_pem()).context("write floor key")?;
        tracing::info!("floor certificate minted at {}", cert_path.display());
    }
    Ok((cert_path, key_path))
}

/// Boot the resolver: issued material when present and readable, else the
/// floor — the door always answers TLS.
pub fn init_resolver(state_dir: &Path, sans: &[String]) -> Result<Arc<ReloadableCertResolver>> {
    let (issued_cert, issued_key) = issued_paths(state_dir);
    if issued_cert.exists() && issued_key.exists() {
        match ReloadableCertResolver::from_pem(&issued_cert, &issued_key) {
            Ok(resolver) => {
                tracing::info!("serving issued certificate from {}", issued_cert.display());
                return Ok(Arc::new(resolver));
            }
            Err(e) => {
                tracing::warn!("issued certificate unreadable ({e}); serving the floor");
            }
        }
    }
    let (floor_cert, floor_key) = ensure_floor(state_dir, sans)?;
    Ok(Arc::new(
        ReloadableCertResolver::from_pem(&floor_cert, &floor_key).context("load floor cert")?,
    ))
}

/// The rustls server config over the hot-swap resolver, h2 + http/1.1.
pub fn tls_config(resolver: Arc<ReloadableCertResolver>) -> Arc<rustls::ServerConfig> {
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Arc::new(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sans() -> Vec<String> {
        vec!["door.test".into(), "www.door.test".into()]
    }

    #[test]
    fn floor_is_minted_once_and_loads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (c1, k1) = ensure_floor(dir.path(), &sans()).expect("mint");
        let first = std::fs::read(&c1).expect("read");
        // A second call reuses the minted floor rather than rotating it.
        let (c2, _k2) = ensure_floor(dir.path(), &sans()).expect("reuse");
        assert_eq!(c1, c2);
        assert_eq!(first, std::fs::read(&c2).expect("read2"));
        let _ = k1;
        let resolver = init_resolver(dir.path(), &sans()).expect("resolver");
        assert!(resolver.current().is_some(), "floor cert must be served");
    }

    #[test]
    fn issued_material_wins_over_the_floor_and_reload_swaps() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Boot on the floor first.
        let resolver = init_resolver(dir.path(), &sans()).expect("floor boot");
        let floor_key = resolver.current().expect("floor loaded");

        // "Issue" a certificate (self-signed stand-in with the same SANs) at
        // the ACME output paths, then hot-swap.
        let key = rcgen::KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(sans().to_vec()).unwrap();
        let cert = params.self_signed(&key).unwrap();
        let (issued_cert, issued_key) = issued_paths(dir.path());
        std::fs::write(&issued_cert, cert.pem()).unwrap();
        std::fs::write(&issued_key, key.serialize_pem()).unwrap();
        resolver.reload(&issued_cert, &issued_key).expect("swap");
        let swapped = resolver.current().expect("issued loaded");
        assert!(
            !Arc::ptr_eq(&floor_key, &swapped),
            "reload must swap the served key"
        );

        // A fresh boot now prefers the issued material.
        let rebooted = init_resolver(dir.path(), &sans()).expect("reboot");
        assert!(rebooted.current().is_some());
    }

    #[test]
    fn tls_config_advertises_h2_and_http11() {
        let dir = tempfile::tempdir().expect("tempdir");
        let resolver = init_resolver(dir.path(), &sans()).expect("resolver");
        let config = tls_config(resolver);
        assert_eq!(
            config.alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
    }
}
