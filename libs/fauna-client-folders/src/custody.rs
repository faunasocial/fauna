//! Shared-folder **content-key custody** — the owner's client-side state
//! transitions over the account's [`FoldersConfig`]: the READ fold of the
//! `fauna.state.folder-keys` rows on the account plane (`config-dissolution.md`
//! — the kinds table's row). Every transition keeps to what a merge-only store
//! can hold (`mls-group-key-material.md` § M2 → *Custody shape of the set nonce*,
//! ruling (l)): nothing is removed but a staged removal, whose clear is the
//! plane's settle.
//!
//! The **owner** (the binder, the sole writer in Slices 2–3) generates each
//! shared set's 32-byte content key at bind time and retains the authoritative
//! [`FolderContentKeys`] here — BackupKey-sealed on the account plane,
//! synced across the owner's device fleet, the same audience + seal as
//! `SubscriptionsConfig`. The [`crate::orchestration::FoldersAuthor`] reads the
//! `current` generation to seal new uploads + the group content-key envelope, and
//! rotates it on a member removal; every rotated-out generation is retained
//! (uncapped) so a new joiner can be granted the full back-catalogue
//! (history-on-join — FS-NUANCE option (a)).
//!
//! These functions are **pure** transitions over an in-memory `FoldersConfig` (the
//! caller supplies the fresh random key + the current time), so they are
//! deterministic and unit-testable with no RNG. The thin RNG generator + the
//! network orchestration that drives them live in [`crate::orchestration`],
//! exactly mirroring how `fauna-client-subscriptions` splits its pure `custody`
//! transitions from the `orchestration` that drives them (priority #3 — same
//! concepts everywhere). A content-keyed set is looked up by its 32-byte derived
//! `ChannelId` (`ChannelId::from_group_id(mls_group_id)`), the stable cross-device
//! identity matching the nest content-key envelope's storage key.
//!
//! **Every set has an entry from creation** (the set-nonce ruling): the create
//! helper mints the set's nonce into a keyless entry before the nest ever sees
//! the set, the serve-enable or bind fills `channel_id` + `keys` in place
//! ([`key_named_set`]), a delete retires the entry rather than removing it
//! ([`retire_set`]), and an owner-only set's nonce is found by name among the
//! live entries under a deterministic pick ([`live_set_by_name`]).
//!
//! Authority for the at-rest custody shape:
//! `docs/goal/architecture/mls-group-key-material.md` § M2 content-key mechanism
//! → *Custody (per holder)*, and § M2 → *Writer-signed change records* →
//! *Custody shape of the set nonce*.

use fauna_core::data::{FolderKeyCustody, FolderPendingRemoval, FoldersConfig};
use fauna_core::folder_keys::{ContentKeyGeneration, FolderContentKeys};
use fauna_core::identity::ActorId;

/// A custody mutation that could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustodyError {
    /// `commit_generation` was asked to commit into a set the owner holds no
    /// content keys for (it was never bound, or was already forgotten).
    SetNotFound,
}

impl std::fmt::Display for CustodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CustodyError::SetNotFound => {
                write!(
                    f,
                    "no content keys held for folder (not bound, or forgotten)"
                )
            }
        }
    }
}

impl std::error::Error for CustodyError {}

/// Index of the custody entry for `channel_id`, if any — the one a by-channel
/// write lands on and whose nonce a by-channel read answers: the pick
/// ([`pick_key`]) among the LIVE entries holding the channel, else among all
/// of them (a retired entry keeps its keys for no-data-loss, and a served set
/// deleted and re-created under its name shares the serve pseudo-channel with
/// the tombstone, which must not shadow the live set). Several entries can
/// hold one channel, because an identity move leaves its old entry behind on
/// a store that never removes one (ruling (l)(i)); [`content_keys`] joins
/// them all.
fn set_index(cfg: &FoldersConfig, channel_id: &[u8; 32]) -> Option<usize> {
    let holding = |live: bool| {
        cfg.sets
            .iter()
            .enumerate()
            .filter(move |(_, s)| {
                s.channel_id.as_ref() == Some(channel_id) && (!live || s.is_live())
            })
            .min_by_key(|(_, s)| pick_key(s))
            .map(|(i, _)| i)
    };
    holding(true).or_else(|| holding(false))
}

/// The join of the content keys held for `channel_id` — over the live entries
/// holding the channel, else over all of them ([`set_index`]'s preference), so
/// no generation hides behind a twin an identity move left behind, and a
/// deleted set's tombstone never lends its keys to a live set re-created under
/// its serve pseudo-channel.
fn joined_keys(cfg: &FoldersConfig, channel_id: &[u8; 32]) -> Option<FolderContentKeys> {
    let live = set_index(cfg, channel_id).is_some_and(|i| cfg.sets[i].is_live());
    cfg.sets
        .iter()
        .filter(|s| s.channel_id.as_ref() == Some(channel_id) && (!live || s.is_live()))
        .filter_map(|s| s.keys.as_ref())
        .fold(None, |acc: Option<FolderContentKeys>, k| {
            Some(acc.map_or_else(|| k.clone(), |a| a.merge(k)))
        })
}

/// The same-name pick (ruling (c)): among several live entries for one name —
/// a two-device create race, or a create retried after a crash — the earliest
/// `created_at`, ties by the smaller nonce. Deterministic on every device. A
/// nonce-bearing entry always outranks a nonce-less one (the entry `key_named_set` writes when no create entry exists; the
/// reconcile repairs it), so a nonce-less key holder never hides the set's nonce.
fn pick_key(s: &FolderKeyCustody) -> (bool, u64, Option<[u8; 32]>) {
    (s.set_nonce.is_none(), s.created_at, s.set_nonce)
}

/// The nest's unrendered folder rows ([`crate::FoldersClient::list_wire`]) with
/// each sealed row's name recovered from custody **by hash**: since schema 114
/// a sealed set's row rests no plaintext name (`path-sealing.md` § the set-name
/// plane), and custody holds the name of every set this holder created
/// ([`FolderKeyCustody::name`]). For a consumer that needs a set's name and
/// holds custody but no label custody — the rendered `list` on such a client
/// omits every sealed set. A sealed row custody cannot name is dropped, the
/// same as the rendered list omits it; a row that still carries its plaintext
/// (unsealed, reserved `__`, `public`) passes through untouched.
pub fn named_from_custody(
    rows: Vec<fauna_protocol::folders::FolderSummary>,
    cfg: &FoldersConfig,
) -> Vec<fauna_protocol::folders::FolderSummary> {
    rows.into_iter()
        .filter_map(|mut row| {
            if row.name.is_empty() {
                row.name = cfg
                    .sets
                    .iter()
                    .filter_map(|e| e.name.as_deref())
                    .find(|name| row.is_named(name))?
                    .to_string();
            }
            Some(row)
        })
        .collect()
}

/// The set name of every row among `rows` (owner-scoped
/// [`crate::FoldersClient::list_wire`] rows) that the owner owns, each named
/// from custody as [`named_from_custody`] names it. A member row is never the
/// reader's own set. These are the names a folder grant id is matched over
/// (`fauna_client_capabilities::OwnedSetNames`).
pub fn owned_set_names_from(
    rows: Vec<fauna_protocol::folders::FolderSummary>,
    cfg: &FoldersConfig,
) -> Vec<String> {
    named_from_custody(rows, cfg)
        .into_iter()
        .filter(|r| r.role.as_deref() != Some("member"))
        .map(|r| r.name)
        .collect()
}

/// The owner's LIVE custody entry for the set named `name` under the pick rule
/// (ruling (c)) — how an owner-only set's nonce is found. The nest's echo of the
/// nonce is never consulted: it selects nothing.
pub fn live_set_by_name<'a>(cfg: &'a FoldersConfig, name: &str) -> Option<&'a FolderKeyCustody> {
    cfg.sets
        .iter()
        .filter(|s| s.is_live() && s.name.as_deref() == Some(name))
        .min_by_key(|s| pick_key(s))
}

/// The serve window custody holds for `channel_id` — `(served_at,
/// unserved_at)`, each the later over the LIVE entries holding the channel
/// (`writer-signed-change-records.md` ruling (7)(b)(ii) rule (1)); an identity
/// move can leave two, and [`content_keys`] joins them the same way. No live
/// holder answers `(None, None)`: a tombstone carries no window.
pub fn serve_stamps(cfg: &FoldersConfig, channel_id: &[u8; 32]) -> (Option<u64>, Option<u64>) {
    cfg.sets
        .iter()
        .filter(|s| s.channel_id.as_ref() == Some(channel_id) && s.is_live())
        .fold((None, None), |(on, off), s| {
            (on.max(s.served_at), off.max(s.unserved_at))
        })
}

/// Whether custody calls the set at `channel_id` WebDAV-served — THE served
/// read of every client judgement (ruling (7)(b)(ii) rule (2)): the owner's
/// entry at the serve pseudo-channel or the bound channel, a member's
/// received copy at the real channel. The nest's `webdav_enabled` is never
/// consulted.
pub fn channel_served(cfg: &FoldersConfig, channel_id: &[u8; 32]) -> bool {
    let (on, off) = serve_stamps(cfg, channel_id);
    fauna_core::folder_keys::serve_window_open(on, off)
}

/// The channel's live entry with the channel's joined window folded onto it —
/// where a serve flip is written.
fn serve_entry<'a>(
    cfg: &'a mut FoldersConfig,
    channel_id: &[u8; 32],
) -> Option<&'a mut FolderKeyCustody> {
    let (on, off) = serve_stamps(cfg, channel_id);
    let i = set_index(cfg, channel_id).filter(|i| cfg.sets[*i].is_live())?;
    let entry = &mut cfg.sets[i];
    entry.served_at = on;
    entry.unserved_at = off;
    Some(entry)
}

/// Stamp the owner's **serve-on** on the live entry holding `channel_id`
/// ([`FolderKeyCustody::serve_on`] — strictly above its serve-off). The
/// serve-on gesture's write and nothing else's: never from the nest's flag.
/// `false` when custody holds no live entry for the channel.
pub fn serve_on(cfg: &mut FoldersConfig, channel_id: &[u8; 32], now_micros: u64) -> bool {
    match serve_entry(cfg, channel_id) {
        Some(entry) => {
            entry.serve_on(now_micros);
            true
        }
        None => false,
    }
}

/// Stamp the owner's **serve-off** on the live entry holding `channel_id`
/// ([`FolderKeyCustody::serve_off`] — strictly above its serve-on): the
/// serve-off gesture's write, and the launch pass's unserve arm's.
pub fn serve_off(cfg: &mut FoldersConfig, channel_id: &[u8; 32], now_micros: u64) -> bool {
    match serve_entry(cfg, channel_id) {
        Some(entry) => {
            entry.serve_off(now_micros);
            true
        }
        None => false,
    }
}

/// The live set nonce for `name`, if the owner holds a nonce-bearing entry.
pub fn live_set_nonce(cfg: &FoldersConfig, name: &str) -> Option<[u8; 32]> {
    live_set_by_name(cfg, name).and_then(|s| s.set_nonce)
}

/// Record a fresh live entry for a set the owner is about to create — the
/// create helper's custody-first write (ruling (d), step 2). Idempotent on the
/// nonce (a retried write of the same mint adds nothing). Returns whether the
/// config changed.
///
/// `minted_by` is the identity whose device minted the nonce
/// (`writer-signed-change-records.md` ruling (11)(a)) — `None` only where the
/// writer cannot say, and the owner's reconcile then re-mints the entry.
pub fn record_created_set(
    cfg: &mut FoldersConfig,
    name: &str,
    set_nonce: [u8; 32],
    minted_by: Option<ActorId>,
    now_micros: u64,
) -> bool {
    if cfg.sets.iter().any(|s| s.set_nonce == Some(set_nonce)) {
        return false;
    }
    cfg.sets.push(FolderKeyCustody {
        set_nonce: Some(set_nonce),
        name: Some(name.to_string()),
        created_at: now_micros,
        minted_by,
        ..Default::default()
    });
    true
}

/// The nonces of the live **owned** entries `identity` did not mint — the
/// succession cut's candidates (`writer-signed-change-records.md` ruling
/// (11)(a)): an entry carrying a `name` (a member's received copy carries
/// none) and a nonce, whose `minted_by` is another identity or none recorded.
pub fn uncut_owned_nonces(cfg: &FoldersConfig, identity: &ActorId) -> Vec<[u8; 32]> {
    let mut nonces: Vec<[u8; 32]> = cfg
        .sets
        .iter()
        .filter(|s| s.is_live() && s.name.is_some() && s.minted_by.as_ref() != Some(identity))
        .filter_map(|s| s.set_nonce)
        .collect();
    nonces.sort_unstable();
    nonces.dedup();
    nonces
}

/// The re-mint itself, over the candidates `marked` whose adoption marker
/// landed ([`uncut_owned_nonces`], re-checked here against the custody the
/// write starts from): each live entry carrying one is retired — a re-mint's
/// retirement, which nothing lifts — and a live entry added under `fresh()`
/// with its channel, keys, name and serve window, `created_at = now`,
/// `minted_by = identity` and `replaces` the old nonce. Returns the names
/// re-minted.
pub fn remint_owned_entries(
    cfg: &mut FoldersConfig,
    identity: &ActorId,
    marked: &[[u8; 32]],
    now_micros: u64,
    mut fresh: impl FnMut() -> [u8; 32],
) -> Vec<String> {
    let still = uncut_owned_nonces(cfg, identity);
    let mut added = Vec::new();
    let mut names = Vec::new();
    for old in marked.iter().filter(|n| still.contains(n)) {
        let mut minted = None;
        for s in cfg.sets.iter_mut() {
            if s.set_nonce.as_ref() == Some(old) && s.is_live() && s.name.is_some() {
                s.retire(now_micros);
                minted.get_or_insert_with(|| FolderKeyCustody {
                    channel_id: s.channel_id,
                    keys: s.keys.clone(),
                    set_nonce: Some(fresh()),
                    name: s.name.clone(),
                    created_at: now_micros,
                    minted_by: Some(*identity),
                    replaces: Some(*old),
                    // The serve window is the set's, not the nonce's: a
                    // re-mint must not un-serve it (ruling (7)(b)(ii)).
                    served_at: s.served_at,
                    unserved_at: s.unserved_at,
                    ..Default::default()
                });
            }
        }
        if let Some(entry) = minted {
            names.extend(entry.name.clone());
            added.push(entry);
        }
    }
    cfg.sets.extend(added);
    names.sort();
    names.dedup();
    names
}

/// Retire the entry carrying `set_nonce` — a failed create's rollback (ruling
/// (d), step 4). Returns whether the config changed.
pub fn retire_set_nonce(cfg: &mut FoldersConfig, set_nonce: &[u8; 32], now_micros: u64) -> bool {
    let mut changed = false;
    for s in cfg.sets.iter_mut() {
        if s.set_nonce.as_ref() == Some(set_nonce) && s.is_live() {
            s.retire(now_micros);
            changed = true;
        }
    }
    changed
}

/// Lift the retirements a refused delete made (`retired_at == retired_at_micros`
/// on an entry whose nonce is in `set_nonces`) — the one sanctioned un-retire:
/// the nest *answered* the delete with a refusal, so the set still exists and
/// its nonce must stay live. The lift is a stamp, `lifted_at =
/// retired_at_micros` (ruling (l)(v)), never a cleared tombstone: a merge-only
/// store keeps it against a stale copy of the retirement, and a later delete's
/// later stamp retires the entry anew. Returns whether the config changed.
pub fn unretire_set_nonces(
    cfg: &mut FoldersConfig,
    set_nonces: &[[u8; 32]],
    retired_at_micros: u64,
) -> bool {
    let mut changed = false;
    for s in cfg.sets.iter_mut() {
        if s.retired_at == Some(retired_at_micros)
            && !s.is_live()
            && s.set_nonce.is_some_and(|n| set_nonces.contains(&n))
        {
            s.lifted_at = Some(retired_at_micros);
            changed = true;
        }
    }
    changed
}

/// Retire every live entry for the set named `name` — the delete helper's
/// custody-first write (ruling (e)). A retired entry keeps its nonce and its
/// keys and stands as a delete intent; entries are never removed (the union
/// merge would resurrect a removed one from any stale device). Returns whether
/// the config changed.
pub fn retire_set(cfg: &mut FoldersConfig, name: &str, now_micros: u64) -> bool {
    let mut changed = false;
    for s in cfg.sets.iter_mut() {
        if s.name.as_deref() == Some(name) && s.is_live() {
            s.retire(now_micros);
            changed = true;
        }
    }
    changed
}

/// Record the genesis (version-1) content key for a newly-bound shared set and
/// return the recorded generation.
///
/// **Idempotent** (mirrors the nest's idempotent `share` re-bind): if the owner
/// already holds keys for `channel_id` this is a no-op and the existing `current`
/// is returned unchanged — re-recording would orphan the content already sealed
/// under the live key. Call this from the bind orchestration with a freshly
/// generated key.
///
/// Name-less: a new entry carries no nonce. The owner's serve-enable and bind
/// go through [`key_named_set`], which fills the set's create-time entry in
/// place instead.
pub fn record_new_set(
    cfg: &mut FoldersConfig,
    channel_id: [u8; 32],
    key: [u8; 32],
    now_micros: u64,
) -> ContentKeyGeneration {
    if let Some(keys) = joined_keys(cfg, &channel_id) {
        return keys.current;
    }
    if let Some(i) = set_index(cfg, &channel_id) {
        let keys = FolderContentKeys::genesis(key, now_micros);
        let current = keys.current.clone();
        cfg.sets[i].keys = Some(keys);
        return current;
    }
    let keys = FolderContentKeys::genesis(key, now_micros);
    let current = keys.current.clone();
    cfg.sets.push(FolderKeyCustody {
        channel_id: Some(channel_id),
        keys: Some(keys),
        ..Default::default()
    });
    current
}

/// Record the genesis content key for the owner's set `name` under
/// `channel_id` (its bind's real `ChannelId`, or its serve pseudo-channel) and
/// return the recorded generation — [`record_new_set`] for a set that has a
/// create-time entry: the set's live, not-yet-keyed entry (the pick,
/// [`live_set_by_name`]'s rule) is keyed **in place** (`channel_id` + genesis
/// `keys`), so the nonce minted at create stays the set's one identity.
/// Idempotent like [`record_new_set`]: a live entry already keyed under
/// `channel_id` returns its `current` unchanged. A set with no such entry (custody
/// lost, e.g. to a restored config) gets a name-bearing, nonce-less entry the owner's
/// custody reconcile repairs.
pub fn key_named_set(
    cfg: &mut FoldersConfig,
    name: &str,
    channel_id: [u8; 32],
    key: [u8; 32],
    now_micros: u64,
) -> ContentKeyGeneration {
    if let Some(i) = set_index(cfg, &channel_id)
        && cfg.sets[i].is_live()
    {
        return record_new_set(cfg, channel_id, key, now_micros);
    }
    let pick = cfg
        .sets
        .iter()
        .enumerate()
        .filter(|(_, s)| s.is_live() && s.name.as_deref() == Some(name) && s.channel_id.is_none())
        .min_by_key(|(_, s)| pick_key(s))
        .map(|(i, _)| i);
    match pick {
        Some(i) => {
            let entry = &mut cfg.sets[i];
            entry.channel_id = Some(channel_id);
            entry
                .keys
                .get_or_insert_with(|| FolderContentKeys::genesis(key, now_micros))
                .current
                .clone()
        }
        None => {
            let keys = FolderContentKeys::genesis(key, now_micros);
            let current = keys.current.clone();
            cfg.sets.push(FolderKeyCustody {
                channel_id: Some(channel_id),
                keys: Some(keys),
                name: Some(name.to_string()),
                created_at: now_micros,
                ..Default::default()
            });
            current
        }
    }
}

/// **Member-side custody ingest** (Phase 0 — the read leg): fold a generation
/// bundle received from the group content-key envelope into this holder's own
/// custody for `channel_id`, returning whether custody **changed**.
///
/// This is the pure half of the member ingest step: the conversations session
/// fetches the sealed envelope (`fauna.folders.content_key.get`), opens it via
/// the MLS group epoch into a [`FolderContentKeys`], and calls this to merge the
/// generations into the holder's custody. A member and a second *owner* device
/// converge by the identical [`FolderContentKeys::merge`] CRDT, so ingest is:
///
/// - **creating** on a member's first ingest (no entry yet) — the row is pushed
///   with the received bundle verbatim (mirrors [`record_new_set`]'s row shape,
///   minus the genesis assumption: the member may receive a rotated bundle
///   whose `current` is version > 1);
/// - **idempotent** on re-ingest — re-folding the same (or an older) bundle
///   leaves custody byte-identical and returns `false`, so the D2 poll-cadence
///   retry never churns the plane write;
/// - **advancing** when a later rotated bundle arrives — `current` moves forward
///   and every prior generation (including a concurrent-rotation same-version
///   shadow) is retained, never dropped (no-user-data-loss).
///
/// The `false` return lets the caller skip a redundant persist — the same
/// changed-reporting contract [`mark_removal_gated_attempted`] carries.
pub fn merge_received_keys(
    cfg: &mut FoldersConfig,
    channel_id: [u8; 32],
    received: FolderContentKeys,
) -> bool {
    match set_index(cfg, &channel_id) {
        Some(i) => {
            let held = &mut cfg.sets[i].keys;
            let merged = match held {
                Some(k) => k.merge(&received),
                None => received,
            };
            if held.as_ref() == Some(&merged) {
                return false;
            }
            *held = Some(merged);
            true
        }
        None => {
            cfg.sets.push(FolderKeyCustody {
                channel_id: Some(channel_id),
                keys: Some(received),
                ..Default::default()
            });
            true
        }
    }
}

/// Write the set nonce a member received inside the owner's content-key
/// envelope into its entry for `channel_id` — **replacing** whatever it held
/// (ruling (h): the owner is the authority, and a re-mint reaches members by the
/// re-publish every membership change already orders). Call after
/// [`merge_received_keys`], which creates the entry on a first ingest. Returns
/// whether custody changed (the CAS-skip contract).
///
/// Nothing is overwritten (ruling (l)(ii)): an entry's first nonce is written
/// onto it, but a *replaced* nonce retires every live entry holding the old
/// one and adds an entry under the new, carrying the channel's keys — a nonce
/// is an entry's identity, and a store that never removes an entry would keep
/// the old one live beside the new. The retired nonce stays migration input,
/// as any retired nonce does under ruling (g).
pub fn record_received_set_nonce(
    cfg: &mut FoldersConfig,
    channel_id: &[u8; 32],
    set_nonce: [u8; 32],
    now_micros: u64,
) -> bool {
    let Some(i) = set_index(cfg, channel_id) else {
        return false;
    };
    let holds = |s: &FolderKeyCustody| s.channel_id.as_ref() == Some(channel_id);
    let already = cfg
        .sets
        .iter()
        .any(|s| holds(s) && s.is_live() && s.set_nonce == Some(set_nonce));
    let keys = joined_keys(cfg, channel_id);
    let mut changed = false;
    for s in cfg.sets.iter_mut() {
        if holds(s) && s.is_live() && s.set_nonce.is_some_and(|n| n != set_nonce) {
            s.retire(now_micros);
            changed = true;
        }
    }
    if already {
        return changed;
    }
    match cfg
        .sets
        .iter()
        .position(|s| holds(s) && s.is_live() && s.set_nonce.is_none())
    {
        Some(j) => {
            let entry = &mut cfg.sets[j];
            entry.set_nonce = Some(set_nonce);
            entry.keys = keys;
        }
        None if cfg.sets[i].set_nonce.is_none() => cfg.sets[i].set_nonce = Some(set_nonce),
        None => cfg.sets.push(FolderKeyCustody {
            channel_id: Some(*channel_id),
            keys,
            set_nonce: Some(set_nonce),
            created_at: now_micros,
            ..Default::default()
        }),
    }
    true
}

/// Why a received content-key envelope was refused whole — keys and nonce
/// discarded together (`writer-signed-change-records.md` ruling (11)(b)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeRefusal {
    /// Its live nonce is one this holder already holds as RETIRED for the
    /// channel — a stale envelope that would un-cut the set.
    Backwards,
    /// Its lineage (siblings included) does not cover a nonce this holder
    /// holds for the channel — a race loser's, or one from before a cut.
    LineageNotCovered,
    /// Signed by someone other than the owner the holder's MLS state names: it
    /// may confirm what the holder holds and move nothing.
    ConfirmOnly,
}

impl std::fmt::Display for EnvelopeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Backwards => "the envelope names a retired nonce as live",
            Self::LineageNotCovered => "the envelope's lineage does not cover the held nonces",
            Self::ConfirmOnly => "a non-owner's envelope may only confirm what is held",
        })
    }
}

/// **The member's whole envelope ingest** (ruling (11)(b)): fold a received
/// [`ContentKeyEnvelopePayload`](fauna_core::folder_keys::ContentKeyEnvelopePayload)
/// into the holder's custody for `channel_id` — the keys
/// ([`merge_received_keys`]), the live nonce with its minter
/// ([`record_received_set_nonce`]'s retire-and-add), and every lineage nonce
/// the holder does not hold yet, as a retired entry carrying its minter, and
/// the owner's serve stamps, each joined to the later onto the channel's live
/// entry (ruling (7)(b)(ii) rule (4): a replayed envelope cannot move the
/// window, and a member's copy only advances to what the owner last said) —
/// or refuse it whole. Returns whether custody changed.
///
/// **Forward only:** refused when its live nonce is one the holder holds as
/// retired for the channel, or when its lineage (live nonce and siblings) does
/// not cover every nonce the holder holds for it — so a stale envelope cannot
/// un-cut a holder, and a race loser's does not lock it out of the winner's.
/// `may_move` is the caller's verdict on the signer: the current owner, as
/// the holder's MLS state names it, may move the nonce and extend the lineage;
/// anyone else's envelope is accepted only when it changes nothing.
pub fn ingest_received_envelope(
    cfg: &mut FoldersConfig,
    channel_id: [u8; 32],
    payload: fauna_core::folder_keys::ContentKeyEnvelopePayload,
    may_move: bool,
    now_micros: u64,
) -> Result<bool, EnvelopeRefusal> {
    let holds = |s: &FolderKeyCustody| s.channel_id.as_ref() == Some(&channel_id);
    if let Some(live) = payload.set_nonce {
        let held: Vec<&FolderKeyCustody> = cfg.sets.iter().filter(|s| holds(s)).collect();
        let held_live = held
            .iter()
            .any(|s| s.set_nonce == Some(live) && s.is_live());
        let held_retired = held
            .iter()
            .any(|s| s.set_nonce == Some(live) && !s.is_live());
        if held_retired && !held_live {
            return Err(EnvelopeRefusal::Backwards);
        }
        let covered =
            |n: &[u8; 32]| *n == live || payload.retired_set_nonces.iter().any(|r| r.nonce == *n);
        if held
            .iter()
            .filter_map(|s| s.set_nonce.as_ref())
            .any(|n| !covered(n))
        {
            return Err(EnvelopeRefusal::LineageNotCovered);
        }
    }
    let mut next = cfg.clone();
    let keys_changed = merge_received_keys(&mut next, channel_id, payload.keys);
    let nonce_changed = payload
        .set_nonce
        .is_some_and(|n| record_received_set_nonce(&mut next, &channel_id, n, now_micros));
    let mut lineage_changed = false;
    for entry in next.sets.iter_mut().filter(|s| holds(s)) {
        let minter = if entry.set_nonce.is_some() && entry.set_nonce == payload.set_nonce {
            payload.minted_by
        } else {
            payload
                .retired_set_nonces
                .iter()
                .find(|r| Some(r.nonce) == entry.set_nonce)
                .and_then(|r| r.minted_by)
        };
        if entry.minted_by.is_none() && minter.is_some() {
            entry.minted_by = minter;
            lineage_changed = true;
        }
    }
    for retired in &payload.retired_set_nonces {
        if next
            .sets
            .iter()
            .any(|s| holds(s) && s.set_nonce == Some(retired.nonce))
        {
            continue;
        }
        let mut entry = FolderKeyCustody {
            channel_id: Some(channel_id),
            set_nonce: Some(retired.nonce),
            minted_by: retired.minted_by,
            created_at: now_micros,
            ..Default::default()
        };
        entry.retire(now_micros);
        next.sets.push(entry);
        lineage_changed = true;
    }
    // The window as held before this ingest (a replaced nonce's new entry
    // starts without it), joined with the owner's.
    let (held_on, held_off) = serve_stamps(cfg, &channel_id);
    let window = (
        held_on.max(payload.served_at),
        held_off.max(payload.unserved_at),
    );
    let mut window_changed = false;
    if let Some(i) = set_index(&next, &channel_id).filter(|i| next.sets[*i].is_live()) {
        let entry = &mut next.sets[i];
        if (entry.served_at, entry.unserved_at) != window {
            (entry.served_at, entry.unserved_at) = window;
            window_changed = (held_on, held_off) != window;
        }
    }
    let changed = keys_changed || nonce_changed || lineage_changed || window_changed;
    if changed && !may_move {
        return Err(EnvelopeRefusal::ConfirmOnly);
    }
    *cfg = next;
    Ok(changed)
}

/// The envelope payload the owner seals for the set at `channel_id` (named
/// `owner_name` in its custody): `keys`, the live nonce with its minter, the
/// set's lineage with its siblings (`writer-signed-change-records.md`
/// ruling (11)(b)) and the serve window (ruling (7)(b)(ii) rule (4)).
pub fn envelope_payload(
    cfg: &FoldersConfig,
    owner_name: Option<&str>,
    channel_id: &[u8; 32],
    keys: FolderContentKeys,
) -> fauna_core::folder_keys::ContentKeyEnvelopePayload {
    let lineage = set_lineage(cfg, owner_name, Some(channel_id));
    let (served_at, unserved_at) = serve_stamps(cfg, channel_id);
    fauna_core::folder_keys::ContentKeyEnvelopePayload {
        keys,
        set_nonce: lineage.live,
        minted_by: lineage.live_minted_by,
        retired_set_nonces: lineage.retired,
        served_at,
        unserved_at,
    }
}

/// The JOIN of custody's payload (`expected`) and the stored one
/// (`published`), with whether custody is **strictly ahead** of what is
/// stored — the envelope reconcile's one decision
/// (`writer-signed-change-records.md` ruling (7)(b)(ii) rule (3), over ruling
/// (11)(a)/(b)'s payload): the join is the only thing ever published over a
/// readable envelope, and only when custody is ahead (or the envelope must be
/// re-sealed anyway), never a stale bundle over a newer one. Ahead means, in
/// any part: a generation the envelope lacks; a later serve stamp; a live
/// nonce the envelope does not name (the envelope's own is absent, in
/// custody's lineage, or unrelated — the owner's custody is the authority);
/// a minter or a lineage nonce the envelope lacks. The join keeps everything
/// the envelope holds beyond custody — its extra generations, its later
/// stamps, its extra lineage — under custody's live nonce.
///
/// `None` when the stored envelope names custody's live nonce as RETIRED:
/// this device lags a cut, and anything it published would un-cut the set.
pub fn envelope_join(
    published: &fauna_core::folder_keys::ContentKeyEnvelopePayload,
    expected: &fauna_core::folder_keys::ContentKeyEnvelopePayload,
) -> Option<(fauna_core::folder_keys::ContentKeyEnvelopePayload, bool)> {
    use fauna_core::folder_keys::ContentKeyEnvelopePayload;
    if let Some(live) = expected.set_nonce
        && published.set_nonce != Some(live)
        && published.retired_set_nonces.iter().any(|r| r.nonce == live)
    {
        return None;
    }
    let mut retired = expected.retired_set_nonces.clone();
    for r in &published.retired_set_nonces {
        if Some(r.nonce) != expected.set_nonce && !retired.iter().any(|x| x.nonce == r.nonce) {
            retired.push(*r);
        }
    }
    let moved = expected.set_nonce.is_some() && published.set_nonce != expected.set_nonce;
    let joined = ContentKeyEnvelopePayload {
        keys: published.keys.merge(&expected.keys),
        set_nonce: expected.set_nonce.or(published.set_nonce),
        minted_by: if moved {
            expected.minted_by
        } else {
            expected.minted_by.or(published.minted_by)
        },
        retired_set_nonces: retired,
        served_at: published.served_at.max(expected.served_at),
        unserved_at: published.unserved_at.max(expected.unserved_at),
    };
    let ahead = !envelope_matches(published, &joined);
    Some((joined, ahead))
}

/// Whether a published payload carries what custody would publish now — the
/// same generations, the same live nonce and minter, the same lineage, the
/// same serve stamps.
pub fn envelope_matches(
    published: &fauna_core::folder_keys::ContentKeyEnvelopePayload,
    expected: &fauna_core::folder_keys::ContentKeyEnvelopePayload,
) -> bool {
    let generations = |k: &FolderContentKeys| {
        let mut g: Vec<(u64, u64)> = k.generations().map(|g| (g.version, g.rotated_at)).collect();
        g.sort_unstable();
        g
    };
    let lineage = |p: &fauna_core::folder_keys::ContentKeyEnvelopePayload| {
        let mut l: Vec<([u8; 32], Option<[u8; 32]>)> = p
            .retired_set_nonces
            .iter()
            .map(|r| (r.nonce, r.minted_by.map(|a| a.0)))
            .collect();
        l.sort_unstable();
        l
    };
    published.set_nonce == expected.set_nonce
        && published.minted_by == expected.minted_by
        && lineage(published) == lineage(expected)
        && generations(&published.keys) == generations(&expected.keys)
        && published.served_at == expected.served_at
        && published.unserved_at == expected.unserved_at
}

/// The owner's live, named, content-keyed sets bound to a real MLS channel —
/// `(name, channel)`, the serve pseudo-channel excluded (a served-but-unshared
/// set has no group and no envelope).
pub fn owned_bound_sets(cfg: &FoldersConfig) -> Vec<(String, [u8; 32])> {
    let mut sets: Vec<(String, [u8; 32])> = cfg
        .sets
        .iter()
        .filter(|s| s.is_live())
        .filter_map(|s| {
            let name = s.name.clone()?;
            let channel = s.channel_id?;
            (channel != fauna_core::folder_keys::serve_custody_channel_id(&name))
                .then_some((name, channel))
        })
        .collect();
    sets.sort();
    sets.dedup();
    sets
}

/// An owned bound set whose content key a succession still owes a rotation
/// ([`sets_owing_succession_rotation`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuccessionRotationOwed {
    pub name: String,
    pub channel_id: [u8; 32],
    /// The identities that minted the lineage's retired nonces — the retired
    /// owners whose leaves must be off the group before the rotation runs.
    pub predecessors: Vec<ActorId>,
    /// When the succession cut minted the set's live nonce (micros) — the
    /// floor of the rotated generation's `rotated_at`.
    pub cut_at: u64,
}

/// The owned bound sets whose **current content-key generation predates the
/// succession cut** (`succession-aftermath.md` § Re-key scope, the MLS groups
/// row): the live nonce is one `identity` minted, the lineage behind it names
/// another identity as a retired nonce's minter — a predecessor — and no
/// generation has been minted since the cut's entry was created. The
/// idempotence lives here: a set rotated after its cut is no longer owed, and
/// a second succession's cut owes it again. A set whose cut has not run yet
/// (the live nonce still a predecessor's) is not listed — the cut comes
/// first; nor is one no predecessor minted, or one a member merely holds a
/// copy of (it carries no `name`).
pub fn sets_owing_succession_rotation(
    cfg: &FoldersConfig,
    identity: &ActorId,
) -> Vec<SuccessionRotationOwed> {
    owned_bound_sets(cfg)
        .into_iter()
        .filter_map(|(name, channel_id)| {
            let lineage = set_lineage(cfg, Some(&name), Some(&channel_id));
            let live = lineage.live?;
            if lineage.live_minted_by.as_ref() != Some(identity) {
                return None;
            }
            let mut predecessors: Vec<ActorId> = lineage
                .retired
                .iter()
                .filter_map(|r| r.minted_by)
                .filter(|minter| minter != identity)
                .collect();
            predecessors.sort_unstable_by_key(|a| a.0);
            predecessors.dedup();
            if predecessors.is_empty() {
                return None;
            }
            let cut_at = cfg
                .sets
                .iter()
                .filter(|s| s.is_live() && s.set_nonce == Some(live))
                .map(|s| s.created_at)
                .max()?;
            let keys = content_keys(cfg, &channel_id)?;
            (keys.current.rotated_at < cut_at).then_some(SuccessionRotationOwed {
                name,
                channel_id,
                predecessors,
                cut_at,
            })
        })
        .collect()
}

/// The set nonce custody holds for the content-keyed set at `channel_id` — the
/// owner's live nonce for the set it seals into the envelope, or a member's
/// received copy.
pub fn set_nonce_for_channel(cfg: &FoldersConfig, channel_id: &[u8; 32]) -> Option<[u8; 32]> {
    set_index(cfg, channel_id).and_then(|i| cfg.sets[i].set_nonce)
}

/// The set's binding pair the engine carries (`FolderEngineKeys::set_nonce` +
/// `retired_set_nonces`, ruling (g)): the live nonce and the set's lineage of
/// retired ones ([`set_lineage`]), which the engine's re-record leg reads to
/// re-sign this device's rows onto the live one.
pub fn set_nonces_for(
    cfg: &FoldersConfig,
    owner_name: Option<&str>,
    channel_id: Option<&[u8; 32]>,
) -> (Option<[u8; 32]>, Vec<[u8; 32]>) {
    let lineage = set_lineage(cfg, owner_name, channel_id);
    (
        lineage.live,
        lineage.retired.iter().map(|r| r.nonce).collect(),
    )
}

/// The set's **nonce lineage** (`writer-signed-change-records.md` ruling
/// (11)(b)): the live nonce — by `channel_id` for a content-keyed set (the
/// owner's entry and a member's received copy alike), else the owner's live
/// pick by `name` — with its minter, and the retired nonces of THIS
/// incarnation with theirs, newest first.
///
/// **On the owner's custody** (`owner_name` given) the lineage is computed by
/// nonce, never by a timestamp: the connected component around the live nonce
/// of the `replaces` edges (a re-mint's) and the `retired_by_pick` edges (a
/// same-name pick's — a create race's loser, a re-mint race's), walked across
/// entries whatever channel they hold now (`migrate_set_identity` keeps
/// nonces). So a second succession keeps the first cut's nonce, and a deleted
/// earlier incarnation of the name — which no edge reaches — stays out: its
/// rows are not this set's, and re-signing one a nest replayed here would
/// launder exactly the replay the nonce refuses.
///
/// **A member's** copy (`owner_name: None`) holds what the owner's envelopes
/// delivered: every other nonce its entries hold for the channel — a channel
/// is one MLS group, so one incarnation.
///
/// The lineage also answers the set's **serve window** ([`serve_stamps`] at
/// `channel_id`) — the binding source's other half, read from the same
/// custody in the same call.
pub fn set_lineage(
    cfg: &FoldersConfig,
    owner_name: Option<&str>,
    channel_id: Option<&[u8; 32]>,
) -> fauna_core::folder_keys::SetNonceLineage {
    use fauna_core::folder_keys::{RetiredSetNonce, SetNonceLineage};
    let live = channel_id
        .and_then(|c| set_nonce_for_channel(cfg, c))
        .or_else(|| owner_name.and_then(|n| live_set_nonce(cfg, n)));
    // The serve window of the content-keyed set (ruling (7)(b)(ii) rule (2)):
    // `channel_id` is given only for a set custody calls content-keyed
    // ([`crate::engine_binding::custody_channel_for`]), so an owner-only set
    // answers none.
    let (served_at, unserved_at) = channel_id.map_or((None, None), |c| serve_stamps(cfg, c));
    let Some(live) = live else {
        return SetNonceLineage {
            served_at,
            unserved_at,
            ..Default::default()
        };
    };
    let minter_of = |nonce: &[u8; 32]| {
        cfg.sets
            .iter()
            .filter(|s| s.set_nonce.as_ref() == Some(nonce))
            .find_map(|s| s.minted_by)
    };
    let created_of = |nonce: &[u8; 32]| {
        cfg.sets
            .iter()
            .filter(|s| s.set_nonce.as_ref() == Some(nonce))
            .map(|s| s.created_at)
            .max()
            .unwrap_or(0)
    };
    let mut held: Vec<[u8; 32]> = match owner_name {
        Some(name) => lineage_component(cfg, live, name),
        None => channel_id
            .map(|c| {
                cfg.sets
                    .iter()
                    .filter(|s| s.channel_id.as_ref() == Some(c))
                    .filter_map(|s| s.set_nonce)
                    .collect()
            })
            .unwrap_or_default(),
    };
    held.retain(|n| *n != live);
    held.sort_unstable();
    held.dedup();
    held.sort_by_key(|n| std::cmp::Reverse(created_of(n)));
    SetNonceLineage {
        live: Some(live),
        live_minted_by: minter_of(&live),
        retired: held
            .into_iter()
            .map(|nonce| RetiredSetNonce {
                nonce,
                minted_by: minter_of(&nonce),
            })
            .collect(),
        served_at,
        unserved_at,
    }
}

/// The nonces connected to `live` by the lineage's edges — each entry's
/// `replaces` and `retired_by_pick`, both undirected — `live` included, and
/// seeded with every other LIVE nonce-bearing entry of `name`: a race the
/// pick has not settled yet, whose loser is this incarnation's by
/// construction (a delete retires every live entry of a name).
fn lineage_component(cfg: &FoldersConfig, live: [u8; 32], name: &str) -> Vec<[u8; 32]> {
    let edges: Vec<([u8; 32], [u8; 32])> = cfg
        .sets
        .iter()
        .filter_map(|s| {
            let nonce = s.set_nonce?;
            Some(
                s.replaces
                    .into_iter()
                    .chain(s.retired_by_pick)
                    .map(move |other| (nonce, other)),
            )
        })
        .flatten()
        .collect();
    let mut component = vec![live];
    for sibling in cfg
        .sets
        .iter()
        .filter(|s| s.is_live() && s.name.as_deref() == Some(name))
        .filter_map(|s| s.set_nonce)
    {
        if !component.contains(&sibling) {
            component.push(sibling);
        }
    }
    let mut i = 0;
    while i < component.len() {
        let at = component[i];
        for (a, b) in &edges {
            let next = if *a == at {
                *b
            } else if *b == at {
                *a
            } else {
                continue;
            };
            if !component.contains(&next) {
                component.push(next);
            }
        }
        i += 1;
    }
    component
}

/// The set nonce a change record into `folder` binds to, resolved by name over
/// the caller's member-visible `roster` (`fauna.folders.list` with
/// `include_shared_with_me`) and custody: a roster row by its custody channel
/// (a member's received copy or the owner's keyed entry), else — for the
/// caller's OWN row — the owner's live pick by name; no roster row → a foreign
/// record by its channel. The same name resolution the folder key resolver
/// uses for keys, so a record's nonce and its seal can never disagree about
/// which set a name means.
///
/// The roster and foreign matches go by `name_hash` ([`find_roster_row`],
/// [`find_foreign_set_by_hash`]) — a sealed set's roster row carries no
/// plaintext name. The owner's pick still goes by `folder` itself: that is
/// the caller's own custody, which rests client-sealed.
pub fn set_nonce_by_name(
    roster: &[fauna_protocol::folders::FolderSummary],
    cfg: &FoldersConfig,
    folder: &str,
) -> Option<[u8; 32]> {
    let name_hash = fauna_core::path_crypto::set_name_hash(folder);
    if let Some(row) = find_named_roster_row(roster, cfg, &name_hash) {
        let owner_name = (row.role.as_deref() != Some("member")).then_some(folder);
        let channel = crate::engine_binding::custody_channel_for(&row, cfg)
            .ok()
            .flatten();
        return set_nonces_for(cfg, owner_name, channel.as_ref()).0;
    }
    find_foreign_set_by_hash(cfg, &name_hash)
        .and_then(|f| set_nonce_for_channel(cfg, &f.channel_id))
}

/// [`set_nonce_by_name`]'s resolution answering the set's whole **lineage**
/// (`writer-signed-change-records.md` ruling (11)(b)) — what a projection
/// reader verifies history under.
///
/// By the set's **hash**, the address a projection carries for a sealed set
/// (`path-sealing.md` § the set-name plane): a Media item of a sealed set
/// names it by `folder_hash` alone. The owner's own entry is keyed by name, so
/// for the caller's OWN row the name comes from custody — the entry whose name
/// hashes to the address; a member's and a foreign set resolve by channel.
pub fn set_lineage_by_hash(
    roster: &[fauna_protocol::folders::FolderSummary],
    cfg: &FoldersConfig,
    name_hash: &[u8; 32],
) -> fauna_core::folder_keys::SetNonceLineage {
    if let Some(row) = find_named_roster_row(roster, cfg, name_hash) {
        let owner_name = (row.role.as_deref() != Some("member"))
            .then(|| {
                cfg.sets
                    .iter()
                    .filter_map(|e| e.name.as_deref())
                    .find(|name| fauna_core::path_crypto::set_name_hash(name) == *name_hash)
            })
            .flatten();
        let channel = crate::engine_binding::custody_channel_for(&row, cfg)
            .ok()
            .flatten();
        return set_lineage(cfg, owner_name, channel.as_ref());
    }
    find_foreign_set_by_hash(cfg, name_hash)
        .map(|f| set_lineage(cfg, None, Some(&f.channel_id)))
        .unwrap_or_default()
}

/// The roster row addressed by `name_hash` — the row's projected `name_hash`,
/// or [`fauna_core::path_crypto::set_name_hash`] of its plaintext when the
/// projection carries none ([`fauna_core::label_custody::set_name_label_salt`],
/// the one place that decides a row's address). By hash, because a sealed
/// set's row rests with no plaintext `name` to match (`path-sealing.md`
/// § the set-name plane).
pub fn find_roster_row<'a>(
    roster: &'a [fauna_protocol::folders::FolderSummary],
    name_hash: &[u8; 32],
) -> Option<&'a fauna_protocol::folders::FolderSummary> {
    roster.iter().find(|s| {
        fauna_core::label_custody::set_name_label_salt(
            s.name_hash.as_deref().map(|b| &b[..]),
            &s.name,
        ) == *name_hash
    })
}

/// [`find_roster_row`]'s row with the holder's OWN sealed set **named from
/// custody** — what every custody resolution derives a channel from. A
/// group-less set's keys rest at the serve pseudo-channel of its NAME
/// ([`fauna_core::folder_keys::serve_custody_channel_id`]), and since schema
/// 114 a sealed set's row rests none: deriving from the blank name answered
/// the channel of the empty string, so a served set read as plain owner-only
/// and an unserved one as never served. A row custody cannot name — a
/// member's, whose channel is its group's and needs no name — is returned as
/// it rests.
pub fn find_named_roster_row(
    roster: &[fauna_protocol::folders::FolderSummary],
    cfg: &FoldersConfig,
    name_hash: &[u8; 32],
) -> Option<fauna_protocol::folders::FolderSummary> {
    let row = find_roster_row(roster, name_hash)?.clone();
    if !row.name.is_empty() || row.role.as_deref() == Some("member") {
        return Some(row);
    }
    Some(
        named_from_custody(vec![row.clone()], cfg)
            .pop()
            .unwrap_or(row),
    )
}

/// The current (active) generation new uploads seal + stamp under, if the owner
/// holds keys for `channel_id`.
pub fn current_generation(
    cfg: &FoldersConfig,
    channel_id: &[u8; 32],
) -> Option<ContentKeyGeneration> {
    content_keys(cfg, channel_id).map(|k| k.current)
}

/// The full content-key history for `channel_id` (the bundle the owner seals into
/// the group content-key envelope, and the keys a live `SyncEngine` loads to seal
/// + open chunks across generations), if held.
pub fn content_keys(cfg: &FoldersConfig, channel_id: &[u8; 32]) -> Option<FolderContentKeys> {
    joined_keys(cfg, channel_id)
}

/// Record (upsert) a **foreign** (cross-nest) shared-set membership in this
/// member's own folder-keys custody — the accept-time write that makes a set whose home
/// is another nest listable and readable from this client (Phase 2 client
/// read-side; `ui/folders.md` § Sharing; record shape:
/// [`fauna_core::data::ForeignFolder`]). Returns whether the config
/// **changed** (the CAS-skip contract [`merge_received_keys`] carries).
///
/// Upsert semantics mirror the home nest's own foreign-member row: a
/// re-accepted share **re-binds** `home_nest_url` + `mls_group_id`
/// (last-writer-wins, S1), while `set_name` and `access` only ever *gain*
/// information (`new.or(old)` — a name-less / access-less re-delivery (an unopenable seal, a relay that
/// omits it) must not erase what is already held). Note `.or` still lets a
/// **re-accept after a demotion** write the lower grant through: the home nest
/// resolves the fresh value, so `new` is `Some("reader")`, not `None`. Only a
/// genuinely absent value falls back. Idempotent — re-recording an identical
/// share returns `false`.
/// Whether `record` carries a display name already held by a **different**
/// foreign set (another channel) in this config — the collision
/// class. Foreign names are chosen by other people on other nests, so two
/// distinct shares legitimately arrive as `photos`; the collision is not
/// refused (that would break share-accept), but the production write path
/// (`NestFolderCustodySink::record_foreign_set` in `custody_ingest`) warns
/// loudly, because [`find_foreign_set_by_hash`] is first-match and the later
/// record becomes unaddressable by name.
pub fn foreign_name_collides(
    cfg: &FoldersConfig,
    record: &fauna_core::data::ForeignFolder,
) -> bool {
    match record.set_name.as_deref() {
        None => false,
        Some(name) => cfg.foreign_sets.iter().any(|f| {
            f.is_live() && f.channel_id != record.channel_id && f.set_name.as_deref() == Some(name)
        }),
    }
}

/// The record's accept (see the upsert semantics above), stamped at
/// `now_micros`: a first accept, and a re-accept after a leave, stamp
/// `accepted_at` past the leave (ruling (l)(iii)); a change to any of the five
/// latest-wins advisory fields stamps `updated_at` past the held stamp
/// (ruling (l)(iv)). The stamps `record` carries are ignored.
pub fn record_foreign_set(
    cfg: &mut FoldersConfig,
    record: fauna_core::data::ForeignFolder,
    now_micros: u64,
) -> bool {
    match cfg
        .foreign_sets
        .iter_mut()
        .find(|f| f.channel_id == record.channel_id)
    {
        Some(existing) => {
            let mut updated = fauna_core::data::ForeignFolder {
                channel_id: record.channel_id,
                mls_group_id: record.mls_group_id,
                home_nest_url: record.home_nest_url,
                // Gain-only like `set_name`/`access`: a re-accept carries fresh
                // Some values (the home nest resolved them), so they land; only a
                // genuinely-absent field (a relay that omits it) falls back to the held one.
                home_nest_actor_id: record
                    .home_nest_actor_id
                    .or_else(|| existing.home_nest_actor_id.clone()),
                set_name: record.set_name.or_else(|| existing.set_name.clone()),
                access: record.access.or_else(|| existing.access.clone()),
                // The owner label lands whole when the re-delivered Welcome
                // carried a verified pair; an unstamped one keeps the held pair.
                owner_handle: if record.owner_handle.is_some() && record.owner_domain.is_some() {
                    record.owner_handle.clone()
                } else {
                    existing.owner_handle.clone()
                },
                owner_domain: if record.owner_handle.is_some() && record.owner_domain.is_some() {
                    record.owner_domain.clone()
                } else {
                    existing.owner_domain.clone()
                },
                // The Welcome relay carries no floor, so a re-accept never
                // clears the one the federated reads have stamped.
                content_key_floor: record.content_key_floor.or(existing.content_key_floor),
                // The Welcome's residency, when the home nest stated one, lands
                // on its own stamp; an unstated one keeps the held reading.
                residency: match record.residency {
                    Some(r) if existing.metadata_only_residency() != Some(r.metadata_only) => {
                        Some(fauna_core::data::ForeignResidency {
                            metadata_only: r.metadata_only,
                            stamped_at: now_micros
                                .max(existing.residency.map_or(0, |h| h.stamped_at + 1)),
                        })
                    }
                    _ => existing.residency,
                },
                accepted_at: existing.accepted_at,
                left_at: existing.left_at,
                updated_at: existing.updated_at,
            };
            let revive = !existing.is_live();
            if *existing == updated && !revive {
                return false;
            }
            if advisory_differs(existing, &updated) {
                updated.updated_at = now_micros.max(existing.updated_at + 1);
            }
            if revive {
                updated.accepted_at = now_micros
                    .max(existing.left_at.map_or(0, |l| l + 1))
                    .max(existing.accepted_at);
            }
            *existing = updated;
            true
        }
        None => {
            cfg.foreign_sets.push(fauna_core::data::ForeignFolder {
                accepted_at: now_micros,
                left_at: None,
                updated_at: now_micros,
                residency: record
                    .residency
                    .map(|r| fauna_core::data::ForeignResidency {
                        stamped_at: now_micros,
                        ..r
                    }),
                ..record
            });
            true
        }
    }
}

/// Name a held, still-nameless foreign set by opening the share's sealed name
/// under the set's own M2 content keys — the accept-time twin of
/// [`crate::engine_binding::named_for_engine_host`]'s member arm. A cross-nest
/// Welcome carries the name only sealed (`set_name_sealed` + `set_name_hash`),
/// and a share not yet joined holds no key to open it, so the accept records the
/// set nameless; once the join has ingested the owner's envelope (published
/// before the Welcome is delivered — `mls-group-key-material.md` § M2,
/// *Admitting a member*), the same audience that opens the set's files can name
/// it. Without a name the set has no engine binding and no Folders-page label.
///
/// Gain-only like [`record_foreign_set`]: a record that already carries a name,
/// no record, no keys, or a seal that does not open leaves `cfg` untouched.
/// Returns whether the config **changed**.
pub fn name_foreign_set_from_seal(
    cfg: &mut FoldersConfig,
    channel_id: &[u8; 32],
    sealed: &[u8],
    name_hash: &[u8],
    now_micros: u64,
) -> bool {
    use fauna_core::path_crypto::SealedLabelRender;
    let Some(record) = find_foreign_set(cfg, channel_id) else {
        return false;
    };
    if record.set_name.is_some() {
        return false;
    }
    let Some(content_keys) = content_keys(cfg, channel_id) else {
        return false;
    };
    let keys = fauna_core::file_download::FileDownloadKeys {
        mls_group_id: Some(record.mls_group_id.clone()),
        content_keys: Some(content_keys),
        ..Default::default()
    };
    let name = match fauna_core::label_custody::render_set_name(
        &keys,
        Some(sealed),
        "",
        Some(name_hash),
    ) {
        SealedLabelRender::Sealed(name) if !name.is_empty() => name,
        _ => return false,
    };
    let named = fauna_core::data::ForeignFolder {
        set_name: Some(name),
        ..record.clone()
    };
    record_foreign_set(cfg, named, now_micros)
}

/// Whether the five latest-wins advisory fields differ between two copies of
/// one record (ruling (l)(iv)) — a change to any of them is what stamps
/// `updated_at`.
fn advisory_differs(
    a: &fauna_core::data::ForeignFolder,
    b: &fauna_core::data::ForeignFolder,
) -> bool {
    a.access != b.access
        || a.home_nest_url != b.home_nest_url
        || a.home_nest_actor_id != b.home_nest_actor_id
        || a.mls_group_id != b.mls_group_id
        || a.owner_handle != b.owner_handle
        || a.owner_domain != b.owner_domain
}

/// The home nest's stamps one federated content-key read reply carried, as
/// [`refresh_foreign_set_from_reply`] takes them — each `None` asserts
/// nothing (a same-nest read, a field the home nest did not state), so a
/// caller names only what the reply held (`..Default::default()`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ForeignReplyStamps<'a> {
    /// The caller's current access grant (`"reader"`/`"writer"`).
    pub access: Option<&'a str>,
    /// The home nest's deployment `nest_actor_id` (hex).
    pub home_nest_actor_id: Option<&'a str>,
    /// The set's owner-stamped content-key floor.
    pub content_key_floor: Option<u64>,
    /// The folder's residency stamp (`"metadata_only"`/`"full"`).
    pub residency: Option<&'a str>,
    /// The set's owner label `(handle, domain)`, as the member's own nest
    /// verified it.
    pub owner: Option<(&'a str, &'a str)>,
}

/// Refresh a foreign set's advisory fields from the stamps a federated read
/// reply carried (`../architecture/federation.md` § Cross-nest → *Recipient-side
/// access discovery*): the caller's `access` grant, the home nest's
/// `home_nest_actor_id` (byte-plane SPKI-pin trust root), and the set's
/// owner-stamped `content_key_floor` (`../behavior/on-demand-files.md` § Shared sets on a
/// capability host → *One mechanism*, question 2 — what the member's engine
/// arms its pre-seal hold from), and the folder's `residency`
/// (`../behavior/file-sync.md` § Relay serving → *A member on another nest*,
/// step (1) — what it arms the upload skip and the holder-keeps gate from),
/// and the set's `owner` label (`handle`, `domain`) as the member's own nest
/// verified it (`../architecture/federation.md` § Cross-nest shared folders +
/// channel append → *The cross-nest owner label*), written as one pair.
/// Unlike [`record_foreign_set`] each is a
/// **last-writer-wins overwrite** where the stamp is `Some`: the home nest just
/// resolved these while gating the read, so a demotion `writer → reader` must
/// land, not be `.or`-swallowed — which is why a changed grant or identity
/// stamps `updated_at` past the held stamp at `now_micros` (ruling
/// (l)(iv)); the floor is a maximum and needs no stamp; a changed residency
/// stamps its own [`fauna_core::data::ForeignResidency::stamped_at`] the same
/// way. A residency stamp this build cannot parse states nothing.
///
/// Returns whether the config **changed** — the CAS-skip contract every custody
/// writer carries, so an unchanged set of stamps (the overwhelmingly common
/// case, once per poll per set) costs no plane write at all.
///
/// A `None` stamp means the home nest asserted nothing for that field (a
/// same-nest read, or no role row); that is **not** a claim it was cleared, so it leaves
/// the held value alone. Revocation is enforced at the next mint/record
/// (fail-closed + loud), never inferred from an absent field. Unknown
/// `channel_id` (not a foreign set — e.g. a same-nest read), or a record the
/// member has left, is a no-op.
pub fn refresh_foreign_set_from_reply(
    cfg: &mut FoldersConfig,
    channel_id: &[u8; 32],
    stamps: ForeignReplyStamps<'_>,
    now_micros: u64,
) -> bool {
    let ForeignReplyStamps {
        access,
        home_nest_actor_id,
        content_key_floor,
        residency,
        owner,
    } = stamps;
    let Some(existing) = cfg
        .foreign_sets
        .iter_mut()
        .find(|f| &f.channel_id == channel_id && f.is_live())
    else {
        return false;
    };
    let mut advisory = false;
    if let Some(access) = access
        && existing.access.as_deref() != Some(access)
    {
        existing.access = Some(access.to_string());
        advisory = true;
    }
    if let Some(actor_id) = home_nest_actor_id
        && existing.home_nest_actor_id.as_deref() != Some(actor_id)
    {
        existing.home_nest_actor_id = Some(actor_id.to_string());
        advisory = true;
    }
    // The cross-nest owner label (`federation.md` § … *The cross-nest owner
    // label*, join rule): the pair the member's own nest verified, written
    // whole — a renamed owner relabels on the next poll.
    if let Some((handle, domain)) = owner
        && (
            existing.owner_handle.as_deref(),
            existing.owner_domain.as_deref(),
        ) != (Some(handle), Some(domain))
    {
        existing.owner_handle = Some(handle.to_string());
        existing.owner_domain = Some(domain.to_string());
        advisory = true;
    }
    if advisory {
        existing.updated_at = now_micros.max(existing.updated_at + 1);
    }
    let mut floor_changed = false;
    if let Some(floor) = content_key_floor
        && existing.content_key_floor != Some(floor)
    {
        existing.content_key_floor = Some(floor);
        floor_changed = true;
    }
    let mut residency_changed = false;
    if let Some(metadata_only) = fauna_core::data::ForeignResidency::parse_stamp(residency)
        && existing.metadata_only_residency() != Some(metadata_only)
    {
        let held = existing.residency.map_or(0, |r| r.stamped_at);
        existing.residency = Some(fauna_core::data::ForeignResidency {
            metadata_only,
            stamped_at: now_micros.max(held + 1),
        });
        residency_changed = true;
    }
    advisory || floor_changed || residency_changed
}

/// The LIVE foreign-set record for `channel_id`, if this holder is a
/// cross-nest member of it — the routing lookup the custody-ingest sink and
/// the commit poll consult (channel → home nest) before falling back to
/// "same-nest set". A record the member has left answers nothing.
pub fn find_foreign_set<'a>(
    cfg: &'a FoldersConfig,
    channel_id: &[u8; 32],
) -> Option<&'a fauna_core::data::ForeignFolder> {
    cfg.foreign_sets
        .iter()
        .find(|f| &f.channel_id == channel_id && f.is_live())
}

/// The LIVE foreign-set record whose display `set_name` hashes to `name_hash`
/// — the key resolver's lookup direction, addressed the way its roster twin
/// [`find_roster_row`] is. Only a named record can match (a name-less record,
/// from an unopenable seal, is unaddressable by name — its reads still work by
/// channel via [`find_foreign_set`]).
pub fn find_foreign_set_by_hash<'a>(
    cfg: &'a FoldersConfig,
    name_hash: &[u8; 32],
) -> Option<&'a fauna_core::data::ForeignFolder> {
    cfg.foreign_sets.iter().find(|f| {
        f.is_live()
            && f.set_name
                .as_deref()
                .is_some_and(|n| fauna_core::path_crypto::set_name_hash(n) == *name_hash)
    })
}

/// The live foreign-set records — what every list and binding reads (a left
/// record stays as a tombstone, ruling (l)(iii)).
pub fn live_foreign_sets(
    cfg: &FoldersConfig,
) -> impl Iterator<Item = &fauna_core::data::ForeignFolder> {
    cfg.foreign_sets.iter().filter(|f| f.is_live())
}

/// Tombstone a foreign-set record when the member leaves the set (the foreign
/// twin of [`forget_set`], called alongside it from the leave path): its
/// `left_at` is stamped at `now_micros`, never before its accept, and the
/// record is never removed — a store that never removes an entry would keep
/// it live, and a removed record returns from any stale device (ruling
/// (l)(iii)). A later accept revives it. Returns whether a live record was
/// left (idempotent).
pub fn forget_foreign_set(cfg: &mut FoldersConfig, channel_id: &[u8; 32], now_micros: u64) -> bool {
    match cfg
        .foreign_sets
        .iter_mut()
        .find(|f| &f.channel_id == channel_id && f.is_live())
    {
        Some(record) => {
            record.left_at = Some(now_micros.max(record.accepted_at));
            true
        }
        None => false,
    }
}

/// Retire every live entry holding `channel_id` when its holder leaves the
/// set — a member's leave retires its received copy (ruling (e)): the entry
/// keeps its keys (no-data-loss) and is never removed, since the union merge
/// would resurrect a removed entry from any stale device; every holder, since
/// an identity move can leave two (ruling (l)(i)). Returns whether a live
/// entry was retired (idempotent — already-retired returns `false`).
pub fn forget_set(cfg: &mut FoldersConfig, channel_id: &[u8; 32], now_micros: u64) -> bool {
    let mut changed = false;
    for s in cfg.sets.iter_mut() {
        if s.channel_id.as_ref() == Some(channel_id) && s.is_live() {
            s.retire(now_micros);
            changed = true;
        }
    }
    changed
}

/// Rotate `channel_id`'s content key **in place**: append a fresh generation
/// (`current.version + 1`, strictly-later `rotated_at`) and make it `current`,
/// returning the new generation — the **serve-disable** rotation
/// (`webdav-server.md` § Key model: unflagging a served set rotates so a leaked
/// `WebdavKeysBlob` never covers future content).
///
/// Unlike a member-removal rotation this commits directly (no
/// `pending_removals` staging): the fresh key is created and committed in the
/// **same** custody write, so there is no publish-before-commit window in which
/// it is irrecoverable — and the committed generation lives in the existing
/// union-merged `sets` field, which every device (old or new) merges without
/// loss. `None` when the owner holds no keys for the set (nothing to rotate; the
/// caller decides whether that is an error).
pub fn rotate_set(
    cfg: &mut FoldersConfig,
    channel_id: &[u8; 32],
    fresh_key: [u8; 32],
    now_micros: u64,
) -> Option<ContentKeyGeneration> {
    let i = set_index(cfg, channel_id)?;
    let keys = joined_keys(cfg, channel_id)?;
    let current = &keys.current;
    let generation = ContentKeyGeneration {
        version: current.version + 1,
        key: fresh_key.into(),
        // Strictly increasing even against a same-instant clock (mirrors the
        // removal rotation's `next_rotated_at`).
        rotated_at: now_micros.max(current.rotated_at + 1),
    };
    let incoming = FolderContentKeys {
        current: generation.clone(),
        prior: Vec::new(),
    };
    cfg.sets[i].keys = Some(keys.merge(&incoming));
    Some(generation)
}

/// Migrate a set's custody identity from `from` to `to` — the **share-after-serve**
/// re-key (`webdav-server.md` § Key model, custody note): a served-but-unshared
/// set is keyed by the serve pseudo-channel
/// ([`fauna_core::folder_keys::serve_custody_channel_id`]); when the owner
/// shares it, `bind_set` calls this with the real derived `ChannelId` so the
/// existing generations (files are already stamped with their versions) move to
/// the group identity and ride the content-key envelope to members.
///
/// - Re-points every live entry holding `from` to `to` **in place** (the
///   set-nonce ruling (b): bound dominates served — the entry keeps its nonce).
///   A live `to` entry that already exists (a resumed bind after a peer
///   device's migration merged in) is left beside it: both now hold `to`, and
///   every by-channel read joins them ([`content_keys`] — no generation lost).
///   Nothing is removed (ruling (l)(ii)).
/// - A nonce-less entry's identity IS its channel, so it cannot be re-pointed
///   on a store keyed by identity: it is retired at `now_micros` and a live
///   copy under `to` added — never a live twin left on the pseudo-channel.
/// - Idempotent: no live `from` entry ⇒ no-op, returns `false`.
pub fn migrate_set_identity(
    cfg: &mut FoldersConfig,
    from: &[u8; 32],
    to: [u8; 32],
    now_micros: u64,
) -> bool {
    if from == &to {
        return false;
    }
    let mut moved_copies = Vec::new();
    let mut changed = false;
    for s in cfg.sets.iter_mut() {
        if s.channel_id.as_ref() != Some(from) || !s.is_live() {
            continue;
        }
        changed = true;
        if s.set_nonce.is_some() {
            s.channel_id = Some(to);
        } else {
            moved_copies.push(FolderKeyCustody {
                channel_id: Some(to),
                ..s.clone()
            });
            s.retire(now_micros);
        }
    }
    cfg.sets.extend(moved_copies);
    changed
}

// ── crash-recovery sentinel (member-removal rotation) ───────────────────────
//
// A removal rotates to a *fresh* content key that is irrecoverable once the nest
// stores the re-sealed envelope, so the orchestration (`crate::orchestration`)
// stages the new generation in `FoldersConfig::pending_removals` BEFORE the
// network publish and only commits it into the set's `current` once the nest
// confirms. These pure helpers are the staged-removal half of that lifecycle,
// mirroring `fauna-client-subscriptions::custody`'s `stage`/`find`/`clear`. Identity
// of a staged removal is `(channel_id, removed_member, new_generation.key)` — the
// random fresh key uniquely distinguishes one device's staging from another's, so
// a `rotated_at` re-stage (on a retry) updates in place without clobbering a
// concurrent device's differently-keyed staging the merge unioned in.

/// Whether two stagings refer to the same fresh-key removal (stable across a
/// `rotated_at` bump).
fn same_staging(
    r: &FolderPendingRemoval,
    channel_id: &[u8; 32],
    removed_member: &ActorId,
    key: &[u8; 32],
) -> bool {
    &r.channel_id == channel_id
        && &r.removed_member == removed_member
        && &r.new_generation.key == key
}

/// Stage (or re-stage) a member-removal rotation sentinel. Upserts by the stable
/// `(channel, member, key)` identity: an existing entry for the same fresh key is
/// replaced (so a `rotated_at` bump persists in place), and a genuinely new staging
/// is appended — a concurrent device's differently-keyed staging is never dropped
/// (no-data-loss). Persist `cfg` after calling, BEFORE the nest publish.
pub fn stage_pending_removal(cfg: &mut FoldersConfig, removal: FolderPendingRemoval) {
    let (channel_id, removed_member, key) = (
        removal.channel_id,
        removal.removed_member,
        removal.new_generation.key.clone(),
    );
    fauna_core::keyed_staging::stage(&mut cfg.pending_removals, removal, |r| {
        same_staging(r, &channel_id, &removed_member, &key)
    });
}

/// Record the produced MLS Remove **commit bytes** onto the already-staged
/// removal sentinel for `(channel_id, removed_member)` — persisted BEFORE the
/// epoch-advancing merge (and so before the channel send) so a crash-resumed
/// drive re-distributes the same bytes to the remaining members (5d(d)
/// epoch-advance liveness; `devices.md` § Cross-device MLS group-state sync,
/// Rule 1). Returns whether a sentinel was found (a `false` means nothing was
/// staged — caller bug).
///
/// **Keep-first, never overwrite:** bytes already recorded on the sentinel may
/// describe a commit that was **distributed** — overwriting them with a rebuilt
/// commit destroys the only durable copy of the transition the remaining
/// members already merged, the exact fork. The drive never rebuilds
/// while bytes exist, so a differing overwrite attempt is a caller bug; keeping
/// the first write makes the invariant structural.
pub fn stage_removal_commit(
    cfg: &mut FoldersConfig,
    channel_id: &[u8; 32],
    removed_member: &ActorId,
    commit: Vec<u8>,
) -> bool {
    match cfg
        .pending_removals
        .iter_mut()
        .find(|r| &r.channel_id == channel_id && &r.removed_member == removed_member)
    {
        Some(r) => {
            if r.commit.is_none() {
                r.commit = Some(commit);
            }
            true
        }
        None => false,
    }
}

/// The first staged removal awaiting commit for `(channel_id, removed_member)`, if
/// any — the resume entry point reads this to re-drive an interrupted publish.
pub fn find_pending_removal<'a>(
    cfg: &'a FoldersConfig,
    channel_id: &[u8; 32],
    removed_member: &ActorId,
) -> Option<&'a FolderPendingRemoval> {
    fauna_core::keyed_staging::find(&cfg.pending_removals, |r| {
        &r.channel_id == channel_id && &r.removed_member == removed_member
    })
}

/// Whether a *different* member's staged removal on `channel_id` carries durable
/// commit bytes.
/// While one exists, a fresh ungated build on the channel must defer: the engine's
/// live staged pending may be that byted sibling's restored merge source, and both
/// `clear_pending_commit` and staging a new commit would destroy it —
/// [`FoldersAuthor::resume_pending_removals`](crate::orchestration::FoldersAuthor::resume_pending_removals)
/// drives byted sentinels first so the sibling completes and unblocks the channel.
pub fn has_other_pending_removal_with_commit(
    cfg: &FoldersConfig,
    channel_id: &[u8; 32],
    removed_member: &ActorId,
) -> bool {
    cfg.pending_removals.iter().any(|r| {
        &r.channel_id == channel_id && &r.removed_member != removed_member && r.commit.is_some()
    })
}

/// Stamp the staged removal for `(channel_id, removed_member)` as having had an
/// **engaged** gated attempt started — persist `cfg` (CAS) BEFORE the gate runs,
/// so the stamp is durable before anything can be distributed (the gated route's
/// commit rides the rebase loop and records no `commit` bytes on the sentinel).
/// One-way: never unset (`true` is the join's top —
/// [`FolderPendingRemoval::gated_attempted`]). Returns whether the stamp
/// **changed** (a `false` means it was already stamped, or no sentinel is
/// staged — the caller skips the redundant persist).
pub fn mark_removal_gated_attempted(
    cfg: &mut FoldersConfig,
    channel_id: &[u8; 32],
    removed_member: &ActorId,
) -> bool {
    match cfg
        .pending_removals
        .iter_mut()
        .find(|r| &r.channel_id == channel_id && &r.removed_member == removed_member)
    {
        Some(r) if !r.gated_attempted => {
            r.gated_attempted = true;
            true
        }
        _ => false,
    }
}

/// Drop the staged removal identified by `(channel, member, key)` once the nest has
/// confirmed the publish and the rotation is committed into `current`. Returns
/// whether an entry was removed (idempotent). Keyed on the fresh key so a concurrent
/// device's differently-keyed staging survives. The one removal this API makes: on
/// the account plane it is the store's settle
/// (`AccountStoreHandle::settle_folder_removal`), which marks the staging's row
/// settled so it leaves the fold on every replica.
pub fn clear_pending_removal(
    cfg: &mut FoldersConfig,
    channel_id: &[u8; 32],
    removed_member: &ActorId,
    key: &[u8; 32],
) -> bool {
    fauna_core::keyed_staging::clear(&mut cfg.pending_removals, |r| {
        same_staging(r, channel_id, removed_member, key)
    })
}

/// **Idempotently** commit a staged generation into the set's history: `generation`
/// becomes `current` (the rotated-out prior current moves into `prior`), and
/// re-committing an already-applied generation is a no-op. Built on
/// [`FolderContentKeys::merge`], so it never drops a key and converges regardless
/// of how many times a crash-resumed commit replays it. Errors with
/// [`CustodyError::SetNotFound`] if the owner holds no keys for the set (a removal
/// always targets a set the owner bound a key for).
pub fn commit_generation(
    cfg: &mut FoldersConfig,
    channel_id: &[u8; 32],
    generation: &ContentKeyGeneration,
) -> Result<(), CustodyError> {
    let (Some(i), Some(keys)) = (set_index(cfg, channel_id), joined_keys(cfg, channel_id)) else {
        return Err(CustodyError::SetNotFound);
    };
    let incoming = FolderContentKeys {
        current: generation.clone(),
        prior: Vec::new(),
    };
    cfg.sets[i].keys = Some(keys.merge(&incoming));
    Ok(())
}

// (No pre-bind re-seal sentinel lives here any more — retired 2026-09-25. The M2
// history-on-join re-seal is the sync agent's ungated, idempotent
// `SyncEngine::reseal_pending_under_current` on every engine start, terminating
// on the local row's `content_key_version` stamp; no custody marker is needed to
// decide whether a set "still owes" it. `mls-group-key-material.md` § M2
// *Pre-bind re-seal migration*.)

#[cfg(test)]
mod tests {
    /// The clock the stamped custody transitions take in these tests.
    const NOW: u64 = 1_000;
    use super::*;
    use fauna_core::data::FolderPendingRemoval;
    use fauna_core::identity::ActorId;

    fn cfg() -> FoldersConfig {
        FoldersConfig::default()
    }

    fn chan(b: u8) -> [u8; 32] {
        [b; 32]
    }

    fn foreign(b: u8, url: &str, name: Option<&str>) -> fauna_core::data::ForeignFolder {
        foreign_with_access(b, url, name, None)
    }

    fn foreign_with_access(
        b: u8,
        url: &str,
        name: Option<&str>,
        access: Option<&str>,
    ) -> fauna_core::data::ForeignFolder {
        fauna_core::data::ForeignFolder {
            channel_id: chan(b),
            mls_group_id: vec![b, b, b],
            home_nest_url: url.into(),
            home_nest_actor_id: None,
            set_name: name.map(Into::into),
            access: access.map(Into::into),
            content_key_floor: None,
            ..Default::default()
        }
    }

    /// A cross-nest accept records the set nameless (the Welcome's name is
    /// sealed); once the join has ingested the set's content keys, the sealed
    /// name opens and lands — and never displaces a name already held, nor
    /// lands from a seal custody cannot open.
    #[test]
    fn a_nameless_foreign_set_is_named_from_its_seal_once_keys_are_held() {
        let content = FolderContentKeys::genesis([0x42; 32], 1);
        let root = fauna_core::path_crypto::LabelRoot::content_key(
            *content.current_key(),
            content.current_version(),
        );
        let sealed = fauna_core::label_custody::seal_set_name(&root, "xnest-docs")
            .unwrap()
            .unwrap();
        let hash = fauna_core::path_crypto::set_name_hash("xnest-docs");

        let mut c = cfg();
        assert!(record_foreign_set(
            &mut c,
            foreign(0xAA, "https://home.example", None),
            NOW
        ));
        assert!(
            !name_foreign_set_from_seal(&mut c, &chan(0xAA), &sealed, &hash, NOW + 1),
            "no content keys yet — the seal cannot open, nothing changes"
        );
        merge_received_keys(&mut c, chan(0xAA), content.clone());
        assert!(name_foreign_set_from_seal(
            &mut c,
            &chan(0xAA),
            &sealed,
            &hash,
            NOW + 2
        ));
        assert_eq!(
            find_foreign_set(&c, &chan(0xAA))
                .unwrap()
                .set_name
                .as_deref(),
            Some("xnest-docs")
        );
        assert!(
            !name_foreign_set_from_seal(&mut c, &chan(0xAA), &sealed, &hash, NOW + 3),
            "idempotent once named"
        );

        let mut held = cfg();
        assert!(record_foreign_set(
            &mut held,
            foreign(0xBB, "https://home.example", Some("kept")),
            NOW
        ));
        merge_received_keys(&mut held, chan(0xBB), content);
        assert!(!name_foreign_set_from_seal(
            &mut held,
            &chan(0xBB),
            &sealed,
            &hash,
            NOW + 1
        ));
        assert_eq!(
            find_foreign_set(&held, &chan(0xBB))
                .unwrap()
                .set_name
                .as_deref(),
            Some("kept"),
            "gain-only: a held name is never displaced"
        );
    }

    /// The collision loudness contract: a same-name /
    /// different-channel foreign record is DETECTED (the production sink warns
    /// on it) and still records — refusing would break share-accept, since two
    /// distinct nests legitimately both share `photos`. Same-channel
    /// re-records and name-less records are not collisions.
    #[test]
    fn a_foreign_name_collision_is_detected_and_still_records() {
        let mut c = cfg();
        assert!(record_foreign_set(
            &mut c,
            foreign(0xAA, "https://home-a.example", Some("photos")),
            NOW
        ));

        let colliding = foreign(0xBB, "https://home-b.example", Some("photos"));
        assert!(
            foreign_name_collides(&c, &colliding),
            "same display name, different channel — the shadowing shape"
        );
        assert!(
            !foreign_name_collides(&c, &foreign(0xAA, "https://home-a.example", Some("photos"))),
            "a same-channel re-record is not a collision"
        );
        assert!(
            !foreign_name_collides(&c, &foreign(0xCC, "https://home-c.example", None)),
            "a name-less record cannot collide"
        );

        assert!(
            record_foreign_set(&mut c, colliding, NOW),
            "collision still records"
        );
        assert_eq!(c.foreign_sets.len(), 2, "both records held");
    }

    /// Accept-time record → find by channel and by name; re-record is
    /// idempotent; a name-less re-delivery preserves a held name while a
    /// re-bind updates the URL; forget removes and is idempotent.
    #[test]
    fn foreign_set_record_find_forget_lifecycle() {
        let mut c = cfg();
        assert!(record_foreign_set(
            &mut c,
            foreign(0xAA, "https://home-a.example", Some("photos")),
            NOW
        ));
        // Idempotent re-record.
        assert!(!record_foreign_set(
            &mut c,
            foreign(0xAA, "https://home-a.example", Some("photos")),
            NOW
        ));
        assert_eq!(
            find_foreign_set(&c, &chan(0xAA))
                .unwrap()
                .set_name
                .as_deref(),
            Some("photos")
        );
        assert!(
            find_foreign_set_by_hash(&c, &fauna_core::path_crypto::set_name_hash("photos"))
                .is_some()
        );
        assert!(
            find_foreign_set_by_hash(&c, &fauna_core::path_crypto::set_name_hash("absent"))
                .is_none()
        );
        // A name-less re-delivery (old sharer nest) must not erase the name;
        // a re-bound URL wins (last-writer, S1 parity).
        assert!(record_foreign_set(
            &mut c,
            foreign(0xAA, "https://home-b.example", None),
            NOW
        ));
        let rec = find_foreign_set(&c, &chan(0xAA)).unwrap();
        assert_eq!(rec.set_name.as_deref(), Some("photos"));
        assert_eq!(rec.home_nest_url, "https://home-b.example");
        // Forget removes; a second forget is a no-op.
        assert!(forget_foreign_set(&mut c, &chan(0xAA), NOW));
        assert!(!forget_foreign_set(&mut c, &chan(0xAA), NOW));
        assert!(find_foreign_set(&c, &chan(0xAA)).is_none());
    }

    /// The advisory grant: a re-accept only *gains* it when the relaying nest
    /// asserted nothing, but a genuine demotion re-accept writes through; and
    /// the `caller_access` refresh is last-writer-wins in BOTH directions,
    /// CAS-skipping when nothing changed.
    #[test]
    fn foreign_set_access_seeds_then_refreshes_both_directions() {
        let mut c = cfg();
        assert!(record_foreign_set(
            &mut c,
            foreign_with_access(0xAA, "https://home-a.example", Some("docs"), Some("writer")),
            NOW
        ));
        // An access-less re-delivery (no role row) must not erase the grant.
        assert!(!record_foreign_set(
            &mut c,
            foreign_with_access(0xAA, "https://home-a.example", Some("docs"), None),
            NOW
        ));
        assert_eq!(
            find_foreign_set(&c, &chan(0xAA)).unwrap().access.as_deref(),
            Some("writer")
        );
        // A demotion re-accept DOES write through (`new` is Some, not None).
        assert!(record_foreign_set(
            &mut c,
            foreign_with_access(0xAA, "https://home-a.example", Some("docs"), Some("reader")),
            NOW
        ));
        assert_eq!(
            find_foreign_set(&c, &chan(0xAA)).unwrap().access.as_deref(),
            Some("reader")
        );
        // Refresh: promotion lands, an unchanged stamp CAS-skips, demotion lands.
        assert!(refresh_foreign_set_from_reply(
            &mut c,
            &chan(0xAA),
            ForeignReplyStamps {
                access: Some("writer"),
                ..Default::default()
            },
            NOW
        ));
        assert!(!refresh_foreign_set_from_reply(
            &mut c,
            &chan(0xAA),
            ForeignReplyStamps {
                access: Some("writer"),
                ..Default::default()
            },
            NOW
        ));
        assert!(refresh_foreign_set_from_reply(
            &mut c,
            &chan(0xAA),
            ForeignReplyStamps {
                access: Some("reader"),
                ..Default::default()
            },
            NOW
        ));
        assert_eq!(
            find_foreign_set(&c, &chan(0xAA)).unwrap().access.as_deref(),
            Some("reader")
        );
        // An absent stamp asserts nothing — it never revokes, and an unknown
        // channel (a same-nest read) is a no-op.
        assert!(!refresh_foreign_set_from_reply(
            &mut c,
            &chan(0xAA),
            ForeignReplyStamps::default(),
            NOW
        ));
        assert_eq!(
            find_foreign_set(&c, &chan(0xAA)).unwrap().access.as_deref(),
            Some("reader")
        );
        assert!(!refresh_foreign_set_from_reply(
            &mut c,
            &chan(0xBB),
            ForeignReplyStamps {
                access: Some("writer"),
                ..Default::default()
            },
            NOW
        ));

        // The same reply carrier refreshes the home-nest identity (byte-plane pin
        // trust root) — last-writer-wins where the
        // stamp is Some, CAS-skip when unchanged, no-op when absent.
        assert!(refresh_foreign_set_from_reply(
            &mut c,
            &chan(0xAA),
            ForeignReplyStamps {
                home_nest_actor_id: Some("ab".repeat(32).as_str()),
                ..Default::default()
            },
            NOW
        ));
        assert_eq!(
            find_foreign_set(&c, &chan(0xAA))
                .unwrap()
                .home_nest_actor_id
                .as_deref(),
            Some("ab".repeat(32).as_str())
        );
        // An unchanged identity CAS-skips; access untouched by its refresh.
        assert!(!refresh_foreign_set_from_reply(
            &mut c,
            &chan(0xAA),
            ForeignReplyStamps {
                home_nest_actor_id: Some("ab".repeat(32).as_str()),
                ..Default::default()
            },
            NOW
        ));
        assert_eq!(
            find_foreign_set(&c, &chan(0xAA)).unwrap().access.as_deref(),
            Some("reader")
        );
    }

    /// The cross-nest owner label (`federation.md` § Cross-nest shared
    /// folders + channel append → *The cross-nest owner label*, join rule): the read
    /// reply's verified pair refreshes the record whole and stamps
    /// `updated_at` like `access`; an unchanged pair CAS-skips; an absent pair
    /// keeps what is held. A re-accept carrying a pair lands it, one carrying
    /// none keeps the held pair.
    #[test]
    fn foreign_set_owner_label_refreshes_as_an_advisory_pair() {
        let mut c = cfg();
        assert!(record_foreign_set(
            &mut c,
            fauna_core::data::ForeignFolder {
                owner_handle: Some("alice".into()),
                owner_domain: Some("a.example".into()),
                ..foreign_with_access(0xAA, "https://home-a.example", Some("docs"), Some("reader"))
            },
            NOW
        ));
        let held = |c: &FoldersConfig| {
            let f = find_foreign_set(c, &chan(0xAA)).unwrap();
            (f.owner_handle.clone(), f.owner_domain.clone(), f.updated_at)
        };
        assert_eq!(
            held(&c),
            (Some("alice".into()), Some("a.example".into()), NOW),
            "accept writes the Welcome's pair"
        );
        // A renamed owner: the pair lands whole, past the held stamp.
        assert!(refresh_foreign_set_from_reply(
            &mut c,
            &chan(0xAA),
            ForeignReplyStamps {
                owner: Some(("alicia", "a.example")),
                ..Default::default()
            },
            NOW
        ));
        assert_eq!(
            held(&c),
            (Some("alicia".into()), Some("a.example".into()), NOW + 1),
            "a changed pair is an advisory change — it stamps `updated_at`"
        );
        // Unchanged → CAS-skip; absent → keep.
        assert!(!refresh_foreign_set_from_reply(
            &mut c,
            &chan(0xAA),
            ForeignReplyStamps {
                owner: Some(("alicia", "a.example")),
                ..Default::default()
            },
            NOW
        ));
        assert!(!refresh_foreign_set_from_reply(
            &mut c,
            &chan(0xAA),
            ForeignReplyStamps::default(),
            NOW
        ));
        assert_eq!(held(&c).0.as_deref(), Some("alicia"));
        // A re-accept with no pair keeps it; one with a pair lands it.
        record_foreign_set(
            &mut c,
            foreign_with_access(0xAA, "https://home-a.example", Some("docs"), Some("reader")),
            NOW + 5,
        );
        assert_eq!(
            held(&c).0.as_deref(),
            Some("alicia"),
            "an unstamped re-accept keeps the pair"
        );
        assert!(record_foreign_set(
            &mut c,
            fauna_core::data::ForeignFolder {
                owner_handle: Some("al".into()),
                owner_domain: Some("a.example".into()),
                ..foreign_with_access(0xAA, "https://home-a.example", Some("docs"), Some("reader"))
            },
            NOW + 6
        ));
        assert_eq!(
            held(&c),
            (Some("al".into()), Some("a.example".into()), NOW + 6)
        );
    }

    /// The federated floor (`on-demand-files.md` § Shared sets on a capability
    /// host → *One mechanism*, question 2): the home nest's stamp of the set's
    /// owner-stamped `content_key_floor` refreshes the record last-writer-wins
    /// where `Some` (the home nest just read it off its row), CAS-skips when
    /// unchanged, and an absent stamp keeps the held value — never a claim the
    /// floor was cleared. A re-accept (the Welcome carries no floor) keeps it too.
    #[test]
    fn foreign_set_floor_refreshes_last_writer_wins_and_cas_skips() {
        let mut c = cfg();
        assert!(record_foreign_set(
            &mut c,
            foreign_with_access(0xAA, "https://home-a.example", Some("docs"), Some("writer")),
            NOW
        ));
        let floor_of =
            |c: &FoldersConfig| find_foreign_set(c, &chan(0xAA)).unwrap().content_key_floor;
        assert_eq!(floor_of(&c), None, "share-accept carries no floor");
        assert!(refresh_foreign_set_from_reply(
            &mut c,
            &chan(0xAA),
            ForeignReplyStamps {
                content_key_floor: Some(2),
                ..Default::default()
            },
            NOW
        ));
        assert_eq!(floor_of(&c), Some(2));
        assert!(
            !refresh_foreign_set_from_reply(
                &mut c,
                &chan(0xAA),
                ForeignReplyStamps {
                    content_key_floor: Some(2),
                    ..Default::default()
                },
                NOW
            ),
            "an unchanged stamp CAS-skips"
        );
        assert!(
            !refresh_foreign_set_from_reply(
                &mut c,
                &chan(0xAA),
                ForeignReplyStamps::default(),
                NOW
            ),
            "an absent stamp asserts nothing"
        );
        assert_eq!(floor_of(&c), Some(2), "…and never clears the held floor");
        assert!(refresh_foreign_set_from_reply(
            &mut c,
            &chan(0xAA),
            ForeignReplyStamps {
                content_key_floor: Some(3),
                ..Default::default()
            },
            NOW
        ));
        assert_eq!(floor_of(&c), Some(3), "a rotation's stamp lands");
        assert!(
            !record_foreign_set(
                &mut c,
                foreign_with_access(0xAA, "https://home-a.example", Some("docs"), Some("writer")),
                NOW
            ),
            "a floor-less re-accept changes nothing"
        );
        assert_eq!(floor_of(&c), Some(3), "…and keeps the held floor");
    }

    /// The federated residency stamp (`file-sync.md` § Relay serving → *A
    /// member on another nest*, step (1)): a record no home nest has stamped
    /// reads unknown; a stated stamp lands on its own stamp in both directions,
    /// an unchanged one CAS-skips, and an absent or unparseable stamp asserts
    /// nothing — never a claim the folder went full. The Welcome seeds it, and a
    /// re-accept that states none keeps the held reading.
    #[test]
    fn foreign_set_residency_refreshes_on_its_own_stamp_and_never_reads_absent_as_full() {
        let mut c = cfg();
        let mut seeded = foreign_with_access(0xAA, "https://home-a.example", Some("docs"), None);
        assert!(record_foreign_set(&mut c, seeded.clone(), NOW));
        let residency_of = |c: &FoldersConfig| {
            find_foreign_set(c, &chan(0xAA))
                .unwrap()
                .metadata_only_residency()
        };
        assert_eq!(residency_of(&c), None, "an unstamped record reads unknown");
        let refresh = |c: &mut FoldersConfig, stamp: Option<&str>, now: u64| {
            refresh_foreign_set_from_reply(
                c,
                &chan(0xAA),
                ForeignReplyStamps {
                    residency: stamp,
                    ..Default::default()
                },
                now,
            )
        };
        assert!(
            !refresh(&mut c, None, NOW),
            "an absent stamp asserts nothing"
        );
        assert!(!refresh(&mut c, Some(""), NOW), "nor does an empty one");
        assert!(!refresh(&mut c, Some("cold"), NOW), "nor an unknown value");
        assert_eq!(residency_of(&c), None);
        assert!(refresh(&mut c, Some("metadata_only"), NOW));
        assert_eq!(residency_of(&c), Some(true));
        assert!(!refresh(&mut c, Some("metadata_only"), NOW + 5), "CAS-skip");
        assert!(
            !refresh(&mut c, None, NOW + 5),
            "absent keeps metadata-only"
        );
        assert_eq!(residency_of(&c), Some(true));
        let stamp_of =
            |c: &FoldersConfig| find_foreign_set(c, &chan(0xAA)).unwrap().residency.unwrap();
        assert!(refresh(&mut c, Some("full"), 0), "the flip back lands");
        assert_eq!(residency_of(&c), Some(false));
        assert_eq!(
            stamp_of(&c).stamped_at,
            NOW + 1,
            "strictly past the held stamp"
        );
        assert!(
            !record_foreign_set(&mut c, seeded.clone(), NOW + 9),
            "a re-accept stating no residency changes nothing"
        );
        assert_eq!(residency_of(&c), Some(false));
        seeded.residency = Some(fauna_core::data::ForeignResidency {
            metadata_only: true,
            stamped_at: 0,
        });
        assert!(
            record_foreign_set(&mut c, seeded, NOW + 9),
            "a re-accept's stated residency lands"
        );
        assert_eq!(residency_of(&c), Some(true));
        assert_eq!(stamp_of(&c).stamped_at, NOW + 9);
    }

    /// Two devices' readings fold on the residency's own stamp, and a tie lands
    /// on metadata-only — the side on which the holder-keeps gate keeps a body.
    #[test]
    fn foreign_residency_folds_on_its_own_stamp_and_a_tie_keeps_metadata_only() {
        use fauna_core::data::ForeignResidency as R;
        let mo = |at| {
            Some(R {
                metadata_only: true,
                stamped_at: at,
            })
        };
        let full = |at| {
            Some(R {
                metadata_only: false,
                stamped_at: at,
            })
        };
        assert_eq!(R::join(mo(1), full(2)), full(2), "the later flip lands");
        assert_eq!(R::join(full(2), mo(1)), full(2), "commutative");
        assert_eq!(R::join(full(3), mo(3)), mo(3), "a tie keeps metadata-only");
        assert_eq!(R::join(mo(3), full(3)), mo(3), "commutative on a tie");
        assert_eq!(R::join(None, full(1)), full(1), "stated beats unknown");
        assert_eq!(R::join(mo(4), mo(4)), mo(4), "idempotent");
        // Through the plane merge: a device that refreshed only `access` later
        // carries no stale residency over another device's flip.
        let mut a = cfg();
        let mut b = cfg();
        let mut rec =
            foreign_with_access(0xAA, "https://home-a.example", Some("docs"), Some("reader"));
        rec.residency = full(1);
        assert!(record_foreign_set(&mut a, rec.clone(), 1));
        assert!(record_foreign_set(&mut b, rec, 1));
        assert!(refresh_foreign_set_from_reply(
            &mut b,
            &chan(0xAA),
            ForeignReplyStamps {
                residency: Some("metadata_only"),
                ..Default::default()
            },
            5
        ));
        assert!(refresh_foreign_set_from_reply(
            &mut a,
            &chan(0xAA),
            ForeignReplyStamps {
                access: Some("writer"),
                ..Default::default()
            },
            9
        ));
        let merged = a.merge(&b);
        let f = find_foreign_set(&merged, &chan(0xAA)).unwrap();
        assert_eq!(f.access.as_deref(), Some("writer"));
        assert_eq!(
            f.metadata_only_residency(),
            Some(true),
            "the flip survives the later access refresh"
        );
        assert_eq!(b.merge(&a), merged, "commutative");
    }

    fn member(b: u8) -> ActorId {
        ActorId([b; 32])
    }

    #[test]
    fn record_new_set_creates_genesis() {
        let mut c = cfg();
        let g = record_new_set(&mut c, chan(1), [0x11; 32], 1000);
        assert_eq!(g.version, 1);
        assert_eq!(g.key, [0x11; 32]);
        assert_eq!(g.rotated_at, 1000);
        assert_eq!(c.sets.len(), 1);
        assert!(c.sets[0].keys.as_ref().unwrap().prior.is_empty());
    }

    #[test]
    fn record_new_set_is_idempotent_and_does_not_clobber() {
        let mut c = cfg();
        record_new_set(&mut c, chan(1), [0x11; 32], 1000);
        // A second bind with a different key must NOT overwrite the live key.
        let g = record_new_set(&mut c, chan(1), [0x22; 32], 2000);
        assert_eq!(g.key, [0x11; 32], "existing content key preserved");
        assert_eq!(g.version, 1);
        assert_eq!(c.sets.len(), 1);
    }

    #[test]
    fn current_generation_and_content_keys_retrieve() {
        let mut c = cfg();
        assert!(current_generation(&c, &chan(1)).is_none());
        assert!(content_keys(&c, &chan(1)).is_none());
        record_new_set(&mut c, chan(1), [0x11; 32], 1000);
        assert_eq!(current_generation(&c, &chan(1)).unwrap().key, [0x11; 32]);
        assert_eq!(
            content_keys(&c, &chan(1)).unwrap().current_key(),
            &[0x11; 32]
        );
        assert!(current_generation(&c, &chan(2)).is_none());
    }

    #[test]
    fn forget_set_retires_keeps_the_keys_and_is_idempotent() {
        let mut c = cfg();
        record_new_set(&mut c, chan(1), [0x11; 32], 1000);
        record_new_set(&mut c, chan(2), [0x22; 32], 1000);
        assert!(forget_set(&mut c, &chan(1), 5_000));
        assert_eq!(c.sets.len(), 2, "never removed — a tombstone");
        let gone = c
            .sets
            .iter()
            .find(|s| s.channel_id == Some(chan(1)))
            .unwrap();
        assert_eq!(gone.retired_at, Some(5_000));
        assert_eq!(
            current_generation(&c, &chan(1)).unwrap().key,
            [0x11; 32],
            "the keys stay (no-data-loss)"
        );
        assert!(!forget_set(&mut c, &chan(1), 6_000), "idempotent");
    }

    #[test]
    fn a_member_entry_takes_the_envelopes_nonce_over_its_own() {
        let mut c = cfg();
        let received = FolderContentKeys::genesis([0x11; 32], 1_000);
        assert!(merge_received_keys(&mut c, chan(4), received));
        assert!(record_received_set_nonce(&mut c, &chan(4), [0xA1; 32], NOW));
        assert!(
            !record_received_set_nonce(&mut c, &chan(4), [0xA1; 32], NOW),
            "unchanged → no CAS"
        );
        // The owner re-minted: the member's copy is REPLACED, not joined.
        assert!(record_received_set_nonce(&mut c, &chan(4), [0xB2; 32], NOW));
        assert_eq!(set_nonce_for_channel(&c, &chan(4)), Some([0xB2; 32]));
        assert!(
            !record_received_set_nonce(&mut c, &chan(9), [0xC3; 32], NOW),
            "no entry for the channel → nothing to write"
        );
    }

    #[test]
    fn set_nonces_for_splits_live_from_retired_and_keeps_members_out_of_the_owners_name() {
        let mut c = cfg();
        record_created_set(&mut c, "docs", [0x01; 32], None, 10);
        retire_set(&mut c, "docs", 20);
        record_created_set(&mut c, "docs", [0x02; 32], None, 30);
        record_created_set(&mut c, "docs", [0x03; 32], None, 40); // a race loser
        assert_eq!(
            set_nonces_for(&c, Some("docs"), None),
            (Some([0x02; 32]), vec![[0x03; 32]]),
            "the deleted earlier incarnation (retired before the live entry was \
             created) is not this set's — its rows are never re-signed"
        );
        // The loser retired by the pick stays: the pick's edge places it in
        // this incarnation's lineage (ruling (11)(b)).
        for s in c
            .sets
            .iter_mut()
            .filter(|s| s.set_nonce == Some([0x03; 32]))
        {
            s.retire(50);
            s.retired_by_pick = Some([0x02; 32]);
        }
        assert_eq!(
            set_nonces_for(&c, Some("docs"), None),
            (Some([0x02; 32]), vec![[0x03; 32]])
        );
        // A create the nest refused (its rollback retires with no edge) holds
        // no rows of this set, and leaves the lineage.
        record_created_set(&mut c, "docs", [0x04; 32], None, 60);
        retire_set_nonce(&mut c, &[0x04; 32], 61);
        assert_eq!(
            set_nonces_for(&c, Some("docs"), None),
            (Some([0x02; 32]), vec![[0x03; 32]])
        );
        // A same-named set the caller merely belongs to: by channel only.
        merge_received_keys(&mut c, chan(5), FolderContentKeys::genesis([5; 32], 1));
        record_received_set_nonce(&mut c, &chan(5), [0x55; 32], NOW);
        assert_eq!(
            set_nonces_for(&c, None, Some(&chan(5))),
            (Some([0x55; 32]), Vec::new())
        );
    }

    // ── Ruling (11): the succession cut over custody ───────────────────────

    const PRED: ActorId = ActorId([0xA0; 32]);
    const SUCC: ActorId = ActorId([0xA1; 32]);
    const SUCC2: ActorId = ActorId([0xA2; 32]);

    /// Re-mint `name`'s live entry under `identity` with the fresh nonce `to`.
    fn remint(c: &mut FoldersConfig, identity: ActorId, to: [u8; 32], at: u64) -> Vec<String> {
        let marked = uncut_owned_nonces(c, &identity);
        remint_owned_entries(c, &identity, &marked, at, || to)
    }

    /// The cut retires the predecessor-minted entry for good and adds one the
    /// current identity minted, replacing it — and a second pass cuts nothing.
    #[test]
    fn the_cut_remints_a_predecessor_minted_entry_once() {
        let mut c = cfg();
        record_created_set(&mut c, "docs", [0x01; 32], Some(PRED), 10);
        record_created_set(&mut c, "mine", [0x09; 32], Some(SUCC), 10);
        assert_eq!(uncut_owned_nonces(&c, &SUCC), vec![[0x01; 32]]);
        assert_eq!(
            remint(&mut c, SUCC, [0x02; 32], 20),
            vec!["docs".to_string()]
        );
        let live = live_set_by_name(&c, "docs").expect("live");
        assert_eq!(live.set_nonce, Some([0x02; 32]));
        assert_eq!(live.minted_by, Some(SUCC));
        assert_eq!(live.replaces, Some([0x01; 32]));
        let old = c
            .sets
            .iter()
            .find(|s| s.set_nonce == Some([0x01; 32]))
            .unwrap();
        assert!(!old.is_live() && old.lifted_at.is_none());
        assert!(uncut_owned_nonces(&c, &SUCC).is_empty(), "idempotent");
        // A member's received copy carries no name and is never cut.
        merge_received_keys(&mut c, chan(5), FolderContentKeys::genesis([5; 32], 1));
        record_received_set_nonce(&mut c, &chan(5), [0x55; 32], NOW);
        assert!(uncut_owned_nonces(&c, &SUCC).is_empty());
    }

    /// A succession owes each inherited bound set one rotation, after its cut:
    /// not before the cut, not twice, again at a second succession — and never
    /// for a set no predecessor minted, an unbound one, or a member's copy.
    #[test]
    fn a_bound_set_owes_one_rotation_per_succession_cut() {
        let mut c = cfg();
        record_created_set(&mut c, "docs", [0x01; 32], Some(PRED), 10);
        key_named_set(&mut c, "docs", chan(1), [7; 32], 10);
        record_created_set(&mut c, "mine", [0x09; 32], Some(SUCC), 10);
        key_named_set(&mut c, "mine", chan(2), [8; 32], 10);
        record_created_set(&mut c, "unbound", [0x0A; 32], Some(PRED), 10);
        merge_received_keys(&mut c, chan(5), FolderContentKeys::genesis([5; 32], 1));
        assert!(
            sets_owing_succession_rotation(&c, &SUCC).is_empty(),
            "the cut comes first"
        );

        remint(&mut c, SUCC, [0x02; 32], 20);
        let owed = sets_owing_succession_rotation(&c, &SUCC);
        assert_eq!(
            owed,
            vec![SuccessionRotationOwed {
                name: "docs".into(),
                channel_id: chan(1),
                predecessors: vec![PRED],
                cut_at: 20,
            }]
        );
        rotate_set(&mut c, &chan(1), [0x22; 32], 20);
        assert!(sets_owing_succession_rotation(&c, &SUCC).is_empty(), "once");

        // A nonce per set (the shared `remint` helper hands every set one).
        let marked = uncut_owned_nonces(&c, &SUCC2);
        let mut next = 0x30u8;
        remint_owned_entries(&mut c, &SUCC2, &marked, 30, || {
            next += 1;
            [next; 32]
        });
        let again = sets_owing_succession_rotation(&c, &SUCC2);
        assert_eq!(
            again
                .iter()
                .map(|o| (o.name.as_str(), o.predecessors.clone(), o.cut_at))
                .collect::<Vec<_>>(),
            vec![("docs", vec![PRED, SUCC], 30), ("mine", vec![SUCC], 30)],
            "a second succession owes every set the first successor held"
        );
    }

    /// The lineage is the `replaces` component around the live nonce: a
    /// second succession keeps the first cut's nonce, a deleted earlier
    /// incarnation of the name stays out, and each nonce carries its minter.
    #[test]
    fn the_lineage_keeps_every_cut_and_no_deleted_incarnation() {
        let mut c = cfg();
        record_created_set(&mut c, "docs", [0x0E; 32], Some(PRED), 1);
        retire_set(&mut c, "docs", 2);
        record_created_set(&mut c, "docs", [0x01; 32], Some(PRED), 10);
        remint(&mut c, SUCC, [0x02; 32], 20);
        remint(&mut c, SUCC2, [0x03; 32], 30);
        let lineage = set_lineage(&c, Some("docs"), None);
        assert_eq!(lineage.live, Some([0x03; 32]));
        assert_eq!(lineage.live_minted_by, Some(SUCC2));
        let retired: Vec<_> = lineage
            .retired
            .iter()
            .map(|r| (r.nonce, r.minted_by))
            .collect();
        assert_eq!(
            retired,
            vec![([0x02; 32], Some(SUCC)), ([0x01; 32], Some(PRED))],
            "newest first, the deleted incarnation [0x0E] out"
        );
    }

    /// A re-mint race: two devices re-mint one entry; the pick keeps one, and
    /// the loser — and an entry re-minted over the loser — stay siblings in
    /// the lineage, so a member holding either is never locked out.
    #[test]
    fn a_remint_races_loser_and_its_successor_stay_in_the_lineage() {
        let mut c = cfg();
        record_created_set(&mut c, "docs", [0x01; 32], Some(PRED), 10);
        let mut other = c.clone();
        remint(&mut c, SUCC, [0x02; 32], 20);
        remint(&mut other, SUCC, [0x03; 32], 21);
        let mut c = c.merge(&other);
        // The pick (earliest) keeps [0x02]; the reconcile retires [0x03] with
        // its edge.
        for s in c
            .sets
            .iter_mut()
            .filter(|s| s.set_nonce == Some([0x03; 32]))
        {
            s.retire(30);
            s.retired_by_pick = Some([0x02; 32]);
        }
        let lineage = set_lineage(&c, Some("docs"), None);
        assert_eq!(lineage.live, Some([0x02; 32]));
        let mut retired: Vec<_> = lineage.retired.iter().map(|r| r.nonce).collect();
        retired.sort();
        assert_eq!(retired, vec![[0x01; 32], [0x03; 32]]);
    }

    fn envelope(
        live: [u8; 32],
        minted_by: Option<ActorId>,
        retired: &[[u8; 32]],
    ) -> fauna_core::folder_keys::ContentKeyEnvelopePayload {
        fauna_core::folder_keys::ContentKeyEnvelopePayload {
            keys: FolderContentKeys::genesis([5; 32], 1),
            set_nonce: Some(live),
            minted_by,
            retired_set_nonces: retired
                .iter()
                .map(|n| fauna_core::folder_keys::RetiredSetNonce {
                    nonce: *n,
                    minted_by: Some(PRED),
                })
                .collect(),
            served_at: None,
            unserved_at: None,
        }
    }

    /// Ruling (7)(b)(ii) rule (1): the stamp writers land on the channel's
    /// live entry, each strictly above the other, and a channel custody does
    /// not hold is not stamped.
    #[test]
    fn serve_flips_stamp_the_channels_live_entry() {
        let mut c = cfg();
        let ch = chan(7);
        assert!(!serve_on(&mut c, &ch, 100), "no entry, no stamp");
        record_new_set(&mut c, ch, [1; 32], 10);
        assert!(
            !channel_served(&c, &ch),
            "keyed and stamp-less is not served"
        );
        assert!(serve_on(&mut c, &ch, 100));
        assert!(channel_served(&c, &ch));
        assert!(serve_off(&mut c, &ch, 50), "a clock stepped back");
        assert_eq!(serve_stamps(&c, &ch), (Some(100), Some(101)));
        assert!(!channel_served(&c, &ch));
        // A retired entry carries no window.
        assert!(serve_on(&mut c, &ch, 200));
        forget_set(&mut c, &ch, 300);
        assert!(!channel_served(&c, &ch));
        assert!(!serve_on(&mut c, &ch, 400), "a tombstone is never stamped");
    }

    /// A re-mint retires the entry and adds one under a fresh nonce: the
    /// serve window moves with the set.
    #[test]
    fn a_remint_keeps_the_serve_window() {
        let mut c = cfg();
        record_created_set(&mut c, "docs", [0x01; 32], Some(PRED), 10);
        let pseudo = fauna_core::folder_keys::serve_custody_channel_id("docs");
        key_named_set(&mut c, "docs", pseudo, [9; 32], 20);
        assert!(serve_on(&mut c, &pseudo, 30));
        let names = remint_owned_entries(&mut c, &SUCC, &[[0x01; 32]], 40, || [0x02; 32]);
        assert_eq!(names, vec!["docs".to_string()]);
        assert!(channel_served(&c, &pseudo), "the re-minted entry is served");
        assert_eq!(live_set_nonce(&c, "docs"), Some([0x02; 32]));
    }

    /// Ruling (7)(b)(ii) rule (4): a member's ingest joins the owner's stamps
    /// to the later — a replayed envelope cannot move the window — and a
    /// non-owner's envelope cannot move it at all.
    #[test]
    fn a_member_joins_the_owners_serve_stamps_and_a_replay_moves_nothing() {
        let mut c = cfg();
        let ch = chan(6);
        let served = fauna_core::folder_keys::ContentKeyEnvelopePayload {
            served_at: Some(100),
            ..envelope([0x01; 32], Some(PRED), &[])
        };
        assert_eq!(
            ingest_received_envelope(&mut c, ch, served.clone(), true, NOW),
            Ok(true)
        );
        assert!(channel_served(&c, &ch));
        let unserved = fauna_core::folder_keys::ContentKeyEnvelopePayload {
            unserved_at: Some(200),
            ..served.clone()
        };
        assert_eq!(
            ingest_received_envelope(&mut c, ch, unserved.clone(), false, NOW),
            Err(EnvelopeRefusal::ConfirmOnly),
            "only the owner moves the window"
        );
        assert_eq!(
            ingest_received_envelope(&mut c, ch, unserved, true, NOW),
            Ok(true)
        );
        assert!(!channel_served(&c, &ch));
        // The nest replays the served envelope: nothing moves.
        assert_eq!(
            ingest_received_envelope(&mut c, ch, served, true, NOW),
            Ok(false)
        );
        assert!(!channel_served(&c, &ch));
        assert_eq!(serve_stamps(&c, &ch), (Some(100), Some(200)));
    }

    /// The window survives the owner's re-mint on the member's side too: the
    /// replaced nonce's new entry carries what the old one held.
    #[test]
    fn a_members_window_survives_a_received_remint() {
        let mut c = cfg();
        let ch = chan(8);
        let served = fauna_core::folder_keys::ContentKeyEnvelopePayload {
            served_at: Some(100),
            ..envelope([0x01; 32], Some(PRED), &[])
        };
        ingest_received_envelope(&mut c, ch, served, true, NOW).unwrap();
        // A re-minted envelope from an owner device that carries no stamps.
        ingest_received_envelope(
            &mut c,
            ch,
            envelope([0x02; 32], Some(SUCC), &[[0x01; 32]]),
            true,
            NOW,
        )
        .unwrap();
        assert!(channel_served(&c, &ch));
    }

    /// Ruling (7)(b)(ii) rule (3): the envelope reconcile publishes the JOIN
    /// and only when custody is strictly ahead — a lagging device's custody
    /// behind the envelope publishes nothing.
    #[test]
    fn the_envelope_join_is_ahead_only_when_custody_is() {
        let base = envelope([0x02; 32], Some(SUCC), &[[0x01; 32]]);
        assert_eq!(envelope_join(&base, &base), Some((base.clone(), false)));

        // Custody carries a later serve stamp: ahead, and the join carries it.
        let custody_served = fauna_core::folder_keys::ContentKeyEnvelopePayload {
            served_at: Some(100),
            ..base.clone()
        };
        assert_eq!(
            envelope_join(&base, &custody_served),
            Some((custody_served.clone(), true))
        );
        // The envelope carries the later stamp: a lagging device is not
        // ahead, and the join is what the envelope already says.
        assert_eq!(
            envelope_join(&custody_served, &base),
            Some((custody_served.clone(), false))
        );

        // Each ahead in a different part: the join of both.
        let mut rotated = FolderContentKeys::genesis([5; 32], 1);
        rotated.rotate([6; 32], 2);
        let custody_rotated = fauna_core::folder_keys::ContentKeyEnvelopePayload {
            keys: rotated.clone(),
            ..base.clone()
        };
        let (joined, ahead) = envelope_join(&custody_served, &custody_rotated).unwrap();
        assert!(ahead, "custody holds a generation the envelope lacks");
        assert_eq!(joined.served_at, Some(100), "the envelope's stamp is kept");
        assert_eq!(joined.keys.current_version(), 2);
        // A lagging device missing the rotation is not ahead.
        assert!(!envelope_join(&custody_rotated, &base).unwrap().1);

        // The envelope names custody's live nonce as retired: behind a cut.
        let cut = envelope([0x03; 32], Some(SUCC), &[[0x02; 32], [0x01; 32]]);
        assert_eq!(envelope_join(&cut, &base), None);
        // And custody past the envelope's nonce is ahead, under its own nonce.
        let (joined, ahead) = envelope_join(&base, &cut).unwrap();
        assert!(ahead);
        assert_eq!(joined.set_nonce, Some([0x03; 32]));
    }

    /// A member ingests the owner's envelope with its lineage, and its
    /// lineage then answers the retired list (no longer empty by
    /// construction).
    #[test]
    fn a_member_merges_the_lineage_and_answers_it() {
        let mut c = cfg();
        let ch = chan(5);
        assert_eq!(
            ingest_received_envelope(&mut c, ch, envelope([0x01; 32], Some(PRED), &[]), true, NOW),
            Ok(true)
        );
        assert_eq!(
            ingest_received_envelope(
                &mut c,
                ch,
                envelope([0x02; 32], Some(SUCC), &[[0x01; 32]]),
                true,
                NOW
            ),
            Ok(true)
        );
        let lineage = set_lineage(&c, None, Some(&ch));
        assert_eq!(lineage.live, Some([0x02; 32]));
        assert_eq!(lineage.live_minted_by, Some(SUCC));
        assert_eq!(
            lineage.retired.iter().map(|r| r.nonce).collect::<Vec<_>>(),
            vec![[0x01; 32]]
        );
    }

    /// Forward only: a stale envelope naming a retired nonce as live is
    /// refused, a lineage that does not cover the held nonces is refused, and
    /// a refusal changes nothing — keys and nonce together.
    #[test]
    fn a_member_refuses_a_stale_or_uncovering_envelope_whole() {
        let mut c = cfg();
        let ch = chan(5);
        ingest_received_envelope(&mut c, ch, envelope([0x01; 32], None, &[]), true, NOW).unwrap();
        ingest_received_envelope(
            &mut c,
            ch,
            envelope([0x02; 32], Some(SUCC), &[[0x01; 32]]),
            true,
            NOW,
        )
        .unwrap();
        let before = c.clone();
        let mut stale = envelope([0x01; 32], None, &[]);
        stale.keys = FolderContentKeys::genesis([9; 32], 1).merge(&stale.keys);
        assert_eq!(
            ingest_received_envelope(&mut c, ch, stale, true, NOW),
            Err(EnvelopeRefusal::Backwards)
        );
        assert_eq!(
            ingest_received_envelope(&mut c, ch, envelope([0x07; 32], Some(SUCC), &[]), true, NOW),
            Err(EnvelopeRefusal::LineageNotCovered)
        );
        assert_eq!(c, before, "a refusal discards keys and nonce together");
    }

    /// A member that ingested a re-mint race's loser accepts the winner's
    /// envelope — the loser is a sibling in its lineage.
    #[test]
    fn a_member_on_a_race_loser_accepts_the_winners_envelope() {
        let mut c = cfg();
        let ch = chan(5);
        ingest_received_envelope(
            &mut c,
            ch,
            envelope([0x03; 32], Some(SUCC), &[[0x01; 32]]),
            true,
            NOW,
        )
        .unwrap();
        assert_eq!(
            ingest_received_envelope(
                &mut c,
                ch,
                envelope([0x02; 32], Some(SUCC), &[[0x03; 32], [0x01; 32]]),
                true,
                NOW
            ),
            Ok(true)
        );
        assert_eq!(set_nonce_for_channel(&c, &ch), Some([0x02; 32]));
    }

    /// A non-owner's envelope (a proven predecessor's, say) may confirm what
    /// the member holds and move nothing.
    #[test]
    fn a_non_owners_envelope_only_confirms() {
        let mut c = cfg();
        let ch = chan(5);
        let held = envelope([0x01; 32], Some(PRED), &[]);
        ingest_received_envelope(&mut c, ch, held.clone(), true, NOW).unwrap();
        assert_eq!(
            ingest_received_envelope(&mut c, ch, held, false, NOW),
            Ok(false)
        );
        assert_eq!(
            ingest_received_envelope(
                &mut c,
                ch,
                envelope([0x02; 32], Some(PRED), &[[0x01; 32]]),
                false,
                NOW
            ),
            Err(EnvelopeRefusal::ConfirmOnly)
        );
        assert_eq!(set_nonce_for_channel(&c, &ch), Some([0x01; 32]));
    }

    #[test]
    fn live_set_by_name_picks_earliest_then_smaller_nonce_and_skips_retired() {
        let mut c = cfg();
        assert!(record_created_set(&mut c, "docs", [0x30; 32], None, 300));
        assert!(record_created_set(&mut c, "docs", [0x20; 32], None, 200));
        assert!(record_created_set(&mut c, "docs", [0x10; 32], None, 200));
        assert!(record_created_set(&mut c, "other", [0x05; 32], None, 1));
        assert!(
            !record_created_set(&mut c, "docs", [0x10; 32], None, 999),
            "idempotent on the nonce"
        );
        assert_eq!(
            live_set_nonce(&c, "docs"),
            Some([0x10; 32]),
            "tie → smaller"
        );
        assert!(retire_set_nonce(&mut c, &[0x10; 32], 400));
        assert_eq!(live_set_nonce(&c, "docs"), Some([0x20; 32]));
        assert!(retire_set(&mut c, "docs", 500));
        assert_eq!(live_set_nonce(&c, "docs"), None, "all retired");
        assert!(!retire_set(&mut c, "docs", 600), "idempotent");
        assert_eq!(live_set_nonce(&c, "other"), Some([0x05; 32]));
        assert_eq!(c.sets.len(), 4, "tombstones are never pruned");
    }

    #[test]
    fn key_named_set_keys_the_create_time_entry_in_place() {
        let mut c = cfg();
        record_created_set(&mut c, "docs", [0x10; 32], None, 100);
        let served = fauna_core::folder_keys::serve_custody_channel_id("docs");
        let g = key_named_set(&mut c, "docs", served, [0x11; 32], 1_000);
        assert_eq!(g.version, 1);
        assert_eq!(c.sets.len(), 1, "no second entry");
        let e = &c.sets[0];
        assert_eq!(e.set_nonce, Some([0x10; 32]));
        assert_eq!(e.channel_id, Some(served));
        // Idempotent: a second serve-enable keeps the live key.
        let again = key_named_set(&mut c, "docs", served, [0x99; 32], 2_000);
        assert_eq!(again.key, [0x11; 32]);
        // Share-after-serve: the bind re-keys the same entry in place.
        assert!(migrate_set_identity(&mut c, &served, chan(7), NOW));
        key_named_set(&mut c, "docs", chan(7), [0x77; 32], 3_000);
        assert_eq!(c.sets.len(), 1);
        assert_eq!(c.sets[0].channel_id, Some(chan(7)));
        assert_eq!(c.sets[0].set_nonce, Some([0x10; 32]));
        assert_eq!(current_generation(&c, &chan(7)).unwrap().key, [0x11; 32]);
    }

    #[test]
    fn a_recreated_served_set_is_not_shadowed_by_its_tombstone() {
        let mut c = cfg();
        let served = fauna_core::folder_keys::serve_custody_channel_id("docs");
        record_created_set(&mut c, "docs", [0x10; 32], None, 100);
        key_named_set(&mut c, "docs", served, [0x11; 32], 200);
        retire_set(&mut c, "docs", 300);
        record_created_set(&mut c, "docs", [0x20; 32], None, 400);
        key_named_set(&mut c, "docs", served, [0x22; 32], 500);
        assert_eq!(c.sets.len(), 2);
        assert_eq!(
            current_generation(&c, &served).unwrap().key,
            [0x22; 32],
            "the live set's fresh genesis, never the deleted set's key"
        );
        assert_eq!(live_set_nonce(&c, "docs"), Some([0x20; 32]));
    }

    #[test]
    fn key_named_set_without_a_create_entry_records_a_nonce_less_named_entry() {
        let mut c = cfg();
        key_named_set(&mut c, "named", chan(3), [0x33; 32], 1_000);
        let e = &c.sets[0];
        assert_eq!(e.name.as_deref(), Some("named"));
        assert_eq!(e.set_nonce, None, "repaired by the owner reconcile");
    }

    fn staged(ch: u8, m: u8, version: u64, key: u8, at: u64) -> FolderPendingRemoval {
        FolderPendingRemoval {
            channel_id: chan(ch),
            name: format!("set-{ch}"),
            removed_member: member(m),
            new_generation: ContentKeyGeneration {
                version,
                key: [key; 32].into(),
                rotated_at: at,
            },
            commit: None,
            gated_attempted: false,
        }
    }

    /// The stamp is one-way, idempotent, and reports whether it changed —
    /// `false` on an already-stamped sentinel (skip the redundant persist) and
    /// on a missing one (nothing to protect).
    #[test]
    fn mark_removal_gated_attempted_is_one_way_and_idempotent() {
        let mut c = cfg();
        assert!(!mark_removal_gated_attempted(&mut c, &chan(1), &member(2)));

        stage_pending_removal(&mut c, staged(1, 2, 2, 0xAA, 2000));
        assert!(
            !find_pending_removal(&c, &chan(1), &member(2))
                .unwrap()
                .gated_attempted,
            "staged unstamped"
        );
        assert!(mark_removal_gated_attempted(&mut c, &chan(1), &member(2)));
        assert!(
            find_pending_removal(&c, &chan(1), &member(2))
                .unwrap()
                .gated_attempted
        );
        assert!(
            !mark_removal_gated_attempted(&mut c, &chan(1), &member(2)),
            "second stamp is a no-op"
        );
    }

    #[test]
    fn stage_find_clear_pending_removal_lifecycle() {
        let mut c = cfg();
        assert!(find_pending_removal(&c, &chan(1), &member(2)).is_none());

        stage_pending_removal(&mut c, staged(1, 2, 2, 0xAA, 2000));
        let found = find_pending_removal(&c, &chan(1), &member(2)).expect("staged");
        assert_eq!(found.new_generation.key, [0xAA; 32]);

        // Re-stage with a bumped rotated_at (same fresh key) updates in place.
        stage_pending_removal(&mut c, staged(1, 2, 2, 0xAA, 9999));
        assert_eq!(c.pending_removals.len(), 1, "upsert, not append");
        assert_eq!(
            find_pending_removal(&c, &chan(1), &member(2))
                .unwrap()
                .new_generation
                .rotated_at,
            9999
        );

        // Clear is keyed on the fresh key; a wrong key is a no-op.
        assert!(!clear_pending_removal(
            &mut c,
            &chan(1),
            &member(2),
            &[0xBB; 32]
        ));
        assert!(clear_pending_removal(
            &mut c,
            &chan(1),
            &member(2),
            &[0xAA; 32]
        ));
        assert!(find_pending_removal(&c, &chan(1), &member(2)).is_none());
    }

    #[test]
    fn has_other_pending_removal_with_commit_sees_only_byted_channel_siblings() {
        let mut c = cfg();
        stage_pending_removal(&mut c, staged(1, 2, 2, 0xAA, 2000)); // byte-less
        stage_pending_removal(&mut c, staged(1, 3, 2, 0xBB, 2001)); // sibling, byte-less
        // No byted sibling yet — from either member's perspective.
        assert!(!has_other_pending_removal_with_commit(
            &c,
            &chan(1),
            &member(2)
        ));

        // Byte the member(3) sibling: member(2) now sees a byted sibling;
        // member(3) itself does not (own bytes are not a "sibling").
        assert!(stage_removal_commit(
            &mut c,
            &chan(1),
            &member(3),
            vec![0xC1]
        ));
        assert!(has_other_pending_removal_with_commit(
            &c,
            &chan(1),
            &member(2)
        ));
        assert!(!has_other_pending_removal_with_commit(
            &c,
            &chan(1),
            &member(3)
        ));
        // A different channel is unaffected.
        assert!(!has_other_pending_removal_with_commit(
            &c,
            &chan(9),
            &member(2)
        ));
    }

    #[test]
    fn stage_does_not_clobber_concurrent_differently_keyed_staging() {
        // A merge may union two devices' stagings for the SAME (channel, member)
        // with DIFFERENT fresh keys — both irrecoverable. Clearing one must leave
        // the other intact.
        let mut c = cfg();
        stage_pending_removal(&mut c, staged(1, 2, 2, 0xAA, 2000));
        stage_pending_removal(&mut c, staged(1, 2, 2, 0xBB, 2050));
        assert_eq!(c.pending_removals.len(), 2);
        assert!(clear_pending_removal(
            &mut c,
            &chan(1),
            &member(2),
            &[0xAA; 32]
        ));
        assert_eq!(c.pending_removals.len(), 1);
        assert_eq!(
            c.pending_removals[0].new_generation.key, [0xBB; 32],
            "the other device's key survives"
        );
    }

    #[test]
    fn commit_generation_makes_it_current_and_is_idempotent() {
        let mut c = cfg();
        record_new_set(&mut c, chan(1), [0x11; 32], 1000);
        let v2 = ContentKeyGeneration {
            version: 2,
            key: [0x22; 32].into(),
            rotated_at: 2000,
        };
        commit_generation(&mut c, &chan(1), &v2).expect("commits");
        assert_eq!(current_generation(&c, &chan(1)).unwrap().key, [0x22; 32]);
        // v1 retained (history-on-join back-catalogue) in prior.
        assert_eq!(
            content_keys(&c, &chan(1)).unwrap().key_for(1),
            Some(&[0x11; 32])
        );

        // A crash-resumed commit replays the SAME generation: must NOT double-rotate.
        commit_generation(&mut c, &chan(1), &v2).expect("re-commits");
        assert_eq!(current_generation(&c, &chan(1)).unwrap().version, 2);
        assert_eq!(
            c.sets[0].keys.as_ref().unwrap().prior.len(),
            1,
            "idempotent — no extra prior entry"
        );
    }

    #[test]
    fn rotate_set_appends_a_fresh_current_and_retains_history() {
        let mut c = cfg();
        record_new_set(&mut c, chan(1), [0x11; 32], 1000);
        let g = rotate_set(&mut c, &chan(1), [0x22; 32], 2000).expect("rotates");
        assert_eq!(g.version, 2);
        assert_eq!(g.key, [0x22; 32]);
        assert_eq!(current_generation(&c, &chan(1)).unwrap().key, [0x22; 32]);
        // Generation 1 retained (already-stamped snapshots stay readable).
        assert_eq!(
            content_keys(&c, &chan(1)).unwrap().key_for(1),
            Some(&[0x11; 32])
        );
        // Strictly-increasing rotated_at even against a stale clock.
        let g3 = rotate_set(&mut c, &chan(1), [0x33; 32], 0).expect("rotates");
        assert!(g3.rotated_at > g.rotated_at);
        assert_eq!(g3.version, 3);
    }

    /// Two of the owner's devices each rotate from the same custody before
    /// either's write reaches the other (the unattended succession rotation, or
    /// two serve-disables): both mint version 2. The merged custody keeps both,
    /// the envelope either device then publishes carries both, and a member's
    /// ingest of it lands the owner's exact history
    /// (`mls-group-key-material.md` § M2 → *Generations*, same-version
    /// candidates).
    #[test]
    fn two_devices_rotating_at_once_settle_and_the_member_ingests_the_envelope() {
        let mut base = cfg();
        record_new_set(&mut base, chan(1), [0x11; 32], 1000);
        let mut member_cfg = cfg();
        merge_received_keys(
            &mut member_cfg,
            chan(1),
            content_keys(&base, &chan(1)).unwrap(),
        );

        let (mut a, mut b) = (base.clone(), base);
        let ga = rotate_set(&mut a, &chan(1), [0xAA; 32], 2000).expect("rotates");
        let gb = rotate_set(&mut b, &chan(1), [0xBB; 32], 2001).expect("rotates");
        assert_eq!((ga.version, gb.version), (2, 2), "the collision");

        let owner = a.merge(&b);
        assert_eq!(owner, b.merge(&a), "both devices converge");
        let owner_keys = content_keys(&owner, &chan(1)).unwrap();
        assert_eq!(owner_keys.current, gb, "the later rotation is current");
        // Settled: neither device's next pass mints again for the same trigger.
        assert!(owner_keys.current.rotated_at > 1000);

        let sealed = envelope_payload(&owner, None, &chan(1), owner_keys.clone())
            .encode()
            .unwrap();
        let received = fauna_core::folder_keys::ContentKeyEnvelopePayload::decode(&sealed)
            .expect("the member decodes a same-version pair");
        assert_eq!(
            ingest_received_envelope(&mut member_cfg, chan(1), received, true, NOW),
            Ok(true)
        );
        let held = content_keys(&member_cfg, &chan(1)).unwrap();
        assert_eq!(held, owner_keys, "the member holds the owner's history");
        let v2: Vec<[u8; 32]> = held.keys_for(2).copied().collect();
        assert_eq!(v2, vec![[0xBB; 32], [0xAA; 32]], "current first");
    }

    #[test]
    fn rotate_set_unknown_set_is_none() {
        let mut c = cfg();
        assert!(rotate_set(&mut c, &chan(9), [0x22; 32], 2000).is_none());
        assert!(c.sets.is_empty(), "no entry conjured");
    }

    #[test]
    fn migrate_set_identity_moves_keys() {
        // Share-after-serve: custody staged at the serve pseudo-channel moves
        // to the real derived ChannelId at bind.
        let mut c = cfg();
        record_new_set(&mut c, chan(1), [0x11; 32], 1000);
        rotate_set(&mut c, &chan(1), [0x22; 32], 2000);

        assert!(migrate_set_identity(&mut c, &chan(1), chan(7), NOW));
        assert!(
            !c.sets
                .iter()
                .any(|s| s.is_live() && s.channel_id == Some(chan(1))),
            "no live entry is left on the old identity"
        );
        let keys = content_keys(&c, &chan(7)).expect("moved");
        assert_eq!(keys.current_key(), &[0x22; 32]);
        assert_eq!(keys.key_for(1), Some(&[0x11; 32]), "history preserved");
        // Idempotent: re-running (resumed bind) is a no-op.
        assert!(!migrate_set_identity(&mut c, &chan(1), chan(7), NOW));
        // A nonce-less entry's identity is its channel, so the old one stays
        // as a retired twin (ruling (l)(ii)) — never removed.
        assert_eq!(c.sets.len(), 2);
    }

    /// A nonce-bearing entry — every set created since the set-nonce ruling —
    /// moves in place: one entry, its nonce kept, nothing left on the
    /// pseudo-channel (ruling (l)(ii)).
    #[test]
    fn migrate_set_identity_moves_a_nonce_bearing_entry_in_place() {
        let mut c = cfg();
        let pseudo = fauna_core::folder_keys::serve_custody_channel_id("docs");
        record_created_set(&mut c, "docs", [0x0A; 32], None, 900);
        key_named_set(&mut c, "docs", pseudo, [0x11; 32], 1000);
        assert!(migrate_set_identity(&mut c, &pseudo, chan(7), NOW));
        assert_eq!(c.sets.len(), 1, "one entry, never a second");
        assert_eq!(c.sets[0].set_nonce, Some([0x0A; 32]));
        assert!(
            content_keys(&c, &pseudo).is_none(),
            "the pseudo identity is gone"
        );
        assert_eq!(
            content_keys(&c, &chan(7)).unwrap().current_key(),
            &[0x11; 32]
        );
        // The move is an advance under the channel join (bound dominates
        // served), so a stale replica still on the pseudo-channel converges
        // on the real one.
        let mut stale = cfg();
        record_created_set(&mut stale, "docs", [0x0A; 32], None, 900);
        key_named_set(&mut stale, "docs", pseudo, [0x11; 32], 1000);
        assert_eq!(stale.merge(&c).sets[0].channel_id, Some(chan(7)));
    }

    /// Two entries holding one channel — the twin an identity move leaves on
    /// a store that never removes one — read as ONE set: the keys join, the
    /// nonce is the live pick, and a leave retires both (ruling (l)(i)).
    #[test]
    fn a_channel_resolves_over_every_entry_holding_it() {
        let mut c = cfg();
        c.sets.push(FolderKeyCustody {
            channel_id: Some(chan(5)),
            keys: Some(FolderContentKeys::genesis([0x11; 32], 1000)),
            ..Default::default()
        });
        c.sets.push(FolderKeyCustody {
            channel_id: Some(chan(5)),
            keys: Some(FolderContentKeys {
                current: ContentKeyGeneration {
                    version: 2,
                    key: [0x22; 32].into(),
                    rotated_at: 2000,
                },
                prior: vec![],
            }),
            set_nonce: Some([0x0B; 32]),
            ..Default::default()
        });
        let keys = content_keys(&c, &chan(5)).unwrap();
        assert_eq!(keys.current_key(), &[0x22; 32]);
        assert_eq!(
            keys.key_for(1),
            Some(&[0x11; 32]),
            "the twin's generation joins"
        );
        assert_eq!(set_nonce_for_channel(&c, &chan(5)), Some([0x0B; 32]));
        let rotated = rotate_set(&mut c, &chan(5), [0x33; 32], 3000).unwrap();
        assert_eq!(
            rotated.version, 3,
            "a rotation counts from the joined current"
        );
        assert!(forget_set(&mut c, &chan(5), NOW));
        assert!(
            c.sets.iter().all(|s| !s.is_live()),
            "the leave retires both"
        );
    }

    /// A member's nonce replaced by the owner's re-mint (ruling (h)) retires
    /// the old entry and adds one under the new, carrying the keys — never an
    /// overwrite of an entry's identity (ruling (l)(ii)).
    #[test]
    fn a_replaced_received_nonce_retires_the_old_entry() {
        let mut c = cfg();
        assert!(merge_received_keys(
            &mut c,
            chan(3),
            FolderContentKeys::genesis([0x11; 32], 1000)
        ));
        assert!(record_received_set_nonce(&mut c, &chan(3), [0x01; 32], NOW));
        assert_eq!(c.sets.len(), 1, "a first nonce is written onto the entry");
        assert!(!record_received_set_nonce(
            &mut c,
            &chan(3),
            [0x01; 32],
            NOW
        ));
        assert!(record_received_set_nonce(&mut c, &chan(3), [0x02; 32], NOW));
        assert_eq!(c.sets.len(), 2);
        assert_eq!(set_nonce_for_channel(&c, &chan(3)), Some([0x02; 32]));
        let old = c
            .sets
            .iter()
            .find(|s| s.set_nonce == Some([0x01; 32]))
            .unwrap();
        assert!(!old.is_live(), "the old nonce is retired, never dropped");
        assert_eq!(
            content_keys(&c, &chan(3)).unwrap().current_key(),
            &[0x11; 32]
        );
        assert!(!record_received_set_nonce(
            &mut c,
            &chan(3),
            [0x02; 32],
            NOW
        ));
    }

    /// A foreign leave tombstones the record, readers stop seeing it, and a
    /// later accept revives it (ruling (l)(iii)); a changed advisory field
    /// stamps `updated_at` past the held stamp, an unchanged refresh does not
    /// (ruling (l)(iv)).
    #[test]
    fn a_foreign_leave_tombstones_and_refreshes_stamp_the_record() {
        let mut c = cfg();
        assert!(record_foreign_set(
            &mut c,
            foreign(0xAA, "https://home.example", Some("photos")),
            100
        ));
        assert_eq!(
            (c.foreign_sets[0].accepted_at, c.foreign_sets[0].updated_at),
            (100, 100)
        );
        assert!(refresh_foreign_set_from_reply(
            &mut c,
            &chan(0xAA),
            ForeignReplyStamps {
                access: Some("writer"),
                ..Default::default()
            },
            100
        ));
        assert_eq!(
            c.foreign_sets[0].updated_at, 101,
            "strictly past the held stamp"
        );
        assert!(!refresh_foreign_set_from_reply(
            &mut c,
            &chan(0xAA),
            ForeignReplyStamps {
                access: Some("writer"),
                ..Default::default()
            },
            500
        ));
        assert_eq!(c.foreign_sets[0].updated_at, 101, "no change, no stamp");

        assert!(forget_foreign_set(&mut c, &chan(0xAA), 200));
        assert_eq!(c.foreign_sets.len(), 1, "a tombstone, never removed");
        assert!(find_foreign_set(&c, &chan(0xAA)).is_none());
        assert!(
            find_foreign_set_by_hash(&c, &fauna_core::path_crypto::set_name_hash("photos"))
                .is_none()
        );
        assert_eq!(live_foreign_sets(&c).count(), 0);
        assert!(!forget_foreign_set(&mut c, &chan(0xAA), 300), "idempotent");

        assert!(record_foreign_set(
            &mut c,
            foreign(0xAA, "https://home.example", Some("photos")),
            150
        ));
        assert!(
            find_foreign_set(&c, &chan(0xAA)).is_some(),
            "a re-accept revives"
        );
        assert!(
            c.foreign_sets[0].accepted_at > 200,
            "stamped past the leave"
        );
    }

    #[test]
    fn migrate_set_identity_merges_when_target_exists() {
        // A peer device already migrated (merged in): the resumed migration
        // merges histories (no generation lost).
        let mut c = cfg();
        record_new_set(&mut c, chan(1), [0x11; 32], 1000);
        record_new_set(&mut c, chan(7), [0x77; 32], 3000);

        assert!(migrate_set_identity(&mut c, &chan(1), chan(7), NOW));
        let keys = content_keys(&c, &chan(7)).expect("merged");
        // Both genesis generations survive the merge (both are version 1 with
        // different keys — merge keeps the deterministically-higher current and
        // retains the other in prior).
        let mut all: Vec<[u8; 32]> = keys.generations().map(|g| g.key.to_array()).collect();
        all.sort();
        assert_eq!(all, vec![[0x11; 32], [0x77; 32]]);
    }

    #[test]
    fn migrate_set_identity_same_identity_is_noop() {
        let mut c = cfg();
        record_new_set(&mut c, chan(1), [0x11; 32], 1000);
        assert!(!migrate_set_identity(&mut c, &chan(1), chan(1), NOW));
        assert!(content_keys(&c, &chan(1)).is_some());
    }

    #[test]
    fn commit_generation_unknown_set_errors() {
        let mut c = cfg();
        let g = ContentKeyGeneration {
            version: 2,
            key: [0x22; 32].into(),
            rotated_at: 2000,
        };
        assert_eq!(
            commit_generation(&mut c, &chan(9), &g),
            Err(CustodyError::SetNotFound)
        );
    }

    // ── member custody ingest (Phase 0 — the read leg) ──────────────────────
    //
    // A member receives the full generation bundle out of the group content-key
    // envelope and merges it into their OWN custody, exactly as a second owner
    // device merges a peer's custody. `merge_received_keys` is the pure half of
    // that ingest (the network fetch + MLS-open live in the conversations
    // session): it creates the entry on a member's first ingest, is idempotent
    // on re-ingest (so the D2 poll-cadence retry never thrashes the plane
    // write), and — built on the same no-generation-lost `FolderContentKeys::merge`
    // as the owner-device convergence — retains a concurrent-rotation shadow and
    // advances `current` when a later rotated bundle arrives.

    #[test]
    fn merge_received_keys_first_ingest_creates_the_entry() {
        let mut c = cfg();
        let received = FolderContentKeys::genesis([0x11; 32], 1000);
        assert!(
            merge_received_keys(&mut c, chan(1), received.clone()),
            "first ingest changes custody"
        );
        assert_eq!(c.sets.len(), 1);
        assert_eq!(content_keys(&c, &chan(1)).unwrap(), received);
    }

    #[test]
    fn merge_received_keys_reingest_is_idempotent() {
        let mut c = cfg();
        let received = FolderContentKeys::genesis([0x11; 32], 1000);
        assert!(merge_received_keys(&mut c, chan(1), received.clone()));
        // The D2 retry re-ingests the identical bundle — no change, no CAS churn.
        assert!(
            !merge_received_keys(&mut c, chan(1), received.clone()),
            "re-ingesting the same bundle reports no change"
        );
        assert_eq!(c.sets.len(), 1);
        assert_eq!(content_keys(&c, &chan(1)).unwrap(), received);
    }

    #[test]
    fn merge_received_keys_later_rotated_bundle_advances_current_and_retains_priors() {
        let mut c = cfg();
        // First ingest at genesis.
        merge_received_keys(
            &mut c,
            chan(1),
            FolderContentKeys::genesis([0x11; 32], 1000),
        );
        // The owner later rotated (member removal); the member receives the
        // richer bundle on the rotation-commit poll.
        let mut rotated = FolderContentKeys::genesis([0x11; 32], 1000);
        rotated.rotate([0x22; 32], 2000);
        assert!(
            merge_received_keys(&mut c, chan(1), rotated.clone()),
            "a richer bundle advances custody"
        );
        let held = content_keys(&c, &chan(1)).unwrap();
        assert_eq!(held.current_version(), 2);
        assert_eq!(held.current_key(), &[0x22; 32]);
        assert_eq!(
            held.key_for(1),
            Some(&[0x11; 32]),
            "prior generation retained (history-on-join)"
        );
        // Re-ingesting the older genesis bundle after the advance is a no-op.
        assert!(
            !merge_received_keys(
                &mut c,
                chan(1),
                FolderContentKeys::genesis([0x11; 32], 1000)
            ),
            "an older bundle does not regress or churn custody"
        );
        assert_eq!(content_keys(&c, &chan(1)).unwrap().current_version(), 2);
    }

    #[test]
    fn merge_received_keys_retains_a_concurrent_rotation_shadow() {
        // if the member already holds one
        // gen-2 key and ingests a differently-keyed gen-2 (two owner devices
        // rotated concurrently, both bundles reaching the member), the merge
        // keeps BOTH candidates — the AEAD tag disambiguates at open time.
        let mut c = cfg();
        let mut a = FolderContentKeys::genesis([0x11; 32], 1000);
        a.rotate([0x99; 32], 2000);
        merge_received_keys(&mut c, chan(1), a);

        let mut b = FolderContentKeys::genesis([0x11; 32], 1000);
        b.rotate([0x55; 32], 2000);
        assert!(
            merge_received_keys(&mut c, chan(1), b),
            "the second concurrent gen-2 key is a change"
        );
        let held = content_keys(&c, &chan(1)).unwrap();
        let v2: Vec<[u8; 32]> = held.keys_for(2).copied().collect();
        assert!(
            v2.contains(&[0x99; 32]) && v2.contains(&[0x55; 32]),
            "both concurrent gen-2 keys retained: {v2:?}"
        );
    }

    /// A scrubbed row (blank `name`, `name_hash` + `name_sealed`) is named from
    /// custody by its hash; one custody cannot name is dropped; a row that
    /// still carries its plaintext passes through.
    /// A projection names a sealed set by its hash alone. The owner's entry is
    /// keyed by name, so the lineage read finds the name in custody by that
    /// hash — and answers nothing for a hash custody holds no name for.
    #[test]
    fn set_lineage_by_hash_finds_the_owners_entry_for_a_scrubbed_row() {
        use fauna_protocol::folders::FolderSummary;
        let hash = fauna_core::path_crypto::set_name_hash;
        let scrubbed = |name: &str| FolderSummary {
            name: String::new(),
            name_hash: Some(fauna_protocol::ByteBuf::from(hash(name).to_vec())),
            name_sealed: Some(fauna_protocol::ByteBuf::from(vec![1u8])),
            role: Some("owner".into()),
            ..Default::default()
        };
        let mut held = FoldersConfig::default();
        record_created_set(&mut held, "site", [0x11; 32], None, 1_000);
        let roster = vec![scrubbed("site"), scrubbed("not-in-custody")];

        assert_eq!(
            set_lineage_by_hash(&roster, &held, &hash("site")).live,
            live_set_nonce(&held, "site")
        );
        assert!(live_set_nonce(&held, "site").is_some());
        assert_eq!(
            set_lineage_by_hash(&roster, &held, &hash("not-in-custody")).live,
            None
        );
    }

    #[test]
    fn named_from_custody_names_a_scrubbed_row_by_hash() {
        use fauna_protocol::folders::FolderSummary;
        let scrubbed = |id: i64, name: &str| FolderSummary {
            id,
            name: String::new(),
            name_hash: Some(fauna_protocol::ByteBuf::from(
                fauna_core::path_crypto::set_name_hash(name).to_vec(),
            )),
            name_sealed: Some(fauna_protocol::ByteBuf::from(vec![1u8])),
            ..Default::default()
        };
        let mut held = FoldersConfig::default();
        record_created_set(&mut held, "site", [0x11; 32], None, 1_000);

        let named = named_from_custody(
            vec![
                scrubbed(1, "site"),
                scrubbed(2, "not-in-custody"),
                FolderSummary {
                    id: 3,
                    name: "__drafts".into(),
                    ..Default::default()
                },
            ],
            &held,
        );
        let got: Vec<(i64, &str)> = named.iter().map(|r| (r.id, r.name.as_str())).collect();
        assert_eq!(got, vec![(1, "site"), (3, "__drafts")]);
    }
}
