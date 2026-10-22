//! The custodian's account over an app's **real** account runtime — the one
//! [`CustodianAccount`] every app hosting a conversations engine links (tui,
//! linux; the `fauna-ffi` seat next), so no app writes its own
//! (`conversations-oracle.md` § The custodian and the door).
//!
//! Each read the custodian's policy needs comes from the account's own state,
//! per call:
//!
//! * **who the principal is** — the roster's row (`fauna.principals.list`):
//!   the principal id is nest-minted at the token endpoint, after the grant,
//!   so the owner's grant log never names it, and the frozen
//!   [`fauna_core::grant_event::GrantEvent`] shape cannot grow it. The roster
//!   is the principal → holder-key map; the **owner's log** is what admits,
//!   and a holder key the log grants nothing live is refused whatever the
//!   roster says. A roster that cannot be read refuses the operation — the
//!   revoke bound is "the next operation", which an offline cache would break.
//! * **what it may do** — [`fauna_core::grant_event::current_grants`] over the
//!   succession ledger the account runtime holds, filtered to the holder and
//!   to a live window, revoked grants already folded out.
//! * **the ceiling** — one replica-local meta row per principal in the account
//!   store ([`MetaRows`]), which every app on this device mounts, so it binds
//!   across restarts and across the apps that take turns serving.
//! * **the record** — a durable outbox in the same store, drained by
//!   [`CustodianAccount::upkeep`] in batches of at most
//!   [`PRINCIPAL_OPS_RECORD_BATCH`] to `fauna.principals.ops.record`, the same
//!   op ids resubmitted until the nest acknowledges them.
//! * **search** — the device's own content index, through the resolver the
//!   app's Search page already registers.

use std::sync::Arc;

use fauna_client_config::SuccessionLedgerStore;
use fauna_client_search::{LocalIndexResolver, SearchKindClass, SearchNav};
use fauna_conversations::ThreadId;
use fauna_core::identity_op::{OPS_PER_WINDOW, RateWindow, window_live};
use fauna_ipc::oracle::DoorFuture;
use fauna_protocol::principals::{
    PRINCIPAL_OPS_RECORD_BATCH, PrincipalInfo, RecordPrincipalOpsRequest, RecordedPrincipalOp,
    RecordedWindow,
};

use crate::{CustodianAccount, IndexHit, LiveTuple};

/// The meta-row namespace this module owns (the store's one-owner-per-key
/// contract).
const META_PREFIX: &str = "conversations-oracle";

/// The most record rows the outbox keeps. A nest unreachable for long enough
/// to fill it loses the oldest rows: the record never blocks the operation,
/// and the operation's admission is the grant and the ceiling, not the record.
pub const OUTBOX_MAX_ROWS: usize = 4096;

/// How long a record row is retried before the outbox lets it go — a row the
/// nest refuses for good (its principal revoked since) must not stand in the
/// outbox for ever.
pub const OUTBOX_MAX_AGE_SECS: i64 = 7 * 24 * 60 * 60;

/// Replica-local rows of the account store — device-local, never synced, read
/// by every process that mounts this device's store.
pub trait MetaRows: Send + Sync + 'static {
    /// `None` when the key was never written.
    fn get<'a>(&'a self, key: &'a str) -> DoorFuture<'a, Result<Option<Vec<u8>>, String>>;
    /// Durable when this answers.
    fn put<'a>(&'a self, key: &'a str, bytes: Vec<u8>) -> DoorFuture<'a, Result<(), String>>;
}

/// The two nest kinds the account reads and writes.
pub trait NestPrincipals: Send + Sync + 'static {
    /// `fauna.principals.list` — the roster.
    fn list_principals(&self) -> DoorFuture<'_, Result<Vec<PrincipalInfo>, String>>;
    /// `fauna.principals.ops.record` — answers the rows newly kept.
    fn record_ops(&self, request: RecordPrincipalOpsRequest)
    -> DoorFuture<'_, Result<u32, String>>;
}

/// The custodian's account over the account runtime's stores.
pub struct RuntimeAccount {
    actor_id: [u8; 32],
    nest: Arc<dyn NestPrincipals>,
    ledger: Arc<dyn SuccessionLedgerStore>,
    meta: Arc<dyn MetaRows>,
    index: Option<Arc<dyn LocalIndexResolver>>,
    /// The outbox's read-modify-write, one writer at a time in this process;
    /// one app serves the door at a time, so no other process writes it while
    /// this one records.
    outbox: tokio::sync::Mutex<()>,
}

impl RuntimeAccount {
    /// `index` is `None` for an app whose content index is not wired; a
    /// device-wide search then answers no hits.
    pub fn new(
        actor_id: [u8; 32],
        nest: Arc<dyn NestPrincipals>,
        ledger: Arc<dyn SuccessionLedgerStore>,
        meta: Arc<dyn MetaRows>,
        index: Option<Arc<dyn LocalIndexResolver>>,
    ) -> Self {
        Self {
            actor_id,
            nest,
            ledger,
            meta,
            index,
            outbox: tokio::sync::Mutex::new(()),
        }
    }

    fn rate_key(principal_id: &[u8]) -> String {
        format!("{META_PREFIX}/rate/{}", hex::encode(principal_id))
    }

    fn outbox_key() -> String {
        format!("{META_PREFIX}/outbox")
    }

    async fn load_outbox(&self) -> Result<Vec<RecordPrincipalOpsRequest>, String> {
        match self.meta.get(&Self::outbox_key()).await? {
            None => Ok(Vec::new()),
            Some(bytes) => fauna_cbor::decode_strict(&bytes).map_err(|e| e.to_string()),
        }
    }

    async fn store_outbox(&self, outbox: &[RecordPrincipalOpsRequest]) -> Result<(), String> {
        let bytes = fauna_cbor::encode_canonical(&outbox).map_err(|e| e.to_string())?;
        self.meta.put(&Self::outbox_key(), bytes).await
    }

    /// The principal's ceiling row as the record reports it — `None` when it
    /// cannot be read or holds no window a ceiling could (opened in the
    /// future, over the ceiling), so a broken row never reports a window and
    /// never gets the batch carrying it refused (the ceiling itself reads an
    /// undecodable row as full).
    async fn reported_window(&self, principal_id: &[u8]) -> Option<RecordedWindow> {
        let bytes = self.meta.get(&Self::rate_key(principal_id)).await.ok()??;
        let window = decode_window(&bytes)
            .filter(|w| w.window_start <= self.now() && w.count <= OPS_PER_WINDOW)?;
        Some(RecordedWindow {
            window_start: window.window_start,
            count: window.count,
            extra: Default::default(),
        })
    }

    /// The rows waiting for the nest, per principal (tests and diagnostics).
    pub async fn pending_records(&self) -> Vec<RecordPrincipalOpsRequest> {
        let _outbox = self.outbox.lock().await;
        self.load_outbox().await.unwrap_or_default()
    }

    /// One pass of the record pump: submit every waiting row, at most
    /// [`PRINCIPAL_OPS_RECORD_BATCH`] per request, and drop what the nest
    /// acknowledged. A failed request leaves its rows for the next pass,
    /// under the same op ids. Answers the rows acknowledged.
    pub async fn drain_records(&self) -> usize {
        let pending = {
            let _outbox = self.outbox.lock().await;
            match self.load_outbox().await {
                Ok(outbox) => outbox,
                Err(e) => {
                    tracing::warn!("the oracle's record outbox could not be read: {e}");
                    return 0;
                }
            }
        };
        let mut sent: Vec<Vec<u8>> = Vec::new();
        for entry in &pending {
            // The ceiling's position rides every batch: the window lives here,
            // and the roster's count in window is read off the newest one.
            let window = self.reported_window(&entry.principal_id).await;
            for batch in entry.ops.chunks(PRINCIPAL_OPS_RECORD_BATCH) {
                let request = RecordPrincipalOpsRequest {
                    principal_id: entry.principal_id.clone(),
                    ops: batch.to_vec(),
                    window: window.clone(),
                    extra: Default::default(),
                };
                match self.nest.record_ops(request).await {
                    Ok(_) => sent.extend(batch.iter().map(|op| op.op_id.clone())),
                    Err(e) => {
                        tracing::debug!("the oracle's record submission failed, kept: {e}");
                        break;
                    }
                }
            }
        }
        let horizon = i64::try_from(self.now())
            .unwrap_or(i64::MAX)
            .saturating_sub(OUTBOX_MAX_AGE_SECS);
        let _outbox = self.outbox.lock().await;
        let Ok(mut outbox) = self.load_outbox().await else {
            return 0;
        };
        let before: usize = outbox.iter().map(|e| e.ops.len()).sum();
        for entry in &mut outbox {
            entry
                .ops
                .retain(|op| !sent.contains(&op.op_id) && op.at >= horizon);
        }
        outbox.retain(|entry| !entry.ops.is_empty());
        let after: usize = outbox.iter().map(|e| e.ops.len()).sum();
        if before != after
            && let Err(e) = self.store_outbox(&outbox).await
        {
            tracing::warn!("the oracle's record outbox could not be written: {e}");
        }
        sent.len()
    }
}

/// A rate row: the window's start (u64) and its count (u32), big-endian.
fn encode_window(window: RateWindow) -> Vec<u8> {
    let mut bytes = window.window_start.to_be_bytes().to_vec();
    bytes.extend_from_slice(&window.count.to_be_bytes());
    bytes
}

fn decode_window(bytes: &[u8]) -> Option<RateWindow> {
    let (start, count) = bytes.split_at_checked(8)?;
    Some(RateWindow {
        window_start: u64::from_be_bytes(start.try_into().ok()?),
        count: u32::from_be_bytes(count.try_into().ok()?),
    })
}

impl CustodianAccount for RuntimeAccount {
    fn actor_id(&self) -> [u8; 32] {
        self.actor_id
    }

    fn principal_holder<'a>(&'a self, principal_id: &'a [u8]) -> DoorFuture<'a, Option<[u8; 32]>> {
        Box::pin(async move {
            let roster = match self.nest.list_principals().await {
                Ok(roster) => roster,
                Err(e) => {
                    tracing::info!("the oracle door could not read the roster, refusing: {e}");
                    return None;
                }
            };
            roster
                .into_iter()
                .find(|p| p.principal_id == principal_id)
                .and_then(|p| p.holder_x25519)
                .and_then(|h| <[u8; 32]>::try_from(h.as_slice()).ok())
        })
    }

    fn live_tuples<'a>(&'a self, holder: &'a [u8; 32], now: u64) -> DoorFuture<'a, Vec<LiveTuple>> {
        Box::pin(async move {
            let ledger = match self.ledger.load().await {
                Ok(ledger) => ledger,
                Err(e) => {
                    tracing::info!("the oracle door could not read the grant log, refusing: {e}");
                    return Vec::new();
                }
            };
            fauna_core::grant_event::current_grants(&ledger)
                .into_iter()
                .filter(|g| g.holder == holder && window_live(g.window_start, g.window_end, now))
                .flat_map(|g| g.scope)
                // The raw kind, never the factor-stripped one: a tuple some
                // later shape confines by a factor must not read as an
                // unconfined class here.
                .map(|t| LiveTuple {
                    class: t.class,
                    kind: t.kind,
                })
                .collect()
        })
    }

    fn rate_window<'a>(&'a self, principal_id: &'a [u8]) -> DoorFuture<'a, RateWindow> {
        Box::pin(async move {
            let full = RateWindow {
                window_start: self.now(),
                count: OPS_PER_WINDOW,
            };
            match self.meta.get(&Self::rate_key(principal_id)).await {
                Ok(None) => RateWindow::default(),
                Ok(Some(bytes)) => decode_window(&bytes).unwrap_or(full),
                Err(e) => {
                    tracing::warn!("the oracle's ceiling row could not be read, refusing: {e}");
                    full
                }
            }
        })
    }

    fn store_rate_window<'a>(
        &'a self,
        principal_id: &'a [u8],
        window: RateWindow,
    ) -> DoorFuture<'a, ()> {
        Box::pin(async move {
            if let Err(e) = self
                .meta
                .put(&Self::rate_key(principal_id), encode_window(window))
                .await
            {
                tracing::warn!("the oracle's ceiling row could not be written: {e}");
            }
        })
    }

    fn record<'a>(&'a self, principal_id: &'a [u8], op: RecordedPrincipalOp) -> DoorFuture<'a, ()> {
        Box::pin(async move {
            let _outbox = self.outbox.lock().await;
            let mut outbox = match self.load_outbox().await {
                Ok(outbox) => outbox,
                Err(e) => {
                    tracing::warn!("the oracle's record outbox could not be read, row lost: {e}");
                    return;
                }
            };
            match outbox.iter_mut().find(|e| e.principal_id == principal_id) {
                Some(entry) => entry.ops.push(op),
                // The window is read as the batch leaves, never kept here.
                None => outbox.push(RecordPrincipalOpsRequest {
                    principal_id: principal_id.to_vec(),
                    ops: vec![op],
                    ..Default::default()
                }),
            }
            // Over the bound: drop the oldest rows, whoever's they are.
            let mut total: usize = outbox.iter().map(|e| e.ops.len()).sum();
            while total > OUTBOX_MAX_ROWS {
                let Some(oldest) = outbox
                    .iter_mut()
                    .filter(|e| !e.ops.is_empty())
                    .min_by_key(|e| e.ops[0].at)
                else {
                    break;
                };
                oldest.ops.remove(0);
                total -= 1;
            }
            outbox.retain(|entry| !entry.ops.is_empty());
            if let Err(e) = self.store_outbox(&outbox).await {
                tracing::warn!("the oracle's record outbox could not be written, row lost: {e}");
            }
        })
    }

    fn search_index<'a>(&'a self, query: &'a str, limit: u32) -> DoorFuture<'a, Vec<IndexHit>> {
        Box::pin(async move {
            let Some(resolver) = self.index.as_ref() else {
                return Vec::new();
            };
            let Some(index) = resolver.resolve().await else {
                return Vec::new();
            };
            let hits = match index
                .query(query, &[SearchKindClass::Conversation], limit as usize)
                .await
            {
                Ok(hits) => hits,
                Err(e) => {
                    tracing::info!("the oracle's index search failed: {e}");
                    return Vec::new();
                }
            };
            hits.into_iter()
                .filter_map(|hit| match hit.navigation? {
                    SearchNav::Mail {
                        thread_id,
                        message_id,
                    } => Some(IndexHit {
                        thread_id: ThreadId(thread_id),
                        message_id,
                        timestamp_ms: hit.timestamp,
                    }),
                    _ => None,
                })
                .collect()
        })
    }

    fn now(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    fn upkeep(&self) -> DoorFuture<'_, ()> {
        Box::pin(async move {
            self.drain_records().await;
        })
    }
}

#[cfg(test)]
mod tests;
