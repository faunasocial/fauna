//! The Nests-page trust-facet **Now/History lens** projections — the pure,
//! wasm-clean half of slice 5's shared view-model (design `docs/goal/ui/nests.md`
//! § Trust facet — grants (Now lens) / History lens + § Expiry / renewal —
//! first-class states).
//!
//! A nest row's trust facet is the set of the owner's *current* grants
//! ([`crate::grant_log::current_grants`], the Now projection of the signed
//! grant-event log, `fauna.state.succession-ledger`) **filtered to the holders that belong to that
//! nest** — a nest's holders being its enrolled content-processor service-users'
//! X25519 pubkeys (the grant `holder`; design § 2.4 `holder_pubkey =
//! bridge_service_users.x25519_pubkey`). The machine discovers a nest's holder
//! set over the bridges seam (`fauna.bridges.list_service_users` +
//! `fetch_bridge_pubkey`); this module is the pure fold + `now`-relative
//! liveness that turns "the log + a holder set + the clock" into what a row
//! renders, with **no clock, transport, or nest read of its own** (wasm-clean,
//! like the rest of this crate). It owns both the Now lens
//! ([`trust_facet_for_holders`]) and the per-nest History lens
//! ([`history_for_holders`], the raw event timeline).
//!
//! Every renewal-policy threshold here is a hard-coded Rust constant, never a
//! config surface (`nests.md` § Where logic lives: "Renewal-policy constants …
//! are hard-coded Rust constants — the only user choices are per-grant duration
//! + blessed-box status"; product invariant — no operator).

use std::collections::{BTreeMap, BTreeSet};

use fauna_core::custody_ceremony::CustodyConfig;
use fauna_core::data::MailConfig;
use fauna_core::grant_event::{GrantEvent, GrantEventKind, GrantEventScope};
use fauna_core::succession_ledger::SuccessionLedger;
use fauna_mls::wrapped_blob::ScopeTuple;

use crate::derive_scope_payload;
use crate::grant_log::{CurrentGrant, current_grants, history, renewal_window_secs};

/// A grant reads **"expiring soon"** once it is within this of its `lasts_until`
/// — the renew-ahead threshold the minting client would renew a blessed grant
/// ahead of, and the window the UI warns an un-blessed grant will lapse in.
/// ~14 days (a hard-coded constant, no config surface — product invariant).
pub const RENEW_AHEAD_SECS: u64 = 14 * 24 * 60 * 60;

/// How often a running app re-checks for blessed grants due for renewal — the
/// periodic half of the auto-renew loop (the other half is app foreground;
/// `nests.md` § Expiry / renewal → *Duration and blessing*). 1 hour: far inside
/// [`RENEW_AHEAD_SECS`], so an app left running never lets a standing blessed
/// grant reach its end. Hard-coded, no config surface.
pub const AUTO_RENEW_CHECK_SECS: u64 = 60 * 60;

/// Whether a grant minted with this window length is **standing** — long
/// enough that renewing it ahead of its end is meaningful. A window no longer
/// than [`RENEW_AHEAD_SECS`] would be "due" from birth, so a one-off grant is
/// never auto-renewed and never reads `AutoRenewing`.
pub const fn is_standing(minted_window_secs: u64) -> bool {
    minted_window_secs > RENEW_AHEAD_SECS
}

/// Whether the auto-renew loop renews this current grant: its holder is a
/// blessed nest's and its chosen duration is [`is_standing`]. The one
/// definition both the liveness pill and the loop read, so the page never
/// says `AutoRenewing` about a grant the loop will leave to lapse.
///
/// A **bounded** mail grant ([`crate::grant_log::is_bounded_mail_grant`]) is due like any
/// other: its renewal carries the epoch wraps for the window it extends into
/// ([`crate::bounded_mail_renewal_keys`], computed from the recorded old
/// end), which the loop's body seals to the holder the live roster names
/// before it records the `Renew` — a bounded grant whose wraps cannot be
/// sealed is skipped there, its recorded end unmoved.
fn auto_renews(
    ledger: &SuccessionLedger,
    grant: &CurrentGrant,
    blessed_holders: &BTreeSet<Vec<u8>>,
) -> bool {
    blessed_holders.contains(&grant.holder)
        && is_standing(renewal_window_secs(ledger, &grant.grant_id))
}

/// One grant the auto-renew loop should renew now, and by how much.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueRenewal {
    pub grant_id: Vec<u8>,
    /// The grant's own mint-time length — the renewed window is
    /// `real now + extend_by_secs` ([`crate::grant_log::renewal_window_secs`]).
    pub extend_by_secs: u64,
}

/// The auto-renew loop's fold: every current grant that auto-renews (a blessed
/// holder, a standing duration), is inside the renew-ahead threshold, and has
/// not yet expired — an expired grant is the user's to renew (its liveness
/// reads `Expired` even when blessed). `now` is the trust facet's render clock
/// (`trust_clock`), so a test that moves that clock sees the loop fire.
/// Ordered soonest-expiry first, like the Now lens.
pub fn grants_due_for_renewal(
    ledger: &SuccessionLedger,
    blessed_holders: &BTreeSet<Vec<u8>>,
    now: u64,
) -> Vec<DueRenewal> {
    let mut due: Vec<(u64, DueRenewal)> = current_grants(ledger)
        .into_iter()
        .filter(|g| auto_renews(ledger, g, blessed_holders))
        .filter(|g| now < g.window_end && g.window_end - now <= RENEW_AHEAD_SECS)
        .map(|g| {
            let extend_by_secs = renewal_window_secs(ledger, &g.grant_id);
            (
                g.window_end,
                DueRenewal {
                    grant_id: g.grant_id,
                    extend_by_secs,
                },
            )
        })
        .collect();
    due.sort_by(|a, b| (a.0, &a.1.grant_id).cmp(&(b.0, &b.1.grant_id)));
    due.into_iter().map(|(_, d)| d).collect()
}

/// A current grant's liveness relative to `now` — the first-class expiry states
/// `nest-trust-grant-status` renders (`nests.md` § Expiry / renewal), so a
/// lapsed grant reads as "processing paused — renew here", never as silent
/// feature loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantLiveness {
    /// Inside its window, comfortably ahead of the renew-ahead threshold.
    Active,
    /// Within [`RENEW_AHEAD_SECS`] of `lasts_until` — warn the user (an
    /// un-blessed grant will lapse unless renewed).
    ExpiringSoon,
    /// `now >= lasts_until` — the window elapsed and the holder has gone dark;
    /// `nest-trust-grant-renew` is the recovery. Still shown (never silently
    /// dropped): a revoked grant leaves the Now lens (`current_grants` omits
    /// it); an *expired* one stays, so the user can renew it.
    Expired,
    /// The blessed-box indicator: the minting client renews this grant in the
    /// background, so its impending expiry is not the user's problem. Supersedes
    /// `Active`/`ExpiringSoon` for a blessed, not-yet-expired holder; an expired
    /// grant reads `Expired` even when blessed (background renewal has stopped
    /// firing — the user acts).
    AutoRenewing,
}

/// One current grant, projected for the trust facet's Now lens — the
/// per-`nest-trust-grant-item` view model. `scope` is the declared content the
/// grant may read; the per-app shell maps each `GrantEventScope` to its
/// localized "trusted to read: Mail, Calendar" label (never rendered here — this
/// stays pure/i18n-free).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantView {
    /// The 16-byte grant id — the handle a `Renew`/`Revoke` action names.
    pub grant_id: Vec<u8>,
    /// The holder X25519 pubkey (the content-processor service-user this grant
    /// is sealed to). Which nest row this grant renders under is decided by the
    /// caller's holder-set membership, not carried here.
    pub holder: Vec<u8>,
    /// The declared scope tuples (which content kinds the holder may read).
    pub scope: Vec<GrantEventScope>,
    /// `window_end` — the grant's `lasts-until` (epoch seconds).
    pub lasts_until: u64,
    /// The `now`-relative liveness state the status pill renders.
    pub liveness: GrantLiveness,
}

/// One nest row's Now-lens trust facet: the current grants whose holder belongs
/// to that nest, most-urgent-first. An empty `grants` is the explicit "not
/// trusted to read anything" state (`nest-trust-empty`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustFacet {
    pub grants: Vec<GrantView>,
}

/// Classify a grant's liveness from its window end, the current time, and
/// whether its holder is a *blessed* box the minting client auto-renews.
///
/// Precedence — expiry first (an elapsed window is always `Expired`, even for a
/// blessed box whose background renewal has stopped), then blessing (a live
/// blessed grant is `AutoRenewing` regardless of how near expiry), then the
/// renew-ahead warning, then plain `Active`.
pub fn compute_liveness(lasts_until: u64, now: u64, blessed: bool) -> GrantLiveness {
    if now >= lasts_until {
        GrantLiveness::Expired
    } else if blessed {
        GrantLiveness::AutoRenewing
    } else if lasts_until.saturating_sub(now) <= RENEW_AHEAD_SECS {
        GrantLiveness::ExpiringSoon
    } else {
        GrantLiveness::Active
    }
}

/// Fold the owner's grant log into one nest row's Now-lens trust facet: the
/// [`current_grants`] whose `holder` is in `holders` (that nest's
/// content-processor service-user pubkeys), each projected to a [`GrantView`]
/// with a `now`-relative [`GrantLiveness`]. `blessed_holders` is the set of
/// blessed nests' holders; a live grant reads `AutoRenewing` only when the
/// auto-renew loop will actually renew it — a blessed holder AND a standing
/// duration ([`is_standing`]), so a one-off grant on a blessed nest still reads
/// its real liveness.
///
/// Deterministic order: soonest `lasts_until` first (the most urgent to renew),
/// tie-broken on `grant_id`, so the render order is stable across the unordered
/// post-merge log.
pub fn trust_facet_for_holders(
    ledger: &SuccessionLedger,
    holders: &BTreeSet<Vec<u8>>,
    now: u64,
    blessed_holders: &BTreeSet<Vec<u8>>,
) -> TrustFacet {
    let mut grants: Vec<GrantView> = current_grants(ledger)
        .into_iter()
        .filter(|g| holders.contains(&g.holder))
        .map(|g| GrantView {
            liveness: compute_liveness(g.window_end, now, auto_renews(ledger, &g, blessed_holders)),
            grant_id: g.grant_id,
            holder: g.holder,
            scope: g.scope,
            lasts_until: g.window_end,
        })
        .collect();
    grants.sort_by(|a, b| (a.lasts_until, &a.grant_id).cmp(&(b.lasts_until, &b.grant_id)));
    TrustFacet { grants }
}

/// A content-processor holder as the mint-option builder sees it — the
/// `(bridge_id, role)` metadata of one enrolled service-user (the pure shape of
/// pair's `AvailableHolder`; key material never reaches this module).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintHolder {
    /// The stable holder name a `Mint` targets (e.g. `"mda-1"`, `"web-serve"`).
    pub bridge_id: String,
    /// `"mda"` or the generic `"content-processor"` — the axis the builder
    /// derives each option's holder candidates from.
    pub role: String,
}

/// Which use case a mint-picker option represents — the i18n selector the
/// per-app shell maps to its localized option label ("Read and filter my
/// mail" / "Read my calendar" / "Serve paywalled posts — ‹tier›"); labels are
/// never rendered here (pure/i18n-free, like [`GrantView`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MintUseCase {
    /// Mail delivery + filtering: `content.read{mail}` **bundled with** the
    /// keyless `content.label-write` (the MDA writes spam scores when it
    /// filters — the production grant shape, never `read{mail}` alone).
    Mail,
    /// Calendar serving: `content.read{calendar}`.
    Calendar,
    /// Serve one tier's paywalled posts on the web: `content.read{post, tier}`
    /// (the tier rides [`MintOptionModel::tier`]).
    PaywalledPosts,
}

/// One option in the trust facet's mint picker (`nest-trust-mint-scope-select`)
/// — a *use case* the user can trust this nest with, pre-resolved to the scope
/// tuples it mints and the holder(s) that can take it (`nests.md` § Mint,
/// scope-first design ratified 2026-07-13).
///
/// `holder_candidates` is the derived-holder axis: exactly one candidate ⇒ the
/// UI mints to it directly (no holder pick); more than one ⇒ the ambiguity case
/// the conditional `nest-trust-mint-holder-select` renders. Zero-candidate or
/// underivable options are never emitted — the picker only offers choices that
/// can actually confirm (mail options need the MSEK, i.e. mail enabled; a
/// post-tier option needs that tier's held period key + a generic
/// content-processor holder enrolled).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintOptionModel {
    pub use_case: MintUseCase,
    /// The tier a [`MintUseCase::PaywalledPosts`] option serves; `None` otherwise.
    pub tier: Option<String>,
    /// The scope tuples a confirm mints — verbatim the `Mint` action's `scope`.
    pub scope: Vec<GrantEventScope>,
    /// `bridge_id`s of the holders this option's grant can target, in roster
    /// order. Always non-empty.
    pub holder_candidates: Vec<String>,
}

/// Build the trust facet's mint-picker options from what the owner's mail
/// custody (`fauna.state.mail`) and period-key custody (`period_keys`, the
/// `fauna.state.subscriptions` fold)
/// can actually derive and which content-processor holders the nest has
/// enrolled — the one shared catalog every app's picker renders (priority #2;
/// `nests.md` § Mint). Deterministic order: Mail, Calendar, then one option
/// per held tier in custody order.
///
/// Holder derivation is by role: mail/calendar target the `"mda"` holder(s);
/// a post-tier grant targets the generic `"content-processor"` holder(s) (the
/// web-serve paywall holder today — `monetization.md` § Pillar 2). The
/// `content.read{folder}` scope is deliberately absent: paywalled folders
/// mint through their own dedicated flow, not this generic picker.
pub fn mint_options(
    period_keys: &fauna_core::data::SubscriptionsConfig,
    mail: &MailConfig,
    holders: &[MintHolder],
) -> Vec<MintOptionModel> {
    let candidates = |role: &str| -> Vec<String> {
        holders
            .iter()
            .filter(|h| h.role == role)
            .map(|h| h.bridge_id.clone())
            .collect()
    };
    let read_scope = |kind: &str, tier: Option<&str>| GrantEventScope {
        class: ScopeTuple::CLASS_CONTENT_READ.into(),
        kind: Some(kind.into()),
        tier: tier.map(str::to_string),
    };
    // Offer only what would actually mint: probe the real payload derivation
    // (the single scope catalog — no parallel availability logic to drift).
    let derivable = |scope: &GrantEventScope| {
        derive_scope_payload(
            Some(period_keys),
            mail,
            &ScopeTuple {
                class: scope.class.clone(),
                kind: scope.kind.clone(),
                tier: scope.tier.clone(),
                set: None,
                factor: None,
            },
        )
        .is_ok()
    };

    let mut options = Vec::new();

    let mda = candidates(fauna_client_bridges::MDA_HOLDER_ROLE);
    if !mda.is_empty() {
        let mail = read_scope(ScopeTuple::KIND_MAIL, None);
        if derivable(&mail) {
            options.push(MintOptionModel {
                use_case: MintUseCase::Mail,
                tier: None,
                scope: vec![
                    mail,
                    GrantEventScope {
                        class: ScopeTuple::CLASS_CONTENT_LABEL_WRITE.into(),
                        kind: None,
                        tier: None,
                    },
                ],
                holder_candidates: mda.clone(),
            });
        }
        let calendar = read_scope(ScopeTuple::KIND_CALENDAR, None);
        if derivable(&calendar) {
            options.push(MintOptionModel {
                use_case: MintUseCase::Calendar,
                tier: None,
                scope: vec![calendar],
                holder_candidates: mda,
            });
        }
    }

    let generic = candidates(fauna_client_bridges::CONTENT_PROCESSOR_ROLE);
    if !generic.is_empty() {
        for tier in &period_keys.tiers {
            let scope = read_scope(ScopeTuple::KIND_POST, Some(&tier.tier_name));
            if derivable(&scope) {
                options.push(MintOptionModel {
                    use_case: MintUseCase::PaywalledPosts,
                    tier: Some(tier.tier_name.clone()),
                    scope: vec![scope],
                    holder_candidates: generic.clone(),
                });
            }
        }
    }

    options
}

/// One grant-event row for the **History lens** (`nest-trust-history-item`) —
/// "Minted / Renewed / Revoked ‹scope› · ‹when›". An owned projection of a
/// signed [`fauna_core::grant_event::GrantEvent`] (the borrowed log entry does
/// not cross the FFI/WASM boundary), self-describing: it carries the grant's
/// scope + window as of that event (empty scope + zeroed window on a `Revoke`),
/// so the row renders with no need to correlate back to the `Mint`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub grant_id: Vec<u8>,
    pub holder: Vec<u8>,
    /// Mint / Renew / Revoke — the transition this row describes.
    pub kind: GrantEventKind,
    /// The declared scope as of this event (empty for `Revoke`).
    pub scope: Vec<GrantEventScope>,
    pub window_start: u64,
    pub window_end: u64,
    /// When the transition was recorded (epoch seconds).
    pub at: u64,
}

/// Fold the owner's grant log into one nest row's **History-lens** timeline: every
/// grant event whose `holder` is in `holders` (that nest's content-processor
/// service-user pubkeys), most-recent-first — the same holder-filtering as
/// [`trust_facet_for_holders`], but over the raw event log rather than the folded
/// current state (Now = projection, History = the log; `nests.md` § Trust facet
/// — History lens). Unlike the Now lens, this keeps `Revoke` events (they are the
/// forensic record of a withdrawn trust).
pub fn history_for_holders(
    ledger: &SuccessionLedger,
    holders: &BTreeSet<Vec<u8>>,
) -> Vec<HistoryEntry> {
    let events = history(ledger);
    // A signed `Revoke` carries no scope (the `GrantEvent` shape is frozen for
    // this major), yet every History row is self-describing (`nests.md`
    // § Trust facet — History lens). So a scope-less entry names the scope its
    // grant last carried: the newest scoped event for that grant_id (`events`
    // is most-recent-first, so the first hit wins). Display-only — the signed
    // log is untouched.
    let scope_of = |e: &GrantEvent| -> Vec<GrantEventScope> {
        if !e.scope.is_empty() {
            return e.scope.clone();
        }
        events
            .iter()
            .find(|p| p.grant_id == e.grant_id && !p.scope.is_empty())
            .map(|p| p.scope.clone())
            .unwrap_or_default()
    };
    events
        .iter()
        .filter(|e| holders.contains(&e.holder))
        .map(|e| HistoryEntry {
            grant_id: e.grant_id.clone(),
            holder: e.holder.clone(),
            kind: e.kind,
            scope: scope_of(e),
            window_start: e.window_start,
            window_end: e.window_end,
            at: e.at,
        })
        .collect()
}

// ── The third-party principal as a participant ───────────────────────────────
//
// `participants.md` § The participant model → *Third-party principal*: an
// approved external app is a participant whose trust facet renders its grants
// and whose history lens renders its log. It is the same fold as a nest row's,
// over a one-key holder set — the principal's attested `holder_x25519` — and
// over **third-party grants only**: those whose scope a consent mints
// (`third-party.md` § The principal model rule 4) — an `ext.*` kind, the
// keyless `deposit` tuple, or the `content.read{folder}` read twin — the only
// grants an app mints to a principal (`ext_consent`). The holder key comes from
// the nest's roster read, so the scope filter is the log's guard against a
// roster naming some other holder's key pulling that holder's grants into a
// principal's row — or into the `Revoke` events
// [`grants_ended_by_principal_revoke`] feeds. Admitting the folder read twin is
// safe although the web-serve paywall grant shares its tuple: that grant's
// holder can never be a roster principal's key, because the nest's
// `refuse_foreign_holder` (`bins/fauna-nest/src/db/third_party_principals.rs`)
// refuses a consent attesting an enrolled bridge's key, the web-serve holder
// among them.

/// Whether a grant's declared scope is a third-party principal's: any tuple
/// over an `ext.*` kind, a `deposit` tuple, or a `content.read{folder}` tuple
/// (`third-party.md` § The principal model rule 4).
fn is_principal_scope(scope: &[GrantEventScope]) -> bool {
    scope.iter().any(|t| {
        t.class == ScopeTuple::CLASS_DEPOSIT
            || (t.class == ScopeTuple::CLASS_CONTENT_READ
                && t.base_kind() == Some(ScopeTuple::KIND_FOLDER))
            || t.base_kind().is_some_and(fauna_core::ext_kind::is_ext_kind)
    })
}

/// A third-party principal's Now-lens trust facet: the current third-party
/// grants to `holder`, soonest-expiring first. A principal is never a blessed
/// box — nothing renews its grants in the background — so its grants read
/// their real liveness.
pub fn principal_trust_facet(ledger: &SuccessionLedger, holder: &[u8], now: u64) -> TrustFacet {
    let mut facet = trust_facet_for_holders(
        ledger,
        &BTreeSet::from([holder.to_vec()]),
        now,
        &BTreeSet::new(),
    );
    facet.grants.retain(|g| is_principal_scope(&g.scope));
    facet
}

/// A third-party principal's History-lens timeline: every event of its
/// third-party grants to `holder`, most-recent-first, `Revoke`s included.
pub fn principal_history(ledger: &SuccessionLedger, holder: &[u8]) -> Vec<HistoryEntry> {
    let mut entries = history_for_holders(ledger, &BTreeSet::from([holder.to_vec()]));
    entries.retain(|e| is_principal_scope(&e.scope));
    entries
}

/// The folder a principal's folder read grant covers, as its facet row and
/// history line name it (`webdav-server.md` § Key model → *A principal's
/// read* rule (1)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrincipalFolder {
    /// The owner's set of that name.
    Named(String),
    /// A set the owner no longer has: no listed set derives the grant's id at
    /// any generation.
    Deleted,
}

/// Which folder the grant `grant_id` of scope `scope` covers, given the
/// owner's id → set-name map ([`crate::folder_principal_set_names`], built
/// above the fold where the owner secret is held — this stays pure). `None`
/// for a grant that reads no folder. The id is matched first, so an entry
/// whose scope the log never carried still names its folder.
pub fn principal_grant_folder(
    grant_id: &[u8],
    scope: &[GrantEventScope],
    set_names: &BTreeMap<[u8; 16], String>,
) -> Option<PrincipalFolder> {
    if let Some(name) = <[u8; 16]>::try_from(grant_id)
        .ok()
        .and_then(|id| set_names.get(&id))
    {
        return Some(PrincipalFolder::Named(name.clone()));
    }
    scope
        .iter()
        .any(|t| {
            t.class == ScopeTuple::CLASS_CONTENT_READ
                && t.base_kind() == Some(ScopeTuple::KIND_FOLDER)
        })
        .then_some(PrincipalFolder::Deleted)
}

impl TrustFacet {
    /// The facet's *lasts-until*: the latest window end among its grants —
    /// when the last of them lapses (epoch seconds). `None` for an empty facet.
    pub fn lasts_until(&self) -> Option<u64> {
        self.grants.iter().map(|g| g.lasts_until).max()
    }
}

/// The grants `fauna.principals.revoke` ends nest-side, as the owner's log
/// holds them: the principal's current third-party grants to `holder`. The
/// revoking app records a `Revoke` event for each (the nest deletes the blobs;
/// the log is where the history lens reads why they vanished,
/// `third-party.md` § The principal model).
pub fn grants_ended_by_principal_revoke(
    ledger: &SuccessionLedger,
    holder: &[u8],
) -> Vec<CurrentGrant> {
    current_grants(ledger)
        .into_iter()
        .filter(|g| g.holder == holder && is_principal_scope(&g.scope))
        .collect()
}

/// A third-party principal's two attested keys — the holder (X25519) and the
/// writer (Ed25519) — as the roster names them or as a consent attests them.
/// `None` is a key not named: a standard client attests neither.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PrincipalKeys {
    pub holder: Option<[u8; 32]>,
    pub writer: Option<[u8; 32]>,
}

/// Which of a principal's keys an approve replaces, when it ends grants the
/// old one reached (`third-party.md` § The principal model → *Key
/// replacement*). A replaced holder ends every third-party grant to it, so it
/// outranks a writer replaced in the same ceremony — the nest's own order at
/// `/oauth/token`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplacedKey {
    /// The attested holder differs from the roster's: every third-party grant
    /// to the old holder ends.
    Holder([u8; 32]),
    /// The holder stays, the attested writer differs from the roster's: every
    /// third-party grant to the holder whose `content.write` tuple names the
    /// old writer ends.
    Writer {
        holder: [u8; 32],
        old_writer: [u8; 32],
    },
}

/// What approving a consent attesting `attested` replaces of the principal
/// the roster names as `roster`. `None` when nothing the roster names changes:
/// a first ceremony (no roster row, so `roster` is empty), a same-key
/// re-consent, or a key not attested at all (the nest keeps the row's key).
pub fn replaced_key(roster: PrincipalKeys, attested: PrincipalKeys) -> Option<ReplacedKey> {
    let replaces = |old: Option<[u8; 32]>, new: Option<[u8; 32]>| match (old, new) {
        (Some(old), Some(new)) if old != new => Some(old),
        _ => None,
    };
    if let Some(old) = replaces(roster.holder, attested.holder) {
        return Some(ReplacedKey::Holder(old));
    }
    let old_writer = replaces(roster.writer, attested.writer)?;
    Some(ReplacedKey::Writer {
        holder: roster.holder?,
        old_writer,
    })
}

/// The grants an approved key replacement ends nest-side, as the owner's log
/// holds them — the selectors the nest's `/oauth/token` applies
/// (`upsert_principal_in_tx`), over the owner's own log and confined to
/// third-party grants as [`grants_ended_by_principal_revoke`] is. A writer
/// replacement selects by the old writer's factor, so the grant the same
/// approve mints to the new writer is never among them. The approving app
/// ends each on the nest, then records its `Revoke`.
pub fn grants_ended_by_key_replacement(
    ledger: &SuccessionLedger,
    replaced: ReplacedKey,
) -> Vec<CurrentGrant> {
    match replaced {
        ReplacedKey::Holder(old) => grants_ended_by_principal_revoke(ledger, &old),
        ReplacedKey::Writer { holder, old_writer } => {
            grants_ended_by_principal_revoke(ledger, &holder)
                .into_iter()
                .filter(|g| g.scope.iter().any(|t| names_writer(t, &old_writer)))
                .collect()
        }
    }
}

/// Whether `tuple` is a `content.write` license confined to `writer`.
fn names_writer(tuple: &GrantEventScope, writer: &[u8; 32]) -> bool {
    tuple.class == fauna_core::grant_event::CLASS_CONTENT_WRITE
        && tuple
            .factor()
            .and_then(fauna_core::grant_event::parse_writer_factor)
            .is_some_and(|w| &w == writer)
}

// ── The T16 custody facet projections ─────────────────────────
//
// The custody rows' pure folds — same contract as the trust facet above: no
// clock, no transport, wasm-clean; the caller supplies `now`. Owner side
// reads the ceremony state's `granted` (`fauna.state.custody-ceremony`) + the
// grant-event fold; host side reads its `held`. Copy/labels stay in the shells (i18n); these
// types carry facts only.

/// A receipt older than this renders **stale** — degraded redundancy the
/// owner sees (`nests.md` § Trust facet — custody rows, the three-state
/// honesty rule). Two check-in intervals: one missed cadence is weather, two
/// is a signal. A UI-honesty threshold, deliberately NOT T18's aging margin
/// (which gates dehydration, not rendering), and a hard-coded constant —
/// nobody would configure when their UI starts telling the truth.
pub const CUSTODY_RECEIPT_STALE_MICROS: u64 =
    2 * fauna_core::custody_receipt::CUSTODY_RECEIPT_INTERVAL_MICROS;

/// The three receipt-freshness states — three different words by spec, never
/// collapsed, never empty (`nest-trust-custody-receipt-status` /
/// `custody-holder-receipt-status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptState {
    /// A receipt younger than [`CUSTODY_RECEIPT_STALE_MICROS`].
    Fresh,
    /// The latest receipt is older than the threshold — degraded redundancy.
    Stale,
    /// No receipt has ever arrived (a real state the row must render).
    NoReceiptYet,
}

/// The three receipt-freshness words for [`ReceiptState`], never collapsed
/// and never empty — the same three-state honesty rule both custody sides
/// render (linux and tui each hand-carried this identical mapping; found by
/// the dev-fleet near-duplicate-function scanner's cross-crate pass, 0.696
/// similarity).
pub fn receipt_text(state: ReceiptState) -> &'static str {
    match state {
        ReceiptState::Fresh => fauna_i18n::strings::admin::custody_hosting::RECEIPT_FRESH,
        ReceiptState::Stale => fauna_i18n::strings::admin::custody_hosting::RECEIPT_STALE,
        ReceiptState::NoReceiptYet => fauna_i18n::strings::admin::custody_hosting::RECEIPT_NONE,
    }
}

/// The receipt facts a custody row renders — decoded from the stored signed
/// envelope (verification happened at ingest; a view decodes, it does not
/// re-judge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustodyReceiptView {
    pub held_bytes: u64,
    pub retained_bytes_cap: u64,
    /// [`fauna_core::custody_receipt::CustodyReceipt::is_degraded`] — evicted
    /// or capped-short coverage.
    pub degraded: bool,
    pub attested_at_micros: u64,
}

/// One owner-side custody row — "who holds my data" (`custody-holder-card`
/// on Devices; the nest-shaped twin renders from the same fold once a host
/// account can be a nest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyRowView {
    pub grant_id: Vec<u8>,
    /// The counterpart account holding custody.
    pub host: [u8; 32],
    /// The serving principal key the ceremony's accept bound (`None` while
    /// the ceremony is still pending the host's consent) — a device
    /// principal, or the host's pinned nest actor identity when
    /// [`Self::custodian_nest_url`] is present.
    pub custodian_key: Option<[u8; 32]>,
    /// `Some` = the bound custodian is the host's NEST (the nest-custodian
    /// identity fact): the row renders in the Nests-page
    /// `nest-trust-custody-*` family, keyed by the nest identity; `None` =
    /// a device custodian, the Devices-page `custody-holder-*` family. One
    /// custody never renders in both.
    pub custodian_nest_url: Option<String>,
    /// The granted coverage, from the offer this ceremony rides.
    pub scopes: Option<fauna_core::custody_grant::CustodyScopeSet>,
    /// The minted grant's `window_end` (unix secs) — `None` while pending.
    pub lasts_until: Option<u64>,
    /// `now`-relative liveness of the minted grant — `None` while pending.
    pub liveness: Option<GrantLiveness>,
    pub receipt: Option<CustodyReceiptView>,
    pub receipt_state: ReceiptState,
    /// The ceremony has not completed (offer out, or accept captured but the
    /// mint/deliver still owed) — renders as a pending row, not a live one.
    pub pending: bool,
}

/// One host-side held custody — "what I hold for others"
/// (`custody-held-card`). The held-bytes/degraded facts come from this
/// device's OWN latest minted receipt — the same numbers the owner sees, by
/// construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldCustodyView {
    pub grant_id: Vec<u8>,
    /// Whose data this device holds.
    pub owner: [u8; 32],
    pub scopes: Option<fauna_core::custody_grant::CustodyScopeSet>,
    /// The budget in force. Seeded from the accept; a caller with registry
    /// reach overlays the row's live value ([`overlay_held_registry_rows`]) —
    /// the row is what the budget pass meters against, and the post-accept
    /// adjust writes the row, never the accept.
    pub retained_bytes_cap: u64,
    pub receipt: Option<CustodyReceiptView>,
    /// The host stopped holding (registry-row fact, overlaid the same way;
    /// `false` when no row is readable).
    pub stopped: bool,
}

/// Overlay the registry rows' live facts onto the config fold: the budget in
/// force (`retained_bytes_cap` — the post-accept adjust writes the row) and
/// the stop mark. Rows are matched by grant id; a custody with no readable
/// row keeps its accept-seeded values.
pub fn overlay_held_registry_rows(
    held: &mut [HeldCustodyView],
    rows: &[fauna_core::custodies_held::CustodyHeld],
) {
    for h in held.iter_mut() {
        if let Some(row) = rows.iter().find(|r| r.grant_id == h.grant_id) {
            if row.retained_bytes_cap > 0 {
                h.retained_bytes_cap = row.retained_bytes_cap;
            }
            h.stopped = row.stopped;
        }
    }
}

/// One incoming custody offer awaiting consent (`custody-offer-card` —
/// indexed: two owners can have offers pending at once).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyOfferView {
    pub grant_id: Vec<u8>,
    /// The account asking this device to hold.
    pub owner: [u8; 32],
    pub scopes: fauna_core::custody_grant::CustodyScopeSet,
    pub offered_at_micros: u64,
    /// Whether a NEST can hold this custody: the offer names the owner's
    /// nest, the only route a nest custodian pulls over (the device-or-nest
    /// bullet, item 6). The consent card renders the target select
    /// (`custody-offer-target-select`) only when this is true AND the app
    /// holds a pinned nest identity — the pin is process state, so the shell
    /// contributes that half.
    pub nest_can_hold: bool,
}

fn decode_receipt_view(bytes: &[u8]) -> Option<CustodyReceiptView> {
    let env: fauna_core::encoding::EmbedAsBytes =
        fauna_core::encoding::canonical_decode(bytes).ok()?;
    let (signed_bytes, _env) = env.into_signed().ok()?;
    let r: fauna_core::custody_receipt::CustodyReceipt =
        fauna_core::encoding::decode_signed_bytes(&signed_bytes).ok()?;
    Some(CustodyReceiptView {
        held_bytes: r.held_bytes,
        retained_bytes_cap: r.retained_bytes_cap,
        degraded: r.is_degraded(),
        attested_at_micros: r.attested_at.0,
    })
}

fn receipt_state(
    latest_receipt_at_micros: u64,
    has_receipt: bool,
    now_micros: u64,
) -> ReceiptState {
    if !has_receipt {
        ReceiptState::NoReceiptYet
    } else if now_micros.saturating_sub(latest_receipt_at_micros) >= CUSTODY_RECEIPT_STALE_MICROS {
        ReceiptState::Stale
    } else {
        ReceiptState::Fresh
    }
}

/// The owner-side fold: every ceremony this account has offered, projected
/// to what a custody row renders. A **revoked** custody (minted once, no
/// longer in [`current_grants`]) drops out of the Now rows — its trail lives
/// in the History lens's event log, exactly like every other grant.
///
/// Deterministic order: pending ceremonies last, then soonest `lasts_until`
/// first, tie-broken on `grant_id`.
pub fn custody_rows(
    custody: &CustodyConfig,
    ledger: &SuccessionLedger,
    now_micros: u64,
) -> Vec<CustodyRowView> {
    let now_secs = now_micros / 1_000_000;
    let current: std::collections::BTreeMap<Vec<u8>, fauna_core::grant_event::CurrentGrant> =
        current_grants(ledger)
            .into_iter()
            .filter(|g| crate::custody_grants::is_custody_grant(&g.scope))
            .map(|g| (g.grant_id.clone(), g))
            .collect();
    let mut rows: Vec<CustodyRowView> = custody
        .granted
        .iter()
        .filter_map(|g| {
            let pending = !g.minted;
            let cur = current.get(&g.grant_id);
            if !pending && cur.is_none() {
                return None; // revoked — History's business, not a Now row
            }
            let scopes = fauna_core::encoding::canonical_decode(&g.offer)
                .ok()
                .and_then(|env| crate::custody_ceremony::decode_offer(&env).ok())
                .map(|o| o.scopes);
            let accept = (!g.accept.is_empty())
                .then(|| crate::custody_ceremony::decode_accept_record_granted(g).ok())
                .flatten();
            let custodian_key = accept.as_ref().map(|a| a.custodian_key);
            let custodian_nest_url = accept.and_then(|a| a.custodian_nest_url);
            let has_receipt = !g.latest_receipt.is_empty();
            Some(CustodyRowView {
                grant_id: g.grant_id.clone(),
                host: g.host,
                custodian_key,
                custodian_nest_url,
                scopes,
                lasts_until: cur.map(|c| c.window_end),
                liveness: cur.map(|c| compute_liveness(c.window_end, now_secs, false)),
                receipt: has_receipt
                    .then(|| decode_receipt_view(&g.latest_receipt))
                    .flatten(),
                receipt_state: receipt_state(g.latest_receipt_at.0, has_receipt, now_micros),
                pending,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        (a.pending, a.lasts_until.unwrap_or(u64::MAX), &a.grant_id).cmp(&(
            b.pending,
            b.lasts_until.unwrap_or(u64::MAX),
            &b.grant_id,
        ))
    });
    rows
}

/// The host-side fold: custodies this account accepted (and has not
/// declined), with the budget the accept bound and this device's own latest
/// minted receipt. Deterministic order: by owner, then grant id.
pub fn held_custodies(custody: &CustodyConfig) -> Vec<HeldCustodyView> {
    let mut rows: Vec<HeldCustodyView> = custody
        .held
        .iter()
        .filter(|h| !h.accept.is_empty() && !h.declined && !h.removed)
        .map(|h| {
            let accept = crate::custody_ceremony::decode_accept_record(h).ok();
            let scopes = fauna_core::encoding::canonical_decode(&h.offer)
                .ok()
                .and_then(|env| crate::custody_ceremony::decode_offer(&env).ok())
                .map(|o| o.scopes);
            HeldCustodyView {
                grant_id: h.grant_id.clone(),
                owner: h.owner,
                scopes,
                retained_bytes_cap: accept.map(|a| a.retained_bytes_cap).unwrap_or_default(),
                receipt: (!h.receipt.is_empty())
                    .then(|| decode_receipt_view(&h.receipt))
                    .flatten(),
                stopped: false,
            }
        })
        .collect();
    rows.sort_by(|a, b| (a.owner, &a.grant_id).cmp(&(b.owner, &b.grant_id)));
    rows
}

/// The consent fold: offers this account has neither accepted nor declined,
/// whose term has not passed (an expired offer is spent on both sides —
/// [`crate::custody_ceremony::held_offer_expired`]).
/// Deterministic order: oldest offer first (first asked, first answered).
pub fn custody_offers(custody: &CustodyConfig, now_micros: u64) -> Vec<CustodyOfferView> {
    let now = fauna_core::data::Timestamp(now_micros);
    let mut rows: Vec<CustodyOfferView> = custody
        .held
        .iter()
        .filter(|h| h.accept.is_empty() && !h.declined && !h.removed && !h.offer.is_empty())
        .filter(|h| !crate::custody_ceremony::held_offer_expired(h, now))
        .filter_map(|h| {
            let env: fauna_core::encoding::EmbedAsBytes =
                fauna_core::encoding::canonical_decode(&h.offer).ok()?;
            let offer = crate::custody_ceremony::decode_offer(&env).ok()?;
            Some(CustodyOfferView {
                grant_id: h.grant_id.clone(),
                owner: h.owner,
                scopes: offer.scopes,
                offered_at_micros: offer.offered_at.0,
                nest_can_hold: offer.owner_nest_url.is_some(),
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        (a.offered_at_micros, &a.grant_id).cmp(&(b.offered_at_micros, &b.grant_id))
    });
    rows
}

/// The custody facet's fold output — the three projections the Devices page's
/// T16 families render (`docs/goal/ui/devices.md` § Custody facet): owner
/// side (`custody-holder-card`), host side (`custody-held-card`), and the
/// incoming-offer consent surface (`custody-offer-card`).
///
/// Lifted out of tui 2026-08-17 so the six
/// trickle-down legs share one fold instead of each rebuilding it — priority
/// #2. The bundle is deliberately **pure and wasm-clean**: it folds the
/// ceremony state and the grant log and nothing else, so the web leg reaches it through the same
/// door the native apps do. The one native-only fact — the R14 (account-data-plane.md § The ratified decisions) registry row's
/// live budget and stop mark — is applied afterwards by
/// [`CustodyFacetSnapshot::overlay_registry_rows`], whose rows the caller
/// reads from its account store (`fauna-sync-engine` is native-only and must
/// NOT become a dependency of this crate; see the crate's wasm-clean dep
/// discipline in `Cargo.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CustodyFacetSnapshot {
    /// Owner side — "who holds my data" (`custody-holder-card`).
    pub rows: Vec<CustodyRowView>,
    /// Host side — "what I hold for others" (`custody-held-card`).
    pub held: Vec<HeldCustodyView>,
    /// Incoming offers awaiting consent (`custody-offer-card`, indexed).
    pub offers: Vec<CustodyOfferView>,
}

impl CustodyFacetSnapshot {
    /// Overlay the R14 registry rows' live facts onto the config fold — the
    /// budget in force and the stop mark. Thin wrapper over
    /// [`overlay_held_registry_rows`] so a caller holding an account store
    /// applies it without reaching into the fold's internals.
    pub fn overlay_registry_rows(&mut self, rows: &[fauna_core::custodies_held::CustodyHeld]) {
        overlay_held_registry_rows(&mut self.held, rows);
    }
}

/// Fold all three custody projections in one pass — the shared half of what
/// every app's Devices page needs. Pure: no I/O, no clock read (the caller
/// passes `now_micros`, which is what makes the receipt three-state honesty
/// testable without a fake clock).
///
/// The caller's remaining work is the impure edges: reading the ceremony
/// state and the grant log off the account store
/// (`fauna_client_config::{CustodyCeremonyStore, SuccessionLedgerStore}`)
/// and, on a native app, the registry overlay.
pub fn fold_custody_facet(
    custody: &CustodyConfig,
    ledger: &SuccessionLedger,
    now_micros: u64,
) -> CustodyFacetSnapshot {
    CustodyFacetSnapshot {
        rows: custody_rows(custody, ledger, now_micros),
        held: held_custodies(custody),
        offers: custody_offers(custody, now_micros),
    }
}

/// The `custody-holder-receipt-status` / `nest-trust-custody-receipt-status`
/// line — the A7 three-state honesty rule as a shared DECISION: fresh, stale,
/// and no-receipt-yet are three different strings and never collapse or go
/// empty (`ui/nests.md` § Trust facet — custody rows, stated once there).
///
/// Split for client-side formatting like `fauna_core::format::
/// BackupLastUploadDisplay`: the timestamp is deliberately NOT rendered here.
/// `format_unix_local` is native-only by construction (it needs the OS timezone
/// database, which wasm32 lacks without a JS bridge), so formatting it in this
/// crate would cost the wasm-cleanliness the web leg's fold depends on. The
/// DECISION — which state maps to which key, and whether a timestamp is even
/// involved — is what stays shared.
///
/// Shared so the seven legs cannot drift on which state reads which way — the
/// drift that would quietly turn a stale custodian into a
/// healthy-looking row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyReceiptStatusDisplay {
    /// The outer key. Carries a `{when}` placeholder for the two timestamped
    /// states; the no-receipt state has nothing to substitute.
    pub label: fauna_core::localized::LocalizedText,
    /// Epoch SECONDS for the client to format and substitute into `{when}`
    /// (native: `fauna_core::format::format_unix_local`; web: its own
    /// locale-aware formatter). `None` when there is no receipt to date.
    pub attested_at_secs: Option<i64>,
}

/// Fold a receipt state into its status line — see [`CustodyReceiptStatusDisplay`].
pub fn custody_receipt_status_display(
    state: ReceiptState,
    attested_at_micros: Option<u64>,
) -> CustodyReceiptStatusDisplay {
    let key = match state {
        ReceiptState::Fresh => "devices.custody_receipt_fresh",
        ReceiptState::Stale => "devices.custody_receipt_stale",
        ReceiptState::NoReceiptYet => "devices.custody_receipt_none",
    };
    CustodyReceiptStatusDisplay {
        label: fauna_core::localized::LocalizedText::key(key),
        // The no-receipt state renders no timestamp even if one were passed —
        // "no confirmation yet" must never carry a date.
        attested_at_secs: match state {
            ReceiptState::NoReceiptYet => None,
            _ => attested_at_micros.map(|m| (m / 1_000_000) as i64),
        },
    }
}

/// [`custody_receipt_status_display`] resolved to the finished
/// `custody-holder-receipt-status` / `nest-trust-custody-receipt-status` line
/// against the caller's own i18n lookup — the resolve that `fauna-linux`'s
/// `i18n::custody_receipt_status` and `fauna-tui`'s
/// `settings::devices::receipt_status_text` held byte-identical: resolve the
/// label, then substitute `{when}` with
/// [`fauna_core::format::format_unix_local`] when the state carries a
/// timestamp. Native-only (`local-clock`), exactly like `fauna_core::format`'s
/// own `*_text` doors (`value-formatting.md` § Resolving a two-level display) —
/// the timestamp render needs the OS timezone database, unreachable from
/// wasm32 without a JS bridge. The web leg keeps resolving the same two levels
/// with its own locale-aware formatter, which is why this stays a native-Rust
/// convenience and never the contract.
#[cfg(feature = "local-clock")]
pub fn custody_receipt_status_text<F, S>(
    state: ReceiptState,
    attested_at_micros: Option<u64>,
    lookup: F,
) -> String
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    let d = custody_receipt_status_display(state, attested_at_micros);
    let text = d.label.resolve(&lookup);
    match d.attested_at_secs {
        Some(secs) => text.replace("{when}", &fauna_core::format::format_unix_local(secs)),
        None => text,
    }
}

/// The `custody-holder-held-bytes` / `custody-held-bytes` line, split for
/// client-side i18n exactly like `fauna_core::format::BackupUsageDisplay` and
/// for the same reason: a [`fauna_core::localized::LocalizedText`] arg is a
/// FLAT string, so the two inner [`fauna_core::format::byte_size`] texts must
/// be resolved client-side before they are substituted into `label`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyHeldBytesDisplay {
    /// The outer key, carrying `{held}` and `{cap}` placeholders the client
    /// fills from the two resolved fields below.
    pub label: fauna_core::localized::LocalizedText,
    /// Held bytes, or the em-dash placeholder when no receipt has landed.
    pub held: fauna_core::localized::LocalizedText,
    /// The budget in force, same placeholder rule.
    pub cap: fauna_core::localized::LocalizedText,
    /// The receipt honestly reports evicted or capped-short coverage. **Orthogonal
    /// to freshness** — a FRESH receipt can truthfully say "I dropped payload" —
    /// so a client must render this alongside the status line, never instead of
    /// it. The badge's own key is [`CUSTODY_DEGRADED_BADGE_KEY`].
    pub degraded: bool,
}

/// The i18n key for the degraded marker that rides a
/// [`CustodyHeldBytesDisplay`] whose `degraded` is set. Named here so the legs
/// agree on the key rather than each hardcoding it.
pub const CUSTODY_DEGRADED_BADGE_KEY: &str = "devices.custody_degraded_badge";

/// The no-receipt placeholder for `held` / `cap`. Locale-invariant punctuation,
/// so it is carried as a bare key and resolves through `LocalizedText`'s
/// missing-key fallback (which returns the key itself) on every client —
/// deliberate, not an oversight. Shared so the legs cannot each pick a
/// different glyph for "nothing confirmed yet".
pub const CUSTODY_NO_RECEIPT_PLACEHOLDER: &str = "—";

/// Fold a receipt into the held-bytes line. `None` (no receipt yet) renders
/// [`CUSTODY_NO_RECEIPT_PLACEHOLDER`] rather than zeroes — "nothing confirmed
/// yet" and "confirmed zero bytes" are different facts and must not look alike.
pub fn custody_held_bytes_display(receipt: Option<&CustodyReceiptView>) -> CustodyHeldBytesDisplay {
    const DASH: &str = CUSTODY_NO_RECEIPT_PLACEHOLDER;
    match receipt {
        Some(r) => CustodyHeldBytesDisplay {
            label: fauna_core::localized::LocalizedText::key("devices.custody_held_bytes"),
            held: fauna_core::format::byte_size(r.held_bytes),
            cap: fauna_core::format::byte_size(r.retained_bytes_cap),
            degraded: r.degraded,
        },
        None => CustodyHeldBytesDisplay {
            label: fauna_core::localized::LocalizedText::key("devices.custody_held_bytes"),
            held: fauna_core::localized::LocalizedText::key(DASH),
            cap: fauna_core::localized::LocalizedText::key(DASH),
            degraded: false,
        },
    }
}

/// [`custody_held_bytes_display`] resolved to the finished
/// `custody-holder-held-bytes` / `custody-held-bytes` line — the held-bytes
/// twin of [`custody_receipt_status_text`], the resolve `fauna-linux`'s
/// `i18n::custody_held_bytes` and `fauna-tui`'s
/// `settings::devices::held_bytes_text` held byte-identical once both apps
/// were on one string table. Gated alongside its sibling for
/// the same one-story reason `fauna_core::format::backup_usage_text` states —
/// it needs no local clock of its own, but ships behind `local-clock` so the
/// custody family stays one gate rather than two. The two inner byte texts are
/// themselves `LocalizedText`s and are resolved before substitution — a
/// `LocalizedText` arg is a flat string. `degraded` rides independently of
/// freshness (a fresh receipt can honestly report dropped payload), so it
/// appends rather than replaces.
#[cfg(feature = "local-clock")]
pub fn custody_held_bytes_text<F, S>(receipt: Option<&CustodyReceiptView>, lookup: F) -> String
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    let d = custody_held_bytes_display(receipt);
    let text = d
        .label
        .resolve(&lookup)
        .replace("{held}", &d.held.resolve(&lookup))
        .replace("{cap}", &d.cap.resolve(&lookup));
    if d.degraded {
        format!(
            "{text} — {}",
            fauna_core::localized::LocalizedText::key(CUSTODY_DEGRADED_BADGE_KEY).resolve(&lookup)
        )
    } else {
        text
    }
}

/// The `custody-held-budget-input` seed texts, one per held row and aligned
/// with [`CustodyFacetSnapshot::held`] — the budget in force, on the shared
/// 1024-unit byte scale (`behavior/value-formatting.md`).
///
/// Shared as a **decision**, resolved per app: each leg maps the returned
/// `LocalizedText`s through its own string lookup, the same split
/// `fauna_core::format::byte_size` already uses everywhere. Shared so the six
/// trickle-down legs do not each hand-roll the loop and drift on which value
/// seeds the input (it is the row's live cap, never the accept's).
pub fn budget_draft_texts(
    facet: &CustodyFacetSnapshot,
) -> Vec<fauna_core::localized::LocalizedText> {
    facet
        .held
        .iter()
        .map(|h| fauna_core::format::byte_size(h.retained_bytes_cap))
        .collect()
}

/// The `admin-custody-hosting-budget` cell for [`AdminHostingRowView::retained_bytes_cap`].
/// A zero cap means "no cap on the row — the pump substitutes the hard-coded
/// default", the opposite of "zero bytes allowed", so a printed `0 B` would
/// state the opposite of the truth. `AdminHostingRowView` itself stays
/// facts-only (its own doc comment); this is a companion text-derivation
/// door, the same division of labor [`custody_held_bytes_text`] already uses
/// for [`CustodyReceiptView`] in this module — `fauna-tui`'s `budget_text`
/// and `fauna-linux`'s `custody_hosting_budget_text` held byte-identical
/// bodies (0.500 similarity, dev-fleet near-duplicate-function scan).
pub fn custody_hosting_budget_text<F, S>(cap: u64, lookup: F) -> String
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    if cap == 0 {
        fauna_i18n::strings::admin::custody_hosting::BUDGET_DEFAULT.to_string()
    } else {
        fauna_core::format::byte_size(cap).resolve(&lookup)
    }
}

/// One row on the ADMIN custody-hosting registry — the nest-wide list
/// `fauna.admin.custody_hosting.list` serves, projected to what a surface
/// renders (`account-data-plane.md` § Two-sided
/// bounds).
///
/// Facts only, like every view in this module: the three-state receipt word,
/// the byte magnitudes and the confirm copy are the shell's own i18n. The
/// pair `(host_actor_id, grant_id)` is exactly the remove door's key, carried
/// here so a row can be removed from what the list rendered — never
/// re-derived by the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminHostingRowView {
    /// Hex actor id of the DEPOSITING host — who armed this pull.
    pub host_actor_id: String,
    /// Hex actor id of the custodied owner.
    pub owner_actor_id: String,
    /// The pull leg's only dial anchor. Rendered verbatim: an admin reading
    /// this surface is looking for exactly the address the nest dials, and a
    /// prettified one would be the wrong fact.
    pub owner_nest_url: String,
    pub grant_id: Vec<u8>,
    /// The budget in force (`0` = the pump substitutes the hard-coded
    /// default; the shell says so rather than printing a zero).
    pub retained_bytes_cap: u64,
    /// Pump-metered bytes currently held (post-eviction); `0` until the first
    /// pass completes.
    pub held_bytes: u64,
    /// The host's stop mark — paused, NOT reclaimed: a stopped row still
    /// holds its bytes, which is precisely why the admin needs remove.
    pub stopped: bool,
    /// The same three-state honesty word both custody sides already render.
    pub receipt_state: ReceiptState,
}

/// Project the admin list reply into renderable rows.
///
/// **Order: held bytes DESCENDING**, tie-broken on `(host, owner, grant_id)`
/// for determinism. Registry order would be arbitrary to the one question
/// this surface exists to answer — *which rows are filling my disk* — so the
/// heaviest row is the first one an admin sees. The tie-break never touches
/// the nest's `ORDER BY`, so the rendering cannot come to depend on it.
pub fn admin_hosting_rows(
    reply: &fauna_protocol::custody::AdminHostingListReply,
    now_micros: u64,
) -> Vec<AdminHostingRowView> {
    let mut rows: Vec<AdminHostingRowView> = reply
        .rows
        .iter()
        .map(|r| AdminHostingRowView {
            host_actor_id: r.host_actor_id.clone(),
            owner_actor_id: r.item.owner_actor_id.clone(),
            owner_nest_url: r.item.owner_nest_url.clone(),
            grant_id: r.item.grant_id.to_vec(),
            retained_bytes_cap: r.item.retained_bytes_cap,
            held_bytes: r.item.held_bytes,
            stopped: r.item.stopped,
            receipt_state: receipt_state(
                r.item.last_receipt_at,
                r.item.last_receipt_at != 0,
                now_micros,
            ),
        })
        .collect();
    rows.sort_by(|a, b| {
        b.held_bytes
            .cmp(&a.held_bytes)
            .then_with(|| a.host_actor_id.cmp(&b.host_actor_id))
            .then_with(|| a.owner_actor_id.cmp(&b.owner_actor_id))
            .then_with(|| a.grant_id.cmp(&b.grant_id))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grant_log::{record_mint, record_renew, record_revoke};
    use fauna_core::identity::{ActorId, ActorKeypair};

    fn empty_ledger() -> SuccessionLedger {
        SuccessionLedger::empty(ActorId([9u8; 32]))
    }

    fn mail_scope() -> GrantEventScope {
        GrantEventScope {
            class: "content.read".into(),
            kind: Some("mail".into()),
            tier: None,
        }
    }

    fn holder_set(holders: &[[u8; 32]]) -> BTreeSet<Vec<u8>> {
        holders.iter().map(|h| h.to_vec()).collect()
    }

    // ── compute_liveness (the pure classifier) ───────────────────────────────

    #[test]
    fn liveness_active_far_from_expiry() {
        // 90 days out, un-blessed → Active (well past the 14-day warn window).
        assert_eq!(
            compute_liveness(1000 + 90 * 86400, 1000, false),
            GrantLiveness::Active
        );
    }

    #[test]
    fn liveness_expiring_soon_inside_renew_ahead() {
        // 10 days out (< 14-day threshold), un-blessed → ExpiringSoon.
        assert_eq!(
            compute_liveness(1000 + 10 * 86400, 1000, false),
            GrantLiveness::ExpiringSoon
        );
        // Exactly at the threshold boundary is still "soon" (<=).
        assert_eq!(
            compute_liveness(1000 + RENEW_AHEAD_SECS, 1000, false),
            GrantLiveness::ExpiringSoon
        );
    }

    #[test]
    fn liveness_expired_at_and_past_window_end() {
        assert_eq!(compute_liveness(1000, 1000, false), GrantLiveness::Expired);
        assert_eq!(compute_liveness(1000, 2000, false), GrantLiveness::Expired);
    }

    #[test]
    fn liveness_auto_renewing_when_blessed_and_live() {
        // Blessed + live → AutoRenewing even when very near expiry (background
        // renewal keeps it alive, so it's not the user's problem).
        assert_eq!(
            compute_liveness(1000 + 86400, 1000, true),
            GrantLiveness::AutoRenewing
        );
    }

    #[test]
    fn liveness_expired_supersedes_blessed() {
        // A blessed box whose window still elapsed reads Expired — background
        // renewal has stopped firing, so the user must act.
        assert_eq!(compute_liveness(1000, 2000, true), GrantLiveness::Expired);
    }

    // ── trust_facet_for_holders (the fold) ───────────────────────────────────

    #[test]
    fn facet_filters_to_this_nests_holders() {
        let mut cfg = empty_ledger();
        let kp = ActorKeypair::generate();
        let mine = [0x11u8; 32];
        let other = [0x22u8; 32];
        // One grant to *this* nest's holder, one to another nest's holder.
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            mine,
            vec![mail_scope()],
            0,
            90 * 86400,
            0,
        )
        .unwrap();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [2u8; 16],
            other,
            vec![mail_scope()],
            0,
            90 * 86400,
            0,
        )
        .unwrap();

        let facet = trust_facet_for_holders(&cfg, &holder_set(&[mine]), 1000, &BTreeSet::new());
        assert_eq!(facet.grants.len(), 1, "only this nest's holder's grant");
        assert_eq!(facet.grants[0].grant_id, vec![1u8; 16]);
        assert_eq!(facet.grants[0].holder, mine.to_vec());
        assert_eq!(facet.grants[0].scope, vec![mail_scope()]);
        assert_eq!(facet.grants[0].lasts_until, 90 * 86400);
        assert_eq!(facet.grants[0].liveness, GrantLiveness::Active);
    }

    #[test]
    fn facet_is_empty_when_no_grant_targets_this_nest() {
        let mut cfg = empty_ledger();
        let kp = ActorKeypair::generate();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            [0x22; 32],
            vec![mail_scope()],
            0,
            90 * 86400,
            0,
        )
        .unwrap();
        // This nest's holder set is disjoint from the only grant's holder.
        let facet =
            trust_facet_for_holders(&cfg, &holder_set(&[[0x11; 32]]), 1000, &BTreeSet::new());
        assert!(facet.grants.is_empty(), "the not-trusted empty state");
    }

    #[test]
    fn facet_excludes_revoked_grant() {
        let mut cfg = empty_ledger();
        let kp = ActorKeypair::generate();
        let h = [0x11u8; 32];
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            h,
            vec![mail_scope()],
            0,
            90 * 86400,
            0,
        )
        .unwrap();
        record_revoke(&mut cfg, kp.signing_key(), [1u8; 16], h, 100).unwrap();
        let facet = trust_facet_for_holders(&cfg, &holder_set(&[h]), 1000, &BTreeSet::new());
        assert!(
            facet.grants.is_empty(),
            "a revoked grant leaves the Now lens (current_grants omits it)"
        );
    }

    #[test]
    fn facet_keeps_expired_grant_marked_expired() {
        let mut cfg = empty_ledger();
        let kp = ActorKeypair::generate();
        let h = [0x11u8; 32];
        // Window ends at 500; now is 1000 → past expiry but NOT revoked.
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            h,
            vec![mail_scope()],
            0,
            500,
            0,
        )
        .unwrap();
        let facet = trust_facet_for_holders(&cfg, &holder_set(&[h]), 1000, &BTreeSet::new());
        assert_eq!(
            facet.grants.len(),
            1,
            "expired grants stay visible for renew"
        );
        assert_eq!(facet.grants[0].liveness, GrantLiveness::Expired);
    }

    #[test]
    fn facet_marks_blessed_holder_grant_auto_renewing() {
        let mut cfg = empty_ledger();
        let kp = ActorKeypair::generate();
        let h = [0x11u8; 32];
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            h,
            vec![mail_scope()],
            0,
            90 * 86400,
            0,
        )
        .unwrap();
        let facet = trust_facet_for_holders(&cfg, &holder_set(&[h]), 1000, &holder_set(&[h]));
        assert_eq!(facet.grants[0].liveness, GrantLiveness::AutoRenewing);
    }

    /// A one-off grant on a blessed nest is not auto-renewed, so it must not
    /// claim to be — it reads its real liveness (8 hours is inside the
    /// renew-ahead threshold, so "expiring soon" from birth).
    #[test]
    fn facet_does_not_mark_a_one_off_grant_auto_renewing_even_when_blessed() {
        let mut cfg = empty_ledger();
        let kp = ActorKeypair::generate();
        let h = [0x11u8; 32];
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            h,
            vec![mail_scope()],
            0,
            crate::ONE_OFF_GRANT_WINDOW_SECS,
            0,
        )
        .unwrap();
        let facet = trust_facet_for_holders(&cfg, &holder_set(&[h]), 10, &holder_set(&[h]));
        assert_eq!(facet.grants[0].liveness, GrantLiveness::ExpiringSoon);
    }

    /// The loop's fold renews exactly the blessed, standing, due, unexpired
    /// grants — each by its own mint-time length, even after an earlier renew
    /// stretched its current window.
    #[test]
    fn due_renewals_are_the_blessed_standing_grants_inside_the_threshold() {
        let mut cfg = empty_ledger();
        let kp = ActorKeypair::generate();
        let blessed = [0x11u8; 32];
        let other = [0x22u8; 32];
        let day = 86_400u64;
        let mint = |cfg: &mut SuccessionLedger, id: u8, holder: [u8; 32], len: u64| {
            record_mint(
                cfg,
                kp.signing_key(),
                [id; 16],
                holder,
                vec![mail_scope()],
                0,
                len,
                0,
            )
            .unwrap();
        };
        mint(&mut cfg, 1, blessed, 90 * day); // due at now = 80d
        mint(&mut cfg, 2, blessed, 200 * day); // not yet inside the threshold
        mint(&mut cfg, 3, blessed, crate::ONE_OFF_GRANT_WINDOW_SECS); // one-off: never
        mint(&mut cfg, 4, other, 90 * day); // un-blessed holder: never
        mint(&mut cfg, 5, blessed, 30 * day); // already expired at 80d: the user's
        mint(&mut cfg, 6, blessed, 60 * day); // renewed to 85d below: due, by 60d
        record_renew(&mut cfg, kp.signing_key(), &[6u8; 16], 0, 85 * day, 1).unwrap();
        mint(&mut cfg, 7, blessed, 90 * day); // revoked: gone from the fold
        record_revoke(&mut cfg, kp.signing_key(), [7u8; 16], blessed, 2).unwrap();

        let due = grants_due_for_renewal(&cfg, &holder_set(&[blessed]), 80 * day);
        assert_eq!(
            due,
            vec![
                DueRenewal {
                    grant_id: vec![6u8; 16],
                    extend_by_secs: 60 * day
                },
                DueRenewal {
                    grant_id: vec![1u8; 16],
                    extend_by_secs: 90 * day
                },
            ]
        );
    }

    /// A bounded mail grant is due like any other standing grant of a blessed
    /// holder: the loop's renewal now carries the epoch wraps for the window
    /// it extends into (the machine computes `bounded_mail_renewal_keys` from
    /// the recorded end), so the fold names it and the page calls it
    /// auto-renewing — a subscribed labeler's trust renews itself.
    #[test]
    fn a_bounded_mail_grant_of_a_blessed_holder_is_due_for_renewal() {
        let mut cfg = empty_ledger();
        let kp = ActorKeypair::generate();
        let blessed = [0x11u8; 32];
        let day = 86_400u64;
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            blessed,
            vec![crate::grant_log::bounded_mail_event_scope()],
            0,
            90 * day,
            0,
        )
        .unwrap();

        let set = holder_set(&[blessed]);
        assert_eq!(
            grants_due_for_renewal(&cfg, &set, 80 * day),
            vec![DueRenewal {
                grant_id: vec![1u8; 16],
                extend_by_secs: 90 * day
            }]
        );
        let facet = trust_facet_for_holders(&cfg, &set, 80 * day, &set);
        assert_eq!(facet.grants[0].liveness, GrantLiveness::AutoRenewing);
    }

    #[test]
    fn facet_orders_soonest_expiry_first_then_grant_id() {
        let mut cfg = empty_ledger();
        let kp = ActorKeypair::generate();
        let h = [0x11u8; 32];
        // Three grants to the same holder with different windows / ids.
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [3u8; 16],
            h,
            vec![mail_scope()],
            0,
            300 * 86400,
            0,
        )
        .unwrap();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            h,
            vec![mail_scope()],
            0,
            100 * 86400,
            0,
        )
        .unwrap();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [2u8; 16],
            h,
            vec![mail_scope()],
            0,
            100 * 86400,
            0,
        )
        .unwrap();
        let facet = trust_facet_for_holders(&cfg, &holder_set(&[h]), 1000, &BTreeSet::new());
        // soonest window first; the two equal-window grants tie-break on id.
        let ids: Vec<Vec<u8>> = facet.grants.iter().map(|g| g.grant_id.clone()).collect();
        assert_eq!(ids, vec![vec![1u8; 16], vec![2u8; 16], vec![3u8; 16]]);
    }

    #[test]
    fn distinct_nests_fold_independently() {
        let mut cfg = empty_ledger();
        let kp = ActorKeypair::generate();
        let nest_a = [0xaau8; 32];
        let nest_b = [0xbbu8; 32];
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            nest_a,
            vec![mail_scope()],
            0,
            90 * 86400,
            0,
        )
        .unwrap();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [2u8; 16],
            nest_b,
            vec![mail_scope()],
            0,
            90 * 86400,
            0,
        )
        .unwrap();
        let facet_a = trust_facet_for_holders(&cfg, &holder_set(&[nest_a]), 1000, &BTreeSet::new());
        let facet_b = trust_facet_for_holders(&cfg, &holder_set(&[nest_b]), 1000, &BTreeSet::new());
        assert_eq!(facet_a.grants.len(), 1);
        assert_eq!(facet_a.grants[0].grant_id, vec![1u8; 16]);
        assert_eq!(facet_b.grants.len(), 1);
        assert_eq!(facet_b.grants[0].grant_id, vec![2u8; 16]);
    }

    // ── history_for_holders (the History lens) ───────────────────────────────

    #[test]
    fn history_filters_to_holder_and_keeps_revoke_most_recent_first() {
        let mut cfg = empty_ledger();
        let kp = ActorKeypair::generate();
        let mine = [0x11u8; 32];
        let other = [0x22u8; 32];
        // A full mint→renew→revoke lifecycle on this nest's holder …
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            mine,
            vec![mail_scope()],
            0,
            2000,
            100,
        )
        .unwrap();
        record_renew(&mut cfg, kp.signing_key(), &[1u8; 16], 0, 9000, 200).unwrap();
        record_revoke(&mut cfg, kp.signing_key(), [1u8; 16], mine, 300).unwrap();
        // … plus one unrelated grant to another nest's holder.
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [2u8; 16],
            other,
            vec![mail_scope()],
            0,
            2000,
            150,
        )
        .unwrap();

        let hist = history_for_holders(&cfg, &holder_set(&[mine]));
        assert_eq!(
            hist.len(),
            3,
            "only this holder's events, but ALL of them (incl. revoke)"
        );
        // Most-recent-first: revoke (300) → renew (200) → mint (100).
        assert_eq!(hist[0].kind, GrantEventKind::Revoke);
        assert_eq!(hist[0].at, 300);
        assert_eq!(hist[1].kind, GrantEventKind::Renew);
        assert_eq!(
            hist[1].window_end, 9000,
            "renew carries the extended window"
        );
        assert_eq!(hist[2].kind, GrantEventKind::Mint);
        assert_eq!(hist[2].scope, vec![mail_scope()]);
        assert!(hist.iter().all(|e| e.holder == mine.to_vec()));
    }

    /// A signed `Revoke` event carries no scope (`build_revoke_event` — the
    /// event shape is frozen for this major), but every History row must be
    /// self-describing (`nests.md` § Trust facet — History lens): the entry
    /// names what was revoked, from the scope that grant was last minted or
    /// renewed with. Found by the tier_3 revoke journey rendering
    /// "Trust revoked:  · ‹when›".
    #[test]
    fn a_revoke_entry_names_the_scope_its_grant_carried() {
        let mut cfg = empty_ledger();
        let kp = ActorKeypair::generate();
        let mine = [0x11u8; 32];
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            mine,
            vec![mail_scope()],
            0,
            2000,
            100,
        )
        .unwrap();
        record_revoke(&mut cfg, kp.signing_key(), [1u8; 16], mine, 300).unwrap();

        let hist = history_for_holders(&cfg, &holder_set(&[mine]));
        assert_eq!(hist[0].kind, GrantEventKind::Revoke);
        assert_eq!(
            hist[0].scope,
            vec![mail_scope()],
            "the revoke row must name what was revoked"
        );
    }

    #[test]
    fn history_is_empty_for_a_nest_with_no_events() {
        let mut cfg = empty_ledger();
        let kp = ActorKeypair::generate();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            [0x22; 32],
            vec![mail_scope()],
            0,
            2000,
            100,
        )
        .unwrap();
        assert!(history_for_holders(&cfg, &holder_set(&[[0x11; 32]])).is_empty());
    }

    // ── the third-party principal as a participant ───────────────────────────

    fn ext_scope() -> GrantEventScope {
        GrantEventScope {
            class: "content.read".into(),
            kind: Some("ext.app.example.com.notes".into()),
            tier: None,
        }
    }

    /// A ledger holding, for `principal`: two third-party grants (one revoked
    /// later) and one mail grant a hostile roster could name; and one
    /// third-party grant to another principal.
    fn principal_ledger(principal: [u8; 32], kp: &ActorKeypair) -> SuccessionLedger {
        let mut cfg = empty_ledger();
        let k = kp.signing_key();
        record_mint(
            &mut cfg,
            k,
            [1; 16],
            principal,
            vec![ext_scope()],
            0,
            5_000,
            10,
        )
        .unwrap();
        record_mint(
            &mut cfg,
            k,
            [2; 16],
            principal,
            vec![ext_scope()],
            0,
            9_000,
            20,
        )
        .unwrap();
        record_mint(
            &mut cfg,
            k,
            [3; 16],
            principal,
            vec![ext_scope()],
            0,
            7_000,
            30,
        )
        .unwrap();
        record_revoke(&mut cfg, k, [3; 16], principal, 40).unwrap();
        record_mint(
            &mut cfg,
            k,
            [4; 16],
            principal,
            vec![mail_scope()],
            0,
            99_000,
            50,
        )
        .unwrap();
        record_mint(
            &mut cfg,
            k,
            [5; 16],
            [0x33; 32],
            vec![ext_scope()],
            0,
            99_000,
            60,
        )
        .unwrap();
        cfg
    }

    #[test]
    fn a_principals_facet_is_its_live_third_party_grants_and_lasts_until_the_latest() {
        let p = [0x44u8; 32];
        let cfg = principal_ledger(p, &ActorKeypair::generate());
        let facet = principal_trust_facet(&cfg, &p, 1000);
        let ids: Vec<_> = facet.grants.iter().map(|g| g.grant_id[0]).collect();
        assert_eq!(
            ids,
            [1, 2],
            "revoked, non-ext and other holders' grants are not this principal's"
        );
        assert_eq!(facet.lasts_until(), Some(9_000));
        assert_eq!(facet.grants[0].liveness, GrantLiveness::ExpiringSoon);
    }

    #[test]
    fn a_principal_with_no_third_party_grant_has_an_empty_facet_and_no_lasts_until() {
        let cfg = principal_ledger([0x44; 32], &ActorKeypair::generate());
        let facet = principal_trust_facet(&cfg, &[0x55; 32], 1000);
        assert!(facet.grants.is_empty());
        assert_eq!(facet.lasts_until(), None);
    }

    #[test]
    fn a_principals_history_keeps_the_revoke_and_drops_non_ext_grants() {
        let p = [0x44u8; 32];
        let cfg = principal_ledger(p, &ActorKeypair::generate());
        let rows: Vec<_> = principal_history(&cfg, &p)
            .into_iter()
            .map(|e| (e.grant_id[0], e.kind))
            .collect();
        assert_eq!(
            rows,
            [
                (3, GrantEventKind::Revoke),
                (3, GrantEventKind::Mint),
                (2, GrantEventKind::Mint),
                (1, GrantEventKind::Mint),
            ]
        );
    }

    #[test]
    fn a_principal_revoke_ends_exactly_its_live_third_party_grants() {
        let p = [0x44u8; 32];
        let cfg = principal_ledger(p, &ActorKeypair::generate());
        let mut ids: Vec<_> = grants_ended_by_principal_revoke(&cfg, &p)
            .into_iter()
            .map(|g| g.grant_id[0])
            .collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            [1, 2],
            "never the mail grant a roster naming the MDA's key would reach"
        );
    }

    /// The folder read twin and the keyless deposit tuple are a principal's
    /// grants too (`third-party.md` § The principal model rule 4): a grant
    /// carrying only one of them is on its facet, in its history and among
    /// the grants its revoke ends.
    #[test]
    fn a_folder_read_or_deposit_only_grant_is_a_principal_grant() {
        let p = [0x44u8; 32];
        let kp = ActorKeypair::generate();
        let mut cfg = principal_ledger(p, &kp);
        let folder_read = GrantEventScope {
            class: ScopeTuple::CLASS_CONTENT_READ.into(),
            kind: Some(ScopeTuple::KIND_FOLDER.into()),
            tier: None,
        };
        let deposit = GrantEventScope {
            class: ScopeTuple::CLASS_DEPOSIT.into(),
            kind: None,
            tier: None,
        };
        for (id, scope, at) in [(6u8, &folder_read, 70), (7, &deposit, 80)] {
            record_mint(
                &mut cfg,
                kp.signing_key(),
                [id; 16],
                p,
                vec![scope.clone()],
                0,
                99_000,
                at,
            )
            .unwrap();
        }
        assert!(is_principal_scope(std::slice::from_ref(&folder_read)));
        assert!(is_principal_scope(std::slice::from_ref(&deposit)));

        let mut ids: Vec<_> = grants_ended_by_principal_revoke(&cfg, &p)
            .into_iter()
            .map(|g| g.grant_id[0])
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, [1, 2, 6, 7]);
        let mut facet: Vec<_> = principal_trust_facet(&cfg, &p, 1000)
            .grants
            .iter()
            .map(|g| g.grant_id[0])
            .collect();
        facet.sort_unstable();
        assert_eq!(facet, [1, 2, 6, 7]);
        assert!(
            principal_history(&cfg, &p)
                .iter()
                .any(|e| e.grant_id[0] == 6)
        );
    }

    /// A principal's folder read grant names its folder on the facet and in
    /// the history lens (`webdav-server.md` § Key model → *A principal's
    /// read* rule (1)): a live generation and a spent one over "photos" —
    /// its `Mint` and its `Revoke` alike — both read "photos", a grant over a
    /// set the owner no longer lists reads as deleted, and a grant that is no
    /// folder read names no folder.
    #[test]
    fn a_principals_folder_read_grant_names_its_folder_over_the_owners_sets() {
        let p = [0x44u8; 32];
        let kp = ActorKeypair::generate();
        let secret = *kp.secret_bytes();
        let mut cfg = principal_ledger(p, &kp);
        let folder_read = GrantEventScope {
            class: ScopeTuple::CLASS_CONTENT_READ.into(),
            kind: Some(ScopeTuple::KIND_FOLDER.into()),
            tier: None,
        };
        let id =
            |set: &str, generation| crate::folder_principal_grant_id(&secret, &p, set, generation);
        let k = kp.signing_key();
        let mint = |cfg: &mut SuccessionLedger, id, at| {
            record_mint(cfg, k, id, p, vec![folder_read.clone()], 0, 99_000, at).unwrap();
        };
        mint(&mut cfg, id("photos", 0), 70);
        record_revoke(&mut cfg, k, id("photos", 0), p, 75).unwrap();
        mint(&mut cfg, id("photos", 1), 80);
        mint(&mut cfg, id("gone", 0), 90);

        let sets = crate::folder_principal_set_names(
            &cfg.grant_events,
            &secret,
            &p,
            &["photos".to_owned()],
        );
        let named = |grant_id: &[u8], scope: &[GrantEventScope]| {
            principal_grant_folder(grant_id, scope, &sets)
        };
        let photos = Some(PrincipalFolder::Named("photos".into()));

        let facet = principal_trust_facet(&cfg, &p, 1000);
        let on_facet = |want: [u8; 16]| {
            let g = facet.grants.iter().find(|g| g.grant_id == want).unwrap();
            named(&g.grant_id, &g.scope)
        };
        assert_eq!(on_facet(id("photos", 1)), photos);
        assert_eq!(on_facet(id("gone", 0)), Some(PrincipalFolder::Deleted));
        assert_eq!(on_facet([1; 16]), None, "an ext grant names no folder");

        let history = principal_history(&cfg, &p);
        let revoke = history
            .iter()
            .find(|e| e.kind == GrantEventKind::Revoke && e.grant_id == id("photos", 0))
            .unwrap();
        assert_eq!(named(&revoke.grant_id, &revoke.scope), photos);
        let spent_mint = history
            .iter()
            .find(|e| e.kind == GrantEventKind::Mint && e.grant_id == id("photos", 0))
            .unwrap();
        assert_eq!(named(&spent_mint.grant_id, &spent_mint.scope), photos);
    }

    fn write_scope(writer: &[u8; 32]) -> GrantEventScope {
        GrantEventScope {
            class: fauna_core::grant_event::CLASS_CONTENT_WRITE.into(),
            kind: Some("ext.app.example.com.notes".into()),
            tier: None,
        }
        .with_factor(&fauna_core::grant_event::writer_factor(writer))
    }

    const OLD_W: [u8; 32] = [0x0A; 32];
    const NEW_W: [u8; 32] = [0x0B; 32];

    /// `principal_ledger` plus, for `principal`: a grant licensing the old
    /// writer (6), one licensing the new writer — what the approve itself
    /// mints (7).
    fn writer_ledger(principal: [u8; 32], kp: &ActorKeypair) -> SuccessionLedger {
        let mut cfg = principal_ledger(principal, kp);
        let k = kp.signing_key();
        for (id, scope) in [
            (6, vec![ext_scope(), write_scope(&OLD_W)]),
            (7, vec![ext_scope(), write_scope(&NEW_W)]),
        ] {
            record_mint(&mut cfg, k, [id; 16], principal, scope, 0, 99_000, 70).unwrap();
        }
        cfg
    }

    fn keys(holder: Option<[u8; 32]>, writer: Option<[u8; 32]>) -> PrincipalKeys {
        PrincipalKeys { holder, writer }
    }

    fn ended(cfg: &SuccessionLedger, roster: PrincipalKeys, attested: PrincipalKeys) -> Vec<u8> {
        let Some(replaced) = replaced_key(roster, attested) else {
            return Vec::new();
        };
        let mut ids: Vec<_> = grants_ended_by_key_replacement(cfg, replaced)
            .into_iter()
            .map(|g| g.grant_id[0])
            .collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn a_replaced_holder_ends_every_live_third_party_grant_to_the_old_key() {
        let p = [0x44u8; 32];
        let cfg = writer_ledger(p, &ActorKeypair::generate());
        assert_eq!(
            ended(
                &cfg,
                keys(Some(p), Some(OLD_W)),
                keys(Some([0x45; 32]), Some(OLD_W))
            ),
            [1, 2, 6, 7],
            "the revoked grant, the non-ext ones and another holder's stay"
        );
    }

    #[test]
    fn a_replaced_writer_ends_only_the_grants_licensing_the_old_writer() {
        let p = [0x44u8; 32];
        let cfg = writer_ledger(p, &ActorKeypair::generate());
        assert_eq!(
            ended(&cfg, keys(Some(p), Some(OLD_W)), keys(Some(p), Some(NEW_W))),
            [6],
            "read-only grants and the new writer's grant stay"
        );
    }

    #[test]
    fn both_keys_replaced_ends_what_the_holder_reached() {
        let p = [0x44u8; 32];
        let cfg = writer_ledger(p, &ActorKeypair::generate());
        assert_eq!(
            replaced_key(
                keys(Some(p), Some(OLD_W)),
                keys(Some([0x45; 32]), Some(NEW_W))
            ),
            Some(ReplacedKey::Holder(p))
        );
        assert_eq!(
            ended(
                &cfg,
                keys(Some(p), Some(OLD_W)),
                keys(Some([0x45; 32]), Some(NEW_W))
            ),
            [1, 2, 6, 7]
        );
    }

    #[test]
    fn nothing_ends_when_no_key_the_roster_names_is_replaced() {
        let p = [0x44u8; 32];
        let cfg = writer_ledger(p, &ActorKeypair::generate());
        let roster = keys(Some(p), Some(OLD_W));
        // The same keys again.
        assert!(ended(&cfg, roster, roster).is_empty());
        // A standard client attests no key: the nest keeps the row's.
        assert!(ended(&cfg, roster, keys(None, None)).is_empty());
        // A first ceremony: no roster row names a key.
        assert!(
            ended(
                &cfg,
                PrincipalKeys::default(),
                keys(Some([0x45; 32]), Some(NEW_W))
            )
            .is_empty()
        );
        // A first writer for a holder that had none replaces nothing.
        assert!(ended(&cfg, keys(Some(p), None), keys(Some(p), Some(NEW_W))).is_empty());
        // A writer with no holder on the roster selects nothing.
        assert_eq!(
            replaced_key(keys(None, Some(OLD_W)), keys(None, Some(NEW_W))),
            None
        );
    }

    // ── mint_options (the scope-first mint picker) ───────────────────────────

    fn mda_holder() -> MintHolder {
        MintHolder {
            bridge_id: "mda-1".into(),
            role: "mda".into(),
        }
    }

    fn web_serve_holder() -> MintHolder {
        MintHolder {
            bridge_id: "web-serve".into(),
            role: "content-processor".into(),
        }
    }

    fn mail_with_msek() -> MailConfig {
        MailConfig {
            msek: Some([0x42; 32].into()),
            ..MailConfig::default()
        }
    }

    #[test]
    fn mint_options_empty_when_no_holders() {
        // Mail enabled + a held tier, but no enrolled holder → nothing to mint to.
        let mail = mail_with_msek();
        let mut custody = fauna_core::data::SubscriptionsConfig::default();
        fauna_client_subscriptions::custody::record_new_tier(
            &mut custody,
            fauna_core::identity::ActorId([9u8; 32]),
            "gold",
            [0x11; 32],
            1000,
        );
        assert!(mint_options(&custody, &mail, &[]).is_empty());
    }

    #[test]
    fn mail_and_calendar_offered_when_msek_and_mda_present() {
        let mail = mail_with_msek();
        let custody = fauna_core::data::SubscriptionsConfig::default();
        let opts = mint_options(&custody, &mail, &[mda_holder()]);
        assert_eq!(
            opts.len(),
            2,
            "mail + calendar; no tiers held → no post option"
        );

        assert_eq!(opts[0].use_case, MintUseCase::Mail);
        // The Mail use case mints the PRODUCTION MDA grant: content.read{mail}
        // bundled with the keyless content.label-write (spam scoring) — never
        // read{mail} alone (design ratified 2026-07-13; the proven flow is
        // test_capability_rescore_drain.py's mail+label-write grant).
        assert_eq!(
            opts[0].scope,
            vec![
                GrantEventScope {
                    class: "content.read".into(),
                    kind: Some("mail".into()),
                    tier: None,
                },
                GrantEventScope {
                    class: "content.label-write".into(),
                    kind: None,
                    tier: None,
                },
            ]
        );
        assert_eq!(opts[0].tier, None);
        assert_eq!(opts[0].holder_candidates, vec!["mda-1".to_string()]);

        assert_eq!(opts[1].use_case, MintUseCase::Calendar);
        assert_eq!(
            opts[1].scope,
            vec![GrantEventScope {
                class: "content.read".into(),
                kind: Some("calendar".into()),
                tier: None,
            }]
        );
        assert_eq!(opts[1].holder_candidates, vec!["mda-1".to_string()]);
    }

    #[test]
    fn mail_and_calendar_absent_without_msek() {
        let mail = MailConfig::default();
        // Mail not enabled (no MSEK) → the mail/calendar payloads can't derive,
        // so the options are not offered (never an option that errors on confirm).
        let custody = fauna_core::data::SubscriptionsConfig::default();
        assert!(mint_options(&custody, &mail, &[mda_holder()]).is_empty());
    }

    #[test]
    fn mail_and_calendar_absent_without_mda_holder() {
        // MSEK held but the only enrolled holder is the web-serve processor →
        // no holder can take a mail/calendar grant, so the options are not offered.
        let mail = mail_with_msek();
        let custody = fauna_core::data::SubscriptionsConfig::default();
        assert!(mint_options(&custody, &mail, &[web_serve_holder()]).is_empty());
    }

    #[test]
    fn one_post_option_per_held_tier_targeting_generic_content_processor() {
        let mail = MailConfig::default();
        let mut custody = fauna_core::data::SubscriptionsConfig::default();
        fauna_client_subscriptions::custody::record_new_tier(
            &mut custody,
            fauna_core::identity::ActorId([9u8; 32]),
            "gold",
            [0x11; 32],
            1000,
        );
        fauna_client_subscriptions::custody::record_new_tier(
            &mut custody,
            fauna_core::identity::ActorId([9u8; 32]),
            "silver",
            [0x22; 32],
            1000,
        );
        let opts = mint_options(&custody, &mail, &[mda_holder(), web_serve_holder()]);
        // No MSEK → no mail/calendar; two held tiers → two post options, cfg order.
        assert_eq!(opts.len(), 2);
        assert!(
            opts.iter()
                .all(|o| o.use_case == MintUseCase::PaywalledPosts)
        );
        assert_eq!(opts[0].tier.as_deref(), Some("gold"));
        assert_eq!(
            opts[0].scope,
            vec![GrantEventScope {
                class: "content.read".into(),
                kind: Some("post".into()),
                tier: Some("gold".into()),
            }]
        );
        assert_eq!(opts[1].tier.as_deref(), Some("silver"));
        // The post-tier grant targets the generic content-processor holder
        // (the web-serve paywall holder), never the MDA.
        assert_eq!(opts[0].holder_candidates, vec!["web-serve".to_string()]);
        assert_eq!(opts[1].holder_candidates, vec!["web-serve".to_string()]);
    }

    #[test]
    fn post_options_absent_without_generic_content_processor_holder() {
        let mail = MailConfig::default();
        let mut custody = fauna_core::data::SubscriptionsConfig::default();
        fauna_client_subscriptions::custody::record_new_tier(
            &mut custody,
            fauna_core::identity::ActorId([9u8; 32]),
            "gold",
            [0x11; 32],
            1000,
        );
        assert!(mint_options(&custody, &mail, &[mda_holder()]).is_empty());
    }

    #[test]
    fn ambiguous_generic_holders_all_listed_as_candidates() {
        let mail = MailConfig::default();
        // Two generic content-processor holders (e.g. web-serve + a future
        // scorer) → the post option lists BOTH candidates, in holder order —
        // the one case the UI's holder-select renders (>1 candidate).
        let mut custody = fauna_core::data::SubscriptionsConfig::default();
        fauna_client_subscriptions::custody::record_new_tier(
            &mut custody,
            fauna_core::identity::ActorId([9u8; 32]),
            "gold",
            [0x11; 32],
            1000,
        );
        let scorer = MintHolder {
            bridge_id: "scorer".into(),
            role: "content-processor".into(),
        };
        let opts = mint_options(&custody, &mail, &[web_serve_holder(), scorer]);
        assert_eq!(opts.len(), 1);
        assert_eq!(
            opts[0].holder_candidates,
            vec!["web-serve".to_string(), "scorer".to_string()]
        );
    }

    #[test]
    fn full_catalog_orders_mail_calendar_then_tiers() {
        let mail = mail_with_msek();
        let mut custody = fauna_core::data::SubscriptionsConfig::default();
        fauna_client_subscriptions::custody::record_new_tier(
            &mut custody,
            fauna_core::identity::ActorId([9u8; 32]),
            "gold",
            [0x11; 32],
            1000,
        );
        let opts = mint_options(&custody, &mail, &[mda_holder(), web_serve_holder()]);
        let cases: Vec<MintUseCase> = opts.iter().map(|o| o.use_case).collect();
        assert_eq!(
            cases,
            vec![
                MintUseCase::Mail,
                MintUseCase::Calendar,
                MintUseCase::PaywalledPosts,
            ]
        );
    }

    // ── The T16 custody-facet folds ──────────────────────────────────────────

    use fauna_core::custody_ceremony::{
        CustodyAccept, CustodyOffer, GrantedCustody, HeldCustody, sign_custody_accept,
        sign_custody_offer,
    };
    use fauna_core::custody_grant::CustodyScopeSet;
    use fauna_core::custody_policy::{CustodyBudgetState, CustodyMeter, ScopeMeter};
    use fauna_core::custody_receipt::{CustodyReceipt, sign_custody_receipt};
    use fauna_core::data::Timestamp;
    use fauna_core::device_endpoints::DeviceEndpoints;

    const GRANT: [u8; 16] = [0x1D; 16];

    fn owner_kp() -> ActorKeypair {
        ActorKeypair::from_secret([0x0A; 32])
    }
    fn host_kp() -> ActorKeypair {
        ActorKeypair::from_secret([0x0B; 32])
    }
    fn custodian_kp() -> ActorKeypair {
        ActorKeypair::from_secret([0xC5; 32])
    }

    fn offer_bytes(owner: &ActorKeypair, host: &ActorKeypair, at: u64) -> Vec<u8> {
        let offer = CustodyOffer {
            grant_id: GRANT.to_vec(),
            owner: owner.actor_id(),
            host: host.actor_id(),
            scopes: CustodyScopeSet::Account,
            duration_secs: 90 * 86400,
            owner_devices: Vec::new(),
            owner_nest_url: None,
            offered_at: Timestamp(at),
        };
        let env = sign_custody_offer(owner, &offer).unwrap();
        fauna_core::encoding::canonical_encode(&env)
            .unwrap()
            .to_vec()
    }

    fn accept_bytes(host: &ActorKeypair, cap: u64) -> Vec<u8> {
        let key = custodian_kp().actor_id().0;
        let accept = CustodyAccept {
            grant_id: GRANT.to_vec(),
            offer_digest: [0u8; 32],
            host: host.actor_id(),
            custodian_key: key,
            custodian_endpoints: DeviceEndpoints {
                node_id: key,
                ..Default::default()
            },
            retained_bytes_cap: cap,
            narrowed_scopes: None,
            accepted_at: Timestamp(2),
            ..Default::default()
        };
        let env = sign_custody_accept(host, &accept).unwrap();
        fauna_core::encoding::canonical_encode(&env)
            .unwrap()
            .to_vec()
    }

    /// A nest-anchored accept (the nest-custodian identity fact): URL
    /// anchor present, endpoints candidate-free.
    fn nest_accept_bytes(host: &ActorKeypair, url: &str) -> Vec<u8> {
        let key = custodian_kp().actor_id().0;
        let accept = CustodyAccept {
            grant_id: GRANT.to_vec(),
            offer_digest: [0u8; 32],
            host: host.actor_id(),
            custodian_key: key,
            custodian_endpoints: DeviceEndpoints {
                node_id: key,
                ..Default::default()
            },
            custodian_nest_url: Some(url.into()),
            accepted_at: Timestamp(2),
            ..Default::default()
        };
        let env = sign_custody_accept(host, &accept).unwrap();
        fauna_core::encoding::canonical_encode(&env)
            .unwrap()
            .to_vec()
    }

    fn receipt_bytes(held: u64, cap: u64, evicted: u64, at: u64) -> Vec<u8> {
        let ck = custodian_kp();
        let receipt = CustodyReceipt::from_meter(
            GRANT.to_vec(),
            owner_kp().actor_id().0,
            ck.actor_id().0,
            &CustodyMeter {
                scopes: vec![ScopeMeter {
                    scope: "state".into(),
                    item_class: "state-entry".into(),
                    rows: 2,
                    payload_bytes: held,
                    ..Default::default()
                }],
            },
            cap,
            evicted,
            if evicted > 0 {
                CustodyBudgetState::OverBudget
            } else {
                CustodyBudgetState::Ok
            },
            0,
            Timestamp(at),
        );
        let env = sign_custody_receipt(&ck, &receipt).unwrap();
        fauna_core::encoding::canonical_encode(&env)
            .unwrap()
            .to_vec()
    }

    /// Owner side: pending → live → revoked, with the receipt three-state
    /// riding the live row.
    #[test]
    fn custody_rows_fold_pending_live_and_revoked() {
        let (o, h) = (owner_kp(), host_kp());
        let now_micros = 10_000_000_000_000u64; // 10^7 secs
        let now_secs = now_micros / 1_000_000;

        // Pending: offer out, nothing minted.
        let mut cfg = CustodyConfig::default();
        let mut ledger = empty_ledger();
        cfg.granted.push(GrantedCustody {
            grant_id: GRANT.to_vec(),
            host: h.actor_id().0,
            offer: offer_bytes(&o, &h, 1),
            ..Default::default()
        });
        let rows = custody_rows(&cfg, &ledger, now_micros);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].pending);
        assert_eq!(rows[0].scopes, Some(CustodyScopeSet::Account));
        assert_eq!(rows[0].custodian_key, None);
        assert_eq!(rows[0].receipt_state, ReceiptState::NoReceiptYet);
        assert_eq!(rows[0].liveness, None);

        // Live: accept captured + mint recorded → bound key, liveness, window.
        {
            let rec = &mut cfg.granted[0];
            rec.accept = accept_bytes(&h, 4096);
            rec.minted = true;
        }
        record_mint(
            &mut ledger,
            owner_kp().signing_key(),
            GRANT,
            custodian_kp().actor_id().0,
            crate::custody_grants::custody_event_scopes(&CustodyScopeSet::Account),
            now_secs,
            now_secs + 90 * 86400,
            now_secs,
        )
        .unwrap();
        let rows = custody_rows(&cfg, &ledger, now_micros);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].pending);
        assert_eq!(rows[0].custodian_key, Some(custodian_kp().actor_id().0));
        assert_eq!(rows[0].lasts_until, Some(now_secs + 90 * 86400));
        assert_eq!(rows[0].liveness, Some(GrantLiveness::Active));

        // A fresh receipt → Fresh with decoded facts; an old one → Stale.
        {
            let rec = &mut cfg.granted[0];
            rec.latest_receipt = receipt_bytes(700, 4096, 100, now_micros - 1);
            rec.latest_receipt_at = Timestamp(now_micros - 1);
        }
        let rows = custody_rows(&cfg, &ledger, now_micros);
        assert_eq!(rows[0].receipt_state, ReceiptState::Fresh);
        let r = rows[0].receipt.expect("decoded");
        assert_eq!((r.held_bytes, r.retained_bytes_cap), (700, 4096));
        assert!(r.degraded, "evicted bytes make the receipt degraded");
        {
            let rec = &mut cfg.granted[0];
            rec.latest_receipt_at = Timestamp(now_micros - CUSTODY_RECEIPT_STALE_MICROS);
        }
        assert_eq!(
            custody_rows(&cfg, &ledger, now_micros)[0].receipt_state,
            ReceiptState::Stale
        );

        // Revoked: drops out of the Now rows entirely.
        record_revoke(
            &mut ledger,
            owner_kp().signing_key(),
            GRANT,
            custodian_kp().actor_id().0,
            now_secs + 1,
        )
        .unwrap();
        assert!(custody_rows(&cfg, &ledger, now_micros).is_empty());
    }

    /// The render split (the nest-custodian identity fact): a nest-anchored
    /// accept's row carries the URL — the Nests-page family's membership
    /// fact — and a device-anchored row carries `None`. One fold, one field,
    /// never both surfaces.
    #[test]
    fn custody_rows_carry_the_nest_custodian_split() {
        let (o, h) = (owner_kp(), host_kp());
        let now_micros = 10_000_000_000_000u64;
        let mut cfg = CustodyConfig::default();
        let ledger = empty_ledger();
        cfg.granted.push(GrantedCustody {
            grant_id: GRANT.to_vec(),
            host: h.actor_id().0,
            offer: offer_bytes(&o, &h, 1),
            accept: accept_bytes(&h, 4096),
            ..Default::default()
        });
        let rows = custody_rows(&cfg, &ledger, now_micros);
        assert_eq!(rows[0].custodian_nest_url, None, "device custodian");
        cfg.granted[0].accept = nest_accept_bytes(&h, "https://friend-nest.example/");
        let rows = custody_rows(&cfg, &ledger, now_micros);
        assert_eq!(
            rows[0].custodian_nest_url.as_deref(),
            Some("https://friend-nest.example/"),
            "nest custodian"
        );
    }

    /// Host side: the accepted custody renders the accept's budget and its
    /// own minted receipt; a pending offer renders on the consent fold; a
    /// decline moves it off both.
    #[test]
    fn held_and_offer_folds_respect_accept_and_decline() {
        let (o, h) = (owner_kp(), host_kp());
        let now_micros = 6;
        let mut cfg = CustodyConfig::default();
        cfg.held.push(HeldCustody {
            grant_id: GRANT.to_vec(),
            owner: o.actor_id().0,
            offer: offer_bytes(&o, &h, 5),
            ..Default::default()
        });

        // Pending: on the consent fold, not the held fold.
        let offers = custody_offers(&cfg, now_micros);
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].owner, o.actor_id().0);
        assert_eq!(offers[0].scopes, CustodyScopeSet::Account);
        assert_eq!(offers[0].offered_at_micros, 5);
        assert!(held_custodies(&cfg).is_empty());

        // Declined: off both folds, and only a PENDING offer can decline.
        assert!(crate::custody_ceremony::decline_offer(
            &mut cfg,
            &GRANT,
            Timestamp(6)
        ));
        assert!(custody_offers(&cfg, now_micros).is_empty());
        assert!(held_custodies(&cfg).is_empty());

        // Accepted (fresh record): on the held fold with the accept's budget
        // + this device's own minted receipt.
        cfg.held[0].declined = false;
        cfg.held[0].accept = accept_bytes(&h, 8192);
        cfg.held[0].receipt = receipt_bytes(300, 8192, 0, 7);
        assert!(
            !crate::custody_ceremony::decline_offer(&mut cfg, &GRANT, Timestamp(8)),
            "an accepted custody is the stop control's business, not decline's"
        );
        assert!(custody_offers(&cfg, now_micros).is_empty());
        let held = held_custodies(&cfg);
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].retained_bytes_cap, 8192);
        assert_eq!(held[0].scopes, Some(CustodyScopeSet::Account));
        let r = held[0].receipt.expect("own receipt decoded");
        assert_eq!(r.held_bytes, 300);
        assert!(!r.degraded);
    }

    /// The bundled fold is exactly the three folds it replaces — the property
    /// the six trickle-down legs rely on when they stop calling the folds
    /// individually. Asserted on a config carrying
    /// all three families at once, so a future reordering of one fold cannot
    /// silently disagree with the bundle.
    #[test]
    fn the_bundled_fold_agrees_with_its_three_parts() {
        let (o, h) = (owner_kp(), host_kp());
        let mut cfg = CustodyConfig::default();
        // Host side: one accepted custody + one still-pending offer.
        cfg.held.push(HeldCustody {
            grant_id: GRANT.to_vec(),
            owner: o.actor_id().0,
            offer: offer_bytes(&o, &h, 5),
            accept: accept_bytes(&h, 8192),
            receipt: receipt_bytes(300, 8192, 0, 7),
            ..Default::default()
        });
        cfg.held.push(HeldCustody {
            grant_id: vec![7u8; 16],
            owner: o.actor_id().0,
            offer: offer_bytes(&o, &h, 9),
            ..Default::default()
        });
        let now_micros = 10;

        let ledger = empty_ledger();
        let facet = fold_custody_facet(&cfg, &ledger, now_micros);
        assert_eq!(facet.rows, custody_rows(&cfg, &ledger, now_micros));
        assert_eq!(facet.held, held_custodies(&cfg));
        assert_eq!(facet.offers, custody_offers(&cfg, now_micros));
        assert_eq!(facet.held.len(), 1, "the accepted one");
        assert_eq!(facet.offers.len(), 1, "the pending one");
    }

    /// The registry overlay reaches the bundle's `held` rows — the native
    /// caller's one impure edge. A row the store does not carry keeps its
    /// accept-seeded budget, which is what makes a store-less (wasm) fold a
    /// correct render rather than a degraded one.
    #[test]
    fn the_bundles_overlay_applies_the_live_budget_and_stop_mark() {
        let (o, h) = (owner_kp(), host_kp());
        let mut cfg = CustodyConfig::default();
        cfg.held.push(HeldCustody {
            grant_id: GRANT.to_vec(),
            owner: o.actor_id().0,
            offer: offer_bytes(&o, &h, 5),
            accept: accept_bytes(&h, 8192),
            ..Default::default()
        });

        let mut facet = fold_custody_facet(&cfg, &empty_ledger(), 10);
        assert_eq!(facet.held[0].retained_bytes_cap, 8192, "accept-seeded");
        assert!(!facet.held[0].stopped);

        facet.overlay_registry_rows(&[fauna_core::custodies_held::CustodyHeld {
            grant_id: GRANT.to_vec(),
            retained_bytes_cap: 4096,
            stopped: true,
            ..Default::default()
        }]);
        assert_eq!(facet.held[0].retained_bytes_cap, 4096, "row wins");
        assert!(facet.held[0].stopped);

        // A row for a custody the fold does not carry changes nothing.
        facet.overlay_registry_rows(&[fauna_core::custodies_held::CustodyHeld {
            grant_id: vec![3u8; 16],
            retained_bytes_cap: 1,
            stopped: false,
            ..Default::default()
        }]);
        assert_eq!(
            facet.held[0].retained_bytes_cap, 4096,
            "unmatched row inert"
        );
        assert!(facet.held[0].stopped);
    }

    /// The budget seed texts are aligned with `held` and carry the row's LIVE
    /// cap — the overlay's value, not the accept's. The alignment is what the
    /// per-app input arrays index into, so a length or order drift here would
    /// seed the wrong row's budget on every leg.
    #[test]
    fn budget_draft_texts_track_the_live_cap_in_held_order() {
        let (o, h) = (owner_kp(), host_kp());
        let mut cfg = CustodyConfig::default();
        for (i, gid) in [GRANT.to_vec(), vec![2u8; 16]].into_iter().enumerate() {
            cfg.held.push(HeldCustody {
                grant_id: gid,
                owner: o.actor_id().0,
                offer: offer_bytes(&o, &h, 5 + i as u64),
                accept: accept_bytes(&h, 8192),
                ..Default::default()
            });
        }

        let mut facet = fold_custody_facet(&cfg, &empty_ledger(), 10);
        assert_eq!(budget_draft_texts(&facet).len(), facet.held.len());

        // Overlay one row; only that row's seed text moves.
        let live = facet.held[0].grant_id.clone();
        facet.overlay_registry_rows(&[fauna_core::custodies_held::CustodyHeld {
            grant_id: live,
            retained_bytes_cap: 1024,
            stopped: false,
            ..Default::default()
        }]);
        let texts = budget_draft_texts(&facet);
        assert_eq!(texts[0], fauna_core::format::byte_size(1024), "live cap");
        assert_eq!(texts[1], fauna_core::format::byte_size(8192), "accept cap");
    }

    /// Every `ReceiptState` maps to its OWN key — the A7 honesty rule. Three
    /// states, three distinct strings, none of them empty: a collapse here is
    /// exactly how a stale custodian would start reading as a healthy one.
    #[test]
    fn custody_receipt_status_maps_every_state_to_a_distinct_key() {
        let fresh = custody_receipt_status_display(ReceiptState::Fresh, Some(2_000_000));
        let stale = custody_receipt_status_display(ReceiptState::Stale, Some(2_000_000));
        let none = custody_receipt_status_display(ReceiptState::NoReceiptYet, None);

        for d in [&fresh, &stale, &none] {
            assert!(!d.label.key.is_empty(), "no state renders empty");
        }
        assert_ne!(fresh.label.key, stale.label.key);
        assert_ne!(fresh.label.key, none.label.key);
        assert_ne!(stale.label.key, none.label.key);

        // The two timestamped states hand the client seconds to format; the
        // third has no date to show.
        assert_eq!(fresh.attested_at_secs, Some(2));
        assert_eq!(stale.attested_at_secs, Some(2));
        assert_eq!(none.attested_at_secs, None);

        // A missing timestamp must not fabricate one.
        assert_eq!(
            custody_receipt_status_display(ReceiptState::Fresh, None).attested_at_secs,
            None
        );
        // ...and "no confirmation yet" never carries a date, even if one is passed.
        assert_eq!(
            custody_receipt_status_display(ReceiptState::NoReceiptYet, Some(9_000_000))
                .attested_at_secs,
            None
        );
    }

    /// Held-bytes carries the two byte texts UNRESOLVED (the client resolves
    /// them before substituting), and `degraded` rides independently of
    /// freshness — a fresh receipt can honestly report dropped payload.
    #[test]
    fn custody_held_bytes_display_splits_its_inner_texts_and_keeps_degraded_orthogonal() {
        let r = CustodyReceiptView {
            held_bytes: 300,
            retained_bytes_cap: 8192,
            degraded: true,
            attested_at_micros: 7,
        };
        let d = custody_held_bytes_display(Some(&r));
        assert_eq!(d.held, fauna_core::format::byte_size(300));
        assert_eq!(d.cap, fauna_core::format::byte_size(8192));
        assert!(
            d.degraded,
            "degraded is independent of the receipt's freshness"
        );
        assert!(
            d.label.args.is_empty(),
            "the client substitutes {{held}}/{{cap}}"
        );

        // No receipt: em-dash placeholders, NOT zeroes — "nothing confirmed
        // yet" and "confirmed zero bytes" are different facts.
        let empty = custody_held_bytes_display(None);
        assert_ne!(empty.held, fauna_core::format::byte_size(0));
        assert_eq!(empty.held, empty.cap);
        assert!(!empty.degraded);
    }

    fn admin_row(
        host: &str,
        owner: &str,
        grant: &[u8],
        held: u64,
        last_receipt_at: u64,
    ) -> fauna_protocol::custody::AdminHostingRow {
        fauna_protocol::custody::AdminHostingRow {
            host_actor_id: host.into(),
            item: fauna_protocol::custody::HostingItem {
                grant_id: fauna_protocol::ByteBuf::from(grant.to_vec()),
                owner_actor_id: owner.into(),
                owner_nest_url: "https://owner.example".into(),
                retained_bytes_cap: 8192,
                stopped: false,
                updated_at: 0,
                held_bytes: held,
                last_receipt_at,
                extra: Default::default(),
            },
            extra: Default::default(),
        }
    }

    /// The one question this surface exists to answer is *which rows are
    /// filling my disk*, so the heaviest row sorts first — registry order
    /// would be arbitrary to it. Equal weights fall back to a total order
    /// that never consults the nest's own `ORDER BY`.
    #[test]
    fn the_admin_registry_puts_the_heaviest_row_first_and_breaks_ties_totally() {
        let reply = fauna_protocol::custody::AdminHostingListReply {
            rows: vec![
                admin_row("aa", "o1", b"g1", 10, 0),
                admin_row("cc", "o1", b"g2", 900, 0),
                admin_row("bb", "o1", b"g3", 10, 0),
            ],
            extra: Default::default(),
        };

        let rows = admin_hosting_rows(&reply, 0);
        assert_eq!(rows[0].held_bytes, 900, "the heaviest row leads");
        assert_eq!(rows[0].host_actor_id, "cc");
        // The two 10-byte rows tie on weight and settle on host id.
        assert_eq!(
            rows.iter()
                .map(|r| r.host_actor_id.as_str())
                .collect::<Vec<_>>(),
            vec!["cc", "aa", "bb"]
        );
    }

    /// `(host_actor_id, grant_id)` IS the remove door's key. A surface that
    /// dropped either half could render a row it cannot remove — the exact
    /// unrecoverable state finding filed.
    #[test]
    fn every_admin_row_carries_the_remove_doors_whole_key() {
        let reply = fauna_protocol::custody::AdminHostingListReply {
            rows: vec![admin_row("host-hex", "owner-hex", b"grant-7", 5, 0)],
            extra: Default::default(),
        };

        let rows = admin_hosting_rows(&reply, 0);
        assert_eq!(rows[0].host_actor_id, "host-hex");
        assert_eq!(rows[0].grant_id, b"grant-7".to_vec());
        assert_eq!(rows[0].owner_actor_id, "owner-hex");
        assert_eq!(rows[0].owner_nest_url, "https://owner.example");
    }

    /// The three-state receipt word is the shared one, and `last_receipt_at
    /// == 0` is *no receipt has ever arrived* — a real state, never rendered
    /// as an infinitely stale one.
    #[test]
    fn the_admin_row_reads_receipt_freshness_with_the_shared_three_states() {
        let now = 10 * CUSTODY_RECEIPT_STALE_MICROS;
        let reply = fauna_protocol::custody::AdminHostingListReply {
            rows: vec![
                admin_row("h", "o", b"never", 3, 0),
                admin_row("h", "o", b"old", 2, 1),
                admin_row("h", "o", b"new", 1, now),
            ],
            extra: Default::default(),
        };

        let rows = admin_hosting_rows(&reply, now);
        assert_eq!(rows[0].receipt_state, ReceiptState::NoReceiptYet);
        assert_eq!(rows[1].receipt_state, ReceiptState::Stale);
        assert_eq!(rows[2].receipt_state, ReceiptState::Fresh);
    }
}

/// The lookup-generic text resolvers — the last step `fauna-linux` and
/// `fauna-tui` used to hand-roll identically. Every case here is a
/// *composition* assertion: the shared decision fns already have their own
/// tests above, so these pin only what moved — which slot gets filled, with
/// which arm, and that the degraded badge rides independently. Mirrors
/// `fauna_core::format`'s own `localized_resolve_tests`.
#[cfg(all(test, feature = "local-clock"))]
mod localized_resolve_tests {
    use super::*;

    /// A stand-in i18n table, transcribed from `i18n/strings/en.yaml`'s
    /// `devices.custody_*` keys — every key these labels can select, so a
    /// leaked placeholder or an unresolved key fails loudly rather than
    /// silently passing through `LocalizedText::resolve`'s missing-key
    /// fallback.
    fn lookup(key: &str) -> Option<&'static str> {
        match key {
            "devices.custody_receipt_fresh" => Some("Last confirmed {when}"),
            "devices.custody_receipt_stale" => {
                Some("Stale — last confirmed {when}. Treat this copy as degraded.")
            }
            "devices.custody_receipt_none" => Some("No confirmation yet"),
            "devices.custody_held_bytes" => Some("Holding {held} of {cap}"),
            "devices.custody_degraded_badge" => {
                Some("Degraded — some copies were dropped under the budget")
            }
            _ => None,
        }
    }

    /// Fresh and stale both substitute `{when}` with the shared local-date
    /// render; the no-receipt state's key carries no slot, so nothing to
    /// substitute and nothing left over.
    #[test]
    fn custody_receipt_status_text_substitutes_when_only_on_the_two_timestamped_states() {
        let attested_micros = 1_700_000_000 * 1_000_000u64;
        let when = fauna_core::format::format_unix_local(1_700_000_000);

        assert_eq!(
            custody_receipt_status_text(ReceiptState::Fresh, Some(attested_micros), lookup),
            format!("Last confirmed {when}"),
        );
        assert_eq!(
            custody_receipt_status_text(ReceiptState::Stale, Some(attested_micros), lookup),
            format!("Stale — last confirmed {when}. Treat this copy as degraded."),
        );
        assert_eq!(
            custody_receipt_status_text(ReceiptState::NoReceiptYet, Some(attested_micros), lookup),
            "No confirmation yet",
            "no-receipt never carries a date, even when one is passed",
        );
    }

    /// Both inner byte texts resolve into the outer `{held}`/`{cap}` slots
    /// before substitution — a `LocalizedText` arg is a flat string, so
    /// resolving in the wrong order would leak a raw key into painted text.
    #[test]
    fn custody_held_bytes_text_resolves_both_inner_slots_before_substituting() {
        let r = CustodyReceiptView {
            held_bytes: 300,
            retained_bytes_cap: 8192,
            degraded: false,
            attested_at_micros: 7,
        };
        let held = fauna_core::format::byte_size(300).resolve(lookup);
        let cap = fauna_core::format::byte_size(8192).resolve(lookup);
        assert_eq!(
            custody_held_bytes_text(Some(&r), lookup),
            format!("Holding {held} of {cap}"),
        );
    }

    /// `degraded` appends the badge text rather than replacing anything — it
    /// rides independently of freshness, exactly like [`custody_held_bytes_display`]
    /// documents.
    #[test]
    fn custody_held_bytes_text_appends_the_degraded_badge_rather_than_replacing() {
        let r = CustodyReceiptView {
            held_bytes: 300,
            retained_bytes_cap: 8192,
            degraded: true,
            attested_at_micros: 7,
        };
        let text = custody_held_bytes_text(Some(&r), lookup);
        assert!(text.starts_with("Holding "), "the usage line stays first");
        assert!(
            text.ends_with("Degraded — some copies were dropped under the budget"),
            "the badge appends: {text}",
        );
    }

    /// No receipt yet renders the em-dash placeholder in both slots — never a
    /// resolved "0 B", which would read as a confirmed-empty custody rather
    /// than "nothing confirmed yet".
    #[test]
    fn custody_held_bytes_text_renders_the_no_receipt_placeholder_in_both_slots() {
        assert_eq!(
            custody_held_bytes_text(None, lookup),
            format!("Holding {ph} of {ph}", ph = CUSTODY_NO_RECEIPT_PLACEHOLDER),
        );
    }

    // ── custody_hosting_budget_text ──────────────────────────────────────

    /// A zero cap means "no cap on the row — the pump substitutes the
    /// default", the opposite of "zero bytes allowed": printing `0 B` would
    /// state the opposite of the truth.
    #[test]
    fn custody_hosting_budget_text_reads_as_default_when_capless_never_as_zero_bytes() {
        assert_eq!(
            custody_hosting_budget_text(0, lookup),
            fauna_i18n::strings::admin::custody_hosting::BUDGET_DEFAULT,
        );
    }

    #[test]
    fn custody_hosting_budget_text_renders_a_real_caps_byte_size() {
        let expected = fauna_core::format::byte_size(8192).resolve(lookup);
        assert_eq!(custody_hosting_budget_text(8192, lookup), expected);
        assert_ne!(
            custody_hosting_budget_text(8192, lookup),
            fauna_i18n::strings::admin::custody_hosting::BUDGET_DEFAULT,
        );
    }

    // ── receipt_text ──────────────────────────────────────────────────────

    #[test]
    fn receipt_text_maps_every_state_to_a_distinct_word() {
        let fresh = receipt_text(ReceiptState::Fresh);
        let stale = receipt_text(ReceiptState::Stale);
        let none = receipt_text(ReceiptState::NoReceiptYet);
        assert_eq!(
            fresh,
            fauna_i18n::strings::admin::custody_hosting::RECEIPT_FRESH
        );
        assert_eq!(
            stale,
            fauna_i18n::strings::admin::custody_hosting::RECEIPT_STALE
        );
        assert_eq!(
            none,
            fauna_i18n::strings::admin::custody_hosting::RECEIPT_NONE
        );
        assert_ne!(fresh, stale);
        assert_ne!(stale, none);
        assert_ne!(fresh, none);
    }
}
