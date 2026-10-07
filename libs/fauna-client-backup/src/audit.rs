//! The client-side backup audit loop — leg (b) of the segment-backup track.
//!
//! **What it is for.** The source nest is the party that seals and uploads an
//! owner's segments, and `fauna.backup.status` is that same nest reporting on
//! its own work. If it stops backing up — through failure, misconfiguration, or
//! hostility — nothing in that projection would say so. The audit is the owner's
//! independent check: it asks each **destination** what it is actually holding,
//! over the owner's own authenticated connection, and compares that against what
//! the client itself knows it has locally. Neither the source nest's word nor
//! the destination's self-report is taken on trust.
//!
//! **The ratified parameters** live in
//! `docs/goal/behavior/backup-restore.md` § Background Tasks and are hard-coded
//! constants (product-invariant bucket 1 — no human ever chooses them):
//! [`AUDIT_SAMPLE_K`], [`AUDIT_MIN_INTERVAL_SECS`], [`FRESHNESS_SLACK_SECS`],
//! [`AUDIT_OVERDUE_SECS`]. All are ≪ the destination-side custody grace window
//! `T` = 30 d by construction, which is what bounds loss-under-attack by the
//! audit cadence rather than by `T`.
//!
//! **The UI surface** is `docs/goal/ui/backups.md` § Audit-alert surface:
//! `backup-destination-last-audit-time` per row, and an indexed
//! `backup-audit-alert` banner per failing destination. The label mapping is
//! shared (`fauna_core::format`), like the upload-status labels.
//!
//! # The three failing states, and the two that are not
//!
//! The ratified alert fires on exactly three verdicts —
//! [`AuditVerdict::FreshnessFailure`], [`AuditVerdict::InclusionFailure`], and
//! [`AuditVerdict::Overdue`]. Two more outcomes exist and deliberately do **not**
//! alert:
//!
//! - [`AuditVerdict::Unreachable`] — a destination that cannot be reached right
//!   now says nothing about the backup's health; a laptop on a plane would
//!   otherwise raise a loud data-loss alarm every flight. Sustained
//!   unreachability is not swallowed, though: it never refreshes the
//!   last-passed clock, so it trips [`AuditVerdict::Overdue`] on its own after
//!   [`AUDIT_OVERDUE_SECS`]. That is the "no device awake for a week" case the
//!   goal doc names, and it is why overdue is measured separately from freshness.
//! - [`AuditVerdict::Passed`] — the healthy case, which still writes a timestamp
//!   so the page can show *positive* evidence the audit runs rather than only
//!   ever showing bad news.
//!
//! # Reports only
//!
//! Per the user ruling of 2026-07-24 the loop performs **no** healing and mints
//! **no** grants — enroll stays the only writer. An audit that repaired what it
//! found would be unable to tell you it had ever been broken.

use crate::trust::{BackupDestinationConnector, BackupNestSeam, connect_destination};
use fauna_core::data::{BackupDestination, ContentHash};
use fauna_core::file_download::{BlobFetcher, FileDownloadKeys};
use fauna_core::format::BackupAuditAlertReason;
use fauna_protocol::backup::{CustodyItem, GenerationItem};
use fauna_protocol::segments::{LiveManifestMirror, SegmentFamily};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Records sampled per (destination, kind-scope) per pass. Ratified: at K = 16 a
/// 10 %-fraction omission is caught with ≈ 81 % probability in a single pass
/// (`1 - 0.9^16`), compounding across daily passes to near-certainty well inside
/// the grace window `T`.
pub const AUDIT_SAMPLE_K: usize = 16;

/// Debounce floor between audit passes for one destination. The loop may be
/// *triggered* far more often (app open, foreground, periodic tick); this is the
/// floor that keeps it from re-running on every trigger.
pub const AUDIT_MIN_INTERVAL_SECS: i64 = 24 * 60 * 60;

/// How far a destination's backed-up high-water may lag the client's own
/// locally-synced high-water before it is a freshness failure.
pub const FRESHNESS_SLACK_SECS: i64 = 48 * 60 * 60;

/// How far ahead of the observer's own clock an observed stamp may sit before
/// [`observe_local_record`] clamps it — the cushion for honest clock drift
/// between the sender's device and this one, nothing more.
///
/// The observation feed's stamps are the **sender's** word: a FaunaMls message's
/// `timestamp` is written by whoever sent it, inside the encrypted payload. An
/// unbounded feed let any chat correspondent latch the monotonic high-water to
/// a forged future date — every later audit of every destination then reported
/// a freshness failure for ever, burying a real backup failure under a standing
/// false alarm. Clamping bounds what one forged stamp can buy to this
/// cushion, which is a sliver of [`FRESHNESS_SLACK_SECS`], so it can never cost
/// an honest destination its pass.
pub const OBSERVATION_MAX_FUTURE_SKEW_SECS: i64 = 5 * 60;

/// How long a destination may go without a *successful* audit before that alone
/// raises the alert, independent of what any single pass found.
pub const AUDIT_OVERDUE_SECS: i64 = 7 * 24 * 60 * 60;

/// The outcome of one audit pass against one destination.
///
/// Serializable because the **standing** verdict persists client-locally
/// between passes ([`DestinationAuditRecord`]) — a destination inside its
/// debounce contributes no fresh verdict, and its alert must survive that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditVerdict {
    /// Fresh, and every sampled record was present and openable.
    Passed,
    /// The destination's backed-up high-water lags the client's own
    /// locally-synced high-water by more than [`FRESHNESS_SLACK_SECS`].
    FreshnessFailure {
        /// Seconds of lag, always > [`FRESHNESS_SLACK_SECS`].
        lag_secs: i64,
    },
    /// A sampled record was missing from the destination, or was present but
    /// could not be opened under the owner's derived `NestBackupKey`.
    InclusionFailure {
        /// How many of the `sampled` records failed.
        missing: u32,
        sampled: u32,
    },
    /// No pass has succeeded for longer than [`AUDIT_OVERDUE_SECS`].
    Overdue {
        /// Seconds since the last successful pass — or since the destination
        /// was added, when none has ever succeeded.
        since_secs: i64,
    },
    /// The destination could not be reached or answered an error. Quiet by
    /// design; see the module docs.
    Unreachable { error: String },
    /// A verdict a newer build of this app wrote into the audit-state file that
    /// this build cannot name — the open arm of `transport.md` § Rule 3 in full
    /// (*Open, carrying*). The file is rewritten whole by every pass and every
    /// observation, so the arm holds the verdict exactly as read (the store is
    /// JSON on every platform, hence a JSON value rather than a dag-cbor one)
    /// and re-emits it unchanged. It renders neutral — no banner, no action —
    /// and makes its destination due a **re-audit now** whatever its debounce
    /// clock says ([`run_audit_pass`]), so the first pass this build runs
    /// replaces it with a verdict this build can stand behind. No pass ever
    /// produces one.
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

impl AuditVerdict {
    /// The banner reason this verdict renders, or `None` when it is quiet.
    ///
    /// [`BackupAuditAlertReason`] is the plain-data mirror of the three
    /// alerting states, living in `fauna-core` because that is where the label
    /// mapping lives and this crate depends on it, not the other way round
    /// (`fauna_core::format::backup_audit_alert_label`).
    ///
    /// This is the **only** verdict→banner map, and [`Self::is_alerting`] is
    /// defined through it, so "alerts" and "has banner text" cannot drift
    /// apart: a new alerting variant that forgot its banner would be a variant
    /// that silently stopped alerting, and the test below would catch it.
    pub fn alert_reason(&self) -> Option<BackupAuditAlertReason> {
        match *self {
            AuditVerdict::FreshnessFailure { lag_secs } => {
                Some(BackupAuditAlertReason::Freshness { lag_secs })
            }
            AuditVerdict::InclusionFailure { missing, sampled } => {
                Some(BackupAuditAlertReason::Inclusion { missing, sampled })
            }
            AuditVerdict::Overdue { since_secs } => {
                Some(BackupAuditAlertReason::Overdue { since_secs })
            }
            AuditVerdict::Passed | AuditVerdict::Unreachable { .. } | AuditVerdict::Unknown(_) => {
                None
            }
        }
    }

    /// Whether this verdict renders a `backup-audit-alert` banner. Exactly the
    /// three ratified failing states — see the module docs for why
    /// [`AuditVerdict::Unreachable`] is not one of them.
    pub fn is_alerting(&self) -> bool {
        self.alert_reason().is_some()
    }
}

/// The client-local audit state for one destination.
///
/// Persisted client-locally, never on the account plane: it is this device's own
/// evidence about a destination, not a user preference, and two devices
/// auditing the same destination hold independently valid records.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DestinationAuditState {
    pub destination_id: String,
    /// Unix seconds of the last **passed** audit — what
    /// `backup-destination-last-audit-time` renders. `None` = never passed.
    pub last_passed_at: Option<i64>,
    /// Unix seconds of the last audit *attempt*, passed or not — the debounce
    /// clock. Kept separate from `last_passed_at` so a destination that keeps
    /// failing does not re-audit on every trigger.
    pub last_attempt_at: Option<i64>,
    /// **The generation pin**: per reserved-set
    /// ledger this destination serves ([`ledger_key`]), the ledger generation
    /// (`LiveManifestMirror::next_segment_id_seen` — the source's saved segment
    /// counter) this device last verified there. A destination later serving
    /// a **lower** one is rolling the set back to an older genuine ledger,
    /// losing everything after it, unless the owner's own source nest confirms
    /// it is the one that regressed ([`SourceLedgerVouch`]).
    ///
    /// This device's own evidence, like the two clocks: what it opened under
    /// the owner's key it cannot be argued out of afterwards. Empty until the
    /// first ledger opens; absent from a state file written before the pin
    /// existed, which loads as empty (one more re-verification, nothing else).
    ///
    /// A pin outlives the ledger row it was taken from: a destination that
    /// stops listing a pinned ledger is judged by [`LedgerPin::seen_live_at`]
    /// — the vanished-ledger rule, [`reconcile_with_ledgers`].
    #[serde(default)]
    pub verified_ledger_generations: BTreeMap<String, LedgerPin>,
    /// **The recovery notice's record** — per reserved-set ledger
    /// ([`ledger_key`], like the pins), the source regression this device
    /// accepted there ([`SourceLedgerVouch`]'s "the source went backwards"
    /// answer), kept while the destination still holds what the source lost
    /// (`backup-restore.md` § Background Tasks → *Implementation status
    /// (audit loop)*, the accepted-regression bullet).
    ///
    /// The verdict stays `Passed` — the destination did nothing wrong — so
    /// this is not a verdict: [`DestinationAuditRecord::alert_reasons`] reads
    /// it into the fifth banner reason while the record stands. Written once
    /// per accepted regression (a second rollback of the same key replaces
    /// it) and re-evaluated by every pass that reaches the inclusion arm: it
    /// **stands while the destination still holds lost custody for its key**
    /// and is pruned when none remains ([`lost_custody`] owns the rule). A
    /// pass that never reaches the ledgers leaves it as it is. Absent from a
    /// state file written before the notice existed, which loads as none.
    #[serde(default)]
    pub accepted_regressions: BTreeMap<String, AcceptedRegression>,
    /// **The bound identity this destination's writer seat was last settled
    /// under** — hex of the source box's identity at the pass whose seat
    /// carry ([`crate::seat_carry`]) reached a verdict here: carried, already
    /// held, revoked, unlinked, or no seat. A pass whose bound identity
    /// differs — the box rotated since — runs this destination at once,
    /// whatever the debounce says (`segment-backup-protocol.md`
    /// § Cross-location backup protocol → *Where the carry runs*: "on the
    /// first pass under a changed bound identity — regardless of
    /// `AUDIT_MIN_INTERVAL`"), and keeps doing so until a carry settles.
    ///
    /// `None` = never settled (a state written before the carry existed loads
    /// so, and costs one prompt pass).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat_settled_under: Option<String>,
}

/// One accepted source regression: the owner's nest went backwards and this
/// destination still holds segments the nest lost — live where the source's
/// counter has not reused their id, retained for the grace window where it
/// has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedRegression {
    /// The generation this device had verified before the regression.
    pub pinned: u32,
    /// The lower generation the destination served, and the source vouched for.
    pub served: u32,
    /// Unix seconds of the pass that accepted the regression.
    pub observed_at: i64,
    /// Unix seconds of the pass whose **counter floor** landed on the source
    /// ([`SourceLedgerVouch::floor_counter`], with `pinned`): from then on the
    /// source numbers its segments above everything the copy holds, so a lost
    /// segment not yet overwritten stays live at the destination. `None` until
    /// it lands — asked at acceptance and again by every pass the record
    /// stands through.
    #[serde(default)]
    pub floored_at: Option<i64>,
    /// Unix seconds at which the first of the lost generations the destination
    /// only *retains* is reclaimed: the earliest `superseded_at` among them
    /// plus the destination's own reported `grace_secs` — both the
    /// destination's figures, so no client constant duplicates `T`. `None`
    /// when what remains is live rows only, which no clock reclaims.
    #[serde(default)]
    pub recoverable_until: Option<i64>,
}

/// One pinned ledger: the generation this device last verified, and when it
/// last saw the ledger row **listed live** — the clock the vanished-ledger
/// rule runs on ([`reconcile_with_ledgers`]).
///
/// A retained sighting does not move `seen_live_at`: a set the source tore
/// down keeps its ledger *retained* for the grace window `T` and then loses
/// it, and the rule needs to know how long ago the row was last live to tell
/// that reclaim from a destination's early one. `None` is a pin persisted
/// before the sighting existed (a bare generation on disk, 2026-09-28): it
/// loads, guards against rollback as before, and counts as unseen for longer
/// than any window — one lenient pass for a state file no released artifact
/// wrote, rather than a refused load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LedgerPin {
    pub generation: u32,
    pub seen_live_at: Option<i64>,
}

impl<'de> Deserialize<'de> for LedgerPin {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Full {
                generation: u32,
                #[serde(default)]
                seen_live_at: Option<i64>,
            },
            Bare(u32),
        }
        Ok(match Repr::deserialize(deserializer)? {
            Repr::Full {
                generation,
                seen_live_at,
            } => LedgerPin {
                generation,
                seen_live_at,
            },
            Repr::Bare(generation) => LedgerPin {
                generation,
                seen_live_at: None,
            },
        })
    }
}

/// The pin's key for one ledger row: the reserved set and the mirror path
/// inside it, which together name one (kind-scope, family) — e.g.
/// `__mail/<scope-hex>/manifest.mail`.
pub fn ledger_key(folder_name: &str, path: &str) -> String {
    format!("{folder_name}/{path}")
}

/// One destination's audit result: the verdict plus the state to persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationAuditOutcome {
    pub destination_id: String,
    pub verdict: AuditVerdict,
    pub state: DestinationAuditState,
}

/// The inputs one pass needs about a destination, gathered by the caller.
#[derive(Debug, Clone, Default)]
pub struct AuditInputs {
    /// Unix seconds of the newest record this client holds **locally** for the
    /// audited scope — the source-untrusted side of the freshness comparison.
    /// `None` when the client holds nothing, in which case there is nothing a
    /// backup could be missing and freshness cannot fail.
    pub local_high_water: Option<i64>,
    /// `BackupDestination.added_at` — the floor for both the freshness and
    /// overdue clocks, so a destination enrolled minutes ago is not judged
    /// against a backup pass that has not had time to run.
    pub added_at: i64,
}

/// Whether a destination is due an audit pass, given its stored state.
///
/// The debounce is on the last **attempt**, not the last pass: a destination
/// that is unreachable or failing must not be re-audited on every app open.
///
/// A destination whose seat is not yet settled under the current bound
/// identity is due whatever this says ([`DestinationAuditState::seat_settled_under`],
/// read by [`audit_all`]).
pub fn should_run(state: &DestinationAuditState, now: i64) -> bool {
    match state.last_attempt_at {
        Some(last) => now.saturating_sub(last) >= AUDIT_MIN_INTERVAL_SECS,
        None => true,
    }
}

/// The destination's backed-up high-water: the most recent moment **it** says it
/// accepted custody for this owner.
///
/// `updated_at` is stamped by the destination on receipt, never supplied by the
/// uploading source nest — which is what keeps this side of the freshness
/// comparison independent of the party being audited.
pub fn destination_high_water(items: &[CustodyItem]) -> Option<i64> {
    items.iter().map(|i| i.updated_at).max()
}

/// Freshness: has the destination fallen more than [`FRESHNESS_SLACK_SECS`]
/// behind what this client itself holds?
///
/// The `added_at` floor is load-bearing. A destination enrolled ten minutes ago
/// legitimately holds nothing yet (the nest's sweep runs every 15 min), so
/// judging its empty custody against a years-old local high-water would alarm on
/// every single enrollment. Flooring the destination's high-water at `added_at`
/// starts its clock when the user actually asked for the backup, and the slack
/// then does its normal job from there.
pub fn evaluate_freshness(inputs: &AuditInputs, dest_high_water: Option<i64>) -> AuditVerdict {
    let Some(local) = inputs.local_high_water else {
        // Nothing held locally ⇒ nothing a backup could be missing.
        return AuditVerdict::Passed;
    };
    let effective_dest = dest_high_water.unwrap_or(0).max(inputs.added_at);
    let lag = local.saturating_sub(effective_dest);
    if lag > FRESHNESS_SLACK_SECS {
        AuditVerdict::FreshnessFailure { lag_secs: lag }
    } else {
        AuditVerdict::Passed
    }
}

/// Overdue: has it been more than [`AUDIT_OVERDUE_SECS`] since a pass last
/// succeeded?
///
/// Measured from `added_at` when no pass has ever succeeded, so a newly enrolled
/// destination is not born overdue — but one that has never once been auditable
/// still raises the alert a week in, which is precisely the case a client that
/// is rarely awake produces.
pub fn evaluate_overdue(
    state: &DestinationAuditState,
    inputs: &AuditInputs,
    now: i64,
) -> Option<i64> {
    let since = state.last_passed_at.unwrap_or(inputs.added_at);
    let elapsed = now.saturating_sub(since);
    (elapsed > AUDIT_OVERDUE_SECS).then_some(elapsed)
}

/// Deterministically choose up to `k` indices out of `len`, varying the subset
/// with `seed`.
///
/// Deliberately not a random draw. A pass must be **reproducible** from its
/// recorded inputs — an audit that cannot be replayed cannot be argued with —
/// while still covering a different subset each pass so detection compounds over
/// days rather than re-checking the same K records forever. A stride coprime
/// with `len` walks the whole population before repeating.
pub fn sample_indices(len: usize, k: usize, seed: u64) -> Vec<usize> {
    if len == 0 || k == 0 {
        return Vec::new();
    }
    if len <= k {
        return (0..len).collect();
    }
    // Any stride coprime with `len` enumerates every index before repeating;
    // walking up from a seed-derived start finds one in a few steps.
    let mut stride = (seed % len as u64).max(1) as usize;
    while gcd(stride, len) != 1 {
        stride = stride % len + 1;
    }
    let start = (seed % len as u64) as usize;
    let mut out = Vec::with_capacity(k);
    let mut at = start;
    for _ in 0..k {
        out.push(at);
        at = (at + stride) % len;
    }
    out
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Escalate a read we could not make into the loud verdict, once not-knowing
/// has gone on too long.
///
/// [`AuditVerdict::Overdue`] is precisely **the escalation of a non-verdict**.
/// That framing is what keeps the two clocks straight: a destination we *can*
/// read always reports what we actually found, and "I have not been able to
/// check for a week" is reserved for when we genuinely could not. The
/// alternative — checking overdue up front — would report `Overdue` for the
/// very first audit of any destination older than the window, without so much
/// as attempting the read.
fn escalate_unreachable(
    prior: &DestinationAuditState,
    inputs: &AuditInputs,
    now: i64,
    error: String,
) -> AuditVerdict {
    match evaluate_overdue(prior, inputs, now) {
        Some(since_secs) => AuditVerdict::Overdue { since_secs },
        None => AuditVerdict::Unreachable { error },
    }
}

// ── the inclusion arm ───────────────────────────────────────────────────────

/// What the inclusion arm needs to open sampled records at one destination: a
/// fetcher for that destination's public content-addressed blob routes, and the
/// owner key material those chunks are sealed under.
///
/// **Separate from [`BackupDestinationConnector`] on purpose** — the two speak
/// different protocols. Custody listing is authenticated WS-RPC to the
/// destination; the blob fetch is the *open* content-addressed HTTP routes
/// (`/api/v1/manifests/{hash}`, `/api/v1/chunks/{hash}`), which carry no bearer
/// because confidentiality here is cryptographic, not access-controlled. Folding
/// the fetch into the RPC seam would force every existing `BackupNestSeam` impl
/// to grow an HTTP leg it otherwise has no use for.
///
/// Both methods are sync: constructing a fetcher wraps a URL, and deriving the
/// key is one blake3. Neither needs `async_trait`.
///
/// **Every leg of this already exists per platform**, which is why the audit
/// needs no declared platform absence: `NestPublicChunkFetcher` /
/// `ForeignPublicChunkFetcher` natively (`fauna-client`), `WasmPublicChunkFetcher`
/// on web (`fauna-core`), and [`FileDownloadKeys::owner`] accepts the granted
/// `NestBackupKey` directly via `OwnerSealKey::SourceNest`.
///
/// The third method is the one piece of this seam that *is* platform-shaped:
/// [`Self::folder_index`], the client's own index of a covered ordinary folder
/// ([`FolderIndex`]), which only a shell holding a synced replica of that
/// folder can supply. Its `None` is a declared absence, never an error — see
/// [`FolderIndexSource`].
pub trait BackupInclusionSource: fauna_protocol::MaybeSendSync {
    /// A fetcher pointed at **this destination's** blob routes. Pointed anywhere
    /// else the audit would be verifying the wrong box — the whole point is that
    /// the bytes come from the party being audited.
    fn fetcher(&self, destination_nest_url: &str) -> Arc<dyn BlobFetcher>;
    /// The owner's reader keys — in production
    /// `FileDownloadKeys::owner(NestBackupKey::derive(&seed))`, the same key the
    /// source nest sealed these segments under.
    fn keys(&self) -> FileDownloadKeys;
    /// This client's own index of the covered folder behind `folder_set` — the
    /// covered-folder mirror plane's inclusion population — or
    /// `None` where this shell holds no replica it can vouch for. The contract
    /// is [`FolderIndexSource::folder_index`]'s; a production source delegates
    /// to one.
    fn folder_index(&self, folder_set: &str) -> Option<FolderIndex>;
}

/// One live head of a covered folder as **this client's own replica** records
/// it — the shape a [`FolderIndex`] names custody in, matching the mirror
/// row the source nest writes for the same head
/// (`bins/fauna-nest/src/segment_backup.rs::run_folder_once`: rest path
/// `hex(path_hash)`, `manifest_hash` the head's manifest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderIndexEntry {
    /// `hex(fauna_core::sync::path_hash(normalized rel path))` — the mirror
    /// row's `path` at the destination.
    pub path_hash_hex: String,
    /// Hex of the head's `ChunkManifest` hash — the mirror row's
    /// `manifest_hash`.
    pub manifest_hash_hex: String,
    /// Unix seconds: when the **source** recorded this head (the head change's
    /// `created_at`, which the replica stores as `remote_mtime`). The mirror is
    /// expected at the destination [`FRESHNESS_SLACK_SECS`] after this — the
    /// same slack the freshness arm grants the reserved rails — so a head the
    /// coordinator has not swept yet is not a miss.
    pub recorded_at: i64,
}

/// A covered folder's live head as this client's own replica holds it: the
/// independent population the mirror plane's inclusion arm reconciles the
/// destination's list against.
///
/// A replica is the client's *own eyes* — what this device pulled from the
/// source through the ordinary sync engine and persisted — so, like the
/// freshness arm's observed high-water, nothing the source or the destination
/// says afterwards can retract it. That is what makes it an anchor and a live
/// `fauna.sync.files` read not one: a live read is the source's word at audit
/// time, and a source outage would then decide a *destination's* verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderIndex {
    /// Unix seconds: when this replica was last **known consistent** with the
    /// source (a completed transfer or a clean pull pass — the sync engine's
    /// `last_sync` stamp). A replica older than [`FRESHNESS_SLACK_SECS`] cannot
    /// witness: its heads may have been superseded so long ago that the
    /// destination legitimately no longer retains them, so the arm falls back
    /// to the destination's list for that set rather than alarm on the
    /// client's own staleness.
    pub consistent_at: i64,
    /// Every live (non-tombstoned) path whose recorded head the replica knows.
    pub entries: Vec<FolderIndexEntry>,
}

/// Where a shell's synced replica of a covered folder is read from — the
/// platform-shaped half of [`BackupInclusionSource`], split out so the native
/// source in `fauna-client-pair` can compose one from the sync engine (which
/// owns the replica database and the set-name parser) without that crate
/// growing an engine dependency of its own.
///
/// `None` is a **declared absence**, and it is the honest answer in three
/// cases, each a stated residual of `backup-destinations.md` § Ordinary-folder
/// coverage → *Retention + audit*: web, whose SPA holds no persistent replica
/// ([`NoFolderIndex`]); a native device with no seat on that folder (no
/// `fsid-<ref>.db` under its state dir); and a set whose source nest is not the
/// one this device's replicas belong to (an owner's linked nest holding the same
/// numeric folder id). For every `None` the mirror plane keeps its floor —
/// hash-verified presence sampled from the destination's list.
pub trait FolderIndexSource: fauna_protocol::MaybeSendSync {
    /// The replica index for `folder_set` (`__folder/<source-nest-hex>/<id>`),
    /// or `None` when this shell cannot vouch for one.
    fn folder_index(&self, folder_set: &str) -> Option<FolderIndex>;
}

/// The declared absence: a shell holding no synced replica of any folder. Web
/// is this by construction; a native shell is this only until its audit call
/// site hands the sync engine's replica reader in.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoFolderIndex;

impl FolderIndexSource for NoFolderIndex {
    fn folder_index(&self, _folder_set: &str) -> Option<FolderIndex> {
        None
    }
}

/// The owner's own **source** nest, asked one question and only ever to settle
/// a ledger regression the inclusion arm observed at a destination: what is your saved segment counter for this
/// `(serve tag, scope)` right now?
///
/// **Why the source may be asked here, when freshness pointedly never asks
/// it.** Freshness refuses the source's word because a hostile source could
/// make the arm *unfailable* by answering "nothing new". The pin's question is
/// different: a regression has already been observed; the source only decides
/// **which party went backwards**. A hostile source answering low legitimises
/// a destination's rollback — but a hostile source can already shrink its own
/// backup outright by uploading a shorter ledger (every writer is trusted for
/// *what* a set contains; the custodian's independent copy is the answer to
/// that), so the question hands it no power it lacked. What a hostile
/// **destination** cannot do is forge the source's answer, and the destination
/// is the party the pin exists to catch.
///
/// The three answers, and what each makes of a ledger served below the pin:
///
/// - `Ok(g)` with `g` **at or above** the pin — the source never went
///   below what this device verified, so the destination did: a rollback,
///   **missing with certainty**. The pin stands.
/// - `Ok(g)` with `g` **below** the pin — the source itself regressed
///   (restored from an older copy of its data directory; re-seeded from a copy
///   whose ledger carried a lower generation). Accepted: the pin **re-bases**
///   to the served generation and the set passes on its other merits.
/// - `Err` — could not ask. **Missing with certainty**: a regression the
///   source cannot vouch for stays a rollback until it can, and the next pass
///   asks again. Loud on purpose — the reverse would let a destination roll
///   back unremarked whenever the owner's nest happened to be down.
///
/// The one residual the acceptance leaves, stated: *during* a genuine source
/// regression a hostile destination may serve any post-regression ledger at or
/// below the source's new counter, and the pin re-bases to what it served —
/// so the segments the source uploaded between that ledger and its current
/// counter are unguarded for that one pass. Bounded by the rarity of the event
/// and by the pin catching up on every later pass.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait SourceLedgerVouch: fauna_protocol::MaybeSendSync {
    /// The source's saved counter for one serve tag (`mail`, `mail-placement`,
    /// …) and scope.
    async fn next_segment_id(&self, kind_tag: &str, scope_hex: &str) -> Result<u32, String>;

    /// Raise the source's saved counter for that serve tag and scope to at
    /// least `floor` (`fauna.segments.counter_floor`), answering the counter
    /// it then stands at. The pass calls it when it accepts a regression, with
    /// the generation this device had **pinned** — read off a ledger the
    /// source itself sealed, never the number the destination served — so the
    /// source stops reusing the ids of the segments it lost
    /// (`segment-backup-protocol.md` § Client-device custodian (pull) →
    /// *Restore* → *Recovery into the lived-in nest that regressed*, part (0)).
    ///
    /// Defaulted to "could not ask": a vouch that cannot floor leaves the
    /// record's `floored_at` unset, and the next pass asks again.
    async fn floor_counter(
        &self,
        kind_tag: &str,
        scope_hex: &str,
        floor: u32,
    ) -> Result<u32, String> {
        Err(format!(
            "this vouch cannot floor the source's counter (kind={kind_tag}, scope={scope_hex}, floor={floor})"
        ))
    }
}

/// A [`SourceLedgerVouch`] that connects to the owner's source nest **on
/// first use** through the same [`BackupDestinationConnector`] the audit opens
/// destinations with — the owner's own authenticated connection, the arm
/// `fauna.segments.list` admits — and keeps that one seam for the rest of the
/// pass. Nothing is dialled on a pass that observes no regression, which is
/// every honest pass.
pub struct LazySourceVouch<'a> {
    connector: &'a dyn BackupDestinationConnector,
    source_nest_url: &'a str,
    seam: std::sync::Mutex<Option<Arc<dyn BackupNestSeam>>>,
}

impl<'a> LazySourceVouch<'a> {
    pub fn new(connector: &'a dyn BackupDestinationConnector, source_nest_url: &'a str) -> Self {
        Self {
            connector,
            source_nest_url,
            seam: std::sync::Mutex::new(None),
        }
    }

    async fn seam(&self) -> Result<Arc<dyn BackupNestSeam>, String> {
        if let Some(seam) = self.seam.lock().map_err(|e| e.to_string())?.as_ref() {
            return Ok(Arc::clone(seam));
        }
        // Connect outside the lock (a held `std` lock across an await would
        // be wrong on native); a racing second connect is merely wasteful.
        // The source nest is the owner's own home nest, not a destination: no
        // enrolled destination id names it, so its seam is taken as the
        // connection hands it back. What it vouches for only ever *adds*
        // alerts (a regression it cannot account for stays a miss).
        let seam = self
            .connector
            .connect(self.source_nest_url)
            .await
            .map_err(|e| format!("connect to the source nest {}: {e}", self.source_nest_url))?
            .seam;
        *self.seam.lock().map_err(|e| e.to_string())? = Some(Arc::clone(&seam));
        Ok(seam)
    }
}

/// The seat carry's chain is the owner's own source nest's too, so the pass
/// reads it over the same lazily dialled seam — never on a pass whose every
/// seat is already the bound identity's.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl crate::seat_carry::RotationChainSource for LazySourceVouch<'_> {
    async fn rotation_chain(
        &self,
    ) -> Result<Vec<fauna_protocol::nest_rotation::SignedNestRotation>, String> {
        self.seam().await?.rotation_chain().await
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl SourceLedgerVouch for LazySourceVouch<'_> {
    async fn next_segment_id(&self, kind_tag: &str, scope_hex: &str) -> Result<u32, String> {
        self.seam()
            .await?
            .segments_next_id(kind_tag.to_string(), scope_hex.to_string())
            .await
    }

    async fn floor_counter(
        &self,
        kind_tag: &str,
        scope_hex: &str,
        floor: u32,
    ) -> Result<u32, String> {
        self.seam()
            .await?
            .segments_counter_floor(kind_tag.to_string(), scope_hex.to_string(), floor)
            .await
    }
}

/// The vouch of a pass that has **no** source to ask — every regression it
/// meets is a certain miss. What the pin-less [`evaluate_inclusion`] passes,
/// which with no pins never asks.
pub struct NoSourceVouch;

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl SourceLedgerVouch for NoSourceVouch {
    async fn next_segment_id(&self, _: &str, _: &str) -> Result<u32, String> {
        Err("no source nest to ask".into())
    }
}

/// The `(folder, kind-scope)` a custody row belongs to.
///
/// Segment backup writes `"{scope_hex}/seg-{id:08}.dat"`, its sidecar sibling
/// `"{scope_hex}/seg-{id:08}.meta"` (since 2026-08-29) and
/// `"{scope_hex}/manifest.{kind}"` inside one reserved set — the layout every
/// writer emits through `fauna_sync_engine::segment_backup::segment_rel_path` /
/// `segment_meta_rel_path` / `manifest_rel_path` — so the leading path
/// component **is** the kind-scope, for every file alike. A sidecar row is one
/// more owner-sealed row in its scope's stratum and opens exactly as its `.dat`
/// does. So is every row of the kind's **placement journal**, which rests in
/// the same set one component deeper (`"{scope_hex}/placement/…"`, since
/// 2026-09-26 — `fauna_sync_engine::segment_backup::SegmentFamily`): the infix
/// comes *after* the scope, so the leading component still names it, and a
/// journal row is sampled and opened like any other row of its scope.
///
/// ⚠ **This is the READ side of that contract, and it hand-parses only the
/// leading component.** The writers share one formatter (and the arms that need
/// the segment id go through `parse_segment_rel_path`); nothing makes this
/// parse follow it. If the layout ever grows a component *before* the scope,
/// the writers change together and this silently recovers the wrong scope — so
/// such a layout change must come here too. The folder name is carried too because it
/// is what makes the pair unique — `folder_name` alone is always `"__mail"`
/// today and would collapse every scope into one bucket.
///
/// A row with no path yields the empty scope; it is deliberately still a member
/// of the population — see [`open_sampled_record`].
fn scope_key(item: &CustodyItem) -> (&str, &str) {
    // ⚠ The ordinary-folder mirror plane has no kind-scope component to parse:
    // a mirror record's plaintext path is `hex(path_hash)` with no `/` at all
    // (`bins/fauna-nest/src/segment_backup.rs`), so the parse below would make
    // **every mirrored file its own stratum** — turning `AUDIT_SAMPLE_K` per
    // scope into "sample the entire corpus, every pass". A covered media
    // library would be re-downloaded whole on every audit. The set name is
    // already unique per covered folder (`__folder/<hex>/<folder-id>`), so it
    // IS the stratum, and K then means K per covered folder — which is exactly
    // the ratified per-scope guarantee.
    if fauna_core::data::is_folder_mirror_set(&item.folder_name) {
        return (item.folder_name.as_str(), "");
    }
    let scope = item
        .path
        .as_deref()
        .and_then(|p| p.split('/').next())
        .unwrap_or("");
    (item.folder_name.as_str(), scope)
}

/// Verify one sampled record by **hash-verified presence**: the destination
/// really holds the manifest and every chunk it names, and their bytes hash to
/// the addresses they are stored under — without opening anything.
///
/// This is the audit floor the goal doc ratifies for the ordinary-folder mirror
/// plane (`docs/goal/behavior/backup-destinations.md` § Ordinary-folder
/// coverage → *Retention + audit (questions 3 + 4)*): "content addressing makes
/// present-and-hash-matched exactly the integrity property custody protects",
/// and "openability is not required where the auditor legitimately lacks keys".
///
/// Here the auditor legitimately lacks them **by construction**, which is why
/// this is not a weakening: mirror bytes are the folder's own content-layer
/// ciphertext, mirrored with no re-seal, while [`open_sampled_record`] opens
/// under the *nest-backup* root — two deliberately domain-separated key trees
/// (`fauna_core::crypto`, `fauna.nest-backup.chunk.v1` vs
/// `fauna.backup.chunk.v1`). A shared folder is further out of reach: its
/// content key was never granted to this auditor at all. Attempting the open
/// there does not detect anything; it only guarantees a permanent failure.
///
/// What it still catches is what custody can actually lose: a deleted manifest,
/// a deleted chunk, a truncated or substituted body. A destination cannot forge
/// a body whose blake3 matches an address the owner recorded.
async fn verify_sampled_record_presence(
    fetcher: &dyn BlobFetcher,
    item: &CustodyItem,
) -> Result<(), String> {
    let Some(path) = item.path.as_deref() else {
        return Err(format!(
            "custody row {} carries no path — unverifiable",
            item.path_hash
        ));
    };
    let raw = hex::decode(&item.manifest_hash)
        .map_err(|e| format!("bad manifest hash hex for {path}: {e}"))?;
    let digest: [u8; 32] = raw
        .as_slice()
        .try_into()
        .map_err(|_| format!("manifest hash for {path} is not 32 bytes"))?;
    let manifest_hash = ContentHash::from_digest_raw(digest);

    let manifest_bytes = fetcher
        .fetch_manifest(&manifest_hash)
        .await
        .map_err(|e| format!("{path}: manifest fetch failed: {e}"))?;
    // Address-vs-bytes, not a trusted echo: the destination serves these bytes.
    if ContentHash::of_raw(&manifest_bytes) != manifest_hash {
        return Err(format!(
            "{path}: manifest bytes do not hash to {}",
            item.manifest_hash
        ));
    }
    let manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(&manifest_bytes)
            .map_err(|e| format!("{path}: manifest does not decode: {e}"))?;

    let store_keys = manifest.store_keys();
    let bodies = fetcher
        .fetch_chunks(&store_keys, path)
        .await
        .map_err(|e| format!("{path}: chunk fetch failed: {e}"))?;
    if bodies.len() != store_keys.len() {
        return Err(format!(
            "{path}: destination returned {} of {} chunks",
            bodies.len(),
            store_keys.len()
        ));
    }
    for (key, body) in store_keys.iter().zip(bodies.iter()) {
        if ContentHash::of_raw(body) != *key {
            return Err(format!(
                "{path}: chunk {} does not hash to its address",
                hex::encode(key.digest())
            ));
        }
    }
    Ok(())
}

/// Fetch one sampled record from the destination and open it.
///
/// `Ok(())` means the destination really holds openable bytes for this row: the
/// manifest fetched, every chunk fetched, the chunks opened under the owner key,
/// and the reassembled file's blake3 matched the manifest's `file_hash` — all
/// inside [`fauna_core::file_download::download_file_bytes_by_manifest`], which
/// is also what every production restore path walks.
///
/// **A path-less row is a failure, not a skip.** `path` is additive and always
/// written by segment backup, so `None` does not occur in production. Skipping
/// such rows would nonetheless hand a hostile destination a one-line bypass of
/// the entire inclusion arm: report custody with the paths omitted and nothing
/// is ever sampled. A row the destination cannot even name is precisely "present
/// but unopenable" (`docs/goal/ui/backups.md` § Audit-alert surface).
async fn open_sampled_record(
    fetcher: &dyn BlobFetcher,
    keys: &FileDownloadKeys,
    item: &CustodyItem,
) -> Result<(), String> {
    let Some(path) = item.path.as_deref() else {
        return Err(format!(
            "custody row {} carries no path — unverifiable",
            item.path_hash
        ));
    };
    let raw = hex::decode(&item.manifest_hash)
        .map_err(|e| format!("bad manifest hash hex for {path}: {e}"))?;
    let digest: [u8; 32] = raw
        .as_slice()
        .try_into()
        .map_err(|_| format!("manifest hash for {path} is not 32 bytes"))?;
    fauna_core::file_download::download_file_bytes_by_manifest(
        fetcher,
        keys,
        ContentHash::from_digest_raw(digest),
        None,
        path,
    )
    .await
    .map(|_| ())
    .map_err(|e| format!("{path}: {e}"))
}

/// Inclusion: are the records this destination claims custody of actually there,
/// and openable?
///
/// The `__folder/…` mirror sets **this client attached** to `destination_id`,
/// read from its own pinned `fauna.state.backup` destination list.
///
/// This is [`evaluate_inclusion`]'s plane-routing input, and the whole point is
/// its provenance: every one of these names came back from the **owner's own**
/// nest in an `attach_folder` reply and was written into the client's sealed
/// config by `fauna_client_config::attach_backup_destination_folder`. Nothing a
/// destination says can add to it.
///
/// Coverage rows share their destination's `destination_id` and differ only in
/// `folder_name` (that helper clones the enrolled row as a template), so the
/// destination's own reserved set — `__mail` and its per-kind siblings — sits in
/// the same list and is filtered out here by shape. That is why the result is a
/// set of *mirror* sets rather than "every name for this destination": routing
/// the reserved set to presence would drop the strong arm on the very plane it
/// is meant to defend.
///
/// Each set maps to its coverage row's `added_at` — the attach time, which
/// floors the mirror plane's inclusion population exactly as
/// [`AuditInputs::added_at`] floors freshness: a folder attached minutes ago
/// legitimately has nothing mirrored yet.
pub fn attached_mirror_sets(
    destinations: &[BackupDestination],
    destination_id: &str,
) -> AttachedMirrorSets {
    destinations
        .iter()
        .filter(|d| d.destination_id == destination_id)
        .filter(|d| fauna_core::data::is_folder_mirror_set(&d.folder_name))
        .map(|d| (d.folder_name.clone(), d.added_at as i64))
        .collect()
}

/// The `__folder/…` sets this client attached to one destination, each with
/// its coverage row's `added_at` — [`attached_mirror_sets`]'s shape, and
/// [`evaluate_inclusion`]'s plane-routing input.
pub type AttachedMirrorSets = BTreeMap<String, i64>;

/// [`AUDIT_SAMPLE_K`] records are sampled **per (destination, kind-scope)**, as
/// ratified — at K = 16 a 10 %-fraction omission is caught with ≈ 81 % probability
/// per pass, and that guarantee is per scope, so sampling K across the whole
/// custody set instead would silently weaken it for every scope but one.
///
/// `seed` varies the subset per pass while staying reproducible from the pass's
/// recorded inputs ([`sample_indices`]) — callers pass `now`, so detection
/// compounds across days rather than re-checking the same K records forever.
///
/// This is what makes an audit **pass** mean what the goal doc says it means. A
/// freshness check alone only proves the destination wrote *something* recently;
/// it cannot tell well-timed garbage from a real backup. Opening sampled bytes
/// under the owner's key can, and a destination cannot forge them — it never
/// holds the key they are sealed under.
///
/// That unforgeability is a property of the **open**, so it is only as good as
/// the guarantee that the open is what runs. Which arm runs is settled below,
/// by the client — from 2026-07-29 to 2026-08-21 this paragraph promised the
/// guarantee while the line that chose the arm read the destination's own reply.
///
/// # Why the plane routing is client-anchored
///
/// `attached_mirror_sets` is the set of `__folder/…` names **this client
/// attached to this destination**, read from its own pinned
/// `fauna.state.backup` destination list ([`attached_mirror_sets`]). It is the
/// routing input, and it must be, because the two planes are not equally
/// strong: the mirror plane takes hash-verified presence (rooted in the
/// destination's own `manifest_hash`, so it is self-consistent by construction)
/// while a reserved segment set takes the full open under a key the destination
/// has never held.
///
/// Until 2026-08-21 the routing read `item.folder_name` — a field of
/// `CustodyItem`, which **is the wire reply of `fauna.backup.custody.list`,
/// answered by the destination being audited**. A destination that had discarded
/// the owner's sealed bytes could therefore answer every row with a
/// `__folder/…` name, route the entire sample to the weak arm, and pass while
/// holding nothing. The strong arm was simply never reached.
///
/// So a `__folder/…` row the client never attached is **not** treated as a
/// mirror row: it takes the full open, which it will fail, correctly — a
/// destination inventing covered sets is precisely what this arm exists to
/// catch. Genuine coverage is unaffected, because the client attached it and
/// therefore knows its name.
///
/// `now` is the pass's clock: it seeds the sample ([`sample_indices`] — so
/// successive passes cover different subsets) and dates the mirror plane's
/// slack windows ([`reconcile_with_folder_indexes`]).
pub async fn evaluate_inclusion(
    inclusion: &dyn BackupInclusionSource,
    destination_nest_url: &str,
    custody: &[CustodyItem],
    retained: &[GenerationItem],
    attached_mirror_sets: &AttachedMirrorSets,
    now: i64,
) -> AuditVerdict {
    // Pin-less: with nothing remembered no regression can be observed, so the
    // vouch is never consulted. The production pass goes through
    // [`evaluate_inclusion_pinned`]; this is the arm the population-anchor
    // proofs exercise.
    evaluate_inclusion_pinned(
        inclusion,
        destination_nest_url,
        custody,
        retained,
        attached_mirror_sets,
        now,
        &BTreeMap::new(),
        &BTreeMap::new(),
        fauna_protocol::backup::BACKUP_CUSTODY_GRACE_SECS,
        &NoSourceVouch,
    )
    .await
    .verdict
}

/// What [`evaluate_inclusion_pinned`] hands back: the verdict, and the pins
/// to persist — the ones passed in, advanced by every ledger that opened this
/// pass and re-based by every accepted regression — and the accepted
/// regressions that **stand** after it, for the caller to persist in place of
/// the ones it passed in ([`DestinationAuditState::accepted_regressions`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InclusionOutcome {
    pub verdict: AuditVerdict,
    pub ledger_generations: BTreeMap<String, LedgerPin>,
    pub accepted_regressions: BTreeMap<String, AcceptedRegression>,
}

/// [`evaluate_inclusion`] with the **generation pin**: `pins` is what this device remembers verifying at this destination
/// ([`DestinationAuditState::verified_ledger_generations`]), and a ledger
/// served **below** its pin is a certain miss unless `source` — the owner's
/// own nest — vouches that it, not the destination, went backwards
/// ([`SourceLedgerVouch`] owns the rule).
///
/// What the population anchor could not see: a hostile destination rolling a
/// whole set back to an older **genuine** state — an older sealed ledger plus
/// exactly the segments it names, every stamp forged fresh — passes the
/// population check, because everything the older ledger names is there.
/// The generation is what the older ledger cannot fake: it was written by the
/// source under a counter that never decreases, so a device that verified a
/// higher one knows the served ledger is not the newest this destination held.
///
/// Every opened ledger advances its pin to the generation served, whatever the
/// sampling found — a sealed ledger is the source's own statement, and the
/// verdict is about the bytes, not the ledger's authenticity. A pinned ledger
/// the destination does not list live is judged by the vanished-ledger rule
/// ([`reconcile_with_ledgers`]): anchored from its retained generation while
/// one exists, a certain miss while it should still exist, dropped once the
/// grace window has passed since this device last saw it live. `grace_secs`
/// is the window the destination reported beside its retained generations,
/// floored at the protocol's own
/// ([`fauna_protocol::backup::BACKUP_CUSTODY_GRACE_SECS`]) — the audited
/// party's word cannot shorten the window its own retention is judged by.
///
/// `accepted` is the recovery notice's record as the last pass left it
/// ([`DestinationAuditState::accepted_regressions`]). A regression accepted
/// this pass **floors the source's counter** to the pinned generation
/// ([`SourceLedgerVouch::floor_counter`]) and joins it; every record is then
/// re-evaluated against this pass's own reads ([`lost_custody`]) — kept, with
/// its deadline refreshed, while the destination still holds something the
/// source lost, dropped when nothing remains — and a standing record whose
/// floor has not landed asks again.
#[allow(clippy::too_many_arguments)]
pub async fn evaluate_inclusion_pinned(
    inclusion: &dyn BackupInclusionSource,
    destination_nest_url: &str,
    custody: &[CustodyItem],
    retained: &[GenerationItem],
    attached_mirror_sets: &AttachedMirrorSets,
    now: i64,
    pins: &BTreeMap<String, LedgerPin>,
    accepted: &BTreeMap<String, AcceptedRegression>,
    grace_secs: i64,
    source: &dyn SourceLedgerVouch,
) -> InclusionOutcome {
    let mut ledger_generations = pins.clone();
    if custody.is_empty() && pins.is_empty() {
        // Nothing claimed and nothing remembered ⇒ nothing to disprove. An
        // empty destination is a freshness question, and freshness has
        // already answered it. A pinned ledger, though, is this device's own
        // claim about what the destination held, and an empty list does not
        // answer it — the vanished-ledger rule below does.
        return InclusionOutcome {
            verdict: AuditVerdict::Passed,
            ledger_generations,
            accepted_regressions: BTreeMap::new(),
        };
    }

    let fetcher = inclusion.fetcher(destination_nest_url);
    let keys = inclusion.keys();
    let seed = now as u64;

    // The population is NOT the destination's list: every
    // reserved-set ledger the destination lists is opened, and what it names
    // must be custody here. De-listed paths are missing with certainty; the
    // retained generations standing in for just-compacted paths join the
    // sampling population below. The covered-folder mirror plane has no
    // ledger; its anchor is the client's own replica index, reconciled by the
    // same rule into the same tallies.
    let mut ledger = reconcile_with_ledgers(
        fetcher.as_ref(),
        &keys,
        custody,
        retained,
        pins,
        now,
        grace_secs.max(fauna_protocol::backup::BACKUP_CUSTODY_GRACE_SECS),
    )
    .await;
    reconcile_with_folder_indexes(
        inclusion,
        custody,
        retained,
        attached_mirror_sets,
        now,
        &mut ledger,
    );
    let mut sampled: u32 = ledger.certain_misses;
    let mut missing: u32 = ledger.certain_misses;
    ledger_generations.extend(ledger.generations);
    for key in &ledger.dropped_pins {
        ledger_generations.remove(key);
    }

    // A ledger served below its pin: ask the source which party went
    // backwards (`SourceLedgerVouch`'s three answers).
    let mut records = accepted.clone();
    let mut floor_asked: BTreeSet<&str> = BTreeSet::new();
    for regression in &ledger.regressions {
        let accepted = match regression.kind_tag {
            None => false,
            Some(tag) => match source.next_segment_id(tag, regression.scope_hex).await {
                Ok(g) => g < regression.pinned,
                Err(_) => false,
            },
        };
        if accepted {
            ledger_generations.insert(
                regression.key.clone(),
                LedgerPin {
                    generation: regression.served,
                    seen_live_at: regression.seen_live_at,
                },
            );
            // The floor at acceptance: the source's counter is raised to the
            // generation this device PINNED — never the served one, which is
            // the destination's word — so the ids of the segments it lost are
            // spent and the copy's not-yet-reused ones stop being overwritten.
            // `accepted` implies a serve tag (the vouch was asked under it).
            let mut floored_at = None;
            if let Some(tag) = regression.kind_tag {
                floor_asked.insert(regression.key.as_str());
                if source
                    .floor_counter(tag, regression.scope_hex, regression.pinned)
                    .await
                    .is_ok()
                {
                    floored_at = Some(now);
                }
            }
            // The recovery notice's record; a second rollback of the same key
            // replaces the first. Whether it stands, and its deadline, are
            // the standing rule's below.
            records.insert(
                regression.key.clone(),
                AcceptedRegression {
                    pinned: regression.pinned,
                    served: regression.served,
                    observed_at: now,
                    floored_at,
                    recoverable_until: None,
                },
            );
        } else {
            sampled += 1;
            missing += 1;
        }
    }

    // The standing rule: each record is judged against the ledger this pass
    // opened for its key. A key whose ledger did not open this pass keeps its
    // record as it is for as long as the pin does (an unreadable or hidden
    // ledger is a miss above, not a recovery), and loses it with the pin.
    let mut accepted_regressions: BTreeMap<String, AcceptedRegression> = BTreeMap::new();
    for (key, mut record) in records {
        let Some(opened) = ledger.opened.get(&key) else {
            if ledger_generations.contains_key(&key) {
                accepted_regressions.insert(key, record);
            }
            continue;
        };
        // The deadline is the destination's OWN `grace_secs`, not the floored
        // figure the vanished-ledger rule judges retention by: the notice says
        // how long the copy will actually hold what the source lost.
        let Some(recoverable_until) =
            lost_custody(opened, record.pinned, custody, retained, grace_secs, now)
        else {
            continue;
        };
        record.recoverable_until = recoverable_until;
        if record.floored_at.is_none()
            && !floor_asked.contains(key.as_str())
            && let Some(tag) = opened.kind_tag
            && source
                .floor_counter(tag, opened.scope_hex, record.pinned)
                .await
                .is_ok()
        {
            record.floored_at = Some(now);
        }
        accepted_regressions.insert(key, record);
    }

    let mut scopes: BTreeMap<(&str, &str), Vec<&CustodyItem>> = BTreeMap::new();
    for item in custody
        .iter()
        .filter(|item| !ledger.failed_ledgers.contains(&item.path_hash))
        .chain(ledger.retained_rows.iter())
    {
        scopes.entry(scope_key(item)).or_default().push(item);
    }

    for items in scopes.values() {
        for idx in sample_indices(items.len(), AUDIT_SAMPLE_K, seed) {
            let item = items[idx];
            sampled += 1;
            // Route by custody plane, never by what happens to open: a reserved
            // segment set is sealed under the root these keys ARE, so it takes
            // the full open; the ordinary-folder mirror plane is the folder's
            // own ciphertext and takes hash-verified presence. Both failures
            // count the same — a hash mismatch and an unproducible path are
            // failures, never skipped samples.
            //
            // The decision is the CLIENT'S: the name must both look like a
            // mirror set and be one this client actually attached. Reading the
            // shape alone off the reply let the audited party pick which exam it
            // sat — see this function's doc.
            let verified = if fauna_core::data::is_folder_mirror_set(&item.folder_name)
                && attached_mirror_sets.contains_key(&item.folder_name)
            {
                verify_sampled_record_presence(fetcher.as_ref(), item).await
            } else {
                open_sampled_record(fetcher.as_ref(), &keys, item).await
            };
            if verified.is_err() {
                missing += 1;
            }
        }
    }

    let verdict = if missing > 0 {
        AuditVerdict::InclusionFailure { missing, sampled }
    } else {
        AuditVerdict::Passed
    };
    InclusionOutcome {
        verdict,
        ledger_generations,
        accepted_regressions,
    }
}

/// What reconciling a destination's list against the independent population
/// found — a reserved set's own sealed ledger ([`reconcile_with_ledgers`]) or
/// a covered folder's client-held index ([`reconcile_with_folder_indexes`]),
/// both feeding these same tallies so the two planes share one rule and one
/// verdict.
#[derive(Debug, Default)]
struct LedgerReconciliation<'a> {
    /// Paths the anchor names that the destination neither lists live nor
    /// retains — plus one per ledger it could not produce, open or decode.
    /// Each is a miss found without sampling.
    certain_misses: u32,
    /// Retained generations standing in for anchor-named paths the destination
    /// no longer lists live — the compaction window, or a covered folder's
    /// superseded head. They hold bytes and are restorable, so they are
    /// custody, and they join the sampling population so those bytes get
    /// checked like any live row's.
    retained_rows: Vec<CustodyItem>,
    /// `path_hash` of every ledger row already counted as a certain miss, so
    /// the sampler does not count the same failure twice.
    failed_ledgers: BTreeSet<String>,
    /// The pin each opened ledger leaves, by [`ledger_key`] — the generation
    /// it was served at and its live sighting — minus the regressed ones
    /// below.
    generations: BTreeMap<String, LedgerPin>,
    /// Ledgers served **below** their pin, for the source to settle.
    regressions: Vec<LedgerRegression<'a>>,
    /// Pins of ledgers that legitimately vanished — unseen live for the whole
    /// grace window, nothing of their set left — for the caller to forget.
    dropped_pins: Vec<String>,
    /// Every ledger that opened this pass, by [`ledger_key`] — what an
    /// accepted regression's standing rule is judged against.
    opened: BTreeMap<String, OpenedLedger<'a>>,
}

/// One reserved-set ledger a pass opened — listed live, or the newest retained
/// generation standing in for a de-listed one — as the standing rule of an
/// accepted regression reads it ([`lost_custody`]).
#[derive(Debug)]
struct OpenedLedger<'a> {
    folder_name: &'a str,
    scope_hex: &'a str,
    family: SegmentFamily,
    /// The serve tag the source lists this family under; `None` as for
    /// [`LedgerRegression::kind_tag`].
    kind_tag: Option<&'static str>,
    /// Every segment id the ledger names live.
    named: BTreeSet<u32>,
}

/// What a destination still holds, for one ledger's (kind-scope, family), of
/// what the source lost in an accepted regression from generation `pinned` —
/// the **standing rule** of the recovery notice's record (`backup-restore.md`
/// § Background Tasks → *Implementation status (audit loop)*, the
/// accepted-regression bullet's revision). Read from the inclusion arm's own
/// reads: the destination's live list, its retained generations, and the
/// source's own sealed ledger.
///
/// Lost custody is a segment row numbered **below `pinned`** — an id the copy
/// held before the regression — that is either
///
/// - **live and unnamed**: listed live, yet the source's live ledger does not
///   name it. The source rolled its ledger back with its data and never
///   learns the row exists, so nothing tombstones it; it stays until the
///   source's climbing counter reuses its id, which the floor at acceptance
///   stops. No clock reclaims it; or
/// - **retained and superseded**: a generation another generation of the same
///   path replaced — the source reused the id and overwrote it. It runs on
///   the grace clock from its `superseded_at`. The list does not say why a
///   generation was retired, and does not need to: a path's newest retained
///   generation was *tombstoned* exactly when no live row stands at the path
///   (compaction's own retirement, or the recovery's post-recovery duty —
///   not loss), and every older generation of a path was replaced by the next.
///
/// `None` — nothing of either sort remains (recovered, reclaimed): the record
/// is pruned. `Some(until)` — it stands; `until` is the earliest unexpired
/// retained expiry (`superseded_at` + the destination's own `grace_secs`),
/// absent when only live rows remain. A retained generation already past its
/// expiry is as good as reclaimed and holds nothing up.
fn lost_custody(
    ledger: &OpenedLedger<'_>,
    pinned: u32,
    custody: &[CustodyItem],
    retained: &[GenerationItem],
    grace_secs: i64,
    now: i64,
) -> Option<Option<i64>> {
    let lost_id = |folder_name: &str, path: Option<&str>| -> Option<u32> {
        if folder_name != ledger.folder_name {
            return None;
        }
        let (id, _) = ledger.family.parse(ledger.scope_hex, path?)?;
        (id < pinned).then_some(id)
    };

    let mut live_paths: BTreeSet<&str> = BTreeSet::new();
    let mut live_unnamed = false;
    for item in custody {
        if let Some(id) = lost_id(&item.folder_name, item.path.as_deref()) {
            live_paths.extend(item.path.as_deref());
            live_unnamed |= !ledger.named.contains(&id);
        }
    }

    let mut by_path: BTreeMap<&str, Vec<i64>> = BTreeMap::new();
    for g in retained {
        if lost_id(&g.folder_name, g.path.as_deref()).is_some()
            && let Some(path) = g.path.as_deref()
        {
            by_path.entry(path).or_default().push(g.superseded_at);
        }
    }
    let mut earliest: Option<i64> = None;
    for (path, mut stamps) in by_path {
        if !live_paths.contains(path) {
            // The newest generation of a path with no live row was tombstoned.
            stamps.sort_unstable();
            stamps.pop();
        }
        for superseded_at in stamps {
            let until = superseded_at.saturating_add(grace_secs);
            if until > now && earliest.is_none_or(|e| until < e) {
                earliest = Some(until);
            }
        }
    }

    (live_unnamed || earliest.is_some()).then_some(earliest)
}

/// One ledger served below the generation this device last verified.
#[derive(Debug)]
struct LedgerRegression<'a> {
    key: String,
    /// The serve tag the source lists this family under (`mail`,
    /// `mail-placement`, …); `None` for a family the kind does not have, which
    /// no honest ledger carries and nothing can ask about.
    kind_tag: Option<&'static str>,
    scope_hex: &'a str,
    served: u32,
    pinned: u32,
    /// The sighting the re-based pin keeps if the source vouches: this pass,
    /// for a ledger listed live; the old one, for a retained stand-in.
    seen_live_at: Option<i64>,
}

/// Anchor the inclusion population in each reserved set's own owner-sealed
/// ledger, never in the destination's list alone.
///
/// **Why the list cannot be the population.** `custody` is the reply of
/// `fauna.backup.custody.list`, answered by the destination being audited. A
/// destination that dropped older custody *and stopped listing it*, keeping
/// only recent rows fresh, used to pass both arms: freshness read the max
/// `updated_at` of that list and inclusion sampled from it. The ratified
/// "≈ 81 % per pass" held only for rows that stayed listed with their bytes
/// lost.
///
/// **What the destination cannot shrink.** The source nest writes the
/// `manifest.<kind>` mirror — a [`LiveManifestMirror`] naming every live
/// segment's `.dat` and, when the ref carries a sidecar hash, its `.meta` —
/// **last** in every pass that moved content
/// (`bins/fauna-nest/src/segment_backup.rs`), sealed under the owner's
/// `NestBackupKey`. The destination serves it but can neither author, edit nor
/// forge it, so it is the source's own upload ledger for the set, carried to
/// this reader with no new wire kind and no trust in the source's *live* word.
/// One per (kind-scope, family): the content segments' and the placement
/// journal's each have their own ([`SegmentFamily::mirror_path`]).
///
/// **The rule.** For every ledger row the destination lists in a reserved
/// set, open it under the owner's key and require every path it names to be
/// custody at this destination:
///
/// - **live** — listed in `custody`; sampled as before;
/// - **retained** — a generation inside the grace window `T` (`retained`, the
///   destination's `fauna.backup.generation.list`). This is the compaction
///   window: the coordinator tombstones compacted-out paths *first* and
///   re-uploads the mirror *last*, so for the span of one pass the live mirror
///   legitimately names paths that were just tombstoned. A retained generation
///   still holds the bytes and is restorable, so it counts — and its bytes join
///   the sampling population rather than being taken on the row's word;
/// - **neither** — missing with certainty. No sample is needed, so a de-listed
///   path is caught at 100 %, not 81 %.
///
/// A ledger the destination cannot produce, or that does not open or decode,
/// fails its set outright: a set without a readable ledger is unverifiable,
/// and hiding the ledger is exactly the de-listing move. Live rows the ledger
/// does not name yet (a segment uploaded moments ago, ahead of its mirror) stay
/// in the population and are sampled as before. The covered-folder mirror plane
/// has no such ledger; its anchor is the client's own replica index, applied by
/// [`reconcile_with_folder_indexes`] under the same rule.
///
/// **Which rows are ledgers is the client's decision, closed over
/// [`fauna_protocol::segments::BACKUP_SET_KINDS`]** ([`SegmentFamily::parse_mirror_path`]):
/// a custody path is the audited party's own text, and a row that merely looks
/// like a mirror must not make this reader decode an arbitrary blob as a set's
/// ledger. Such a row is an ordinary row: sampled and opened like any other.
///
/// **The ledger's generation is pinned per device** (2026-09-28): each opened ledger's `next_segment_id_seen` is
/// reported back for the caller to remember, and one served below `pins` is
/// reported as a regression — a rollback to an older genuine ledger unless the
/// source vouches otherwise ([`evaluate_inclusion_pinned`]).
///
/// **The vanished-ledger rule** (2026-09-29). Both of the above fire only for a ledger row the destination
/// *lists*, so a destination could de-list the ledger row itself — the
/// de-listing move aimed at the anchor instead of the history — and leave the
/// set unanchored, sampled at 81 % from its own list, with the pin silent. A
/// pin is this device's own claim that the ledger existed, so a pinned key
/// the destination does not list live is judged, in order:
///
/// - **retained** — the destination still holds a generation of the ledger
///   row (`retained`): the newest one is opened and anchors the set exactly
///   as a live ledger would, generation pin included. This is what a set the
///   source tore down looks like for the whole grace window `T` (the
///   coordinator tombstones per path; a writer's tombstone is retained for
///   `T`), and also what a destination that hid the live ledger but kept its
///   history looks like — either way the newest retained ledger names what
///   must still be custody. The pin stays, its live sighting unchanged;
/// - **still live below** — no generation of the ledger anywhere, yet the
///   destination lists segment rows of the same (kind-scope, family): an
///   honest teardown removes the whole set together and an honest upload
///   writes the ledger last within one pass, so a family with live segments
///   and no ledger, live or retained, for a set this device has *seen*
///   ledgered, is missing its anchor with certainty. The pin stays, so the
///   alarm holds for as long as the rows do;
/// - **inside the window** — nothing of the family left, but this device saw
///   the ledger live less than `grace_secs` ago: the destination's own
///   retention promised to hold a tombstoned generation for that long, so
///   the absence is an early reclaim or a de-listing — a certain miss, the
///   pin kept, re-judged every pass until the window lapses;
/// - **past the window** — the set is gone and this device last saw its
///   ledger live at least `grace_secs` ago: a teardown this device slept
///   through, its generations legitimately reclaimed. Accepted, and the pin
///   is dropped. The loss window this leaves is `T` itself — the bound the
///   whole design already accepts — and no owner gesture is needed: the
///   owner could judge a vanished set no better than the pass can.
///
/// The clocks compare cleanly across machines: the sighting is stamped by
/// this device at a pass during which the destination still listed the row,
/// and the destination reclaims `T` after its own later tombstone, so the
/// interval measured here is never shorter than `T` on an honest pair. A pin
/// with no sighting on record ([`LedgerPin::seen_live_at`] `None`) counts as
/// past the window.
async fn reconcile_with_ledgers<'a>(
    fetcher: &dyn BlobFetcher,
    keys: &FileDownloadKeys,
    custody: &'a [CustodyItem],
    retained: &'a [GenerationItem],
    pins: &BTreeMap<String, LedgerPin>,
    now: i64,
    grace_secs: i64,
) -> LedgerReconciliation<'a> {
    let live: BTreeSet<(&str, &str)> = custody
        .iter()
        .filter_map(|i| i.path.as_deref().map(|p| (i.folder_name.as_str(), p)))
        .collect();
    // The newest retained generation per (set, path): the one the tombstone
    // retired, which is what a just-compacted path's bytes rest as.
    let mut retained_by_path: BTreeMap<(&str, &str), &'a GenerationItem> = BTreeMap::new();
    for g in retained {
        let Some(path) = g.path.as_deref() else {
            continue;
        };
        retained_by_path
            .entry((g.folder_name.as_str(), path))
            .and_modify(|held| {
                if g.superseded_at > held.superseded_at {
                    *held = g;
                }
            })
            .or_insert(g);
    }

    let mut out = LedgerReconciliation::default();
    // Every ledger key the live list accounts for, plus every (set, scope,
    // family) the live list holds segment rows of — the vanished-ledger
    // rule's two live-side facts.
    let mut listed_ledgers: BTreeSet<String> = BTreeSet::new();
    let mut families_with_live_segments: BTreeSet<(&str, &str, SegmentFamily)> = BTreeSet::new();
    for item in custody {
        if fauna_core::data::is_folder_mirror_set(&item.folder_name) {
            continue;
        }
        let Some(path) = item.path.as_deref() else {
            continue;
        };
        let (_, scope) = scope_key(item);
        let Some((family, kind)) = SegmentFamily::PASS_ORDER
            .iter()
            .copied()
            .find_map(|family| {
                family
                    .parse_mirror_path(scope, path)
                    .map(|kind| (family, kind))
            })
        else {
            for family in SegmentFamily::PASS_ORDER {
                if family.parse(scope, path).is_some() {
                    families_with_live_segments.insert((item.folder_name.as_str(), scope, family));
                }
            }
            continue;
        };
        listed_ledgers.insert(ledger_key(&item.folder_name, path));

        let Ok(mirror) = open_ledger(fetcher, keys, item).await else {
            out.certain_misses += 1;
            out.failed_ledgers.insert(item.path_hash.clone());
            continue;
        };
        anchor_on_ledger(
            &mut out,
            &mirror,
            &item.folder_name,
            path,
            scope,
            family,
            kind,
            pins,
            Some(now),
            &live,
            &retained_by_path,
        );
    }

    for (key, pin) in pins {
        if listed_ledgers.contains(key) {
            continue;
        }
        // Retained: the newest generation of the ledger row anchors the set.
        let newest_retained = retained_by_path.iter().find_map(|((folder, path), g)| {
            (ledger_key(folder, path) == *key).then_some((*folder, *path, *g))
        });
        if let Some((folder, path, g)) = newest_retained {
            // The kind-scope is the path's first component, as `scope_key`
            // reads it off a custody row of the same set.
            let scope = path.split('/').next().unwrap_or("");
            let Some((family, kind)) =
                SegmentFamily::PASS_ORDER
                    .iter()
                    .copied()
                    .find_map(|family| {
                        family
                            .parse_mirror_path(scope, path)
                            .map(|kind| (family, kind))
                    })
            else {
                continue;
            };
            let stand_in = retained_as_custody(g);
            match open_ledger(fetcher, keys, &stand_in).await {
                Ok(mirror) => anchor_on_ledger(
                    &mut out,
                    &mirror,
                    folder,
                    path,
                    scope,
                    family,
                    kind,
                    pins,
                    pin.seen_live_at,
                    &live,
                    &retained_by_path,
                ),
                Err(_) => out.certain_misses += 1,
            }
            continue;
        }
        // Absent from both lists: the set's own live rows, then the clock.
        let family_still_live =
            families_with_live_segments
                .iter()
                .any(|(folder, scope, family)| {
                    fauna_protocol::segments::BACKUP_SET_KINDS
                        .iter()
                        .any(|kind| ledger_key(folder, &family.mirror_path(scope, kind)) == *key)
                });
        let inside_window = pin
            .seen_live_at
            .is_some_and(|seen| now.saturating_sub(seen) < grace_secs);
        if family_still_live || inside_window {
            out.certain_misses += 1;
        } else {
            out.dropped_pins.push(key.clone());
        }
    }
    out
}

/// Anchor one reserved set's family in an opened ledger — listed live this
/// pass (`seen_live_at` = now) or the newest retained generation standing in
/// for a de-listed one (the sighting the pin already carries) — and settle
/// its generation against the pin ([`reconcile_with_ledgers`] owns the rule).
#[allow(clippy::too_many_arguments)]
fn anchor_on_ledger<'a>(
    out: &mut LedgerReconciliation<'a>,
    mirror: &LiveManifestMirror,
    folder_name: &'a str,
    path: &str,
    scope: &'a str,
    family: SegmentFamily,
    kind: &str,
    pins: &BTreeMap<String, LedgerPin>,
    seen_live_at: Option<i64>,
    live: &BTreeSet<(&str, &str)>,
    retained_by_path: &BTreeMap<(&str, &str), &'a GenerationItem>,
) {
    let key = ledger_key(folder_name, path);
    let served = mirror.next_segment_id_seen;
    out.opened.insert(
        key.clone(),
        OpenedLedger {
            folder_name,
            scope_hex: scope,
            family,
            kind_tag: family.serve_kind(kind),
            named: mirror.live.iter().map(|seg| seg.segment_id).collect(),
        },
    );
    match pins.get(&key) {
        Some(pin) if served < pin.generation => out.regressions.push(LedgerRegression {
            key,
            kind_tag: family.serve_kind(kind),
            scope_hex: scope,
            served,
            pinned: pin.generation,
            seen_live_at,
        }),
        _ => {
            out.generations.insert(
                key,
                LedgerPin {
                    generation: served,
                    seen_live_at,
                },
            );
        }
    }
    for seg in &mirror.live {
        let mut named = vec![family.dat_path(scope, seg.segment_id)];
        named.push(family.meta_path(scope, seg.segment_id));
        for named_path in named {
            let key = (folder_name, named_path.as_str());
            if live.contains(&key) {
                continue;
            }
            match retained_by_path.get(&key) {
                Some(g) => out.retained_rows.push(retained_as_custody(g)),
                None => out.certain_misses += 1,
            }
        }
    }
}

/// A retained generation as a sampling-population row: it holds bytes and is
/// restorable, so its bytes are checked like any live row's rather than taken
/// on the row's word.
fn retained_as_custody(g: &GenerationItem) -> CustodyItem {
    CustodyItem {
        folder_name: g.folder_name.clone(),
        path: g.path.clone(),
        path_hash: g.path_hash.clone(),
        manifest_hash: g.manifest_hash.clone(),
        size_bytes: g.size_bytes,
        updated_at: g.superseded_at,
        ..Default::default()
    }
}

/// Anchor the covered-folder mirror plane's population in the client's **own
/// replica index** of each attached folder, never in the destination's list
/// alone — [`reconcile_with_ledgers`]'s rule, one plane over.
///
/// **Why this plane needs its own anchor.** A reserved set's ledger is written
/// by the source nest and served by the destination; a covered folder's mirror
/// rows are the folder's own content-layer ciphertext, copied with no re-seal,
/// and nothing owner-sealed at the destination names them. So until now the
/// mirror plane's floor — hash-verified presence — sampled from the
/// destination's own list, and a destination that dropped part of a covered
/// folder *and de-listed it* passed. What the
/// destination cannot shrink here is what the attaching client holds itself:
/// the folder's live head as its synced replica records it, path by path with
/// each head's manifest hash — the same `(hex(path_hash), manifest_hash)` pair
/// the source's coordinator writes each mirror row under
/// (`bins/fauna-nest/src/segment_backup.rs::run_folder_once`). The shell
/// supplies it through [`BackupInclusionSource::folder_index`]; `None` is a
/// declared absence and leaves that set on the destination's list
/// ([`FolderIndexSource`]).
///
/// **The rule.** For every attached set with an index, every index entry
/// `(path, manifest)` must be custody at this destination:
///
/// - **live** — listed in `custody` under that path *with that manifest*;
///   sampled as before;
/// - **retained** — a generation of that path with that manifest (`retained`,
///   the destination's `fauna.backup.generation.list`): the head was
///   superseded since the replica recorded it and the older bytes rest in the
///   grace window. Custody, and its bytes join the sampling population;
/// - **not yet expected** — the source recorded the head, or the folder was
///   attached, less than [`FRESHNESS_SLACK_SECS`] ago: the coordinator's next
///   sweep may not have run. Not a miss — the same slack freshness grants the
///   reserved rails, and the same `added_at` floor;
/// - **neither** — missing with certainty: de-listed, substituted under a
///   manifest this client never recorded, or lagging the client by more than
///   the slack. No sample is needed, so it is caught at 100 %, not 81 %.
///
/// **What the client itself can vouch for.** The index is only an anchor
/// while the replica was known consistent with the source inside the same
/// slack ([`FolderIndex::consistent_at`]): a device asleep for weeks may hold
/// heads superseded so long ago that the destination legitimately no longer
/// retains them, and alarming on the client's own staleness would be a false
/// alarm the user cannot act on. Such a replica falls back to the list for
/// that set — a stated residual, not a failure. A replica fresh inside the
/// slack cannot name a head the destination has legitimately dropped, since
/// the grace window `T` is ≫ the slack by construction.
///
/// Rows the index does not name (a head another device recorded that this
/// replica has not pulled yet) stay in the population and are sampled as
/// before; a set the client never attached is never asked for an index.
fn reconcile_with_folder_indexes(
    inclusion: &dyn BackupInclusionSource,
    custody: &[CustodyItem],
    retained: &[GenerationItem],
    attached_mirror_sets: &AttachedMirrorSets,
    now: i64,
    out: &mut LedgerReconciliation<'_>,
) {
    // (set, path) → manifest the destination lists live under it.
    let live: BTreeMap<(&str, &str), &str> = custody
        .iter()
        .filter_map(|i| {
            i.path
                .as_deref()
                .map(|p| ((i.folder_name.as_str(), p), i.manifest_hash.as_str()))
        })
        .collect();
    // (set, path, manifest) → the newest retained generation holding exactly
    // those bytes.
    let mut retained_by_head: BTreeMap<(&str, &str, &str), &GenerationItem> = BTreeMap::new();
    for g in retained {
        let Some(path) = g.path.as_deref() else {
            continue;
        };
        retained_by_head
            .entry((g.folder_name.as_str(), path, g.manifest_hash.as_str()))
            .and_modify(|held| {
                if g.superseded_at > held.superseded_at {
                    *held = g;
                }
            })
            .or_insert(g);
    }

    for (set, added_at) in attached_mirror_sets {
        let Some(index) = inclusion.folder_index(set) else {
            continue;
        };
        if now.saturating_sub(index.consistent_at) > FRESHNESS_SLACK_SECS {
            continue;
        }
        for entry in &index.entries {
            let key = (set.as_str(), entry.path_hash_hex.as_str());
            if live.get(&key) == Some(&entry.manifest_hash_hex.as_str()) {
                continue;
            }
            if let Some(g) = retained_by_head.get(&(
                set.as_str(),
                entry.path_hash_hex.as_str(),
                entry.manifest_hash_hex.as_str(),
            )) {
                out.retained_rows.push(retained_as_custody(g));
                continue;
            }
            let expected_by = entry
                .recorded_at
                .max(*added_at)
                .saturating_add(FRESHNESS_SLACK_SECS);
            if now < expected_by {
                continue;
            }
            out.certain_misses += 1;
        }
    }
}

/// Fetch one reserved set's ledger from the destination, open it under the
/// owner's key and decode it — the same walk [`open_sampled_record`] runs, with
/// the bytes kept.
async fn open_ledger(
    fetcher: &dyn BlobFetcher,
    keys: &FileDownloadKeys,
    item: &CustodyItem,
) -> Result<LiveManifestMirror, String> {
    let Some(path) = item.path.as_deref() else {
        return Err(format!(
            "custody row {} carries no path — unverifiable",
            item.path_hash
        ));
    };
    let raw = hex::decode(&item.manifest_hash)
        .map_err(|e| format!("bad manifest hash hex for {path}: {e}"))?;
    let digest: [u8; 32] = raw
        .as_slice()
        .try_into()
        .map_err(|_| format!("manifest hash for {path} is not 32 bytes"))?;
    let bytes = fauna_core::file_download::download_file_bytes_by_manifest(
        fetcher,
        keys,
        ContentHash::from_digest_raw(digest),
        None,
        path,
    )
    .await
    .map_err(|e| format!("{path}: {e}"))?;
    LiveManifestMirror::from_bytes(&bytes)
        .map_err(|e| format!("{path}: ledger does not decode: {e}"))
}

/// The complete retained-generation read: walk `fauna.backup.generation.list`'s
/// pages by the server-minted `next_cursor` to absence, exactly as
/// [`read_full_custody`] walks the live list, returning the grace window `T`
/// the destination reports beside the rows.
///
/// Two consumers, one walk: the generations facet
/// (`crate::generations::list_retained_generations`) and the inclusion arm,
/// which needs the retained rows to tell a just-compacted path from a
/// de-listed one ([`reconcile_with_ledgers`]). Contract violations fail toward
/// "could not ask" for the reason [`read_full_custody`] gives.
pub async fn read_full_generations(
    seam: &dyn BackupNestSeam,
) -> Result<(i64, Vec<GenerationItem>), String> {
    let mut cursor: Option<String> = None;
    let mut grace_secs;
    let mut items: Vec<GenerationItem> = Vec::new();
    let mut pages_fetched: usize = 0;
    loop {
        let reply = seam.generation_list(cursor.clone()).await?;
        pages_fetched += 1;
        grace_secs = reply.grace_secs;
        let page_empty = reply.generations.is_empty();
        items.extend(reply.generations);
        match crate::cursor::advance_cursor(
            "generation.list",
            &cursor,
            page_empty,
            reply.next_cursor,
            pages_fetched,
        )? {
            crate::cursor::CursorStep::Done => break,
            crate::cursor::CursorStep::Next(next) => cursor = Some(next),
        }
    }
    Ok((grace_secs, items))
}

/// The complete live-custody read: walk `fauna.backup.custody.list`'s pages by
/// the server-minted `next_cursor` to **absence** (`transport.md` § Max frame
/// corollary — the same storm that floods `generation.list` must not blind the
/// read that warns about it, so both kinds page identically).
///
/// The drained last page carries no cursor, so a reply without one ends
/// the walk. Contract violations (a repeated cursor, an empty
/// page claiming more, or the walk running past
/// [`crate::cursor::MAX_PAGES_PER_DESTINATION_WALK`] pages — a destination
/// alternating cursors never repeats one, so it needs its own bound) return
/// `Err` — the caller escalates exactly as it would a failed connection,
/// failing toward "could not ask", never toward a partial custody set
/// silently passing as complete: a short set would understate the
/// destination's high-water and could hide the very rows a storm produced.
pub async fn read_full_custody(seam: &dyn BackupNestSeam) -> Result<Vec<CustodyItem>, String> {
    let mut cursor: Option<String> = None;
    let mut items: Vec<CustodyItem> = Vec::new();
    let mut pages_fetched: usize = 0;
    loop {
        let reply = seam.custody_list(cursor.clone()).await?;
        pages_fetched += 1;
        let page_empty = reply.items.is_empty();
        items.extend(reply.items);
        match crate::cursor::advance_cursor(
            "custody.list",
            &cursor,
            page_empty,
            reply.next_cursor,
            pages_fetched,
        )? {
            crate::cursor::CursorStep::Done => break,
            crate::cursor::CursorStep::Next(next) => cursor = Some(next),
        }
    }
    Ok(items)
}

/// Audit one destination over an already-open seam.
///
/// Ordering is deliberate: **the read is attempted first, and a real verdict
/// always beats "I don't know"**. Only a read we could not make escalates to
/// [`AuditVerdict::Overdue`] (see [`escalate_unreachable`]).
///
/// Freshness runs before inclusion because it is free (the custody reply is
/// already in hand) while inclusion costs a network fetch per sampled record; a
/// destination that has already failed freshness gains nothing from sampling.
/// **The last-passed clock advances only when both arms pass**, which is what
/// makes `backup-destination-last-audit-time` mean "fully verified at" rather
/// than "was reachable at".
#[allow(clippy::too_many_arguments)]
pub async fn audit_destination(
    seam: &dyn BackupNestSeam,
    inclusion: &dyn BackupInclusionSource,
    destination_nest_url: &str,
    inputs: &AuditInputs,
    prior: &DestinationAuditState,
    attached_mirror_sets: &AttachedMirrorSets,
    source: &dyn SourceLedgerVouch,
    now: i64,
) -> (AuditVerdict, DestinationAuditState) {
    let mut state = DestinationAuditState {
        destination_id: prior.destination_id.clone(),
        last_passed_at: prior.last_passed_at,
        last_attempt_at: Some(now),
        // The pins survive a pass that never reaches the ledgers (unreachable,
        // or a freshness failure); only the inclusion arm advances them.
        verified_ledger_generations: prior.verified_ledger_generations.clone(),
        // The recovery notice's records likewise: only the inclusion arm can
        // say whether the destination still holds what the source lost.
        accepted_regressions: prior.accepted_regressions.clone(),
        seat_settled_under: prior.seat_settled_under.clone(),
    };

    let custody = match read_full_custody(seam).await {
        Ok(items) => items,
        Err(error) => {
            // Pointedly does NOT refresh `last_passed_at` — that is what lets
            // sustained unreachability become overdue on its own.
            return (escalate_unreachable(prior, inputs, now, error), state);
        }
    };

    let mut verdict = evaluate_freshness(inputs, destination_high_water(&custody));
    if verdict == AuditVerdict::Passed {
        // The retained generations are the inclusion arm's second input — read only once freshness has passed, since a stale
        // destination gains nothing from sampling, and failing toward "could
        // not ask" exactly as the custody read does.
        let (grace_secs, retained) = match read_full_generations(seam).await {
            Ok(read) => read,
            Err(error) => return (escalate_unreachable(prior, inputs, now, error), state),
        };
        let outcome = evaluate_inclusion_pinned(
            inclusion,
            destination_nest_url,
            &custody,
            &retained,
            attached_mirror_sets,
            now,
            &prior.verified_ledger_generations,
            &prior.accepted_regressions,
            grace_secs,
            source,
        )
        .await;
        verdict = outcome.verdict;
        state.verified_ledger_generations = outcome.ledger_generations;
        // The records that stand after this pass: the prior ones re-judged,
        // this pass's acceptances joined, the recovered and reclaimed gone.
        state.accepted_regressions = outcome.accepted_regressions;
    }
    if verdict == AuditVerdict::Passed {
        state.last_passed_at = Some(now);
    }
    (verdict, state)
}

/// Audit every configured destination, skipping those inside the debounce.
///
/// `destinations` is the client's own pinned `fauna.state.backup` destination list,
/// source-untrusted by construction: a destination the source nest has
/// "forgotten" is still audited, and still alerts. Deriving the list from the
/// source's registry instead would let a hostile source silence the audit by
/// dropping a row.
///
/// ⚠ **The return value is the destinations that RAN, not the full picture.** A
/// destination inside its [`AUDIT_MIN_INTERVAL_SECS`] debounce is absent from
/// the result entirely — which is correct for "what did this pass do", and
/// wrong as a render source. A caller driving the UI must merge these outcomes
/// over its persisted state, or a debounced destination's standing alert would
/// vanish from the page every time the loop ran without it.
#[allow(clippy::too_many_arguments)]
pub async fn audit_all(
    connector: &dyn BackupDestinationConnector,
    inclusion: &dyn BackupInclusionSource,
    destinations: &[BackupDestination],
    prior: &[DestinationAuditState],
    local_high_water: Option<i64>,
    source_nest_url: &str,
    bound_source: &[u8; 32],
    now: i64,
) -> Vec<DestinationAuditOutcome> {
    let bound_hex = fauna_core::hex32::encode(bound_source);
    let mut out = Vec::new();
    // The owner's own nest, dialled only if some destination serves a ledger
    // below its pin (`SourceLedgerVouch`) — one seam for the whole pass.
    let source = LazySourceVouch::new(connector, source_nest_url);
    // One pass per DESTINATION, not per row: a destination with N covered
    // folders has N+1 rows sharing one `destination_id`, and
    // auditing each row would query the same destination N+1 times per cycle.
    // `attached_mirror_sets` below still reads the full, undeduped
    // `destinations` — it needs every coverage row to know which mirror sets
    // this client attached.
    for dest in fauna_core::data::distinct_destinations(destinations) {
        let dest = &dest;
        let prior_state = prior
            .iter()
            .find(|s| s.destination_id == dest.destination_id)
            .cloned()
            .unwrap_or_else(|| DestinationAuditState {
                destination_id: dest.destination_id.clone(),
                ..Default::default()
            });
        // A seat not yet settled under the bound identity is due now: the box
        // rotated, and every pass the carry waits is a pass its uploads are
        // refused.
        let carry_due = crate::seat_carry::carries_a_seat(dest)
            && prior_state.seat_settled_under.as_deref() != Some(bound_hex.as_str());
        if !carry_due && !should_run(&prior_state, now) {
            continue;
        }
        let inputs = AuditInputs {
            local_high_water,
            added_at: dest.added_at as i64,
        };
        let (verdict, state) = match connect_destination(connector, dest).await {
            Ok(seam) => {
                // The seat carry runs ahead of the freshness arm, over the
                // connection the audit is about to read: a seat carried here
                // is one this pass's custody read already sees renamed.
                let settled = if crate::seat_carry::carries_a_seat(dest) {
                    crate::seat_carry::carry_writer_seat(seam.as_ref(), bound_source, &source)
                        .await
                        .is_ok()
                } else {
                    false
                };
                let (verdict, mut state) = audit_destination(
                    seam.as_ref(),
                    inclusion,
                    &dest.destination_nest_url,
                    &inputs,
                    &prior_state,
                    // The plane routing's input is the client's OWN attachment
                    // record for this destination, never the reply it is about
                    // to audit.
                    &attached_mirror_sets(destinations, &dest.destination_id),
                    &source,
                    now,
                )
                .await;
                if settled {
                    state.seat_settled_under = Some(bound_hex.clone());
                }
                (verdict, state)
            }
            Err(error) => {
                // A destination that will not connect degrades only its own
                // row — one dead destination must never blank the others.
                let mut state = prior_state.clone();
                state.last_attempt_at = Some(now);
                (
                    escalate_unreachable(&prior_state, &inputs, now, error),
                    state,
                )
            }
        };
        out.push(DestinationAuditOutcome {
            destination_id: dest.destination_id.clone(),
            verdict,
            state,
        });
    }
    out
}

// ── client-local persistence, and the one call a shell makes ────────────────

/// What one destination's row renders from: the clocks plus the verdict that is
/// currently **standing** against it.
///
/// The verdict is persisted, not recomputed, and that is the whole point. See
/// [`audit_all`]'s ⚠: a destination inside its debounce window contributes no
/// outcome to a pass, so a shell rendering only the pass's results would blank
/// a standing alert every time the loop ran without that destination — the alarm
/// would flicker off precisely while the problem persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestinationAuditRecord {
    pub state: DestinationAuditState,
    /// The last verdict actually reached. `None` = never audited (a fresh
    /// enrollment), which renders "never" and is **not** an alert.
    pub verdict: Option<AuditVerdict>,
}

impl DestinationAuditRecord {
    /// A never-audited destination's record.
    pub fn never(destination_id: &str) -> Self {
        Self {
            state: DestinationAuditState {
                destination_id: destination_id.to_string(),
                ..Default::default()
            },
            verdict: None,
        }
    }

    /// The banner reason standing against this destination, if any — the shell's
    /// whole `backup-audit-alert` decision, so no client re-derives which
    /// verdicts are loud.
    pub fn alert_reason(&self) -> Option<BackupAuditAlertReason> {
        self.verdict.as_ref().and_then(|v| v.alert_reason())
    }

    /// Every banner reason standing against this destination at `now` — the
    /// shell's single `backup-audit-alert` answer (`docs/goal/ui/backups.md` §
    /// Audit-alert surface): the standing verdict's reason ([`Self::alert_reason`]),
    /// then at most **one** [`BackupAuditAlertReason::SourceRegressed`] — however
    /// many ledgers regressed together, one banner per destination. It counts
    /// down to the earliest deadline still ahead among the standing records,
    /// and carries none when the records that stand hold live rows only —
    /// nothing reclaims those, so the copy keeps them until recovered. A
    /// record whose deadline has passed says nothing until the next pass
    /// re-judges it (it is pruned, or stands on its live rows alone).
    pub fn alert_reasons(&self, now: i64) -> Vec<BackupAuditAlertReason> {
        let mut reasons: Vec<_> = self.alert_reason().into_iter().collect();
        let standing: Vec<Option<i64>> = self
            .state
            .accepted_regressions
            .values()
            .map(|r| r.recoverable_until)
            .filter(|until| until.is_none_or(|until| until > now))
            .collect();
        if !standing.is_empty() {
            reasons.push(BackupAuditAlertReason::SourceRegressed {
                left_secs: standing.iter().flatten().min().map(|until| until - now),
            });
        }
        reasons
    }
}

/// Everything the audit persists on this device, in one value.
///
/// Client-local, never on the account plane: it is this device's own evidence, and two
/// devices auditing the same destination hold independently valid records
/// (`docs/goal/ui/backups.md` § Audit-alert surface).
///
/// **Not recreatable, so never rewritten unread.** The generation pins, the
/// accepted regressions and the observed high-water are rollback evidence: a
/// re-audit cannot rebuild what this device once saw. An *absent* store is the
/// first-run state and loads as the default; an *unreadable* one — corrupt, or
/// written by a newer build in a shape this one cannot read — is left exactly
/// as it is: the pass still runs, from the default, and saves nothing
/// ([`run_audit_pass`], [`observe_thread_activity`];
/// `version-compatibility.md` § Dimension 1, `transport.md` § Rule 3 in full →
/// *The store around the enum*).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AuditStateSnapshot {
    pub records: Vec<DestinationAuditRecord>,
    /// Unix seconds of the newest message-kind record this client has ever
    /// **observed** — the source-untrusted side of the freshness comparison
    /// ([`AuditInputs::local_high_water`]).
    ///
    /// It is an observation the client wrote down, not a live read, and that is
    /// deliberate: any read of the source nest is the *audited party's* own
    /// word, so a hostile source answering "nothing new" would make freshness
    /// unfailable forever. What this device saw with its own eyes it cannot be
    /// argued out of afterwards. Monotonic — see [`observe_local_record`].
    pub observed_high_water: Option<i64>,
}

/// The client-local store behind [`AuditStateSnapshot`] — a file under the
/// client's data dir natively, `localStorage` on web.
///
/// Sync, not async, because both backings are: a small file read and a
/// `localStorage` get. Fallible because a caller that cannot persist should say
/// so in its logs.
///
/// **`load` keeps absent and unreadable apart:** a store that holds nothing yet
/// answers `Ok(default)`, and one that holds something it cannot read answers
/// `Err` — the signal every caller here takes as "never save over this"
/// ([`AuditStateSnapshot`]).
///
/// `MaybeSendSync` (`Send + Sync` natively, nothing on wasm) for the same reason
/// [`BackupNestSeam`] carries it: native apps drive the pass from a worker
/// runtime and hold the store across an await, while the browser is
/// single-threaded.
pub trait AuditStateStore: fauna_protocol::MaybeSendSync {
    fn load(&self) -> Result<AuditStateSnapshot, String>;
    fn save(&self, snapshot: &AuditStateSnapshot) -> Result<(), String>;
}

/// Record that this client has seen a message-kind record stamped `secs`,
/// advancing [`AuditStateSnapshot::observed_high_water`] if it is newer.
///
/// **Monotonic on purpose.** The high-water answers "what is the newest thing I
/// know exists?", so it may never move backwards — otherwise opening an old
/// conversation would erase the evidence that newer data exists and quietly
/// clear a genuine freshness failure. Returns whether anything changed, so a
/// caller can skip the write on the common no-op.
///
/// **Bounded by the observer's clock.** `now` is the audit's own clock
/// ([`crate::audit_clock::now_secs`] in production). The *incoming* stamp is
/// clamped to `now + OBSERVATION_MAX_FUTURE_SKEW_SECS` before the comparison —
/// a record stamped past that is evidence of the sender's clock, not of data
/// this device knows exists (see [`OBSERVATION_MAX_FUTURE_SKEW_SECS`]). The
/// clamp touches only the offered stamp, never a stored high-water, so the
/// monotonic rule above holds unchanged.
pub fn observe_local_record(snapshot: &mut AuditStateSnapshot, secs: i64, now: i64) -> bool {
    let secs = secs.min(now.saturating_add(OBSERVATION_MAX_FUTURE_SKEW_SECS));
    if secs <= 0 || snapshot.observed_high_water.is_some_and(|hw| hw >= secs) {
        return false;
    }
    snapshot.observed_high_water = Some(secs);
    true
}

/// The **one owner of the millisecond→second boundary** on the observation feed.
///
/// Every app's thread summary carries `last_activity_ms` in epoch
/// **milliseconds**; [`observe_local_record`] and
/// [`AuditStateSnapshot::observed_high_water`] are in epoch **seconds**. A
/// missing `/1000` reads ~50 years ahead, which makes every later freshness
/// comparison unfailable while still "persisting something" — a silent failure
/// no test on either side of the boundary can see, because both ends stay
/// perfectly self-consistent.
///
/// It is a named function rather than an inline division precisely because it
/// was previously re-derived in four shells (`fauna-ffi`, `fauna-wasm`, linux
/// and tui), under a comment in one of them claiming it happened "once … so it
/// is not left to four shells to each get right".
///
/// Truncating on purpose: the high-water only ever needs to name the second a
/// record falls in, and truncation keeps it monotonic with the raw stamp.
pub fn activity_secs(last_activity_ms: i64) -> i64 {
    last_activity_ms / 1_000
}

/// What folding one observation into an [`AuditStateStore`] did — the return of
/// [`observe_thread_activity`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ObservedActivity {
    /// Whether the fold advanced the high-water **and** persisted it. A shell
    /// keeping its own in-memory high-water can skip the next call on `false`.
    pub persisted: bool,
    /// The high-water the snapshot holds **after** the fold — what a shell with
    /// a render-path cache should seed that cache from, rather than from the
    /// value it just offered (a store already holding something newer must be
    /// what the fast path compares against).
    pub high_water: Option<i64>,
    /// Non-fatal degradations, in the order they happened: a store that would
    /// not load (nothing was folded or saved — see [`observe_thread_activity`])
    /// or a failed save.
    ///
    /// Returned as data rather than logged here because this crate carries no
    /// logging dependency, and because each shell logs in its own idiom.
    /// Nothing here is worth surfacing to the user: a missed observation costs
    /// at most a weaker freshness comparison until the next render.
    pub degradations: Vec<String>,
}

/// Fold one displayed-activity observation into `store` — **the one owner of
/// the load → observe → save fold** every client's observation feed performs.
///
/// Called from the conversation list's render, which is the one place a client
/// shows the user what it knows about the nest-originated message kinds, and
/// therefore the honest moment to say "I have seen a record this new".
///
/// Cheap on the common path: [`observe_local_record`] is monotonic, so a repeat
/// render of the same threads writes nothing at all.
///
/// A store that will not load is **left alone**: the observation is dropped and
/// the failure reported, never folded into a default snapshot and saved — that
/// save would erase the rollback evidence the unreadable file holds (the pins,
/// the accepted regressions, the high-water itself; [`AuditStateSnapshot`]).
/// One missed observation costs at most a weaker freshness comparison until a
/// build that can read the file observes again.
pub fn observe_thread_activity(
    store: &impl AuditStateStore,
    last_activity_ms: i64,
) -> ObservedActivity {
    let mut degradations = Vec::new();
    let mut snapshot = match store.load() {
        Ok(s) => s,
        Err(e) => {
            degradations.push(e);
            return ObservedActivity {
                persisted: false,
                high_water: None,
                degradations,
            };
        }
    };
    let mut persisted = false;
    let now = crate::audit_clock::now_secs();
    if observe_local_record(&mut snapshot, activity_secs(last_activity_ms), now) {
        match store.save(&snapshot) {
            Ok(()) => persisted = true,
            Err(e) => degradations.push(e),
        }
    }
    ObservedActivity {
        persisted,
        high_water: snapshot.observed_high_water,
        degradations,
    }
}

/// Forget the accepted regressions this device holds for one reserved set at
/// one destination — every ledger of the set, both families — and persist it.
/// The recovery's post-recovery duty: once the source has taken back what the
/// copy held, the notice has nothing left to say, and the driving device need
/// not wait for its next pass to find that out (`segment-backup-protocol.md`
/// § Client-device custodian (pull) → *Restore* → *Recovery into the lived-in
/// nest that regressed*, the post-recovery duties).
///
/// Returns whether anything was cleared; nothing is written when nothing was.
/// A store that will not load is left alone and the failure returned — the
/// pins it holds are rollback evidence ([`AuditStateSnapshot`]), and the next
/// pass prunes the record by the standing rule anyway.
pub fn clear_accepted_regressions(
    store: &dyn AuditStateStore,
    destination_id: &str,
    set_name: &str,
) -> Result<bool, String> {
    let mut snapshot = store.load()?;
    let prefix = ledger_key(set_name, "");
    let mut cleared = false;
    for record in &mut snapshot.records {
        if record.state.destination_id != destination_id {
            continue;
        }
        let before = record.state.accepted_regressions.len();
        record
            .state
            .accepted_regressions
            .retain(|key, _| !key.starts_with(&prefix));
        cleared |= record.state.accepted_regressions.len() != before;
    }
    if cleared {
        store.save(&snapshot)?;
    }
    Ok(cleared)
}

/// Fold one pass's outcomes over the persisted records, for the currently
/// configured destination set.
///
/// Three rules, each of which a naive "just render the outcomes" shell gets
/// wrong:
///
/// 1. **A destination that did not run keeps its record** — the debounce case
///    ([`audit_all`]'s ⚠). Its standing alert stays on screen.
/// 2. **A destination no longer configured is dropped**, even if it has a
///    standing alert. Removing a destination must clear its alarm; a banner for
///    a backup the user deliberately stopped is noise they cannot dismiss.
/// 3. **A newly configured destination gains a "never" record**, so the row can
///    render "never" instead of vanishing until its first pass completes.
///
/// The result is ordered like `destinations`, so the page's banner order and row
/// order agree without the shell sorting anything.
pub fn merge_outcomes(
    prior: &[DestinationAuditRecord],
    outcomes: &[DestinationAuditOutcome],
    destinations: &[BackupDestination],
) -> Vec<DestinationAuditRecord> {
    // One record per DESTINATION — `destinations` carries one row per
    // attached folder besides the enrollment row, and a naive per-row map emits that many identical
    // records for one `destination_id`.
    fauna_core::data::distinct_destinations(destinations)
        .iter()
        .map(|dest| {
            let id = dest.destination_id.as_str();
            match outcomes.iter().find(|o| o.destination_id == id) {
                Some(fresh) => DestinationAuditRecord {
                    state: fresh.state.clone(),
                    verdict: Some(fresh.verdict.clone()),
                },
                None => prior
                    .iter()
                    .find(|r| r.state.destination_id == id)
                    .cloned()
                    .unwrap_or_else(|| DestinationAuditRecord::never(id)),
            }
        })
        .collect()
}

/// One source regression standing on this device's own custodian store: a
/// pull pass refused because the source served a saved counter below the
/// ledger the store holds (`segment-backup-protocol.md` § Client-device
/// custodian (pull) → *The pull never tombstones against a source below its
/// copy*). The app-side form of the store's own record — the store's host
/// carries it out (the sync agent's pipe on a desktop, the in-process read on
/// a phone) and [`run_audit_pass`] folds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreSourceRegression {
    /// The held ledger's own path in the store — one per set and family.
    pub ledger: String,
    /// The generation of the ledger the store holds.
    pub held: u32,
    /// The lower counter the source served.
    pub served: u32,
    /// Unix seconds of the first pass that found the source there.
    pub observed_at: i64,
}

/// What this device's own custodian store reported, read by the shell from
/// the store's host and handed to [`run_audit_pass`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OwnCustodianStore {
    /// This device's stable sync device id — the id a client-device row names
    /// its custodian by ([`fauna_core::data::custodian_assignment_for`]).
    pub device_id: String,
    /// Every regression standing on the store's record; empty when none does.
    pub source_regressions: Vec<StoreSourceRegression>,
}

/// Fold this device's own custodian store's standing source regressions into
/// the record of the client-device row that assigns it, so
/// [`DestinationAuditRecord::alert_reasons`] yields
/// [`BackupAuditAlertReason::SourceRegressed`] with no deadline — nothing
/// reclaims what the store holds while its pull is refused, so the copy keeps
/// it *until recovered*.
///
/// The store's record is the whole truth for that row, so the fold
/// **replaces** the row's `accepted_regressions` every pass: a regression the
/// store has since cleared leaves with it. A client-device row has no address
/// and never reaches the inclusion arm, so nothing else writes that map; each
/// folded entry carries `floored_at: None` and `recoverable_until: None` — the
/// floor is the nest-destination record's
/// ([`SourceLedgerVouch::floor_counter`]), asked only by the inclusion arm.
///
/// No row assigning `own.device_id` (the shared matching rule's answer, two
/// rows naming it included) folds nothing.
pub fn fold_own_custodian_regressions(
    records: &mut [DestinationAuditRecord],
    destinations: &[BackupDestination],
    own: &OwnCustodianStore,
) {
    let distinct = fauna_core::data::distinct_destinations(destinations);
    let Some(assignment) = fauna_core::data::custodian_assignment_for(
        distinct.iter().map(BackupDestination::custodian_row),
        &own.device_id,
    ) else {
        return;
    };
    let Some(record) = records
        .iter_mut()
        .find(|r| r.state.destination_id == assignment.destination_id)
    else {
        return;
    };
    record.state.accepted_regressions = own
        .source_regressions
        .iter()
        .map(|r| {
            (
                r.ledger.clone(),
                AcceptedRegression {
                    pinned: r.held,
                    served: r.served,
                    observed_at: r.observed_at,
                    floored_at: None,
                    recoverable_until: None,
                },
            )
        })
        .collect();
}

/// Run one audit pass and persist it: **the single call a client shell makes.**
///
/// Load the client-local state, audit every destination that is due, merge the
/// outcomes over what was already known ([`merge_outcomes`]), write it back, and
/// hand the caller the full per-destination picture to render. Everything a
/// shell needs is in the returned records — no client re-implements the merge,
/// the debounce, or which verdicts are loud.
///
/// `now` is a parameter rather than a clock read so the whole loop stays
/// testable without waiting out a 48-hour slack window (testing.md convention
/// 14) — and so the same code path serves a client whose clock is being driven
/// by a test harness.
///
/// A store that will not load still gets a pass — run from the empty snapshot,
/// so the alerts it finds are shown now — but **nothing is saved over it**: the
/// unreadable file holds rollback evidence a re-audit cannot rebuild
/// ([`AuditStateSnapshot`]), and the failure is reported in
/// [`AuditPass::degradations`]. A store that will not save still returns the
/// pass's findings — the alert is shown now and simply re-derived next pass.
///
/// A record whose standing verdict this build cannot name
/// ([`AuditVerdict::Unknown`]) is due at once, whatever its debounce clock says:
/// the arm renders nothing, so leaving it standing for the debounce would show
/// a destination with no verdict at all.
///
/// `source_nest_url` is the owner's own nest — the one the app is signed into
/// — dialled through `connector` only when a destination serves a ledger below
/// this device's pin ([`SourceLedgerVouch`]); an honest pass never dials it.
///
/// `own_custodian` is this device's own custodian store as its host reported
/// it, folded into the client-device row that assigns this device after the
/// merge and before the save ([`fold_own_custodian_regressions`]) — on every
/// pass, due or debounced, since it costs no round trip. `None` is "no store
/// was read" (a shell with no custodian host, or a read that failed): the
/// row's record stands as the last pass left it, so a failed read never
/// switches a standing notice off.
///
/// `bound_source` is the identity this device is bound to at `source_nest_url`
/// — the one its pinned destination list rests under. Each destination's
/// writer seat is carried to it when a verified rotation chain links the
/// seat's holder there ([`crate::seat_carry`]), and a destination whose seat
/// is not yet settled under it is audited whatever its debounce says
/// ([`DestinationAuditState::seat_settled_under`]).
#[allow(clippy::too_many_arguments)]
pub async fn run_audit_pass(
    connector: &dyn BackupDestinationConnector,
    inclusion: &dyn BackupInclusionSource,
    store: &dyn AuditStateStore,
    destinations: &[BackupDestination],
    source_nest_url: &str,
    bound_source: &[u8; 32],
    own_custodian: Option<&OwnCustodianStore>,
    now: i64,
) -> AuditPass {
    let mut degradations = Vec::new();
    let (mut snapshot, readable) = match store.load() {
        Ok(snapshot) => (snapshot, true),
        Err(e) => {
            degradations.push(e);
            (AuditStateSnapshot::default(), false)
        }
    };
    let prior_states: Vec<DestinationAuditState> = snapshot
        .records
        .iter()
        .map(|r| {
            let mut state = r.state.clone();
            if matches!(r.verdict, Some(AuditVerdict::Unknown(_))) {
                state.last_attempt_at = None;
            }
            state
        })
        .collect();

    let outcomes = audit_all(
        connector,
        inclusion,
        destinations,
        &prior_states,
        snapshot.observed_high_water,
        source_nest_url,
        bound_source,
        now,
    )
    .await;

    snapshot.records = merge_outcomes(&snapshot.records, &outcomes, destinations);
    if let Some(own) = own_custodian {
        fold_own_custodian_regressions(&mut snapshot.records, destinations, own);
    }
    if readable && let Err(e) = store.save(&snapshot) {
        degradations.push(e);
    }
    AuditPass {
        records: snapshot.records,
        degradations,
    }
}

/// What one [`run_audit_pass`] produced: the full per-destination picture to
/// render, plus what went wrong around it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuditPass {
    /// One record per configured destination, ordered like the destination
    /// list ([`merge_outcomes`]).
    pub records: Vec<DestinationAuditRecord>,
    /// Non-fatal degradations, in the order they happened: a store that would
    /// not load (the pass ran from the empty snapshot and **saved nothing**,
    /// leaving the file as it was) and/or a failed save. Returned as data for
    /// each shell to log in its own idiom, like
    /// [`ObservedActivity::degradations`].
    pub degradations: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::{BackupNestSeam, DestinationConnection};
    use async_trait::async_trait;
    use fauna_client_testkit::block_on;
    use fauna_protocol::backup::{BackupStatusReply, CustodyListReply, WriterGrantListReply};
    use std::collections::HashMap;
    use std::sync::Arc;

    const DAY: i64 = 24 * 60 * 60;
    const HOUR: i64 = 60 * 60;
    /// A fixed "now" so no test depends on the wall clock.
    const NOW: i64 = 1_800_000_000;
    /// The identity this device is bound to at the source, in every pass.
    const BOUND: [u8; 32] = [0xB0; 32];

    fn bound_hex() -> String {
        fauna_core::hex32::encode(&BOUND)
    }

    fn custody(updated_at: i64) -> CustodyItem {
        CustodyItem {
            folder_name: "__mail".into(),
            path: Some("4242/seg-00000001.dat".into()),
            path_hash: "aa".repeat(32),
            manifest_hash: manifest_hash_hex(),
            size_bytes: 4096,
            updated_at,
            ..Default::default()
        }
    }

    fn inputs(local_high_water: Option<i64>, added_at: i64) -> AuditInputs {
        AuditInputs {
            local_high_water,
            added_at,
        }
    }

    fn state(destination_id: &str, last_passed_at: Option<i64>) -> DestinationAuditState {
        DestinationAuditState {
            destination_id: destination_id.into(),
            last_passed_at,
            last_attempt_at: None,
            verified_ledger_generations: Default::default(),
            accepted_regressions: Default::default(),
            // Settled under the pass's bound identity, so the debounce is
            // what decides — the carry's own trigger is pinned on its own.
            seat_settled_under: Some(bound_hex()),
        }
    }

    /// A destination that answers `custody_list` with canned rows, or an error.
    struct MockDestination {
        custody: Result<Vec<CustodyItem>, String>,
        /// What `generation.list` answers — empty for every test that is not
        /// about the compaction window.
        generations: Result<Vec<GenerationItem>, String>,
    }

    #[async_trait]
    impl BackupNestSeam for MockDestination {
        async fn status(&self) -> Result<BackupStatusReply, String> {
            panic!("the audit must never ask a destination for the source's status")
        }
        async fn nest_key_revoke(&self) -> Result<(), String> {
            panic!("the audit reports only — it never revokes")
        }
        async fn writer_grant_list(&self) -> Result<WriterGrantListReply, String> {
            // The seat carry's read, ahead of the freshness arm: no seat here
            // (the carry's own outcomes are pinned in `crate::seat_carry`).
            match &self.custody {
                Ok(_) => Ok(WriterGrantListReply::default()),
                Err(e) => Err(e.clone()),
            }
        }
        async fn writer_grant_revoke(&self, _: String) -> Result<bool, String> {
            panic!("the audit reports only — it never revokes")
        }
        async fn generation_list(
            &self,
            _cursor: Option<String>,
        ) -> Result<fauna_protocol::backup::GenerationListReply, String> {
            // The inclusion arm's second input: a single
            // complete page, like `custody_list` below.
            match &self.generations {
                Ok(items) => Ok(fauna_protocol::backup::GenerationListReply {
                    generations: items.clone(),
                    grace_secs: 30 * DAY,
                    next_cursor: None,
                    extra: Default::default(),
                }),
                Err(e) => Err(e.clone()),
            }
        }
        async fn generation_restore(
            &self,
            _: String,
            _: String,
            _: String,
        ) -> Result<fauna_protocol::backup::GenerationRestoreReply, String> {
            panic!("the audit reports only — it never restores")
        }
        async fn custody_list(&self, _cursor: Option<String>) -> Result<CustodyListReply, String> {
            // A single complete reply with no `next_cursor` — the drained last
            // page, which the walk must treat as drained after one page.
            match &self.custody {
                Ok(items) => Ok(CustodyListReply {
                    items: items.clone(),
                    next_cursor: None,
                    extra: Default::default(),
                }),
                Err(e) => Err(e.clone()),
            }
        }
    }

    fn reachable(items: Vec<CustodyItem>) -> MockDestination {
        MockDestination {
            custody: Ok(items),
            generations: Ok(Vec::new()),
        }
    }

    fn dead() -> MockDestination {
        MockDestination {
            custody: Err("connection refused".into()),
            generations: Err("connection refused".into()),
        }
    }

    // ── the inclusion arm's test doubles ────────────────────────────────────
    //
    // These drive the REAL `fauna_core::file_download` walk over an in-memory
    // blob store, so "missing" is a genuine fetch failure and "unopenable" is a
    // genuine whole-file-hash mismatch — not booleans a mock returns. A double
    // that simply answered "inclusion failed" would pass just as well against an
    // implementation that never fetched anything.

    const DEST_URL: &str = "https://destination.example";
    /// The plaintext bytes standing in for one backed-up segment.
    const PAYLOAD: &[u8] = b"one backed-up segment's bytes";

    /// A self-consistent one-chunk plaintext manifest for [`PAYLOAD`].
    ///
    /// Plaintext (`stored_hashes: None`) rather than sealed, because what these
    /// tests pin is the *sampling and verdict* logic; the seal path itself is
    /// `fauna-core`'s own covered territory, and the key material's necessity is
    /// pinned separately by `a_sealed_record_this_reader_holds_no_key_for_fails`.
    fn manifest_bytes() -> Vec<u8> {
        let manifest = fauna_core::chunk::ChunkManifest {
            file_hash: ContentHash::of_raw(PAYLOAD),
            total_size: PAYLOAD.len() as u64,
            chunk_hashes: vec![ContentHash::of_raw(PAYLOAD)],
            chunk_sizes: vec![PAYLOAD.len() as u64],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };
        fauna_core::encoding::canonical_encode(&manifest).expect("encode manifest")
    }

    /// The `SealedUnopenable` posture's manifest — a *different* document from
    /// [`manifest_bytes`], so it needs its own address. The walk now checks
    /// served bytes against the address asked for, so a custody row pointed at this
    /// posture must carry [`sealed_manifest_hash_hex`] — otherwise it fails on
    /// the address before it ever reaches the seal, and the test would pin the
    /// wrong mechanism.
    fn sealed_manifest_bytes() -> Vec<u8> {
        let manifest = fauna_core::chunk::ChunkManifest {
            file_hash: ContentHash::of_raw(PAYLOAD),
            total_size: PAYLOAD.len() as u64,
            chunk_hashes: vec![ContentHash::of_raw(PAYLOAD)],
            chunk_sizes: vec![PAYLOAD.len() as u64],
            stored_hashes: Some(vec![ContentHash::of_raw(b"ciphertext")]),
            sealed_hashes: None,
            min_reader: None,
        };
        fauna_core::encoding::canonical_encode(&manifest).expect("encode sealed manifest")
    }

    /// The address a custody row must carry to be served [`manifest_bytes`].
    fn manifest_hash_hex() -> String {
        hex::encode(ContentHash::of_raw(&manifest_bytes()).digest())
    }

    /// The address a custody row must carry to be served
    /// [`sealed_manifest_bytes`].
    fn sealed_manifest_hash_hex() -> String {
        hex::encode(ContentHash::of_raw(&sealed_manifest_bytes()).digest())
    }

    /// How a destination answers the blob routes. Each variant is one of the
    /// ratified failing states, or the healthy case.
    #[derive(Clone, Copy, PartialEq)]
    enum Blobs {
        /// Everything is there and intact.
        Intact,
        /// The manifest 404s — the record is simply gone.
        ManifestGone,
        /// The manifest resolves but the chunk bytes are not the ones it
        /// describes: present, unopenable.
        CorruptChunks,
        /// The manifest declares a seal this reader has no key for.
        SealedUnopenable,
        /// The **ordinary-folder mirror** plane, served intact: the manifest's
        /// store keys address content-layer CIPHERTEXT the auditor holds no key
        /// for, and the destination really has those bytes. Present and
        /// hash-matched; unopenable by construction, forever.
        MirrorPlane,
        /// The same plane with one chunk genuinely dropped — the failure that
        /// must still be caught once opening stops being the test.
        MirrorChunkDropped,
    }

    /// The mirrored bytes a covered folder rests as at the destination: the
    /// folder's own content-layer ciphertext, copied with no re-seal.
    const CIPHERTEXT: &[u8] = b"the folder's own content-layer ciphertext";

    /// A mirror manifest: `stored_hashes` (the store keys) address
    /// [`CIPHERTEXT`], while `file_hash` is the plaintext the auditor cannot
    /// reach. Opening it under the nest-backup root can only fail; presence is
    /// exactly what custody can still lose.
    fn mirror_manifest_bytes() -> Vec<u8> {
        let manifest = fauna_core::chunk::ChunkManifest {
            file_hash: ContentHash::of_raw(PAYLOAD),
            total_size: PAYLOAD.len() as u64,
            chunk_hashes: vec![ContentHash::of_raw(PAYLOAD)],
            chunk_sizes: vec![PAYLOAD.len() as u64],
            stored_hashes: Some(vec![ContentHash::of_raw(CIPHERTEXT)]),
            sealed_hashes: None,
            min_reader: None,
        };
        fauna_core::encoding::canonical_encode(&manifest).expect("encode mirror manifest")
    }

    /// One row of a covered folder's mirror. ⚠ Its path is `hex(path_hash)`
    /// with **no `/`** — the real layout, and the reason the old kind-scope
    /// parse made every file its own sampling stratum.
    fn mirror_custody(folder_set: &str, i: u32) -> CustodyItem {
        CustodyItem {
            folder_name: folder_set.into(),
            path: Some(format!("{i:064x}")),
            path_hash: format!("{i:064x}"),
            manifest_hash: hex::encode(ContentHash::of_raw(&mirror_manifest_bytes()).digest()),
            size_bytes: 4096,
            updated_at: NOW,
            ..Default::default()
        }
    }

    /// The client's own attachment record, as `evaluate_inclusion` takes it —
    /// each set attached a month ago, well outside the slack.
    ///
    /// Spelled out at each call site rather than derived from the custody rows
    /// under test: deriving it from the reply would reinstate exactly the
    /// defect these tests exist to hold closed.
    fn attached_to(sets: &[&str]) -> AttachedMirrorSets {
        sets.iter()
            .map(|s| (s.to_string(), NOW - 30 * DAY))
            .collect()
    }

    fn covered_folder_set() -> String {
        format!("__folder/{}/12", "ab".repeat(32))
    }

    struct MapFetcher {
        blobs: Blobs,
    }

    #[async_trait]
    impl BlobFetcher for MapFetcher {
        async fn fetch_manifest(&self, _hash: &ContentHash) -> anyhow::Result<Vec<u8>> {
            match self.blobs {
                Blobs::ManifestGone => anyhow::bail!("404 no such manifest"),
                Blobs::SealedUnopenable => Ok(sealed_manifest_bytes()),
                Blobs::MirrorPlane | Blobs::MirrorChunkDropped => Ok(mirror_manifest_bytes()),
                _ => Ok(manifest_bytes()),
            }
        }

        async fn fetch_chunks(
            &self,
            store_keys: &[ContentHash],
            _relative_path: &str,
        ) -> anyhow::Result<Vec<Vec<u8>>> {
            match self.blobs {
                Blobs::CorruptChunks => Ok(store_keys
                    .iter()
                    .map(|_| b"not the bytes the manifest describes".to_vec())
                    .collect()),
                // Present and hash-matched: the destination really holds the
                // ciphertext the manifest addresses.
                Blobs::MirrorPlane => Ok(store_keys.iter().map(|_| CIPHERTEXT.to_vec()).collect()),
                // Genuinely gone — the case that must still fail.
                Blobs::MirrorChunkDropped => anyhow::bail!("404 no such chunk"),
                _ => Ok(store_keys.iter().map(|_| PAYLOAD.to_vec()).collect()),
            }
        }
    }

    /// An inclusion source over one [`Blobs`] posture, recording which
    /// destination URL it was asked for.
    struct MockInclusion {
        blobs: Blobs,
        asked: std::sync::Mutex<Vec<String>>,
        keyed: bool,
    }

    impl MockInclusion {
        fn new(blobs: Blobs) -> Self {
            Self {
                blobs,
                asked: std::sync::Mutex::new(Vec::new()),
                keyed: true,
            }
        }
        /// A reader holding no owner key — the fail-closed case.
        fn keyless(blobs: Blobs) -> Self {
            Self {
                blobs,
                asked: std::sync::Mutex::new(Vec::new()),
                keyed: false,
            }
        }
    }

    impl BackupInclusionSource for MockInclusion {
        fn fetcher(&self, destination_nest_url: &str) -> Arc<dyn BlobFetcher> {
            self.asked
                .lock()
                .unwrap()
                .push(destination_nest_url.to_string());
            Arc::new(MapFetcher { blobs: self.blobs })
        }
        fn keys(&self) -> FileDownloadKeys {
            if self.keyed {
                FileDownloadKeys::owner(fauna_core::crypto::NestBackupKey::derive(&[7u8; 32]))
            } else {
                FileDownloadKeys::default()
            }
        }
        fn folder_index(&self, _folder_set: &str) -> Option<FolderIndex> {
            // The declared absence: these tests are about routing and
            // sampling, and the mirror plane's population stays the list.
            None
        }
    }

    /// The neutral inclusion source for tests that are about something else
    /// (freshness, overdue, the debounce, the merge): every sampled record is
    /// present and openable, so the inclusion arm never changes their verdict.
    fn all_present() -> MockInclusion {
        MockInclusion::new(Blobs::Intact)
    }

    // Thin wrappers so the freshness/overdue/merge tests below keep their exact
    // call shapes while the production signatures carry the inclusion seam.
    async fn audit_destination_t(
        seam: &dyn BackupNestSeam,
        inputs: &AuditInputs,
        prior: &DestinationAuditState,
        now: i64,
    ) -> (AuditVerdict, DestinationAuditState) {
        audit_destination(
            seam,
            &all_present(),
            DEST_URL,
            inputs,
            prior,
            &AttachedMirrorSets::new(),
            &NoSourceVouch,
            now,
        )
        .await
    }

    /// The owner's own nest, as the pass driver dials it. Never actually
    /// reached by a test that observes no ledger regression.
    const SOURCE_URL: &str = "https://home.example";

    async fn audit_all_t(
        connector: &dyn BackupDestinationConnector,
        destinations: &[BackupDestination],
        prior: &[DestinationAuditState],
        local_high_water: Option<i64>,
        now: i64,
    ) -> Vec<DestinationAuditOutcome> {
        audit_all(
            connector,
            &all_present(),
            destinations,
            prior,
            local_high_water,
            SOURCE_URL,
            &BOUND,
            now,
        )
        .await
    }

    async fn run_audit_pass_t(
        connector: &dyn BackupDestinationConnector,
        store: &dyn AuditStateStore,
        destinations: &[BackupDestination],
        now: i64,
    ) -> Vec<DestinationAuditRecord> {
        run_audit_pass(
            connector,
            &all_present(),
            store,
            destinations,
            SOURCE_URL,
            &BOUND,
            None,
            now,
        )
        .await
        .records
    }

    /// A custody row at `scope`, with an explicit path.
    fn custody_at(scope: &str, seg: u32, updated_at: i64) -> CustodyItem {
        CustodyItem {
            folder_name: "__mail".into(),
            path: Some(format!("{scope}/seg-{seg:08}.dat")),
            path_hash: format!("{seg:064x}"),
            manifest_hash: manifest_hash_hex(),
            size_bytes: 4096,
            updated_at,
            ..Default::default()
        }
    }

    // ── inclusion sampling ──────────────────────────────────────────────────

    // ── the ordinary-folder mirror plane ─────────────
    //
    // Mirror bytes rest at the destination as the folder's OWN content-layer
    // ciphertext, mirrored with no re-seal, while this arm's keys are the
    // nest-backup root — two deliberately domain-separated trees. Opening is
    // therefore impossible by construction, and the goal doc
    // (`backup-destinations.md` § Ordinary-folder coverage → *Retention +
    // audit*) sets hash-verified presence as this plane's floor.

    /// **The headline defect, inverted:** attaching one ordinary folder used to
    /// turn a byte-perfect destination permanently red. It must now pass —
    /// while the destination is genuinely intact.
    #[test]
    fn a_covered_folder_passes_the_destination_audit_it_cannot_open() {
        let set = covered_folder_set();
        let custody: Vec<CustodyItem> = (0..3).map(|i| mirror_custody(&set, i)).collect();
        // This client DID attach that set, so its rows route to the mirror plane.
        let attached = attached_to(&[&set]);
        let verdict = block_on(evaluate_inclusion(
            &MockInclusion::new(Blobs::MirrorPlane),
            DEST_URL,
            &custody,
            &[],
            &attached,
            NOW,
        ));
        assert_eq!(
            verdict,
            AuditVerdict::Passed,
            "a covered folder the auditor legitimately cannot open must not read as lost custody"
        );
    }

    /// The same, for a **shared** folder: its content key was never granted to
    /// this auditor at all, so it is even further out of reach than a private
    /// one — and the verdict must be identical. (Same plane, same routing: the
    /// set name is what decides, never what happens to open.)
    #[test]
    fn a_covered_shared_folder_passes_on_the_same_routing() {
        let set = format!("__folder/{}/77", "cd".repeat(32));
        let custody: Vec<CustodyItem> = (0..3).map(|i| mirror_custody(&set, i)).collect();
        // This client DID attach that set, so its rows route to the mirror plane.
        let attached = attached_to(&[&set]);
        let verdict = block_on(evaluate_inclusion(
            &MockInclusion::keyless(Blobs::MirrorPlane),
            DEST_URL,
            &custody,
            &[],
            &attached,
            NOW,
        ));
        assert_eq!(verdict, AuditVerdict::Passed);
    }

    /// **The load-bearing half.** A remedy that merely *skipped* mirror rows
    /// would pass the two tests above and quietly drop the coverage the goal
    /// doc's "a hash mismatch or an unproducible live path is a failure, never
    /// a skipped sample" clause requires. So: a destination that genuinely
    /// dropped a mirror chunk still FAILS, and the sampled count proves the
    /// rows were actually visited rather than skipped.
    #[test]
    fn a_dropped_mirror_chunk_still_fails_and_the_rows_are_not_skipped() {
        let set = covered_folder_set();
        let custody: Vec<CustodyItem> = (0..3).map(|i| mirror_custody(&set, i)).collect();
        // This client DID attach that set, so its rows route to the mirror plane.
        let attached = attached_to(&[&set]);
        let verdict = block_on(evaluate_inclusion(
            &MockInclusion::new(Blobs::MirrorChunkDropped),
            DEST_URL,
            &custody,
            &[],
            &attached,
            NOW,
        ));
        match verdict {
            AuditVerdict::InclusionFailure { missing, sampled } => {
                assert_eq!(sampled, 3, "every mirror row must be sampled, not skipped");
                assert_eq!(missing, 3, "each dropped chunk is a failure");
            }
            other => panic!("a destination missing mirror chunks must fail: {other:?}"),
        }
    }

    /// **The sampling blow-up.** A mirror record's path is `hex(path_hash)`
    /// with no `/`, so the kind-scope parse used to give every file its own
    /// stratum — sampling the WHOLE corpus every pass (a covered media library
    /// re-downloaded in full, forever). The set name is the stratum, so K means
    /// K per covered folder.
    #[test]
    fn one_covered_folder_is_one_sampling_stratum_not_one_per_file() {
        let set = covered_folder_set();
        let custody: Vec<CustodyItem> = (0..40).map(|i| mirror_custody(&set, i)).collect();
        // This client DID attach that set, so its rows route to the mirror plane.
        let attached = attached_to(&[&set]);
        let verdict = block_on(evaluate_inclusion(
            &MockInclusion::new(Blobs::MirrorChunkDropped),
            DEST_URL,
            &custody,
            &[],
            &attached,
            NOW,
        ));
        match verdict {
            AuditVerdict::InclusionFailure { sampled, .. } => assert_eq!(
                sampled as usize, AUDIT_SAMPLE_K,
                "40 files in ONE covered folder must cost K samples, not 40"
            ),
            other => panic!("expected the dropped-chunk failure: {other:?}"),
        }
    }

    /// **The audited party must not get to pick its own exam.**
    ///
    /// Identical inputs to
    /// `a_covered_folder_passes_the_destination_audit_it_cannot_open` in every
    /// respect but one: there, the client had attached that covered folder;
    /// here it has attached nothing. The custody reply is the same, the blob
    /// plane is the same, the set name is the same well-formed
    /// `__folder/<hex>/<id>`.
    ///
    /// That single difference has to flip the verdict, because the two planes
    /// are not equally strong. Hash-verified presence is rooted in the
    /// `manifest_hash` the **destination itself** supplied, so it is
    /// self-consistent by construction — a destination that discarded the
    /// owner's sealed bytes can still answer it perfectly. The full open cannot
    /// be answered that way: it runs under a key the destination has never held.
    ///
    /// So a destination that discarded everything and then labelled every
    /// custody row `__folder/…` used to route its entire sample to the weak arm
    /// and pass while holding nothing — the strong arm simply never ran. The
    /// routing input is now the client's own attachment record
    /// (`attached_mirror_sets`, read from its pinned
    /// `fauna.state.backup` destination list), which no destination can add to.
    ///
    /// Red-verified against the pre-fix routing: drop the
    /// `attached_mirror_sets.contains(…)` conjunct in `evaluate_inclusion` and
    /// this returns `Passed`.
    #[test]
    fn a_destination_cannot_route_itself_to_the_weak_arm_by_naming_sets_the_client_never_attached()
    {
        let invented = format!("__folder/{}/999", "ef".repeat(32));
        let custody: Vec<CustodyItem> = (0..3).map(|i| mirror_custody(&invented, i)).collect();

        // The client attached NO covered folders to this destination. Anything
        // `__folder/…` in the reply is therefore the destination's own
        // invention, whatever it looks like.
        let attached = AttachedMirrorSets::new();

        let verdict = block_on(evaluate_inclusion(
            &MockInclusion::new(Blobs::MirrorPlane),
            DEST_URL,
            &custody,
            &[],
            &attached,
            NOW,
        ));
        match verdict {
            AuditVerdict::InclusionFailure { missing, sampled } => {
                assert_eq!(
                    sampled, 3,
                    "every invented row must be sampled, not skipped"
                );
                assert_eq!(
                    missing, 3,
                    "each invented row must take the FULL OPEN and fail it — routing \
                     them to presence is the bypass this test exists to close"
                );
            }
            other => panic!(
                "a destination inventing covered-folder names must not be able to \
                 downgrade its own exam, got {other:?}"
            ),
        }
    }

    /// The other half of the same rule, so the fix cannot be "fail every
    /// `__folder/…` row": a set the client **did** attach still routes to
    /// presence and still passes, even though the reply naming it is the same
    /// untrusted wire message.
    ///
    /// Without this, the obvious over-correction — treat the mirror plane as
    /// unverifiable and fail it — would satisfy the hostile test above while
    /// re-opening an old bypass: attaching one ordinary folder would turn a
    /// byte-perfect destination permanently red, and the frozen `last_passed_at`
    /// would silence the alarm that reports real rot.
    #[test]
    fn the_same_reply_passes_for_a_set_this_client_actually_attached() {
        let real = covered_folder_set();
        let custody: Vec<CustodyItem> = (0..3).map(|i| mirror_custody(&real, i)).collect();
        let attached = attached_to(&[&real]);

        let verdict = block_on(evaluate_inclusion(
            &MockInclusion::new(Blobs::MirrorPlane),
            DEST_URL,
            &custody,
            &[],
            &attached,
            NOW,
        ));
        assert_eq!(
            verdict,
            AuditVerdict::Passed,
            "genuine coverage must keep passing on presence — the client attached \
             this set, so its name is the client's own fact, not the destination's"
        );
    }

    /// The routing set is derived from the client's config, and this is the
    /// derivation: coverage rows share their destination's `destination_id` and
    /// differ only in `folder_name`, so the destination's own RESERVED set sits
    /// in the same list and must be filtered out by shape.
    ///
    /// Keeping `__mail` out is not tidiness — it is the whole strong arm. A
    /// derivation that returned "every `folder_name` for this destination"
    /// would put the reserved segment set into the mirror set and route the one
    /// plane that can actually be opened to presence instead, which is the
    /// bypass again, arriving from the client's side.
    #[test]
    fn the_attached_set_is_the_mirror_rows_only_never_the_destinations_reserved_set() {
        let covered = covered_folder_set();
        let other_destination_set = format!("__folder/{}/5", "11".repeat(32));
        let row = |id: &str, set: &str, added_at: i64| BackupDestination {
            destination_id: id.into(),
            folder_name: set.into(),
            added_at: added_at as u64,
            ..Default::default()
        };
        let rows = vec![
            row("dest-a", "__mail", NOW - 90 * DAY),
            row("dest-a", &covered, NOW - HOUR),
            row("dest-b", &other_destination_set, NOW - DAY),
        ];

        let sets = attached_mirror_sets(&rows, "dest-a");
        assert_eq!(
            sets.get(&covered),
            Some(&(NOW - HOUR)),
            "the folder this client attached to dest-a must be in its routing set, \
             dated by its own coverage row's attach time (the mirror plane's \
             population floor), never the enrollment row's"
        );
        assert!(
            !sets.contains_key("__mail"),
            "the reserved segment set must NOT be in the mirror set — routing it to \
             presence would drop the full open on the only plane that has one"
        );
        assert!(
            !sets.contains_key(&other_destination_set),
            "another destination's coverage must not leak into this one's routing set"
        );
    }

    /// And the routing is by **plane**, not by "try the open and fall back":
    /// a reserved segment set whose chunks are corrupt keeps failing exactly as
    /// before. Falling back would have made every corrupt segment pass on the
    /// presence check.
    #[test]
    fn a_reserved_segment_set_keeps_the_stronger_open_test() {
        let custody: Vec<CustodyItem> = (0..3).map(|i| custody_at("4242", i, NOW)).collect();
        // This client attached no covered folders, so nothing may route to the
        // weak arm — which is the point for every reserved-set case here.
        let attached = AttachedMirrorSets::new();
        let verdict = block_on(evaluate_inclusion(
            &MockInclusion::new(Blobs::CorruptChunks),
            DEST_URL,
            &custody,
            &[],
            &attached,
            NOW,
        ));
        assert!(
            matches!(verdict, AuditVerdict::InclusionFailure { .. }),
            "segment sets are sealed under the root this arm holds — they keep the open test"
        );
    }

    #[test]
    fn every_record_present_and_openable_passes() {
        let custody: Vec<CustodyItem> = (0..40).map(|i| custody_at("4242", i, NOW)).collect();
        // This client attached no covered folders, so nothing may route to the
        // weak arm — which is the point for every reserved-set case here.
        let attached = AttachedMirrorSets::new();
        let verdict = block_on(evaluate_inclusion(
            &all_present(),
            DEST_URL,
            &custody,
            &[],
            &attached,
            NOW,
        ));
        assert_eq!(verdict, AuditVerdict::Passed);
    }

    /// The headline: a destination that has *lost* the bytes it claims custody
    /// of is caught, even though its custody list — and therefore freshness —
    /// looks perfect. This is the whole reason the inclusion arm exists.
    #[test]
    fn a_destination_that_lost_the_bytes_fails_inclusion_despite_perfect_freshness() {
        let custody: Vec<CustodyItem> = (0..40).map(|i| custody_at("4242", i, NOW)).collect();
        // Freshness alone is clean: the destination's high-water IS `NOW`.
        assert_eq!(
            evaluate_freshness(&inputs(Some(NOW), NOW - 30 * DAY), Some(NOW)),
            AuditVerdict::Passed
        );
        // This client attached no covered folders, so nothing may route to the
        // weak arm — which is the point for every reserved-set case here.
        let attached = AttachedMirrorSets::new();
        let verdict = block_on(evaluate_inclusion(
            &MockInclusion::new(Blobs::ManifestGone),
            DEST_URL,
            &custody,
            &[],
            &attached,
            NOW,
        ));
        match verdict {
            AuditVerdict::InclusionFailure { missing, sampled } => {
                assert_eq!(sampled, AUDIT_SAMPLE_K as u32);
                assert_eq!(missing, AUDIT_SAMPLE_K as u32);
            }
            other => panic!("expected InclusionFailure, got {other:?}"),
        }
    }

    /// Present but not what it claims to be: the bytes come back, and the
    /// whole-file hash the manifest declares does not match them.
    #[test]
    fn a_record_whose_bytes_do_not_match_its_manifest_is_unopenable() {
        let custody: Vec<CustodyItem> = (0..40).map(|i| custody_at("4242", i, NOW)).collect();
        // This client attached no covered folders, so nothing may route to the
        // weak arm — which is the point for every reserved-set case here.
        let attached = AttachedMirrorSets::new();
        let verdict = block_on(evaluate_inclusion(
            &MockInclusion::new(Blobs::CorruptChunks),
            DEST_URL,
            &custody,
            &[],
            &attached,
            NOW,
        ));
        assert!(
            matches!(verdict, AuditVerdict::InclusionFailure { missing, .. } if missing > 0),
            "corrupt chunks must fail inclusion, got {verdict:?}"
        );
    }

    /// The key material is load-bearing, not decoration: a sealed record that
    /// this reader holds no key for fails closed rather than passing through.
    #[test]
    fn a_sealed_record_this_reader_holds_no_key_for_fails() {
        // Addressed to the sealed posture's own manifest, so the row reaches
        // the seal — pointed at the ordinary one it would fail on the manifest
        // address instead, and this test would pass for the wrong reason.
        let custody: Vec<CustodyItem> = (0..40)
            .map(|i| CustodyItem {
                manifest_hash: sealed_manifest_hash_hex(),
                ..custody_at("4242", i, NOW)
            })
            .collect();
        // This client attached no covered folders, so nothing may route to the
        // weak arm — which is the point for every reserved-set case here.
        let attached = AttachedMirrorSets::new();
        let verdict = block_on(evaluate_inclusion(
            &MockInclusion::keyless(Blobs::SealedUnopenable),
            DEST_URL,
            &custody,
            &[],
            &attached,
            NOW,
        ));
        assert!(
            matches!(verdict, AuditVerdict::InclusionFailure { missing, .. } if missing > 0),
            "a sealed manifest with no open root must fail closed, got {verdict:?}"
        );
    }

    /// **The bypass this closes.** A hostile destination that reports custody
    /// with the paths omitted would, under a "skip what you cannot address"
    /// rule, be sampled zero times and pass forever.
    #[test]
    fn custody_rows_without_a_path_cannot_silently_bypass_sampling() {
        let custody: Vec<CustodyItem> = (0..40)
            .map(|i| CustodyItem {
                path: None,
                ..custody_at("4242", i, NOW)
            })
            .collect();
        // This client attached no covered folders, so nothing may route to the
        // weak arm — which is the point for every reserved-set case here.
        let attached = AttachedMirrorSets::new();
        let verdict = block_on(evaluate_inclusion(
            &all_present(),
            DEST_URL,
            &custody,
            &[],
            &attached,
            NOW,
        ));
        match verdict {
            AuditVerdict::InclusionFailure { missing, sampled } => {
                assert_eq!(missing, sampled, "every path-less row is unverifiable");
                assert!(sampled > 0, "path-less rows must still be sampled");
            }
            other => panic!("path-less custody must not pass, got {other:?}"),
        }
    }

    /// K is ratified **per (destination, kind-scope)**, so three scopes are
    /// sampled three times over — not K spread thinly across the whole set.
    #[test]
    fn sampling_is_k_per_kind_scope_not_k_per_destination() {
        let mut custody = Vec::new();
        for scope in ["4242", "beef", "cafe"] {
            custody.extend((0..40).map(|i| custody_at(scope, i, NOW)));
        }
        // This client attached no covered folders, so nothing may route to the
        // weak arm — which is the point for every reserved-set case here.
        let attached = AttachedMirrorSets::new();
        let verdict = block_on(evaluate_inclusion(
            &MockInclusion::new(Blobs::ManifestGone),
            DEST_URL,
            &custody,
            &[],
            &attached,
            NOW,
        ));
        match verdict {
            AuditVerdict::InclusionFailure { sampled, .. } => {
                assert_eq!(
                    sampled,
                    3 * AUDIT_SAMPLE_K as u32,
                    "each of the 3 scopes must contribute its own K samples"
                );
            }
            other => panic!("expected InclusionFailure, got {other:?}"),
        }
    }

    /// The bytes must be fetched from the **destination being audited**. Asking
    /// the source nest — the party under suspicion — would let it answer for its
    /// own homework.
    #[test]
    fn the_sampled_bytes_are_fetched_from_the_audited_destination() {
        let custody: Vec<CustodyItem> = (0..40).map(|i| custody_at("4242", i, NOW)).collect();
        let inclusion = MockInclusion::new(Blobs::Intact);
        // This client attached no covered folders, so nothing may route to the
        // weak arm — which is the point for every reserved-set case here.
        let attached = AttachedMirrorSets::new();
        block_on(evaluate_inclusion(
            &inclusion,
            DEST_URL,
            &custody,
            &[],
            &attached,
            NOW,
        ));
        let asked = inclusion.asked.lock().unwrap().clone();
        assert!(
            !asked.is_empty(),
            "the inclusion arm never fetched anything"
        );
        assert!(
            asked.iter().all(|u| u == DEST_URL),
            "fetched from {asked:?}, expected only {DEST_URL}"
        );
    }

    /// An empty destination raises no inclusion alarm — there is nothing it
    /// claims to hold, so there is nothing to disprove. Whether an empty
    /// destination is *acceptable* is freshness's question, and it already
    /// answers it (`the_first_ever_audit_of_an_old_destination_actually_audits`).
    #[test]
    fn an_empty_destination_is_a_freshness_question_not_an_inclusion_one() {
        // This client attached no covered folders, so nothing may route to the
        // weak arm — which is the point for every reserved-set case here.
        let attached = AttachedMirrorSets::new();
        let verdict = block_on(evaluate_inclusion(
            &all_present(),
            DEST_URL,
            &[],
            &[],
            &attached,
            NOW,
        ));
        assert_eq!(verdict, AuditVerdict::Passed);
    }

    /// A pass must now mean **fresh AND included**. Before this slice a fresh
    /// destination holding garbage passed, which is what the goal doc called
    /// "strictly weaker than the ratified contract".
    #[test]
    fn the_last_passed_clock_does_not_advance_on_an_inclusion_failure() {
        let custody: Vec<CustodyItem> = (0..40).map(|i| custody_at("4242", i, NOW)).collect();
        let prior = state("d1", None);
        let (verdict, new_state) = block_on(audit_destination(
            &reachable(custody),
            &MockInclusion::new(Blobs::ManifestGone),
            DEST_URL,
            &inputs(Some(NOW), NOW - 30 * DAY),
            &prior,
            &AttachedMirrorSets::new(),
            &NoSourceVouch,
            NOW,
        ));
        assert!(
            verdict.is_alerting(),
            "expected a loud verdict, got {verdict:?}"
        );
        assert_eq!(
            new_state.last_passed_at, None,
            "an inclusion failure must NOT refresh the last-passed clock"
        );
        assert_eq!(
            new_state.last_attempt_at,
            Some(NOW),
            "but it is still an attempt, so the debounce clock moves"
        );
    }

    /// Freshness is checked first and short-circuits: a destination already
    /// failing freshness must not pay a network fetch per sampled record.
    #[test]
    fn a_freshness_failure_skips_the_inclusion_fetches() {
        let custody: Vec<CustodyItem> = (0..40)
            .map(|i| custody_at("4242", i, NOW - 10 * DAY))
            .collect();
        let inclusion = MockInclusion::new(Blobs::Intact);
        let (verdict, _) = block_on(audit_destination(
            &reachable(custody),
            &inclusion,
            DEST_URL,
            &inputs(Some(NOW), NOW - 30 * DAY),
            &state("d1", None),
            &AttachedMirrorSets::new(),
            &NoSourceVouch,
            NOW,
        ));
        assert!(matches!(verdict, AuditVerdict::FreshnessFailure { .. }));
        assert!(
            inclusion.asked.lock().unwrap().is_empty(),
            "inclusion must not run once freshness has already failed"
        );
    }

    // ── the ratified constants ───────────────────────────────────────────────

    /// The constants are ratified in the goal doc; pin them so a future edit is
    /// a deliberate act rather than a typo, and pin the property the threat
    /// model rests on: every cadence is well inside the grace window T = 30 d.
    #[test]
    fn ratified_constants_are_all_far_inside_the_grace_window() {
        assert_eq!(AUDIT_SAMPLE_K, 16);
        assert_eq!(AUDIT_MIN_INTERVAL_SECS, DAY);
        assert_eq!(FRESHNESS_SLACK_SECS, 2 * DAY);
        assert_eq!(AUDIT_OVERDUE_SECS, 7 * DAY);
        let t_grace = 30 * DAY;
        for c in [
            AUDIT_MIN_INTERVAL_SECS,
            FRESHNESS_SLACK_SECS,
            AUDIT_OVERDUE_SECS,
        ] {
            assert!(c * 2 < t_grace, "{c} is not comfortably inside T");
        }
    }

    // ── freshness ────────────────────────────────────────────────────────────

    #[test]
    fn a_destination_keeping_up_passes_freshness() {
        let v = evaluate_freshness(&inputs(Some(NOW), NOW - 30 * DAY), Some(NOW - HOUR));
        assert_eq!(v, AuditVerdict::Passed);
    }

    /// The boundary, from both sides: exactly at the slack is still fine, one
    /// second past it is a failure. An off-by-one here either alarms on healthy
    /// backups or lets a stale one through.
    #[test]
    fn freshness_boundary_is_exclusive_at_exactly_the_slack() {
        let at = evaluate_freshness(
            &inputs(Some(NOW), NOW - 30 * DAY),
            Some(NOW - FRESHNESS_SLACK_SECS),
        );
        assert_eq!(
            at,
            AuditVerdict::Passed,
            "exactly at the slack still passes"
        );

        let past = evaluate_freshness(
            &inputs(Some(NOW), NOW - 30 * DAY),
            Some(NOW - FRESHNESS_SLACK_SECS - 1),
        );
        assert_eq!(
            past,
            AuditVerdict::FreshnessFailure {
                lag_secs: FRESHNESS_SLACK_SECS + 1
            }
        );
    }

    /// A client holding nothing locally cannot have a stale backup — there is
    /// nothing for the destination to be behind on.
    #[test]
    fn a_client_with_no_local_data_never_fails_freshness() {
        let v = evaluate_freshness(&inputs(None, NOW - 30 * DAY), None);
        assert_eq!(v, AuditVerdict::Passed);
    }

    /// The `added_at` floor: a destination enrolled minutes ago holds nothing
    /// yet (the nest sweeps every 15 min) and must not alarm.
    #[test]
    fn a_just_added_destination_with_no_custody_passes() {
        let v = evaluate_freshness(&inputs(Some(NOW), NOW - 10 * 60), None);
        assert_eq!(v, AuditVerdict::Passed);
    }

    /// …but the floor is a grace period, not an exemption: a destination that
    /// has held nothing since well before the slack window fails.
    #[test]
    fn a_destination_that_never_received_anything_eventually_fails_freshness() {
        let v = evaluate_freshness(&inputs(Some(NOW), NOW - 10 * DAY), None);
        assert_eq!(v, AuditVerdict::FreshnessFailure { lag_secs: 10 * DAY });
    }

    #[test]
    fn destination_high_water_is_the_newest_custody_row() {
        let items = vec![
            custody(NOW - 5 * DAY),
            custody(NOW - HOUR),
            custody(NOW - DAY),
        ];
        assert_eq!(destination_high_water(&items), Some(NOW - HOUR));
        assert_eq!(destination_high_water(&[]), None);
    }

    // ── overdue ──────────────────────────────────────────────────────────────

    #[test]
    fn a_recently_passed_destination_is_not_overdue() {
        let s = state("d1", Some(NOW - DAY));
        assert_eq!(evaluate_overdue(&s, &inputs(Some(NOW), 0), NOW), None);
    }

    #[test]
    fn a_destination_unaudited_past_the_overdue_window_is_overdue() {
        let s = state("d1", Some(NOW - 8 * DAY));
        assert_eq!(
            evaluate_overdue(&s, &inputs(Some(NOW), 0), NOW),
            Some(8 * DAY)
        );
    }

    /// A newly enrolled destination is not born overdue — the clock starts when
    /// the user added it, not at the epoch.
    #[test]
    fn a_never_audited_new_destination_is_not_overdue() {
        let s = state("d1", None);
        assert_eq!(
            evaluate_overdue(&s, &inputs(Some(NOW), NOW - HOUR), NOW),
            None
        );
    }

    /// …but one that has never once been auditable still alerts a week in.
    #[test]
    fn a_never_audited_old_destination_is_overdue() {
        let s = state("d1", None);
        assert_eq!(
            evaluate_overdue(&s, &inputs(Some(NOW), NOW - 9 * DAY), NOW),
            Some(9 * DAY)
        );
    }

    // ── which verdicts alert ─────────────────────────────────────────────────

    /// Exactly the three ratified failing states raise the banner. Unreachable
    /// must not: a laptop on a plane would otherwise cry data loss every flight.
    #[test]
    fn exactly_the_three_ratified_failures_alert() {
        assert!(AuditVerdict::FreshnessFailure { lag_secs: 1 }.is_alerting());
        assert!(
            AuditVerdict::InclusionFailure {
                missing: 1,
                sampled: 16
            }
            .is_alerting()
        );
        assert!(AuditVerdict::Overdue { since_secs: 1 }.is_alerting());

        assert!(!AuditVerdict::Passed.is_alerting());
        assert!(
            !AuditVerdict::Unreachable {
                error: "offline".into()
            }
            .is_alerting(),
            "a transient unreachable must stay quiet — sustained ones go overdue"
        );
    }

    // ── the debounce ─────────────────────────────────────────────────────────

    #[test]
    fn a_never_attempted_destination_runs_immediately() {
        assert!(should_run(&state("d1", None), NOW));
    }

    #[test]
    fn the_debounce_floor_is_the_last_attempt_not_the_last_pass() {
        let mut s = state("d1", Some(NOW - 30 * DAY)); // passed long ago…
        s.last_attempt_at = Some(NOW - HOUR); // …but attempted an hour ago.
        assert!(
            !should_run(&s, NOW),
            "a failing destination must not re-audit on every trigger"
        );

        s.last_attempt_at = Some(NOW - AUDIT_MIN_INTERVAL_SECS);
        assert!(should_run(&s, NOW), "exactly at the floor is due");
    }

    // ── one full pass ────────────────────────────────────────────────────────

    #[test]
    fn a_healthy_pass_records_the_passed_timestamp() {
        let dest = reachable(vec![custody(NOW - HOUR)]);
        let (verdict, state) = block_on(audit_destination_t(
            &dest,
            &inputs(Some(NOW), NOW - 30 * DAY),
            &state("d1", Some(NOW - 2 * DAY)),
            NOW,
        ));
        assert_eq!(verdict, AuditVerdict::Passed);
        assert_eq!(state.last_passed_at, Some(NOW));
        assert_eq!(state.last_attempt_at, Some(NOW));
    }

    /// A stale destination is reported AND leaves the last-passed clock alone —
    /// the page must keep showing when it was last genuinely healthy, not
    /// relabel a failure as a pass.
    #[test]
    fn a_stale_pass_reports_and_does_not_advance_the_passed_clock() {
        let dest = reachable(vec![custody(NOW - 5 * DAY)]);
        let prior = state("d1", Some(NOW - 4 * DAY));
        let (verdict, state) = block_on(audit_destination_t(
            &dest,
            &inputs(Some(NOW), NOW - 30 * DAY),
            &prior,
            NOW,
        ));
        assert_eq!(
            verdict,
            AuditVerdict::FreshnessFailure { lag_secs: 5 * DAY }
        );
        assert_eq!(
            state.last_passed_at,
            Some(NOW - 4 * DAY),
            "a failed pass must not advance the last-passed clock"
        );
        assert_eq!(state.last_attempt_at, Some(NOW));
    }

    /// The mechanism that turns sustained unreachability into a loud alert:
    /// an unreachable pass never refreshes `last_passed_at`, so the overdue
    /// clock keeps running.
    #[test]
    fn an_unreachable_destination_is_quiet_but_does_not_refresh_the_clock() {
        let prior = state("d1", Some(NOW - 2 * DAY));
        let (verdict, state) = block_on(audit_destination_t(
            &dead(),
            &inputs(Some(NOW), 0),
            &prior,
            NOW,
        ));
        assert!(matches!(verdict, AuditVerdict::Unreachable { .. }));
        assert!(!verdict.is_alerting());
        assert_eq!(
            state.last_passed_at,
            Some(NOW - 2 * DAY),
            "unreachable must not count as a pass"
        );
        assert_eq!(state.last_attempt_at, Some(NOW));
    }

    /// The same destination, a week later: still unreachable, now loud.
    #[test]
    fn sustained_unreachability_becomes_an_overdue_alert() {
        let prior = state("d1", Some(NOW - 8 * DAY));
        let (verdict, _) = block_on(audit_destination_t(
            &dead(),
            &inputs(Some(NOW), 0),
            &prior,
            NOW,
        ));
        assert_eq!(
            verdict,
            AuditVerdict::Overdue {
                since_secs: 8 * DAY
            }
        );
        assert!(verdict.is_alerting());
    }

    /// A destination we CAN read reports what we actually found, never
    /// `Overdue` — even when it has not passed in longer than the overdue
    /// window. Overdue means "I could not check", and we just did.
    #[test]
    fn a_readable_destination_reports_its_real_verdict_not_overdue() {
        let dest = reachable(vec![custody(NOW - 20 * DAY)]);
        let prior = state("d1", Some(NOW - 9 * DAY));
        let (verdict, _) = block_on(audit_destination_t(
            &dest,
            &inputs(Some(NOW), NOW - 30 * DAY),
            &prior,
            NOW,
        ));
        assert_eq!(
            verdict,
            AuditVerdict::FreshnessFailure { lag_secs: 20 * DAY },
            "a determinable verdict must beat the not-knowing escalation"
        );
    }

    /// The first-ever audit of a long-configured destination must actually run,
    /// not report `Overdue` on the strength of its age alone. This is the case
    /// every existing user hits the day the audit ships.
    #[test]
    fn the_first_ever_audit_of_an_old_destination_actually_audits() {
        let dest = reachable(vec![custody(NOW - HOUR)]);
        let prior = state("d1", None); // never audited…
        let (verdict, state) = block_on(audit_destination_t(
            &dest,
            &inputs(Some(NOW), NOW - 90 * DAY), // …and configured months ago
            &prior,
            NOW,
        ));
        assert_eq!(verdict, AuditVerdict::Passed);
        assert_eq!(state.last_passed_at, Some(NOW));
    }

    // ── sampling ─────────────────────────────────────────────────────────────

    #[test]
    fn sampling_takes_everything_when_the_population_is_small() {
        assert_eq!(sample_indices(3, AUDIT_SAMPLE_K, 7), vec![0, 1, 2]);
        assert!(sample_indices(0, AUDIT_SAMPLE_K, 7).is_empty());
    }

    /// K distinct indices out of a larger population — a sample that repeated an
    /// index would silently weaken the detection probability the constant is
    /// justified by.
    #[test]
    fn sampling_picks_k_distinct_indices() {
        let picked = sample_indices(100, AUDIT_SAMPLE_K, 12345);
        assert_eq!(picked.len(), AUDIT_SAMPLE_K);
        let unique: std::collections::BTreeSet<_> = picked.iter().collect();
        assert_eq!(
            unique.len(),
            AUDIT_SAMPLE_K,
            "sampled indices must be distinct"
        );
        assert!(picked.iter().all(|&i| i < 100));
    }

    /// Successive passes must cover different records, or daily auditing would
    /// re-check the same 16 forever and never compound.
    #[test]
    fn successive_passes_sample_different_subsets() {
        let a = sample_indices(100, AUDIT_SAMPLE_K, 1);
        let b = sample_indices(100, AUDIT_SAMPLE_K, 2);
        assert_ne!(a, b);
    }

    // ── the whole-fleet pass ─────────────────────────────────────────────────

    struct MockConnector {
        by_url: HashMap<String, Arc<MockDestination>>,
    }

    #[async_trait]
    impl BackupDestinationConnector for MockConnector {
        async fn connect(&self, url: &str) -> Result<DestinationConnection, String> {
            match self.by_url.get(url) {
                Some(d) => Ok(DestinationConnection {
                    seam: d.clone() as Arc<dyn BackupNestSeam>,
                    bound_nest_id: ENROLLED_ID,
                }),
                None => Err("no route to destination".into()),
            }
        }
    }

    /// The identity every [`destination`] row enrolled, and the one an
    /// honest [`MockConnector`] destination proves.
    const ENROLLED_ID: [u8; 32] = [7u8; 32];

    fn destination(id: &str, url: &str, added_at: u64) -> BackupDestination {
        BackupDestination {
            destination_id: id.into(),
            destination_nest_url: url.into(),
            destination_actor_pubkey: ENROLLED_ID,
            folder_name: "__mail".into(),
            added_at,
            ..Default::default()
        }
    }

    /// One dead destination degrades only its own row — the healthy one beside
    /// it still passes. The inverse (a failure blanking the whole page) is the
    /// bug leg (c) already had to fix once.
    #[test]
    fn one_dead_destination_never_blanks_the_others() {
        let mut by_url = HashMap::new();
        by_url.insert(
            "wss://good.example".to_string(),
            Arc::new(reachable(vec![custody(NOW - HOUR)])),
        );
        let connector = MockConnector { by_url };

        let dests = vec![
            destination("good", "wss://good.example", (NOW - 30 * DAY) as u64),
            destination("gone", "wss://gone.example", (NOW - 30 * DAY) as u64),
        ];
        let out = block_on(audit_all_t(&connector, &dests, &[], Some(NOW), NOW));

        assert_eq!(out.len(), 2);
        assert_eq!(out[0].destination_id, "good");
        assert_eq!(out[0].verdict, AuditVerdict::Passed);
        assert_eq!(out[1].destination_id, "gone");
        assert!(matches!(
            out[1].verdict,
            AuditVerdict::Unreachable { .. } | AuditVerdict::Overdue { .. }
        ));
    }

    /// A destination inside its debounce window is skipped entirely — no
    /// connection is opened, which is why the mock would panic if it were.
    #[test]
    fn a_debounced_destination_is_skipped_without_connecting() {
        let connector = MockConnector {
            by_url: HashMap::new(), // any connect would error → verdict, not skip
        };
        let dests = vec![destination(
            "d1",
            "wss://d.example",
            (NOW - 30 * DAY) as u64,
        )];
        let prior = vec![DestinationAuditState {
            destination_id: "d1".into(),
            last_passed_at: Some(NOW - HOUR),
            last_attempt_at: Some(NOW - HOUR),
            verified_ledger_generations: Default::default(),
            accepted_regressions: Default::default(),
            seat_settled_under: Some(bound_hex()),
        }];
        let out = block_on(audit_all_t(&connector, &dests, &prior, Some(NOW), NOW));
        assert!(out.is_empty(), "inside the debounce nothing runs");
    }

    // ── the seat carry's caller ─────────────────────────────────────────────

    /// A destination whose seat names the box's predecessor, answering an
    /// empty custody list — enough for a pass, and a handover it applies the
    /// way the nest does.
    struct SeatedDestination {
        seat: std::sync::Mutex<Vec<fauna_protocol::backup::WriterGrantItem>>,
        handovers: std::sync::Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl BackupNestSeam for SeatedDestination {
        async fn status(&self) -> Result<BackupStatusReply, String> {
            panic!("the audit must never ask a destination for the source's status")
        }
        async fn nest_key_revoke(&self) -> Result<(), String> {
            panic!("the audit never revokes")
        }
        async fn writer_grant_list(&self) -> Result<WriterGrantListReply, String> {
            Ok(WriterGrantListReply {
                grants: self.seat.lock().unwrap().clone(),
                ..Default::default()
            })
        }
        async fn writer_grant_revoke(&self, _: String) -> Result<bool, String> {
            panic!("the audit never revokes")
        }
        async fn custody_list(&self, _: Option<String>) -> Result<CustodyListReply, String> {
            Ok(CustodyListReply::default())
        }
        async fn generation_list(
            &self,
            _: Option<String>,
        ) -> Result<fauna_protocol::backup::GenerationListReply, String> {
            Ok(Default::default())
        }
        async fn generation_restore(
            &self,
            _: String,
            _: String,
            _: String,
        ) -> Result<fauna_protocol::backup::GenerationRestoreReply, String> {
            panic!("the audit never restores")
        }
        async fn writer_grant_succeed(
            &self,
            writer_nest_id: String,
            succeeds: String,
        ) -> Result<(), String> {
            self.handovers
                .lock()
                .unwrap()
                .push((writer_nest_id.clone(), succeeds));
            *self.seat.lock().unwrap() = vec![fauna_protocol::backup::WriterGrantItem {
                writer_nest_id,
                ..Default::default()
            }];
            Ok(())
        }
    }

    /// The owner's source nest: answers the rotation chain, or not.
    struct ChainSource(Option<Vec<fauna_protocol::nest_rotation::SignedNestRotation>>);

    #[async_trait]
    impl BackupNestSeam for ChainSource {
        async fn status(&self) -> Result<BackupStatusReply, String> {
            unreachable!()
        }
        async fn nest_key_revoke(&self) -> Result<(), String> {
            unreachable!()
        }
        async fn writer_grant_list(&self) -> Result<WriterGrantListReply, String> {
            panic!("writer grants are read at the destination, never the source")
        }
        async fn writer_grant_revoke(&self, _: String) -> Result<bool, String> {
            unreachable!()
        }
        async fn custody_list(&self, _: Option<String>) -> Result<CustodyListReply, String> {
            unreachable!()
        }
        async fn generation_list(
            &self,
            _: Option<String>,
        ) -> Result<fauna_protocol::backup::GenerationListReply, String> {
            unreachable!()
        }
        async fn generation_restore(
            &self,
            _: String,
            _: String,
            _: String,
        ) -> Result<fauna_protocol::backup::GenerationRestoreReply, String> {
            unreachable!()
        }
        async fn rotation_chain(
            &self,
        ) -> Result<Vec<fauna_protocol::nest_rotation::SignedNestRotation>, String> {
            self.0.clone().ok_or_else(|| "no chain kind".into())
        }
    }

    /// URL → seam, every connection proving [`ENROLLED_ID`].
    struct SeamConnector(HashMap<String, Arc<dyn BackupNestSeam>>);

    #[async_trait]
    impl BackupDestinationConnector for SeamConnector {
        async fn connect(&self, url: &str) -> Result<DestinationConnection, String> {
            self.0
                .get(url)
                .map(|seam| DestinationConnection {
                    seam: Arc::clone(seam),
                    bound_nest_id: ENROLLED_ID,
                })
                .ok_or_else(|| "no route".into())
        }
    }

    /// A box rotated from `p` to [`BOUND`]'s keypair: the predecessor's id
    /// and the hop's signed statement.
    fn rotated_box() -> (
        [u8; 32],
        [u8; 32],
        fauna_protocol::nest_rotation::SignedNestRotation,
    ) {
        use fauna_core::identity::ActorKeypair;
        let p = ActorKeypair::from_secret([0x11; 32]);
        let b = ActorKeypair::from_secret([0x22; 32]);
        let hop = fauna_protocol::nest_rotation::NestRotation {
            old_nest_actor_id: p.actor_id().0,
            new_nest_actor_id: b.actor_id().0,
            seq: 1,
            rotated_at: NOW - DAY,
        }
        .sign(p.signing_key(), b.signing_key())
        .unwrap();
        (p.actor_id().0, b.actor_id().0, hop)
    }

    /// **The first pass under a changed bound identity runs regardless of the
    /// debounce, and carries the seat ahead of its freshness arm**
    /// (`segment-backup-protocol.md` § *Where the carry runs*). The record
    /// then remembers the identity it settled under, so the next pass under
    /// it keeps the debounce.
    #[test]
    fn a_rotated_bound_identity_is_audited_at_once_and_its_seat_carried() {
        let (p, b, hop) = rotated_box();
        let dest = Arc::new(SeatedDestination {
            seat: std::sync::Mutex::new(vec![fauna_protocol::backup::WriterGrantItem {
                writer_nest_id: fauna_core::hex32::encode(&p),
                ..Default::default()
            }]),
            handovers: Default::default(),
        });
        let connector = SeamConnector(HashMap::from([
            (
                "wss://d.example".to_string(),
                Arc::clone(&dest) as Arc<dyn BackupNestSeam>,
            ),
            (
                SOURCE_URL.to_string(),
                Arc::new(ChainSource(Some(vec![hop]))) as Arc<dyn BackupNestSeam>,
            ),
        ]));
        let dests = vec![destination(
            "d1",
            "wss://d.example",
            (NOW - 30 * DAY) as u64,
        )];
        // Attempted an hour ago, settled under the predecessor.
        let prior = vec![DestinationAuditState {
            last_attempt_at: Some(NOW - HOUR),
            seat_settled_under: Some(fauna_core::hex32::encode(&p)),
            ..state("d1", Some(NOW - HOUR))
        }];

        let out = block_on(audit_all(
            &connector,
            &all_present(),
            &dests,
            &prior,
            Some(NOW),
            SOURCE_URL,
            &b,
            NOW,
        ));

        assert_eq!(out.len(), 1, "due despite the debounce");
        assert_eq!(
            dest.handovers.lock().unwrap().clone(),
            vec![(fauna_core::hex32::encode(&b), fauna_core::hex32::encode(&p))]
        );
        assert_eq!(
            out[0].state.seat_settled_under,
            Some(fauna_core::hex32::encode(&b))
        );

        // Under the same identity, inside the debounce, nothing runs again.
        let again = block_on(audit_all(
            &connector,
            &all_present(),
            &dests,
            &[out[0].state.clone()],
            Some(NOW),
            SOURCE_URL,
            &b,
            NOW + 60,
        ));
        assert!(again.is_empty());
        assert_eq!(dest.handovers.lock().unwrap().len(), 1);
    }

    /// A carry that cannot settle — here the source will not serve its chain
    /// — leaves the record unsettled, so the next pass tries again whatever
    /// the debounce says; and nothing is handed over meanwhile.
    #[test]
    fn an_unsettled_carry_is_retried_on_the_next_pass() {
        let (p, b, _) = rotated_box();
        let dest = Arc::new(SeatedDestination {
            seat: std::sync::Mutex::new(vec![fauna_protocol::backup::WriterGrantItem {
                writer_nest_id: fauna_core::hex32::encode(&p),
                ..Default::default()
            }]),
            handovers: Default::default(),
        });
        let connector = SeamConnector(HashMap::from([
            (
                "wss://d.example".to_string(),
                Arc::clone(&dest) as Arc<dyn BackupNestSeam>,
            ),
            (
                SOURCE_URL.to_string(),
                Arc::new(ChainSource(None)) as Arc<dyn BackupNestSeam>,
            ),
        ]));
        let dests = vec![destination(
            "d1",
            "wss://d.example",
            (NOW - 30 * DAY) as u64,
        )];

        let first = block_on(audit_all(
            &connector,
            &all_present(),
            &dests,
            &[],
            Some(NOW),
            SOURCE_URL,
            &b,
            NOW,
        ));
        assert_eq!(first[0].state.seat_settled_under, None);
        assert!(dest.handovers.lock().unwrap().is_empty());

        let second = block_on(audit_all(
            &connector,
            &all_present(),
            &dests,
            &[first[0].state.clone()],
            Some(NOW),
            SOURCE_URL,
            &b,
            NOW + 60,
        ));
        assert_eq!(second.len(), 1, "inside the debounce, yet due: unsettled");
    }

    /// a destination with two attached folders has three rows in
    /// the `fauna.state.backup` destination list — the enrollment row plus one coverage
    /// clone per folder, all sharing `destination_id` — and the pass must
    /// still produce exactly one outcome for it, not one per row.
    #[test]
    fn a_destination_with_covered_folders_gets_one_outcome_not_one_per_row() {
        let mut by_url = HashMap::new();
        by_url.insert(
            "wss://good.example".to_string(),
            Arc::new(reachable(vec![custody(NOW - HOUR)])),
        );
        let connector = MockConnector { by_url };

        let enrolled = destination("good", "wss://good.example", (NOW - 30 * DAY) as u64);
        let covered_a = BackupDestination {
            folder_name: "__folder/deadbeef/1".into(),
            added_at: NOW as u64,
            ..enrolled.clone()
        };
        let covered_b = BackupDestination {
            folder_name: "__folder/deadbeef/2".into(),
            added_at: NOW as u64,
            ..enrolled.clone()
        };
        let dests = vec![enrolled, covered_a, covered_b];

        let out = block_on(audit_all_t(&connector, &dests, &[], Some(NOW), NOW));

        assert_eq!(out.len(), 1, "one destination, one outcome");
        assert_eq!(out[0].destination_id, "good");
        assert_eq!(out[0].verdict, AuditVerdict::Passed);
    }

    /// The same fold applies to the merge — a naive per-row map would emit
    /// three near-identical records for the one destination above.
    #[test]
    fn merge_outcomes_emits_one_record_per_destination_not_per_coverage_row() {
        let enrolled = destination("good", "wss://good.example", (NOW - 30 * DAY) as u64);
        let covered = BackupDestination {
            folder_name: "__folder/deadbeef/1".into(),
            added_at: NOW as u64,
            ..enrolled.clone()
        };
        let dests = vec![enrolled, covered];
        let outcomes = vec![DestinationAuditOutcome {
            destination_id: "good".into(),
            verdict: AuditVerdict::Passed,
            state: state("good", Some(NOW)),
        }];

        let merged = merge_outcomes(&[], &outcomes, &dests);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].state.destination_id, "good");
    }

    // ── the banner map ───────────────────────────────────────────────────────

    /// `is_alerting` is *defined* through `alert_reason`, so the two can never
    /// disagree — but pin the mapping itself, variant by variant, because a
    /// future variant that forgot its arm would silently stop alerting rather
    /// than fail to compile.
    #[test]
    fn every_alerting_verdict_carries_its_banner_reason_and_no_other_does() {
        let cases: Vec<(AuditVerdict, Option<BackupAuditAlertReason>)> = vec![
            (
                AuditVerdict::FreshnessFailure { lag_secs: 3 * DAY },
                Some(BackupAuditAlertReason::Freshness { lag_secs: 3 * DAY }),
            ),
            (
                AuditVerdict::InclusionFailure {
                    missing: 2,
                    sampled: 16,
                },
                Some(BackupAuditAlertReason::Inclusion {
                    missing: 2,
                    sampled: 16,
                }),
            ),
            (
                AuditVerdict::Overdue {
                    since_secs: 9 * DAY,
                },
                Some(BackupAuditAlertReason::Overdue {
                    since_secs: 9 * DAY,
                }),
            ),
            (AuditVerdict::Passed, None),
            (
                AuditVerdict::Unreachable {
                    error: "offline".into(),
                },
                None,
            ),
        ];
        for (verdict, expected) in cases {
            assert_eq!(
                verdict.alert_reason(),
                expected,
                "banner reason for {verdict:?}"
            );
            assert_eq!(
                verdict.is_alerting(),
                expected.is_some(),
                "is_alerting must agree with alert_reason for {verdict:?}"
            );
        }
    }

    // ── the observation high-water ───────────────────────────────────────────

    /// Monotonic: reading an *older* record must never retract the evidence that
    /// newer data exists, or opening an old conversation would quietly clear a
    /// genuine freshness failure.
    #[test]
    fn the_observed_high_water_only_ever_moves_forward() {
        let mut snap = AuditStateSnapshot::default();

        assert!(observe_local_record(&mut snap, NOW - DAY, NOW));
        assert_eq!(snap.observed_high_water, Some(NOW - DAY));

        assert!(
            !observe_local_record(&mut snap, NOW - 5 * DAY, NOW),
            "an older observation changes nothing"
        );
        assert_eq!(snap.observed_high_water, Some(NOW - DAY));

        assert!(observe_local_record(&mut snap, NOW, NOW));
        assert_eq!(snap.observed_high_water, Some(NOW));

        assert!(
            !observe_local_record(&mut snap, 0, NOW),
            "a zero/absent timestamp is not an observation"
        );
        assert_eq!(snap.observed_high_water, Some(NOW));
    }

    /// A sender-dated stamp from the far future — one message from a
    /// modified client, stamped year 2100 — must not latch the high-water past
    /// the observer's own clock. Unclamped, the lag against every destination
    /// stays above the slack for ever and the audit alarms permanently; clamped,
    /// a destination that is keeping up passes the very next audit.
    #[test]
    fn a_future_dated_observation_cannot_latch_a_freshness_failure() {
        const YEAR_2100: i64 = 4_102_444_800;
        let mut snap = AuditStateSnapshot::default();

        assert!(observe_local_record(&mut snap, YEAR_2100, NOW));
        assert_eq!(
            snap.observed_high_water,
            Some(NOW + OBSERVATION_MAX_FUTURE_SKEW_SECS),
            "the offered stamp is clamped to the observer's clock plus the skew"
        );

        // A destination holding custody from an hour ago is keeping up.
        let later = NOW + DAY;
        assert_eq!(
            evaluate_freshness(
                &inputs(snap.observed_high_water, NOW - 30 * DAY),
                Some(later - HOUR)
            ),
            AuditVerdict::Passed,
            "an honest destination passes once the forged stamp is bounded"
        );

        // A stamp inside the cushion is honest drift and lands unchanged.
        let mut drift = AuditStateSnapshot::default();
        assert!(observe_local_record(&mut drift, NOW + 60, NOW));
        assert_eq!(drift.observed_high_water, Some(NOW + 60));
    }

    /// The clamp bounds only the *incoming* stamp: a stored high-water already
    /// past `now + skew` is never lowered by it (the monotonic rule wins), and a
    /// future stamp offered against it changes nothing.
    #[test]
    fn the_clamp_never_lowers_a_stored_high_water() {
        let stored = NOW + DAY;
        let mut snap = AuditStateSnapshot {
            observed_high_water: Some(stored),
            ..Default::default()
        };
        assert!(!observe_local_record(&mut snap, NOW + 30 * DAY, NOW));
        assert_eq!(snap.observed_high_water, Some(stored));
    }

    /// The skew is a drift cushion, not a window: it must stay a sliver of the
    /// freshness slack, or one forged stamp could spend the slack on its own.
    #[test]
    fn the_observation_skew_is_a_sliver_of_the_freshness_slack() {
        assert_eq!(OBSERVATION_MAX_FUTURE_SKEW_SECS, 5 * 60);
        const { assert!(OBSERVATION_MAX_FUTURE_SKEW_SECS * 100 < FRESHNESS_SLACK_SECS) };
    }

    // ── the merge ────────────────────────────────────────────────────────────

    fn record(id: &str, verdict: Option<AuditVerdict>) -> DestinationAuditRecord {
        DestinationAuditRecord {
            state: state(id, Some(NOW - DAY)),
            verdict,
        }
    }

    fn outcome(id: &str, verdict: AuditVerdict) -> DestinationAuditOutcome {
        DestinationAuditOutcome {
            destination_id: id.into(),
            verdict,
            state: state(id, Some(NOW)),
        }
    }

    /// The debounce case, and the reason the merge exists at all: a destination
    /// that did not run this pass keeps its standing alert. Rendering only the
    /// pass's outcomes would blank the alarm on every pass that skipped it —
    /// i.e. most passes, for exactly the destination that is broken.
    #[test]
    fn a_debounced_destination_keeps_its_standing_alert() {
        let prior = vec![
            record(
                "stale",
                Some(AuditVerdict::FreshnessFailure { lag_secs: 5 * DAY }),
            ),
            record("fine", Some(AuditVerdict::Passed)),
        ];
        let dests = vec![
            destination("stale", "wss://stale.example", (NOW - 30 * DAY) as u64),
            destination("fine", "wss://fine.example", (NOW - 30 * DAY) as u64),
        ];
        // Only "fine" ran this pass.
        let outcomes = vec![outcome("fine", AuditVerdict::Passed)];

        let merged = merge_outcomes(&prior, &outcomes, &dests);

        assert_eq!(merged.len(), 2);
        assert_eq!(
            merged[0].verdict,
            Some(AuditVerdict::FreshnessFailure { lag_secs: 5 * DAY }),
            "the skipped destination's alert must survive the pass"
        );
        assert!(merged[0].alert_reason().is_some());
        assert_eq!(merged[0].state.last_passed_at, Some(NOW - DAY));
        assert_eq!(
            merged[1].state.last_passed_at,
            Some(NOW),
            "…and the one that ran advanced"
        );
    }

    /// Removing a destination clears its alarm. A banner for a backup the user
    /// deliberately stopped is noise they have no way to dismiss.
    #[test]
    fn a_removed_destination_drops_its_record_and_its_alert() {
        let prior = vec![record(
            "gone",
            Some(AuditVerdict::Overdue {
                since_secs: 9 * DAY,
            }),
        )];
        let merged = merge_outcomes(&prior, &[], &[]);
        assert!(merged.is_empty());
    }

    /// A destination added since the last pass renders "never" rather than
    /// vanishing from the page until its first pass lands.
    #[test]
    fn a_newly_configured_destination_gets_a_never_record() {
        let dests = vec![destination("new", "wss://new.example", NOW as u64)];
        let merged = merge_outcomes(&[], &[], &dests);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].state.destination_id, "new");
        assert_eq!(merged[0].state.last_passed_at, None);
        assert_eq!(merged[0].verdict, None);
        assert!(
            merged[0].alert_reason().is_none(),
            "never-audited is not a failure"
        );
    }

    /// The merged order follows the configured destination list, so the page's
    /// banners and rows agree without the shell sorting anything.
    #[test]
    fn the_merge_is_ordered_like_the_configured_destinations() {
        let dests = vec![
            destination("b", "wss://b.example", 0),
            destination("a", "wss://a.example", 0),
        ];
        let prior = vec![record("a", Some(AuditVerdict::Passed))];
        let merged = merge_outcomes(&prior, &[], &dests);
        let ids: Vec<&str> = merged
            .iter()
            .map(|r| r.state.destination_id.as_str())
            .collect();
        assert_eq!(ids, vec!["b", "a"]);
    }

    // ── the whole pass, through the store ────────────────────────────────────

    /// An in-memory store, standing in for a file / `localStorage`.
    struct MemStore {
        snapshot: std::sync::Mutex<AuditStateSnapshot>,
        fail_load: bool,
        fail_save: bool,
        saves: std::sync::atomic::AtomicUsize,
    }

    impl MemStore {
        fn with(snapshot: AuditStateSnapshot) -> Self {
            Self {
                snapshot: std::sync::Mutex::new(snapshot),
                fail_load: false,
                fail_save: false,
                saves: Default::default(),
            }
        }
        fn broken() -> Self {
            Self {
                snapshot: std::sync::Mutex::new(AuditStateSnapshot::default()),
                fail_load: true,
                fail_save: false,
                saves: Default::default(),
            }
        }
        fn unwritable() -> Self {
            Self {
                snapshot: std::sync::Mutex::new(AuditStateSnapshot::default()),
                fail_load: false,
                fail_save: true,
                saves: Default::default(),
            }
        }
        fn saves(&self) -> usize {
            self.saves.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn saved(&self) -> AuditStateSnapshot {
            self.snapshot.lock().unwrap().clone()
        }
    }

    impl AuditStateStore for MemStore {
        fn load(&self) -> Result<AuditStateSnapshot, String> {
            if self.fail_load {
                return Err("store unreadable".into());
            }
            Ok(self.snapshot.lock().unwrap().clone())
        }
        fn save(&self, snapshot: &AuditStateSnapshot) -> Result<(), String> {
            if self.fail_save {
                return Err("store unwritable".into());
            }
            self.saves.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *self.snapshot.lock().unwrap() = snapshot.clone();
            Ok(())
        }
    }

    // ── the observation feed's fold, owned once for all four shells ──────────

    /// The unit boundary the four shells used to each re-derive. A stamp in
    /// milliseconds passed through unconverted reads ~50 years ahead, which is
    /// the silent failure this function exists to make unrepresentable.
    #[test]
    fn the_millisecond_boundary_truncates_and_is_owned_once() {
        assert_eq!(activity_secs(1_700_000_123_456), 1_700_000_123);
        assert_eq!(activity_secs(999), 0, "sub-second truncates toward zero");
        assert_eq!(activity_secs(0), 0);
        // The hazard, stated as a test: the raw stamp is ~1000× the seconds
        // value, i.e. tens of thousands of years of apparent freshness.
        let ms = 1_700_000_123_456_i64;
        assert!(ms - activity_secs(ms) > 50 * 365 * DAY);
    }

    /// The common path: a newer observation advances the high-water, persists,
    /// and reports back the value a shell should cache.
    #[test]
    fn a_newer_observation_persists_and_reports_the_stored_high_water() {
        let store = MemStore::with(AuditStateSnapshot::default());
        let out = observe_thread_activity(&store, 1_700_000_123_456);
        assert!(out.persisted);
        assert_eq!(out.high_water, Some(1_700_000_123));
        assert!(out.degradations.is_empty());
        assert_eq!(store.saved().observed_high_water, Some(1_700_000_123));
    }

    /// The steady state — a repeat render of the same threads must not write.
    #[test]
    fn a_repeat_observation_writes_nothing() {
        let store = MemStore::with(AuditStateSnapshot {
            observed_high_water: Some(1_700_000_123),
            ..Default::default()
        });
        let out = observe_thread_activity(&store, 1_700_000_123_456);
        assert!(!out.persisted, "a no-op fold must not claim a write");
        assert_eq!(out.high_water, Some(1_700_000_123));
        assert!(out.degradations.is_empty());
    }

    /// The tui/linux render-path cache seeds from this: a store already holding
    /// something newer than what this render offers must be what the fast path
    /// compares against, or every render would re-read the file.
    #[test]
    fn the_returned_high_water_is_the_stores_when_the_store_is_newer() {
        let store = MemStore::with(AuditStateSnapshot {
            observed_high_water: Some(1_700_000_900),
            ..Default::default()
        });
        let out = observe_thread_activity(&store, 1_700_000_123_456);
        assert!(!out.persisted);
        assert_eq!(
            out.high_water,
            Some(1_700_000_900),
            "seeding from the offered value would re-read the store every render"
        );
    }

    /// An unreadable store is left alone: the observation is dropped and the
    /// failure reported, never folded into a default snapshot and saved over
    /// the rollback evidence the file holds.
    #[test]
    fn an_unreadable_store_is_never_saved_over_by_an_observation() {
        let store = MemStore::broken();
        let out = observe_thread_activity(&store, 1_700_000_123_456);
        assert!(
            !out.persisted,
            "nothing may be written over an unread store"
        );
        assert_eq!(out.high_water, None);
        assert_eq!(out.degradations, vec!["store unreadable".to_string()]);
        assert_eq!(store.saves(), 0);
    }

    /// A failed write is reported as a degradation, never as a persist — a
    /// shell caching on `persisted` would otherwise never retry.
    #[test]
    fn a_failing_save_is_a_degradation_not_a_persist() {
        let store = MemStore::unwritable();
        let out = observe_thread_activity(&store, 1_700_000_123_456);
        assert!(!out.persisted);
        assert_eq!(out.degradations, vec!["store unwritable".to_string()]);
    }

    /// The shared fold every shell calls bounds the sender's stamp
    /// against the audit's own clock, so the FFI and wasm faces inherit the
    /// clamp rather than each owing one.
    #[test]
    fn the_fold_clamps_a_future_dated_stamp_to_the_audit_clock() {
        let store = MemStore::with(AuditStateSnapshot::default());
        let year_2100_ms = 4_102_444_800_000_i64;
        let out = observe_thread_activity(&store, year_2100_ms);
        let ceiling = crate::audit_clock::now_secs() + OBSERVATION_MAX_FUTURE_SKEW_SECS;
        assert!(out.persisted);
        let hw = out.high_water.expect("the fold observed something");
        assert!(
            hw <= ceiling,
            "high-water {hw} past the clock ceiling {ceiling}"
        );
        assert_eq!(store.saved().observed_high_water, Some(hw));
    }

    /// A non-positive stamp is not an observation at all (`observe_local_record`
    /// rejects it), so nothing is written and nothing is claimed.
    #[test]
    fn a_zero_stamp_observes_nothing() {
        let store = MemStore::with(AuditStateSnapshot::default());
        let out = observe_thread_activity(&store, 0);
        assert!(!out.persisted);
        assert_eq!(out.high_water, None);
        assert!(out.degradations.is_empty());
    }

    /// The one call a shell makes: the pass reads the persisted observation
    /// high-water, audits, and writes the result back — so the *next* pass sees
    /// the clocks this one advanced.
    #[test]
    fn a_pass_feeds_the_persisted_high_water_in_and_writes_the_verdict_back() {
        let mut by_url = HashMap::new();
        // The destination holds custody from 20 days ago; the client has
        // observed a record from an hour ago ⇒ stale.
        by_url.insert(
            "wss://d.example".to_string(),
            Arc::new(reachable(vec![custody(NOW - 20 * DAY)])),
        );
        let connector = MockConnector { by_url };
        let store = MemStore::with(AuditStateSnapshot {
            records: Vec::new(),
            observed_high_water: Some(NOW - HOUR),
        });
        let dests = vec![destination(
            "d1",
            "wss://d.example",
            (NOW - 30 * DAY) as u64,
        )];

        let rendered = block_on(run_audit_pass_t(&connector, &store, &dests, NOW));

        assert_eq!(rendered.len(), 1);
        assert!(
            matches!(
                rendered[0].verdict,
                Some(AuditVerdict::FreshnessFailure { .. })
            ),
            "the persisted observation is what makes this stale: {:?}",
            rendered[0].verdict
        );
        assert_eq!(
            store.saved().records,
            rendered,
            "what the shell renders is what the next pass will read"
        );
        assert_eq!(
            store.saved().observed_high_water,
            Some(NOW - HOUR),
            "a pass must not disturb the observation high-water"
        );
    }

    /// With nothing observed locally there is nothing a backup could be missing,
    /// so the same stale-looking destination passes. This is the documented
    /// `None` contract, and it is what keeps a fresh install quiet.
    #[test]
    fn a_pass_with_no_observation_cannot_fail_freshness() {
        let mut by_url = HashMap::new();
        by_url.insert(
            "wss://d.example".to_string(),
            Arc::new(reachable(vec![custody(NOW - 20 * DAY)])),
        );
        let connector = MockConnector { by_url };
        let store = MemStore::with(AuditStateSnapshot::default());
        let dests = vec![destination(
            "d1",
            "wss://d.example",
            (NOW - 30 * DAY) as u64,
        )];

        let rendered = block_on(run_audit_pass_t(&connector, &store, &dests, NOW));
        assert_eq!(rendered[0].verdict, Some(AuditVerdict::Passed));
        assert_eq!(rendered[0].state.last_passed_at, Some(NOW));
    }

    /// An unreadable store still gets a pass — its alerts are shown now — but
    /// the pass saves nothing over it and says so: the file holds rollback
    /// evidence a re-audit cannot rebuild.
    #[test]
    fn an_unreadable_store_still_audits_and_is_never_saved_over() {
        let mut by_url = HashMap::new();
        by_url.insert(
            "wss://d.example".to_string(),
            Arc::new(reachable(vec![custody(NOW - HOUR)])),
        );
        let connector = MockConnector { by_url };
        let store = MemStore::broken();
        let dests = vec![destination(
            "d1",
            "wss://d.example",
            (NOW - 30 * DAY) as u64,
        )];

        let pass = block_on(run_audit_pass(
            &connector,
            &all_present(),
            &store,
            &dests,
            SOURCE_URL,
            &BOUND,
            None,
            NOW,
        ));
        assert_eq!(pass.records.len(), 1);
        assert_eq!(pass.records[0].verdict, Some(AuditVerdict::Passed));
        assert_eq!(pass.degradations, vec!["store unreadable".to_string()]);
        assert_eq!(store.saves(), 0, "the unread store must not be overwritten");
    }

    // ── this device's own custodian store, folded into its row ───────────────

    /// This device's stable sync id in the fold's tests.
    const THIS_DEVICE: &str = "aa11";

    /// A client-device row naming `device` as its custodian — no address, no
    /// identity, like the row enrollment writes.
    fn custodian_destination(id: &str, device: &str) -> BackupDestination {
        BackupDestination {
            destination_id: id.into(),
            kind: fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE.into(),
            custodian_device_id: Some(device.into()),
            folder_name: "__mail".into(),
            added_at: (NOW - 30 * DAY) as u64,
            ..Default::default()
        }
    }

    /// The store's record for one refused pull: held at 40, served 0.
    fn own_store(regressions: &[&str]) -> OwnCustodianStore {
        OwnCustodianStore {
            device_id: THIS_DEVICE.into(),
            source_regressions: regressions
                .iter()
                .map(|ledger| StoreSourceRegression {
                    ledger: (*ledger).to_string(),
                    held: 40,
                    served: 0,
                    observed_at: NOW - HOUR,
                })
                .collect(),
        }
    }

    /// A connector that reaches nothing and remembers every URL it was asked
    /// for — a client-device row has no address, and the floor would dial the
    /// source.
    #[derive(Default)]
    struct DialLog {
        dialled: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl BackupDestinationConnector for DialLog {
        async fn connect(&self, url: &str) -> Result<DestinationConnection, String> {
            self.dialled.lock().unwrap().push(url.to_string());
            Err("no route to destination".into())
        }
    }

    async fn pass_with_store(
        connector: &dyn BackupDestinationConnector,
        store: &dyn AuditStateStore,
        destinations: &[BackupDestination],
        own: Option<&OwnCustodianStore>,
        now: i64,
    ) -> Vec<DestinationAuditRecord> {
        run_audit_pass(
            connector,
            &all_present(),
            store,
            destinations,
            SOURCE_URL,
            &BOUND,
            own,
            now,
        )
        .await
        .records
    }

    /// The fold: a regression standing on this device's store becomes the
    /// fifth banner reason on the row that assigns this device — with no
    /// deadline, the *until recovered* label — and on no other row.
    #[test]
    fn a_standing_store_regression_folds_into_this_devices_row_with_no_deadline() {
        let connector = DialLog::default();
        let store = MemStore::with(AuditStateSnapshot::default());
        let dests = vec![
            custodian_destination("other", "bb22"),
            custodian_destination("mine", THIS_DEVICE),
        ];
        let own = own_store(&["manifest.mail"]);

        let rendered = block_on(pass_with_store(&connector, &store, &dests, Some(&own), NOW));

        assert_eq!(rendered.len(), 2);
        assert!(
            rendered[0].state.accepted_regressions.is_empty(),
            "another device's row carries nothing of this store's"
        );
        assert_eq!(
            rendered[1].state.accepted_regressions.get("manifest.mail"),
            Some(&AcceptedRegression {
                pinned: 40,
                served: 0,
                observed_at: NOW - HOUR,
                floored_at: None,
                recoverable_until: None,
            })
        );
        assert!(
            rendered[1]
                .alert_reasons(NOW)
                .contains(&BackupAuditAlertReason::SourceRegressed { left_secs: None }),
            "{:?}",
            rendered[1].alert_reasons(NOW)
        );
        assert!(
            !rendered[0]
                .alert_reasons(NOW)
                .iter()
                .any(|r| matches!(r, BackupAuditAlertReason::SourceRegressed { .. }))
        );
        assert_eq!(
            store.saved().records,
            rendered,
            "the fold lands before the save"
        );
    }

    /// The store's record is the row's whole truth: once the store has cleared
    /// its regression, the next pass removes the folded entry — and it does so
    /// inside the debounce, where the row itself does not run.
    #[test]
    fn an_empty_store_record_removes_the_folded_entry() {
        let connector = DialLog::default();
        let store = MemStore::with(AuditStateSnapshot::default());
        let dests = vec![custodian_destination("mine", THIS_DEVICE)];

        let folded = block_on(pass_with_store(
            &connector,
            &store,
            &dests,
            Some(&own_store(&["manifest.mail", "manifest.post"])),
            NOW,
        ));
        assert_eq!(folded[0].state.accepted_regressions.len(), 2);

        let one_left = block_on(pass_with_store(
            &connector,
            &store,
            &dests,
            Some(&own_store(&["manifest.post"])),
            NOW + HOUR,
        ));
        assert_eq!(
            one_left[0]
                .state
                .accepted_regressions
                .keys()
                .collect::<Vec<_>>(),
            vec!["manifest.post"]
        );

        let cleared = block_on(pass_with_store(
            &connector,
            &store,
            &dests,
            Some(&own_store(&[])),
            NOW + 2 * HOUR,
        ));
        assert!(cleared[0].state.accepted_regressions.is_empty());
        assert!(
            !cleared[0]
                .alert_reasons(NOW + 2 * HOUR)
                .iter()
                .any(|r| matches!(r, BackupAuditAlertReason::SourceRegressed { .. }))
        );
        assert!(
            store.saved().records[0]
                .state
                .accepted_regressions
                .is_empty()
        );
    }

    /// "No store was read" is not "the store is clean": a pass handed no store
    /// leaves the folded entry standing, so a failed read never switches the
    /// notice off.
    #[test]
    fn a_pass_with_no_store_read_leaves_the_folded_entry_standing() {
        let connector = DialLog::default();
        let store = MemStore::with(AuditStateSnapshot::default());
        let dests = vec![custodian_destination("mine", THIS_DEVICE)];

        let folded = block_on(pass_with_store(
            &connector,
            &store,
            &dests,
            Some(&own_store(&["manifest.mail"])),
            NOW,
        ));
        let unread = block_on(pass_with_store(
            &connector,
            &store,
            &dests,
            None,
            NOW + HOUR,
        ));
        assert_eq!(
            unread[0].state.accepted_regressions,
            folded[0].state.accepted_regressions
        );
    }

    /// A folded entry is the store's, not a nest destination's accepted
    /// regression: across passes that are due it is neither pruned by the
    /// standing rule nor asks the source to floor its counter — the source is
    /// never dialled on its account.
    #[test]
    fn a_folded_entry_is_neither_pruned_nor_floors_the_source() {
        let connector = DialLog::default();
        let store = MemStore::with(AuditStateSnapshot::default());
        let dests = vec![custodian_destination("mine", THIS_DEVICE)];
        let own = own_store(&["manifest.mail"]);

        block_on(pass_with_store(&connector, &store, &dests, Some(&own), NOW));
        // Past the debounce, so the row itself runs again with the folded
        // entry in its prior state — and with no store handed in, so what
        // stands is what the pass itself left.
        let later = block_on(pass_with_store(
            &connector,
            &store,
            &dests,
            None,
            NOW + 3 * DAY,
        ));

        assert_eq!(
            later[0].state.accepted_regressions.get("manifest.mail"),
            Some(&AcceptedRegression {
                pinned: 40,
                served: 0,
                observed_at: NOW - HOUR,
                floored_at: None,
                recoverable_until: None,
            }),
            "the standing rule must not prune or floor a folded entry"
        );
        assert!(
            !connector
                .dialled
                .lock()
                .unwrap()
                .iter()
                .any(|url| url == SOURCE_URL),
            "the source was dialled: {:?}",
            connector.dialled.lock().unwrap()
        );
    }

    /// A destination's coverage rows share its id; the fold still finds the
    /// one row that assigns this device.
    #[test]
    fn the_fold_finds_this_devices_row_among_its_coverage_rows() {
        let enrolled = custodian_destination("mine", THIS_DEVICE);
        let covered = BackupDestination {
            folder_name: "__folder/deadbeef/1".into(),
            ..enrolled.clone()
        };
        let dests = vec![enrolled, covered];
        let mut records = vec![DestinationAuditRecord::never("mine")];

        fold_own_custodian_regressions(&mut records, &dests, &own_store(&["manifest.mail"]));

        assert_eq!(records[0].state.accepted_regressions.len(), 1);
    }

    // ── the open arm of `transport.md` § Rule 3 in full on `AuditVerdict` ────
    //
    // Proven against a test-only twin standing in for a NEWER build of this
    // app — one verdict the real type has never heard of — through the store's
    // own format (JSON, on every platform).

    /// The newer build's verdict vocabulary: one known verdict plus one added.
    #[derive(Serialize)]
    enum NewerAuditVerdict {
        Passed,
        Quarantined { since_secs: i64, by: String },
    }

    #[derive(Serialize)]
    struct NewerRecord {
        state: DestinationAuditState,
        verdict: Option<NewerAuditVerdict>,
    }

    #[derive(Serialize)]
    struct NewerSnapshot {
        records: Vec<NewerRecord>,
        observed_high_water: Option<i64>,
    }

    /// A newer build's file: `d1` stands under a verdict this build lacks,
    /// attempted a minute ago (inside the debounce); `d2` passed.
    fn newer_file() -> String {
        serde_json::to_string(&NewerSnapshot {
            records: vec![
                NewerRecord {
                    state: DestinationAuditState {
                        destination_id: "d1".into(),
                        last_passed_at: Some(NOW - 60),
                        last_attempt_at: Some(NOW - 60),
                        ..Default::default()
                    },
                    verdict: Some(NewerAuditVerdict::Quarantined {
                        since_secs: 42,
                        by: "a newer check".into(),
                    }),
                },
                NewerRecord {
                    state: DestinationAuditState {
                        destination_id: "d2".into(),
                        ..Default::default()
                    },
                    verdict: Some(NewerAuditVerdict::Passed),
                },
            ],
            // Behind the real clock, so a live observation advances it.
            observed_high_water: Some(1_700_000_000),
        })
        .unwrap()
    }

    /// A file holding a verdict this build lacks still loads; the unknown
    /// verdict renders neutral, and re-encoding the snapshot gives back the
    /// newer build's value unchanged.
    #[test]
    fn an_unknown_verdict_decodes_renders_neutral_and_re_emits_unchanged() {
        let raw = newer_file();
        let snapshot: AuditStateSnapshot =
            serde_json::from_str(&raw).expect("the snapshot decodes");
        let unknown = snapshot.records[0].verdict.clone().expect("a verdict");
        assert!(matches!(unknown, AuditVerdict::Unknown(_)), "{unknown:?}");
        assert_eq!(snapshot.records[1].verdict, Some(AuditVerdict::Passed));

        assert_eq!(
            unknown.alert_reason(),
            None,
            "an unknown verdict alerts on nothing"
        );
        assert!(!unknown.is_alerting());
        assert!(snapshot.records[0].alert_reasons(NOW).is_empty());

        let re_encoded = serde_json::to_value(&snapshot).unwrap();
        let original: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            re_encoded, original,
            "the carried verdict is re-emitted as read"
        );
    }

    /// The observation fold rewrites the whole snapshot — the unknown verdict
    /// rides through its save rather than being dropped or reset.
    #[test]
    fn an_observation_carries_the_unknown_verdict_through_its_save() {
        let store = MemStore::with(serde_json::from_str(&newer_file()).unwrap());
        let out = observe_thread_activity(&store, crate::audit_clock::now_ms());
        assert!(out.persisted, "{out:?}");

        let saved = serde_json::to_value(store.saved()).unwrap();
        assert_eq!(
            saved["records"][0]["verdict"],
            serde_json::json!({"Quarantined": {"since_secs": 42, "by": "a newer check"}}),
        );
    }

    /// An unknown standing verdict makes its destination due at once, inside
    /// its debounce window: the arm renders nothing, so this build replaces it
    /// with a verdict of its own on the first pass it runs — while `d2`, under
    /// the same clock, keeps its debounce.
    #[test]
    fn an_unknown_verdict_is_re_audited_inside_the_debounce() {
        let store = MemStore::with(serde_json::from_str(&newer_file()).unwrap());
        let connector = MockConnector {
            by_url: HashMap::new(),
        };
        let dests = vec![
            destination("d1", "wss://d1.example", (NOW - 30 * DAY) as u64),
            destination("d2", "wss://d2.example", (NOW - 30 * DAY) as u64),
        ];
        // `d2` was never attempted in the twin file, so pin it inside the
        // debounce too: only the unknown verdict may override the clock.
        let mut seeded = store.saved();
        seeded.records[1].state.last_attempt_at = Some(NOW - 60);
        // And settle its seat under the pass's identity, or the carry's own
        // trigger would run it (the twin file predates the field).
        seeded.records[1].state.seat_settled_under = Some(bound_hex());
        let store = MemStore::with(seeded);

        let rendered = block_on(run_audit_pass_t(&connector, &store, &dests, NOW));
        assert_eq!(
            rendered[0].verdict,
            Some(AuditVerdict::Unreachable {
                error: "no route to destination".into()
            }),
            "attempted a minute ago, yet the unknown verdict is re-audited"
        );
        assert_eq!(rendered[0].state.last_attempt_at, Some(NOW));
        assert_eq!(
            rendered[1].verdict,
            Some(AuditVerdict::Passed),
            "a known verdict keeps its debounce"
        );
    }

    /// The persisted shape must survive a round trip — it is written by one app
    /// launch and read by the next.
    #[test]
    fn the_persisted_snapshot_round_trips() {
        let snap = AuditStateSnapshot {
            records: vec![DestinationAuditRecord {
                state: DestinationAuditState {
                    destination_id: "d1".into(),
                    last_passed_at: Some(NOW - DAY),
                    last_attempt_at: Some(NOW),
                    verified_ledger_generations: Default::default(),
                    accepted_regressions: Default::default(),
                    seat_settled_under: Some("ab".repeat(32)),
                },
                verdict: Some(AuditVerdict::InclusionFailure {
                    missing: 3,
                    sampled: 16,
                }),
            }],
            observed_high_water: Some(NOW),
        };
        let json = serde_json::to_string(&snap).expect("serialize");
        let back: AuditStateSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, snap);
    }

    /// The destination set comes from the client's own pinned config, so a
    /// destination the source nest has "forgotten" is still audited. Deriving
    /// the list from the source would let a hostile source silence the audit.
    #[test]
    fn the_audited_set_is_the_clients_own_config_not_the_sources_registry() {
        let mut by_url = HashMap::new();
        by_url.insert(
            "wss://forgotten.example".to_string(),
            Arc::new(reachable(vec![custody(NOW - 20 * DAY)])),
        );
        let connector = MockConnector { by_url };
        let dests = vec![destination(
            "forgotten",
            "wss://forgotten.example",
            (NOW - 30 * DAY) as u64,
        )];
        let out = block_on(audit_all_t(&connector, &dests, &[], Some(NOW), NOW));
        assert_eq!(out.len(), 1);
        assert!(
            out[0].verdict.is_alerting(),
            "it still gets audited, and it still alerts"
        );
    }

    // ── the paged custody walk ──────────────────────────────────────────────

    /// Serves `custody.list` in scripted pages — the walk must stitch every
    /// one before any verdict logic runs (a storm's rows live past page one,
    /// and both the high-water and the sampled population come from the full
    /// set).
    struct PagedDestination {
        expect: Vec<Option<String>>,
        pages: Vec<(Vec<CustodyItem>, Option<String>)>,
        calls: std::sync::Mutex<usize>,
    }

    #[async_trait]
    impl BackupNestSeam for PagedDestination {
        async fn status(&self) -> Result<BackupStatusReply, String> {
            panic!("not part of the walk")
        }
        async fn nest_key_revoke(&self) -> Result<(), String> {
            panic!("the audit reports only — it never revokes")
        }
        async fn writer_grant_list(&self) -> Result<WriterGrantListReply, String> {
            panic!("not part of the walk")
        }
        async fn writer_grant_revoke(&self, _: String) -> Result<bool, String> {
            panic!("the audit reports only — it never revokes")
        }
        async fn generation_list(
            &self,
            _cursor: Option<String>,
        ) -> Result<fauna_protocol::backup::GenerationListReply, String> {
            panic!("the audit reads live custody — recovery is the generations surface")
        }
        async fn generation_restore(
            &self,
            _: String,
            _: String,
            _: String,
        ) -> Result<fauna_protocol::backup::GenerationRestoreReply, String> {
            panic!("the audit reports only — it never restores")
        }
        async fn custody_list(&self, cursor: Option<String>) -> Result<CustodyListReply, String> {
            let mut calls = self.calls.lock().unwrap();
            let i = *calls;
            *calls += 1;
            assert_eq!(
                cursor, self.expect[i],
                "page {i} was requested with the wrong cursor"
            );
            let (items, next_cursor) = self.pages[i].clone();
            Ok(CustodyListReply {
                items,
                next_cursor,
                extra: Default::default(),
            })
        }
    }

    #[test]
    fn read_full_custody_stitches_every_page_and_stops_at_the_absent_cursor() {
        let seam = PagedDestination {
            expect: vec![None, Some("p1".into()), Some("p2".into())],
            pages: vec![
                (
                    vec![custody(NOW - 10), custody(NOW - 20)],
                    Some("p1".into()),
                ),
                (vec![custody(NOW - 30)], Some("p2".into())),
                (vec![custody(NOW - 40)], None),
            ],
            calls: std::sync::Mutex::new(0),
        };

        let items = block_on(read_full_custody(&seam)).unwrap();
        assert_eq!(
            items.iter().map(|i| i.updated_at).collect::<Vec<_>>(),
            vec![NOW - 10, NOW - 20, NOW - 30, NOW - 40],
            "every page's rows in serve order — a short-page walk would hide \
             exactly the rows a storm produced"
        );
    }

    #[test]
    fn a_repeated_custody_cursor_is_an_error_not_a_partial_read() {
        let seam = PagedDestination {
            expect: vec![None, Some("same".into())],
            pages: vec![
                (vec![custody(NOW - 10)], Some("same".into())),
                (vec![custody(NOW - 10)], Some("same".into())),
            ],
            calls: std::sync::Mutex::new(0),
        };

        let err = block_on(read_full_custody(&seam)).unwrap_err();
        assert!(err.contains("advanced no cursor"), "got: {err}");
    }

    #[test]
    fn an_empty_custody_page_claiming_more_is_an_error() {
        let seam = PagedDestination {
            expect: vec![None],
            pages: vec![(vec![], Some("p1".into()))],
            calls: std::sync::Mutex::new(0),
        };

        assert!(block_on(read_full_custody(&seam)).is_err());
    }

    /// The repeated-cursor guard above remembers exactly one step back, so a
    /// destination alternating two (or more) distinct cursors passes it on
    /// every page — `next` never equals the cursor just sent — while never
    /// draining. Only the page-count ceiling in
    /// [`crate::cursor`] stops this one. `read_full_custody` is reached in
    /// production from the audit pass (`audit_destination` → this fn), so an
    /// unbounded walk here hangs a background loop, not just a UI read.
    #[test]
    fn a_two_cursor_cycle_is_bounded_not_infinite() {
        let mut expect = vec![None];
        let mut pages = Vec::new();
        for i in 0..crate::cursor::MAX_PAGES_PER_DESTINATION_WALK {
            let next = if i % 2 == 0 { "b" } else { "a" };
            pages.push((vec![custody(NOW - i as i64)], Some(next.to_string())));
            expect.push(Some(next.to_string()));
        }
        let seam = PagedDestination {
            expect,
            pages,
            calls: std::sync::Mutex::new(0),
        };

        let err = block_on(read_full_custody(&seam)).unwrap_err();
        assert!(
            err.contains(&format!(
                "exceeded {} pages",
                crate::cursor::MAX_PAGES_PER_DESTINATION_WALK
            )),
            "got: {err}"
        );
    }

    #[test]
    fn the_audit_verdict_is_computed_over_the_walked_pages_not_the_first_one() {
        // Freshness fails only if the destination high-water — the max
        // `updated_at` over the FULL walk — lags. Rows are served newest-first,
        // so this arranges the pages adversarially: a walk that silently
        // stopped after page one would see the same high-water but hand
        // downstream logic a truncated set; one that dropped the read entirely
        // would escalate instead of judging. The real verdict over all pages is
        // what the storm defect used to make impossible.
        let seam = PagedDestination {
            expect: vec![None, Some("p1".into())],
            pages: vec![
                (vec![custody(NOW - 10 * DAY)], Some("p1".into())),
                (vec![custody(NOW - 11 * DAY)], None),
            ],
            calls: std::sync::Mutex::new(0),
        };

        let (verdict, _) = block_on(audit_destination(
            &seam,
            &all_present(),
            "wss://dest.example",
            &AuditInputs {
                local_high_water: Some(NOW),
                added_at: 0,
            },
            &DestinationAuditState::default(),
            &AttachedMirrorSets::new(),
            &NoSourceVouch,
            NOW,
        ));

        assert!(
            matches!(verdict, AuditVerdict::FreshnessFailure { .. }),
            "a real verdict over the complete walk, got {verdict:?}"
        );
        assert_eq!(*seam.calls.lock().unwrap(), 2, "both pages were walked");
    }

    // ── the ledger-anchored population ──────────────
    //
    // Both arms used to draw their population from `custody.list` — the
    // audited party's own statement — so a destination that dropped older
    // custody and de-listed it passed. The population is now anchored in each
    // reserved set's owner-sealed `manifest.<kind>` ledger, which the
    // destination serves but cannot edit. These run the REAL open over a
    // content-addressed corpus, like the inclusion tests above: "de-listed" is
    // a path the ledger names and the corpus has no custody row for, and
    // "retained" is a real generation row whose bytes the walk fetches.

    use fauna_protocol::segments::SegmentRef;

    /// A content-addressed corpus — manifests and chunks by hash — so one test
    /// can serve MANY distinct plaintext records: the segments a ledger names
    /// AND the ledger blob itself.
    #[derive(Default)]
    struct Corpus {
        /// Keyed by the address's hex — `ContentHash` is a CID with no ordering.
        manifests: HashMap<String, Vec<u8>>,
        chunks: HashMap<String, Vec<u8>>,
    }

    fn address_hex(hash: &ContentHash) -> String {
        hex::encode(hash.digest())
    }

    impl Corpus {
        /// Store `payload` as a one-chunk plaintext record; the returned hex is
        /// the manifest address a custody row carries for it.
        fn add(&mut self, payload: &[u8]) -> String {
            let manifest = fauna_core::chunk::ChunkManifest {
                file_hash: ContentHash::of_raw(payload),
                total_size: payload.len() as u64,
                chunk_hashes: vec![ContentHash::of_raw(payload)],
                chunk_sizes: vec![payload.len() as u64],
                stored_hashes: None,
                sealed_hashes: None,
                min_reader: None,
            };
            let bytes = fauna_core::encoding::canonical_encode(&manifest).expect("encode manifest");
            let address = address_hex(&ContentHash::of_raw(&bytes));
            self.manifests.insert(address.clone(), bytes);
            self.chunks
                .insert(address_hex(&ContentHash::of_raw(payload)), payload.to_vec());
            address
        }

        /// The address `payload` WOULD have — for a row pointing at bytes this
        /// corpus does not hold, a genuine 404.
        fn absent(payload: &[u8]) -> String {
            Corpus::default().add(payload)
        }
    }

    #[async_trait]
    impl BlobFetcher for Corpus {
        async fn fetch_manifest(&self, hash: &ContentHash) -> anyhow::Result<Vec<u8>> {
            self.manifests
                .get(&address_hex(hash))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("404 no such manifest"))
        }

        async fn fetch_chunks(
            &self,
            store_keys: &[ContentHash],
            _relative_path: &str,
        ) -> anyhow::Result<Vec<Vec<u8>>> {
            store_keys
                .iter()
                .map(|k| {
                    self.chunks
                        .get(&address_hex(k))
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("404 no such chunk"))
                })
                .collect()
        }
    }

    /// An inclusion source over a [`Corpus`], keyed as the owner, holding the
    /// replica indexes a shell would supply — and recording which sets it was
    /// asked for.
    struct CorpusInclusion {
        corpus: Arc<Corpus>,
        indexes: BTreeMap<String, FolderIndex>,
        asked: std::sync::Mutex<Vec<String>>,
    }

    impl CorpusInclusion {
        fn new(corpus: Corpus) -> Self {
            Self {
                corpus: Arc::new(corpus),
                indexes: BTreeMap::new(),
                asked: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn with_index(mut self, folder_set: &str, index: FolderIndex) -> Self {
            self.indexes.insert(folder_set.to_string(), index);
            self
        }
    }

    impl BackupInclusionSource for CorpusInclusion {
        fn fetcher(&self, _destination_nest_url: &str) -> Arc<dyn BlobFetcher> {
            let fetcher: Arc<dyn BlobFetcher> = self.corpus.clone();
            fetcher
        }
        fn keys(&self) -> FileDownloadKeys {
            FileDownloadKeys::owner(fauna_core::crypto::NestBackupKey::derive(&[7u8; 32]))
        }
        fn folder_index(&self, folder_set: &str) -> Option<FolderIndex> {
            self.asked.lock().unwrap().push(folder_set.to_string());
            self.indexes.get(folder_set).cloned()
        }
    }

    const SCOPE: &str = "4242";

    /// The plaintext standing in for segment `id`'s `.dat` — distinct per id,
    /// so every segment has its own address.
    fn segment_payload(id: u32) -> Vec<u8> {
        format!("segment {id}'s bytes").into_bytes()
    }

    /// The one sidecar body every fixture segment carries (its bytes are not
    /// what these tests are about; the ledger naming the path is).
    const SIDECAR: &[u8] = b"the sidecar";

    fn segment_ref(id: u32) -> SegmentRef {
        SegmentRef {
            segment_id: id,
            blake3_hex: hex::encode(ContentHash::of_raw(&segment_payload(id)).digest()),
            bucket: "2026-09".into(),
            record_count: 1,
            tombstone_count: 0,
            size_bytes: 1,
            created_at_secs: NOW as u64,
            is_open: false,
            meta_blake3_hex: hex::encode(ContentHash::of_raw(SIDECAR).digest()),
            ..Default::default()
        }
    }

    /// The ledger bytes naming `refs`, exactly as the source nest writes them.
    fn ledger_bytes(refs: &[SegmentRef]) -> Vec<u8> {
        LiveManifestMirror {
            next_segment_id_seen: refs.iter().map(|r| r.segment_id).max().map_or(0, |m| m + 1),
            live: refs.to_vec(),
            extra: BTreeMap::new(),
        }
        .to_bytes()
        .unwrap()
    }

    fn path_hash_of(path: &str) -> String {
        hex::encode(ContentHash::of_raw(path.as_bytes()).digest())
    }

    /// A reserved-set custody row at `path`, addressing `manifest_hex`.
    fn row(path: &str, manifest_hex: String) -> CustodyItem {
        CustodyItem {
            folder_name: "__mail".into(),
            path: Some(path.to_string()),
            path_hash: path_hash_of(path),
            manifest_hash: manifest_hex,
            size_bytes: 1,
            updated_at: NOW,
            ..Default::default()
        }
    }

    /// A retained generation of `path`, superseded an hour ago.
    fn retained(path: &str, manifest_hex: String) -> GenerationItem {
        GenerationItem {
            folder_name: "__mail".into(),
            path: Some(path.to_string()),
            path_hash: path_hash_of(path),
            manifest_hash: manifest_hex,
            size_bytes: 1,
            superseded_at: NOW - HOUR,
            extra: Default::default(),
        }
    }

    /// A destination that lists `listed` of the segments `0..n` its own ledger
    /// names, every listed byte intact and every stamp fresh — the de-listing
    /// shape when `listed` is a strict subrange. Each listed
    /// segment is both halves unless `shed_meta`, which keeps the `.dat`s alone.
    fn de_listing_destination(
        n: u32,
        listed: std::ops::Range<u32>,
        family: SegmentFamily,
        shed_meta: bool,
    ) -> (Corpus, Vec<CustodyItem>) {
        let mut corpus = Corpus::default();
        let refs: Vec<SegmentRef> = (0..n).map(segment_ref).collect();
        let mut custody = Vec::new();
        for id in listed {
            let address = corpus.add(&segment_payload(id));
            custody.push(row(&family.dat_path(SCOPE, id), address));
            if !shed_meta {
                let meta = corpus.add(SIDECAR);
                custody.push(row(&family.meta_path(SCOPE, id), meta));
            }
        }
        let ledger = corpus.add(&ledger_bytes(&refs));
        custody.push(row(&family.mirror_path(SCOPE, "mail"), ledger));
        (corpus, custody)
    }

    fn inclusion_over(
        corpus: Corpus,
        custody: &[CustodyItem],
        retained: &[GenerationItem],
    ) -> AuditVerdict {
        block_on(evaluate_inclusion(
            &CorpusInclusion::new(corpus),
            DEST_URL,
            custody,
            retained,
            &AttachedMirrorSets::new(),
            NOW,
        ))
    }

    /// The headline: a destination that lists k of the n records
    /// its own sealed ledger names — every listed byte intact, every stamp
    /// fresh — used to pass both arms. Each de-listed path is now a miss found
    /// with certainty, not at 81 %.
    #[test]
    fn a_destination_that_de_lists_what_its_own_ledger_names_fails_inclusion() {
        let (corpus, custody) = de_listing_destination(40, 32..40, SegmentFamily::Content, false);
        match inclusion_over(corpus, &custody, &[]) {
            AuditVerdict::InclusionFailure { missing, sampled } => {
                assert_eq!(
                    missing, 64,
                    "both halves of each of the 32 de-listed segments are certain misses"
                );
                assert!(sampled >= missing);
            }
            other => panic!("expected InclusionFailure, got {other:?}"),
        }
    }

    /// The same destination with nothing de-listed passes: the anchor adds no
    /// false alarm to an honest set.
    #[test]
    fn a_destination_listing_everything_its_ledger_names_passes() {
        let (corpus, custody) = de_listing_destination(40, 0..40, SegmentFamily::Content, false);
        assert_eq!(inclusion_over(corpus, &custody, &[]), AuditVerdict::Passed);
    }

    /// The ledger names the `.meta` path too: a destination that keeps every
    /// `.dat` and sheds the sidecars has lost the half nothing can reopen a
    /// segment without.
    #[test]
    fn the_ledger_names_the_sidecar_half_too() {
        let (corpus, custody) = de_listing_destination(4, 0..4, SegmentFamily::Content, true);
        match inclusion_over(corpus, &custody, &[]) {
            AuditVerdict::InclusionFailure { missing, .. } => {
                assert_eq!(missing, 4, "one missing `.meta` per segment")
            }
            other => panic!("expected InclusionFailure, got {other:?}"),
        }
    }

    /// The compaction window: the coordinator tombstones compacted-out paths
    /// first and re-uploads the ledger last, so the live ledger legitimately
    /// names paths that are now retained generations. Retained is custody —
    /// and the retained bytes are sampled, not taken on the row's word.
    #[test]
    fn a_just_compacted_path_the_destination_still_retains_is_custody() {
        let (mut corpus, custody) =
            de_listing_destination(10, 5..10, SegmentFamily::Content, false);
        let retained_rows: Vec<GenerationItem> = (0..5)
            .flat_map(|id| {
                let address = corpus.add(&segment_payload(id));
                let meta = corpus.add(SIDECAR);
                [
                    retained(&SegmentFamily::Content.dat_path(SCOPE, id), address),
                    retained(&SegmentFamily::Content.meta_path(SCOPE, id), meta),
                ]
            })
            .collect();
        assert_eq!(
            inclusion_over(corpus, &custody, &retained_rows),
            AuditVerdict::Passed
        );
    }

    /// A retained row whose bytes are gone is the de-listing move wearing a
    /// tombstone: the generation joins the population and fails the open.
    #[test]
    fn a_retained_generation_without_its_bytes_is_still_missing() {
        // Small enough that the whole population fits one sample (K = 16).
        let (mut corpus, custody) = de_listing_destination(4, 2..4, SegmentFamily::Content, false);
        // The sidecars are retained intact; only the `.dat`s' bytes are gone.
        let retained_rows: Vec<GenerationItem> = (0..2)
            .flat_map(|id| {
                let meta = corpus.add(SIDECAR);
                [
                    retained(
                        &SegmentFamily::Content.dat_path(SCOPE, id),
                        Corpus::absent(&segment_payload(id)),
                    ),
                    retained(&SegmentFamily::Content.meta_path(SCOPE, id), meta),
                ]
            })
            .collect();
        match inclusion_over(corpus, &custody, &retained_rows) {
            AuditVerdict::InclusionFailure { missing, sampled } => {
                assert_eq!(missing, 2);
                assert_eq!(
                    sampled, 9,
                    "4 live halves + the ledger + 4 retained halves, all sampled (≤ K)"
                );
            }
            other => panic!("expected InclusionFailure, got {other:?}"),
        }
    }

    /// A set whose ledger the destination cannot produce is unverifiable —
    /// hiding the ledger is exactly the de-listing move — so it fails outright,
    /// however intact the rows it does list are.
    #[test]
    fn a_ledger_the_destination_cannot_produce_fails_its_set() {
        let mut corpus = Corpus::default();
        let mut custody: Vec<CustodyItem> = (0..40)
            .map(|id| {
                let address = corpus.add(&segment_payload(id));
                row(&SegmentFamily::Content.dat_path(SCOPE, id), address)
            })
            .collect();
        custody.push(row(
            &SegmentFamily::Content.mirror_path(SCOPE, "mail"),
            Corpus::absent(b"the ledger"),
        ));
        assert!(matches!(
            inclusion_over(corpus, &custody, &[]),
            AuditVerdict::InclusionFailure { .. }
        ));
    }

    /// Bytes at the ledger's path that do not decode as a ledger are no ledger:
    /// a destination cannot substitute an openable-but-meaningless blob for the
    /// source's list and thereby be reconciled against nothing.
    #[test]
    fn a_ledger_that_does_not_decode_fails_its_set() {
        let mut corpus = Corpus::default();
        let mut custody: Vec<CustodyItem> = (0..8)
            .map(|id| {
                let address = corpus.add(&segment_payload(id));
                row(&SegmentFamily::Content.dat_path(SCOPE, id), address)
            })
            .collect();
        custody.push(row(
            &SegmentFamily::Content.mirror_path(SCOPE, "mail"),
            corpus.add(b"not a ledger at all"),
        ));
        assert!(matches!(
            inclusion_over(corpus, &custody, &[]),
            AuditVerdict::InclusionFailure { .. }
        ));
    }

    /// The placement journal is its own family with its own ledger, one path
    /// component deeper: de-listing journal segments is caught the same way,
    /// and the content family's ledger says nothing about them.
    #[test]
    fn a_journal_ledger_covers_its_own_family() {
        let (mut corpus, mut custody) =
            de_listing_destination(3, 0..3, SegmentFamily::Content, false);
        // The journal: three segments named, only the newest listed.
        let journal_refs: Vec<SegmentRef> = (1..=3).map(segment_ref).collect();
        let newest = corpus.add(&segment_payload(3));
        custody.push(row(&SegmentFamily::Placement.dat_path(SCOPE, 3), newest));
        let newest_meta = corpus.add(SIDECAR);
        custody.push(row(
            &SegmentFamily::Placement.meta_path(SCOPE, 3),
            newest_meta,
        ));
        let journal_ledger = corpus.add(&ledger_bytes(&journal_refs));
        custody.push(row(
            &SegmentFamily::Placement.mirror_path(SCOPE, "mail"),
            journal_ledger,
        ));
        match inclusion_over(corpus, &custody, &[]) {
            AuditVerdict::InclusionFailure { missing, .. } => {
                assert_eq!(missing, 4, "both halves of journal segments 1 and 2")
            }
            other => panic!("expected InclusionFailure, got {other:?}"),
        }
    }

    /// Which rows are ledgers is the client's decision: a path that merely
    /// looks like one (`manifest.<not-a-kind>`) is the audited party's own text,
    /// and is sampled as an ordinary row rather than decoded as a set's list.
    #[test]
    fn a_path_that_only_looks_like_a_ledger_is_an_ordinary_row() {
        let mut corpus = Corpus::default();
        let mut custody: Vec<CustodyItem> = (0..4)
            .map(|id| {
                let address = corpus.add(&segment_payload(id));
                row(&SegmentFamily::Content.dat_path(SCOPE, id), address)
            })
            .collect();
        custody.push(row(
            &format!("{SCOPE}/manifest.bogus"),
            corpus.add(b"not a ledger at all"),
        ));
        assert_eq!(inclusion_over(corpus, &custody, &[]), AuditVerdict::Passed);
    }

    /// End to end through `audit_destination`: the retained generations come
    /// from the destination's own `generation.list`, walked after freshness
    /// passed, and a pass over them advances the last-passed clock.
    #[test]
    fn the_de_listing_check_consults_the_destinations_retained_generations() {
        let (mut corpus, custody) =
            de_listing_destination(10, 5..10, SegmentFamily::Content, false);
        let retained_rows: Vec<GenerationItem> = (0..5)
            .flat_map(|id| {
                let address = corpus.add(&segment_payload(id));
                let meta = corpus.add(SIDECAR);
                [
                    retained(&SegmentFamily::Content.dat_path(SCOPE, id), address),
                    retained(&SegmentFamily::Content.meta_path(SCOPE, id), meta),
                ]
            })
            .collect();
        let seam = MockDestination {
            custody: Ok(custody),
            generations: Ok(retained_rows),
        };
        let (verdict, after) = block_on(audit_destination(
            &seam,
            &CorpusInclusion::new(corpus),
            DEST_URL,
            &inputs(Some(NOW), NOW - 30 * DAY),
            &state("d1", None),
            &AttachedMirrorSets::new(),
            &NoSourceVouch,
            NOW,
        ));
        assert_eq!(verdict, AuditVerdict::Passed);
        assert_eq!(after.last_passed_at, Some(NOW));
    }

    /// A generation list the destination will not answer is a read we could
    /// not make — `Unreachable`, never a verdict — and the last-passed clock
    /// does not move.
    #[test]
    fn an_unanswered_generation_list_is_unreachable_not_a_verdict() {
        let (corpus, custody) = de_listing_destination(4, 0..4, SegmentFamily::Content, false);
        let seam = MockDestination {
            custody: Ok(custody),
            generations: Err("frame too large".into()),
        };
        let (verdict, after) = block_on(audit_destination(
            &seam,
            &CorpusInclusion::new(corpus),
            DEST_URL,
            &inputs(Some(NOW), NOW - DAY),
            &state("d1", None),
            &AttachedMirrorSets::new(),
            &NoSourceVouch,
            NOW,
        ));
        assert!(
            matches!(verdict, AuditVerdict::Unreachable { .. }),
            "got {verdict:?}"
        );
        assert_eq!(after.last_passed_at, None);
    }

    // ── the covered-folder mirror plane's population ─────────────
    //
    // The mirror plane has no owner-sealed ledger at the destination, so its
    // anchor is the attaching client's OWN replica index of the folder — the
    // `(hex(path_hash), manifest_hash)` head per live path, as the sync engine
    // persisted it. These drive the real presence walk over a `Corpus`, so a
    // "miss" is a genuine absence at the destination and a "retained" head's
    // bytes are genuinely fetched.

    /// The covered set under test, attached a month ago (outside the slack).
    fn indexed_set() -> (String, AttachedMirrorSets) {
        let set = covered_folder_set();
        let attached: AttachedMirrorSets = [(set.clone(), NOW - 30 * DAY)].into_iter().collect();
        (set, attached)
    }

    /// The mirror plane's rest path for file `i`: `hex(path_hash)`, no `/`.
    fn mirror_path(i: u32) -> String {
        format!("{i:064x}")
    }

    /// The plaintext standing in for file `i`'s content-layer bytes.
    fn file_payload(i: u32) -> Vec<u8> {
        format!("file {i}'s ciphertext").into_bytes()
    }

    /// A mirror row of `set` at `path` addressing `manifest_hex`.
    fn mirror_row(set: &str, path: &str, manifest_hex: String) -> CustodyItem {
        CustodyItem {
            folder_name: set.into(),
            path: Some(path.to_string()),
            path_hash: path_hash_of(path),
            manifest_hash: manifest_hex,
            size_bytes: 1,
            updated_at: NOW,
            ..Default::default()
        }
    }

    /// A retained generation of `set`'s `path`, superseded an hour ago.
    fn mirror_retained(set: &str, path: &str, manifest_hex: String) -> GenerationItem {
        GenerationItem {
            folder_name: set.into(),
            path: Some(path.to_string()),
            path_hash: path_hash_of(path),
            manifest_hash: manifest_hex,
            size_bytes: 1,
            superseded_at: NOW - HOUR,
            extra: Default::default(),
        }
    }

    /// The client's replica index naming files `0..n`, every head recorded by
    /// the source a week ago and the replica clean-passed an hour ago — a
    /// witness the arm must honour.
    fn replica_index(n: u32) -> FolderIndex {
        FolderIndex {
            consistent_at: NOW - HOUR,
            entries: (0..n)
                .map(|i| FolderIndexEntry {
                    path_hash_hex: mirror_path(i),
                    manifest_hash_hex: Corpus::absent(&file_payload(i)),
                    recorded_at: NOW - 7 * DAY,
                })
                .collect(),
        }
    }

    /// A destination listing `listed` of the files `0..n` the client's index
    /// names — every listed byte intact, every stamp fresh — the de-listing
    /// shape, one plane over.
    fn de_listing_mirror(set: &str, listed: std::ops::Range<u32>) -> (Corpus, Vec<CustodyItem>) {
        let mut corpus = Corpus::default();
        let custody = listed
            .map(|i| {
                let address = corpus.add(&file_payload(i));
                mirror_row(set, &mirror_path(i), address)
            })
            .collect();
        (corpus, custody)
    }

    fn inclusion_with_index(
        inclusion: CorpusInclusion,
        custody: &[CustodyItem],
        retained: &[GenerationItem],
        attached: &AttachedMirrorSets,
    ) -> AuditVerdict {
        block_on(evaluate_inclusion(
            &inclusion, DEST_URL, custody, retained, attached, NOW,
        ))
    }

    /// **The headline:** a destination that lists k of the n
    /// mirror rows the client's own folder index names — every listed byte
    /// intact, every stamp fresh — used to pass this plane on presence over its
    /// own list. Each de-listed path is now a miss found with certainty.
    #[test]
    fn a_destination_that_de_lists_what_the_clients_own_folder_index_names_fails_inclusion() {
        let (set, attached) = indexed_set();
        let (corpus, custody) = de_listing_mirror(&set, 32..40);
        let inclusion = CorpusInclusion::new(corpus).with_index(&set, replica_index(40));
        match inclusion_with_index(inclusion, &custody, &[], &attached) {
            AuditVerdict::InclusionFailure { missing, sampled } => {
                assert_eq!(
                    missing, 32,
                    "each of the 32 de-listed files is a certain miss"
                );
                assert!(sampled >= missing);
            }
            other => panic!("expected InclusionFailure, got {other:?}"),
        }
    }

    /// The same destination holding everything the index names passes: the
    /// anchor adds no false alarm to an honest mirror.
    #[test]
    fn a_destination_holding_everything_the_index_names_passes() {
        let (set, attached) = indexed_set();
        let (corpus, custody) = de_listing_mirror(&set, 0..40);
        let inclusion = CorpusInclusion::new(corpus).with_index(&set, replica_index(40));
        assert_eq!(
            inclusion_with_index(inclusion, &custody, &[], &attached),
            AuditVerdict::Passed
        );
    }

    /// A head the source recorded inside the slack may not have been swept to
    /// the destination yet: not expected, so not a miss. The mirror plane's
    /// per-folder freshness is exactly this window — the same slack the
    /// reserved rails get.
    #[test]
    fn a_head_recorded_inside_the_slack_is_not_expected_at_the_destination_yet() {
        let (set, attached) = indexed_set();
        let (corpus, custody) = de_listing_mirror(&set, 0..4);
        let mut index = replica_index(6);
        for entry in &mut index.entries[4..] {
            entry.recorded_at = NOW - HOUR;
        }
        let inclusion = CorpusInclusion::new(corpus).with_index(&set, index);
        assert_eq!(
            inclusion_with_index(inclusion, &custody, &[], &attached),
            AuditVerdict::Passed
        );
    }

    /// …and once the slack has elapsed the same absent head IS a miss: a
    /// destination lagging the client's replica by more than the slack is the
    /// freshness failure this plane never had.
    #[test]
    fn a_head_absent_past_the_slack_is_a_miss() {
        let (set, attached) = indexed_set();
        let (corpus, custody) = de_listing_mirror(&set, 0..4);
        let mut index = replica_index(6);
        for entry in &mut index.entries[4..] {
            entry.recorded_at = NOW - FRESHNESS_SLACK_SECS - 1;
        }
        let inclusion = CorpusInclusion::new(corpus).with_index(&set, index);
        assert!(matches!(
            inclusion_with_index(inclusion, &custody, &[], &attached),
            AuditVerdict::InclusionFailure { missing: 2, .. }
        ));
    }

    /// A folder attached inside the slack legitimately has nothing mirrored
    /// yet — the `added_at` floor, exactly as freshness floors a just-enrolled
    /// destination. The index's old heads are not expected until the slack
    /// after the attach.
    #[test]
    fn a_folder_attached_inside_the_slack_is_not_expected_yet() {
        let set = covered_folder_set();
        let attached: AttachedMirrorSets = [(set.clone(), NOW - HOUR)].into_iter().collect();
        let (corpus, custody) = de_listing_mirror(&set, 0..2);
        let inclusion = CorpusInclusion::new(corpus).with_index(&set, replica_index(40));
        assert_eq!(
            inclusion_with_index(inclusion, &custody, &[], &attached),
            AuditVerdict::Passed
        );
    }

    /// A head the destination has since superseded rests as a retained
    /// generation inside `T`: custody — and its bytes are sampled, not taken on
    /// the row's word.
    #[test]
    fn a_superseded_head_the_destination_still_retains_is_custody() {
        let (set, attached) = indexed_set();
        let mut corpus = Corpus::default();
        // Live: the newer head M2 the client has not pulled yet.
        let newer = corpus.add(b"file 0, newer");
        let custody = vec![mirror_row(&set, &mirror_path(0), newer)];
        // Retained: the head M1 the client's index names.
        let older = corpus.add(&file_payload(0));
        let retained_rows = vec![mirror_retained(&set, &mirror_path(0), older)];
        let inclusion = CorpusInclusion::new(corpus).with_index(&set, replica_index(1));
        assert_eq!(
            inclusion_with_index(inclusion, &custody, &retained_rows, &attached),
            AuditVerdict::Passed
        );
    }

    /// The same retained head with its bytes gone is the de-listing move
    /// wearing a tombstone: the generation joins the population and fails
    /// presence.
    #[test]
    fn a_retained_head_without_its_bytes_is_still_missing() {
        let (set, attached) = indexed_set();
        let mut corpus = Corpus::default();
        let newer = corpus.add(b"file 0, newer");
        let custody = vec![mirror_row(&set, &mirror_path(0), newer)];
        let retained_rows = vec![mirror_retained(
            &set,
            &mirror_path(0),
            Corpus::absent(&file_payload(0)),
        )];
        let inclusion = CorpusInclusion::new(corpus).with_index(&set, replica_index(1));
        match inclusion_with_index(inclusion, &custody, &retained_rows, &attached) {
            AuditVerdict::InclusionFailure { missing, sampled } => {
                assert_eq!(missing, 1);
                assert_eq!(
                    sampled, 2,
                    "the live row and the retained head, both sampled"
                );
            }
            other => panic!("expected InclusionFailure, got {other:?}"),
        }
    }

    /// A path listed live under a manifest this client never recorded, with no
    /// retained generation of the head it did record, is a miss: a substituted
    /// row is self-consistent under presence and only the client's own index
    /// can tell it from the real head.
    #[test]
    fn a_live_row_under_a_manifest_the_index_never_recorded_is_a_miss() {
        let (set, attached) = indexed_set();
        let mut corpus = Corpus::default();
        let substituted = corpus.add(b"not the bytes the client recorded");
        let custody = vec![mirror_row(&set, &mirror_path(0), substituted)];
        let inclusion = CorpusInclusion::new(corpus).with_index(&set, replica_index(1));
        assert!(matches!(
            inclusion_with_index(inclusion, &custody, &[], &attached),
            AuditVerdict::InclusionFailure { missing: 1, .. }
        ));
    }

    /// A replica not known consistent with the source inside the slack cannot
    /// witness — its heads may have been superseded so long ago that the
    /// destination legitimately no longer retains them — so that set falls
    /// back to the destination's list. The stated residual, pinned: the
    /// de-listing passes on this pass rather than raising a false alarm.
    #[test]
    fn a_replica_not_recently_consistent_with_the_source_cannot_witness() {
        let (set, attached) = indexed_set();
        let (corpus, custody) = de_listing_mirror(&set, 32..40);
        let mut index = replica_index(40);
        index.consistent_at = NOW - FRESHNESS_SLACK_SECS - 1;
        let inclusion = CorpusInclusion::new(corpus).with_index(&set, index);
        assert_eq!(
            inclusion_with_index(inclusion, &custody, &[], &attached),
            AuditVerdict::Passed
        );
    }

    /// A shell holding no replica of the folder — web by construction, a native
    /// device with no seat on it — declares the absence with `None`, and the
    /// plane keeps its floor: hash-verified presence over the destination's
    /// list. The other stated residual, pinned the same way.
    #[test]
    fn a_shell_with_no_folder_index_keeps_the_destinations_list_as_the_population() {
        let (set, attached) = indexed_set();
        let (corpus, custody) = de_listing_mirror(&set, 32..40);
        let inclusion = CorpusInclusion::new(corpus);
        assert_eq!(
            inclusion_with_index(inclusion, &custody, &[], &attached),
            AuditVerdict::Passed
        );
    }

    /// The index is asked for only the sets this client attached to THIS
    /// destination: a `__folder/…` name in the destination's reply that the
    /// client never attached is routed to the full open without
    /// the shell ever being asked to look a replica up for it — the anchor
    /// can never widen the routing input it hangs off.
    #[test]
    fn the_index_is_asked_for_only_the_sets_this_client_attached() {
        let (set, attached) = indexed_set();
        let (corpus, custody) = de_listing_mirror(&set, 0..2);
        let inclusion = CorpusInclusion::new(corpus).with_index(&set, replica_index(2));
        let asked = Arc::new(inclusion);
        block_on(evaluate_inclusion(
            asked.as_ref(),
            DEST_URL,
            &custody,
            &[],
            &attached,
            NOW,
        ));
        assert_eq!(*asked.asked.lock().unwrap(), vec![set.clone()]);

        let unattached = AttachedMirrorSets::new();
        let (corpus, custody) = de_listing_mirror(&set, 0..2);
        let inclusion = CorpusInclusion::new(corpus).with_index(&set, replica_index(2));
        let asked = Arc::new(inclusion);
        block_on(evaluate_inclusion(
            asked.as_ref(),
            DEST_URL,
            &custody,
            &[],
            &unattached,
            NOW,
        ));
        assert!(
            asked.asked.lock().unwrap().is_empty(),
            "a set this client never attached must not be looked up"
        );
    }

    /// End to end through `audit_destination`: the retained generations come
    /// from the destination's own `generation.list`, the coverage floor from
    /// the client's attachment record, and the de-listed heads fail the pass
    /// while a superseded-and-retained one does not.
    #[test]
    fn the_folder_index_check_runs_inside_the_destination_audit() {
        let (set, attached) = indexed_set();
        let (mut corpus, mut custody) = de_listing_mirror(&set, 1..8);
        // File 0: superseded at the destination, its recorded head retained.
        let newer = corpus.add(b"file 0, newer");
        custody.push(mirror_row(&set, &mirror_path(0), newer));
        let older = corpus.add(&file_payload(0));
        let retained_rows = vec![mirror_retained(&set, &mirror_path(0), older)];
        let seam = MockDestination {
            custody: Ok(custody),
            generations: Ok(retained_rows),
        };
        let inclusion = CorpusInclusion::new(corpus).with_index(&set, replica_index(10));
        let (verdict, after) = block_on(audit_destination(
            &seam,
            &inclusion,
            DEST_URL,
            &inputs(Some(NOW), NOW - 30 * DAY),
            &state("d1", None),
            &attached,
            &NoSourceVouch,
            NOW,
        ));
        assert!(
            matches!(verdict, AuditVerdict::InclusionFailure { missing: 2, .. }),
            "files 8 and 9 are de-listed; file 0 is retained custody — got {verdict:?}"
        );
        assert_eq!(after.last_passed_at, None);
    }

    // ── the generation pin ─────────────────────

    /// A source nest's answer to "what is your counter now?" — fixed per test.
    struct Vouch(Result<u32, String>);

    #[async_trait]
    impl SourceLedgerVouch for Vouch {
        async fn next_segment_id(&self, _: &str, _: &str) -> Result<u32, String> {
            self.0.clone()
        }
    }

    /// The ledger bytes naming `refs` at generation `counter` — what a source
    /// whose top segment compacted away writes: a counter above `max(id) + 1`.
    fn ledger_bytes_at(refs: &[SegmentRef], counter: u32) -> Vec<u8> {
        LiveManifestMirror {
            next_segment_id_seen: counter,
            live: refs.to_vec(),
            extra: BTreeMap::new(),
        }
        .to_bytes()
        .unwrap()
    }

    /// A destination listing segments `0..n` intact, under a ledger at
    /// `counter` — every byte present, every stamp fresh: a genuine state.
    fn genuine_destination(n: u32, counter: u32) -> (Corpus, Vec<CustodyItem>) {
        let mut corpus = Corpus::default();
        let refs: Vec<SegmentRef> = (0..n).map(segment_ref).collect();
        let mut custody = Vec::new();
        for id in 0..n {
            let address = corpus.add(&segment_payload(id));
            custody.push(row(&SegmentFamily::Content.dat_path(SCOPE, id), address));
            let meta = corpus.add(SIDECAR);
            custody.push(row(&SegmentFamily::Content.meta_path(SCOPE, id), meta));
        }
        let ledger = corpus.add(&ledger_bytes_at(&refs, counter));
        custody.push(row(
            &SegmentFamily::Content.mirror_path(SCOPE, "mail"),
            ledger,
        ));
        (corpus, custody)
    }

    /// The content ledger's pin key at this destination.
    fn mail_ledger_key() -> String {
        ledger_key("__mail", &SegmentFamily::Content.mirror_path(SCOPE, "mail"))
    }

    /// The generation one pin holds, if the key is pinned.
    fn generation_of(pins: &BTreeMap<String, LedgerPin>, key: &str) -> Option<u32> {
        pins.get(key).map(|p| p.generation)
    }

    fn pinned_inclusion(
        corpus: Corpus,
        custody: &[CustodyItem],
        pins: &BTreeMap<String, LedgerPin>,
        vouch: &dyn SourceLedgerVouch,
    ) -> InclusionOutcome {
        pinned_inclusion_at(corpus, custody, &[], pins, 30 * DAY, NOW, vouch)
    }

    /// [`pinned_inclusion`] with the retained generations, the window the
    /// destination reports and the pass's clock in the caller's hands.
    fn pinned_inclusion_at(
        corpus: Corpus,
        custody: &[CustodyItem],
        retained: &[GenerationItem],
        pins: &BTreeMap<String, LedgerPin>,
        grace_secs: i64,
        now: i64,
        vouch: &dyn SourceLedgerVouch,
    ) -> InclusionOutcome {
        block_on(evaluate_inclusion_pinned(
            &CorpusInclusion::new(corpus),
            DEST_URL,
            custody,
            retained,
            &AttachedMirrorSets::new(),
            now,
            pins,
            &BTreeMap::new(),
            grace_secs,
            vouch,
        ))
    }

    /// A first sight records the generation served; an honest later pass at a
    /// higher one advances it. Nothing is asked of the source on either.
    #[test]
    fn an_opened_ledger_pins_its_generation_and_a_higher_one_advances_it() {
        let (corpus, custody) = genuine_destination(4, 4);
        let first = pinned_inclusion(corpus, &custody, &BTreeMap::new(), &NoSourceVouch);
        assert_eq!(first.verdict, AuditVerdict::Passed);
        assert_eq!(
            generation_of(&first.ledger_generations, &mail_ledger_key()),
            Some(4)
        );

        let (corpus, custody) = genuine_destination(6, 6);
        let second = pinned_inclusion(corpus, &custody, &first.ledger_generations, &NoSourceVouch);
        assert_eq!(second.verdict, AuditVerdict::Passed);
        assert_eq!(
            generation_of(&second.ledger_generations, &mail_ledger_key()),
            Some(6)
        );
    }

    /// The headline: a destination that verified at generation 40 and later
    /// serves the genuine generation-30 ledger plus exactly the segments it
    /// names — the population anchor's blind spot — is a rollback. The source
    /// still stands at 40, so the destination is the party that went backwards:
    /// a certain miss, and the pin does not move.
    #[test]
    fn a_destination_serving_an_older_genuine_ledger_after_a_newer_one_was_verified_fails() {
        let (corpus, custody) = genuine_destination(40, 40);
        let pins =
            pinned_inclusion(corpus, &custody, &BTreeMap::new(), &NoSourceVouch).ledger_generations;
        assert_eq!(generation_of(&pins, &mail_ledger_key()), Some(40));

        let (corpus, custody) = genuine_destination(30, 30);
        let out = pinned_inclusion(corpus, &custody, &pins, &Vouch(Ok(40)));
        match out.verdict {
            AuditVerdict::InclusionFailure { missing, .. } => {
                assert!(missing >= 1, "the regressed ledger is a certain miss")
            }
            other => panic!("expected InclusionFailure, got {other:?}"),
        }
        assert_eq!(
            generation_of(&out.ledger_generations, &mail_ledger_key()),
            Some(40),
            "a rollback never lowers the pin"
        );
    }

    /// The correction the judge insisted on: an honest source whose TOP segment
    /// compacted to nothing lists a lower maximum while its saved counter
    /// stands, and the ledger it writes carries that counter. Against the pin
    /// that is no regression — and a ledger built the OLD way (greatest live
    /// id plus one) would have been one, which is why the writers carry the
    /// saved counter now.
    #[test]
    fn an_honest_empty_output_compaction_of_the_top_segment_does_not_alarm() {
        let (corpus, custody) = genuine_destination(40, 40);
        let pins =
            pinned_inclusion(corpus, &custody, &BTreeMap::new(), &NoSourceVouch).ledger_generations;

        // Segment 39 retired without a successor: live 0..39, counter still 40.
        let (corpus, custody) = genuine_destination(39, 40);
        let out = pinned_inclusion(corpus, &custody, &pins, &Vouch(Ok(40)));
        assert_eq!(out.verdict, AuditVerdict::Passed);
        assert_eq!(
            generation_of(&out.ledger_generations, &mail_ledger_key()),
            Some(40)
        );

        // The same state under the old derivation reads as a rollback — the
        // false alarm the saved counter exists to prevent.
        let (corpus, custody) = genuine_destination(39, 39);
        let out = pinned_inclusion(corpus, &custody, &pins, &Vouch(Ok(40)));
        assert!(matches!(out.verdict, AuditVerdict::InclusionFailure { .. }));
    }

    /// A source that itself regressed — restored from an older copy, or
    /// re-seeded from a copy carrying a lower generation — vouches for the
    /// lower ledger: accepted, and the pin re-bases to what was served, so the
    /// next pass measures from there.
    #[test]
    fn a_source_that_itself_regressed_is_accepted_and_the_pin_follows_it() {
        let (corpus, custody) = genuine_destination(40, 40);
        let pins =
            pinned_inclusion(corpus, &custody, &BTreeMap::new(), &NoSourceVouch).ledger_generations;

        let (corpus, custody) = genuine_destination(30, 30);
        let out = pinned_inclusion(corpus, &custody, &pins, &Vouch(Ok(31)));
        assert_eq!(out.verdict, AuditVerdict::Passed);
        assert_eq!(
            generation_of(&out.ledger_generations, &mail_ledger_key()),
            Some(30)
        );
    }

    /// A regression the source cannot be asked about stays a rollback — loud,
    /// and re-asked next pass — rather than an acceptance the owner's nest
    /// being down would grant every hostile destination.
    #[test]
    fn a_regression_the_source_cannot_be_asked_about_is_a_certain_miss() {
        let (corpus, custody) = genuine_destination(40, 40);
        let pins =
            pinned_inclusion(corpus, &custody, &BTreeMap::new(), &NoSourceVouch).ledger_generations;

        let (corpus, custody) = genuine_destination(30, 30);
        let out = pinned_inclusion(
            corpus,
            &custody,
            &pins,
            &Vouch(Err("connection refused".into())),
        );
        assert!(matches!(out.verdict, AuditVerdict::InclusionFailure { .. }));
        assert_eq!(
            generation_of(&out.ledger_generations, &mail_ledger_key()),
            Some(40)
        );
    }

    /// End to end through `audit_destination`: the pin rides the persisted
    /// per-destination state — recorded by one pass, read by the next, kept
    /// across a pass that never reaches the ledgers.
    #[test]
    fn the_pin_rides_the_persisted_destination_state() {
        let (corpus, custody) = genuine_destination(40, 40);
        let (verdict, after) = block_on(audit_destination(
            &reachable(custody),
            &CorpusInclusion::new(corpus),
            DEST_URL,
            &inputs(Some(NOW), NOW - 30 * DAY),
            &state("d1", None),
            &AttachedMirrorSets::new(),
            &NoSourceVouch,
            NOW,
        ));
        assert_eq!(verdict, AuditVerdict::Passed);
        assert_eq!(
            generation_of(&after.verified_ledger_generations, &mail_ledger_key()),
            Some(40)
        );

        // A pass that cannot reach the destination keeps the pin as it was.
        let (verdict, kept) = block_on(audit_destination(
            &dead(),
            &all_present(),
            DEST_URL,
            &inputs(Some(NOW), NOW - 30 * DAY),
            &after,
            &AttachedMirrorSets::new(),
            &NoSourceVouch,
            NOW + DAY,
        ));
        assert!(matches!(verdict, AuditVerdict::Unreachable { .. }));
        assert_eq!(
            kept.verified_ledger_generations,
            after.verified_ledger_generations
        );

        // The rollback is caught from the persisted pin.
        let (corpus, custody) = genuine_destination(30, 30);
        let (verdict, _) = block_on(audit_destination(
            &reachable(custody),
            &CorpusInclusion::new(corpus),
            DEST_URL,
            &inputs(Some(NOW), NOW - 30 * DAY),
            &kept,
            &AttachedMirrorSets::new(),
            &Vouch(Ok(40)),
            NOW + 2 * DAY,
        ));
        assert!(matches!(verdict, AuditVerdict::InclusionFailure { .. }));
    }

    /// A state file without the pin field loads with an empty pin —
    /// one re-verification, never a refused load — and a pinned state round
    /// trips.
    #[test]
    fn a_state_file_without_the_pin_loads_with_an_empty_one() {
        let unpinned = r#"{"destination_id":"d1","last_passed_at":1,"last_attempt_at":2}"#;
        let loaded: DestinationAuditState = serde_json::from_str(unpinned).expect("loads");
        assert!(loaded.verified_ledger_generations.is_empty());
        assert!(
            loaded.accepted_regressions.is_empty(),
            "a state file without the field loads with no record"
        );

        let mut pinned = loaded.clone();
        pinned.verified_ledger_generations.insert(
            mail_ledger_key(),
            LedgerPin {
                generation: 40,
                seen_live_at: Some(NOW),
            },
        );
        pinned.accepted_regressions.insert(
            mail_ledger_key(),
            AcceptedRegression {
                pinned: 40,
                served: 30,
                observed_at: NOW,
                floored_at: Some(NOW),
                recoverable_until: Some(NOW + 30 * DAY),
            },
        );
        let json = serde_json::to_string(&pinned).expect("saves");
        let back: DestinationAuditState = serde_json::from_str(&json).expect("loads");
        assert_eq!(back, pinned);

        // A record written before the floor existed carries a bare deadline
        // and no `floored_at`: it loads, the floor still owed.
        let unfloored = r#"{"pinned":40,"served":30,"observed_at":5,"recoverable_until":9}"#;
        let record: AcceptedRegression = serde_json::from_str(unfloored).expect("loads");
        assert_eq!(record.floored_at, None);
        assert_eq!(record.recoverable_until, Some(9));
    }

    /// A state file from the one day the pin was a bare generation
    /// (2026-09-28) loads with no live sighting on record — the generation
    /// still guards against rollback, and the vanished-ledger rule treats the
    /// pin as unseen for longer than any window.
    #[test]
    fn a_state_file_with_a_bare_generation_pin_loads_without_a_sighting() {
        let key = mail_ledger_key();
        let bare = format!(
            r#"{{"destination_id":"d1","last_passed_at":1,"last_attempt_at":2,"verified_ledger_generations":{{"{key}":40}}}}"#
        );
        let loaded: DestinationAuditState = serde_json::from_str(&bare).expect("loads");
        assert_eq!(
            loaded.verified_ledger_generations.get(&key),
            Some(&LedgerPin {
                generation: 40,
                seen_live_at: None
            })
        );

        // Nothing of the set left and no sighting to measure from: dropped.
        let out = pinned_inclusion_at(
            Corpus::default(),
            &[],
            &[],
            &loaded.verified_ledger_generations,
            30 * DAY,
            NOW,
            &NoSourceVouch,
        );
        assert_eq!(out.verdict, AuditVerdict::Passed);
        assert!(out.ledger_generations.is_empty());
    }

    // ── the accepted-regression recovery notice ──

    /// A source nest that answers its counter and takes — or refuses — the
    /// floor, remembering every floor it was asked for.
    struct Flooring {
        counter: u32,
        lands: bool,
        asked: std::sync::Mutex<Vec<(String, String, u32)>>,
    }

    impl Flooring {
        fn new(counter: u32, lands: bool) -> Self {
            Self {
                counter,
                lands,
                asked: Default::default(),
            }
        }

        fn asked(&self) -> Vec<(String, String, u32)> {
            self.asked.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl SourceLedgerVouch for Flooring {
        async fn next_segment_id(&self, _: &str, _: &str) -> Result<u32, String> {
            Ok(self.counter)
        }

        async fn floor_counter(&self, kind: &str, scope: &str, floor: u32) -> Result<u32, String> {
            self.asked
                .lock()
                .unwrap()
                .push((kind.to_string(), scope.to_string(), floor));
            if self.lands {
                Ok(floor.max(self.counter))
            } else {
                Err("connection refused".into())
            }
        }
    }

    fn ids(range: std::ops::Range<u32>) -> Vec<u32> {
        range.collect()
    }

    /// A destination holding live rows for the segments `held` under a ledger
    /// at `counter` that names only `named` — every byte present. What a
    /// source that went backwards leaves when `held` reaches past `named`: the
    /// rows its rolled-back ledger no longer knows of.
    fn destination_holding(
        named: &[u32],
        held: &[u32],
        counter: u32,
    ) -> (Corpus, Vec<CustodyItem>) {
        let mut corpus = Corpus::default();
        let refs: Vec<SegmentRef> = named.iter().copied().map(segment_ref).collect();
        let mut custody = Vec::new();
        for &id in held {
            let address = corpus.add(&segment_payload(id));
            custody.push(row(&SegmentFamily::Content.dat_path(SCOPE, id), address));
            let meta = corpus.add(SIDECAR);
            custody.push(row(&SegmentFamily::Content.meta_path(SCOPE, id), meta));
        }
        let ledger = corpus.add(&ledger_bytes_at(&refs, counter));
        custody.push(row(
            &SegmentFamily::Content.mirror_path(SCOPE, "mail"),
            ledger,
        ));
        (corpus, custody)
    }

    /// The source went back from 40 to 30 and the destination still holds
    /// segments 30..40 live: the rows the nest lost.
    fn regressed_to_30_holding_40() -> (Corpus, Vec<CustodyItem>) {
        destination_holding(&ids(0..30), &ids(0..40), 30)
    }

    /// A retained generation of segment `id`'s `.dat`, retired at `superseded_at`.
    fn retained_dat(id: u32, superseded_at: i64) -> GenerationItem {
        GenerationItem {
            superseded_at,
            ..retained(&SegmentFamily::Content.dat_path(SCOPE, id), "ab".repeat(32))
        }
    }

    /// One audit pass through `audit_destination` from `prior`, against a
    /// destination reporting a 30-day window and retaining `retained`.
    fn pass_retaining(
        prior: &DestinationAuditState,
        (corpus, custody): (Corpus, Vec<CustodyItem>),
        retained: Vec<GenerationItem>,
        vouch: &dyn SourceLedgerVouch,
        now: i64,
    ) -> (AuditVerdict, DestinationAuditState) {
        block_on(audit_destination(
            &MockDestination {
                custody: Ok(custody),
                generations: Ok(retained),
            },
            &CorpusInclusion::new(corpus),
            DEST_URL,
            &inputs(Some(NOW), NOW - 30 * DAY),
            prior,
            &AttachedMirrorSets::new(),
            vouch,
            now,
        ))
    }

    /// [`pass_retaining`] against a destination retaining nothing.
    fn pass(
        prior: &DestinationAuditState,
        destination: (Corpus, Vec<CustodyItem>),
        vouch: &dyn SourceLedgerVouch,
        now: i64,
    ) -> (AuditVerdict, DestinationAuditState) {
        pass_retaining(prior, destination, Vec::new(), vouch, now)
    }

    /// The state a first verified pass at generation 40 leaves.
    fn state_verified_at_40() -> DestinationAuditState {
        let (verdict, after) = pass(
            &state("d1", None),
            genuine_destination(40, 40),
            &NoSourceVouch,
            NOW,
        );
        assert_eq!(verdict, AuditVerdict::Passed);
        assert!(after.accepted_regressions.is_empty());
        after
    }

    /// The state the pass at `NOW + DAY` leaves after accepting the regression
    /// to 30 with segments 30..40 still live at the destination, the floor
    /// landed.
    fn state_after_acceptance() -> DestinationAuditState {
        let (verdict, after) = pass(
            &state_verified_at_40(),
            regressed_to_30_holding_40(),
            &Flooring::new(31, true),
            NOW + DAY,
        );
        assert_eq!(verdict, AuditVerdict::Passed);
        after
    }

    /// The headline: a regression the source vouches for passes (the
    /// destination did nothing wrong), **floors the source's counter at the
    /// generation this device had pinned** — 40, never the 30 the destination
    /// served — and writes ONE record: the generations, the pass's clock, the
    /// floor landed, and no deadline, since what the copy holds is live rows.
    #[test]
    fn an_acceptance_floors_the_source_at_the_pin_and_records_it() {
        let before = state_verified_at_40();
        let source = Flooring::new(31, true);
        let (verdict, after) = pass(&before, regressed_to_30_holding_40(), &source, NOW + DAY);
        assert_eq!(verdict, AuditVerdict::Passed);
        assert_eq!(
            source.asked(),
            vec![("mail".to_string(), SCOPE.to_string(), 40)],
            "one floor, under the content family's serve tag, at the PINNED generation"
        );
        assert_eq!(after.accepted_regressions.len(), 1);
        assert_eq!(
            after.accepted_regressions.get(&mail_ledger_key()),
            Some(&AcceptedRegression {
                pinned: 40,
                served: 30,
                observed_at: NOW + DAY,
                floored_at: Some(NOW + DAY),
                recoverable_until: None,
            })
        );
    }

    /// A floor that did not land is owed: the record carries no `floored_at`,
    /// the next pass asks again — still with the generation pinned at
    /// acceptance, though the pin itself has re-based — and once it lands no
    /// later pass asks.
    #[test]
    fn a_floor_that_did_not_land_is_retried_by_the_next_pass() {
        let refusing = Flooring::new(31, false);
        let (_, accepted) = pass(
            &state_verified_at_40(),
            regressed_to_30_holding_40(),
            &refusing,
            NOW + DAY,
        );
        assert_eq!(refusing.asked().len(), 1, "asked once at acceptance");
        let record = accepted.accepted_regressions[&mail_ledger_key()];
        assert_eq!(record.floored_at, None);

        let landing = Flooring::new(31, true);
        let (_, retried) = pass(
            &accepted,
            regressed_to_30_holding_40(),
            &landing,
            NOW + 2 * DAY,
        );
        assert_eq!(
            landing.asked(),
            vec![("mail".to_string(), SCOPE.to_string(), 40)]
        );
        assert_eq!(
            retried.accepted_regressions.get(&mail_ledger_key()),
            Some(&AcceptedRegression {
                floored_at: Some(NOW + 2 * DAY),
                ..record
            }),
            "only the floor's stamp moved"
        );

        let settled = Flooring::new(40, true);
        let (_, later) = pass(
            &retried,
            regressed_to_30_holding_40(),
            &settled,
            NOW + 3 * DAY,
        );
        assert!(settled.asked().is_empty(), "a landed floor is not re-asked");
        assert_eq!(later.accepted_regressions, retried.accepted_regressions);
    }

    /// A rollback the source refutes (its counter still at the pin) is the
    /// destination's fault — a verdict, never a recovery notice, and nothing
    /// is floored.
    #[test]
    fn a_rollback_the_source_refutes_records_no_notice() {
        let source = Flooring::new(40, true);
        let (verdict, after) = pass(
            &state_verified_at_40(),
            regressed_to_30_holding_40(),
            &source,
            NOW + DAY,
        );
        assert!(matches!(verdict, AuditVerdict::InclusionFailure { .. }));
        assert!(after.accepted_regressions.is_empty());
        assert!(source.asked().is_empty());
    }

    /// A regression whose lost segments the destination no longer holds in
    /// any form leaves nothing to recover: accepted and floored, no notice.
    #[test]
    fn an_acceptance_with_nothing_left_to_recover_records_no_notice() {
        let source = Flooring::new(31, true);
        let (verdict, after) = pass(
            &state_verified_at_40(),
            genuine_destination(30, 30),
            &source,
            NOW + DAY,
        );
        assert_eq!(verdict, AuditVerdict::Passed);
        assert_eq!(source.asked().len(), 1);
        assert!(after.accepted_regressions.is_empty());
    }

    /// The standing rule's first arm: the record stands — with no deadline —
    /// for as long as the destination lists a live row below the pin that the
    /// source's ledger does not name, and is pruned once none remains. A
    /// tombstoned generation (what the recovery's own retirement, or a
    /// compaction, leaves) does not keep it standing, nor does an unnamed live
    /// row at or above the pin (a segment uploaded ahead of its ledger).
    #[test]
    fn the_record_stands_on_an_unnamed_live_row_below_the_pin_and_prunes_without_one() {
        let accepted = state_after_acceptance();
        let record = accepted.accepted_regressions.clone();

        // The floored source appends at 40 and 41; segments 30..40 are still
        // live at the destination, unnamed.
        let mut named = ids(0..30);
        named.extend([40, 41]);
        let (verdict, standing) = pass(
            &accepted,
            destination_holding(&named, &ids(0..42), 42),
            &NoSourceVouch,
            NOW + 5 * DAY,
        );
        assert_eq!(verdict, AuditVerdict::Passed);
        assert_eq!(standing.accepted_regressions, record);

        // Recovered: the lost rows are tombstoned (retained, no live row), and
        // segment 42 is live ahead of its ledger.
        let mut held = named.clone();
        held.push(42);
        let (verdict, pruned) = pass_retaining(
            &standing,
            destination_holding(&named, &held, 43),
            (30..40).map(|id| retained_dat(id, NOW + 6 * DAY)).collect(),
            &NoSourceVouch,
            NOW + 7 * DAY,
        );
        assert_eq!(verdict, AuditVerdict::Passed);
        assert!(pruned.accepted_regressions.is_empty());
    }

    /// The standing rule's second arm: an id the source reused before the
    /// acceptance left a SUPERSEDED generation at the destination, retained
    /// for the destination's own window. The record stands on it, its deadline
    /// the earliest such expiry still ahead, and is pruned once the last has
    /// been reclaimed.
    #[test]
    fn the_record_stands_on_a_retained_superseded_generation_until_its_expiry() {
        // The source came back at 30 and had already rewritten 30 and 31
        // before this device's pass: live rows 0..32, all named.
        let reused = || destination_holding(&ids(0..32), &ids(0..32), 32);
        let overwritten = vec![retained_dat(30, NOW - 2 * DAY), retained_dat(31, NOW - DAY)];

        let (verdict, accepted) = pass_retaining(
            &state_verified_at_40(),
            reused(),
            overwritten.clone(),
            &Flooring::new(32, true),
            NOW + DAY,
        );
        assert_eq!(verdict, AuditVerdict::Passed);
        let first_expiry = NOW - 2 * DAY + 30 * DAY;
        assert_eq!(
            accepted.accepted_regressions[&mail_ledger_key()].recoverable_until,
            Some(first_expiry)
        );

        // At the first expiry that generation is as good as reclaimed; the
        // deadline moves to the next.
        let (_, later) = pass_retaining(
            &accepted,
            reused(),
            overwritten,
            &NoSourceVouch,
            first_expiry,
        );
        assert_eq!(
            later.accepted_regressions[&mail_ledger_key()].recoverable_until,
            Some(NOW - DAY + 30 * DAY)
        );

        // Every lost generation reclaimed: nothing left to recover.
        let (_, pruned) = pass(&later, reused(), &NoSourceVouch, first_expiry + DAY);
        assert!(pruned.accepted_regressions.is_empty());
    }

    /// The deadline is the destination's OWN window, never the protocol floor
    /// the vanished-ledger rule judges retention by: a destination reporting a
    /// week keeps a superseded generation a week.
    #[test]
    fn the_recovery_deadline_is_the_destinations_own_grace() {
        let pins = state_verified_at_40().verified_ledger_generations;
        let (corpus, custody) = destination_holding(&ids(0..32), &ids(0..32), 32);
        let out = pinned_inclusion_at(
            corpus,
            &custody,
            &[retained_dat(30, NOW)],
            &pins,
            7 * DAY,
            NOW + DAY,
            &Flooring::new(32, true),
        );
        assert_eq!(out.verdict, AuditVerdict::Passed);
        assert_eq!(
            out.accepted_regressions
                .get(&mail_ledger_key())
                .map(|r| r.recoverable_until),
            Some(Some(NOW + 7 * DAY))
        );
    }

    /// A pass that never reaches the ledgers cannot say what the destination
    /// still holds: the record is left exactly as it was.
    #[test]
    fn a_pass_that_never_reaches_the_ledgers_leaves_the_record() {
        let accepted = state_after_acceptance();
        let (verdict, kept) = block_on(audit_destination(
            &dead(),
            &all_present(),
            DEST_URL,
            &inputs(Some(NOW), NOW - 30 * DAY),
            &accepted,
            &AttachedMirrorSets::new(),
            &NoSourceVouch,
            NOW + 2 * DAY,
        ));
        assert!(matches!(verdict, AuditVerdict::Unreachable { .. }));
        assert_eq!(kept.accepted_regressions, accepted.accepted_regressions);
    }

    /// A record whose set is gone — ledger, rows and generations, past the
    /// window, so the pin itself is dropped — goes with its pin.
    #[test]
    fn a_record_whose_set_is_gone_goes_with_its_pin() {
        let accepted = state_after_acceptance();
        let out = block_on(evaluate_inclusion_pinned(
            &CorpusInclusion::new(Corpus::default()),
            DEST_URL,
            &[],
            &[],
            &AttachedMirrorSets::new(),
            NOW + 40 * DAY,
            &accepted.verified_ledger_generations,
            &accepted.accepted_regressions,
            30 * DAY,
            &NoSourceVouch,
        ));
        assert!(out.ledger_generations.is_empty());
        assert!(out.accepted_regressions.is_empty());

        // Inside the window the vanished ledger is a miss, the pin is kept,
        // and so is the record.
        let out = block_on(evaluate_inclusion_pinned(
            &CorpusInclusion::new(Corpus::default()),
            DEST_URL,
            &[],
            &[],
            &AttachedMirrorSets::new(),
            NOW + 2 * DAY,
            &accepted.verified_ledger_generations,
            &accepted.accepted_regressions,
            30 * DAY,
            &NoSourceVouch,
        ));
        assert!(matches!(out.verdict, AuditVerdict::InclusionFailure { .. }));
        assert_eq!(out.accepted_regressions, accepted.accepted_regressions);
    }

    fn regression_until(recoverable_until: Option<i64>) -> AcceptedRegression {
        AcceptedRegression {
            pinned: 40,
            served: 30,
            observed_at: NOW,
            floored_at: Some(NOW),
            recoverable_until,
        }
    }

    /// The door: one fifth reason per destination, however many ledgers
    /// regressed — counting down to the EARLIEST deadline still ahead, beside
    /// the standing verdict's own reason — and nothing of its own once every
    /// deadline has passed.
    #[test]
    fn the_door_yields_one_source_regressed_reason_inside_the_window_only() {
        let mut record = DestinationAuditRecord {
            state: state("d1", Some(NOW)),
            verdict: Some(AuditVerdict::Passed),
        };
        record
            .state
            .accepted_regressions
            .insert("a".into(), regression_until(Some(NOW + 10 * DAY)));
        record
            .state
            .accepted_regressions
            .insert("b".into(), regression_until(Some(NOW + 4 * DAY)));
        record
            .state
            .accepted_regressions
            .insert("c".into(), regression_until(Some(NOW - DAY)));

        assert_eq!(
            record.alert_reasons(NOW),
            vec![BackupAuditAlertReason::SourceRegressed {
                left_secs: Some(4 * DAY)
            }]
        );
        assert_eq!(
            record.alert_reasons(NOW + 5 * DAY),
            vec![BackupAuditAlertReason::SourceRegressed {
                left_secs: Some(5 * DAY)
            }]
        );
        assert!(record.alert_reasons(NOW + 10 * DAY).is_empty());

        // A standing verdict keeps its own reason, first.
        record.verdict = Some(AuditVerdict::Overdue {
            since_secs: 8 * DAY,
        });
        assert_eq!(
            record.alert_reasons(NOW),
            vec![
                BackupAuditAlertReason::Overdue {
                    since_secs: 8 * DAY
                },
                BackupAuditAlertReason::SourceRegressed {
                    left_secs: Some(4 * DAY)
                },
            ]
        );
        assert_eq!(
            record.alert_reasons(NOW + 10 * DAY),
            vec![BackupAuditAlertReason::Overdue {
                since_secs: 8 * DAY
            }]
        );
    }

    /// The door carries no deadline when the records that stand hold live
    /// rows only: nothing reclaims those. A deadline still ahead on another
    /// ledger wins while it lasts.
    #[test]
    fn the_door_yields_no_deadline_when_only_live_rows_remain() {
        let mut record = DestinationAuditRecord {
            state: state("d1", Some(NOW)),
            verdict: Some(AuditVerdict::Passed),
        };
        record
            .state
            .accepted_regressions
            .insert("live-only".into(), regression_until(None));
        assert_eq!(
            record.alert_reasons(NOW + 400 * DAY),
            vec![BackupAuditAlertReason::SourceRegressed { left_secs: None }]
        );

        record
            .state
            .accepted_regressions
            .insert("dated".into(), regression_until(Some(NOW + 4 * DAY)));
        assert_eq!(
            record.alert_reasons(NOW),
            vec![BackupAuditAlertReason::SourceRegressed {
                left_secs: Some(4 * DAY)
            }]
        );
        assert_eq!(
            record.alert_reasons(NOW + 4 * DAY),
            vec![BackupAuditAlertReason::SourceRegressed { left_secs: None }]
        );
    }

    /// The recovery's post-recovery duty: the driving device forgets the
    /// records of the recovered set at that destination — both families' —
    /// and nothing else: another set's record, another destination's, and
    /// the pins all stay. A store that cannot be read is never written over.
    #[test]
    fn clearing_after_a_recovery_forgets_that_sets_records_only() {
        let record_with = |id: &str, keys: &[&str]| {
            let mut record = DestinationAuditRecord::never(id);
            for key in keys {
                record
                    .state
                    .accepted_regressions
                    .insert(key.to_string(), regression_until(None));
            }
            record.state.verified_ledger_generations.insert(
                mail_ledger_key(),
                LedgerPin {
                    generation: 30,
                    seen_live_at: Some(NOW),
                },
            );
            record
        };
        let journal_key = ledger_key(
            "__mail",
            &SegmentFamily::Placement.mirror_path(SCOPE, "mail"),
        );
        let post_key = ledger_key("__post", &SegmentFamily::Content.mirror_path(SCOPE, "post"));
        let keys = [mail_ledger_key(), journal_key, post_key.clone()];
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        let store = MemStore::with(AuditStateSnapshot {
            records: vec![record_with("d1", &keys), record_with("d2", &keys)],
            observed_high_water: Some(NOW),
        });

        assert_eq!(clear_accepted_regressions(&store, "d1", "__mail"), Ok(true));
        let saved = store.saved();
        assert_eq!(
            saved.records[0]
                .state
                .accepted_regressions
                .keys()
                .collect::<Vec<_>>(),
            vec![&post_key]
        );
        assert_eq!(saved.records[0].state.verified_ledger_generations.len(), 1);
        assert_eq!(saved.records[1].state.accepted_regressions.len(), 3);
        assert_eq!(saved.observed_high_water, Some(NOW));

        // Nothing left to clear: no write.
        let saves = store.saves();
        assert_eq!(
            clear_accepted_regressions(&store, "d1", "__mail"),
            Ok(false)
        );
        assert_eq!(store.saves(), saves);

        let broken = MemStore::broken();
        assert!(clear_accepted_regressions(&broken, "d1", "__mail").is_err());
        assert_eq!(broken.saves(), 0);
    }

    // ── the vanished-ledger rule ────────────────

    /// The pins a first verified pass at generation `counter` leaves, all seen
    /// live at `NOW`.
    fn pins_after_verifying(counter: u32) -> BTreeMap<String, LedgerPin> {
        let (corpus, custody) = genuine_destination(counter, counter);
        let out = pinned_inclusion(corpus, &custody, &BTreeMap::new(), &NoSourceVouch);
        assert_eq!(out.verdict, AuditVerdict::Passed);
        assert_eq!(
            out.ledger_generations.get(&mail_ledger_key()),
            Some(&LedgerPin {
                generation: counter,
                seen_live_at: Some(NOW)
            })
        );
        out.ledger_generations
    }

    /// `custody` without its content ledger row.
    fn without_ledger(custody: Vec<CustodyItem>) -> Vec<CustodyItem> {
        let mirror = SegmentFamily::Content.mirror_path(SCOPE, "mail");
        custody
            .into_iter()
            .filter(|row| row.path.as_deref() != Some(mirror.as_str()))
            .collect()
    }

    /// The headline: a destination this device verified at generation 40
    /// de-lists the ledger row itself — nothing of it live or retained — and
    /// keeps the segment rows, rolled back to the first 32 with every stamp
    /// fresh. Sampled from the list alone the set would pass at 81 %; the pin
    /// remembers the ledger existed, and a family with live segments and no
    /// ledger anywhere is missing its anchor with certainty. The pin stays.
    #[test]
    fn a_destination_that_de_lists_a_pinned_ledger_row_fails_inclusion() {
        let pins = pins_after_verifying(40);
        let (corpus, custody) = genuine_destination(32, 32);
        let custody = without_ledger(custody);
        let out = pinned_inclusion_at(
            corpus,
            &custody,
            &[],
            &pins,
            30 * DAY,
            NOW + DAY,
            &NoSourceVouch,
        );
        assert!(
            matches!(
                out.verdict,
                AuditVerdict::InclusionFailure { missing: 1, .. }
            ),
            "the missing anchor is one certain miss — got {:?}",
            out.verdict
        );
        assert_eq!(
            out.ledger_generations, pins,
            "the pin holds until the rows go"
        );

        // The same set with no pin — a device that never saw the ledger — is
        // the unanchored 81 % case the rule cannot help: the destination's
        // list is all there is, and every listed byte is intact.
        let (corpus, custody) = genuine_destination(32, 32);
        let custody = without_ledger(custody);
        let out = pinned_inclusion(corpus, &custody, &BTreeMap::new(), &NoSourceVouch);
        assert_eq!(out.verdict, AuditVerdict::Passed);
    }

    /// A ledger the destination no longer lists live but still retains —
    /// what a set the source tore down looks like for the whole grace
    /// window, and what a destination that hid the live row but kept its
    /// history looks like — anchors the set from its newest retained
    /// generation exactly as a live one would: every path it names must be
    /// custody, and its generation meets the pin.
    #[test]
    fn a_retained_ledger_anchors_its_set_after_the_live_row_is_gone() {
        let pins = pins_after_verifying(40);
        let mirror = SegmentFamily::Content.mirror_path(SCOPE, "mail");

        // Torn down honestly: every row tombstoned, all retained.
        let (corpus, custody) = genuine_destination(40, 40);
        let history: Vec<GenerationItem> = custody
            .iter()
            .map(|row| retained(row.path.as_deref().unwrap(), row.manifest_hash.clone()))
            .collect();
        let out = pinned_inclusion_at(
            corpus,
            &[],
            &history,
            &pins,
            30 * DAY,
            NOW + DAY,
            &NoSourceVouch,
        );
        assert_eq!(out.verdict, AuditVerdict::Passed);
        assert_eq!(
            out.ledger_generations.get(&mail_ledger_key()),
            Some(&LedgerPin {
                generation: 40,
                seen_live_at: Some(NOW)
            }),
            "a retained sighting keeps the pin and does not refresh the live clock"
        );

        // The live ledger row hidden, its retained generation intact, and the
        // segments above 32 de-listed: the retained ledger names them, so
        // each is a certain miss.
        let (corpus, custody) = genuine_destination(40, 40);
        let ledger_row = custody
            .iter()
            .find(|row| row.path.as_deref() == Some(mirror.as_str()))
            .cloned()
            .unwrap();
        let history = vec![retained(&mirror, ledger_row.manifest_hash.clone())];
        let custody: Vec<CustodyItem> = without_ledger(custody)
            .into_iter()
            .filter(|row| row.path.as_deref() < Some(&SegmentFamily::Content.dat_path(SCOPE, 32)))
            .collect();
        let out = pinned_inclusion_at(
            corpus,
            &custody,
            &history,
            &pins,
            30 * DAY,
            NOW + DAY,
            &NoSourceVouch,
        );
        assert!(
            matches!(
                out.verdict,
                AuditVerdict::InclusionFailure { missing: 16, .. }
            ),
            "both halves of segments 32..40 are named by the retained ledger — got {:?}",
            out.verdict
        );

        // Only an older retained generation left — the newest ones de-listed
        // with the live row — is the rollback the pin exists to catch.
        let (corpus, custody) = genuine_destination(30, 30);
        let ledger_row = custody
            .iter()
            .find(|row| row.path.as_deref() == Some(mirror.as_str()))
            .cloned()
            .unwrap();
        let history = vec![retained(&mirror, ledger_row.manifest_hash.clone())];
        let custody = without_ledger(custody);
        let out = pinned_inclusion_at(
            corpus,
            &custody,
            &history,
            &pins,
            30 * DAY,
            NOW + DAY,
            &Vouch(Ok(40)),
        );
        assert!(matches!(out.verdict, AuditVerdict::InclusionFailure { .. }));
        assert_eq!(
            generation_of(&out.ledger_generations, &mail_ledger_key()),
            Some(40)
        );
    }

    /// A pinned ledger gone from both lists while this device saw it live
    /// less than the window ago: the destination promised to retain a
    /// tombstoned generation that long, so the absence is an early reclaim
    /// or a de-listing — a certain miss every pass, the pin kept. Once the
    /// window has passed since the sighting, the set is accepted as torn
    /// down while this device was away, and the pin is dropped.
    #[test]
    fn a_pinned_ledger_gone_from_both_lists_alarms_inside_the_window_and_is_dropped_after() {
        let pins = pins_after_verifying(40);
        for (now, expect_alarm) in [
            (NOW + DAY, true),
            (NOW + 30 * DAY - 1, true),
            (NOW + 30 * DAY, false),
            (NOW + 400 * DAY, false),
        ] {
            let out = pinned_inclusion_at(
                Corpus::default(),
                &[],
                &[],
                &pins,
                30 * DAY,
                now,
                &NoSourceVouch,
            );
            if expect_alarm {
                assert!(
                    matches!(
                        out.verdict,
                        AuditVerdict::InclusionFailure { missing: 1, .. }
                    ),
                    "at +{}d the absence is a certain miss — got {:?}",
                    (now - NOW) / DAY,
                    out.verdict
                );
                assert_eq!(out.ledger_generations, pins);
            } else {
                assert_eq!(
                    out.verdict,
                    AuditVerdict::Passed,
                    "at +{}d",
                    (now - NOW) / DAY
                );
                assert!(out.ledger_generations.is_empty(), "the pin is dropped");
            }
        }
    }

    /// The window is the destination's reported one floored at the
    /// protocol's: a destination reporting an hour cannot shorten its own
    /// audit to an hour, and one honestly reporting sixty days is held to
    /// sixty.
    #[test]
    fn the_vanished_ledger_window_is_floored_at_the_protocol_grace() {
        let pins = pins_after_verifying(40);
        let absent = |grace_secs: i64, now: i64| {
            pinned_inclusion_at(
                Corpus::default(),
                &[],
                &[],
                &pins,
                grace_secs,
                now,
                &NoSourceVouch,
            )
            .verdict
        };
        assert!(matches!(
            absent(HOUR, NOW + DAY),
            AuditVerdict::InclusionFailure { .. }
        ));
        assert_eq!(absent(HOUR, NOW + 30 * DAY), AuditVerdict::Passed);
        assert!(matches!(
            absent(60 * DAY, NOW + 45 * DAY),
            AuditVerdict::InclusionFailure { .. }
        ));
        assert_eq!(absent(60 * DAY, NOW + 60 * DAY), AuditVerdict::Passed);
    }

    /// A pinned ledger the destination has listed all along is untouched by
    /// the rule, and the rule never invents a pin: a set this device has
    /// never seen ledgered is judged by the population anchor alone.
    #[test]
    fn the_vanished_ledger_rule_only_judges_pinned_keys_the_list_lacks() {
        let pins = pins_after_verifying(40);
        let (corpus, custody) = genuine_destination(41, 41);
        let out = pinned_inclusion_at(
            corpus,
            &custody,
            &[],
            &pins,
            30 * DAY,
            NOW + 40 * DAY,
            &NoSourceVouch,
        );
        assert_eq!(out.verdict, AuditVerdict::Passed);
        assert_eq!(
            out.ledger_generations.get(&mail_ledger_key()),
            Some(&LedgerPin {
                generation: 41,
                seen_live_at: Some(NOW + 40 * DAY)
            }),
            "a live sighting advances the pin and refreshes its clock"
        );
    }

    /// End to end through `audit_destination`: the window the destination
    /// reports beside its retained generations is the one the rule runs on,
    /// the dropped pin leaves the persisted state, and the alarm inside the
    /// window is the ordinary inclusion failure.
    #[test]
    fn the_vanished_ledger_rule_rides_the_persisted_state_and_the_reported_window() {
        let (corpus, custody) = genuine_destination(40, 40);
        let (verdict, after) = block_on(audit_destination(
            &reachable(custody),
            &CorpusInclusion::new(corpus),
            DEST_URL,
            &inputs(Some(NOW), NOW - 30 * DAY),
            &state("d1", None),
            &AttachedMirrorSets::new(),
            &NoSourceVouch,
            NOW,
        ));
        assert_eq!(verdict, AuditVerdict::Passed);
        assert_eq!(after.verified_ledger_generations.len(), 1);

        // The set gone from both lists a day later: alarm, pin kept. (This
        // device has observed nothing locally, so an empty destination does
        // not fail freshness first and the inclusion arm is reached.)
        let (verdict, kept) = block_on(audit_destination(
            &reachable(Vec::new()),
            &all_present(),
            DEST_URL,
            &inputs(None, NOW - 30 * DAY),
            &after,
            &AttachedMirrorSets::new(),
            &NoSourceVouch,
            NOW + DAY,
        ));
        assert!(matches!(
            verdict,
            AuditVerdict::InclusionFailure { missing: 1, .. }
        ));
        assert_eq!(
            kept.verified_ledger_generations,
            after.verified_ledger_generations
        );
        assert_eq!(kept.last_passed_at, Some(NOW));

        // Past the destination's own 30-day window: accepted, pin gone.
        let (verdict, dropped) = block_on(audit_destination(
            &reachable(Vec::new()),
            &all_present(),
            DEST_URL,
            &inputs(None, NOW - 30 * DAY),
            &kept,
            &AttachedMirrorSets::new(),
            &NoSourceVouch,
            NOW + 31 * DAY,
        ));
        assert_eq!(verdict, AuditVerdict::Passed);
        assert!(dropped.verified_ledger_generations.is_empty());
        assert_eq!(dropped.last_passed_at, Some(NOW + 31 * DAY));
    }
}
