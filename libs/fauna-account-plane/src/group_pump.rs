//! The group pump leg's store half — what the share plane's pass needs from
//! this replica to pull a group scope from its other members
//! (`docs/goal/behavior/p2p-shared-set-build.md` § Build design — the group
//! set, decisions 6 and 8).
//!
//! The pass itself (`fauna_sync_engine::share_glue`) owns the seat: it dials,
//! admits, and hands the open channel back here, because the store lives on
//! the account runtime's own thread and the walk must run where the store
//! is. Three things cross that seam, each a plain read or a plane write:
//!
//! - **What to pull** ([`pump_scope`]): per held scope, this member's own
//!   `Enrolled` roster record — the witness its admit presents — and the
//!   other members' `fauna.group.endpoints` cells, the dial targets; for a
//!   scope this account initiated, the recipient's compare-code candidates
//!   from the ceremony record stand in for the recipient's cell until it has
//!   landed.
//! - **Where this member is** ([`own_endpoints_due`], [`own_endpoints_cell`]): the seat's own
//!   endpoints cell in every held scope, written when it is missing or its
//!   reachability changed.
//! - **The walk** ([`ChannelRequester`]): the open peer channel, type-erased
//!   to the one request shape [`crate::group_state_plane::GroupStatePlane`]'s
//!   walk makes of it, so this crate needs no peer transport of its own.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result};
use fauna_account_store::types::StateEntry;
use fauna_core::device_endpoints::DeviceEndpointsEntry;
use fauna_core::encoding::canonical_decode;
use fauna_core::group_ceremony::GroupShareConfig;
use fauna_core::group_endpoints::{
    GroupEndpointsCore, GroupEndpointsRecord, group_endpoints_cell_key,
};
use fauna_core::group_scope::GroupRosterRecord;
use fauna_core::identity::ActorId;
use fauna_protocol::group_state::{KIND_GROUP_ENDPOINTS, KIND_GROUP_ROSTER};
use fauna_protocol::merge_policy::LwwStamp;
use fauna_protocol::{RpcRequester, Value, decode_strict, encode_canonical};

use crate::group_set::GroupWriterCheck;

/// A peer channel, erased to the one call the group walk makes of it: one
/// request of `kind` carrying `payload`, answered by one reply value. The
/// share plane implements it over its `PeerChannel`.
pub trait GroupFeedChannel: Send + Sync {
    fn request_value<'a>(
        &'a self,
        kind: &'static str,
        payload: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;
}

/// An [`RpcRequester`] over a [`GroupFeedChannel`] — the peer requester's
/// shape (encode the request to a value, decode the reply from one), behind
/// the erased seam.
pub struct ChannelRequester(pub Arc<dyn GroupFeedChannel>);

impl RpcRequester for ChannelRequester {
    type Error = anyhow::Error;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let value: Value = decode_strict(&encode_canonical(&payload).context("encode request")?)
            .context("request as Value")?;
        let reply = self
            .0
            .request_value(kind, value)
            .await
            .with_context(|| format!("peer request {kind}"))?;
        decode_strict(&encode_canonical(&reply).context("encode reply")?).context("decode reply")
    }
}

/// One held scope as the pump pulls it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupPumpScope {
    pub scope_id: [u8; 32],
    /// This member's own `Enrolled` roster record, verbatim — the witness its
    /// admit presents.
    pub witness: Vec<u8>,
    /// The other members' published endpoints, one per member device that
    /// wrote a cell. The entry's `node_id` is the member's ACTOR key: the
    /// cross-user seat is actor-keyed.
    pub peers: Vec<DeviceEndpointsEntry>,
}

/// Read one held scope's opened rows into what the pump pulls, or `None` when
/// this member holds no live `Enrolled` entry in it (never admitted, or
/// removed): a member with no witness has nothing to present and is not
/// served, so it dials nobody.
///
/// A peer cell is used only when it passes the cell's writer check
/// ([`fauna_core::group_endpoints::verify_group_endpoints`], through
/// [`crate::group_set::GroupWriterCheck::verify_endpoints`]) and its writer
/// is a live member other than this one: the cell sits under its writer
/// device's key, the device chains to the authority or to a live member, the
/// entry names that member's own actor, and the signature and stamp are the
/// device's. Anything else would let one member steer the others' dials at
/// a stranger, or at a co-member's node over an address of its
/// choosing. The walk refuses the same rows before
/// the merge; this is the second line, as the folds are for the file and
/// name kinds.
///
/// `ceremonies` is this account's ceremony fold
/// ([`crate::group_share_rows::group_shares_of`]). For a scope this account
/// initiated, the recipient's compare-code candidates kept in the record
/// become one more peer — `node_id` the recipient's actor, the candidates as
/// its LAN addresses — while no cell naming the recipient has landed: the
/// walk is pull-only, so until the initiator's first pull of the recipient
/// nothing else says where the recipient is (`p2p-shared-set-build.md`
/// § Build design — the group set, decision 8). The recipient must hold a
/// live entry, as for a cell; once its own cell lands, the cell alone is
/// dialed.
pub fn pump_scope(
    scope_id: [u8; 32],
    rows: &[StateEntry],
    own_actor: &ActorId,
    ceremonies: &GroupShareConfig,
) -> Option<GroupPumpScope> {
    let mut witness = None;
    let mut members = std::collections::BTreeSet::new();
    for row in rows
        .iter()
        .filter(|r| r.kind == KIND_GROUP_ROSTER && !r.tombstone)
    {
        if let Ok(GroupRosterRecord::Enrolled { core, .. }) = canonical_decode(&row.value) {
            if core.member_actor == *own_actor {
                witness = Some(row.value.clone());
            } else {
                members.insert(core.member_actor.0);
            }
        }
    }
    let witness = witness?;
    let check = GroupWriterCheck::from_rows(&scope_id, rows);
    if check.is_none() {
        tracing::warn!(
            scope = %fauna_core::hex32::encode(&scope_id),
            "group pump: this replica holds no readable birth row for the scope — no cell is a \
             dial target"
        );
    }
    let mut peers: Vec<DeviceEndpointsEntry> = rows
        .iter()
        .filter(|r| r.kind == KIND_GROUP_ENDPOINTS && !r.tombstone)
        .filter_map(|r| {
            let honoured = match check
                .as_ref()?
                .verify_endpoints(&r.key, &r.value, r.merge_meta.as_deref())
            {
                Ok(honoured) => honoured,
                Err(why) => {
                    tracing::warn!(key = %r.key, "group endpoints cell is not a dial target: {why}");
                    return None;
                }
            };
            members
                .contains(&honoured.member.0)
                .then_some(honoured.record.core.entry)
        })
        .collect();
    for record in ceremonies.initiated.iter().filter(|r| {
        r.scope_id == scope_id
            && !r.recipient_lan_addrs.is_empty()
            && members.contains(&r.recipient.0)
    }) {
        if !peers.iter().any(|p| p.node_id == record.recipient.0) {
            peers.push(DeviceEndpointsEntry {
                node_id: record.recipient.0,
                lan_addrs: record.recipient_lan_addrs.clone(),
                ..Default::default()
            });
        }
    }
    Some(GroupPumpScope {
        scope_id,
        witness,
        peers,
    })
}

/// Whether this device's cell in a scope must be (re)written: absent, or its
/// reachability differs from `entry`. A cell whose facts are unchanged is
/// left alone — a member-pulled plane has no expiry, so rewriting the same
/// facts on every advertisement would only grow every member's log. A cell
/// under this device's key that does not decode as the cell's record (a
/// displacement forgery the walk let through, or a pre-carriage write) is
/// rewritten.
pub fn own_endpoints_due(
    rows: &[StateEntry],
    own_device_hex: &str,
    entry: &DeviceEndpointsEntry,
) -> bool {
    let current = rows
        .iter()
        .find(|r| r.kind == KIND_GROUP_ENDPOINTS && r.key == own_device_hex && !r.tombstone)
        .and_then(|r| canonical_decode::<GroupEndpointsRecord>(&r.value).ok())
        .map(|record| record.core.entry);
    current.as_ref() != Some(entry)
}

/// The item, value and stamp of this device's own endpoints cell in
/// `scope_id`: the record signed by `device` carrying `authorization` (its
/// `DeviceAuthorization` carriage), stamped with the device's own instant.
///
/// # Errors
/// The carriage does not decode or certifies another device than the one
/// signing; or the record does not encode.
pub fn own_endpoints_cell(
    scope_id: &[u8; 32],
    device: &ed25519_dalek::SigningKey,
    authorization: Vec<u8>,
    entry: &DeviceEndpointsEntry,
    at_ms: i64,
) -> Result<(crate::account_state_plane::ItemId, Vec<u8>, Vec<u8>)> {
    let own_device = device.verifying_key().to_bytes();
    let cert = fauna_core::group_scope::verify_device_cert_carriage(
        "endpoints cell authorization",
        &authorization,
        at_ms,
    )
    .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        cert.device_key == own_device,
        "the device authorization certifies another device than the one signing the cell"
    );
    let record = GroupEndpointsRecord::sign(
        GroupEndpointsCore {
            scope_id: *scope_id,
            entry: entry.clone(),
            stamped_at_ms: at_ms,
        },
        device,
        authorization,
    )
    .context("sign the endpoints cell")?;
    let value = fauna_core::encoding::canonical_encode(&record)
        .context("encode the endpoints cell")?
        .to_vec();
    let stamp = LwwStamp {
        at_ms,
        writer: own_device,
    }
    .encode()
    .context("encode the endpoints stamp")?;
    Ok((
        crate::account_state_plane::ItemId {
            kind: KIND_GROUP_ENDPOINTS.into(),
            key: group_endpoints_cell_key(&own_device),
        },
        value,
        stamp,
    ))
}
