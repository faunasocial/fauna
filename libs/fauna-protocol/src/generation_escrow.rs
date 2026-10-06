//! The generation **escrow doors** — R14 (account-data-plane.md § The ratified decisions) build step 4 (nest-side
//! requirement 4; contract owner: `account-data-plane.md` § The generation
//! machinery → *The escrow doors*).
//!
//! The v1 holder is the user's nest: `put` persists an identity-targeted
//! escrow wrap durably and returns the **holder-signed receipt** (the nest
//! signs with its deployment identity — the key clients already pin); `get`
//! serves the account's own wraps (an enrolled device, or a recovery-ceremony
//! session — both authenticate as the account, which is the whole gate);
//! `delete` is the per-generation crypto-shred half, user-gated at the
//! calling surface. All three are **User-class** and derive the account from
//! the authenticated connection — no request carries an actor id.
//!
//! The receipt's signed encoding and its holder-generic verification live in
//! `fauna_core::generation` (`sign_escrow_receipt` / `verify_escrow_receipt`)
//! — shared so clients and future non-nest holders (T16-era: another device,
//! a friend's nest) run the identical contract; nothing on this wire assumes
//! the deployment key.

use crate::Value;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

/// WS-RPC kind: deposit one escrow wrap; idempotent per
/// `(generation id, wrap hash)` — a byte-identical re-deposit returns a
/// byte-identical receipt (stamped with the FIRST deposit's instant), so a
/// crash-retrying minter never mints two receipt variants for one wrap.
pub const KIND_ESCROW_PUT: &str = "fauna.generation.escrow.put";
/// WS-RPC kind: list the calling account's escrow wraps (optionally one
/// generation's).
pub const KIND_ESCROW_GET: &str = "fauna.generation.escrow.get";
/// WS-RPC kind: delete one generation's wraps — the holder-side half of the
/// per-generation crypto-shred.
pub const KIND_ESCROW_DELETE: &str = "fauna.generation.escrow.delete";

/// Ceiling for one escrow wrap's ciphertext. An X-Wing envelope sealing a
/// 32-byte generation key is ~1.2 KiB; 64 KiB leaves format headroom without
/// admitting blob abuse (the `bridge_wrapped_mls_blobs` precedent).
pub const MAX_ESCROW_WRAP_BYTES: usize = 64 * 1024;

/// Deposit one escrow wrap for the calling account.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EscrowPutRequest {
    /// The 32-byte content-derived generation id.
    pub generation_id: ByteBuf,
    /// The X-Wing envelope sealing the generation key to the identity's
    /// published escrow target. Opaque to the holder by construction.
    pub wrap: ByteBuf,
    /// The escrow-target row's logical key the wrap was sealed under
    /// (`fauna_core::generation::escrow_target_identity_key` —
    /// `identity/<actor-id-hex>`). The holder signs it into the receipt, so a
    /// receipt acks a deposit for exactly one identity and a successor's
    /// re-escrow is never satisfied by a predecessor's receipt (the succession
    /// rider, 2026-09-28). Required — the door refuses it empty.
    #[serde(default)]
    pub target_key: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to [`EscrowPutRequest`]: the holder-signed durable receipt.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EscrowPutReply {
    /// Canonical dag-cbor bytes of a
    /// `fauna_core::generation::EscrowReceiptRecord`. The depositor verifies
    /// it (`verify_escrow_receipt` + its own holder pin) before writing the
    /// `fauna.state.escrow-receipt` plane entry — the row tip resolution
    /// checks.
    pub receipt: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// List the calling account's deposited wraps.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EscrowGetRequest {
    /// Restrict to one generation; absent lists every wrap the holder has for
    /// this account (the recovery ceremony's shape — it wants everything).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_id: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One deposited wrap in an [`EscrowGetReply`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EscrowWrapRow {
    /// The 32-byte generation id.
    pub generation_id: ByteBuf,
    /// The wrap ciphertext, byte-identical to what was deposited.
    pub wrap: ByteBuf,
    /// Holder-side deposit instant, unix ms — the stamp its receipt carries.
    pub deposited_at_ms: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to [`EscrowGetRequest`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EscrowGetReply {
    pub wraps: Vec<EscrowWrapRow>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Delete one generation's wraps for the calling account.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EscrowDeleteRequest {
    /// The 32-byte generation id whose wraps die.
    pub generation_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to [`EscrowDeleteRequest`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EscrowDeleteReply {
    /// How many wrap rows were deleted (0 = the generation had none — not an
    /// error: the shred is idempotent).
    pub deleted: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}
