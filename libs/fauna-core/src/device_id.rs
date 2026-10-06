//! The sync device id an app registers under: derived from an install-scoped
//! secret and the account, never minted per sign-in.
//!
//! Owner: `docs/goal/architecture/apps/sync-agent-credentials.md` § Credential
//! model, the 2026-09-20 ruling. A sign-out erases every account-scoped store
//! — the persisted id included — so an id that is only *stored* comes back
//! different at the next sign-in and the machine registers a second named
//! `sync_devices` row, leaving its memberships on the first. Deriving it makes
//! the persisted copy a cache: the same install signing in as the same account
//! re-derives the same id and comes back to its own row, while two accounts on
//! one install get ids no nest can link (the secret never leaves the machine).

/// Length of the install device secret, in bytes.
pub const INSTALL_DEVICE_SECRET_LEN: usize = 32;

/// A fresh install device secret from the platform CSPRNG.
///
/// Only the first arrival on an install mints; every store that keeps one
/// (the file-backed `fauna_sync_engine::engine_lifecycle`, the secret-store
/// `fauna_client_accounts::device_id`) makes the mint create-once, because two
/// secrets on one install derive two ids and register two rows.
pub fn mint_install_device_secret() -> [u8; INSTALL_DEVICE_SECRET_LEN] {
    let mut secret = [0u8; INSTALL_DEVICE_SECRET_LEN];
    getrandom::fill(&mut secret).expect("getrandom failed");
    secret
}

/// The id `actor_id` registers under on the install holding `install_secret`.
///
/// Stable for a given pair, and unlinkable across actors without the secret.
/// The shared two-step shape of every derivation in this crate
/// ([`crate::domain_key`]): the context isolates the secret, the actor id is
/// the salt.
pub fn derive_device_id(
    install_secret: &[u8; INSTALL_DEVICE_SECRET_LEN],
    actor_id: &[u8; 32],
) -> [u8; 32] {
    crate::domain_key::derive_domain_key("fauna.sync.device-id.v1", install_secret, actor_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_install_and_one_account_always_derive_the_same_id() {
        let secret = [7u8; 32];
        let actor = [1u8; 32];
        assert_eq!(
            derive_device_id(&secret, &actor),
            derive_device_id(&secret, &actor)
        );
    }

    #[test]
    fn a_second_account_on_the_same_install_gets_a_different_id() {
        let secret = [7u8; 32];
        assert_ne!(
            derive_device_id(&secret, &[1u8; 32]),
            derive_device_id(&secret, &[2u8; 32])
        );
    }

    #[test]
    fn the_same_account_on_a_second_install_gets_a_different_id() {
        let actor = [1u8; 32];
        assert_ne!(
            derive_device_id(&[7u8; 32], &actor),
            derive_device_id(&[8u8; 32], &actor)
        );
    }

    /// The derivation is at-rest-visible on every nest the account syncs with:
    /// changing it re-registers every machine. Pin the bytes.
    #[test]
    fn the_derivation_is_pinned() {
        let id = derive_device_id(&[7u8; 32], &[1u8; 32]);
        assert_eq!(crate::hex32::encode(&id), PINNED);
    }

    const PINNED: &str = "674897a544ed49fdea103d1db00f146c73be6e79754010b102eb12b0b637fea9";
}
