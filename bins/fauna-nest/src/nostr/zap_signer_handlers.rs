//! WS-RPC handlers for the NIP-57 zap trust root —
//! `fauna.nostr.zap_signers.{list,add,remove}`
//! (`docs/goal/behavior/monetization.md` § Zap receipts — the trust model).
//!
//! A kind-9735 zap receipt is signed by the *recipient's* LNURL/wallet
//! server, not the sender, and is plain signed JSON anyone may mint naming
//! any recipient — so its own signature proves nothing about payment. This
//! roster is what turns a receipt into a claim worth believing: each payee
//! designates which signer pubkey(s) may speak for their money. Which
//! wallet/LNURL provider a payee trusts is a genuine user choice, so it is
//! app UI + nest state, never a config file (§ Product invariants).
//!
//! **The empty roster is the default and it is load-bearing**: a payee who
//! has designated nobody believes nobody, so a fresh nest silently believes
//! no zap at all. Adding a signer is the opt-in.
//!
//! All three kinds are **User-class, caller-scoped**: every query keys on the
//! authenticated connection's actor (`hex::encode(actor_id)`), never a
//! request field — the `bunker_handlers` contract. Caller-class
//! enforcement lives in `bridge_method_allowlist::is_permitted` (User-only
//! arms). This module is the transport shell; the storage primitives are
//! `crate::nostr::db`, and the verdict that consumes the roster is the pure
//! `fauna_bridge_nostr::nip57::classify_zap_receipt`.

use std::time::Duration;

use fauna_protocol::nostr::{
    AddZapSignerReply, AddZapSignerRequest, ListZapSignersReply, ListZapSignersRequest,
    RemoveZapSignerReply, RemoveZapSignerRequest, ZapSignerEntry,
};
use fauna_protocol::{RpcError, decode_strict as decode};

use crate::nostr::db;
use crate::rpc_errors::internal;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

use crate::rpc_errors::{encode_reply, malformed};

/// A malformed signer pubkey — the same wire surface the bunker and DM
/// clusters use for caller-supplied argument failures.
fn invalid_params(reason: &str) -> RpcError {
    crate::rpc_errors::invalid_params_ns("nostr", reason)
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

fn entry(row: db::ZapSigner) -> ZapSignerEntry {
    ZapSignerEntry {
        id: row.id,
        signer_pubkey: row.signer_pubkey,
        label: row.label,
        created_at: row.created_at as u64,
        extra: Default::default(),
    }
}

// ── fauna.nostr.zap_signers.list ───────────────────────────────────

fn list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.nostr.zap_signers.list").await?;
            let _req: ListZapSignersRequest = decode(&payload).map_err(malformed)?;
            let actor_hex = hex::encode(actor_id);

            let conn = state.db.conn().await;
            let signers = db::list_zap_signers(&conn, &actor_hex).map_err(internal)?;
            drop(conn);

            encode_reply(&ListZapSignersReply {
                signers: signers.into_iter().map(entry).collect(),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.nostr.zap_signers.add ────────────────────────────────────

/// **Gate surface `zaps.signer.designate`** (`dynamic-features.md` § Charter
/// members — *"zap-signer designation … the trust-root enablement"*).
///
/// This is the opt-in the empty roster makes load-bearing: with no designated
/// signer the payee believes no receipt, so designating one is the act that
/// turns the zaps plane **on** for this account. Denying `zaps` — or `payments`,
/// which reaches here along the subset edge — must therefore refuse it, or a
/// denied account could still arm the trust root and only discover the deny at
/// ingest.
///
/// `remove` is deliberately ungated, the same de-escalation rule
/// `payments.providers.set`/`remove` follows: a tier that can only tighten must
/// never be able to freeze a user's trust root in place.
///
/// A designated signer is a wallet/LNURL service, not somebody the account
/// transacts with, so it introduces no counterparty and moves no value — the
/// zaps plane's counterparties are the senders counted at `receipt.ingest`.
fn add_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.nostr.zap_signers.add").await?;
            let req: AddZapSignerRequest = decode(&payload).map_err(malformed)?;
            let actor_hex = hex::encode(actor_id);

            crate::feature_gate::gate(
                &state,
                &actor_id,
                &fauna_core::feature_gate::GateOp {
                    feature: fauna_core::feature_gate::GatedFeature::Zaps,
                    surface: fauna_core::feature_gate::SURFACE_ZAPS_SIGNER_DESIGNATE,
                    new_counterparties: 0,
                    magnitude: 0,
                },
            )
            .await?;

            let conn = state.db.conn().await;
            // A malformed pubkey is refused loudly rather than stored: a
            // designation that can never match a real `event.pubkey` is
            // silently dead weight the payee believes they made, and would
            // present as "my zaps are ignored" against a correct-looking
            // roster.
            let row = db::add_zap_signer(&conn, &actor_hex, &req.signer_pubkey, &req.label)
                .map_err(|e| invalid_params(&e.to_string()))?;
            drop(conn);

            encode_reply(&AddZapSignerReply {
                signer: entry(row),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.nostr.zap_signers.remove ─────────────────────────────────

fn remove_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.nostr.zap_signers.remove").await?;
            let req: RemoveZapSignerRequest = decode(&payload).map_err(malformed)?;
            let actor_hex = hex::encode(actor_id);

            let conn = state.db.conn().await;
            let removed =
                db::remove_zap_signer(&conn, &actor_hex, &req.signer_pubkey).map_err(internal)?;
            drop(conn);

            encode_reply(&RemoveZapSignerReply {
                removed,
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ────────────────────────────────────────

/// All three are light reads/writes at 5 s and replay-safe: `add` is an
/// upsert on `(actor_id, signer_pubkey)` and `remove` a delete, so a
/// replayed envelope is idempotent (the bunker `revoke`/`set_label`
/// precedent).
pub fn register_nostr_zap_signer_handlers(b: &mut RpcRouterBuilder) {
    let light = || Duration::from_secs(5);
    b.add(
        "fauna.nostr.zap_signers.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: light(),
            handler: list_handler(),
        },
    );
    b.add(
        "fauna.nostr.zap_signers.add",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: light(),
            handler: add_handler(),
        },
    );
    b.add(
        "fauna.nostr.zap_signers.remove",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: light(),
            handler: remove_handler(),
        },
    );
}
