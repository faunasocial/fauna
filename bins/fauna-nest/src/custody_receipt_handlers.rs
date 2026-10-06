//! The custody-receipt doors — stage (c) of the custodian-nest runtime
//! (`account-data-plane.md` § Replica posture → The custody grant + ceremony,
//! the device-or-nest bullet, item 6): `fauna.custody.receipt.deposit`
//! (CUSTODIAN class — the custodian nest's pump, over the same
//! custody-handshake bearer its pull rides) and `fauna.custody.receipt.list`
//! (User class — the owner's own fleet fetching at sync).
//!
//! **The deposit door verifies BEFORE staging** (the gotcha: a lying
//! deposit is refused at the door, not at the fleet fold): the live
//! capability row for `(owner, grant_id)` is re-derived per request — exactly
//! the `custody_auth_core` recipe, so a revoke severs deposits at the very
//! next call — the row must be custody-class, name THE CALLER as holder, and
//! be inside both window bounds; and the receipt's signature is verified
//! against the ROW's holder key, with the receipt's own owner and grant id
//! required to match the request's. Staging is latest-per-grant, monotone in
//! `attested_at` (`crate::db::custody_receipts` owns the SQL); the fleet
//! re-verifies against its recorded accept at fold, exactly as the
//! channel-carried receipts today.

use std::time::Duration;

use fauna_protocol::custody::{
    RECEIPT_DEPOSIT_KIND, RECEIPT_LIST_KIND, ReceiptDepositReply, ReceiptDepositRequest,
    ReceiptItem, ReceiptListReply, ReceiptListRequest,
};
use fauna_protocol::decode_strict as decode;

use crate::bridge_routing_handlers::{encode_reply, internal, malformed, require_class};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

fn parse_owner_hex(hex_str: &str) -> Result<[u8; 32], fauna_protocol::RpcError> {
    let bytes =
        hex::decode(hex_str).map_err(|_| malformed("owner_actor_id must be hex".to_string()))?;
    bytes
        .try_into()
        .map_err(|_| malformed("owner_actor_id must be 32 bytes".to_string()))
}

fn receipt_deposit_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, RECEIPT_DEPOSIT_KIND).await?;
            let req: ReceiptDepositRequest = decode(&payload).map_err(malformed)?;
            let owner = parse_owner_hex(&req.owner_actor_id)?;

            // The LIVE capability row, re-derived per request (T13's
            // nest-side revocation store; the custody_auth_core recipe): must
            // exist for exactly (owner, grant_id), be custody-class, name THE
            // CALLER as holder, and be inside both window bounds. All
            // refusals collapse to one message — shape probing learns
            // nothing, matching the handshake's own posture.
            let refuse = || malformed("no custody grant".to_string());
            let row = state
                .db
                .get_capability_grant(&owner, &req.grant_id)
                .await
                .map_err(internal)?
                .ok_or_else(refuse)?;
            let blob = fauna_mls::wrapped_blob::GrantBlob::from_canonical_bytes(&row)
                .map_err(|_| refuse())?;
            let now_secs = fauna_core::data::Timestamp::now_millis() / 1000;
            let is_custody = !blob.scope.is_empty()
                && blob
                    .scope
                    .iter()
                    .all(|t| t.class == fauna_mls::wrapped_blob::ScopeTuple::CLASS_CUSTODY);
            if !is_custody
                || blob.holder.as_slice() != actor_id.as_slice()
                || !fauna_mls::wrapped_blob::grant_window_is_open(
                    &blob,
                    i64::try_from(now_secs).unwrap_or(i64::MAX),
                )
            {
                return Err(refuse());
            }
            let holder: [u8; 32] = blob.holder.as_slice().try_into().map_err(|_| refuse())?;

            // The signature, against the ROW's holder key — at the door.
            let envelope: fauna_core::encoding::EmbedAsBytes =
                fauna_core::encoding::canonical_decode(&req.receipt)
                    .map_err(|e| malformed(format!("receipt envelope: {e}")))?;
            let receipt = fauna_core::custody_receipt::verify_custody_receipt(&envelope, &holder)
                .map_err(|e| malformed(format!("receipt refused: {e}")))?;
            if receipt.grant_id != req.grant_id.as_slice() {
                return Err(malformed(
                    "receipt names a grant other than the deposit's".to_string(),
                ));
            }
            if receipt.owner != owner {
                return Err(malformed(
                    "receipt names an owner other than the deposit's".to_string(),
                ));
            }

            let staged = state
                .db
                .stage_custody_receipt(&owner, &req.grant_id, &req.receipt, receipt.attested_at.0)
                .await
                .map_err(internal)?;
            encode_reply(&ReceiptDepositReply {
                ok: true,
                staged,
                extra: Default::default(),
            })
        })
    })
}

fn receipt_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, RECEIPT_LIST_KIND).await?;
            let _req: ReceiptListRequest = decode(&payload).map_err(malformed)?;
            let rows = state
                .db
                .list_custody_receipts(&actor_id)
                .await
                .map_err(internal)?;
            encode_reply(&ReceiptListReply {
                rows: rows
                    .into_iter()
                    .map(|r| ReceiptItem {
                        grant_id: serde_bytes::ByteBuf::from(r.grant_id),
                        receipt: serde_bytes::ByteBuf::from(r.receipt),
                        attested_at: r.attested_at,
                        staged_at: r.staged_at.max(0) as u64,
                        extra: Default::default(),
                    })
                    .collect(),
                extra: Default::default(),
            })
        })
    })
}

pub fn register_custody_receipt_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        RECEIPT_DEPOSIT_KIND,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: receipt_deposit_handler(),
        },
    );
    b.add(
        RECEIPT_LIST_KIND,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: receipt_list_handler(),
        },
    );
}
