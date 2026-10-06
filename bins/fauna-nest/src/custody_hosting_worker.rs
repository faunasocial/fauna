//! The custody-hosting PUMP — the nest half of the custodian-nest runtime's
//! stage (b) (`account-data-plane.md` § Replica posture → The custody grant +
//! ceremony, the device-or-nest bullet, item 6): every tick, pull every
//! registered custody's covered planes from the owner's nest, with **no host
//! device running**.
//!
//! Shape: the [`crate::segment_backup::NestBackupWorker`] mold — a plain
//! interval loop (no lease, no leader election; this nest is the single
//! custodian for its own hosting rows by construction), first tick at boot,
//! each sweep on the blocking pool, one row's failure logged and skipped.
//! Substance: the shared client custody leg's own pull core
//! (`fauna_sync_engine::custody_leg` — [`NestLegSession`],
//! [`pull_from_owner_nest`], [`pullable_scopes`], [`meter_and_evict`]), so
//! the nest and the custodian-device leg cannot drift; the custodian identity
//! here is the NEST's own deployment key ([`crate::nest_identity`]), which is
//! exactly what a nest-form witness names (`custodian_key` = the host's
//! pinned nest actor identity).
//!
//! Per pass and per row, in order: the counterparty dial policy re-check at
//! **nest** scope (a row already at rest, or rewritten
//! later, never reaches `connect()`; refusals are tallied separately from
//! weather), witness decode
//! and scope derivation, a fresh custody session (the handshake mints at
//! connect, so a revoked capability row takes its honest refusal there),
//! the pull (account-state scopes verbatim into the keyless per-owner store,
//! then each content scope bulk-first), the T15 budget
//! ([`meter_and_evict`] — cap `0` falls back to the ceremony default, never
//! "hold nothing"; the host's number is re-clamped to
//! `MAX_RETAINED_BYTES_CAP` here as well as at the door, so a row already at
//! rest is bounded without it), and the metering write-back the host UI reads over
//! `fauna.custody.hosting.list`. Sessions are deliberately NOT cached across
//! passes (unlike the client leg): one handshake per row per interval is
//! noise, and a nest holding standing sockets to every custodied owner's nest
//! would pay a liveness tax for no freshness win.
//!
//! Receipts (stage (c)) ride the same pass: after the budget, a due receipt
//! (`receipt_due` — first pass, 24 h cadence, an eviction, or a degraded
//! flip) is minted under the nest identity and DEPOSITED at the owner's nest
//! custody door over the same custody bearer; the cadence bookkeeping
//! (`last_receipt_at`/`last_receipt_degraded`) advances only on an acked
//! deposit, so a failed deposit stays due and redrives next pass.

use std::sync::Arc;

use anyhow::{Context, Result};
use fauna_account_store::{sqlite::SqliteBackend, store::AccountStore, types::WriterId};
use fauna_sync_engine::custody_leg::{
    NestLegSession, meter_and_evict, pull_from_owner_nest, pullable_scopes,
};

use crate::routes::AppState;

/// Pull cadence. Hard-coded (bucket 1 — no human would choose this): custody
/// freshness is bounded by the owner's own write rate, receipts are due on a
/// 24 h cadence, and the backup worker's identical interval has proven the
/// weight acceptable.
pub const PULL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// What one sweep over the hosting registry did — every slot
/// latency-independent state, never timing.
#[derive(Debug, Clone, Copy, Default)]
pub struct CustodyHostingPassReport {
    /// Rows enumerated (stopped included — the honest denominator).
    pub rows: usize,
    /// Rows skipped because the host stopped them.
    pub stopped: usize,
    /// Rows refused by the counterparty dial policy this pass — deliberately
    /// distinct from `failed`: policy is a verdict, weather is not.
    pub refused_url: usize,
    /// Rows beyond this host's registry cap
    /// (`MAX_CUSTODY_HOSTING_ROWS_PER_HOST`) — never dialed. Nonzero means rows
    /// exist that the register door would refuse today, so it is a signal worth
    /// reading, not just a skip.
    pub refused_fan_out: usize,
    /// Rows whose pull completed.
    pub pulled: usize,
    /// Relay rows recorded across all pulls.
    pub recorded: usize,
    /// Segment files adopted across all pulls.
    pub adopted_segments: usize,
    /// Rows that failed this pass (unreadable witness, refused handshake,
    /// pull error) — logged, skipped, retried next tick.
    pub failed: usize,
    /// Receipts minted AND acked by the owner's nest this pass (stage c).
    pub receipts_deposited: usize,
    /// Due receipts whose deposit failed — they stay due and redrive.
    pub receipts_failed: usize,
    /// Rows skipped because their witness is expired — never dialed (the
    /// owner-side handshake would refuse them anyway; this is the learn-early
    /// half, row 67).
    pub expired: usize,
    /// `(host, owner)` custodied stores reclaimed this pass — every row of
    /// the pair expired past [`fauna_core::custody_ceremony::
    /// HOSTING_EXPIRED_STORE_GC_GRACE_SECS`] (the GC; stopped or merely
    /// expired rows keep their bytes).
    pub stores_reclaimed: usize,
}

/// The always-on hosting pump. See the module doc for the pass anatomy.
pub struct CustodyHostingWorker {
    state: Arc<AppState>,
    interval: std::time::Duration,
}

impl CustodyHostingWorker {
    /// `interval` is [`PULL_INTERVAL`] in production — a parameter so a test
    /// can drive the loop without waiting.
    pub fn new(state: Arc<AppState>, interval: std::time::Duration) -> Self {
        Self { state, interval }
    }

    /// One sweep over every hosting row. A single row's failure is logged and
    /// skipped — never fatal to the sweep.
    pub async fn run_once(&self) -> Result<CustodyHostingPassReport> {
        let mut report = CustodyHostingPassReport::default();
        let Some(root) = self.state.custody_hosting_root.clone() else {
            // No root configured (test rigs without the pump) — a disabled
            // pump is a no-op, not an error.
            return Ok(report);
        };
        // The custodian identity IS the nest's own — always present (the
        // single-identity unification: `NestIdentity` is a view over the
        // reconciled deployment key).
        let signing_key = self.state.nest_identity.signing_key.clone();
        let nest_pub = signing_key.verifying_key().to_bytes();
        // Read once per pass, not per row: the dial policy's nest scope — a publicly reachable nest has no plaintext-loopback carve-out.
        let public_deployment = self.state.is_public_deployment();

        let rows = self
            .state
            .db
            .list_all_custody_hosting()
            .await
            .context("list custody hosting rows")?;

        // Per-host aggregates for the tier-bounded accounting.
        // The held-bytes figure is DERIVED, and this enumeration already carries
        // every metered row, so the sum needs no extra query — that is the point
        // of counting rather than charging. The tier bound does need one lookup
        // per host; `enforce_tier_quotas` off means no cap at all, the same
        // fail-open the sync plane's metering callers take for an absent lookup.
        let enforce_quotas = *self.state.enforce_tier_quotas.read().await;
        let mut held_by_host: std::collections::HashMap<Vec<u8>, u64> =
            std::collections::HashMap::new();
        for (host, row) in &rows {
            *held_by_host.entry(host.clone()).or_default() += row.held_bytes;
        }
        let mut tier_bounds: std::collections::HashMap<Vec<u8>, Option<u64>> =
            std::collections::HashMap::new();
        if enforce_quotas {
            for host in held_by_host.keys() {
                let bound = match <[u8; 32]>::try_from(host.as_slice()) {
                    Ok(id) => self
                        .state
                        .db
                        .get_user_tier_max_storage_bytes(&id)
                        .await
                        .unwrap_or(None)
                        .map(|b| b.max(0) as u64),
                    // A malformed host id has no users row to bound it; the row
                    // itself is skipped below on the same grounds.
                    Err(_) => None,
                };
                tier_bounds.insert(host.clone(), bound);
            }
        }

        // The per-host row cap, enforced HERE as well as at the register
        // door: the door's count is the only thing that ever bounded
        // the pump's outbound dial fan-out, so a row set exceeding the cap —
        // planted by any second writer that skips the door — would keep dialing every address it named, every pass. The byte
        // ceiling always had this backstop; the row cap did not, while three
        // artifacts said it did. Counted the way the door counts (stopped rows
        // included: a stopped row still holds a registry slot).
        //
        // Per-host tally rather than a run-length counter over the enumeration:
        // the query does order by `(host, updated_at, grant_id)` — which is what
        // makes the admitted set deterministically the host's oldest-updated N —
        // but a **security bound must not depend on an ORDER BY clause in another
        // module**. A grouped counter holds the cap under any order; only *which*
        // rows land inside it would change.
        let mut seen_per_host: std::collections::HashMap<Vec<u8>, usize> =
            std::collections::HashMap::new();
        // The expired-store GC's pair ledger: the custodied store dir
        // is keyed `(host, owner)` and shared by every grant of the pair, so
        // it may be reclaimed only when EVERY row of the pair is expired past
        // the grace. Tallied for every enumerated row BEFORE any skip — a
        // stopped, over-cap, or URL-refused row still holds its pair's bytes
        // alive, and an unreadable witness is conservatively keep-alive (a
        // corrupt row must never steer reclamation).
        let now_micros = fauna_core::data::Timestamp(
            fauna_core::data::Timestamp::now_millis().saturating_mul(1000),
        );
        let mut pair_reclaimable: std::collections::HashMap<(Vec<u8>, Vec<u8>), bool> =
            std::collections::HashMap::new();
        let mut pair_grants: std::collections::HashMap<(Vec<u8>, Vec<u8>), Vec<Vec<u8>>> =
            std::collections::HashMap::new();
        for (host, row) in rows {
            let host_held = held_by_host.get(&host).copied().unwrap_or(0);
            let tier_bound = tier_bounds.get(&host).copied().flatten();
            report.rows += 1;

            let row_expiry = fauna_core::encoding::canonical_decode::<
                fauna_core::encoding::EmbedAsBytes,
            >(&row.witness)
            .ok()
            .and_then(|w| fauna_core::custody_grant::custody_witness_expiry(&w).ok());
            let reclaimable = row_expiry.is_some_and(|expires| {
                fauna_core::custody_ceremony::hosting_store_reclaimable(expires, now_micros)
            });
            let pair = (host.clone(), row.owner_actor_id.clone());
            pair_reclaimable
                .entry(pair.clone())
                .and_modify(|r| *r &= reclaimable)
                .or_insert(reclaimable);
            pair_grants
                .entry(pair)
                .or_default()
                .push(row.grant_id.clone());

            let seen_for_host = seen_per_host.entry(host.clone()).or_insert(0);
            let admitted = fauna_core::custody_ceremony::hosting_pump_admits_row(*seen_for_host);
            *seen_for_host += 1;
            if !admitted {
                tracing::warn!(
                    host = %hex::encode(&host),
                    "custody hosting pump: host is past the registry row cap — row not dialed \
                     (a row the register door would refuse today)"
                );
                report.refused_fan_out += 1;
                continue;
            }
            if row.stopped {
                report.stopped += 1;
                continue;
            }
            // An expired witness never dials: the owner-side handshake would
            // refuse it, so skipping is the learn-early half of the same
            // verdict. Merely expired is NOT reclaimable — the store
            // holds through the grace window above.
            if row_expiry.is_some_and(|expires| expires.0 < now_micros.0) {
                report.expired += 1;
                continue;
            }
            // The dial-policy gate holds every pass, before any witness work —
            // a row already at rest (or rewritten under an older door) never
            // reaches `connect()`. At NEST scope, so a public
            // deployment does not inherit the device's plaintext-loopback
            // carve-out.
            if let Err(reason) = fauna_core::counterparty_url::validate_counterparty_nest_url_scoped(
                &row.owner_nest_url,
                fauna_core::counterparty_url::DialScope::Nest { public_deployment },
            ) {
                tracing::warn!(
                    "custody hosting pump: owner_nest_url refused by dial policy \
                     ({reason}) — not dialed"
                );
                report.refused_url += 1;
                continue;
            }
            let witness: fauna_core::encoding::EmbedAsBytes =
                match fauna_core::encoding::canonical_decode(&row.witness) {
                    Ok(w) => w,
                    Err(e) => {
                        tracing::warn!(
                            "custody hosting pump: hosting witness unreadable ({e}) — skipped"
                        );
                        report.failed += 1;
                        continue;
                    }
                };
            let scopes = match pullable_scopes(&witness) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        "custody hosting pump: witness scopes underivable ({e}) — skipped"
                    );
                    report.failed += 1;
                    continue;
                }
            };
            let owner: [u8; 32] = match row.owner_actor_id.as_slice().try_into() {
                Ok(o) => o,
                Err(_) => {
                    tracing::warn!(
                        owner = %hex::encode(&row.owner_actor_id),
                        "custody hosting pump: malformed owner id in hosting row — skipped"
                    );
                    report.failed += 1;
                    continue;
                }
            };
            let owner_hex = fauna_core::hex32::encode(&owner);
            let dir = root.join(hex::encode(&host)).join(&owner_hex);
            let store = match open_custodied_store(&dir, &owner_hex, nest_pub).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        owner = %owner_hex,
                        "custody hosting pump: custodied store unavailable ({e:#}) — skipped"
                    );
                    report.failed += 1;
                    continue;
                }
            };
            // A fresh session per pass — the handshake mints here, so a
            // revoked capability row takes its honest refusal at connect.
            let session = match NestLegSession::connect(
                &row.owner_nest_url,
                owner,
                signing_key.clone(),
                witness,
            )
            .await
            {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!(
                        owner = %owner_hex,
                        "custody hosting pump: the owner's nest refused or is unreachable: {e}"
                    );
                    report.failed += 1;
                    continue;
                }
            };
            // The cap is resolved BEFORE the pull (it reads only the row, the
            // tier and the other rows' last metering): the pull's segment half
            // adopts within it, so the host's bound holds on the
            // way in, not only after the budget pass below.
            let others_held = host_held.saturating_sub(row.held_bytes);
            let cap = fauna_core::custody_ceremony::effective_hosting_cap(
                row.retained_bytes_cap,
                tier_bound,
                others_held,
            );
            match pull_from_owner_nest(&store, &session, &owner, &scopes, Some(cap)).await {
                Ok(tally) => {
                    report.pulled += 1;
                    report.recorded += tally.recorded;
                    report.adopted_segments += tally.adopted_segments;
                }
                Err(e) => {
                    report.failed += 1;
                    tracing::debug!(
                        owner = %owner_hex,
                        "custody hosting pump: pull failed: {e:#}"
                    );
                }
            }
            // T15 runs unconditionally — a failed pull does not excuse the
            // budget (the client leg's rule), and the metering write-back is
            // what keeps the host UI's read honest either way.
            //
            // The bounds are re-applied HERE as well as at the register
            // door, and the redundancy is the point: this is the
            // enforcement backstop for a row already at rest — planted by any
            // writer that skips the door.
            //
            // `effective_hosting_cap` resolves all three: the row's own number,
            // the per-row ceiling, and the headroom the host's TIER leaves after
            // its other rows (`others_held`, derived from the same metered
            // figures — no second ledger). It returns an unambiguous budget, so
            // it is passed as `Some(..)`: a tier squeeze legitimately reaches 0,
            // and `Some(0)` means "hold the floor, no payload" where a bare 0
            // would have meant "no cap recorded" and handed the row 8 GiB.
            match meter_and_evict(&store, Some(cap)).await {
                Ok(budget) => {
                    if let Err(e) = self
                        .state
                        .db
                        .update_custody_hosting_metering(&host, &row.grant_id, budget.held_bytes)
                        .await
                    {
                        tracing::warn!(
                            owner = %owner_hex,
                            "custody hosting pump: metering write-back failed: {e:#}"
                        );
                    }
                    // ── Stage (c): the receipt leg, riding the same pass and
                    // the same custody bearer. Bookkeeping advances ONLY on
                    // an acked deposit, so a failed deposit stays due and
                    // redrives next pass.
                    let now = fauna_core::data::Timestamp(
                        fauna_core::data::Timestamp::now_millis().saturating_mul(1000),
                    );
                    let receipt = budget.receipt(row.grant_id.clone(), owner, nest_pub, now);
                    let degraded = receipt.is_degraded();
                    if fauna_core::custody_receipt::receipt_due(
                        now,
                        fauna_core::data::Timestamp(row.last_receipt_at),
                        budget.evicted.bytes > 0,
                        degraded,
                        row.last_receipt_degraded,
                    ) {
                        match self
                            .deposit_receipt(&session, &row.grant_id, owner, &signing_key, &receipt)
                            .await
                        {
                            Ok(()) => {
                                report.receipts_deposited += 1;
                                if let Err(e) = self
                                    .state
                                    .db
                                    .record_custody_hosting_receipt(
                                        &host,
                                        &row.grant_id,
                                        receipt.attested_at.0,
                                        degraded,
                                    )
                                    .await
                                {
                                    tracing::warn!(
                                        owner = %owner_hex,
                                        "custody hosting pump: receipt bookkeeping failed: {e:#}"
                                    );
                                }
                            }
                            Err(e) => {
                                report.receipts_failed += 1;
                                tracing::debug!(
                                    owner = %owner_hex,
                                    "custody hosting pump: receipt deposit failed \
                                     (stays due, redrives next pass): {e:#}"
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        owner = %owner_hex,
                        "custody hosting pump: budget pass failed: {e:#}"
                    );
                }
            }
            session.disconnect().await;
        }

        // ── The expired-store GC: reclaim the `(host, owner)` pairs
        // whose EVERY row sat expired past the grace window. All bytes under
        // the pair's dir are derived and re-pullable — the owner's nest holds
        // the canonical planes, and a re-minted witness re-pulls them — so
        // dropping the dir loses nothing a user cannot recreate (the
        // ephemeral-drop rule's why-recreatable). The rows themselves stay:
        // they are the host's visible record ("expired"), and removal is the
        // host's (or the admin's) explicit act through the remove doors.
        for (pair, reclaim) in pair_reclaimable {
            if !reclaim {
                continue;
            }
            let (host, owner_bytes) = pair;
            let Ok(owner) = <[u8; 32]>::try_from(owner_bytes.as_slice()) else {
                continue;
            };
            let dir = root
                .join(hex::encode(&host))
                .join(fauna_core::hex32::encode(&owner));
            match tokio::fs::remove_dir_all(&dir).await {
                Ok(()) => {
                    report.stores_reclaimed += 1;
                }
                // Never held any bytes — nothing to reclaim, nothing to zero.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    tracing::warn!(
                        dir = %dir.display(),
                        error = %e,
                        "custody hosting pump: expired-store reclaim failed — retried next pass"
                    );
                    continue;
                }
            }
            // The metered figure is derived from the store; with the store
            // gone the rows' held_bytes must read 0, or the host's tier
            // headroom and UI would count bytes no longer held.
            for grant_id in pair_grants
                .get(&(host.clone(), owner_bytes.clone()))
                .into_iter()
                .flatten()
            {
                if let Err(e) = self
                    .state
                    .db
                    .update_custody_hosting_metering(&host, grant_id, 0)
                    .await
                {
                    tracing::warn!(
                        host = %hex::encode(&host),
                        "custody hosting pump: reclaim metering write-back failed: {e:#}"
                    );
                }
            }
        }
        Ok(report)
    }

    /// Sign a due receipt under the nest identity and deposit it at the
    /// owner's nest over the SAME custody-bearer session the pull rode —
    /// `fauna.custody.receipt.deposit`, a custodian-class call, so revocation
    /// severs the deposit exactly where it severs the pull. A not-newer reply
    /// (`staged: false`) still counts as acked: the owner's nest already
    /// holds something at least as fresh.
    async fn deposit_receipt(
        &self,
        session: &NestLegSession,
        grant_id: &[u8],
        owner: [u8; 32],
        signing_key: &ed25519_dalek::SigningKey,
        receipt: &fauna_core::custody_receipt::CustodyReceipt,
    ) -> anyhow::Result<()> {
        use fauna_protocol::RpcRequester;
        let custodian = fauna_core::identity::ActorKeypair::from_secret(signing_key.to_bytes());
        let env = fauna_core::custody_receipt::sign_custody_receipt(&custodian, receipt)
            .map_err(|e| anyhow::anyhow!("sign receipt: {e}"))?;
        let bytes = fauna_core::encoding::canonical_encode(&env)
            .map_err(|e| anyhow::anyhow!("encode receipt: {e}"))?;
        let reply: fauna_protocol::custody::ReceiptDepositReply = session
            .client()
            .request(
                fauna_protocol::custody::RECEIPT_DEPOSIT_KIND,
                fauna_protocol::custody::ReceiptDepositRequest {
                    owner_actor_id: fauna_core::hex32::encode(&owner),
                    grant_id: fauna_protocol::ByteBuf::from(grant_id.to_vec()),
                    receipt: fauna_protocol::ByteBuf::from(bytes.to_vec()),
                    extra: Default::default(),
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!("deposit rpc: {e}"))?;
        if !reply.ok {
            anyhow::bail!("owner's nest refused the deposit");
        }
        Ok(())
    }

    /// Spawn the loop. First tick at boot (a restarted nest resumes its holds
    /// immediately); each sweep on the **blocking pool** driven by this
    /// runtime's handle — the same two reasons as
    /// [`crate::segment_backup::NestBackupWorker::spawn`]: a pass is
    /// blocking-shaped (SQLite store work), and the store futures are `!Send`
    /// by construction (rusqlite).
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        // spawn-ok(returns-handle-for-scope): the caller adopts this handle via `AppState::scope_handle`
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(self.interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                if !periodic_pass_allowed(&self.state) {
                    continue;
                }
                let state = Arc::clone(&self.state);
                let sweep = tokio::task::spawn_blocking(move || {
                    let worker = CustodyHostingWorker::new(state, std::time::Duration::MAX);
                    tokio::runtime::Handle::current().block_on(worker.run_once())
                })
                .await;
                match sweep {
                    Ok(Ok(report)) => {
                        if report.rows > 0 {
                            tracing::debug!(?report, "custody hosting pump: pass complete");
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::warn!("custody hosting pump: pass failed: {e:#}");
                    }
                    Err(e) => {
                        tracing::warn!("custody hosting pump: pass join error: {e}");
                    }
                }
            }
        })
    }
}

/// Whether the periodic loop may run its pass on this tick. Always `true` in
/// production; a `test-hooks` build lets a test hold the loop
/// ([`AppState::custody_hosting_periodic_held`]) so the only passes are its
/// own `run-now` pokes — never a knob anywhere else.
fn periodic_pass_allowed(state: &AppState) -> bool {
    #[cfg(feature = "test-hooks")]
    {
        !state
            .custody_hosting_periodic_held
            .load(std::sync::atomic::Ordering::SeqCst)
    }
    #[cfg(not(feature = "test-hooks"))]
    {
        let _ = state;
        true
    }
}

/// Open (creating on first use) the keyless per-owner custodied store under
/// `<root>/<host_hex>/<owner_hex>/`. The writer identity tag is the NEST's
/// own key — the custodied store never authors, exactly like the
/// custodian-device leg's stores.
async fn open_custodied_store(
    dir: &std::path::Path,
    owner_hex: &str,
    nest_pub: [u8; 32],
) -> Result<AccountStore<SqliteBackend>> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let backend = SqliteBackend::open(dir).context("open custodied store backend")?;
    AccountStore::open(backend, owner_hex, WriterId(nest_pub))
        .await
        .context("open custodied store")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test hold gates the periodic loop only — and only while set.
    #[cfg(feature = "test-hooks")]
    #[test]
    fn the_test_hold_gates_the_periodic_pass() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        let state = crate::routes::AppState::for_test(db);
        assert!(periodic_pass_allowed(&state), "unheld by default");
        state
            .custody_hosting_periodic_held
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(
            !periodic_pass_allowed(&state),
            "held: the tick skips its pass"
        );
        state
            .custody_hosting_periodic_held
            .store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(periodic_pass_allowed(&state), "released: passes resume");
    }

    /// — the pump's own fan-out backstop, witnessed against rows the
    /// register door never saw.
    ///
    /// The rows are planted straight through `put_custody_hosting`, which is
    /// exactly the threat model the backstop exists for: a row set that exceeds
    /// the per-host cap because some second writer skipped the door's check. Before this pass existed, all `cap + 2`
    /// rows reached `connect()` every 15 minutes.
    ///
    /// Latency-independent: one `run_once` poke, and every assertion reads the
    /// returned pass report — no clock.
    #[tokio::test]
    async fn the_pump_refuses_rows_past_the_host_registry_cap() {
        let cap = fauna_core::custody_ceremony::MAX_CUSTODY_HOSTING_ROWS_PER_HOST;
        let excess = 2usize;
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        let mut state = crate::routes::AppState::for_test(db.clone());
        state.custody_hosting_root = Some(tmp.path().to_path_buf());
        let state = std::sync::Arc::new(state);

        let host = [0x51u8; 32];
        let other_host = [0x52u8; 32];
        let owner = [0x53u8; 32];
        for i in 0..(cap + excess) {
            db.put_custody_hosting(
                &host,
                &[i as u8; 16],
                &owner,
                b"witness-bytes",
                "https://owner.test/",
                b"",
                4096,
                false,
            )
            .await
            .expect("plant a hosting row");
        }
        // A second host's single row proves the counter is PER HOST and resets
        // at the boundary — a global counter would refuse this one.
        db.put_custody_hosting(
            &other_host,
            &[0xEEu8; 16],
            &owner,
            b"witness-bytes",
            "https://owner.test/",
            b"",
            4096,
            false,
        )
        .await
        .expect("plant the second host's row");

        let worker = CustodyHostingWorker::new(state, std::time::Duration::from_secs(3600));
        let report = worker.run_once().await.expect("one pass");

        assert_eq!(
            report.rows,
            cap + excess + 1,
            "every planted row must be enumerated — the denominator stays honest"
        );
        assert_eq!(
            report.refused_fan_out, excess,
            "exactly the rows past this host's cap are refused, and the second host's row is \
             not among them"
        );
        assert_eq!(
            report.pulled, 0,
            "no row can pull in this rig (unreachable owner nest); the fan-out verdict must not \
             depend on that"
        );
    }

    /// A real owner-signed witness with a chosen expiry — the GC predicate
    /// reads the expiry through the signature check, so the fixture must be
    /// honestly signed.
    fn witness_expiring_at(
        owner: &fauna_core::identity::ActorKeypair,
        grant_id: &[u8],
        expires_at: fauna_core::data::Timestamp,
    ) -> Vec<u8> {
        let grant = fauna_core::custody_grant::CustodyGrant {
            grant_id: grant_id.to_vec(),
            owner: owner.actor_id(),
            custodian_key: [0xC0; 32],
            scopes: fauna_core::custody_grant::CustodyScopeSet::Account,
            minted_at: fauna_core::data::Timestamp(0),
            expires_at,
            removed_devices: Vec::new(),
        };
        fauna_core::encoding::canonical_encode(
            &fauna_core::custody_grant::sign_custody_grant(owner, &grant).expect("sign"),
        )
        .expect("encode")
        .to_vec()
    }

    /// The expired-store GC, all four verdicts in one pass — every
    /// assertion latency-independent (expiry deltas are ±days against a
    /// millisecond test; no clock is awaited):
    ///
    /// - a pair whose EVERY row is expired past the grace loses its store dir
    ///   and its metered figure reads 0;
    /// - a pair expired but within the grace keeps its bytes (and is counted
    ///   `expired` — never dialed);
    /// - a pair with one live grant beside an expired-past-grace one keeps
    ///   its bytes;
    /// - a STOPPED row with a live witness keeps its bytes — stop is a pause,
    ///   not a reclaim (the row's success criterion).
    #[tokio::test]
    async fn the_pump_reclaims_only_pairs_expired_past_the_grace() {
        use fauna_core::custody_ceremony::HOSTING_EXPIRED_STORE_GC_GRACE_SECS;
        use fauna_core::data::Timestamp;

        let tmp = tempfile::tempdir().expect("tempdir");
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        let mut state = crate::routes::AppState::for_test(db.clone());
        state.custody_hosting_root = Some(tmp.path().to_path_buf());
        let state = std::sync::Arc::new(state);

        let host = [0x51u8; 32];
        let now_micros = Timestamp::now_millis().saturating_mul(1000);
        let grace_micros = HOSTING_EXPIRED_STORE_GC_GRACE_SECS * 1_000_000;
        let day_micros = 24 * 3600 * 1_000_000u64;
        let past_grace = Timestamp(now_micros.saturating_sub(grace_micros + day_micros));
        let within_grace = Timestamp(now_micros.saturating_sub(day_micros));
        let live = Timestamp(now_micros + 30 * day_micros);

        // Four owners = four (host, owner) pairs, each with its store dir.
        let owners: [([u8; 32], &str); 4] = [
            ([0xA1; 32], "reclaim"),
            ([0xA2; 32], "within-grace"),
            ([0xA3; 32], "one-live"),
            ([0xA4; 32], "stopped-live"),
        ];
        let dir_of = |owner: &[u8; 32]| {
            tmp.path()
                .join(hex::encode(host))
                .join(fauna_core::hex32::encode(owner))
        };
        for (owner, _) in &owners {
            std::fs::create_dir_all(dir_of(owner)).unwrap();
            std::fs::write(dir_of(owner).join("held.bytes"), b"custodied").unwrap();
        }
        let plant = |grant: [u8; 16], owner: [u8; 32], expires: Timestamp, stopped: bool| {
            let db = db.clone();
            async move {
                let owner_kp = fauna_core::identity::ActorKeypair::from_secret([0x61; 32]);
                db.put_custody_hosting(
                    &host,
                    &grant,
                    &owner,
                    &witness_expiring_at(&owner_kp, &grant, expires),
                    "https://owner.test/",
                    b"",
                    4096,
                    stopped,
                )
                .await
                .expect("plant");
            }
        };
        // Pair 1: a single row expired past grace — reclaimed. Metered bytes
        // planted first so the zeroing is observable.
        plant([0x01; 16], owners[0].0, past_grace, false).await;
        db.update_custody_hosting_metering(&host, &[0x01; 16], 4096)
            .await
            .unwrap();
        // Pair 2: expired but within the grace — kept.
        plant([0x02; 16], owners[1].0, within_grace, false).await;
        // Pair 3: one expired-past-grace + one live (stopped so this rig
        // never dials) — the live grant keeps the pair's bytes.
        plant([0x03; 16], owners[2].0, past_grace, false).await;
        plant([0x04; 16], owners[2].0, live, true).await;
        // Pair 4: stopped with a live witness — stop is a pause, bytes stay.
        plant([0x05; 16], owners[3].0, live, true).await;

        let worker = CustodyHostingWorker::new(state, std::time::Duration::from_secs(3600));
        let report = worker.run_once().await.expect("one pass");

        assert_eq!(report.stores_reclaimed, 1, "{report:?}");
        assert!(
            !dir_of(&owners[0].0).exists(),
            "the all-expired-past-grace pair's store is gone"
        );
        for (owner, why) in &owners[1..] {
            assert!(dir_of(owner).exists(), "{why}: bytes must survive");
        }
        // Expired rows never dialed: pairs 1 (past grace), 2 (within), and
        // 3's expired half — the stopped rows are counted as stopped instead.
        assert_eq!(report.expired, 3, "{report:?}");
        assert_eq!(report.stopped, 2, "{report:?}");
        // The reclaimed pair's metered figure reads 0 — the held-bytes sum is
        // derived, and the store it derived from is gone.
        let rows = db.list_custody_hosting(&host).await.unwrap();
        let reclaimed = rows.iter().find(|r| r.grant_id == [0x01; 16]).unwrap();
        assert_eq!(reclaimed.held_bytes, 0, "zeroed with the reclaim");
        // The rows themselves all stand — GC reclaims bytes, never the
        // host's visible record; removal is the remove doors' explicit act.
        assert_eq!(rows.len(), 5);
    }
}
