//! The mail seam's crossing of web's account port — both halves, beside the
//! [`MailStore`] trait they cross (`docs/goal/architecture/account-client-lifecycle.md`
//! § The client-side lifecycle → *The account port*, decisions (c) and (h)).
//!
//! The account's mail custody (`fauna.state.mail`) lives in the core chunk's
//! runtime; the labeler catalog's machine, which mints a mail labeler's
//! per-labeler grant from the MSEK, lives in its own chunk. So that chunk
//! wires [`PortMailStore`] — the **forwarder**, which encodes a method's
//! arguments, calls its door over the port and decodes the answer, and does
//! nothing else — and the core chunk's `accountPortCall` hands each door to
//! [`serve`] over the seam's one implementation on the account-store handle.
//!
//! **The crossing set is a positive list (decision (h)).** Only the READ fold
//! crosses (`mail.load`): the grant mint needs the MSEK and nothing more. The
//! row writes — the state row, a credential's put, re-wrap mark and revoke,
//! and the raw rows read — are the mail-settings machine's, which runs in the
//! core chunk; on the forwarder they refuse with `StoreError::Save` /
//! `StoreError::Load` naming the door that does not cross, so a caller that
//! reaches one learns it at once rather than writing nowhere.
//!
//! **A port fault is never a success (decision (f)).** No runtime, another
//! account's runtime, an unknown door, undecodable bytes and broken glue all
//! answer `StoreError::Load` — an empty custody is never invented.

use crate::store_seam::{MailStore, StoreError};
use fauna_account_port::{PortFault, PortTransport, answer, forward};
use fauna_core::data::{MailConfig, MailCredential, MsekFingerprint};
use fauna_core::mail_rows::{MailRows, MailStateRow};

/// The mail seam's doors — the crossing ones only (decision (h)). Prefixed
/// with the seam, since the core chunk's dispatch holds every seam's doors in
/// one namespace.
pub mod doors {
    pub const LOAD: &str = "mail.load";

    /// Every door of the seam that crosses.
    pub const ALL: [&str; 1] = [LOAD];
}

/// What a door answers: the seam method's own result, its failure as the
/// refusal's message — the [`custody_port`](crate::custody_port) shape. The
/// message crosses bare (not `StoreError`'s `Display`, which prefixes it), so
/// the forwarder restores the same refusal and `StoreError::is_not_ready`
/// still recognises the transient one on the far side.
type Reply = Result<MailConfig, String>;

/// A refusal's own message, without `StoreError`'s `Display` prefix.
fn message(e: StoreError) -> String {
    match e {
        StoreError::Load(m) | StoreError::Save(m) => m,
    }
}

/// The forwarder: [`MailStore`] over an account-port transport.
pub struct PortMailStore<T> {
    transport: T,
}

impl<T: PortTransport> PortMailStore<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}

fn not_crossing(door: &str) -> String {
    format!("the mail door `{door}` does not cross the account port")
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<T: PortTransport> MailStore for PortMailStore<T> {
    async fn load(&self) -> Result<MailConfig, StoreError> {
        let answered: Reply = forward(&self.transport, doors::LOAD, &())
            .await
            .map_err(|f| StoreError::Load(f.to_string()))?;
        answered.map_err(StoreError::Load)
    }

    async fn load_rows(&self) -> Result<MailRows, StoreError> {
        Err(StoreError::Load(not_crossing("load_rows")))
    }

    async fn write_state(&self, _state: MailStateRow) -> Result<bool, StoreError> {
        Err(StoreError::Save(not_crossing("write_state")))
    }

    async fn put_credential(&self, _credential: MailCredential) -> Result<bool, StoreError> {
        Err(StoreError::Save(not_crossing("put_credential")))
    }

    async fn mark_wrapped(
        &self,
        _credential_id: String,
        _fingerprint: MsekFingerprint,
    ) -> Result<bool, StoreError> {
        Err(StoreError::Save(not_crossing("mark_wrapped")))
    }

    async fn revoke(&self, _credential_id: String) -> Result<bool, StoreError> {
        Err(StoreError::Save(not_crossing("revoke")))
    }

    async fn retire_generation(
        &self,
        _generation: fauna_core::data::PriorMsekRetirement,
    ) -> Result<bool, StoreError> {
        Err(StoreError::Save(not_crossing("retire_generation")))
    }
}

/// Answer one door of the mail seam from `seam` — the core chunk's half.
/// `None` when `door` is not this seam's, so the dispatch can ask the next
/// seam and refuse a door nobody knows by name.
pub async fn serve(
    seam: &dyn MailStore,
    door: &str,
    payload: &[u8],
) -> Option<Result<Vec<u8>, PortFault>> {
    Some(match door {
        doors::LOAD => {
            answer(payload, |(): ()| async move {
                let reply: Reply = seam.load().await.map_err(message);
                reply
            })
            .await
        }
        _ => return None,
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::sync::Arc;

    use fauna_account_port::loopback::Loopback;

    use super::*;
    use crate::test_helpers::FakeMailStore;

    fn over(seam: Arc<dyn MailStore>) -> PortMailStore<Loopback> {
        PortMailStore::new(Loopback::new(move |door, payload| {
            let seam = Arc::clone(&seam);
            async move {
                serve(&*seam, door, &payload)
                    .await
                    .unwrap_or_else(|| Err(PortFault::UnknownDoor(door.to_string())))
            }
        }))
    }

    fn enabled() -> MailConfig {
        MailConfig {
            msek: Some([0x42u8; 32].into()),
            mail_enabled: Some(true),
            ..MailConfig::default()
        }
    }

    /// A seam whose every method refuses with `StoreError::Load(why)`.
    struct Refusing(&'static str);

    #[async_trait::async_trait]
    impl MailStore for Refusing {
        async fn load(&self) -> Result<MailConfig, StoreError> {
            Err(StoreError::Load(self.0.into()))
        }
        async fn load_rows(&self) -> Result<MailRows, StoreError> {
            Err(StoreError::Load(self.0.into()))
        }
        async fn write_state(&self, _: MailStateRow) -> Result<bool, StoreError> {
            Err(StoreError::Save(self.0.into()))
        }
        async fn put_credential(&self, _: MailCredential) -> Result<bool, StoreError> {
            Err(StoreError::Save(self.0.into()))
        }
        async fn mark_wrapped(&self, _: String, _: MsekFingerprint) -> Result<bool, StoreError> {
            Err(StoreError::Save(self.0.into()))
        }
        async fn revoke(&self, _: String) -> Result<bool, StoreError> {
            Err(StoreError::Save(self.0.into()))
        }
        async fn retire_generation(
            &self,
            _: fauna_core::data::PriorMsekRetirement,
        ) -> Result<bool, StoreError> {
            Err(StoreError::Save(self.0.into()))
        }
    }

    /// The read crosses and comes back as the seam's own fold — the MSEK the
    /// grant mint derives from included — and the seam's refusal crosses as
    /// the same refusal, arm and message intact.
    #[tokio::test]
    async fn load_round_trips_both_arms() {
        let seam = FakeMailStore::with(&enabled());
        let port = over(Arc::new(seam.clone()));
        let read = port.load().await.expect("the fold crosses");
        assert_eq!(read, seam.current());
        assert_eq!(read.msek.as_ref().map(|m| m.to_array()), Some([0x42u8; 32]));

        let port = over(Arc::new(Refusing(crate::store_seam::LEDGER_NOT_READY)));
        let err = port.load().await.expect_err("the refusal crosses");
        assert!(
            matches!(&err, StoreError::Load(m) if m == crate::store_seam::LEDGER_NOT_READY),
            "{err:?}"
        );
        assert!(
            err.is_not_ready(),
            "the not-ready refusal stays recognisable"
        );
    }

    /// A faulting transport answers the read with a refusal — never an empty
    /// custody, so a grant mint that cannot reach the runtime mints nothing.
    #[tokio::test]
    async fn a_faulting_transport_is_refused() {
        for fault in [
            PortFault::NoRuntime("none".into()),
            PortFault::UnknownDoor("x".into()),
            PortFault::Codec("bad".into()),
            PortFault::Glue("boom".into()),
        ] {
            let port = PortMailStore::new(Loopback::faulting(fault.clone()));
            assert!(
                matches!(port.load().await, Err(StoreError::Load(m)) if m == fault.to_string()),
                "{fault:?}"
            );
        }
    }

    /// The row doors do not cross (decision (h)): each refuses on the
    /// forwarder without calling the port, and `serve` answers none of them.
    #[tokio::test]
    async fn the_row_doors_do_not_cross() {
        let seam = FakeMailStore::with(&enabled());
        let port = over(Arc::new(seam.clone()));
        assert!(matches!(port.load_rows().await, Err(StoreError::Load(_))));
        assert!(matches!(
            port.write_state(MailStateRow::default()).await,
            Err(StoreError::Save(_))
        ));
        let credential = MailCredential {
            credential_id: "second".into(),
            display_name: "Phone".into(),
            kind: fauna_core::data::MailCredentialKind::Plain,
            secret: vec![7u8; 16].into(),
            created_at: 1,
            updated_at: Default::default(),
            wrapped_under: None,
            revoked_at_unix: None,
            burned: None,
        };
        assert!(matches!(
            port.put_credential(credential).await,
            Err(StoreError::Save(_))
        ));
        assert!(matches!(
            port.mark_wrapped("default".into(), MsekFingerprint::of(&[1u8; 32].into()))
                .await,
            Err(StoreError::Save(_))
        ));
        assert!(matches!(
            port.revoke("default".into()).await,
            Err(StoreError::Save(_))
        ));
        assert_eq!(seam.writes(), 0, "nothing was written through the port");
        for door in ["mail.load_rows", "mail.write_state", "mail.revoke"] {
            assert!(serve(&seam, door, &[]).await.is_none(), "{door}");
        }
    }

    /// A door that is not this seam's is not answered, so the dispatch can
    /// refuse it by name; every door of the seam is.
    #[tokio::test]
    async fn a_foreign_door_is_not_this_seams() {
        let seam = FakeMailStore::with(&enabled());
        assert!(
            serve(&seam, "fleet_removal.fleet_members", &[])
                .await
                .is_none()
        );
        let payload = fauna_account_port::encode(&()).unwrap();
        for door in doors::ALL {
            assert!(serve(&seam, door, &payload).await.is_some(), "{door}");
        }
    }
}
