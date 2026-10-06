//! `fauna.custody.*` — the custodian-nest runtime's wire surface.
//!
//! The nest-custodian identity fact (ruled 2026-08-17) lets a custody accept
//! bind the host's NEST as the serving principal
//! (`docs/goal/architecture/account-data-plane.md` § Replica posture → The
//! custody grant + ceremony, the device-or-nest bullet). Its runtime is item
//! 6's stages (b) and (c):
//!
//! * **Hosting registration (stage b)** — the host's app deposits a *keyless
//!   custody-hosting row* on its OWN nest (`fauna.custody.hosting.register`,
//!   read back over `.list`): witness verbatim + owner-fleet endpoints
//!   snapshot + `owner_nest_url` + budget, the keyless capability-row
//!   precedent. The nest's pump runs the custody pull leg against that row;
//!   stop and budget-adjust from the host's app rewrite the same row.
//! * **The receipt deposit arm (stage c)** — the custodian NEST mints A7
//!   receipts under its pinned actor identity and DEPOSITS them at the
//!   OWNER's nest custody door (`fauna.custody.receipt.deposit`, a
//!   custodian-class caller over the same custody-handshake bearer the pull
//!   rides); the owner's nest stages latest-per-grant, and the owner's fleet
//!   fetches (`fauna.custody.receipt.list`) and folds + re-verifies at sync
//!   exactly as the channel-carried receipts today. REJECTED (the bullet's
//!   item 6 names why; do not re-litigate): app-relay carriage, owner-side
//!   receipt pulls from the custodian nest.
//!
//! The hosting row is a **projection of the host's client-sealed ceremony
//! record** (`HeldCustody`, a `fauna.state.custody-ceremony` row): losing it loses nothing a
//! re-deposit cannot recreate, which is what licenses its keyless plaintext
//! rest and its `Succession::Burn` disposition nest-side.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

use fauna_cbor::Value;

/// `fauna.custody.hosting.register` — deposit (or rewrite) a custody-hosting
/// row on the caller's own nest. The caller is the HOST user (the account that
/// signed the nest-form accept); the row keys on `(caller, grant_id)`, so
/// re-registering is an in-place LWW rewrite — the stop control and the
/// budget-adjust are this same verb with different field values, never a
/// second door.
pub const HOSTING_REGISTER_KIND: &str = "fauna.custody.hosting.register";

/// `fauna.custody.hosting.list` — the caller's own hosting rows, with the
/// pump's metering read back. This is how the host's app renders "what my
/// nest holds for others" (`docs/goal/ui/devices.md` § Custody facet piece 3)
/// — policy through the nest, never a side channel (the
/// `fauna.backup.destination.list` read-back precedent).
pub const HOSTING_LIST_KIND: &str = "fauna.custody.hosting.list";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostingRegisterRequest {
    /// The ceremony's grant id (`CustodyGrant.grant_id`, 16 bytes) — the row
    /// key together with the authenticated caller.
    #[serde(default)]
    pub grant_id: ByteBuf,
    /// Hex-encoded 32-byte actor id of the custodied OWNER (the account whose
    /// planes the nest will pull). Must match the witness body.
    #[serde(default)]
    pub owner_actor_id: String,
    /// The owner-signed custody witness, canonical bytes VERBATIM
    /// (`fauna_core::custody_grant::CustodyGrant` under its signed envelope).
    /// The door verifies it decodes, verifies under the owner it names, names
    /// THIS nest's identity as `custodian_key`, and is inside its window —
    /// a row the pump could never use is refused at deposit, not discovered
    /// dead on the first pass.
    #[serde(default)]
    pub witness: ByteBuf,
    /// The owner's nest URL — the pull leg's only dial anchor. Validated
    /// against `fauna_core::counterparty_url::validate_counterparty_nest_url`
    /// at this door (ingest) AND re-checked every pump pass before `connect()`
    /// (the row may rest and be rewritten later).
    #[serde(default)]
    pub owner_nest_url: String,
    /// Owner-fleet endpoints snapshot, canonical CBOR of
    /// `Vec<fauna_core::device_endpoints::DeviceEndpoints>`, carried OPAQUE.
    /// The nest pull leg never dials owner devices (the nest↔nest leg is its
    /// whole route), but the ceremony hands the snapshot to every custodian
    /// alike and the row carries it for the restore/serve follow-on rather
    /// than forcing a wire bump then. Empty is valid.
    #[serde(default)]
    pub owner_devices: ByteBuf,
    /// The host-chosen byte budget (`retained_bytes_cap`, T15). `0` means
    /// "cap missing" and the pump substitutes the hard-coded default — never
    /// "hold nothing" (the `custody_leg::meter_and_evict` rule).
    #[serde(default)]
    pub retained_bytes_cap: u64,
    /// The stop control: a stopped row is kept (the UI still lists it) but
    /// the pump skips it — no pull, no receipt, coverage decays owner-side
    /// per the A7 honesty rule.
    #[serde(default)]
    pub stopped: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostingRegisterReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostingListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One hosting row read back: the registered fields plus the pump's metering.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostingItem {
    #[serde(default)]
    pub grant_id: ByteBuf,
    #[serde(default)]
    pub owner_actor_id: String,
    #[serde(default)]
    pub owner_nest_url: String,
    #[serde(default)]
    pub retained_bytes_cap: u64,
    #[serde(default)]
    pub stopped: bool,
    /// Unix seconds; when the row was last registered/rewritten.
    #[serde(default)]
    pub updated_at: u64,
    /// Pump-metered bytes currently held for this custody (post-eviction).
    /// `0` until the first pull pass completes.
    #[serde(default)]
    pub held_bytes: u64,
    /// `attested_at` (microseconds) of the newest receipt the pump minted for
    /// this row; `0` = none yet. Host-side freshness renders from this with
    /// the same three-state honesty words the owner side uses.
    #[serde(default)]
    pub last_receipt_at: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.admin.custody_hosting.list` — every hosting row on this nest, with
/// its depositing host: the NEST-WIDE list the
/// per-caller `fauna.custody.hosting.list` deliberately is not. Required by
/// the *no client-causable unrecoverable nest state* invariant: without it an
/// admin cannot even see the rows an account holder planted.
pub const ADMIN_HOSTING_LIST_KIND: &str = "fauna.admin.custody_hosting.list";

/// `fauna.admin.custody_hosting.remove` — drop one hosting row AND, when it
/// was the last row for its `(host, owner)` pair, the custodied store beneath
/// it. The recoverability half of the invariant: until
/// this door, the only remedy for a disk filled through the register door was
/// `sqlite3` on `nest.db` plus `rm -rf` — off-box-only-fixable state, which
/// is a bug by chartered rule. No credit-back is owed anywhere: the held-byte
/// counter is DERIVED (`SUM(held_bytes)`), so dropping the row drops the
/// figure.
pub const ADMIN_HOSTING_REMOVE_KIND: &str = "fauna.admin.custody_hosting.remove";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AdminHostingListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One nest-wide hosting row: the host it belongs to plus the same read-back
/// shape the host's own list serves.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AdminHostingRow {
    /// Hex-encoded 32-byte actor id of the DEPOSITING host.
    #[serde(default)]
    pub host_actor_id: String,
    #[serde(default)]
    pub item: HostingItem,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AdminHostingListReply {
    #[serde(default)]
    pub rows: Vec<AdminHostingRow>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AdminHostingRemoveRequest {
    /// Hex-encoded 32-byte actor id of the depositing host.
    #[serde(default)]
    pub host_actor_id: String,
    /// The row's grant id.
    #[serde(default)]
    pub grant_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AdminHostingRemoveReply {
    /// The row existed and was dropped.
    pub removed: bool,
    /// The `(host, owner)` custodied store was dropped too — true only when
    /// the removed row was the pair's last (another grant keeps the store).
    pub store_dropped: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostingListReply {
    #[serde(default)]
    pub rows: Vec<HostingItem>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.custody.hosting.remove` — the HOST drops one of its own hosting
/// rows AND, when it was the last row for its `(host, owner)` pair, the
/// custodied store beneath it (the reclaim half the runtime build
/// deliberately left open). The User-class twin of
/// [`ADMIN_HOSTING_REMOVE_KIND`], host-derived from the authenticated
/// connection exactly like register/list: a caller can only ever remove its
/// OWN rows. Stop is a pause (bytes remain); remove is the reclaim. Same
/// no-credit-back reasoning as the admin door — the held-byte figure is
/// derived, so dropping the row drops it.
pub const HOSTING_REMOVE_KIND: &str = "fauna.custody.hosting.remove";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostingRemoveRequest {
    /// The row's grant id (keyed with the authenticated caller as host).
    #[serde(default)]
    pub grant_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostingRemoveReply {
    /// The row existed under this caller and was dropped.
    pub removed: bool,
    /// The `(host, owner)` custodied store was dropped too — true only when
    /// the removed row was the pair's last (another grant keeps the store).
    pub store_dropped: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.custody.receipt.deposit` — a CUSTODIAN-class caller (the custodian
/// nest, authenticated over `fauna.auth.custody_handshake`; its bearer's
/// actor IS the custodian key) deposits a signed A7 custody receipt at the
/// OWNER's nest. The door verifies the receipt signature against the LIVE
/// capability row's holder key BEFORE staging (a lying deposit is refused at
/// the door, not at the fleet fold — the gotcha), and stages
/// latest-per-grant, monotone in `attested_at` — which is also the replay
/// argument: re-depositing the same receipt is a no-op, never a duplicate.
pub const RECEIPT_DEPOSIT_KIND: &str = "fauna.custody.receipt.deposit";

/// `fauna.custody.receipt.list` — the OWNER's own staged receipts, fetched at
/// sync and folded through the same verify-against-the-recorded-accept path
/// the channel-carried receipts use. Owner-scoped: a caller only ever sees
/// receipts deposited for its own grants.
pub const RECEIPT_LIST_KIND: &str = "fauna.custody.receipt.list";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReceiptDepositRequest {
    /// Hex-encoded 32-byte actor id of the OWNER whose grant this receipt
    /// attests for — the capability-row lookup key together with `grant_id`
    /// (the bearer's actor is the custodian, so the owner must ride the
    /// request, exactly as the custody pull doors take `of_owner`).
    #[serde(default)]
    pub owner_actor_id: String,
    /// The ceremony's grant id (`CustodyGrant.grant_id`).
    #[serde(default)]
    pub grant_id: ByteBuf,
    /// The signed receipt, canonical `EmbedAsBytes` bytes VERBATIM
    /// (`fauna_core::custody_receipt::CustodyReceipt` under its envelope) —
    /// stored verbatim because the owner's fleet re-verifies the very
    /// signature a re-encode would invalidate.
    #[serde(default)]
    pub receipt: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReceiptDepositReply {
    pub ok: bool,
    /// Whether this deposit displaced the staged receipt — `false` for a
    /// not-newer re-delivery (the idempotent no-op), `true` when staged.
    #[serde(default)]
    pub staged: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReceiptListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One staged receipt: the newest the owner's nest holds for one grant.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReceiptItem {
    #[serde(default)]
    pub grant_id: ByteBuf,
    /// The signed receipt envelope, verbatim as deposited.
    #[serde(default)]
    pub receipt: ByteBuf,
    /// The receipt's own `attested_at` (microseconds) — the monotone key.
    #[serde(default)]
    pub attested_at: u64,
    /// Unix seconds the owner's nest staged it.
    #[serde(default)]
    pub staged_at: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReceiptListReply {
    #[serde(default)]
    pub rows: Vec<ReceiptItem>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_strict, encode_canonical};

    /// The register request must round-trip dag-cbor with every field
    /// populated AND decode from an empty map (all-`default` fields) — the
    /// additive-everywhere floor for a kind an older client will never send
    /// but a newer one must never break on.
    #[test]
    fn hosting_register_round_trips_and_tolerates_absence() {
        let req = HostingRegisterRequest {
            grant_id: ByteBuf::from(vec![7u8; 16]),
            owner_actor_id: "ab".repeat(32),
            witness: ByteBuf::from(vec![1, 2, 3]),
            owner_nest_url: "https://owner.example".into(),
            owner_devices: ByteBuf::from(vec![9, 9]),
            retained_bytes_cap: 4096,
            stopped: false,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).expect("encode");
        let back: HostingRegisterRequest = decode_strict(&bytes).expect("decode");
        assert_eq!(req, back);

        let empty: HostingRegisterRequest =
            decode_strict(&encode_canonical(&HostingRegisterRequest::default()).unwrap())
                .expect("empty decodes");
        assert!(empty.grant_id.is_empty() && !empty.stopped);
    }

    #[test]
    fn hosting_item_round_trips() {
        let item = HostingItem {
            grant_id: ByteBuf::from(vec![7u8; 16]),
            owner_actor_id: "cd".repeat(32),
            owner_nest_url: "wss://owner.example".into(),
            retained_bytes_cap: 10,
            stopped: true,
            updated_at: 5,
            held_bytes: 3,
            last_receipt_at: 4,
            extra: Default::default(),
        };
        let back: HostingItem = decode_strict(&encode_canonical(&item).unwrap()).expect("decode");
        assert_eq!(item, back);
    }

    /// The admin surface's four payloads: populated
    /// round trip + the all-`default` absence floor, same contract as the
    /// host-side doors above.
    #[test]
    fn admin_hosting_payloads_round_trip_and_tolerate_absence() {
        let row = AdminHostingRow {
            host_actor_id: "ef".repeat(32),
            item: HostingItem {
                grant_id: ByteBuf::from(vec![7u8; 16]),
                owner_actor_id: "cd".repeat(32),
                owner_nest_url: "https://owner.example".into(),
                retained_bytes_cap: 10,
                stopped: false,
                updated_at: 5,
                held_bytes: 3,
                last_receipt_at: 4,
                extra: Default::default(),
            },
            extra: Default::default(),
        };
        let reply = AdminHostingListReply {
            rows: vec![row],
            extra: Default::default(),
        };
        let back: AdminHostingListReply =
            decode_strict(&encode_canonical(&reply).unwrap()).expect("decode list reply");
        assert_eq!(reply, back);

        let req = AdminHostingRemoveRequest {
            host_actor_id: "ef".repeat(32),
            grant_id: ByteBuf::from(vec![7u8; 16]),
            extra: Default::default(),
        };
        let back: AdminHostingRemoveRequest =
            decode_strict(&encode_canonical(&req).unwrap()).expect("decode remove req");
        assert_eq!(req, back);

        let rep = AdminHostingRemoveReply {
            removed: true,
            store_dropped: false,
            extra: Default::default(),
        };
        let back: AdminHostingRemoveReply =
            decode_strict(&encode_canonical(&rep).unwrap()).expect("decode remove reply");
        assert_eq!(rep, back);

        let empty: AdminHostingListRequest =
            decode_strict(&encode_canonical(&AdminHostingListRequest::default()).unwrap())
                .expect("empty list request decodes");
        assert!(empty.extra.is_empty());
    }

    #[test]
    fn hosting_remove_payloads_round_trip_and_tolerate_absence() {
        let req = HostingRemoveRequest {
            grant_id: ByteBuf::from(vec![7u8; 16]),
            extra: Default::default(),
        };
        let back: HostingRemoveRequest =
            decode_strict(&encode_canonical(&req).unwrap()).expect("decode remove req");
        assert_eq!(req, back);

        let rep = HostingRemoveReply {
            removed: true,
            store_dropped: true,
            extra: Default::default(),
        };
        let back: HostingRemoveReply =
            decode_strict(&encode_canonical(&rep).unwrap()).expect("decode remove reply");
        assert_eq!(rep, back);

        let empty: HostingRemoveRequest =
            decode_strict(&encode_canonical(&HostingRemoveRequest::default()).unwrap())
                .expect("empty remove request decodes");
        assert!(empty.grant_id.is_empty());
    }
}
