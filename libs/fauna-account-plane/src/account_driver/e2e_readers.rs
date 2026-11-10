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

/// One fauna-native channel's read marker as the e2e assertion reads it —
/// `{"found": false}`, or `{"found": true, "through": N}` — beside the
/// channel's standing in this replica's departure state
/// ([`crate::departure`]): `subscribed` when the subscription marker names its
/// `conv` scope (a full pass derived it, so a later leave is concluded),
/// `departed` when the departed list does (a pass concluded the leave). The
/// two are the barriers a departure witness waits on — a join and a leave
/// between two passes leave nothing to conclude.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadMarkerStateView {
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub through: Option<u64>,
    /// Serialized only when set, so the bare marker shape reads unchanged.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub subscribed: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub departed: bool,
}

/// The account store's read marker for `channel_id_hex` — the e2e's durability
/// barrier for a read (`docs/goal/behavior/conversation-read-state.md` § The
/// read-marker record): a test that must know a read reached the store before
/// it quits the app asks this, never a clock. Built on
/// [`AccountStoreHandle::read_markers`], the same read the conversations glue
/// delivers from; the channel id compares case-insensitively. An on-demand
/// reader exactly like [`device_set_state`] (async store I/O, never on the
/// per-tick state path). No handle, a failed read or an absent row is
/// `found: false`.
pub async fn read_marker_state(
    handle: Option<&AccountStoreHandle>,
    channel_id_hex: &str,
) -> ReadMarkerStateView {
    let Some(handle) = handle else {
        return ReadMarkerStateView::NOT_FOUND;
    };
    let mut view = match handle.read_markers().await {
        Ok(markers) => read_marker_of(&markers, channel_id_hex),
        Err(_) => ReadMarkerStateView::NOT_FOUND,
    };
    if let Some(scope) = conv_scope_of(channel_id_hex) {
        // The departure state's own `store_meta` lists, read through the
        // handle's meta door — read-only: the departure step owns both keys.
        let subscribed = handle.meta_get(crate::departure::META_SUBSCRIBED).await;
        view.subscribed = scope_list_names(subscribed.ok().flatten().as_deref(), &scope);
        let departed = handle.meta_get(crate::departure::META_DEPARTED).await;
        view.departed = scope_list_names(departed.ok().flatten().as_deref(), &scope);
    }
    view
}

/// The `conv` member scope of `channel_id_hex` (any case), as the departure
/// lists spell it; `None` for anything but 64 hex digits.
fn conv_scope_of(channel_id_hex: &str) -> Option<String> {
    let mut channel = [0u8; 32];
    hex::decode_to_slice(channel_id_hex, &mut channel).ok()?;
    fauna_protocol::scope::ContentScope::new(crate::scope_set::CONV_KIND, channel)
        .ok()
        .map(|scope| scope.to_string())
}

/// Does a newline-framed departure list ([`crate::departure`]) name `scope`?
fn scope_list_names(raw: Option<&[u8]>, scope: &str) -> bool {
    raw.is_some_and(|raw| {
        String::from_utf8_lossy(raw)
            .lines()
            .any(|line| line == scope)
    })
}

/// The pure lookup, split out so it is unit-testable without an assembled store.
fn read_marker_of(markers: &[(String, u64)], channel_id_hex: &str) -> ReadMarkerStateView {
    match markers
        .iter()
        .find(|(channel, _)| channel.eq_ignore_ascii_case(channel_id_hex))
    {
        Some((_, through)) => ReadMarkerStateView {
            found: true,
            through: Some(*through),
            ..ReadMarkerStateView::NOT_FOUND
        },
        None => ReadMarkerStateView::NOT_FOUND,
    }
}

impl ReadMarkerStateView {
    const NOT_FOUND: Self = Self {
        found: false,
        through: None,
        subscribed: false,
        departed: false,
    };
}

/// Where one of this replica's `fauna.state.escrow-wrap` rows sits on the
/// fleet feed: its logical key (`<generation-hex>/<actor-id-hex>`), the
/// blinded item key the nest knows it by, and every writer whose relay copy
/// this replica holds at that item key. The kind is gen-0 (form v1), so the
/// item key derives from the plane's own schedule
/// ([`AccountStatePlane::gen0_item_key`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EscrowWrapCoordinate {
    pub key: String,
    pub item_key_hex: String,
    pub writers_hex: Vec<String>,
}

/// Every live `fauna.state.escrow-wrap` row of this replica's merged state,
/// with its coordinates ([`EscrowWrapCoordinate`]) — the store-thread half of
/// [`AccountStoreHandle::escrow_wrap_coordinates`]. Coordinates only: the
/// wrap bytes never leave the store.
pub(crate) async fn escrow_wrap_coordinates<B, R>(
    store: &fauna_account_store::store::AccountStore<B>,
    fleet: &crate::account_state_plane::AccountStatePlane<'_, B, R>,
) -> anyhow::Result<Vec<EscrowWrapCoordinate>>
where
    B: fauna_account_store::backend::StoreBackend,
    R: fauna_protocol::RpcRequester,
{
    use fauna_protocol::merge_policy::KIND_ESCROW_WRAP;
    let mut out = Vec::new();
    for row in store.states_of_kind(KIND_ESCROW_WRAP).await? {
        let Some(item_key) = fleet.gen0_item_key(KIND_ESCROW_WRAP, &row.key) else {
            continue;
        };
        let writers_hex = fleet
            .relay_rows_at(&item_key)
            .await?
            .iter()
            .map(|r| fauna_core::hex32::encode(&r.writer.0))
            .collect();
        out.push(EscrowWrapCoordinate {
            key: row.key,
            item_key_hex: fauna_core::hex32::encode(&item_key),
            writers_hex,
        });
    }
    Ok(out)
}

/// [`AccountStoreHandle::escrow_wrap_coordinates`] as the e2e command answers
/// it — `{"found": true, "rows": [...]}`, or `{"found": false}` with no
/// handle or a failed read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EscrowWrapCoordinatesView {
    pub found: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<EscrowWrapCoordinate>,
}

/// The on-demand reader over [`AccountStoreHandle::escrow_wrap_coordinates`];
/// the hosting app calls it from its async command dispatcher, like
/// [`device_set_state`].
pub async fn escrow_wrap_coordinates_view(
    handle: Option<&AccountStoreHandle>,
) -> EscrowWrapCoordinatesView {
    let not_found = EscrowWrapCoordinatesView {
        found: false,
        rows: Vec::new(),
    };
    let Some(handle) = handle else {
        return not_found;
    };
    match handle.escrow_wrap_coordinates().await {
        Ok(rows) => EscrowWrapCoordinatesView { found: true, rows },
        Err(_) => not_found,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::generation::DeviceSetRecord;

    #[test]
    fn a_read_marker_is_found_case_insensitively() {
        let markers = vec![("ABcd".to_string(), 9u64)];
        let view = read_marker_of(&markers, "abCD");
        assert!(view.found);
        assert_eq!(view.through, Some(9));
        assert_eq!(
            serde_json::to_value(&view).expect("json"),
            serde_json::json!({ "found": true, "through": 9 })
        );
    }

    #[test]
    fn an_absent_read_marker_reports_not_found() {
        let view = read_marker_of(&[("aa".to_string(), 1)], "bb");
        assert_eq!(view, ReadMarkerStateView::NOT_FOUND);
        assert_eq!(
            serde_json::to_value(&view).expect("json"),
            serde_json::json!({ "found": false })
        );
    }

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

    #[test]
    fn the_departure_lists_name_a_channel_by_its_conv_scope() {
        let channel = "AB".repeat(32);
        let scope = conv_scope_of(&channel).expect("64 hex digits");
        assert_eq!(scope, conv_scope_of(&channel.to_lowercase()).unwrap());
        let list = format!("content:post:{}\n{scope}\n", "00".repeat(32));
        assert!(scope_list_names(Some(list.as_bytes()), &scope));
        assert!(!scope_list_names(Some(b"content:conv:zz"), &scope));
        assert!(!scope_list_names(None, &scope));
        assert_eq!(conv_scope_of("not-hex"), None);
    }
}
