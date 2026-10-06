//! **The let-go** — a dead generation's rows are retired by the user, and by
//! nothing else (`account-data-taxonomy.md` § The generation machinery →
//! *Fleet-scope reclamation*, clause (3)(j); the surface is
//! `docs/goal/ui/settings.md` § Recovery kit).
//!
//! A generation is **dead** when no device keys it and no holder wraps it. The
//! rows sealed under it count against the fleet scope's cap for as long as
//! they rest, and no reclamation pass may retire them: a device of the user's,
//! asleep since before the loss, may still key the generation and read every
//! row under it the day it is signed in again, and nothing a client or the
//! nest can observe tells that device apart from one that never existed. So
//! the decision is the user's. This module is its two halves:
//!
//! - [`dead_generations`] — the read the surface renders: each generation the
//!   fleet plane's relay rows are sealed under (their cleartext form-v2
//!   header, `relay_rows.generation_id`) that is not `Shredded` (clause
//!   (3)(h) already forgets those), that this device does not key, that no
//!   other verified member's reach lists, and that the bound holder does not
//!   wrap — the durable answered-empty bit where the escrow recovery already
//!   asked, else one filtered `fauna.generation.escrow.get` asked here, whose
//!   empty answer is the same observation. Each comes with its mint stamp
//!   where merged state carries the mint record, and its live-row count —
//!   the rows the bound nest's last listing still shows. A copy it no longer
//!   shows was let go by another replica, and the read forgets it.
//! - [`let_go`] — the act, on the user's confirm: the dead predicate is
//!   re-read (the gate is checked when the act fires, never only at render),
//!   then every relay row sealed under each named generation is retired by
//!   its coordinates through the existing retire door — never opened; the
//!   nest's retention gate applies as to any retire, and the store's retire
//!   record carries each to the linked nests — then the generation's own mint
//!   rows (where this device's schedule names them) behind the dataless belt,
//!   then its escrow wraps are deleted at the bound holder, an idempotent
//!   no-op where none rest, and the generation joins the store's let-go set,
//!   which the secondary leg reads to delete them at every linked holder too
//!   (`crate::linked_leg`). The relay copies of what the nest confirmed gone
//!   are forgotten, as clause (3)(h) forgets a shredded generation's.
//!
//! Generation-sealed rows live in the fleet scope alone (the nest refuses a
//! generation belt on any other — `fauna.account.state.retire`), so the read
//! and the act take the fleet plane only.
//!
//! **Compatibility:** an existing retire and an existing delete, from a new
//! caller; no wire change. The let-go set is a new replica-local meta key.

use std::collections::BTreeSet;

use anyhow::Result;
use ed25519_dalek::SigningKey;
use fauna_account_store::{backend::StoreBackend, store::AccountStore};
use fauna_protocol::generation_escrow::{EscrowGetReply, EscrowGetRequest, KIND_ESCROW_GET};
use fauna_protocol::merge_policy::{KIND_ESCROW_RECEIPT, KIND_GENERATION_MINT};
use fauna_protocol::{ByteBuf, RpcErrorClass, RpcRequester};

use crate::account_state_plane::{AccountStatePlane, RetireOutcome, withheld_by_gate};
use crate::generation_reclaim::{ReclaimState, receipt_generation};
use crate::generation_store::live_rows;
use crate::generation_tip::{self, GenerationTrust};

/// One dead generation, as the surface lists it: the data sealed under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadGeneration {
    /// The generation id its rows' cleartext headers name.
    pub generation_id: [u8; 32],
    /// Its mint stamp (`MintCore::minted_at_ms`), where merged state carries
    /// a live `Minted` record for it; `None` where this device cannot open
    /// one (a predecessor's mint no carried key reaches).
    pub minted_at_ms: Option<i64>,
    /// The live relay rows sealed under it — what letting it go retires.
    pub live_rows: u64,
}

/// The i18n key of the unreadable-data line when a mint date is known.
pub const KEY_UNREADABLE_STATUS: &str = "settings.recovery_kit.unreadable_status";
/// …and when no listed generation's mint record is readable here.
pub const KEY_UNREADABLE_STATUS_UNDATED: &str = "settings.recovery_kit.unreadable_status_undated";
/// The confirm word `recovery-kit-let-go-confirm-field` must hold before
/// `recovery-kit-let-go-button` arms — one constant for all seven apps, and
/// re-checked when the act fires.
pub const LET_GO_CONFIRM_WORD: &str = "LET GO";

/// The `recovery-kit-unreadable-status` line (`settings.md` § Recovery kit):
/// how many rows the dead generations hold and, where any mint record is
/// readable, the earliest mint date — the one thing the client cannot know
/// is whether a device not signed in since then still reads them, and the
/// copy says so. `None` when nothing is dead: the line, its confirm field and
/// its button render only while this answers `Some`. `format_date` renders an
/// epoch-millisecond instant as the app's local date (native Rust apps pass
/// `fauna_core::format::format_unix_local_date_ms`).
pub fn unreadable_status(
    dead: &[DeadGeneration],
    format_date: impl Fn(i64) -> String,
) -> Option<fauna_core::localized::LocalizedText> {
    use fauna_core::localized::LocalizedText;
    if dead.is_empty() {
        return None;
    }
    let rows: u64 = dead.iter().map(|d| d.live_rows).sum();
    Some(match dead.iter().filter_map(|d| d.minted_at_ms).min() {
        Some(since) => LocalizedText::key_args(
            KEY_UNREADABLE_STATUS,
            [("rows", rows.to_string()), ("since", format_date(since))],
        ),
        None => LocalizedText::key_arg(KEY_UNREADABLE_STATUS_UNDATED, "rows", rows.to_string()),
    })
}

/// What one [`let_go`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LetGoReport {
    /// The generations acted on: the requested ones the predicate still read
    /// dead when the act fired.
    pub let_go: Vec<[u8; 32]>,
    /// Requested generations that no longer read dead — a device keyed one,
    /// a holder wraps one — and were left untouched.
    pub refused: Vec<[u8; 32]>,
    /// Rows the nest retired (or already held no more).
    pub retired: usize,
    /// Rows the nest's gate withheld or deferred: still live, retired by a
    /// later let-go while the generation still reads dead.
    pub deferred: usize,
    /// Generations whose wraps the bound holder's delete door accepted.
    pub wraps_deleted: usize,
    /// The nest serves no retire kind: nothing was retired.
    pub unsupported: bool,
}

/// The dead generations of this account's fleet scope (module docs). Asks
/// the bound holder at most once per candidate whose answered-empty bit is
/// not set. Its one write is local: a dead generation's relay copy that the
/// bound nest's last listing ([`AccountStatePlane::listing`]) no longer shows
/// at its coordinate was let go by another replica, and is forgotten rather
/// than counted.
///
/// # Errors
///
/// Store I/O, a key lookup, or a holder that could not be asked — a
/// generation whose holder answer is unknown is never read dead.
pub async fn dead_generations<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
) -> Result<Vec<DeadGeneration>>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let me = writer_key.verifying_key().to_bytes();
    let state = ReclaimState::read(store, trust, me).await?;
    let answered_empty = store.unkeyed(fleet.scope()).await?;
    let listing = fleet.listing();
    let mut generations = store.relay_generations(fleet.scope()).await?;
    generations.sort_unstable();
    let mut dead = Vec::new();
    for g in generations {
        crate::pass_breath::pass_breath().await;
        if state.is_shredded(&g) || state.a_sibling_reach_lists(&g) {
            continue;
        }
        if generation_tip::generation_key_for(store, &g, writer_key, fleet.generation_custody())
            .await?
            .is_some()
        {
            continue;
        }
        if answered_empty.get(&g) != Some(&true) && holder_wraps(fleet.requester(), &g).await? {
            continue;
        }
        let mut live_rows = 0u64;
        for row in store.relay_rows_sealed_under(fleet.scope(), &g).await? {
            // A copy the bound nest's last listing no longer shows at its
            // coordinate was retired there — another replica's let-go — so it
            // is forgotten here, as clause (3)(h) forgets a shredded
            // generation's residue. No sibling's diff can put it back: a row
            // no device opens is never pushed (`crate::publish_diff`).
            let retired_at_the_nest = listing.as_ref().is_some_and(|listing| {
                <[u8; 32]>::try_from(row.item_key.as_slice())
                    .is_ok_and(|item| listing.get(&(row.writer, item)) != Some(&row.writer_seq))
            });
            if retired_at_the_nest {
                store
                    .relay_forget(fleet.scope(), &row.writer, &row.item_key)
                    .await?;
            } else {
                live_rows += 1;
            }
        }
        if live_rows == 0 {
            continue;
        }
        dead.push(DeadGeneration {
            generation_id: g,
            minted_at_ms: state.minted_core(&g).map(|core| core.minted_at_ms),
            live_rows,
        });
    }
    Ok(dead)
}

/// Does the bound holder serve any wrap of `g`? One filtered get; any wrap at
/// all counts — a wrap this identity cannot open may still be another's way
/// back (the kept wrap), so only an empty answer reads "wrapped by no holder".
async fn holder_wraps<R: RpcRequester>(rpc: &R, g: &[u8; 32]) -> Result<bool> {
    let reply: EscrowGetReply = rpc
        .request(
            KIND_ESCROW_GET,
            EscrowGetRequest {
                generation_id: Some(ByteBuf::from(g.to_vec())),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "asking the holder for generation {}'s wraps: {e}",
                fauna_core::hex32::encode(g)
            )
        })?;
    Ok(!reply.wraps.is_empty())
}

/// Let go of `requested` (module docs): the user's confirmed act.
///
/// # Errors
///
/// Store I/O, the dead read, or a retire or delete the nest failed outright
/// (the gate's and the belt's verdicts are counted, never errors). What
/// landed before the error stays landed; the act is idempotent, so a repeat
/// finishes it.
pub async fn let_go<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
    requested: &BTreeSet<[u8; 32]>,
) -> Result<LetGoReport>
where
    B: StoreBackend,
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let dead: BTreeSet<[u8; 32]> = dead_generations(store, fleet, trust, writer_key)
        .await?
        .into_iter()
        .map(|d| d.generation_id)
        .collect();
    let mut report = LetGoReport::default();
    for g in requested {
        if dead.contains(g) {
            report.let_go.push(*g);
        } else {
            report.refused.push(*g);
        }
    }
    if report.let_go.is_empty() {
        return Ok(report);
    }
    // Recorded before any row moves: the linked holders' wraps are owed from
    // the user's confirm on, whatever the bound nest answers below.
    store
        .add_let_go(&report.let_go.iter().copied().collect())
        .await?;
    let watermark = fleet.retirable_through_seq();
    for g in report.let_go.clone() {
        // 1. Every row sealed under it, by its coordinates — never opened.
        let mut dataless = true;
        for row in store.relay_rows_sealed_under(fleet.scope(), &g).await? {
            crate::pass_breath::pass_breath().await;
            let Ok(item_key) = <[u8; 32]>::try_from(row.item_key.as_slice()) else {
                continue;
            };
            match retire(&mut report, fleet, watermark, &item_key, &row, None).await? {
                Some(true) => fleet.relay_forget(&row.writer, &item_key).await?,
                Some(false) => dataless = false,
                None => return Ok(report),
            }
        }
        // 2. Its machinery behind the dataless belt — the mint rows where this
        // device's schedule names them (a predecessor's stays, bound (α)) and
        // any receipt of it, whose belt also takes the nest's wraps.
        if dataless {
            let g_hex = fauna_core::hex32::encode(&g);
            let mut machinery: Vec<(&str, String, bool)> =
                vec![(KIND_GENERATION_MINT, g_hex.clone(), false)];
            for receipt in live_rows(store, KIND_ESCROW_RECEIPT).await? {
                if receipt_generation(&receipt.key) == Some(g) {
                    machinery.push((KIND_ESCROW_RECEIPT, receipt.key, true));
                }
            }
            for (kind, key, sweep) in machinery {
                let Some(item_key) = fleet.gen0_item_key(kind, &key) else {
                    continue;
                };
                let mut settled = true;
                for row in fleet.relay_rows_at(&item_key).await? {
                    match retire(
                        &mut report,
                        fleet,
                        watermark,
                        &item_key,
                        &row,
                        Some((&g, sweep)),
                    )
                    .await?
                    {
                        Some(true) => fleet.relay_forget(&row.writer, &item_key).await?,
                        Some(false) => settled = false,
                        None => return Ok(report),
                    }
                }
                if settled {
                    store.forget_state(kind, &key).await?;
                }
            }
        }
        // 3. Its wraps at the bound holder — idempotent where none rest.
        crate::linked_leg::sweep_wraps(fleet.requester(), &g).await?;
        report.wraps_deleted += 1;
    }
    tracing::info!(
        let_go = report.let_go.len(),
        refused = report.refused.len(),
        retired = report.retired,
        deferred = report.deferred,
        wraps_deleted = report.wraps_deleted,
        "generation let-go"
    );
    Ok(report)
}

/// One retire. `Some(true)`: off the feed (retired, or already gone);
/// `Some(false)`: the gate withheld or deferred it; `None`: the nest serves
/// no retire kind, so the act stops.
async fn retire<B, R>(
    report: &mut LetGoReport,
    fleet: &AccountStatePlane<'_, B, R>,
    watermark: Option<u64>,
    item_key: &[u8; 32],
    row: &fauna_account_store::types::RelayRow,
    belt: Option<(&[u8; 32], bool)>,
) -> Result<Option<bool>>
where
    B: StoreBackend,
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    if withheld_by_gate(watermark, row.feed_seq) {
        report.deferred += 1;
        return Ok(Some(false));
    }
    let (no_rows_sealed_under, delete_escrow_wraps) = match belt {
        Some((g, sweep)) => (Some(g), sweep),
        None => (None, false),
    };
    match fleet
        .retire(
            item_key,
            &row.writer,
            row.writer_seq,
            no_rows_sealed_under,
            delete_escrow_wraps,
        )
        .await?
    {
        RetireOutcome::Retired => {
            report.retired += 1;
            Ok(Some(true))
        }
        RetireOutcome::Gone => Ok(Some(true)),
        RetireOutcome::NotYetStable | RetireOutcome::GenerationInUse => {
            report.deferred += 1;
            Ok(Some(false))
        }
        RetireOutcome::Unsupported => {
            report.unsupported = true;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dead(g: u8, minted_at_ms: Option<i64>, live_rows: u64) -> DeadGeneration {
        DeadGeneration {
            generation_id: [g; 32],
            minted_at_ms,
            live_rows,
        }
    }

    /// The line renders only while something is dead, sums the rows, names
    /// the earliest readable mint date, and falls back to the undated copy
    /// when no mint record is readable here.
    #[test]
    fn the_unreadable_line_counts_rows_and_names_the_earliest_mint() {
        let fmt = |ms: i64| format!("day{ms}");
        assert_eq!(unreadable_status(&[], fmt), None);
        let line = unreadable_status(
            &[dead(1, Some(9), 3), dead(2, Some(4), 8), dead(3, None, 1)],
            fmt,
        )
        .expect("something is dead");
        assert_eq!(line.key, KEY_UNREADABLE_STATUS);
        assert_eq!(line.args["rows"], "12");
        assert_eq!(line.args["since"], "day4");
        let undated = unreadable_status(&[dead(3, None, 11)], fmt).expect("dead");
        assert_eq!(undated.key, KEY_UNREADABLE_STATUS_UNDATED);
        assert_eq!(undated.args["rows"], "11");
    }
}
