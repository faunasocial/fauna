//! E2e test helper: mint real MLS key packages for a given Ed25519 identity.
//!
//! Used by `tests/e2e-unified/tests/test_fauna_mls_real_roundtrip.py`. The
//! tier_3 real-wire FaunaMls round-trip drives the linux app's REAL backend,
//! whose group bootstrap fetches and **parses** each peer's key package
//! (`MlsEngine::key_package_from_bytes`). The peers in that test are API-tier
//! actors with no MLS engine, so they can't produce a parseable key package
//! themselves — a fake byte string (as the API-only `test_mls_channels.py`
//! uses) is consumed from the nest but then fails to parse, so no group /
//! Welcome / channel envelope is produced.
//!
//! This helper spins a throwaway in-memory engine bound to the peer's identity
//! and emits a real, TLS-serialized `KeyPackage` (hex, one per line) the linux
//! engine accepts. The key package's private half lives only in the throwaway
//! engine and is discarded on exit — fine, because the API-tier peer never
//! processes the Welcome (it observes nest-side effects, it doesn't decrypt;
//! decrypt is proven in `libs/fauna-conversations/tests/fauna_mls_backend_tests.rs`
//! with two real engines).
//!
//! Usage: `mls-keypackage-gen <ed25519_secret_hex> [count]` (count defaults to 1).

use fauna_core::identity::ActorKeypair;
use fauna_mls::engine::MlsEngine;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let secret_hex = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: mls-keypackage-gen <secret_hex> [count]"))?;
    let count: usize = match args.next() {
        Some(s) => s.parse()?,
        None => 1,
    };

    let secret_bytes = hex::decode(secret_hex.trim())?;
    let secret: [u8; 32] = secret_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("secret must be 32 bytes"))?;

    let engine = MlsEngine::new_in_memory(ActorKeypair::from_secret(secret))
        .map_err(|e| anyhow::anyhow!("engine init: {e:?}"))?;
    let packages = engine
        .generate_key_packages_bytes(count)
        .map_err(|e| anyhow::anyhow!("generate key packages: {e:?}"))?;

    for kp in packages {
        println!("{}", hex::encode(kp));
    }
    Ok(())
}
