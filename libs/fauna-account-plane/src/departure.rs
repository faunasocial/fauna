//! **Scope departure** — T2 transition 3, the judgment half.
//!
//! Owner: `docs/goal/architecture/account-data-plane.md` § The replica
//! boundary → T2 transition (3): leaving a channel, a shared set unshared or a
//! membership revoked "ends the subscription and drops that scope's items from
//! the replica — the data belonged to the membership."
//!
//! [`crate::scope_set`] built the *subscription* half — a left channel stops
//! being walked, because the pump re-derives the set every pass. This module
//! is the other half, and finding is why it is a module rather
//! than a branch in that one: **it is a deletion path with its own rules, not
//! the inverse of the derivation.** Deriving wrongly costs a pass; deleting
//! wrongly costs data.
//!
//! # Departure is concluded, never inferred
//!
//! The store cannot answer "did I leave this?" — it knows only what it holds,
//! and everything it holds looks alike. So the replica keeps an explicit
//! **subscription marker** ([`META_SUBSCRIBED`]): the scope set it last
//! affirmatively subscribed to. A departure is `marker − fresh`, and *only*
//! that. Nothing is ever dropped because it is absent from a derivation the
//! marker never confirmed.
//!
//! # The three refusals
//!
//! Each one exists because its absence deletes a user's data, and each is a
//! shape rather than a check where that was possible:
//!
//! 1. **No affirmative membership answer this pass → drop nothing.** The
//!    `fauna_sync_engine::account_runtime::MembershipSource` contract
//!    is that `None` means *cannot tell right now*, not *no channels* — a
//!    still-loading MLS engine answering empty would otherwise read as "left
//!    everything". For the walk set that mistake costs a skipped pass; here it
//!    would cost every channel's local content, so the runtime passes the
//!    answer's affirmativeness down and this module refuses without it.
//!    Deliberately per-pass, not sticky-since-startup: a source that answered
//!    once and then went unavailable is unavailable *now*.
//! 2. **No membership source at all → drop nothing.** Every host wires one
//!    today (`fauna-ffi`, `fauna-wasm`, `apps/fauna-linux`, `apps/fauna-tui`),
//!    but a runtime started without one derives no `conv` scope at all.
//!    Treating that as authoritative would mean such a runtime deleted every
//!    channel's content — the refusal above covers it by construction, since a
//!    source that does not exist cannot answer affirmatively.
//! 3. **A registered scope is never a departure.** Scopes an app registered
//!    explicitly are unioned into every derivation, so they are always in
//!    `fresh` and can never appear in the difference. Stated here because the
//!    property is load-bearing, not incidental.
//!
//! A fourth, quieter one: own-actor scopes are pure functions of the actor id
//! (`scope_set::derive_own_actor_scopes`), so they are in every derivation for
//! as long as the account exists. Departure applies to memberships only, which
//! is exactly what the charter says the transition is about.
//!
//! And a fifth, the class-2 rails: **`state` and `state-fleet` are never
//! memberships, so they are never seeded into the marker and never dropped.**
//! Found 2026-09-15, the day the e2e relaunch carry started restoring the
//! account-store replica beside the writer key: the first-run seed widened the
//! marker from every scope holding a frontier and excluded only `state`, so a
//! replica whose `state-fleet` had been walked before its first affirmative
//! pass carried the fleet rail in its marker and DROPPED it — device set,
//! escrow target, every fleet-only row — on the very next pass. A fresh store
//! per relaunch had masked it (the walk re-ingested the rows as though another
//! replica wrote them); with the journal restored, the walk read the nest's
//! own rows above a journal that held nothing and rotated the writer as burnt
//! (`account-replica-posture.md` § The store device principal, refinement
//! 11). The rail filter runs on the departed set too, not only on the seed,
//! because markers written by earlier builds already carry the fleet rail.
//!
//! # First run
//!
//! An upgrade meets a store with no marker. Seeding it from the fresh set
//! alone would forget scopes this replica already holds, so a later leave of
//! one of them would never be detected. The seed is therefore
//! `fresh ∪ (scopes the store holds)` — which only ever *widens* the marker,
//! and widening is safe: a wider marker can produce a spurious departure only
//! for a scope the account genuinely is not in.
//!
//! # The departed list
//!
//! A departure drops the scope's content, and the scope's two delegable items
//! — its seen-set entry and its read marker — stay in the store
//! (`delegable-scope-reclamation.md` § Delegable-scope reclamation, part
//! (6)). Their *rows at a nest* go: the cover step retires them, and the
//! publish and the diff send none. All three read [`departed_scopes`], a
//! device-local `store_meta` list beside the subscription marker
//! (`account-replica-posture.md` § The replica boundary, T2 transition (3) →
//! *What the replica remembers of a departure*). A scope enters it when a pass
//! concludes its departure, and leaves it when an affirmative answer names it
//! again. It is seeded once, at the first affirmative pass under this build,
//! with every member scope this replica's own journal wrote an item row for
//! that the answer does not name: it wrote there, so it was in the scope.
//! Absence alone never lists a scope — the seed needs an own row, and a
//! conclusion needs the subscription marker.

use std::collections::BTreeSet;

use fauna_account_store::backend::{ScopeDropCounts, StoreBackend};
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::ItemRef;
use fauna_protocol::account_state::{ACCOUNT_STATE_FLEET_SCOPE, ACCOUNT_STATE_SCOPE};
use fauna_protocol::merge_policy::{KIND_READ_MARKER, KIND_SEEN_SET};
use fauna_protocol::scope::{ContentScope, Scope};

use crate::scope_set::{CONV_KIND, OWN_ACTOR_KINDS};

/// The class-2 rails — the account-state scope and its fleet-only partition
/// (`account-data-plane.md` § The generation machinery). Neither is a
/// membership: a marker naming one could only ever produce a departure that
/// deleted the account's own state.
fn is_class2_rail(scope: &str) -> bool {
    scope == ACCOUNT_STATE_SCOPE || scope == ACCOUNT_STATE_FLEET_SCOPE
}

/// `store_meta` key holding the scope set this replica last affirmatively
/// subscribed to — newline-separated scope strings, the framing
/// `ContentScope`'s hex-tailed `Display` makes safe without an encoder.
///
/// Local, deliberately: each device derives its own memberships from its own
/// MLS state, and each drops its own copy when that state says it left. A
/// synced marker would let one device's stale view delete another's data.
const META_SUBSCRIBED: &str = "subscribed_content_scopes";

/// `store_meta` key holding the member scopes this replica has concluded it
/// left and no affirmative answer has named since — the departed list (module
/// docs). Local for the subscription marker's reason.
const META_DEPARTED: &str = "departed_member_scopes";

/// `store_meta` key set once the departed list's one-time seed has run.
const META_DEPARTED_SEEDED: &str = "departed_member_scopes_seeded";

/// **The member scope a delegable item belongs to** — the one map from an
/// item to its scope that the departure pass, the cover step, the publish and
/// the diff all read. A read marker keyed `conv:<channel hex>` belongs to that
/// channel's `conv` scope; a seen-set entry belongs to the content scope its
/// key names when that scope is held by membership (its kind is not one of
/// [`OWN_ACTOR_KINDS`]). Preference records, the own-actor seen-set entries
/// and a key of any other shape belong to none: they never depart.
pub fn member_scope_of_item(kind: &str, key: &str) -> Option<String> {
    let scope = if kind == KIND_READ_MARKER {
        let hex = fauna_core::read_marker::channel_of_key(key)?;
        if !fauna_core::hex32::is_lowercase_hex64(hex) {
            return None;
        }
        let mut channel = [0u8; 32];
        hex::decode_to_slice(hex, &mut channel).ok()?;
        ContentScope::new(CONV_KIND, channel).ok()?
    } else if kind == KIND_SEEN_SET {
        match key.parse::<Scope>().ok()? {
            Scope::Content(scope) if !OWN_ACTOR_KINDS.contains(&scope.kind()) => scope,
            _ => return None,
        }
    } else {
        return None;
    };
    Some(scope.to_string())
}

/// The departed list (module docs): the member scopes whose delegable items'
/// rows leave the nest. Empty before any departure was concluded.
pub async fn departed_scopes<B: StoreBackend>(
    store: &AccountStore<B>,
) -> anyhow::Result<BTreeSet<String>> {
    read_scope_list(store, META_DEPARTED)
        .await
        .map(Option::unwrap_or_default)
}

/// Is the delegable item `(kind, key)` one of a departed scope's
/// ([`member_scope_of_item`] in `departed`)?
pub fn of_departed_scope(departed: &BTreeSet<String>, kind: &str, key: &str) -> bool {
    !departed.is_empty() && member_scope_of_item(kind, key).is_some_and(|s| departed.contains(&s))
}

/// What one departure pass did.
#[derive(Debug, Default, Clone)]
pub struct DepartureReport {
    /// Scopes dropped this pass, with what left the store for each.
    pub dropped: Vec<(String, ScopeDropCounts)>,
    /// Set when the pass refused to judge at all — no affirmative membership
    /// answer, or no actor. Carried rather than logged so a caller (and a
    /// test) can tell "nothing departed" from "not asked".
    pub skipped: Option<&'static str>,
    /// Per-scope failures, already labeled. One scope's trouble never blocks
    /// the rest, and never advances the marker past it.
    pub errors: Vec<String>,
}

/// Run one departure pass against `fresh`, the scope set just derived.
///
/// `membership_answered` is the pass's affirmativeness (refusal 1). Callers
/// that cannot supply it must pass `false` — the safe direction.
pub async fn drop_departed_scopes<B: StoreBackend>(
    store: &AccountStore<B>,
    fresh: &[ContentScope],
    membership_answered: bool,
) -> DepartureReport {
    let mut report = DepartureReport::default();
    if !membership_answered {
        report.skipped = Some("no affirmative membership answer this pass");
        return report;
    }
    if fresh.is_empty() {
        // No actor, or a derivation that refused: an empty fresh set would
        // make the whole marker look departed. Nothing legitimate produces it
        // — a live account always has its four own-actor scopes.
        report.skipped = Some("empty derived scope set");
        return report;
    }

    let fresh_strings: BTreeSet<String> = fresh.iter().map(ContentScope::to_string).collect();
    let marker = match read_marker(store).await {
        Ok(Some(marker)) => marker,
        // First run under this build: seed and judge nothing yet.
        Ok(None) => {
            if let Err(e) = settle_departed(store, &fresh_strings, &[]).await {
                report.errors.push(format!("departed list: {e:#}"));
            }
            let seed: BTreeSet<String> = fresh_strings
                .union(&held_scopes(store).await.unwrap_or_default())
                .cloned()
                .collect();
            if let Err(e) = write_marker(store, &seed).await {
                report
                    .errors
                    .push(format!("seed subscription marker: {e:#}"));
            }
            report.skipped = Some("subscription marker seeded this pass");
            return report;
        }
        Err(e) => {
            report
                .errors
                .push(format!("read subscription marker: {e:#}"));
            return report;
        }
    };

    // The rails are filtered here as well as at the seed: a marker an earlier
    // build seeded (before 2026-09-15) may carry `state-fleet`, and the
    // rewrite below — `fresh ∪ still_owed` — is what retires it.
    let departed: Vec<String> = marker
        .difference(&fresh_strings)
        .filter(|s| !is_class2_rail(s))
        .cloned()
        .collect();
    let mut still_owed = BTreeSet::new();
    for scope in departed {
        match store.drop_scope(&scope).await {
            Ok(counts) => report.dropped.push((scope, counts)),
            Err(e) => {
                // Keep it in the marker: an undropped scope must stay a
                // departure candidate, or one transient failure would strand
                // its rows forever.
                report.errors.push(format!("drop scope ({scope}): {e:#}"));
                still_owed.insert(scope);
            }
        }
    }

    let concluded: Vec<String> = report.dropped.iter().map(|(s, _)| s.clone()).collect();
    if let Err(e) = settle_departed(store, &fresh_strings, &concluded).await {
        report.errors.push(format!("departed list: {e:#}"));
    }

    let next: BTreeSet<String> = fresh_strings.union(&still_owed).cloned().collect();
    if next != marker
        && let Err(e) = write_marker(store, &next).await
    {
        report
            .errors
            .push(format!("advance subscription marker: {e:#}"));
    }
    report
}

/// Move the departed list on an affirmative pass: seed it once from the own
/// journal, take out every scope `fresh` names, and add the departures this
/// pass `concluded`. Written only when it changed.
async fn settle_departed<B: StoreBackend>(
    store: &AccountStore<B>,
    fresh: &BTreeSet<String>,
    concluded: &[String],
) -> anyhow::Result<()> {
    let held = read_scope_list(store, META_DEPARTED).await?;
    let mut list = held.clone().unwrap_or_default();
    let seeded = store
        .backend()
        .meta_get(META_DEPARTED_SEEDED)
        .await?
        .is_some();
    if !seeded {
        list.extend(own_member_scopes(store).await?);
    }
    list.retain(|scope| !fresh.contains(scope));
    list.extend(concluded.iter().cloned());
    if held.as_ref() != Some(&list) {
        write_scope_list(store, META_DEPARTED, &list).await?;
    }
    if !seeded {
        // After the list: a crash between the two re-runs a seed that comes
        // to the same list.
        store.backend().meta_put(META_DEPARTED_SEEDED, b"1").await?;
    }
    Ok(())
}

/// The member scopes this replica's own journal wrote an item row for on the
/// delegable scope — the departed list's one-time seed.
async fn own_member_scopes<B: StoreBackend>(
    store: &AccountStore<B>,
) -> anyhow::Result<BTreeSet<String>> {
    const PAGE: u32 = 512;
    let writer = store.writer();
    let mut scopes = BTreeSet::new();
    let mut after = 0;
    loop {
        let rows = store
            .scope_rows(ACCOUNT_STATE_SCOPE, &writer, after, PAGE)
            .await?;
        let Some(last) = rows.last() else { break };
        after = last.seq;
        for row in rows {
            if let ItemRef::StateKey { kind, key, .. } = &row.item
                && let Some(scope) = member_scope_of_item(kind, key)
            {
                scopes.insert(scope);
            }
        }
    }
    Ok(scopes)
}

async fn read_marker<B: StoreBackend>(
    store: &AccountStore<B>,
) -> anyhow::Result<Option<BTreeSet<String>>> {
    read_scope_list(store, META_SUBSCRIBED).await
}

async fn write_marker<B: StoreBackend>(
    store: &AccountStore<B>,
    scopes: &BTreeSet<String>,
) -> anyhow::Result<()> {
    write_scope_list(store, META_SUBSCRIBED, scopes).await
}

/// A scope list held under the `store_meta` key `key` — newline-separated
/// scope strings, the framing `ContentScope`'s hex-tailed `Display` makes
/// safe without an encoder.
async fn read_scope_list<B: StoreBackend>(
    store: &AccountStore<B>,
    key: &str,
) -> anyhow::Result<Option<BTreeSet<String>>> {
    let Some(raw) = store.backend().meta_get(key).await? else {
        return Ok(None);
    };
    Ok(Some(
        String::from_utf8_lossy(&raw)
            .lines()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
    ))
}

async fn write_scope_list<B: StoreBackend>(
    store: &AccountStore<B>,
    key: &str,
    scopes: &BTreeSet<String>,
) -> anyhow::Result<()> {
    let joined = scopes.iter().cloned().collect::<Vec<_>>().join("\n");
    store.backend().meta_put(key, joined.as_bytes()).await
}

/// Content scopes this replica actually holds rows for — the first-run seed's
/// widening term.
///
/// Read from the frontier plane, which has one row per scope the replica has
/// ever walked. The class-2 rails are excluded ([`is_class2_rail`]): they are
/// never memberships, and a marker naming one could only ever produce a
/// departure that deleted the account's own state.
async fn held_scopes<B: StoreBackend>(store: &AccountStore<B>) -> anyhow::Result<BTreeSet<String>> {
    Ok(store
        .backend()
        .scopes_with_frontiers()
        .await?
        .into_iter()
        .filter(|s| !is_class2_rail(s))
        .collect())
}

#[cfg(test)]
mod tests {
    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_account_store::types::{ItemRef, JournalOp, JournalRow, WriterId};
    use fauna_core::data::ContentHash;

    use super::*;

    const ACTOR: [u8; 32] = [0x0A; 32];

    async fn store() -> AccountStore<SqliteBackend> {
        AccountStore::open(
            SqliteBackend::open_in_memory().unwrap(),
            "0a0a",
            WriterId([7; 32]),
        )
        .await
        .unwrap()
    }

    fn conv(channel: u8) -> ContentScope {
        ContentScope::new(crate::scope_set::CONV_KIND, [channel; 32]).unwrap()
    }

    fn own() -> Vec<ContentScope> {
        crate::scope_set::derive_own_actor_scopes(ACTOR).unwrap()
    }

    /// Give `scope` a walked row + frontier, so it is genuinely "held".
    async fn walk_in(s: &AccountStore<SqliteBackend>, scope: &ContentScope) {
        let cid = ContentHash::of_raw(scope.to_string().as_bytes());
        s.ingest_row(&JournalRow {
            writer: WriterId::NEST_SEQUENCER,
            seq: 1,
            scope: scope.to_string(),
            op: JournalOp::RecordAdded,
            item: ItemRef::Cid(cid),
        })
        .await
        .unwrap();
        s.advance_frontier(&scope.to_string(), &WriterId::NEST_SEQUENCER, 1)
            .await
            .unwrap();
    }

    fn fresh(extra: &[ContentScope]) -> Vec<ContentScope> {
        let mut v = own();
        v.extend_from_slice(extra);
        v
    }

    /// The first pass under this build judges nothing — there is no record of
    /// what the replica *subscribed* to, only of what it holds, and those are
    /// not the same question.
    #[tokio::test]
    async fn the_first_pass_seeds_the_marker_and_drops_nothing() {
        let s = store().await;
        walk_in(&s, &conv(1)).await;

        // A derivation that does NOT include the held channel — the very shape
        // that would delete it if seeding judged.
        let report = drop_departed_scopes(&s, &fresh(&[]), true).await;
        assert!(report.dropped.is_empty(), "{report:?}");
        assert_eq!(report.skipped, Some("subscription marker seeded this pass"));

        // ...and the seed remembered the held scope, so the *next* pass can
        // detect its departure rather than being blind to it forever.
        let report = drop_departed_scopes(&s, &fresh(&[]), true).await;
        assert_eq!(
            report
                .dropped
                .iter()
                .map(|(s, _)| s.clone())
                .collect::<Vec<_>>(),
            vec![conv(1).to_string()],
            "the widened seed is what makes a pre-marker leave detectable"
        );
    }

    /// Give a class-2 rail a walked row + frontier, as a replica that has
    /// walked its fleet scope holds it.
    async fn walk_in_rail(s: &AccountStore<SqliteBackend>, rail: &str) {
        s.ingest_row(&JournalRow {
            writer: WriterId([0x33; 32]),
            seq: 1,
            scope: rail.to_string(),
            op: JournalOp::StatePut,
            item: ItemRef::StateKey {
                kind: "fauna.state.device-set".into(),
                key: "fleet".into(),
                entry_version: 1,
            },
        })
        .await
        .unwrap();
        s.advance_frontier(rail, &WriterId([0x33; 32]), 1)
            .await
            .unwrap();
    }

    /// The fifth refusal: the class-2 rails are never memberships. A replica
    /// whose fleet scope was walked before its first affirmative pass must
    /// not have the rail seeded into its marker (the 2026-09-15 shape: the
    /// restored replica lost its device set and escrow target to a spurious
    /// departure, and the walk then read the nest's own rows as a burnt
    /// journal) — and a marker an earlier build already seeded with the rail
    /// must be retired, never obeyed.
    #[tokio::test]
    async fn a_class2_rail_is_never_seeded_into_the_marker_nor_dropped_as_a_departure() {
        let s = store().await;
        walk_in_rail(&s, ACCOUNT_STATE_FLEET_SCOPE).await;
        walk_in_rail(&s, ACCOUNT_STATE_SCOPE).await;

        let report = drop_departed_scopes(&s, &fresh(&[]), true).await;
        assert_eq!(report.skipped, Some("subscription marker seeded this pass"));
        let marker = read_marker(&s).await.unwrap().unwrap();
        assert!(
            !marker.contains(ACCOUNT_STATE_FLEET_SCOPE) && !marker.contains(ACCOUNT_STATE_SCOPE),
            "the rails are not memberships: {marker:?}"
        );
        let report = drop_departed_scopes(&s, &fresh(&[]), true).await;
        assert!(report.dropped.is_empty(), "{report:?}");

        // A marker an earlier build seeded with the fleet rail.
        let mut stale = marker.clone();
        stale.insert(ACCOUNT_STATE_FLEET_SCOPE.to_string());
        write_marker(&s, &stale).await.unwrap();
        let report = drop_departed_scopes(&s, &fresh(&[]), true).await;
        assert!(
            report.dropped.is_empty(),
            "a rail in an old marker is retired, never dropped: {report:?}"
        );
        assert!(
            !read_marker(&s)
                .await
                .unwrap()
                .unwrap()
                .contains(ACCOUNT_STATE_FLEET_SCOPE),
            "the pass rewrote the marker without the rail"
        );
        assert_eq!(
            s.frontier(ACCOUNT_STATE_FLEET_SCOPE).await.unwrap().len(),
            1,
            "the fleet rail's rows and frontier are untouched"
        );
    }

    /// Refusal 1, at the unit: no affirmative answer, no judgment — and the
    /// marker is not advanced either, so the next affirmative pass still sees
    /// the true difference.
    #[tokio::test]
    async fn an_unanswered_pass_neither_drops_nor_advances_the_marker() {
        let s = store().await;
        drop_departed_scopes(&s, &fresh(&[conv(1)]), true).await; // seed
        let report = drop_departed_scopes(&s, &fresh(&[]), false).await;
        assert!(report.dropped.is_empty());
        assert_eq!(
            report.skipped,
            Some("no affirmative membership answer this pass")
        );
        assert!(
            read_marker(&s)
                .await
                .unwrap()
                .unwrap()
                .contains(&conv(1).to_string()),
            "the marker still names the channel, so a real leave is still detectable"
        );
    }

    /// An empty derived set is never legitimate — a live account always has its
    /// four own-actor scopes — so it is refused rather than read as "left
    /// everything". The belt to the `None` contract's braces.
    #[tokio::test]
    async fn an_empty_derived_set_is_refused_rather_than_obeyed() {
        let s = store().await;
        drop_departed_scopes(&s, &fresh(&[conv(1)]), true).await; // seed
        let report = drop_departed_scopes(&s, &[], true).await;
        assert!(report.dropped.is_empty(), "{report:?}");
        assert_eq!(report.skipped, Some("empty derived scope set"));
    }

    /// Own-actor scopes are a pure function of the account, so they are in
    /// every derivation and can never be a departure. Pinned because the
    /// consequence of getting it wrong is deleting the account's own mail.
    #[tokio::test]
    async fn own_actor_scopes_never_depart() {
        let s = store().await;
        for scope in own() {
            walk_in(&s, &scope).await;
        }
        drop_departed_scopes(&s, &fresh(&[conv(1)]), true).await; // seed
        let report = drop_departed_scopes(&s, &fresh(&[]), true).await;

        assert_eq!(report.dropped.len(), 1, "only the channel: {report:?}");
        for scope in own() {
            assert!(
                !s.frontier(&scope.to_string()).await.unwrap().is_empty(),
                "{scope} survived"
            );
        }
    }

    /// This device writes `kind` at `key` on the delegable scope — an own
    /// journal row, as a raise or the seen-set producer writes one.
    async fn write_own(s: &AccountStore<SqliteBackend>, kind: &str, key: &str) {
        s.put_state(fauna_account_store::types::StateEntry {
            kind: kind.to_string(),
            key: key.to_string(),
            scope: ACCOUNT_STATE_SCOPE.to_string(),
            value: vec![1],
            merge_meta: None,
            entry_version: 0,
            tombstone: false,
        })
        .await
        .unwrap();
    }

    fn marker_key(channel: u8) -> String {
        fauna_core::read_marker::channel_key(&hex::encode([channel; 32]))
    }

    /// The one map from an item to its member scope: a read marker and a
    /// member scope's seen-set entry name the channel's `conv` scope; an
    /// own-actor seen-set entry, a preference record and a key of another
    /// shape name none.
    #[test]
    fn an_item_maps_to_its_member_scope_and_only_a_members_item_does() {
        let want = Some(conv(1).to_string());
        assert_eq!(member_scope_of_item(KIND_READ_MARKER, &marker_key(1)), want);
        assert_eq!(
            member_scope_of_item(KIND_SEEN_SET, &conv(1).to_string()),
            want
        );
        for scope in own() {
            assert_eq!(
                member_scope_of_item(KIND_SEEN_SET, &scope.to_string()),
                None
            );
        }
        assert_eq!(
            member_scope_of_item("fauna.state.moderation", "default"),
            None
        );
        assert_eq!(member_scope_of_item(KIND_READ_MARKER, "other:abc"), None);
        assert_eq!(
            member_scope_of_item(KIND_READ_MARKER, &format!("conv:{}", "AB".repeat(32))),
            None,
            "an uppercase id spells another key"
        );
    }

    /// A concluded departure lists the scope; an affirmative answer that
    /// names it again takes it out.
    #[tokio::test]
    async fn a_concluded_departure_is_listed_until_an_answer_names_the_scope_again() {
        let s = store().await;
        drop_departed_scopes(&s, &fresh(&[conv(1), conv(2)]), true).await; // seed
        assert!(departed_scopes(&s).await.unwrap().is_empty());

        let report = drop_departed_scopes(&s, &fresh(&[conv(2)]), true).await;
        assert_eq!(report.dropped.len(), 1, "{report:?}");
        assert_eq!(
            departed_scopes(&s).await.unwrap(),
            BTreeSet::from([conv(1).to_string()])
        );

        // Unanswered, then answered without it: still listed.
        drop_departed_scopes(&s, &fresh(&[]), false).await;
        drop_departed_scopes(&s, &fresh(&[conv(2)]), true).await;
        assert_eq!(departed_scopes(&s).await.unwrap().len(), 1);

        drop_departed_scopes(&s, &fresh(&[conv(1), conv(2)]), true).await;
        assert!(
            departed_scopes(&s).await.unwrap().is_empty(),
            "the re-join took it out"
        );
    }

    /// The seed runs once, at the first affirmative pass: every member scope
    /// the own journal wrote an item for that the answer does not name.
    #[tokio::test]
    async fn the_list_is_seeded_once_from_the_own_journal() {
        let s = store().await;
        write_own(&s, KIND_READ_MARKER, &marker_key(1)).await;
        write_own(&s, KIND_SEEN_SET, &conv(2).to_string()).await;
        write_own(&s, KIND_READ_MARKER, &marker_key(3)).await;
        write_own(&s, KIND_SEEN_SET, &own()[0].to_string()).await;

        // Unanswered: no seed.
        drop_departed_scopes(&s, &fresh(&[]), false).await;
        assert!(departed_scopes(&s).await.unwrap().is_empty());

        drop_departed_scopes(&s, &fresh(&[conv(3)]), true).await;
        assert_eq!(
            departed_scopes(&s).await.unwrap(),
            BTreeSet::from([conv(1).to_string(), conv(2).to_string()]),
            "channel 3 is named, the own-actor entry is no member's"
        );

        // A re-join takes one out, and the seed does not run again.
        drop_departed_scopes(&s, &fresh(&[conv(1), conv(3)]), true).await;
        assert_eq!(
            departed_scopes(&s).await.unwrap(),
            BTreeSet::from([conv(2).to_string()])
        );
        // An own row written after the seed, for a scope never subscribed
        // here: the seed does not run again, so absence does not list it.
        write_own(&s, KIND_READ_MARKER, &marker_key(6)).await;
        drop_departed_scopes(&s, &fresh(&[conv(1), conv(3)]), true).await;
        assert_eq!(
            departed_scopes(&s).await.unwrap(),
            BTreeSet::from([conv(2).to_string()]),
            "the seed ran once"
        );
    }

    /// Absence is not departure: a scope the answer omits, which this replica
    /// never wrote a row for and never subscribed to, is never listed.
    #[tokio::test]
    async fn a_scope_merely_absent_from_the_answer_is_never_listed() {
        let s = store().await;
        // A sibling's marker row for channel 5, walked in: a row of the
        // item, but not this replica's own.
        s.ingest_row(&JournalRow {
            writer: WriterId([0x55; 32]),
            seq: 1,
            scope: ACCOUNT_STATE_SCOPE.to_string(),
            op: JournalOp::StatePut,
            item: ItemRef::StateKey {
                kind: KIND_READ_MARKER.into(),
                key: marker_key(5),
                entry_version: 1,
            },
        })
        .await
        .unwrap();
        for _ in 0..3 {
            drop_departed_scopes(&s, &fresh(&[conv(4)]), true).await;
        }
        assert!(
            departed_scopes(&s).await.unwrap().is_empty(),
            "channel 5 was never subscribed here and holds no own row"
        );
    }

    /// A drop that failed must stay a departure candidate: dropping it from
    /// the marker would strand its rows in the store forever, un-departed and
    /// un-walked.
    #[tokio::test]
    async fn a_scope_still_in_the_marker_is_retried_next_pass() {
        let s = store().await;
        walk_in(&s, &conv(1)).await;
        drop_departed_scopes(&s, &fresh(&[conv(1)]), true).await; // seed with it
        let first = drop_departed_scopes(&s, &fresh(&[]), true).await;
        assert_eq!(first.dropped.len(), 1);

        // Converged: the marker now equals the fresh set, so the next pass has
        // nothing to do — a departure is not re-run forever.
        let second = drop_departed_scopes(&s, &fresh(&[]), true).await;
        assert!(second.dropped.is_empty(), "{second:?}");
        assert_eq!(
            read_marker(&s).await.unwrap().unwrap().len(),
            own().len(),
            "the marker converged on the fresh set"
        );
    }
}
