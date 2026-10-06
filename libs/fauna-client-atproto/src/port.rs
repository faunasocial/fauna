//! The ATProto account-state seam's crossing of web's account port — both
//! halves, beside the [`AtprotoIdentityStore`] trait they cross
//! (`docs/goal/architecture/account-client-lifecycle.md` § The client-side
//! lifecycle → *The account port*; the `fauna_devices_machine::port` shape).
//!
//! The ATProto settings page's machine lives in its own wasm chunk
//! (`fauna-wasm-atproto-settings`); the account runtime lives in the core
//! chunk. So the settings chunk wires [`PortAtprotoIdentityStore`] — the
//! **forwarder**, which encodes a method's arguments, calls its door over the
//! port and decodes the answer, and does nothing else — and the core chunk's
//! `accountPortCall` hands each door to [`serve`] over
//! `fauna_account_seams::atproto_identity::RuntimeAtprotoIdentity`, the one adapter
//! the six native apps wire. Both chunks compile this module, so the two ends
//! of every crossing are one definition.
//!
//! What crosses includes the senior rotation keys' scalars: the settings
//! chunk signs the tombstone and the recovery-fork contest with them, exactly
//! as the native machine does. The hop is between two wasm modules of one
//! tab, as canonical bytes, never off the device.
//!
//! **A port fault is never a success.** No runtime, another account's
//! runtime, an unknown door, undecodable bytes and broken glue all answer the
//! method's own `Err`, so a custody read that cannot reach the runtime is
//! "cannot verify" and a write that cannot reach it is refused.

use fauna_account_port::{PortFault, PortTransport, answer, forward};
use fauna_core::data::AtprotoIdentityConfig;

use crate::identity_store::AtprotoIdentityStore;

/// The seam's doors, one per [`AtprotoIdentityStore`] method. Prefixed with
/// the seam, since the core chunk's dispatch holds every seam's doors in one
/// namespace.
pub mod doors {
    pub const ATPROTO_IDENTITY: &str = "atproto_identity.atproto_identity";
    pub const MERGE_ATPROTO_IDENTITY: &str = "atproto_identity.merge_atproto_identity";

    /// Every door of the seam.
    pub const ALL: [&str; 2] = [ATPROTO_IDENTITY, MERGE_ATPROTO_IDENTITY];
}

/// The forwarder: [`AtprotoIdentityStore`] over an account-port transport.
pub struct PortAtprotoIdentityStore<T> {
    transport: T,
}

impl<T: PortTransport> PortAtprotoIdentityStore<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<T: PortTransport> AtprotoIdentityStore for PortAtprotoIdentityStore<T> {
    async fn atproto_identity(&self) -> Result<AtprotoIdentityConfig, String> {
        forward(&self.transport, doors::ATPROTO_IDENTITY, &())
            .await
            .map_err(|f| f.to_string())?
    }

    async fn merge_atproto_identity(
        &self,
        replica: AtprotoIdentityConfig,
    ) -> Result<AtprotoIdentityConfig, String> {
        forward(&self.transport, doors::MERGE_ATPROTO_IDENTITY, &replica)
            .await
            .map_err(|f| f.to_string())?
    }
}

/// Answer one door of the seam from `seam` — the core chunk's half. `None`
/// when `door` is not this seam's, so the dispatch can ask the next seam and
/// refuse a door nobody knows by name.
pub async fn serve(
    seam: &dyn AtprotoIdentityStore,
    door: &str,
    payload: &[u8],
) -> Option<Result<Vec<u8>, PortFault>> {
    Some(match door {
        doors::ATPROTO_IDENTITY => {
            answer(
                payload,
                |(): ()| async move { seam.atproto_identity().await },
            )
            .await
        }
        doors::MERGE_ATPROTO_IDENTITY => {
            answer(payload, |replica: AtprotoIdentityConfig| async move {
                seam.merge_atproto_identity(replica).await
            })
            .await
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use fauna_account_port::loopback::Loopback;
    use fauna_core::data::{AtprotoContestIntent, AtprotoRotationKey};

    use super::*;
    use crate::identity_store::{
        InMemoryAtprotoIdentityStore, NO_ACCOUNT_RUNTIME, NoAccountRuntime,
    };

    fn custody() -> AtprotoIdentityConfig {
        AtprotoIdentityConfig {
            rotation_keys: vec![AtprotoRotationKey {
                secret_scalar: [7u8; 32].into(),
                pubkey_did_key: "did:key:zSenior".into(),
                created_at: 11,
                published_for_dids: vec!["did:plc:aaa".into()],
            }],
            tombstone_consents: vec!["did:plc:aaa".into()],
            contest_intents: vec![AtprotoContestIntent {
                did: "did:plc:aaa".into(),
                contested_op_cid: "bafyop".into(),
                requested_at: 12,
            }],
            nest_named_dids: vec!["did:plc:aaa".into()],
        }
    }

    fn over<S: AtprotoIdentityStore + 'static>(
        seam: std::sync::Arc<S>,
    ) -> PortAtprotoIdentityStore<Loopback> {
        PortAtprotoIdentityStore::new(Loopback::new(move |door, payload| {
            let seam = std::sync::Arc::clone(&seam);
            async move {
                serve(&*seam, door, &payload)
                    .await
                    .unwrap_or_else(|| Err(PortFault::UnknownDoor(door.to_string())))
            }
        }))
    }

    /// Both methods cross and come back with the seam's own answer — the
    /// custody, secret scalar included, byte-for-byte; a merge is the seam's
    /// join, not a replacement.
    #[tokio::test]
    async fn every_method_round_trips_the_custody() {
        let seam = std::sync::Arc::new(InMemoryAtprotoIdentityStore::default());
        let port = over(std::sync::Arc::clone(&seam));
        assert_eq!(
            port.atproto_identity().await,
            Ok(AtprotoIdentityConfig::default())
        );
        assert_eq!(port.merge_atproto_identity(custody()).await, Ok(custody()));
        // An empty replica adds nothing and removes nothing.
        assert_eq!(
            port.merge_atproto_identity(AtprotoIdentityConfig::default())
                .await,
            Ok(custody())
        );
        assert_eq!(port.atproto_identity().await, Ok(custody()));
        assert_eq!(seam.current(), custody());
    }

    /// The seam's own refusal crosses as data, verbatim.
    #[tokio::test]
    async fn the_seams_refusal_crosses_as_its_own_err() {
        let port = over(std::sync::Arc::new(NoAccountRuntime));
        assert_eq!(
            port.atproto_identity().await,
            Err(NO_ACCOUNT_RUNTIME.into())
        );
        assert_eq!(
            port.merge_atproto_identity(custody()).await,
            Err(NO_ACCOUNT_RUNTIME.into())
        );
    }

    /// A faulting transport answers every method with `Err` — never a
    /// success, never an empty custody a check would read as "no key held".
    #[tokio::test]
    async fn a_faulting_transport_is_refused_on_every_method() {
        for fault in [
            PortFault::NoRuntime("none".into()),
            PortFault::UnknownDoor("x".into()),
            PortFault::Codec("bad".into()),
            PortFault::Glue("boom".into()),
        ] {
            let port = PortAtprotoIdentityStore::new(Loopback::faulting(fault.clone()));
            let said = fault.to_string();
            assert_eq!(port.atproto_identity().await, Err(said.clone()));
            assert_eq!(port.merge_atproto_identity(custody()).await, Err(said));
        }
    }

    /// A door that is not this seam's is not answered, so the dispatch can
    /// refuse it by name.
    #[tokio::test]
    async fn a_foreign_door_is_not_this_seams() {
        let seam = InMemoryAtprotoIdentityStore::default();
        assert!(
            serve(&seam, "fleet_removal.remove_member", &[])
                .await
                .is_none()
        );
        for door in doors::ALL {
            assert!(serve(&seam, door, &[0xff]).await.is_some(), "{door}");
        }
    }
}
