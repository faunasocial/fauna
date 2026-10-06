#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_provisioning");

#[cfg(feature = "atproto-seal")]
pub mod atproto;
pub mod bundled_api;
pub mod cloud_init;
pub mod dispatch;
pub mod dkim;
pub mod dns;
pub mod error;
mod money;
pub mod namecheap_api;
// UNgated, unlike the key plane inside it: the module's token-lifetime and
// key-retirement-horizon constants are policy integers with no crypto, and they
// are read by consumers that mint nothing — the Go bridge through fauna-ffi,
// and any client that needs the number. Gating the whole module behind
// `oauth-issuer` would drag p256 into every one of those builds to read two
// integers. The items that DO need a curve carry the feature individually.
pub mod oauth_issuer;
pub mod orchestrator;
pub mod probe;
pub mod progress;
pub mod providers_generated;
pub mod proxy;
pub mod registrar;
/// The DNS-cleanup plan for retiring a box (`behavior/nest-retirement.md`
/// § DNS cleanup) — the inverse of `orchestrator::build_records`.
pub mod retire_dns;
pub mod verify_json;
pub mod vps;

pub use providers_generated::{
    Capability, CorsPolicy, FieldMeta, FieldType, PROVIDERS, PostVerifySelect, ProviderId,
    ProviderMeta,
};

/// Convenience wrapper for the onboarding machine: returns a fresh Ed25519
/// keypair as a 64-char hex secret string. Mirrors the `generate_keypair`
/// FFI export in libs/fauna-wasm and libs/fauna-ffi but lives here so the
/// machine can call it without pulling in those crates.
pub fn generate_keypair_hex() -> String {
    let kp = fauna_core::identity::ActorKeypair::generate();
    hex::encode(kp.secret_bytes())
}

/// Mint a one-time admin claim code the user injects into the nest at
/// provisioning time (the client mints it before the box exists and passes it
/// via cloud-init). Delegates to `fauna_core::claim_code::generate` — the
/// single source of truth for the format, shared with the nest's own minting
/// path (`bins/fauna-nest/src/claim.rs`) so a client-minted and a nest-minted
/// code are byte-identical. 40-bit, ambiguity-free, displayed grouped.
pub fn generate_claim_code() -> String {
    fauna_core::claim_code::generate()
}

/// Mint a fresh deployment Ed25519 signing seed as a 64-char hex string, for
/// the **client-provisioned-cloud** box-recovery origin
/// (`docs/goal/architecture/nest/box-recovery.md` § Mechanism — Capture): the
/// admin's client is the seed's origin — it generates the seed here, injects it
/// as `FAUNA_DEPLOYMENT_SEED` at provision so the box boots with that
/// `nest_actor_id`, and (post-claim) custodies the same seed off-box in
/// `fauna.state.deployment-seeds` so a rebuilt box can re-present the identity after total box loss.
///
/// A deployment seed *is* a 32-byte Ed25519 signing seed, so this delegates to
/// the same primitive as [`generate_keypair_hex`] — the nest decodes the 64-hex
/// value back to `[u8; 32]` (`deployment_key::deployment_seed_from_env`).
pub fn generate_deployment_seed() -> String {
    generate_keypair_hex()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deployment seed must be a 64-char hex string decoding to a 32-byte
    /// Ed25519 seed (the shape `deployment_key::deployment_seed_from_env`
    /// decodes), and fresh on each call (it is the box's unique identity).
    #[test]
    fn generate_deployment_seed_is_fresh_32_byte_hex() {
        let a = generate_deployment_seed();
        let b = generate_deployment_seed();
        assert_eq!(a.len(), 64, "deployment seed must be 64 hex chars");
        let bytes = hex::decode(&a).expect("deployment seed must be valid hex");
        assert_eq!(bytes.len(), 32, "deployment seed must decode to 32 bytes");
        assert_ne!(a, b, "each generated seed must be unique");
    }
}
