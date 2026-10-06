//! The ATProto credential seam's crossing of web's account port — both
//! halves, beside the [`AtprotoCredentialStore`] trait they cross
//! (`docs/goal/architecture/account-client-lifecycle.md` § The client-side
//! lifecycle → *The account port*, decisions (c) and (h)).
//!
//! The ATProto settings machine lives in its own chunk; the account runtime
//! that holds the credentials lives in the core chunk. So the ATProto chunk
//! wires [`PortAtprotoCredentials`] — the **forwarder**, which encodes a
//! method's arguments, calls its door over the port and decodes the answer,
//! and does nothing else — and the core chunk's `accountPortCall` hands each
//! door to [`serve`] over `fauna_account_seams::atproto_credentials`'s
//! `RuntimeAtprotoCredentials`, the one implementation the six native apps
//! wire. Both chunks compile this module, so the two ends of every crossing
//! are one definition.
//!
//! **A port fault is never a success (decision (f)).** No runtime, another
//! account's runtime, an unknown door, undecodable bytes and broken glue all
//! answer the method's own refusal — [`StoreError::Load`] on the read,
//! [`StoreError::Save`] on the two writes — so a mint that cannot reach the
//! runtime is refused before the nest provisions anything.

use fauna_account_port::{PortFault, PortTransport, answer, forward};
use fauna_client_config::StoreError;
use fauna_core::data::{AtprotoAppCredential, AtprotoConfig};
use serde::{Deserialize, Serialize};

use crate::credentials::AtprotoCredentialStore;

/// The seam's doors, one per [`AtprotoCredentialStore`] method, prefixed with
/// the seam — the core chunk's dispatch holds every seam's doors in one
/// namespace.
pub mod doors {
    pub const ATPROTO: &str = "atproto_credentials.atproto";
    pub const PUT_APP_CREDENTIAL: &str = "atproto_credentials.put_app_credential";
    pub const REVOKE_APP_CREDENTIAL: &str = "atproto_credentials.revoke_app_credential";

    /// Every door of the seam.
    pub const ALL: [&str; 3] = [ATPROTO, PUT_APP_CREDENTIAL, REVOKE_APP_CREDENTIAL];
}

/// [`StoreError`] as it crosses: the seam's own refusal is data, not a fault.
#[derive(Serialize, Deserialize)]
enum Refusal {
    Load(String),
    Save(String),
}

impl From<StoreError> for Refusal {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::Load(why) => Self::Load(why),
            StoreError::Save(why) => Self::Save(why),
        }
    }
}

impl From<Refusal> for StoreError {
    fn from(r: Refusal) -> Self {
        match r {
            Refusal::Load(why) => Self::Load(why),
            Refusal::Save(why) => Self::Save(why),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct NoArgs {}

#[derive(Serialize, Deserialize)]
struct Put {
    credential: AtprotoAppCredential,
}

#[derive(Serialize, Deserialize)]
struct Revoke {
    credential_id: String,
}

/// The forwarder: [`AtprotoCredentialStore`] over an account-port transport.
pub struct PortAtprotoCredentials<T> {
    transport: T,
}

impl<T: PortTransport> PortAtprotoCredentials<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<T: PortTransport> AtprotoCredentialStore for PortAtprotoCredentials<T> {
    async fn atproto(&self) -> Result<AtprotoConfig, StoreError> {
        let answered: Result<AtprotoConfig, Refusal> =
            forward(&self.transport, doors::ATPROTO, &NoArgs {})
                .await
                .map_err(|f| StoreError::Load(f.to_string()))?;
        answered.map_err(StoreError::from)
    }

    async fn put_app_credential(
        &self,
        credential: AtprotoAppCredential,
    ) -> Result<bool, StoreError> {
        let answered: Result<bool, Refusal> = forward(
            &self.transport,
            doors::PUT_APP_CREDENTIAL,
            &Put { credential },
        )
        .await
        .map_err(|f| StoreError::Save(f.to_string()))?;
        answered.map_err(StoreError::from)
    }

    async fn revoke_app_credential(&self, credential_id: String) -> Result<bool, StoreError> {
        let answered: Result<bool, Refusal> = forward(
            &self.transport,
            doors::REVOKE_APP_CREDENTIAL,
            &Revoke { credential_id },
        )
        .await
        .map_err(|f| StoreError::Save(f.to_string()))?;
        answered.map_err(StoreError::from)
    }
}

/// Answer one door of the seam from `seam` — the core chunk's half. `None`
/// when `door` is not this seam's, so the dispatch can ask the next seam and
/// refuse a door nobody knows by name.
pub async fn serve(
    seam: &dyn AtprotoCredentialStore,
    door: &str,
    payload: &[u8],
) -> Option<Result<Vec<u8>, PortFault>> {
    Some(match door {
        doors::ATPROTO => {
            answer(payload, |_: NoArgs| async move {
                seam.atproto().await.map_err(Refusal::from)
            })
            .await
        }
        doors::PUT_APP_CREDENTIAL => {
            answer(payload, |a: Put| async move {
                seam.put_app_credential(a.credential)
                    .await
                    .map_err(Refusal::from)
            })
            .await
        }
        doors::REVOKE_APP_CREDENTIAL => {
            answer(payload, |a: Revoke| async move {
                seam.revoke_app_credential(a.credential_id)
                    .await
                    .map_err(Refusal::from)
            })
            .await
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fauna_account_port::loopback::Loopback;
    use fauna_core::secret::SecretByteBuf;

    use super::*;
    use crate::credentials::FakeCredentialStore;

    fn credential(id: &str) -> AtprotoAppCredential {
        AtprotoAppCredential {
            credential_id: id.into(),
            label: format!("{id} label"),
            secret: SecretByteBuf::new(b"abcd-efgh-ijkl-mnop".to_vec()),
            dm_allowed: true,
            created_at: 1_700_000_000,
        }
    }

    fn over(seam: FakeCredentialStore) -> PortAtprotoCredentials<Loopback> {
        let seam = Arc::new(seam);
        PortAtprotoCredentials::new(Loopback::new(move |door, payload| {
            let seam = Arc::clone(&seam);
            async move {
                serve(&*seam, door, &payload)
                    .await
                    .unwrap_or_else(|| Err(PortFault::UnknownDoor(door.to_string())))
            }
        }))
    }

    /// Every method crosses and comes back with the seam's own answer, its
    /// arguments intact — the secret's bytes included — on the success arm.
    #[tokio::test]
    async fn every_method_round_trips_its_success_arm() {
        let store = FakeCredentialStore::new();
        let port = over(store.clone());

        assert_eq!(port.atproto().await.unwrap(), AtprotoConfig::default());
        assert!(port.put_app_credential(credential("ivory")).await.unwrap());
        assert!(
            !port.put_app_credential(credential("ivory")).await.unwrap(),
            "an identical put writes nothing"
        );
        assert_eq!(store.current().app_credentials, vec![credential("ivory")]);
        assert_eq!(
            port.atproto().await.unwrap().app_credentials,
            vec![credential("ivory")],
            "the read crosses back with the secret's bytes intact"
        );
        assert!(port.revoke_app_credential("ivory".into()).await.unwrap());
        assert!(!port.revoke_app_credential("ivory".into()).await.unwrap());
        assert!(store.current().app_credentials.is_empty());
    }

    /// The seam's own refusal crosses as data and comes back as the same
    /// [`StoreError`] arm — the refusal arm of every method.
    #[tokio::test]
    async fn every_method_round_trips_its_refusal_arm() {
        let store = FakeCredentialStore::new();
        store.set_load_failure(true);
        store.arm_save_failure();
        let port = over(store);

        assert!(
            matches!(port.atproto().await, Err(StoreError::Load(w)) if w == "fake: load failure")
        );
        assert!(matches!(
            port.put_app_credential(credential("ivory")).await,
            Err(StoreError::Save(w)) if w == "fake: save failure"
        ));
        assert!(matches!(
            port.revoke_app_credential("ivory".into()).await,
            Err(StoreError::Save(w)) if w == "fake: save failure"
        ));
    }

    /// A faulting transport answers every method with its refusal — never a
    /// success, so a mint that cannot reach the runtime keeps nothing it
    /// believes kept.
    #[tokio::test]
    async fn a_faulting_transport_is_refused_on_every_method() {
        for fault in [
            PortFault::NoRuntime("none".into()),
            PortFault::UnknownDoor("x".into()),
            PortFault::Codec("bad".into()),
            PortFault::Glue("boom".into()),
        ] {
            let port = PortAtprotoCredentials::new(Loopback::faulting(fault.clone()));
            let said = fault.to_string();
            assert!(matches!(port.atproto().await, Err(StoreError::Load(w)) if w == said));
            assert!(matches!(
                port.put_app_credential(credential("ivory")).await,
                Err(StoreError::Save(w)) if w == said
            ));
            assert!(matches!(
                port.revoke_app_credential("ivory".into()).await,
                Err(StoreError::Save(w)) if w == said
            ));
        }
    }

    /// A door that is not this seam's is not answered, so the dispatch can
    /// refuse it by name; every door of the seam is.
    #[tokio::test]
    async fn a_foreign_door_is_not_this_seams() {
        let seam = FakeCredentialStore::new();
        assert!(
            serve(&seam, "fleet_removal.fleet_members", &[])
                .await
                .is_none()
        );
        for door in doors::ALL {
            assert!(serve(&seam, door, &[0xff]).await.is_some(), "{door}");
        }
    }
}
