//! e2e-only on-demand READERS over the driver's handle — one implementation
//! for every app that hosts the account runtime, web included (priority #2).
//!
//! They live here, beside [`AccountStoreHandle`], because this crate compiles
//! for wasm32 and the native assembly (`fauna-client-account-runtime`) never
//! will (`docs/goal/architecture/account-client-lifecycle.md` § The
//! client-side lifecycle → *The trigger fired*, ruling (4)). The native crate
//! keeps its `device_set_state_json` at its old path as a JSON wrapper over
//! [`device_set_state`], so two apps cannot publish two shapes of one
//! cross-app contract.
//!
//! **Gated** (convention 15 rule (a)): each returns real plane content, so it
//! compiles only into debug builds or under this crate's `e2e-agent` feature,
//! which a consumer forwards from its own opt-in (rule (b)) — web's release
//! e2e build through `fauna-wasm`'s `test-helpers`.

use serde::Serialize;

use super::AccountStoreHandle;

/// One device's `fauna.state.device-set` plane row as the e2e assertion reads
/// it — `{"found": false}`, or `found: true` with `state: "removed"` plus who
/// removed it and when, or `state: "enrolled"` plus the enrollment stamp. The
/// "still enrolled" and "removed" states stay distinct: the assertion must tell
/// them apart, not just learn that a row exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceSetStateView {
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed_at_ms: Option<i64>,
    /// The removing writer's 32-byte id, hex.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enrolled_at_ms: Option<i64>,
}

impl DeviceSetStateView {
    const NOT_FOUND: Self = Self {
        found: false,
        state: None,
        removed_at_ms: None,
        removed_by: None,
        enrolled_at_ms: None,
    };
}

/// The plane's raw `fauna.state.device-set` record for `device_id_hex` (a
/// device's granted principal, hex — the same fleet id
/// `AccountStoreHandle::remove_fleet_member` takes), if any: the
/// convergence-assertion read half of the devices page's removals
/// (`docs/goal/behavior/devices.md` § Removing a Device).
///
/// Reads through [`AccountStoreHandle::states_of_kind`], a production door,
/// so this is a decode + filter above an existing read, never a new path into
/// the store. **Not a command**: it does async store I/O, so it never joins a
/// per-tick state blob (convention 11's corollary) — each hosting app calls it
/// from its async command dispatcher, keyed on the device id the test asks
/// about. No handle, a failed read or an absent row is `found: false`.
pub async fn device_set_state(
    handle: Option<&AccountStoreHandle>,
    device_id_hex: &str,
) -> DeviceSetStateView {
    let Some(handle) = handle else {
        return DeviceSetStateView::NOT_FOUND;
    };
    let Ok(rows) = handle
        .states_of_kind(fauna_protocol::merge_policy::KIND_DEVICE_SET)
        .await
    else {
        return DeviceSetStateView::NOT_FOUND;
    };
    match rows.iter().find(|e| e.key == device_id_hex) {
        Some(row) => device_set_record(&row.value),
        None => DeviceSetStateView::NOT_FOUND,
    }
}

/// Decode one row's value. Split out so the decode is unit-testable without an
/// assembled store. Undecodable bytes report absent rather than panicking.
fn device_set_record(row_value: &[u8]) -> DeviceSetStateView {
    use fauna_core::generation::DeviceSetRecord;
    match fauna_core::encoding::canonical_decode::<DeviceSetRecord>(row_value) {
        Ok(DeviceSetRecord::Removed {
            removed_at_ms,
            removed_by,
        }) => DeviceSetStateView {
            found: true,
            state: Some("removed"),
            removed_at_ms: Some(removed_at_ms),
            removed_by: Some(fauna_core::hex32::encode(&removed_by)),
            ..DeviceSetStateView::NOT_FOUND
        },
        Ok(DeviceSetRecord::Enrolled { enrolled_at_ms, .. }) => DeviceSetStateView {
            found: true,
            state: Some("enrolled"),
            enrolled_at_ms: Some(enrolled_at_ms),
            ..DeviceSetStateView::NOT_FOUND
        },
        Err(_) => DeviceSetStateView::NOT_FOUND,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::generation::DeviceSetRecord;

    #[test]
    fn a_removed_row_decodes_to_the_removed_shape() {
        let removed_by = [7u8; 32];
        let value = fauna_core::encoding::canonical_encode(&DeviceSetRecord::Removed {
            removed_at_ms: 42,
            removed_by,
        })
        .expect("encode");
        let view = device_set_record(&value);
        assert_eq!(view.state, Some("removed"));
        assert_eq!(view.removed_at_ms, Some(42));
        assert_eq!(
            view.removed_by,
            Some(fauna_core::hex32::encode(&removed_by))
        );
        assert!(view.found && view.enrolled_at_ms.is_none());
    }

    #[test]
    fn an_enrolled_row_decodes_to_the_enrolled_shape() {
        let value = fauna_core::encoding::canonical_encode(&DeviceSetRecord::Enrolled {
            xwing_pubkey: vec![1, 2, 3],
            authorization: vec![4, 5, 6],
            enrolled_at_ms: 7,
            device_sig: Vec::new(),
        })
        .expect("encode");
        let view = device_set_record(&value);
        assert_eq!(view.state, Some("enrolled"));
        assert_eq!(view.enrolled_at_ms, Some(7));
        assert!(view.found && view.removed_by.is_none());
    }

    #[test]
    fn undecodable_bytes_report_not_found() {
        assert_eq!(
            device_set_record(b"not dag-cbor"),
            DeviceSetStateView::NOT_FOUND
        );
    }
}
