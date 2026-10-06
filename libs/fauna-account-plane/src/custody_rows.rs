//! The custody registry rows' production writer (W8.4 (account-data-plane.md § Workstreams)) — typed door-puts
//! for `fauna.state.custodian-endpoints` (owner side) and
//! `fauna.state.custodies-held` (custodian side), mirrored on
//! [`crate::device_endpoints_writer`]'s equivalent pump — both ride the
//! shared last-writer-wins stamp under this writer, through the fleet
//! plane's REAL R14 (account-data-plane.md § The ratified decisions) writer door (both kinds
//! are `GenerationTip`-sealed — a put while no tip resolves is refused at
//! the door and stays owed at the ceremony driver, which retries). Each put
//! is the **local write only**
//! ([`crate::account_state_plane::put_lww_row_local`]): the row is durable
//! and stamped when it answers, and the account runtime's publish step
//! ships it — so a put answers inside a pass in flight
//! (`account-data-plane.md` § The client-side lifecycle, the pump bullet →
//! *Commands and passes*).
//!
//! Exposed through `fauna_sync_engine::account_runtime::AccountStoreHandle`'s typed
//! puts so the ceremony glue (`fauna_client_capabilities::custody_ceremony`'s
//! `CustodyRegistryWriter` seam) never touches a plane handle directly —
//! same discipline as every other store access. Authority:
//! `docs/goal/architecture/account-data-plane.md` § Replica posture → *The
//! custody grant + ceremony* (ceremony step 3 / the custodian's
//! "custodies held" record).

use anyhow::{Result, bail};
use fauna_account_store::backend::StoreBackend;
use fauna_core::custodian_endpoints::CustodianEndpoints;
use fauna_core::custodies_held::CustodyHeld;
use fauna_core::custody_grant::{CUSTODY_GRANT_ID_LEN, custody_entry_key};
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::{KIND_CUSTODIAN_ENDPOINTS, KIND_CUSTODIES_HELD};

use crate::account_state_plane::{AccountStatePlane, put_lww_row_local};

/// Which `custodian-endpoints` rows a round of observed peer candidates
/// (T13 step 4) rewrites — the decision half of the refresh, separated from
/// the door so the rule below is directly testable.
///
/// **A row is rewritten only when the observation's key is the node that row
/// ALREADY names.** The re-exchange therefore moves only *where* a known
/// custodian is reachable, never *who* the custodian is — that stays a
/// ceremony act (step 3's mint). Rows nobody was seen for are left alone: a
/// quiet pass must never erase the only address held for a custodian, and an
/// unchanged value is not re-put (an LWW stamp per pass would be pure churn).
pub fn custodian_rows_to_refresh(
    rows: impl IntoIterator<Item = CustodianEndpoints>,
    observed: &std::collections::HashMap<[u8; 32], fauna_core::device_endpoints::DeviceEndpoints>,
) -> Vec<CustodianEndpoints> {
    rows.into_iter()
        .filter_map(|mut row| {
            let fresh = observed.get(&row.endpoints.node_id)?;
            if &row.endpoints == fresh {
                return None;
            }
            row.endpoints = fresh.clone();
            Some(row)
        })
        .collect()
}

/// The custodian-side twin of [`custodian_rows_to_refresh`]: fold observed
/// candidates into each held custody's `owner_devices`.
///
/// Same rule, same reason — only a device the row ALREADY names is moved, so
/// admitting a peer can never add or redirect an owner device. This exists
/// beside the dial-reply path because the two see different sessions: a
/// custodian whose OWN dials all fail (every stored owner address stale) is
/// reached only inbound, and without this it could never learn the fresh
/// addresses that would let it dial again.
pub fn held_rows_to_refresh(
    rows: impl IntoIterator<Item = CustodyHeld>,
    observed: &std::collections::HashMap<[u8; 32], fauna_core::device_endpoints::DeviceEndpoints>,
) -> Vec<CustodyHeld> {
    rows.into_iter()
        .filter_map(|mut row| {
            let mut moved = false;
            for device in &mut row.owner_devices {
                if let Some(fresh) = observed.get(&device.node_id)
                    && device != fresh
                {
                    *device = fresh.clone();
                    moved = true;
                }
            }
            moved.then_some(row)
        })
        .collect()
}

/// Write the owner-side `fauna.state.custodian-endpoints` entry for one
/// custody — keyed by the grant id, so re-puts (ceremony re-drives,
/// per-session candidate refreshes) converge on one row.
pub async fn put_custodian_endpoints<B, R>(
    fleet: &AccountStatePlane<'_, B, R>,
    device_id: [u8; 32],
    value: &CustodianEndpoints,
) -> Result<u64>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    if value.grant_id.len() != CUSTODY_GRANT_ID_LEN {
        bail!("custodian-endpoints row needs a {CUSTODY_GRANT_ID_LEN}-byte grant id");
    }
    put_lww_row_local(
        fleet,
        KIND_CUSTODIAN_ENDPOINTS,
        &custody_entry_key(&value.grant_id),
        fauna_core::encoding::canonical_encode(value)?.to_vec(),
        device_id,
    )
    .await
}

/// Write one `fauna.state.share-endpoints` entry — the third member of the
/// location-data family this module's two custody puts belong to
/// (`fauna_core::share_endpoints`, the share leg's cached discovery row).
/// Keyed by [`fauna_core::share_endpoints::share_entry_key`] over the row's
/// OWN ids: the caller must hand a row that already passed
/// `fauna_peer_share::bind_share_advertisement` (which refuses rather than
/// repairs a claim naming anyone but the channel-proven sender), so deriving
/// the key from the bound row keeps key and value structurally consistent.
pub async fn put_share_endpoints<B, R>(
    fleet: &AccountStatePlane<'_, B, R>,
    device_id: [u8; 32],
    value: &fauna_core::share_endpoints::ShareEndpoints,
) -> Result<u64>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let (Ok(channel), Ok(member)) = (
        <[u8; 32]>::try_from(value.channel_id.as_slice()),
        <[u8; 32]>::try_from(value.member_actor.as_slice()),
    ) else {
        bail!("share-endpoints row needs 32-byte channel + member ids");
    };
    put_lww_row_local(
        fleet,
        fauna_protocol::merge_policy::KIND_SHARE_ENDPOINTS,
        &fauna_core::share_endpoints::share_entry_key(&channel, &member),
        fauna_core::encoding::canonical_encode(value)?.to_vec(),
        device_id,
    )
    .await
}

/// Write the custodian-side `fauna.state.custodies-held` entry for one held
/// custody — the custodian's own account plane (this runtime IS the
/// custodian's), keyed by the grant id.
pub async fn put_custodies_held<B, R>(
    fleet: &AccountStatePlane<'_, B, R>,
    device_id: [u8; 32],
    value: &CustodyHeld,
) -> Result<u64>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    if value.grant_id.len() != CUSTODY_GRANT_ID_LEN {
        bail!("custodies-held row needs a {CUSTODY_GRANT_ID_LEN}-byte grant id");
    }
    if value.witness.is_empty() {
        bail!("custodies-held row without a witness serves nothing — refuse before the door");
    }
    put_lww_row_local(
        fleet,
        KIND_CUSTODIES_HELD,
        &custody_entry_key(&value.grant_id),
        fauna_core::encoding::canonical_encode(value)?.to_vec(),
        device_id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::device_endpoints::DeviceEndpoints;
    use std::collections::HashMap;

    const CUSTODIAN: [u8; 32] = [0xC5; 32];
    const STRANGER: [u8; 32] = [0xEE; 32];

    fn endpoints(node_id: [u8; 32], port: u16) -> DeviceEndpoints {
        DeviceEndpoints {
            node_id,
            lan_addrs: vec![format!("192.168.1.9:{port}")],
            public_addrs: Vec::new(),
            relay_url: None,
        }
    }

    fn row(node_id: [u8; 32], port: u16) -> CustodianEndpoints {
        // Struct-update: this row grows (W8.7 added `latest_receipt`), and a
        // hand-listed fixture turns every growth into a merge conflict.
        CustodianEndpoints {
            grant_id: vec![0x1D; CUSTODY_GRANT_ID_LEN],
            endpoints: endpoints(node_id, port),
            ..Default::default()
        }
    }

    /// The refresh moves WHERE a known custodian is reachable — the row keeps
    /// its grant id and its node, and only the candidates change.
    #[test]
    fn a_seen_custodians_row_takes_the_fresh_candidates() {
        let observed = HashMap::from([(CUSTODIAN, endpoints(CUSTODIAN, 5555))]);
        let out = custodian_rows_to_refresh([row(CUSTODIAN, 1111)], &observed);
        assert_eq!(out, vec![row(CUSTODIAN, 5555)]);
    }

    /// …and never WHO the custodian is: an observation for a node no row
    /// names rewrites nothing, so admitting a peer cannot insert itself into
    /// somebody else's custody row (identity stays a ceremony act).
    #[test]
    fn an_observation_for_an_unnamed_node_rewrites_nothing() {
        let observed = HashMap::from([(STRANGER, endpoints(STRANGER, 5555))]);
        assert!(custodian_rows_to_refresh([row(CUSTODIAN, 1111)], &observed).is_empty());
    }

    /// A quiet pass leaves every row alone — a custodian that simply did not
    /// talk to us this pass must keep the only address we hold for it.
    #[test]
    fn a_pass_with_no_observations_erases_nothing() {
        let out = custodian_rows_to_refresh([row(CUSTODIAN, 1111)], &HashMap::new());
        assert!(out.is_empty(), "no observation ⇒ no write, never a wipe");
    }

    /// An unchanged value is not re-put: an LWW stamp every pass would be
    /// pure fleet churn for a custodian that simply has not moved.
    #[test]
    fn an_unchanged_value_is_not_rewritten() {
        let observed = HashMap::from([(CUSTODIAN, endpoints(CUSTODIAN, 1111))]);
        assert!(custodian_rows_to_refresh([row(CUSTODIAN, 1111)], &observed).is_empty());
    }

    const OWNER_A: [u8; 32] = [0xA1; 32];
    const OWNER_B: [u8; 32] = [0xB2; 32];

    fn held(devices: &[DeviceEndpoints]) -> CustodyHeld {
        CustodyHeld {
            grant_id: vec![0x1D; CUSTODY_GRANT_ID_LEN],
            owner: [0x0A; 32],
            witness: vec![0xEE; 8],
            owner_devices: devices.to_vec(),
            owner_nest_url: None,
            retained_bytes_cap: 42,
            ..Default::default()
        }
    }

    /// The custodian-side twin: an owner device seen this pass moves, its
    /// siblings in the same row are untouched. This is the path that saves a
    /// custodian reachable only INBOUND — its own dials cannot teach it
    /// anything, so the addresses it holds would stay stale forever.
    #[test]
    fn a_seen_owner_device_moves_and_its_siblings_do_not() {
        let observed = HashMap::from([(OWNER_A, endpoints(OWNER_A, 5555))]);
        let out = held_rows_to_refresh(
            [held(&[endpoints(OWNER_A, 1111), endpoints(OWNER_B, 2222)])],
            &observed,
        );
        assert_eq!(
            out,
            vec![held(&[endpoints(OWNER_A, 5555), endpoints(OWNER_B, 2222)])]
        );
    }

    /// An observation for a device the row does not name adds nobody — a
    /// custody row's device set is a ceremony artifact, not a wire claim.
    #[test]
    fn an_unnamed_device_is_never_added_to_a_held_row() {
        let observed = HashMap::from([(STRANGER, endpoints(STRANGER, 5555))]);
        assert!(
            held_rows_to_refresh([held(&[endpoints(OWNER_A, 1111)])], &observed).is_empty(),
            "no row rewritten, and certainly no device appended"
        );
    }

    /// Nothing moved ⇒ nothing written, on this side too.
    #[test]
    fn an_unchanged_held_row_is_not_rewritten() {
        let observed = HashMap::from([(OWNER_A, endpoints(OWNER_A, 1111))]);
        assert!(held_rows_to_refresh([held(&[endpoints(OWNER_A, 1111)])], &observed).is_empty());
    }
}
