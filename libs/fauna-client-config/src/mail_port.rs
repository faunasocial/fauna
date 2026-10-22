//! The mail seam's crossing of web's account port — both halves, beside the
//! [`MailStore`] trait they cross (`docs/goal/architecture/account-client-lifecycle.md`
//! § The client-side lifecycle → *The account port*, decisions (c) and (h)).
//!
//! The account's mail custody (`fauna.state.mail`) lives in the core chunk's
//! runtime; two machines in other chunks read it — the labeler catalog's,
//! which mints a mail labeler's per-labeler grant from the MSEK, and the
//! Connected apps roster's own mail-settings machine, which lists, reveals and
//! revokes the mail app passwords. So those chunks wire [`PortMailStore`] —
//! the **forwarder**, which encodes a method's arguments, calls its door over
//! the port and decodes the answer, and does nothing else — and the core
//! chunk's `accountPortCall` hands each door to [`serve`] over the seam's one
//! implementation on the account-store handle.
//!
//! **The crossing set is a positive list (decision (h)).** Three doors cross:
//! the READ fold (`mail.load` — the MSEK, and a revealed secret), the raw rows
//! read (`mail.load_rows` — the roster's listing) and a credential's revoke
//! (`mail.revoke` — the roster row's Revoke, after the machine deleted the
//! row's nest-side blobs). The other writes — the state row, a credential's
//! put, the re-wrap mark and a generation's retirement — are the enable, add,
//! rotate and heal paths of the mail page's machine, which runs in the core
//! chunk — and so is the publish those paths await; on the forwarder they
//! refuse with `StoreError::Save` naming the door that does not cross, so a
//! caller that reaches one learns it at once rather than writing nowhere. The
//! walk the connect-time refresh takes runs there too, and refuses on the
//! forwarder the same way as a `StoreError::Load` — nothing walked.
//!
//! **A port fault is never a success (decision (f)).** No runtime, another
//! account's runtime, an unknown door, undecodable bytes and broken glue all
//! answer `StoreError::Load` — an empty custody is never invented.

use crate::store_seam::{MailStore, StoreError};
use fauna_account_port::{PortFault, PortTransport, answer, forward};
use fauna_core::data::{MailConfig, MailCredential, MsekFingerprint, PriorMsekRetirement};
use fauna_core::mail_rows::{MailRows, MailStateRow};
use fauna_core::secret::SecretArray32;
use serde::{Deserialize, Serialize};

/// The mail seam's doors — the crossing ones only (decision (h)). Prefixed
/// with the seam, since the core chunk's dispatch holds every seam's doors in
/// one namespace.
pub mod doors {
    pub const LOAD: &str = "mail.load";
    pub const LOAD_ROWS: &str = "mail.load_rows";
    pub const REVOKE: &str = "mail.revoke";

    /// Every door of the seam that crosses.
    pub const ALL: [&str; 3] = [LOAD, LOAD_ROWS, REVOKE];
}

/// [`MailRows`] as it crosses: the wire codec keys a map by text only, and the
/// retired generations are keyed by fingerprint, so both maps cross as lists
/// and the forwarder rebuilds them through the set's own joins.
#[derive(Serialize, Deserialize)]
struct RowsWire {
    state: Option<MailStateRow>,
    credentials: Vec<MailCredential>,
    generations: Vec<PriorMsekRetirement>,
}

impl From<MailRows> for RowsWire {
    fn from(rows: MailRows) -> Self {
        Self {
            state: rows.state,
            credentials: rows.credentials.into_values().collect(),
            generations: rows.generations.into_values().collect(),
        }
    }
}

impl From<RowsWire> for MailRows {
    fn from(wire: RowsWire) -> Self {
        let mut rows = MailRows {
            state: wire.state,
            ..MailRows::default()
        };
        for c in wire.credentials {
            rows.credentials.insert(c.credential_id.clone(), c);
        }
        for g in wire.generations {
            rows.join_generation(g);
        }
        rows
    }
}

/// What a door answers: the seam method's own result, its failure as the
/// refusal's message — the [`custody_port`](crate::custody_port) shape. The
/// message crosses bare (not `StoreError`'s `Display`, which prefixes it), so
/// the forwarder restores the same refusal and `StoreError::is_not_ready`
/// still recognises the transient one on the far side.
type Reply = Result<MailConfig, String>;
type RowsReply = Result<RowsWire, String>;
type RevokeReply = Result<bool, String>;

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
        let answered: RowsReply = forward(&self.transport, doors::LOAD_ROWS, &())
            .await
            .map_err(|f| StoreError::Load(f.to_string()))?;
        answered.map(MailRows::from).map_err(StoreError::Load)
    }

    async fn write_state(&self, _state: MailStateRow) -> Result<bool, StoreError> {
        Err(StoreError::Save(not_crossing("write_state")))
    }

    async fn clear_rotation(&self, _new_msek: SecretArray32) -> Result<bool, StoreError> {
        Err(StoreError::Save(not_crossing("clear_rotation")))
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

    async fn mark_token_minted(
        &self,
        _credential_id: String,
        _minted_at: u64,
    ) -> Result<bool, StoreError> {
        Err(StoreError::Save(not_crossing("mark_token_minted")))
    }

    async fn revoke(&self, credential_id: String) -> Result<bool, StoreError> {
        let answered: RevokeReply = forward(&self.transport, doors::REVOKE, &credential_id)
            .await
            .map_err(|f| StoreError::Save(f.to_string()))?;
        answered.map_err(StoreError::Save)
    }

    async fn retire_generation(
        &self,
        _generation: fauna_core::data::PriorMsekRetirement,
    ) -> Result<bool, StoreError> {
        Err(StoreError::Save(not_crossing("retire_generation")))
    }

    async fn publish(&self) -> Result<(), StoreError> {
        Err(StoreError::Save(not_crossing("publish")))
    }

    async fn walk(&self) -> Result<(), StoreError> {
        Err(StoreError::Load(not_crossing("walk")))
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
        doors::LOAD_ROWS => {
            answer(payload, |(): ()| async move {
                let reply: RowsReply = seam.load_rows().await.map(RowsWire::from).map_err(message);
                reply
            })
            .await
        }
        doors::REVOKE => {
            answer(payload, |credential_id: String| async move {
                let reply: RevokeReply = seam.revoke(credential_id).await.map_err(message);
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
        async fn clear_rotation(&self, _: SecretArray32) -> Result<bool, StoreError> {
            Err(StoreError::Save(self.0.into()))
        }
        async fn put_credential(&self, _: MailCredential) -> Result<bool, StoreError> {
            Err(StoreError::Save(self.0.into()))
        }
        async fn mark_wrapped(&self, _: String, _: MsekFingerprint) -> Result<bool, StoreError> {
            Err(StoreError::Save(self.0.into()))
        }
        async fn mark_token_minted(&self, _: String, _: u64) -> Result<bool, StoreError> {
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
        async fn publish(&self) -> Result<(), StoreError> {
            Err(StoreError::Save(self.0.into()))
        }
        async fn walk(&self) -> Result<(), StoreError> {
            Err(StoreError::Load(self.0.into()))
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

    /// The rows read crosses and comes back as the seam's own set — the
    /// credential rows and the retired generations rebuilt by key — and the
    /// seam's refusal crosses as the same refusal.
    #[tokio::test]
    async fn load_rows_round_trips_both_arms() {
        let seam = FakeMailStore::with(&enabled());
        seam.put_credential(credential("phone")).await.unwrap();
        seam.retire_generation(retirement()).await.unwrap();
        let port = over(Arc::new(seam.clone()));
        let read = port.load_rows().await.expect("the rows cross");
        assert_eq!(read, seam.rows());
        assert!(read.credentials.contains_key("phone"));
        assert_eq!(read.generations.len(), 1);

        let port = over(Arc::new(Refusing(crate::store_seam::LEDGER_NOT_READY)));
        let err = port.load_rows().await.expect_err("the refusal crosses");
        assert!(err.is_not_ready(), "{err:?}");
    }

    /// A credential's revoke crosses and lands on the seam, answering the
    /// seam's own "did it change" — and the seam's refusal crosses as a
    /// `Save` refusal.
    #[tokio::test]
    async fn revoke_round_trips_both_arms() {
        let seam = FakeMailStore::with(&enabled());
        seam.put_credential(credential("phone")).await.unwrap();
        let port = over(Arc::new(seam.clone()));
        assert!(matches!(port.revoke("phone".into()).await, Ok(true)));
        assert!(seam.rows().credentials["phone"].revoked_at_unix.is_some());
        assert!(
            matches!(port.revoke("phone".into()).await, Ok(false)),
            "already revoked"
        );

        let port = over(Arc::new(Refusing("down")));
        assert!(
            matches!(port.revoke("phone".into()).await, Err(StoreError::Save(m)) if m == "down")
        );
    }

    /// A faulting transport answers the rows read and the revoke with a
    /// refusal, never an empty set or a quiet no-op.
    #[tokio::test]
    async fn a_faulting_transport_refuses_rows_and_revoke() {
        let fault = PortFault::NoRuntime("none".into());
        let port = PortMailStore::new(Loopback::faulting(fault.clone()));
        assert!(
            matches!(port.load_rows().await, Err(StoreError::Load(m)) if m == fault.to_string())
        );
        assert!(
            matches!(port.revoke("x".into()).await, Err(StoreError::Save(m)) if m == fault.to_string())
        );
    }

    fn credential(id: &str) -> MailCredential {
        MailCredential {
            credential_id: id.into(),
            display_name: "Phone".into(),
            kind: fauna_core::data::MailCredentialKind::Plain,
            secret: vec![7u8; 16].into(),
            created_at: 1,
            updated_at: Default::default(),
            wrapped_under: None,
            token_minted_at: None,
            revoked_at_unix: None,
            burned: None,
        }
    }

    fn retirement() -> PriorMsekRetirement {
        PriorMsekRetirement {
            msek: [0x11u8; 32].into(),
            retired_at_unix: 5,
        }
    }

    /// The other row writes do not cross (decision (h)): each refuses on the
    /// forwarder without calling the port, and `serve` answers none of them.
    #[tokio::test]
    async fn the_row_writes_do_not_cross() {
        let seam = FakeMailStore::with(&enabled());
        let port = over(Arc::new(seam.clone()));
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
            token_minted_at: None,
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
            port.mark_token_minted("default".into(), 1).await,
            Err(StoreError::Save(_))
        ));
        assert_eq!(seam.writes(), 0, "nothing was written through the port");
        for door in [
            "mail.write_state",
            "mail.put_credential",
            "mail.mark_wrapped",
        ] {
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
