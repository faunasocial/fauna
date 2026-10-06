//! The folder-key custody seam's crossing of web's account port — both
//! halves, beside the [`FolderKeyStore`] trait they cross
//! (`docs/goal/architecture/account-client-lifecycle.md` § The client-side
//! lifecycle → *The account port*; the `fauna_devices_machine::port` shape).
//!
//! The folders and media chunks read (and the folders chunk writes) the
//! account's shared-folder content-key custody (`fauna.state.folder-keys`);
//! the account runtime that holds it lives in the core chunk. So those chunks
//! wire [`PortFolderKeys`] — the **forwarder**, which encodes a method's
//! arguments, calls its door over the port and decodes the answer, and does
//! nothing else — and the core chunk's `accountPortCall` hands each door to
//! [`serve`] over `fauna_account_seams::folder_keys::PlaneFolderKeys`, the one
//! adapter the six native apps wire. Both chunks compile this module, so the
//! two ends of every crossing are one definition.
//!
//! What crosses is content-key material. The hop is between two wasm modules
//! of one tab, as canonical bytes, never off the device.
//!
//! **A port fault is never a success.** No runtime, another account's
//! runtime, an unknown door, undecodable bytes and broken glue all answer the
//! method's own `Err`, so a read that cannot reach the runtime is "custody
//! unreadable" (a bound set builds keyless, never plaintext) and a write that
//! cannot reach it is refused. A runtime still assembling is the far seam's
//! to wait out, for a write and for the read a write starts from
//! ([`doors::LOAD_FOR_WRITE`]) — the forwarder never waits.

use fauna_account_port::{PortFault, PortTransport, answer, forward};
use fauna_core::data::{FolderPendingRemoval, FoldersConfig};

use crate::key_reader::{FolderKeyReader, FolderKeyStore};

/// The seam's doors, one per [`FolderKeyStore`] method. Prefixed with the
/// seam, since the core chunk's dispatch holds every seam's doors in one
/// namespace.
pub mod doors {
    pub const LOAD: &str = "folder_keys.load";
    pub const LOAD_FOR_WRITE: &str = "folder_keys.load_for_write";
    pub const MERGE: &str = "folder_keys.merge";
    pub const SETTLE_REMOVAL: &str = "folder_keys.settle_removal";
    pub const ADOPTION_MARKERS: &str = "folder_keys.adoption_markers";
    pub const RECORD_ADOPTION_MARKER: &str = "folder_keys.record_adoption_marker";
    pub const MINTING_IDENTITY: &str = "folder_keys.minting_identity";

    /// Every door of the seam.
    pub const ALL: [&str; 7] = [
        LOAD,
        LOAD_FOR_WRITE,
        MERGE,
        SETTLE_REMOVAL,
        ADOPTION_MARKERS,
        RECORD_ADOPTION_MARKER,
        MINTING_IDENTITY,
    ];
}

/// The forwarder: [`FolderKeyStore`] over an account-port transport.
pub struct PortFolderKeys<T> {
    transport: T,
}

impl<T: PortTransport> PortFolderKeys<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}

/// A crossing's answer as the seam's own `Result`: a port fault and the seam's
/// refusal are both `Err`.
fn answered(
    crossed: Result<Result<FoldersConfig, String>, PortFault>,
) -> anyhow::Result<FoldersConfig> {
    crossed
        .map_err(|f| anyhow::anyhow!(f.to_string()))?
        .map_err(|e| anyhow::anyhow!(e))
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<T: PortTransport> FolderKeyReader for PortFolderKeys<T> {
    async fn load(&self) -> anyhow::Result<FoldersConfig> {
        answered(forward(&self.transport, doors::LOAD, &()).await)
    }

    // The list crosses in its stored form (`encode_adoption_markers`).
    async fn adoption_markers(&self) -> anyhow::Result<Vec<[u8; 32]>> {
        let crossed: Result<Result<fauna_protocol::ByteBuf, String>, PortFault> =
            forward(&self.transport, doors::ADOPTION_MARKERS, &()).await;
        let bytes = crossed
            .map_err(|f| anyhow::anyhow!(f.to_string()))?
            .map_err(|e| anyhow::anyhow!(e))?;
        crate::key_reader::decode_adoption_markers(Some(&bytes))
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<T: PortTransport> FolderKeyStore for PortFolderKeys<T> {
    // Its own door, never `LOAD`: the wait for a runtime still assembling is
    // the core chunk's seam's, and it only runs when the crossing says which
    // read this is.
    async fn load_for_write(&self) -> anyhow::Result<FoldersConfig> {
        answered(forward(&self.transport, doors::LOAD_FOR_WRITE, &()).await)
    }

    async fn merge(&self, replica: FoldersConfig) -> anyhow::Result<FoldersConfig> {
        answered(forward(&self.transport, doors::MERGE, &replica).await)
    }

    async fn settle_removal(&self, removal: FolderPendingRemoval) -> anyhow::Result<FoldersConfig> {
        answered(forward(&self.transport, doors::SETTLE_REMOVAL, &removal).await)
    }

    async fn record_adoption_marker(&self, replaced: [u8; 32]) -> anyhow::Result<()> {
        let crossed: Result<Result<(), String>, PortFault> = forward(
            &self.transport,
            doors::RECORD_ADOPTION_MARKER,
            &fauna_protocol::ByteBuf::from(replaced.to_vec()),
        )
        .await;
        crossed
            .map_err(|f| anyhow::anyhow!(f.to_string()))?
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn minting_identity(&self) -> anyhow::Result<Option<fauna_core::identity::ActorId>> {
        let crossed: Result<Result<Option<fauna_core::identity::ActorId>, String>, PortFault> =
            forward(&self.transport, doors::MINTING_IDENTITY, &()).await;
        crossed
            .map_err(|f| anyhow::anyhow!(f.to_string()))?
            .map_err(|e| anyhow::anyhow!(e))
    }
}

/// Answer one door of the seam from `seam` — the core chunk's half. `None`
/// when `door` is not this seam's, so the dispatch can ask the next seam and
/// refuse a door nobody knows by name.
pub async fn serve(
    seam: &dyn FolderKeyStore,
    door: &str,
    payload: &[u8],
) -> Option<Result<Vec<u8>, PortFault>> {
    let said = |e: anyhow::Error| format!("{e:#}");
    Some(match door {
        doors::LOAD => {
            answer(
                payload,
                |(): ()| async move { seam.load().await.map_err(said) },
            )
            .await
        }
        doors::LOAD_FOR_WRITE => {
            answer(payload, |(): ()| async move {
                seam.load_for_write().await.map_err(said)
            })
            .await
        }
        doors::MERGE => {
            answer(payload, |replica: FoldersConfig| async move {
                seam.merge(replica).await.map_err(said)
            })
            .await
        }
        doors::SETTLE_REMOVAL => {
            answer(payload, |removal: FolderPendingRemoval| async move {
                seam.settle_removal(removal).await.map_err(said)
            })
            .await
        }
        doors::ADOPTION_MARKERS => {
            answer(payload, |(): ()| async move {
                let markers = seam.adoption_markers().await.map_err(said)?;
                crate::key_reader::encode_adoption_markers(&markers)
                    .map(fauna_protocol::ByteBuf::from)
                    .map_err(said)
            })
            .await
        }
        doors::RECORD_ADOPTION_MARKER => {
            answer(payload, |replaced: fauna_protocol::ByteBuf| async move {
                let replaced: [u8; 32] = replaced
                    .as_slice()
                    .try_into()
                    .map_err(|_| "an adoption marker is 32 bytes".to_string())?;
                seam.record_adoption_marker(replaced).await.map_err(said)
            })
            .await
        }
        doors::MINTING_IDENTITY => {
            answer(payload, |(): ()| async move {
                seam.minting_identity().await.map_err(said)
            })
            .await
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use fauna_account_port::loopback::Loopback;
    use fauna_client_testkit::block_on;
    use fauna_core::folder_keys::ContentKeyGeneration;
    use fauna_core::identity::ActorId;

    use super::*;
    use crate::key_reader::MemoryFolderKeyStore;

    const CH: [u8; 32] = [9; 32];

    fn over(seam: std::sync::Arc<MemoryFolderKeyStore>) -> PortFolderKeys<Loopback> {
        PortFolderKeys::new(Loopback::new(move |door, payload| {
            let seam = std::sync::Arc::clone(&seam);
            async move {
                serve(&*seam, door, &payload)
                    .await
                    .unwrap_or_else(|| Err(PortFault::UnknownDoor(door.to_string())))
            }
        }))
    }

    /// Every method crosses and comes back with the seam's own answer — the
    /// custody, content keys included, byte-for-byte; a merge is the seam's
    /// join and a settle the seam's settle.
    #[test]
    fn every_method_round_trips_the_custody() {
        let seam = std::sync::Arc::new(MemoryFolderKeyStore::default());
        let port = over(std::sync::Arc::clone(&seam));
        assert_eq!(block_on(port.load()).unwrap(), FoldersConfig::default());
        let mut replica = FoldersConfig::default();
        crate::custody::record_new_set(&mut replica, CH, [0x42; 32], 1_000);
        let removal = FolderPendingRemoval {
            channel_id: CH,
            name: "docs".into(),
            removed_member: ActorId([3; 32]),
            new_generation: ContentKeyGeneration {
                version: 2,
                key: [0x43; 32].into(),
                rotated_at: 2_000,
            },
            commit: None,
            gated_attempted: false,
        };
        crate::custody::stage_pending_removal(&mut replica, removal.clone());
        assert_eq!(block_on(port.merge(replica)).unwrap(), seam.snapshot());
        let settled = block_on(port.settle_removal(removal)).unwrap();
        assert!(settled.pending_removals.is_empty());
        assert_eq!(settled, seam.snapshot());
        assert_eq!(block_on(port.load()).unwrap(), seam.snapshot());
    }

    /// The succession cut's device-local doors cross too
    /// (`writer-signed-change-records.md` ruling (11)): a marker recorded
    /// through the port is the seam's, read back through it, and the identity
    /// the seam serves comes back as itself.
    #[test]
    fn the_adoption_markers_and_the_minting_identity_cross() {
        let seam = std::sync::Arc::new(MemoryFolderKeyStore::default().serving(ActorId([4; 32])));
        let port = over(std::sync::Arc::clone(&seam));
        assert!(block_on(port.adoption_markers()).unwrap().is_empty());
        block_on(port.record_adoption_marker([7; 32])).unwrap();
        block_on(port.record_adoption_marker([7; 32])).unwrap();
        assert_eq!(block_on(port.adoption_markers()).unwrap(), vec![[7; 32]]);
        assert_eq!(
            block_on(FolderKeyReader::adoption_markers(&*seam)).unwrap(),
            vec![[7; 32]]
        );
        assert_eq!(
            block_on(port.minting_identity()).unwrap(),
            Some(ActorId([4; 32]))
        );
    }

    /// A faulting transport answers every method with `Err` — never a
    /// success, never an empty custody a build would read as "no key held".
    #[test]
    fn a_faulting_transport_is_refused_on_every_method() {
        for fault in [
            PortFault::NoRuntime("none".into()),
            PortFault::UnknownDoor("x".into()),
            PortFault::Codec("bad".into()),
            PortFault::Glue("boom".into()),
        ] {
            let port = PortFolderKeys::new(Loopback::faulting(fault));
            assert!(block_on(port.load()).is_err());
            assert!(block_on(port.load_for_write()).is_err());
            assert!(block_on(port.merge(FoldersConfig::default())).is_err());
        }
    }

    /// A seam whose plain read refuses while its write-intent read answers —
    /// `PlaneFolderKeys` over a runtime still assembling.
    struct AssemblingRuntime(MemoryFolderKeyStore);

    #[async_trait::async_trait]
    impl FolderKeyReader for AssemblingRuntime {
        async fn load(&self) -> anyhow::Result<FoldersConfig> {
            Err(anyhow::anyhow!("the account runtime is not running"))
        }
    }

    #[async_trait::async_trait]
    impl FolderKeyStore for AssemblingRuntime {
        async fn load_for_write(&self) -> anyhow::Result<FoldersConfig> {
            self.0.load().await
        }
        async fn merge(&self, replica: FoldersConfig) -> anyhow::Result<FoldersConfig> {
            self.0.merge(replica).await
        }
        async fn settle_removal(
            &self,
            removal: FolderPendingRemoval,
        ) -> anyhow::Result<FoldersConfig> {
            self.0.settle_removal(removal).await
        }
    }

    /// The read a write starts from crosses by its own door, so the far
    /// seam's wait for an assembling runtime runs for it — and only for it: a
    /// plain read crossing the port is still refused at once.
    #[test]
    fn a_write_intent_read_crosses_as_one() {
        let seam = std::sync::Arc::new(AssemblingRuntime(MemoryFolderKeyStore::default()));
        let port = PortFolderKeys::new(Loopback::new(move |door, payload| {
            let seam = std::sync::Arc::clone(&seam);
            async move {
                serve(&*seam, door, &payload)
                    .await
                    .unwrap_or_else(|| Err(PortFault::UnknownDoor(door.to_string())))
            }
        }));
        assert!(block_on(port.load()).is_err());
        assert_eq!(
            block_on(port.load_for_write()).unwrap(),
            FoldersConfig::default()
        );
        block_on(crate::key_reader::update(&port, |c| {
            crate::custody::record_new_set(c, CH, [0x42; 32], 1_000);
        }))
        .expect("a custody write over the port waits with the far seam");
    }

    /// A door that is not this seam's is not answered, so the dispatch can
    /// refuse it by name.
    #[test]
    fn a_foreign_door_is_not_this_seams() {
        let seam = MemoryFolderKeyStore::default();
        assert!(block_on(serve(&seam, "fleet_removal.remove_member", &[])).is_none());
        for door in doors::ALL {
            assert!(block_on(serve(&seam, door, &[0xff])).is_some(), "{door}");
        }
    }
}
