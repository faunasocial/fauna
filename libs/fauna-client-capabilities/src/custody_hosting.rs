//! Host-side WS-RPC seam for the `fauna.custody.hosting.*` doors — the thin
//! transport wrapper the HOST's app uses to deposit, rewrite, and read back
//! the custody-hosting row on its OWN nest (the custodian-nest runtime's
//! stage (b): `account-data-plane.md` § Replica posture → The custody grant +
//! ceremony, the device-or-nest bullet, item 6).
//!
//! The row is the nest-side projection of a NEST-anchored [`HeldCustody`]
//! ceremony record: witness verbatim + owner-fleet endpoints snapshot +
//! `owner_nest_url` + budget. Depositing it is what arms the host nest's pull
//! pump; the stop control and the budget-adjust are re-deposits of the same
//! row (`register` is an LWW upsert nest-side), and `list` is the read-back
//! the host UI renders "what my nest holds for others" from — policy through
//! the nest, never a side channel.
//!
//! **Pattern:** the wasm-clean generic seam ([`crate::rpc::CapabilitiesClient`]
//! / `BridgesClient`) — `struct .. <R: RpcRequester>`, one `async fn` per
//! kind, no state machine, no concrete transport.
//!
//! [`HeldCustody`]: fauna_core::custody_ceremony::HeldCustody

use fauna_protocol::ByteBuf;
use fauna_protocol::RpcRequester;
use fauna_protocol::custody::{
    ADMIN_HOSTING_LIST_KIND, ADMIN_HOSTING_REMOVE_KIND, AdminHostingListReply,
    AdminHostingListRequest, AdminHostingRemoveReply, AdminHostingRemoveRequest, HOSTING_LIST_KIND,
    HOSTING_REGISTER_KIND, HostingListReply, HostingListRequest, HostingRegisterReply,
    HostingRegisterRequest,
};

/// A thin host-side wrapper over the `fauna.custody.hosting.{register,list}`
/// WS-RPC kinds, generic over the `R: RpcRequester` transport.
pub struct CustodyHostingClient<R: RpcRequester> {
    nest: R,
}

/// The registered half of a hosting row — what the host's app deposits and
/// rewrites. Metering (`held_bytes`, `last_receipt_at`) is pump-written and
/// only ever read back, so it has no place here.
#[derive(Debug, Clone, PartialEq)]
pub struct HostingDeposit {
    /// The ceremony's grant id (`CustodyGrant.grant_id`).
    pub grant_id: Vec<u8>,
    /// The custodied owner's 32-byte actor id.
    pub owner: [u8; 32],
    /// The owner-signed custody witness, canonical `EmbedAsBytes` bytes
    /// verbatim — it must name the host's PINNED nest identity as
    /// `custodian_key`, or the nest's register door refuses it.
    pub witness: Vec<u8>,
    /// The owner's nest URL (the pull leg's only dial anchor).
    pub owner_nest_url: String,
    /// Opaque owner-fleet endpoints snapshot (canonical CBOR of
    /// `Vec<DeviceEndpoints>`); empty is valid.
    pub owner_devices: Vec<u8>,
    /// The host-chosen byte budget; `0` = cap missing (the pump substitutes
    /// the hard-coded default).
    pub retained_bytes_cap: u64,
    /// The stop control: a stopped row is kept and listed but the pump skips
    /// it.
    pub stopped: bool,
}

impl<R: RpcRequester> CustodyHostingClient<R> {
    /// Wrap a transport that can talk to the HOST's own nest.
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.custody.hosting.register` — deposit (or rewrite) the hosting
    /// row. Idempotent LWW on `(caller, grant_id)`; stop and budget-adjust
    /// are this same verb with different field values.
    pub async fn register(
        &self,
        deposit: &HostingDeposit,
    ) -> Result<HostingRegisterReply, R::Error> {
        self.nest
            .request(
                HOSTING_REGISTER_KIND,
                HostingRegisterRequest {
                    grant_id: ByteBuf::from(deposit.grant_id.clone()),
                    owner_actor_id: fauna_core::hex32::encode(&deposit.owner),
                    witness: ByteBuf::from(deposit.witness.clone()),
                    owner_nest_url: deposit.owner_nest_url.clone(),
                    owner_devices: ByteBuf::from(deposit.owner_devices.clone()),
                    retained_bytes_cap: deposit.retained_bytes_cap,
                    stopped: deposit.stopped,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.custody.hosting.list` — the caller's own hosting rows with the
    /// pump's metering read back.
    pub async fn list(&self) -> Result<HostingListReply, R::Error> {
        self.nest
            .request(HOSTING_LIST_KIND, HostingListRequest::default())
            .await
    }

    /// `fauna.custody.hosting.remove` — the reclaim: drop the
    /// caller's own row and, when it was the `(host, owner)` pair's last, the
    /// custodied store beneath it. Stop pauses and keeps the bytes; this
    /// frees them. Host-scoped like its siblings — a grant id the caller does
    /// not host answers `removed: false`.
    pub async fn remove(
        &self,
        grant_id: &[u8],
    ) -> Result<fauna_protocol::custody::HostingRemoveReply, R::Error> {
        self.nest
            .request(
                fauna_protocol::custody::HOSTING_REMOVE_KIND,
                fauna_protocol::custody::HostingRemoveRequest {
                    grant_id: ByteBuf::from(grant_id.to_vec()),
                    extra: Default::default(),
                },
            )
            .await
    }
}

/// A thin wrapper over the `fauna.custody.receipt.{deposit,list}` kinds —
/// stage (c)'s two carriers. `deposit` is spoken by the custodian NEST's pump
/// over its custody-bearer session at the OWNER's nest; `list` by the owner's
/// own app at sync, feeding the recorded-accept re-verify fold.
pub struct CustodyReceiptsClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> CustodyReceiptsClient<R> {
    /// Wrap a transport: the custody-bearer session (deposit) or the owner's
    /// own authed connection (list).
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.custody.receipt.deposit` — deposit a signed receipt at the
    /// owner's nest. `receipt` is the canonical signed envelope bytes,
    /// verbatim. Idempotent: a not-newer re-delivery replies `staged: false`.
    pub async fn deposit(
        &self,
        owner: [u8; 32],
        grant_id: &[u8],
        receipt: Vec<u8>,
    ) -> Result<fauna_protocol::custody::ReceiptDepositReply, R::Error> {
        self.nest
            .request(
                fauna_protocol::custody::RECEIPT_DEPOSIT_KIND,
                fauna_protocol::custody::ReceiptDepositRequest {
                    owner_actor_id: fauna_core::hex32::encode(&owner),
                    grant_id: ByteBuf::from(grant_id.to_vec()),
                    receipt: ByteBuf::from(receipt),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.custody.receipt.list` — the caller's own staged receipts.
    pub async fn list(&self) -> Result<fauna_protocol::custody::ReceiptListReply, R::Error> {
        self.nest
            .request(
                fauna_protocol::custody::RECEIPT_LIST_KIND,
                fauna_protocol::custody::ReceiptListRequest::default(),
            )
            .await
    }
}

/// A thin ADMIN-side wrapper over the `fauna.admin.custody_hosting.{list,remove}`
/// kinds — the Admin-class twin of
/// [`CustodyHostingClient`], and deliberately its neighbour rather than a
/// resident of `fauna-client-admin`: the two speak the same wire module and
/// share one teardown rule (the custodied store falls only with the
/// `(host, owner)` pair's LAST row), so a session reading one should be
/// reading the other.
///
/// This is the surface the *no client-causable unrecoverable nest state*
/// invariant requires: `fauna.custody.hosting.list` is host-scoped, so
/// without these doors an admin cannot see — let alone drop — a row an
/// account holder planted (`account-data-plane.md` § Two-sided bounds, whose
/// § Implementation status carries the security review that filed it).
pub struct AdminHostingClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> AdminHostingClient<R> {
    /// Wrap an ADMIN-authenticated transport to this nest.
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.admin.custody_hosting.list` — every hosting row on this nest,
    /// each attributed to its depositing host. The nest-wide read the
    /// per-caller door deliberately is not.
    pub async fn list(&self) -> Result<AdminHostingListReply, R::Error> {
        self.nest
            .request(ADMIN_HOSTING_LIST_KIND, AdminHostingListRequest::default())
            .await
    }

    /// `fauna.admin.custody_hosting.remove` — drop one row, keyed by the
    /// `(host, grant)` pair the list serves. The custodied store beneath it
    /// falls only when the removed row was its `(host, owner)` pair's last
    /// (the reply's `store_dropped` says which happened). A grant id no host
    /// holds answers `removed: false` — an honest no-op, never an error.
    ///
    /// No credit-back is owed anywhere: the held-byte figure is DERIVED
    /// (`SUM(held_bytes)`), so dropping the row drops the figure.
    pub async fn remove(
        &self,
        host_actor_id: &str,
        grant_id: &[u8],
    ) -> Result<AdminHostingRemoveReply, R::Error> {
        self.nest
            .request(
                ADMIN_HOSTING_REMOVE_KIND,
                AdminHostingRemoveRequest {
                    host_actor_id: host_actor_id.to_string(),
                    grant_id: ByteBuf::from(grant_id.to_vec()),
                    extra: Default::default(),
                },
            )
            .await
    }
}

/// One `admin-custody-hosting` render's data: the nest-wide registry rows in
/// [`crate::view_model::admin_hosting_rows`]'s order, the last remove's
/// verdict, and a page-scoped read/write error. Shared by tui and linux
/// (previously two byte-identical hand-rolled twins).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdminHostingSnapshot {
    /// Every hosting row on this nest, heaviest hold first
    /// (`crate::view_model::admin_hosting_rows`).
    pub rows: Vec<crate::view_model::AdminHostingRowView>,
    /// The last remove's outcome sentence. NOT an error: `removed: false` on a
    /// row someone else already dropped is an honest no-op, and an error line
    /// would report the opposite of what happened.
    pub status: Option<String>,
    /// The last read/write failure, bridged onto the page's `error-message`.
    pub error: Option<String>,
}

impl AdminHostingSnapshot {
    /// Carry a remove's verdict onto the re-read that follows it.
    pub fn with_status(mut self, status: Option<String>) -> Self {
        self.status = status;
        self
    }
}

/// The `admin-custody-hosting` read: `fauna.admin.custody_hosting.list`
/// through [`AdminHostingClient`], folded by
/// [`crate::view_model::admin_hosting_rows`] so every app renders the same
/// rows in the same order.
///
/// `error` carries a failure the CALLER already has in hand (a failed
/// remove), which the list read does not overwrite: an admin whose remove
/// failed needs to be told that, not handed a silently fine-looking page.
pub async fn load_admin_hosting_snapshot<R: RpcRequester>(
    nest: R,
    error: Option<String>,
) -> AdminHostingSnapshot {
    match AdminHostingClient::new(nest).list().await {
        Ok(reply) => AdminHostingSnapshot {
            rows: crate::view_model::admin_hosting_rows(
                &reply,
                fauna_core::data::Timestamp::now().0,
            ),
            status: None,
            error,
        },
        Err(e) => AdminHostingSnapshot {
            rows: Vec::new(),
            status: None,
            error: Some(error.unwrap_or_else(|| format!("load held custody: {e}"))),
        },
    }
}
