//! Re-exports the page-level Devices state machine so its UniFFI exports surface
//! in the generated Swift / Kotlin / C# bindings, plus a free-fn constructor
//! that builds the machine over an [`FfiNestClient`]'s WS-RPC connection. The
//! machine itself lives in libs/fauna-devices-machine; this file is a thin glue
//! layer (mirrors src/folders.rs).

use std::sync::Arc;

pub use fauna_devices_machine::{
    ConflictCandidateSummary, ConflictSummary, DeviceFolderRole, DeviceSummary, DevicesMachine,
    DevicesObserver, DevicesSnapshot, FolderSummary,
};

use crate::folders_client::ffi_folder_members;
use crate::{FfiError, FfiFolderMember, FfiFoldersClient, FfiNestClient, stringify};

/// The device-place editor's read. It lives here rather than beside
/// `members_list` in `src/folders_client.rs` because it takes the Devices
/// page's roster — a `folders`-feature type the Go mail-bridge build does not
/// carry.
#[fauna_uniffi_async::export]
impl FfiFoldersClient {
    /// `fauna.folders.members.list`, projected for `folder-place-row` and
    /// NAMED from `devices` — the roster the app painted the Devices page from
    /// (`DevicesSnapshot::devices`, already unsealed). Every user-chosen device
    /// label rests sealed, so without the roster each named seat paints blank;
    /// a seat the roster does not hold keeps the nest's label
    /// (`fauna_devices_machine::place_rows`, the one join all seven apps use).
    pub async fn place_rows(
        &self,
        name: String,
        devices: Vec<DeviceSummary>,
    ) -> Result<Vec<FfiFolderMember>, FfiError> {
        let reply = self.client().members_list(name).await.map_err(stringify)?;
        let rows = fauna_devices_machine::place_rows(&reply.members, &devices);
        Ok(ffi_folder_members(reply.members, rows))
    }
}

/// [`fauna_devices_machine::FleetRemoval`] over this process's own
/// account-store runtime (`crate::account_runtime::handle()`), the
/// devices page's remove-device action's second leg beside
/// `fauna.sync.devices.delete` (`docs/goal/behavior/devices.md` § Removing a
/// Device) — the shared adapter every runtime-hosting seat wires
/// (`fauna_client_account_runtime::fleet_removal`, which owns the rule that an
/// absent runtime refuses the removal).
///
/// Wired once in [`build_devices_machine`] below, the ONE seam
/// android/windows/macOS/iOS all funnel through (`Self`'s doc comment there),
/// so this closes the fleet-scope leg for all four UniFFI apps at once
/// instead of four separate per-app calls — mirrors this file's own
/// `set_label_custody` precedent.
#[cfg(feature = "account-runtime")]
fn account_runtime_fleet_removal() -> Arc<dyn fauna_devices_machine::FleetRemoval> {
    Arc::new(
        fauna_client_account_runtime::fleet_removal::RuntimeFleetRemoval::new(
            crate::account_runtime::handle,
        ),
    )
}

/// Build a [`DevicesMachine`] for the Devices page over `nest`'s authenticated
/// WS-RPC connection. `observer` ticks on every snapshot change. The machine
/// owns the device / folder / conflict reads (`refresh()`), the page write
/// gestures (`remove_device` / `delete_folder` / `resolve_conflict` /
/// `set_folder_paths`), and the embedded folder creation wizard
/// (`open_wizard` / `wizard()` / `close_wizard`) — all over the same connection.
///
/// **Label custody is wired here, not left to each app's build glue**
/// (`docs/goal/behavior/path-sealing.md` § Sealed names & paths — S3): this
/// free fn is the one seam apple/android/windows all funnel through (unlike
/// linux/tui, which build `DevicesMachine` directly and wire it themselves),
/// so wiring it once here closes the conflict-list render for all three FFI
/// apps at once instead of three separate per-app `set_label_custody` calls.
/// Same `client()` pattern `FfiSnapshotsClient`/`FfiFoldersClient` already
/// use: derive the resolver + owner `BackupKey` from the connection's own
/// keypair, gated on `folders-author` (the resolver's crate feature) so a
/// `--no-default-features` build (the Go mail-bridge) still compiles keyless.
/// A bearer-only connection with no keypair leaves the machine keyless,
/// byte-identical to pre-wiring behaviour.
///
/// **The fleet-scope removal door is wired here too, the same reason**
/// (see [`account_runtime_fleet_removal`]) — unconditionally under
/// `account-runtime` rather than gated on a keypair, since the adapter reads
/// its handle fresh per call rather than capturing anything at this point.
#[uniffi::export]
pub fn build_devices_machine(
    nest: Arc<FfiNestClient>,
    observer: Arc<dyn DevicesObserver>,
) -> Arc<DevicesMachine> {
    let nest_arc = nest.nest_arc();
    #[cfg(feature = "account-runtime")]
    let custody = Some(crate::account_runtime::folder_key_store());
    #[cfg(not(feature = "account-runtime"))]
    let custody = None;
    let machine =
        fauna_devices_machine::build_devices_machine(Arc::clone(&nest_arc), custody, observer);
    #[cfg(feature = "account-runtime")]
    machine.set_fleet_removal(account_runtime_fleet_removal());
    // This device's own peer-participation door (`p2p.md` § Per-device
    // participation) — the same shared impl over the same handle read, so
    // the four FFI apps' devices pages inherit the own-row switch with the
    // machine and never gate a listener themselves. When a co-located agent
    // holds the engine (windows, macOS), the switch asks it for its pass
    // through the seat's holder nudge, which `FfiSyncAgentProvisioner`
    // publishes at its build.
    #[cfg(feature = "account-runtime")]
    machine.set_p2p_participation_door(Arc::new(
        fauna_client_account_runtime::p2p_participation::RuntimeP2pParticipation::new(
            crate::account_runtime::handle,
        )
        .with_holder_nudge(
            fauna_client_account_runtime::p2p_participation::EngineHolderNudgeSlot::seat(),
        ),
    ));
    // The audience attestor — the same one-seam reasoning as label custody
    // below, and ungated: the `→public` flip every FFI app lands through this
    // machine must carry the owner's signature (`encryption-at-rest.md`
    // § Readable classes → *The declassification is owner-ATTESTED*), or no
    // verifying seat ever unseals the folder. A bearer-only connection has no
    // key and flips unattested, exactly as before.
    if let Some(keypair) = nest_arc.auth().keypair() {
        machine.set_audience_attestor(Arc::new(fauna_core::identity::ActorKeypair::from_secret(
            *keypair.secret_bytes(),
        )));
    }
    #[cfg(all(feature = "folders-author", not(target_arch = "wasm32")))]
    if let Some(keypair) = nest_arc.auth().keypair() {
        let secret = *keypair.secret_bytes();
        let resolver: Arc<dyn fauna_core::folder_keys::FolderKeyResolver> =
            Arc::new(fauna_client_folders::NestFolderKeyResolver::new(
                Arc::clone(&nest_arc),
                crate::account_runtime::folder_key_store(),
            ));
        machine.set_label_custody(fauna_core::label_custody::LabelCustody::new(
            Some(resolver),
            Some(fauna_core::crypto::BackupKey::derive(&secret)),
        ));
    }
    machine
}

/// The roster row the Devices page's `device-this-mark-badge` marks
/// (`docs/goal/behavior/devices.md` § This-device marker): the row this
/// machine's enrollment latched on (`AccountStoreHandle::enrolled_device_row`
/// over this process's account runtime), else `own_device_id` — the app's own
/// locally-stored id — else `None`. The rule is
/// [`fauna_devices_machine::this_device_row`]'s, the one tui and linux already
/// read; this is the UniFFI apps' door to it, so no app re-derives it in its
/// own language.
///
/// A local slot read — never a network call and never IPC — so it can ride
/// every Devices hydrate. With no runtime assembled (before sign-in, or a
/// build without `account-runtime`) or an unreadable latch, the rule's
/// fallback answers: the own id, which is the correct row for every case that
/// enrolls under it.
#[fauna_uniffi_async::export]
pub async fn devices_this_device_row(own_device_id: Option<String>) -> Option<String> {
    this_device_row_over(enrolled_device_row().await, own_device_id)
}

#[cfg(feature = "account-runtime")]
async fn enrolled_device_row() -> Option<String> {
    let handle = crate::account_runtime::handle()?;
    match handle.enrolled_device_row().await {
        Ok(row) => row,
        Err(e) => {
            tracing::debug!("devices_this_device_row: {e}");
            None
        }
    }
}

#[cfg(not(feature = "account-runtime"))]
async fn enrolled_device_row() -> Option<String> {
    None
}

fn this_device_row_over(enrolled: Option<String>, own: Option<String>) -> Option<String> {
    fauna_devices_machine::this_device_row(enrolled.as_deref(), own.as_deref())
}

/// Which roster rows carry the keyless-posture marker
/// (`device-keyless-posture-badge`; `docs/goal/ui/devices.md` § Custody facet
/// piece 1) — one answer per entry of `principals`, in order, where each entry
/// is a roster row's `DeviceSummary.principal`.
///
/// The keyed set is read ONCE per call (`AccountStoreHandle::keyed_principals`
/// over this process's account runtime) and each row is answered by
/// [`fauna_devices_machine::keyless_posture`], the rule tui and linux read, so
/// no app re-derives the join or its fail-safes in its own language. With no
/// runtime assembled, no resolved tip, or an unreadable store, every answer is
/// `false`: a "holds no keys" marker never rests on an unknown. A local read —
/// never a network call and never IPC.
#[fauna_uniffi_async::export]
pub async fn devices_keyless_posture(principals: Vec<Option<String>>) -> Vec<bool> {
    keyless_posture_over(keyed_principals().await.as_ref(), &principals)
}

#[cfg(feature = "account-runtime")]
async fn keyed_principals() -> Option<std::collections::BTreeSet<[u8; 32]>> {
    let handle = crate::account_runtime::handle()?;
    match handle.keyed_principals().await {
        Ok(keyed) => keyed,
        Err(e) => {
            tracing::debug!("devices_keyless_posture: {e}");
            None
        }
    }
}

#[cfg(not(feature = "account-runtime"))]
async fn keyed_principals() -> Option<std::collections::BTreeSet<[u8; 32]>> {
    None
}

fn keyless_posture_over(
    keyed: Option<&std::collections::BTreeSet<[u8; 32]>>,
    principals: &[Option<String>],
) -> Vec<bool> {
    principals
        .iter()
        .map(|p| fauna_devices_machine::keyless_posture(keyed, p.as_deref()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NullObserver;
    impl DevicesObserver for NullObserver {
        fn on_changed(&self) {}
    }

    /// `build_devices_machine` used to hand every FFI app (apple/android/
    /// windows) a keyless `DevicesMachine`, so a conflict on a sealed-only
    /// plane rendered no name once the flip scrubbed the plaintext column
    /// (`docs/goal/behavior/path-sealing.md` § Sealed names & paths — S3,
    /// "the remaining per-app custody wiring is the same batched
    /// trickle-down as the media sweep"). linux/tui wire custody themselves
    /// (they build `DevicesMachine` directly and hold the keypair already);
    /// the three FFI apps all funnel through this one free fn, so wiring it
    /// here — the same `client()` pattern `FfiSnapshotsClient`/
    /// `FfiFoldersClient` already use — closes all three at once instead
    /// of three separate per-app calls.
    ///
    /// Mutation: drop the wiring below → `has_resolver()`/`has_owner_key()`
    /// both false → this pin reds (`cargo test -p fauna-ffi --lib`).
    #[cfg(all(feature = "folders-author", not(target_arch = "wasm32")))]
    #[test]
    fn the_builder_hands_the_machine_a_resolver_wired_custody() {
        let nest = FfiNestClient::new("wss://unreachable.invalid".into(), vec![7u8; 32]).unwrap();
        let observer: Arc<dyn DevicesObserver> = Arc::new(NullObserver);
        let machine = build_devices_machine(nest, observer);
        let custody = machine.label_custody();
        assert_eq!(
            (custody.has_resolver(), custody.has_owner_key()),
            (true, true),
            "the conflict-list render funnel needs BOTH arms: the resolver so a bound \
             set's paths open under the M2 generation its roster can, and the owner key \
             so an unbound set still renders",
        );
    }

    /// The `→public` audience flip apple / android / windows land through this
    /// machine must carry the owner's signed attestation, or no verifying seat
    /// ever unseals the folder (`encryption-at-rest.md` § Readable classes →
    /// *The declassification is owner-ATTESTED*). Ungated, unlike custody
    /// above: the attestor needs only the identity key the connection holds.
    ///
    /// Mutation: drop the `set_audience_attestor` wiring → this reds
    /// (`cargo test -p fauna-ffi --lib`).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_builder_hands_the_machine_the_audience_attestor() {
        let nest = FfiNestClient::new("wss://unreachable.invalid".into(), vec![7u8; 32]).unwrap();
        let observer: Arc<dyn DevicesObserver> = Arc::new(NullObserver);
        let machine = build_devices_machine(nest, observer);
        assert!(
            machine.has_audience_attestor(),
            "a key-authenticated connection must hand the machine its identity key, \
             so the seam signs the flip"
        );
    }

    /// The fleet-scope removal door every android/windows/macOS/iOS build
    /// funnels through this one seam (`account_runtime_fleet_removal`'s doc) —
    /// wired unconditionally under `account-runtime`, so the built machine
    /// must actually carry it. `account_runtime`'s own tests install a store
    /// into the process-global host, so this one takes a turn on it with
    /// nothing installed and the refusal is asserted unconditionally rather
    /// than merely "if unwired".
    ///
    /// Mutation: drop the `machine.set_fleet_removal(...)` wiring in
    /// [`build_devices_machine`] → `fleet_removal()` answers `None` → this pin
    /// reds (`cargo test -p fauna-ffi --lib`).
    #[cfg(feature = "account-runtime")]
    #[tokio::test]
    async fn the_builder_hands_the_machine_the_fleet_removal_door() {
        let _host = crate::account_runtime::tests::host_with_no_runtime().await;
        let nest = FfiNestClient::new("wss://unreachable.invalid".into(), vec![7u8; 32]).unwrap();
        let observer: Arc<dyn DevicesObserver> = Arc::new(NullObserver);
        let machine = build_devices_machine(nest, observer);
        let door = machine
            .fleet_removal()
            .expect("account-runtime build must wire the fleet-removal door");
        let refusal = door
            .resolve_removal("aa", Some([0x11; 32]))
            .await
            .expect_err("no runtime installed in this process, so nothing may be removed");
        assert!(matches!(
            refusal,
            fauna_devices_machine::FleetRemovalRefusal::Unavailable(_)
        ));
    }

    /// With no enrolled row to read (no runtime assembled yet, or a build
    /// without one) the badge falls back to the app's own id — the shared
    /// rule's fallback half (`devices.md` § This-device marker), never a
    /// blank badge on a single-app box. An empty own id marks nothing.
    #[test]
    fn no_enrolled_row_falls_back_to_the_apps_own_id() {
        assert_eq!(
            this_device_row_over(None, Some("ownid".into())),
            Some("ownid".to_string())
        );
        assert_eq!(this_device_row_over(None, Some(String::new())), None);
        assert_eq!(this_device_row_over(None, None), None);
    }

    /// The defect this door closes on windows/android/apple: an enrollment
    /// that converged onto a co-located agent's row names a row the app's own
    /// id does not — the badge must follow the enrollment.
    #[test]
    fn the_enrolled_row_wins_over_the_apps_own_id() {
        assert_eq!(
            this_device_row_over(Some("agentrow".into()), Some("ownid".into())),
            Some("agentrow".to_string())
        );
    }

    /// One answer per row, in order, through the shared rule — including its
    /// principal-less-row fail-safe.
    #[test]
    fn keyless_posture_answers_each_row_in_order() {
        let keyed: std::collections::BTreeSet<[u8; 32]> = [[1u8; 32]].into_iter().collect();
        let rows = vec![
            Some(fauna_core::hex32::encode(&[1u8; 32])),
            Some(fauna_core::hex32::encode(&[2u8; 32])),
            None,
        ];
        assert_eq!(
            keyless_posture_over(Some(&keyed), &rows),
            vec![false, true, false]
        );
    }

    /// No runtime assembled in a unit test → no resolved tip → no row is
    /// marked, and the answer still has one entry per row.
    #[tokio::test]
    async fn keyless_posture_without_a_store_marks_nothing() {
        #[cfg(feature = "account-runtime")]
        let _host = crate::account_runtime::tests::host_with_no_runtime().await;
        let rows = vec![Some(fauna_core::hex32::encode(&[2u8; 32])), None];
        assert_eq!(devices_keyless_posture(rows).await, vec![false, false]);
    }
}
