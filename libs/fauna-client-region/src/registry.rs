//! The region registry an app verifies against (`region-blocking.md` § The
//! region registry): the compiled-in snapshot — **empty at version 0 today**,
//! which is its correct content, so on every release build no document reaches
//! any render.
//!
//! **Test-capable builds only:** `FAUNA_E2E_REGION_REGISTRY` joins the snapshot
//! — the hex of one canonical dag-cbor [`RegionRegistry`] enrolling a synthetic
//! authority, which is how a tier_3 journey drives the plane end to end without
//! a real region ever being enrolled. The same gate and posture as
//! `fauna-anon-client`'s `FAUNA_E2E_TRUST_NEST_IDENTITY` seed
//! (`e2e-automation-surface-gating.md`, convention 15): compiled out of release
//! artifacts, so a shipped app never even names the variable.

use fauna_core::region_authority::{RegionRegistry, compiled_in_registry};

/// The environment variable carrying the e2e registry seed.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub const E2E_REGISTRY_ENV: &str = "FAUNA_E2E_REGION_REGISTRY";

/// The registry this app verifies region artifacts against: the compiled-in
/// snapshot, plus — in a test-capable build — the e2e seed's regions.
pub fn effective_registry() -> RegionRegistry {
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    if let Ok(hex) = std::env::var(E2E_REGISTRY_ENV) {
        return seeded_registry(&hex);
    }
    compiled_in_registry()
}

/// **Test-capable builds only.** The compiled-in snapshot plus the regions of
/// one hex seed — [`effective_registry`]'s env read, and the entry point for a
/// shell that reads no environment (web, whose page hands the seed over from
/// browser storage). An empty seed adds nothing.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn seeded_registry(seed_hex: &str) -> RegionRegistry {
    let mut registry = compiled_in_registry();
    if let Some(seed) = decode_seed(seed_hex) {
        merge_seed(&mut registry, seed);
    }
    registry
}

/// A seed region the compiled-in snapshot already enrols is ignored — the seed
/// may add synthetic regions, never re-key a real one.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
fn merge_seed(registry: &mut RegionRegistry, seed: RegionRegistry) {
    for entry in seed.regions {
        if registry.region(&entry.region).is_none() {
            registry.regions.push(entry);
        }
    }
    registry.version = registry.version.max(seed.version);
}

/// The seed, or `None` when it is empty or does not decode (a malformed seed is
/// loud in the log and enrols nothing).
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
fn decode_seed(hex: &str) -> Option<RegionRegistry> {
    if hex.is_empty() {
        return None;
    }
    let Some(bytes) = fauna_core::format::hex_decode(hex) else {
        tracing::warn!("region: {E2E_REGISTRY_ENV} is not hex");
        return None;
    };
    fauna_protocol::decode_strict::<RegionRegistry>(&bytes)
        .map_err(|e| tracing::warn!("region: {E2E_REGISTRY_ENV} does not decode: {e:?}"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::region_authority::{RegionCode, RegionEntry};

    fn entry(code: &str, authority: &str) -> RegionEntry {
        RegionEntry {
            region: RegionCode::parse(code).unwrap(),
            authority_name: authority.into(),
            official_domain: "authority.example".into(),
            parent: None,
            keys: Vec::new(),
        }
    }

    #[test]
    fn a_seed_adds_regions_but_never_rekeys_an_enrolled_one() {
        let mut registry = RegionRegistry {
            version: 3,
            regions: vec![entry("NO", "Real")],
        };
        merge_seed(
            &mut registry,
            RegionRegistry {
                version: 1,
                regions: vec![entry("NO", "Impostor"), entry("XZ", "Synthetic")],
            },
        );
        assert_eq!(registry.version, 3);
        let no = registry.region(&RegionCode::parse("NO").unwrap()).unwrap();
        assert_eq!(no.authority_name, "Real");
        assert!(registry.region(&RegionCode::parse("XZ").unwrap()).is_some());
    }
}
